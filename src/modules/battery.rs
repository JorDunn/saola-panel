//! The bar's battery readout, fed by UPower over D-Bus.
//!
//! This module establishes the zbus→iced bridge pattern that the network
//! module (Stage 5) copies: an async worker task owns the D-Bus proxy,
//! listens to property-change streams, and pushes snapshots into the panel
//! through an `iced::stream` channel wrapped in `Subscription::run`. The
//! UI thread never touches D-Bus — it only receives ready-made [`Battery`]
//! values via [`Message::Updated`].
//!
//! # Design language
//!
//! The settled concept ("4b Ledger final") renders every right-region status
//! module **bare, directly on the ink bar** — no pill fill at all. As of
//! 2026-08-01 (Jordan: the details live in the popover) the readout is
//! **icon-only**: [`battery_icon`]'s leveled Lucide ladder
//! (`battery`/`-low`/`-medium`/`-full`, bolt while charging) carries the
//! level, and the exact percentage plus time remaining show only in the
//! quick-settings popover. A resting readout is information, not a control
//! that is switched on, so it gets no fill and no accent: a plain ivory
//! (`on_ink.primary`) glyph.
//!
//! Terracotta is reserved for the *live* state — here, **charging** — and even
//! then it is a small accent, not a flood: the glyph takes
//! `palette.accent_light`, and nothing behind it changes. That restraint is
//! the style guide's "at most one terracotta element per surface" rule; a
//! solid terracotta pill would spend the bar's entire accent budget on a
//! battery. No third treatment, no green/red battery colors.
//!
//! (`accent_light` rather than raw `accent`: #C67139 on ink fails contrast —
//! style guide §1's accent ramp exists precisely so accent-colored *text and
//! strokes* on the ink surface have a legible token to reach for.)
//!
//! If no battery is present — or UPower isn't on the bus at all — the module
//! renders nothing and the panel carries on.

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, StreamExt};
use iced::futures::{SinkExt, Stream};
use iced::widget::Space;
use iced::{Element, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;
use zbus::Connection;

use crate::icons::{self, Icon};

/// The battery module's own message type (Stage 7's per-module refactor —
/// see `modules::clock::Message` for the full teaching note on why every
/// module now owns its `Message` instead of contributing variants to the
/// panel's flat enum). `main.rs` nests this as `Message::Battery(battery::
/// Message)` and its `Panel::update` delegates by pattern-matching straight
/// through both layers: `Message::Battery(battery::Message::Updated(b))`.
///
/// Single variant, same payload as the old flat enum's `BatteryUpdated`.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Battery),
}

/// UPower's `State` value for "charging" (the only state the one rule
/// gives the terracotta accent — everything else is "at rest", ivory).
const UPOWER_STATE_CHARGING: u32 = 1;

/// A zbus proxy for UPower's aggregate battery.
///
/// Teaching note (the proxy macro): `#[zbus::proxy]` reads this trait and
/// generates a struct named `UPowerDeviceProxy` — the trait itself is never
/// implemented by us or referenced again. Each `#[zbus(property)]` method
/// becomes three things on the generated struct:
///
/// - `proxy.percentage().await` — an async getter (served from zbus's local
///   property cache once it's warm, so repeat reads are not bus round-trips),
/// - `proxy.receive_percentage_changed().await` — a `Stream` of change
///   events driven by the standard `PropertiesChanged` D-Bus signal,
/// - `proxy.cached_percentage()` — cache-only access (unused here).
///
/// Method names are snake_case; the macro converts them to the PascalCase
/// D-Bus property names (`is_present` → `IsPresent`). With both
/// `default_service` and `default_path` given, the generated constructor is
/// just `UPowerDeviceProxy::new(&connection)`.
///
/// `DisplayDevice` is UPower's composite "the battery as the UI should show
/// it" object — it exists even on desktops (with `IsPresent = false`), so
/// we never have to enumerate individual devices.
#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait UPowerDevice {
    /// Charge level, 0.0–100.0.
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;

    /// Charging state: 1 = charging, 2 = discharging (other values exist —
    /// unknown/empty/fully-charged — and all count as "at rest" here).
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;

    /// Whether a battery is physically present (false on desktops).
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;

    /// Seconds until the battery is empty at the current discharge rate.
    ///
    /// UPower's D-Bus type here is `x` (a signed 64-bit integer), which maps
    /// to Rust's `i64` — signed even though a duration can't sensibly be
    /// negative. `0` is UPower's documented "unknown" marker: it reports zero
    /// while charging, and also for the first few seconds after boot or a
    /// cable change, before the rate estimate settles. See
    /// [`Battery::time_remaining`] for how both of those become `None`.
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;

    /// Seconds until the battery is full at the current charge rate — the
    /// mirror of `time_to_empty`, and `0`/unknown whenever discharging.
    #[zbus(property)]
    fn time_to_full(&self) -> zbus::Result<i64>;

    /// Instantaneous power flow through the battery, in watts. UPower
    /// documents it as the drain rate, but in practice firmware reports it
    /// for charging too, and some report the charging direction as a
    /// *negative* number — see [`Battery::power_draw`] for how the sign and
    /// the `0.0`-means-unknown convention are handled.
    #[zbus(property)]
    fn energy_rate(&self) -> zbus::Result<f64>;
}

/// Battery module state: the last snapshot the D-Bus worker pushed through
/// [`Message::Updated`]. Unlike `Clock` (which reads the
/// system clock fresh every render), this module *must* cache — reading
/// D-Bus during `view` would block the UI thread.
///
/// `Default` is the boot state: `present: false`, i.e. "no battery known
/// yet", so the module stays hidden until UPower actually reports one. That
/// makes "no battery", "UPower missing", and "worker hasn't reported yet"
/// all render identically: as nothing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Battery {
    /// Charge level, 0.0–100.0 (as UPower reports it).
    percentage: f64,
    /// True while on AC and charging — the module's "live" state, and the
    /// only thing that earns the terracotta accent.
    charging: bool,
    /// False when there is no battery (or none known yet) — renders nothing.
    present: bool,
    /// Seconds until empty, straight from UPower — `0` meaning "unknown"
    /// (its convention, not ours: see the proxy's `time_to_empty`). Only
    /// meaningful while discharging.
    time_to_empty_secs: i64,
    /// Seconds until full, same `0`-is-unknown convention. Only meaningful
    /// while charging.
    time_to_full_secs: i64,
    /// Instantaneous power flow in watts, straight from UPower's
    /// `EnergyRate` — `0.0` meaning "unknown", and possibly negative while
    /// charging on some firmware (see [`Self::power_draw`]).
    energy_rate_watts: f64,
}

impl Battery {
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module (a `view` that returns a zero-sized `Space` is invisible on
    /// the ledger bar, but wrapped in its own scrim pill it would be an
    /// empty 40px blob). Reads the same field as `view`'s early return, so
    /// the two cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// Charge level, 0.0–100.0. The bar no longer renders the number at all
    /// (icon-only, 2026-08-01 — [`battery_icon`] buckets it into the glyph
    /// ladder); the quick-settings popover is where the exact figure shows.
    pub fn percentage(&self) -> f64 {
        self.percentage
    }

    /// True while charging — the module's "live" state (see the module doc
    /// comment's note on why that, and only that, earns the terracotta
    /// accent).
    pub fn charging(&self) -> bool {
        self.charging
    }

    /// "2h 14m until full" / "…until empty", as a bare duration label — or
    /// `None` when UPower has no estimate to give.
    ///
    /// Which of the two UPower fields to read is decided by [`charging`](Self::charging),
    /// not by which one happens to be non-zero: UPower zeroes the irrelevant
    /// one, but it does so *asynchronously* with the state change, so for a
    /// frame or two after a cable event both can be non-zero (or the stale
    /// one can outlive the new state). Keying off `charging` means the label
    /// always agrees with the glyph beside it, even mid-transition.
    ///
    /// `None` covers three cases that all mean the same thing to a reader —
    /// no estimate yet (UPower's `0`), a nonsense negative value, and a
    /// battery that isn't there — so callers have exactly one thing to
    /// handle: draw the percentage alone.
    ///
    /// Deliberately *not* used by [`Self::view`]: the bar readout is a bare
    /// glyph, and a time estimate that appears and vanishes as UPower's rate
    /// estimate settles would make the right region reflow — the exact
    /// jitter this module's design notes set out to avoid. This is
    /// popover-only information.
    pub fn time_remaining(&self) -> Option<String> {
        if !self.present {
            return None;
        }
        let secs = if self.charging {
            self.time_to_full_secs
        } else {
            self.time_to_empty_secs
        };
        (secs > 0).then(|| duration_label(secs))
    }

    /// The current power draw as a bare label — `"9.5W"` — or `None` when
    /// UPower has no reading to give.
    ///
    /// The *magnitude* is what renders: UPower documents `EnergyRate` as the
    /// drain rate, but some firmware reports the charging direction as a
    /// negative number, and the direction is already told by the charging
    /// glyph/tint beside this label — a minus sign would just repeat it in a
    /// more cryptic way. `0.0` is the "no reading" marker (mirroring
    /// `time_to_empty`'s `0`), so, like [`Self::time_remaining`], callers
    /// have exactly one case to handle: omit the label.
    ///
    /// Popover-only, for the same reason as `time_remaining`: the draw
    /// fluctuates with load on every UPower refresh, and a number that
    /// churns would make the bar's right region reflow.
    pub fn power_draw(&self) -> Option<String> {
        if !self.present {
            return None;
        }
        let watts = self.energy_rate_watts.abs();
        (watts > 0.0).then(|| format!("{watts:.1}W"))
    }

    /// Renders the leveled battery glyph, or nothing at all when no battery
    /// is present (`Space::new()` with no size is a zero-area widget — the
    /// row simply closes up around it). Bare icon straight on the ink bar,
    /// no pill, no text — see the module doc comment.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if !self.present {
            return Space::new().into();
        }

        // Icon-only as of 2026-08-01 (Jordan: the glyph carries the level,
        // the numbers live in the popover): the leveled Lucide battery
        // ladder replaces the percentage text entirely. The one rule, in
        // its bare-status form, still colors it: terracotta *accent* while
        // charging (live), plain ivory at rest — and charging also swaps to
        // the bolt glyph, so the state reads at a glance even for a reader
        // who can't tell the accent hue apart.
        let color = if self.charging {
            theme.palette.accent_light
        } else {
            theme.on_ink.primary
        }
        .into_iced();

        icons::icon(
            battery_icon(self.percentage, self.charging),
            theme.sizes.icon_bar,
            color,
        )
        .into()
    }

    /// The battery's D-Bus feed as an iced subscription.
    ///
    /// Teaching note (subscription identity): `Subscription::run` takes a
    /// plain function *pointer* (`fn() -> impl Stream`) — not a closure —
    /// and uses that pointer as the subscription's identity. iced calls
    /// `Panel::subscription` after every message, but because the pointer
    /// compares equal every time, the runtime keeps the one already-running
    /// worker instead of spawning another. This module's `Subscription<Self::
    /// Message>` gets `.map(crate::Message::Battery)`ed in `main.rs` before
    /// joining the panel's `Subscription::batch` — `.map` composes a mapper
    /// function onto the *already-keyed* stream returned by `Subscription::
    /// run`, it doesn't touch the key the runtime compares, so the fn-pointer
    /// identity above still survives across every `.map`ped re-subscribe.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(battery_stream)
    }
}

/// Builds the async stream the subscription runs: a channel whose sending
/// half is owned by the D-Bus worker future.
///
/// Teaching note (the bridge): `iced::stream::channel` hands our async
/// closure the `Sender` of an mpsc channel and returns the `Receiver` as a
/// `Stream` — that stream is what iced polls, on the tokio runtime that
/// iced's own `tokio` feature provides. zbus (built with *its* `tokio`
/// feature, see Cargo.toml) spawns its internal socket-reader tasks onto
/// whatever tokio runtime is current — which, inside this closure, is
/// iced's. One runtime, no blocking, no second executor.
///
/// Teaching note (ownership across the async boundary): everything D-Bus —
/// connection, proxy, property streams — is created inside and owned by
/// this async task, never by `Panel`. Only plain `Battery` values (which
/// are `Copy`) cross to the UI side, through the channel. That is what
/// keeps the UI thread free of D-Bus types *and* of D-Bus latency.
fn battery_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        // Any D-Bus failure — no system bus, UPower not on it, a property
        // read failing — lands here. Report "no battery" so the module
        // hides, and let the worker end quietly: the panel keeps running.
        if watch_upower(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(Battery::default())).await;
        }
    })
}

/// The worker proper: connect, read once, then re-read and push a snapshot
/// on every property change, forever.
async fn watch_upower(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // System bus: hardware services (UPower, iwd, ...) live there, not on
    // the per-user session bus.
    let connection = Connection::system().await?;
    let proxy = UPowerDeviceProxy::new(&connection).await?;

    // One merged "something changed" stream. Each `receive_*_changed`
    // stream yields typed change events; we only need the *fact* of a
    // change (we re-read the full snapshot below), so each is mapped to
    // `()` — which also gives them all the same item type, letting
    // `stream::select` merge them. These streams don't hold a borrow of
    // `proxy` (they clone its internals), so the getters below stay usable.
    //
    // Teaching note (merging more than two): `stream::select` is strictly
    // binary, so six streams become a right-nested tree of it. There is no
    // ordering or priority implied by the nesting — `select` polls both
    // sides fairly — so the shape is purely how the pairs happened to be
    // written. (Once a merge grows past a handful of *statically known*
    // streams, `stream::select_all` over a `Vec` of boxed streams reads
    // better; `media.rs` uses its dynamic sibling `SelectAll` for exactly
    // that reason. Six is still comfortably under that line.)
    //
    // UPower quirk: each stream also fires once immediately when the
    // property cache is already warm, so a couple of redundant snapshots
    // arrive right after boot. Harmless — the view just re-renders.
    let mut changed = {
        let percentage = proxy.receive_percentage_changed().await;
        let state = proxy.receive_state_changed().await;
        let present = proxy.receive_is_present_changed().await;
        let to_empty = proxy.receive_time_to_empty_changed().await;
        let to_full = proxy.receive_time_to_full_changed().await;
        let energy_rate = proxy.receive_energy_rate_changed().await;
        stream::select(
            stream::select(percentage.map(|_| ()), energy_rate.map(|_| ())),
            stream::select(
                stream::select(state.map(|_| ()), present.map(|_| ())),
                stream::select(to_empty.map(|_| ()), to_full.map(|_| ())),
            ),
        )
    };

    loop {
        // Property reads are served from zbus's local cache once it's warm
        // (updated by the same PropertiesChanged signals that drive the
        // streams above), so this "re-read everything" is cheap — not
        // three bus round-trips per event.
        let snapshot = Battery {
            percentage: proxy.percentage().await?,
            charging: is_charging(proxy.state().await?),
            present: proxy.is_present().await?,
            time_to_empty_secs: proxy.time_to_empty().await?,
            time_to_full_secs: proxy.time_to_full().await?,
            energy_rate_watts: proxy.energy_rate().await?,
        };

        // A send error means the receiving side is gone (the subscription
        // was dropped) — stop quietly rather than erroring.
        if sender.send(Message::Updated(snapshot)).await.is_err() {
            return Ok(());
        }

        // Park until any watched property changes, then loop and re-read.
        if changed.next().await.is_none() {
            return Ok(());
        }
    }
}

/// UPower `State` → is the battery charging? (1 = charging.) Everything
/// else — discharging (2), unknown (0), fully-charged (4), ... — is "at
/// rest" per the one rule: only *live* gets the terracotta accent.
///
/// Pure function, unit-tested below; the D-Bus plumbing above is not.
fn is_charging(state: u32) -> bool {
    state == UPOWER_STATE_CHARGING
}

/// Percentage + charging → which Lucide glyph the readout shows. Charging
/// always wins (the bolt is the "live" state's own shape, matching its
/// accent tint); otherwise the charge ladder: full ≥ 75, medium ≥ 40,
/// low ≥ 15, and the bare outline below that. Thresholds sit between
/// Lucide's own three fill bars — coarse on purpose: the glyph answers
/// "roughly how much is left", the popover has the exact number.
///
/// `pub(crate)`: `popovers::quick_settings::battery_row` reuses this exact
/// mapping beside its percentage text, rather than a second hand-copied
/// ladder that could drift from the bar's.
///
/// Pure function of its arguments (no D-Bus, no globals) — which is what
/// makes it unit-testable without a bus.
pub(crate) fn battery_icon(percentage: f64, charging: bool) -> Icon {
    if charging {
        Icon::BatteryCharging
    } else if percentage >= 75.0 {
        Icon::BatteryFull
    } else if percentage >= 40.0 {
        Icon::BatteryMedium
    } else if percentage >= 15.0 {
        Icon::BatteryLow
    } else {
        Icon::Battery
    }
}

/// Seconds → a terse duration label: `"2h 14m"` at an hour or more,
/// `"14m"` below that, `"<1m"` under a minute.
///
/// Terse on purpose. This sits immediately next to the percentage in the
/// quick-settings popover, where the surrounding text already supplies the
/// noun ("until full") — spelling out "2 hours 14 minutes" would push the row
/// wider than the popover for no added meaning. `"<1m"` rather than `"0m"`
/// because a battery reporting a positive number of seconds is not empty, and
/// a `0m` next to a live percentage reads as a bug.
///
/// Callers gate on `secs > 0` before reaching here ([`Battery::time_remaining`]
/// is the only one), so the zero/negative case never renders; the `<1m` arm
/// still catches it defensively rather than producing something stranger.
///
/// Pure function of its argument (no D-Bus, no globals) — which is what makes
/// it unit-testable without a bus.
fn duration_label(secs: i64) -> String {
    if secs < 60 {
        return "<1m".to_string();
    }
    let minutes = secs / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charging_always_shows_the_bolt() {
        assert_eq!(battery_icon(3.0, true), Icon::BatteryCharging);
        assert_eq!(battery_icon(100.0, true), Icon::BatteryCharging);
    }

    #[test]
    fn the_charge_ladder_steps_at_its_documented_thresholds() {
        assert_eq!(battery_icon(100.0, false), Icon::BatteryFull);
        assert_eq!(battery_icon(75.0, false), Icon::BatteryFull);
        assert_eq!(battery_icon(74.9, false), Icon::BatteryMedium);
        assert_eq!(battery_icon(40.0, false), Icon::BatteryMedium);
        assert_eq!(battery_icon(39.9, false), Icon::BatteryLow);
        assert_eq!(battery_icon(15.0, false), Icon::BatteryLow);
        assert_eq!(battery_icon(14.9, false), Icon::Battery);
        assert_eq!(battery_icon(0.0, false), Icon::Battery);
    }

    #[test]
    fn only_upower_state_charging_is_live() {
        assert!(is_charging(1)); // charging
        assert!(!is_charging(0)); // unknown
        assert!(!is_charging(2)); // discharging
        assert!(!is_charging(4)); // fully charged: plugged in, but not live
    }

    #[test]
    fn duration_label_shows_hours_and_minutes_past_an_hour() {
        assert_eq!(duration_label(2 * 3600 + 14 * 60), "2h 14m");
        assert_eq!(duration_label(3600), "1h 0m");
        // Truncates rather than rounds — the estimate is far coarser than
        // the seconds it's reported in, so precision here would be theatre.
        assert_eq!(duration_label(3600 + 59), "1h 0m");
    }

    #[test]
    fn duration_label_drops_the_hours_below_an_hour() {
        assert_eq!(duration_label(14 * 60), "14m");
        assert_eq!(duration_label(60), "1m");
        assert_eq!(duration_label(3599), "59m");
    }

    #[test]
    fn duration_label_collapses_under_a_minute() {
        assert_eq!(duration_label(59), "<1m");
        assert_eq!(duration_label(1), "<1m");
        // Defensive only — `time_remaining` never calls with these.
        assert_eq!(duration_label(0), "<1m");
        assert_eq!(duration_label(-5), "<1m");
    }

    /// A `Battery` in one specific state, so the tests below read as the
    /// scenario they're pinning rather than a wall of struct literals.
    fn battery(charging: bool, to_empty: i64, to_full: i64) -> Battery {
        Battery {
            percentage: 62.0,
            charging,
            present: true,
            time_to_empty_secs: to_empty,
            time_to_full_secs: to_full,
            energy_rate_watts: 0.0,
        }
    }

    #[test]
    fn discharging_reads_time_to_empty() {
        let b = battery(false, 2 * 3600 + 14 * 60, 0);
        assert_eq!(b.time_remaining().as_deref(), Some("2h 14m"));
    }

    #[test]
    fn charging_reads_time_to_full() {
        let b = battery(true, 0, 40 * 60);
        assert_eq!(b.time_remaining().as_deref(), Some("40m"));
    }

    /// The reason the choice is keyed off `charging` and not "whichever is
    /// non-zero": right after a cable event UPower can briefly report both,
    /// and the label must agree with the glyph beside it.
    #[test]
    fn the_charging_flag_decides_which_field_wins_when_both_are_set() {
        assert_eq!(
            battery(true, 90 * 60, 20 * 60).time_remaining().as_deref(),
            Some("20m")
        );
        assert_eq!(
            battery(false, 90 * 60, 20 * 60).time_remaining().as_deref(),
            Some("1h 30m")
        );
    }

    #[test]
    fn an_unknown_estimate_has_no_label() {
        // UPower's `0` = "no estimate yet" on whichever field is relevant.
        assert!(battery(false, 0, 45 * 60).time_remaining().is_none());
        assert!(battery(true, 45 * 60, 0).time_remaining().is_none());
        // And a nonsense negative is treated the same way.
        assert!(battery(false, -1, 0).time_remaining().is_none());
    }

    #[test]
    fn an_absent_battery_has_no_label() {
        // Same "quiet until proven otherwise" contract as `view`'s early
        // return: a stale estimate must not outlive the battery it describes.
        assert!(Battery::default().time_remaining().is_none());
    }

    #[test]
    fn power_draw_labels_the_rate_to_one_decimal() {
        let b = Battery {
            energy_rate_watts: 9.46,
            ..battery(false, 0, 0)
        };
        assert_eq!(b.power_draw().as_deref(), Some("9.5W"));
    }

    /// Some firmware reports the charging direction as a negative rate;
    /// the label shows the magnitude, since the charging glyph beside it
    /// already tells the direction.
    #[test]
    fn power_draw_drops_the_sign_while_charging() {
        let b = Battery {
            energy_rate_watts: -22.3,
            ..battery(true, 0, 40 * 60)
        };
        assert_eq!(b.power_draw().as_deref(), Some("22.3W"));
    }

    #[test]
    fn an_unknown_rate_has_no_power_label() {
        // UPower's `0.0` = "no reading", same convention as time_to_empty.
        assert!(battery(false, 0, 0).power_draw().is_none());
    }

    #[test]
    fn an_absent_battery_has_no_power_label() {
        assert!(Battery::default().power_draw().is_none());
    }

    #[test]
    fn accessors_read_through_to_the_stored_fields() {
        let b = battery(true, 0, 60);
        assert_eq!(b.percentage(), 62.0);
        assert!(b.charging());
        assert!(b.is_present());
    }
}
