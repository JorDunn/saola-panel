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
//! module as a **bare Lucide icon + label sitting directly on the ink bar** —
//! no pill fill at all. A resting readout ("78%") is information, not a
//! control that is switched on, so it gets no fill and no accent: plain ivory
//! (`on_ink.primary`) glyph and text.
//!
//! Terracotta is reserved for the *live* state — here, **charging** — and even
//! then it is a small accent, not a flood: the glyph and the label take
//! `palette.accent_light`, and nothing behind them changes. That restraint is
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
use iced::widget::{row, text, Space};
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
}

impl Battery {
    /// Renders the battery glyph + percentage, or nothing at all when no
    /// battery is present (`Space::new()` with no size is a zero-area widget
    /// — the row simply closes up around it).
    ///
    /// There is no `button` and no pill here: this is a bare row of icon and
    /// text drawn straight onto the ink bar, per the concept (see the module
    /// doc comment). Nothing but the *color* changes between states, which is
    /// what keeps the right region from reflowing when the cable goes in.
    ///
    /// Teaching note (why one `color` binding for both widgets): an iced
    /// `Svg` never inherits a text color from anything above it — the tint
    /// has to be handed to `icons::icon` explicitly — while `text` needs its
    /// own `.color(..)`. Computing the role once and passing the same
    /// `iced::Color` to both is what guarantees the glyph and the label can't
    /// drift apart as this module grows. `ColorExt::into_iced` is the
    /// conversion from a `saola_theme` color to iced's own `Color` type;
    /// every theme role has to make that hop before a widget will take it.
    ///
    /// Spacing note: `sizes.bar_icon_gap` (7.0) is the icon↔value gap inside
    /// one status readout. The `island_gap` is for gaps between modules, not
    /// within them.
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module (a `view` that returns a zero-sized `Space` is invisible on
    /// the ledger bar, but wrapped in its own scrim pill it would be an
    /// empty 40px blob). Reads the same field as `view`'s early return, so
    /// the two cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.present
    }

    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if !self.present {
            return Space::new().into();
        }

        // The one rule, in its bare-status form: terracotta *accent* while
        // charging (live), plain ivory at rest. Charging tints only the glyph
        // and the digits — never a fill behind them.
        let color = if self.charging {
            theme.palette.accent_light
        } else {
            theme.on_ink.primary
        }
        .into_iced();

        row![
            icons::icon(Icon::Battery, theme.sizes.icon_bar, color),
            text(battery_label(self.percentage))
                .size(theme.typography.size.bar)
                .color(color),
        ]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center)
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
    // `()` — which also gives all three the same item type, letting
    // `stream::select` merge them. These streams don't hold a borrow of
    // `proxy` (they clone its internals), so the getters below stay usable.
    //
    // UPower quirk: each stream also fires once immediately when the
    // property cache is already warm, so a couple of redundant snapshots
    // arrive right after boot. Harmless — the view just re-renders.
    let mut changed = {
        let percentage = proxy.receive_percentage_changed().await;
        let state = proxy.receive_state_changed().await;
        let present = proxy.receive_is_present_changed().await;
        stream::select(
            percentage.map(|_| ()),
            stream::select(state.map(|_| ()), present.map(|_| ())),
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

/// Percentage → the module's label, e.g. `87%`. Clamped defensively so a
/// misreporting UPower can't render `-3%` or `104%`.
///
/// Battery keeps its `%` suffix (volume drops its own — a charge level reads
/// as a proportion, a volume level as a dial position), and leans on the
/// theme's bar font having tabular numerals so the row doesn't jitter as
/// digits change.
///
/// Pure function of its argument (no D-Bus, no globals) — which is what
/// makes it unit-testable without a bus.
fn battery_label(percentage: f64) -> String {
    format!("{:.0}%", percentage.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_shows_whole_percent() {
        assert_eq!(battery_label(87.0), "87%");
        assert_eq!(battery_label(100.0), "100%");
        assert_eq!(battery_label(0.0), "0%");
    }

    #[test]
    fn label_rounds_fractional_percentages() {
        assert_eq!(battery_label(87.4), "87%");
        assert_eq!(battery_label(87.6), "88%");
    }

    #[test]
    fn label_clamps_out_of_range_values() {
        assert_eq!(battery_label(-3.0), "0%");
        assert_eq!(battery_label(104.0), "100%");
    }

    #[test]
    fn only_upower_state_charging_is_live() {
        assert!(is_charging(1)); // charging
        assert!(!is_charging(0)); // unknown
        assert!(!is_charging(2)); // discharging
        assert!(!is_charging(4)); // fully charged: plugged in, but not live
    }
}
