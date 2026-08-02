//! Power profiles, fed by power-profiles-daemon over D-Bus.
//!
//! This is the panel's **first popover-only module**: it has no `view` at
//! all, no `config::ModuleName`, and no slot in any bar region. It exists so
//! the quick-settings popover can show (and switch) the machine's active
//! power profile — "power-saver", "balanced", "performance" — while still
//! being a module in every other sense: its own state struct, its own
//! `Message`, its own `subscription`, and the same zbus→iced bridge shape
//! `battery.rs` established. Read `battery.rs` first; the worker here is a
//! near-verbatim copy of `watch_upower` and every teaching note there
//! (channel bridge, ownership across the async boundary, `Subscription::run`
//! fn-pointer identity) applies unchanged.
//!
//! # Which bus name (a real trap)
//!
//! power-profiles-daemon ≥0.20 owns **two** well-known names for backwards
//! compatibility: the modern `org.freedesktop.UPower.PowerProfiles` and the
//! legacy `net.hadess.PowerProfiles` (its pre-freedesktop name). They serve
//! the same object, but only the freedesktop one is guaranteed to survive the
//! deprecation, so this module uses it exclusively. Despite the name, the
//! daemon is *not* part of UPower — it merely lives under UPower's namespace,
//! which is why `battery.rs`'s proxy can't be reused here.
//!
//! # Absent daemon
//!
//! Same "quiet until proven otherwise" contract as [`super::battery`]: a
//! `Power::default()` has `present: false`, so a machine with no
//! power-profiles-daemon (or one where the bus itself is unreachable) leaves
//! the quick-settings row hidden and never takes the panel down.

use std::collections::HashMap;

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, StreamExt};
use iced::futures::{SinkExt, Stream};
use iced::{Subscription, Task};
use zbus::zvariant::OwnedValue;
use zbus::Connection;

/// The power module's own message type (Stage 7's per-module refactor — see
/// `modules::clock::Message` for the full teaching note on why every module
/// owns its `Message`). `main.rs` nests this as
/// `Message::Power(power::Message)`.
#[derive(Debug, Clone)]
pub enum Message {
    /// A fresh snapshot from the worker.
    Updated(Power),
    /// A quick-settings profile chip was clicked; the payload is the profile
    /// name to switch to (one of [`Power::profiles`]). Resolves to the
    /// one-shot command-out `Task` [`set_profile`] builds — see that
    /// function, and `modules::media`'s "command-out pattern" section for
    /// the reasoning this copies.
    SetProfile(String),
}

/// The daemon's dictionary key naming a profile inside the `Profiles`
/// property's `aa{sv}` payload. Each entry is a dict of attributes
/// (`Profile`, `Driver`, `PlatformDriver`, …); only the name interests us.
const PROFILE_KEY: &str = "Profile";

/// A zbus proxy for power-profiles-daemon.
///
/// See `battery.rs`'s `UPowerDevice` proxy for the full `#[zbus::proxy]`
/// teaching note (what the macro generates, why the trait itself is never
/// implemented, how snake_case method names become PascalCase D-Bus
/// property names).
///
/// Teaching note (a *writable* property): `ActiveProfile` is the panel's
/// first D-Bus property we ever write rather than only read. The proxy macro
/// spells that as a second method whose name is the getter's with a `set_`
/// prefix — `set_active_profile` — and it maps to the standard
/// `org.freedesktop.DBus.Properties.Set` call rather than a bespoke method.
/// The daemon is free to refuse it (polkit sits in front of the write on
/// some setups), which is exactly why [`set_profile`] treats failure as
/// "log it and carry on" rather than an error worth surfacing.
#[zbus::proxy(
    interface = "org.freedesktop.UPower.PowerProfiles",
    default_service = "org.freedesktop.UPower.PowerProfiles",
    default_path = "/org/freedesktop/UPower/PowerProfiles"
)]
trait PowerProfiles {
    /// The profile currently in effect, e.g. `"balanced"`.
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;

    /// Switch profiles. See the proxy's doc comment for why this is a
    /// property *setter* rather than a method.
    #[zbus(property)]
    fn set_active_profile(&self, profile: &str) -> zbus::Result<()>;

    /// Every profile this machine's driver supports, in the daemon's own
    /// order (least to most performant) — `aa{sv}`, an array of attribute
    /// dictionaries rather than a plain array of names. See
    /// [`profile_names`] for the extraction, and `media.rs`'s
    /// `extract_title_artist` for the general zvariant-ownership gotchas
    /// that shape it.
    #[zbus(property)]
    fn profiles(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}

/// Power module state: the last snapshot the D-Bus worker pushed through
/// [`Message::Updated`]. Cached for the same reason as `Battery` — reading
/// D-Bus during `view` would block the UI thread.
///
/// `Default` is the boot state (`present: false`), i.e. "no
/// power-profiles-daemon known yet", so "daemon absent", "bus unreachable",
/// and "the worker hasn't reported yet" all render identically: as nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Power {
    /// The profile currently in effect (`""` until the worker reports).
    active: String,
    /// The profiles this machine supports, in the daemon's order.
    profiles: Vec<String>,
    /// False when power-profiles-daemon isn't there (or isn't known yet) —
    /// the quick-settings row draws nothing.
    present: bool,
}

impl Power {
    /// Whether the daemon has reported at all — the presence question the
    /// quick-settings popover asks before spending a row on this module,
    /// exactly like `Battery::is_present` guards the bar's battery readout.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// The profile currently in effect, e.g. `"balanced"`. Empty before the
    /// worker's first report (which is also when `is_present` is false, so a
    /// caller that checks presence first never sees the empty string).
    pub fn active(&self) -> &str {
        &self.active
    }

    /// Every profile this machine supports, in the daemon's own order — the
    /// order a row of chips should be laid out in, since the daemon sorts
    /// least- to most-performant.
    pub fn profiles(&self) -> &[String] {
        &self.profiles
    }

    /// The power-profiles-daemon feed as an iced subscription. See
    /// `battery.rs`'s `subscription` for the function-pointer-identity
    /// teaching note — identical reasoning applies verbatim.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(power_stream)
    }
}

/// Builds the async stream the subscription runs. See `battery.rs`'s
/// `battery_stream` for the full bridge teaching note (the channel, the
/// runtime it runs on, why every D-Bus type stays inside the worker).
fn power_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        // Any D-Bus failure — no system bus, no power-profiles-daemon on it,
        // a property read failing — lands here. Report "not present" so the
        // quick-settings row hides, and let the worker end quietly.
        if watch_power_profiles(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(Power::default())).await;
        }
    })
}

/// The worker proper: connect, read once, then re-read and push a snapshot
/// on every property change, forever. Structurally identical to
/// `battery.rs`'s `watch_upower`.
async fn watch_power_profiles(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // System bus: power-profiles-daemon is a machine-wide hardware service,
    // like UPower and iwd — not a per-user desktop application.
    let connection = Connection::system().await?;
    let proxy = PowerProfilesProxy::new(&connection).await?;

    // One merged "something changed" stream, same normalize-to-`()`-and-
    // `stream::select` shape as `battery.rs`. `Profiles` changes far more
    // rarely than `ActiveProfile` (only if the driver's capabilities change
    // — a laptop losing its performance profile on battery, say), but it
    // *can* change, so it's watched rather than read once.
    let mut changed = {
        let active = proxy.receive_active_profile_changed().await;
        let profiles = proxy.receive_profiles_changed().await;
        stream::select(active.map(|_| ()), profiles.map(|_| ()))
    };

    loop {
        // Served from zbus's local property cache once warm (kept fresh by
        // the same `PropertiesChanged` signals driving the streams above),
        // so re-reading everything per event is cheap.
        let snapshot = Power {
            active: proxy.active_profile().await?,
            profiles: profile_names(proxy.profiles().await?),
            present: true,
        };

        // A send error means the receiving side is gone (the subscription
        // was dropped) — stop quietly rather than erroring.
        if sender.send(Message::Updated(snapshot)).await.is_err() {
            return Ok(());
        }

        // Park until either watched property changes, then loop and re-read.
        if changed.next().await.is_none() {
            return Ok(());
        }
    }
}

/// Pulls each entry's `"Profile"` name out of the decoded `aa{sv}` payload,
/// dropping any entry that doesn't carry one (a malformed or future-shaped
/// entry is skipped rather than rendered as an empty chip).
///
/// Teaching note (why this takes the *decoded* type): the whole point of
/// splitting this out of the worker is that it is a pure function of a plain
/// Rust value — no proxy, no bus, no `async` — so it can be unit-tested by
/// building the maps by hand (see the tests below). Taking `Vec<HashMap<..>>`
/// **by value** also sidesteps zvariant's ownership gotcha: `String:
/// TryFrom<OwnedValue>` is a by-*value* conversion, so owning the map lets us
/// `.remove(..)` the key straight out of it instead of fighting for a
/// borrowed conversion that doesn't exist (`media.rs`'s
/// `extract_title_artist` documents that trap in full).
fn profile_names(entries: Vec<HashMap<String, OwnedValue>>) -> Vec<String> {
    entries
        .into_iter()
        .filter_map(|mut entry| {
            entry
                .remove(PROFILE_KEY)
                .and_then(|value| String::try_from(value).ok())
        })
        .collect()
}

/// Asks power-profiles-daemon to switch to `profile`.
///
/// The command-out pattern, copied from `modules::media` (read that module's
/// "command-out pattern" section for the reasoning): a **fresh, one-shot
/// system-bus connection**, one property write, connection dropped.
/// `Task::future(..).discard()` runs it to completion and throws away the
/// `()` result — `Panel::update` has nothing useful to do with success or
/// failure beyond the `eprintln!` below.
///
/// Failure is expected in normal operation, not exceptional: writing
/// `ActiveProfile` is a privileged action on some setups and polkit may
/// refuse it outright. So every path here logs and returns — never panics,
/// never unwraps — and the next `PropertiesChanged` (or the absence of one)
/// leaves the UI showing whatever the daemon actually did.
pub fn set_profile(profile: String) -> Task<Message> {
    Task::future(send_set_profile(profile)).discard()
}

async fn send_set_profile(profile: String) {
    let Ok(connection) = Connection::system().await else {
        return;
    };
    let Ok(proxy) = PowerProfilesProxy::new(&connection).await else {
        return;
    };
    if let Err(error) = proxy.set_active_profile(&profile).await {
        eprintln!("saola-panel: setting power profile to {profile:?} failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    /// One `a{sv}` entry as the daemon shapes it: the profile name plus the
    /// extra attributes it also carries (which this module ignores).
    fn entry(profile: &str) -> HashMap<String, OwnedValue> {
        HashMap::from([
            (
                PROFILE_KEY.to_string(),
                OwnedValue::try_from(Value::from(profile)).unwrap(),
            ),
            (
                "Driver".to_string(),
                OwnedValue::try_from(Value::from("intel_pstate")).unwrap(),
            ),
        ])
    }

    #[test]
    fn profile_names_preserve_the_daemons_order() {
        let entries = vec![
            entry("power-saver"),
            entry("balanced"),
            entry("performance"),
        ];
        assert_eq!(
            profile_names(entries),
            vec![
                "power-saver".to_string(),
                "balanced".to_string(),
                "performance".to_string()
            ]
        );
    }

    #[test]
    fn entries_without_a_profile_key_are_skipped() {
        let nameless = HashMap::from([(
            "Driver".to_string(),
            OwnedValue::try_from(Value::from("placeholder")).unwrap(),
        )]);
        let entries = vec![entry("balanced"), nameless, entry("performance")];
        assert_eq!(
            profile_names(entries),
            vec!["balanced".to_string(), "performance".to_string()]
        );
    }

    #[test]
    fn a_non_string_profile_value_is_skipped_rather_than_rendered() {
        // Defensive: the daemon declares `Profile` as a string, but the
        // payload is variant-typed, so nothing on the wire *enforces* that.
        let wrong_type = HashMap::from([(
            PROFILE_KEY.to_string(),
            OwnedValue::try_from(Value::from(42u32)).unwrap(),
        )]);
        assert!(profile_names(vec![wrong_type]).is_empty());
    }

    #[test]
    fn no_profiles_yields_no_names() {
        assert!(profile_names(Vec::new()).is_empty());
    }

    #[test]
    fn default_power_is_absent_and_empty() {
        // The "quiet until proven otherwise" contract: before the worker's
        // first report the quick-settings row must draw nothing.
        let power = Power::default();
        assert!(!power.is_present());
        assert_eq!(power.active(), "");
        assert!(power.profiles().is_empty());
    }

    #[test]
    fn accessors_read_through_to_the_stored_fields() {
        let power = Power {
            active: "balanced".to_string(),
            profiles: vec!["power-saver".to_string(), "balanced".to_string()],
            present: true,
        };
        assert!(power.is_present());
        assert_eq!(power.active(), "balanced");
        assert_eq!(power.profiles(), ["power-saver", "balanced"]);
    }
}
