//! The bar's Wi-Fi readout, fed by iwd over D-Bus.
//!
//! This module copies the zbus→iced bridge pattern battery.rs established
//! (see that module's doc comment for the full shape): an async worker owns
//! the D-Bus proxies, listens to property-change streams, and pushes
//! snapshots into the panel through an `iced::stream` channel wrapped in
//! `Subscription::run`. The one real difference from battery: UPower's
//! `DisplayDevice` object always exists at a fixed path, but iwd's Station
//! object lives at a path that depends on which wifi adapter is present
//! (`/net/connman/iwd/phy0/1`, say) — so this worker has to *find* it first,
//! via the standard `org.freedesktop.DBus.ObjectManager` interface, before
//! it can proxy it.
//!
//! The signal-strength readout is the module's second D-Bus direction: as
//! well as *consuming* iwd's interfaces, the panel **serves** one of its own
//! (`net.connman.iwd.SignalLevelAgent`) and hands iwd its object path — see
//! [`SignalLevelAgent`] and [`register_signal_agent`] for why that is the
//! only signal-driven way to learn the signal strength, and `tray/watcher.rs`
//! for the other place this crate serves an interface.
//!
//! # Design language
//!
//! Like every other right-region status module (see `battery.rs` for the full
//! note), this renders as a **bare Lucide glyph directly on the ink bar** —
//! no pill, no fill, and (as of 2026-08-01) no text: [`wifi_icon`]'s arc
//! ladder carries the strength, and the SSID plus the exact percentage live
//! in the quick-settings popover only.
//!
//! Being connected is deliberately **not** a terracotta state, which is the
//! one place this module diverges from what an earlier stage built. Terracotta
//! marks something that is switched *on* or actively live, and the style guide
//! allows at most one terracotta element per surface; "connected to HomeNet"
//! is the resting, expected condition of a laptop, so painting it accent would
//! spend the bar's whole accent budget on a permanent state. Connected is
//! therefore plain ivory (`on_ink.primary`), exactly like the clock.
//!
//! Disconnected-but-present is *quieter* still: the `wifi-off` glyph in the
//! `on_ink.secondary` role — not a custom gray, and not a dimmed pill. If
//! iwd isn't on the bus at all (or no Station object is found), the module
//! renders nothing, same as battery with no UPower.

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, StreamExt};
use iced::futures::{SinkExt, Stream};
use iced::widget::Space;
use iced::{Element, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;
use zbus::fdo::ObjectManagerProxy;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use zbus::Connection;

use crate::icons::{self, Icon};

/// The network module's own message type (Stage 7's per-module refactor —
/// see `modules::clock::Message` for the full teaching note). `main.rs`
/// nests this as `Message::Network(network::Message)`; `Panel::update`
/// delegates by matching through both layers:
/// `Message::Network(network::Message::Updated(n))`.
///
/// Single variant, same payload as the old flat enum's `NetworkUpdated`.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Network),
}

/// iwd's D-Bus service name — never NetworkManager (Jordan runs iwd).
const IWD_SERVICE: &str = "net.connman.iwd";

/// The interface name we scan `ObjectManager`'s output for, to find the
/// Station object. A plain string compare (`InterfaceName::as_str`) rather
/// than a typed proxy call — we don't need to *use* this interface's
/// properties on the ObjectManager reply itself, just spot which object path
/// implements it.
const STATION_INTERFACE: &str = "net.connman.iwd.Station";

/// Where the panel serves its own [`SignalLevelAgent`] object. Any path is
/// allowed (iwd only ever hands it back to us), so this follows the crate's
/// `io.saola` namespace rather than borrowing iwd's.
const SIGNAL_AGENT_PATH: &str = "/io/saola/panel/SignalLevelAgent";

/// The RSSI thresholds (dBm, **descending** — iwd rejects any other order)
/// the signal-level agent is registered with. Nine thresholds cut the range
/// into ten buckets, which [`level_percent`] turns into 100%…10%.
///
/// Teaching note (why thresholds at all): iwd deliberately offers no
/// "current RSSI" property. Radio signal is noisy and would fire a
/// `PropertiesChanged` several times a second; instead the client declares
/// the boundaries it actually cares about and iwd reports only when one is
/// crossed. That is what makes this module signal-driven rather than a poll
/// (CLAUDE.md: every module maps to a signal) — the cost is that the
/// percentage is a bucketed approximation, never a live dBm reading.
const SIGNAL_LEVELS: [i16; 9] = [-50, -55, -60, -65, -70, -75, -80, -85, -90];

/// A zbus proxy for iwd's per-adapter Station object.
///
/// Teaching note (no `default_path`): unlike UPower's `DisplayDevice`
/// (battery.rs), a Station's object path is not fixed — it depends on which
/// wifi radio iwd has bound (`/net/connman/iwd/{phy0,phy1,...}/{1,2,...}`
/// per iwd's own `doc/station-api.txt`). Leaving `default_path` off the
/// macro attribute is what makes the generated `StationProxy::new` take the
/// path as a second argument instead of assuming one; `watch_iwd` below
/// finds the real path first, via `ObjectManager`.
#[zbus::proxy(
    interface = "net.connman.iwd.Station",
    default_service = "net.connman.iwd"
)]
trait Station {
    /// Connection state: one of "connected", "disconnected", "connecting",
    /// "disconnecting", "roaming" (`doc/station-api.txt`). Only "connected"
    /// and "roaming" count as showing a settled SSID here.
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;

    /// The `net.connman.iwd.Network` object path currently connected (or
    /// connecting) to. iwd's docs mark this property *optional*: it is
    /// absent from the object entirely while disconnected, rather than
    /// present-and-empty, so reading it then is expected to fail — see
    /// `watch_iwd`'s `.ok()` handling below, never a bare `?`.
    #[zbus(property)]
    fn connected_network(&self) -> zbus::Result<OwnedObjectPath>;

    /// Ask iwd to start calling our [`SignalLevelAgent`] at `path` whenever
    /// the connection's RSSI crosses one of `levels` (dBm, descending).
    ///
    /// Teaching note (a method, not a property): the two `#[zbus(property)]`
    /// entries above are *state we read*; these two are *calls we make*, so
    /// they carry no attribute and the macro generates a plain async method
    /// that sends a real D-Bus method call every time. iwd keeps at most one
    /// agent per client per station, so a second registration from this
    /// process is an error — see [`register_signal_agent`], which treats
    /// every failure here as "no strength readout", never as fatal.
    fn register_signal_level_agent(
        &self,
        path: &ObjectPath<'_>,
        levels: &[i16],
    ) -> zbus::Result<()>;

    /// The inverse of the above. Not called on the happy path — dropping the
    /// `Connection` when the worker ends tells iwd the same thing — but it
    /// is what lets [`register_signal_agent`] clear a stale registration left
    /// behind by an earlier worker on a re-subscribe.
    fn unregister_signal_level_agent(&self, path: &ObjectPath<'_>) -> zbus::Result<()>;
}

/// What the served agent posts into the worker.
///
/// `Released` is not the same as "signal lost": it means iwd has dropped the
/// agent (an `Unregister` call, or iwd shutting down), after which no further
/// levels will arrive — so the honest reading is "strength unknown", i.e.
/// `None`, rather than a stale bar.
#[derive(Debug, Clone, Copy)]
enum LevelEvent {
    /// iwd's bucket index: `0` = stronger than the first threshold, and it
    /// grows as the signal weakens. [`level_percent`] does the mapping.
    Changed(u8),
    Released,
}

/// The `net.connman.iwd.SignalLevelAgent` object the panel serves, so iwd
/// has something to call back into.
///
/// Teaching note (serving vs. consuming): `#[zbus::proxy]` above generates a
/// *client* for somebody else's interface; `#[zbus::interface]` below is the
/// mirror image — it makes this struct answer D-Bus calls addressed to
/// [`SIGNAL_AGENT_PATH`] on our connection. Once handed to
/// `ObjectServer::at`, the value is owned by zbus (behind a lock) and its
/// methods run on zbus's own task, *not* on the worker's — which is exactly
/// why it holds a channel `Sender` instead of a `&mut` into the worker's
/// state: a channel is the only sane way for those two tasks to talk.
///
/// Unbounded because a D-Bus method handler must never be made to wait on
/// the UI, and the traffic is a handful of messages per hour of Wi-Fi use.
struct SignalLevelAgent {
    levels: mpsc::UnboundedSender<LevelEvent>,
}

#[zbus::interface(name = "net.connman.iwd.SignalLevelAgent")]
impl SignalLevelAgent {
    /// iwd reporting that the signal crossed into a new bucket. It also
    /// calls this once shortly after registration and after each connect,
    /// which is what spares this module an initial poll.
    ///
    /// The `device` argument names which station the level belongs to; this
    /// module only ever registers with one, so it is ignored (`_device`) —
    /// the parameter still has to be *declared*, because its type is part of
    /// the method's D-Bus signature (`oy`).
    fn changed(&self, _device: ObjectPath<'_>, level: u8) {
        // A failed send means the worker is gone; nothing to do about it
        // here, and erroring back at iwd would not help.
        let _ = self.levels.unbounded_send(LevelEvent::Changed(level));
    }

    /// iwd letting go of the agent (unregistered, or iwd is going away).
    fn release(&self, _device: ObjectPath<'_>) {
        let _ = self.levels.unbounded_send(LevelEvent::Released);
    }
}

/// A zbus proxy for a `net.connman.iwd.Network` object (one candidate/known
/// network, not the connection itself — that's `Station`).
///
/// Teaching note (the name): the D-Bus interface is `net.connman.iwd.Network`,
/// but that name is already taken in this file by [`Network`], the module's
/// *state* struct (mirroring `Battery` in battery.rs). Trait and struct
/// names share Rust's type namespace, so the proxy trait is named
/// `IwdNetwork` instead — `#[zbus::proxy]` only cares about the `interface =`
/// string, not the trait's Rust name, so this rename changes nothing on the
/// wire.
#[zbus::proxy(
    interface = "net.connman.iwd.Network",
    default_service = "net.connman.iwd"
)]
trait IwdNetwork {
    /// The network's SSID.
    #[zbus(property)]
    fn name(&self) -> zbus::Result<String>;
}

/// Network module state: the last snapshot the D-Bus worker pushed through
/// [`Message::Updated`]. Like `Battery`, this module caches —
/// reading D-Bus during `view` would block the UI thread.
///
/// `Default` is the boot state: `present: false`, `ssid: None` — "no Station
/// known yet", so boot, no wifi adapter, and iwd-missing all render
/// identically (nothing), same contract as `Battery::default()`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Network {
    /// The connected network's SSID. `None` covers both "iwd reports
    /// disconnected/connecting" and "connected but the SSID hasn't been
    /// read yet" — either way the module shows the quiet offline state,
    /// never a stale or blank SSID.
    ssid: Option<String>,
    /// Signal strength as a bucketed percentage (see [`level_percent`]).
    /// `None` means "not known": disconnected, or connected on an iwd that
    /// wouldn't take our signal-level agent. Never `Some` while `ssid` is
    /// `None` — the worker clears it whenever the link is not settled.
    strength_percent: Option<u8>,
    /// Whether a Station object was found on the bus at all. `false` covers
    /// "iwd isn't running" *and* "no wifi adapter" — the module renders
    /// nothing regardless of `ssid` (mirrors `Battery::present`).
    present: bool,
}

impl Network {
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Reads the same field as `view`'s early return, so the two
    /// cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// The connected network's SSID, or `None` when offline — the same
    /// value the bar label is built from, for the quick-settings popover to
    /// read. (`as_deref` hands out a borrow rather than cloning the
    /// `String`: the caller only ever needs to display it.)
    pub fn ssid(&self) -> Option<&str> {
        self.ssid.as_deref()
    }

    /// Signal strength as a bucketed percentage, or `None` when it isn't
    /// known (offline, or an iwd that refused the signal-level agent). Also
    /// for the quick-settings popover; `Copy`, so no borrow needed.
    pub fn strength_percent(&self) -> Option<u8> {
        self.strength_percent
    }

    /// Renders the leveled Wi-Fi glyph, or nothing when no Station was ever
    /// found. Icon-only as of 2026-08-01 (Jordan: the glyph carries the
    /// level, the SSID and exact strength live in the popover):
    /// [`wifi_icon`]'s arc ladder shows roughly how strong the link is, and
    /// `wifi-off` in the quiet `secondary` role is the offline state — the
    /// same two treatments the old glyph + text row had, minus the text.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if !self.present {
            return Space::new().into();
        }

        let connected = self.ssid.is_some();
        let color = if connected {
            // Connected: full-emphasis ivory. Resting state, no accent.
            theme.on_ink.primary
        } else {
            // Offline: the quiet role.
            theme.on_ink.secondary
        }
        .into_iced();

        icons::icon(
            wifi_icon(connected, self.strength_percent),
            theme.sizes.icon_bar,
            color,
        )
        .into()
    }

    /// The network's D-Bus feed as an iced subscription. See battery.rs's
    /// `subscription` for the function-pointer-identity teaching note (why
    /// `Subscription::run`'s identity survives the `.map(crate::Message::
    /// Network)` `main.rs` applies) — the same reasoning applies verbatim.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(network_stream)
    }
}

/// Builds the async stream the subscription runs. See battery.rs's
/// `battery_stream` for the full bridge teaching note (the channel, the
/// runtime it runs on). Every failure path here — no system bus, iwd not
/// running, no Station object found — funnels into "send the hidden/default
/// state, worker ends quietly": the panel never goes down because Wi-Fi
/// isn't there.
fn network_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        if watch_iwd(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(Network::default())).await;
        }
    })
}

/// The worker proper: find the Station object, read once, then re-read and
/// push a snapshot on every property change, forever.
async fn watch_iwd(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // System bus: iwd (like UPower) is a system service, not per-session.
    let connection = Connection::system().await?;

    // Teaching note (ObjectManager discovery): iwd doesn't publish a fixed
    // "the station" path the way UPower publishes `DisplayDevice` — it can
    // manage more than one wifi radio, each with its own Station object
    // under `/net/connman/iwd/...`. `org.freedesktop.DBus.ObjectManager` is
    // the standard D-Bus mechanism for "ask a service to enumerate all its
    // objects and the interfaces each one implements"; iwd exposes it at
    // the bus root ("/"). `get_managed_objects()` returns every object path
    // iwd knows about, each mapped to its interfaces (and their
    // properties) — we only need the *path* of whichever object implements
    // `net.connman.iwd.Station`, so everything else in the reply is
    // discarded. v0.1 only wires up the first Station found; a machine with
    // two wifi radios would need a richer module (out of scope here).
    let object_manager = ObjectManagerProxy::builder(&connection)
        .destination(IWD_SERVICE)?
        .path("/")?
        .build()
        .await?;
    let objects = object_manager.get_managed_objects().await?;
    let station_path = objects
        .into_iter()
        .find_map(|(path, interfaces)| {
            interfaces
                .keys()
                .any(|name| name.as_str() == STATION_INTERFACE)
                .then_some(path)
        })
        .ok_or_else(|| zbus::Error::Failure("no iwd Station object found".to_string()))?;

    let station = StationProxy::new(&connection, station_path).await?;

    // One merged "the link changed" stream, exactly like battery's —
    // different item types per property, normalized to `()` so
    // `stream::select` can merge them; we re-read both properties on any
    // change rather than trying to apply a partial update.
    let changed = {
        let state = station.receive_state_changed().await;
        let connected_network = station.receive_connected_network_changed().await;
        stream::select(state.map(|_| ()), connected_network.map(|_| ()))
    };

    // The second source: signal levels, pushed by the object we serve. May
    // be `None` on an iwd that wouldn't take the agent — the module then
    // behaves exactly as it did before strength existed.
    let levels = register_signal_agent(&connection, &station).await;

    // Teaching note (`left_stream`/`right_stream`): `stream::select` needs
    // both halves to be the *same* Rust type, but "the receiver" and "no
    // receiver at all" are different types. These two combinators wrap
    // either one in a common `Either` stream so the `match` produces one
    // type. `stream::empty()` (not `pending()`) is the deliberate choice for
    // the absent case: an empty stream finishes immediately, so the merged
    // stream still ends when iwd's property stream ends, and the worker
    // still returns instead of parking forever.
    let level_events = match levels {
        Some(receiver) => receiver.left_stream(),
        None => stream::empty::<LevelEvent>().right_stream(),
    };

    let mut events = stream::select(changed.map(|_| Event::Link), level_events.map(Event::Level));

    // The worker's own view of the link, updated in place: `Event::Link`
    // re-reads it from iwd, `Event::Level` only touches `strength`. That is
    // the whole reason the loop no longer re-reads everything at the top —
    // a signal-level callback should not cost two D-Bus property reads.
    let (mut state, mut ssid) = read_link(&connection, &station).await?;
    let mut strength: Option<u8> = None;

    loop {
        // Only a settled "connected"/"roaming" state shows the SSID —
        // "connecting"/"disconnecting" are transitional, so (like battery's
        // "only *live* is charging" rule) they still render as offline
        // rather than flashing a stale or half-set SSID. Strength goes with
        // it: a percentage for a link we aren't showing is noise, and iwd's
        // last level would be stale the moment we disconnect.
        if !is_connected(&state) {
            ssid = None;
            strength = None;
        }

        let snapshot = Network {
            ssid: ssid.clone(),
            strength_percent: strength,
            present: true,
        };

        if sender.send(Message::Updated(snapshot)).await.is_err() {
            return Ok(());
        }

        match events.next().await {
            // Both sources are done — nothing left to report.
            None => return Ok(()),
            Some(Event::Link) => (state, ssid) = read_link(&connection, &station).await?,
            Some(Event::Level(LevelEvent::Changed(level))) => {
                strength = Some(level_percent(level));
            }
            // iwd dropped the agent: no further levels are coming, so stop
            // claiming to know the strength.
            Some(Event::Level(LevelEvent::Released)) => strength = None,
        }
    }
}

/// What woke the worker loop: iwd changed a Station property, or the agent
/// we serve reported a signal level. Merging two differently-shaped sources
/// means normalizing them to one item type, and an enum is the explicit way
/// to do that (the old code could map property changes to `()` because they
/// were the only source).
enum Event {
    /// A `State` / `ConnectedNetwork` change — re-read the link.
    Link,
    /// A callback from [`SignalLevelAgent`].
    Level(LevelEvent),
}

/// Reads the pair of Station properties the label is built from: the
/// connection state, and the SSID of the network it points at (if any).
///
/// `ConnectedNetwork` is an *optional* D-Bus property (iwd's own docs:
/// absent from the object entirely while disconnected, not
/// present-with-a-null-path) — reading it then fails at the D-Bus level,
/// which is expected, not a worker-ending error. `.ok()` turns "property
/// doesn't exist right now" into `None` instead of propagating a
/// `zbus::Error` through `?` and killing the loop. `State`, which always
/// exists, keeps its `?`.
async fn read_link(
    connection: &Connection,
    station: &StationProxy<'_>,
) -> zbus::Result<(String, Option<String>)> {
    let state = station.state().await?;

    let ssid = match station.connected_network().await.ok() {
        Some(path) => match IwdNetworkProxy::new(connection, path).await {
            Ok(network) => network.name().await.ok(),
            Err(_) => None,
        },
        None => None,
    };

    Ok((state, ssid))
}

/// Serves a [`SignalLevelAgent`] and hands its path to iwd, returning the
/// channel its callbacks arrive on — or `None` if any step failed.
///
/// **Every failure here is quiet by design.** An older iwd without the
/// method, or another agent already registered on this connection, must cost
/// the panel nothing more than the percentage: the SSID readout carries on
/// exactly as it did before this existed. That is why nothing in this
/// function uses `?` to reach `watch_iwd`'s fatal path — it logs and returns
/// `None`.
async fn register_signal_agent(
    connection: &Connection,
    station: &StationProxy<'_>,
) -> Option<mpsc::UnboundedReceiver<LevelEvent>> {
    // `ObjectPath` is a *validated* string type — D-Bus rejects malformed
    // paths on the wire, so zvariant checks at construction instead. Ours is
    // a const, so a failure here would be a typo in this file.
    let agent_path = match ObjectPath::try_from(SIGNAL_AGENT_PATH) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("saola-panel: bad signal-level agent path: {error}");
            return None;
        }
    };

    let (levels, receiver) = mpsc::unbounded();

    // Serve the object *before* registering: iwd may call `Changed` the
    // instant it accepts the agent, and a callback landing on a path we
    // haven't published yet would just bounce.
    //
    // `at` returns `Ok(false)` when something is already serving that path —
    // a leftover from an earlier worker on a re-subscribe. Its channel feeds
    // a worker that is gone, so replace it rather than inheriting it.
    match connection
        .object_server()
        .at(&agent_path, SignalLevelAgent { levels })
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            // Tell iwd to forget the stale registration too, then swap the
            // object. Both calls are best-effort: if either fails we simply
            // go without a strength readout.
            let _ = station.unregister_signal_level_agent(&agent_path).await;
            eprintln!("saola-panel: signal-level agent path already in use; not re-serving");
            return None;
        }
        Err(error) => {
            eprintln!("saola-panel: could not serve signal-level agent: {error}");
            return None;
        }
    }

    if let Err(error) = station
        .register_signal_level_agent(&agent_path, &SIGNAL_LEVELS)
        .await
    {
        eprintln!("saola-panel: iwd refused the signal-level agent ({error}); no Wi-Fi strength");
        // Take the now-pointless object back down so the panel doesn't
        // advertise an interface nothing will ever call.
        let _ = connection
            .object_server()
            .remove::<SignalLevelAgent, _>(&agent_path)
            .await;
        return None;
    }

    Some(receiver)
}

/// iwd `Station.State` → does the module show a connected SSID (ivory)
/// rather than the quiet offline state? Only "connected" and "roaming" read
/// as connected; "connecting"/"disconnecting" (transitional) and
/// "disconnected" all render offline.
///
/// Pure function, unit-tested below; the D-Bus plumbing above is not.
fn is_connected(state: &str) -> bool {
    matches!(state, "connected" | "roaming")
}

/// iwd bucket index → the percentage the bar shows.
///
/// [`SIGNAL_LEVELS`]' nine thresholds make ten buckets: `0` is "stronger
/// than −50 dBm" and `9` is "weaker than −90 dBm", so the mapping is simply
/// `100 − 10 × bucket` — 100% down to 10%, evenly spaced and monotonically
/// decreasing. The floor is 10 rather than 0 deliberately: a link we are
/// *connected on* is never honestly "0% signal", and a zero would read as
/// "no signal" next to an SSID that plainly is connected.
///
/// Indexes past the last bucket can't happen with iwd as documented, but
/// clamping costs one call and keeps the arithmetic provably in range (no
/// underflow, so no `saturating_sub` sleight of hand to read past).
///
/// Pure function, unit-tested below; the D-Bus plumbing above is not.
fn level_percent(level: u8) -> u8 {
    // `SIGNAL_LEVELS.len()` is 9, comfortably inside u8 — the cast can't
    // truncate, and clippy's `cast_possible_truncation` is satisfied by the
    // const-ness of the array length.
    let bucket = level.min(SIGNAL_LEVELS.len() as u8);
    100 - bucket * 10
}

/// Connection state + bucketed strength → which Lucide glyph the readout
/// shows. Offline is `wifi-off`; connected climbs the arc ladder — full
/// `wifi` ≥ 75, `wifi-high` ≥ 50, `wifi-low` ≥ 25, `wifi-zero` below that —
/// and a connection whose strength is unknown (iwd refused the signal-level
/// agent, or no level has arrived yet) shows the full glyph rather than
/// pretending to know it's weak. Coarse on purpose: the glyph answers
/// "roughly how good is the link", the popover has the SSID and the number.
///
/// `pub(crate)`: `popovers::quick_settings::network_row` reuses this exact
/// mapping beside its SSID text, rather than a second hand-copied ladder
/// that could drift from the bar's.
///
/// Pure function of its arguments — unit-tested below.
pub(crate) fn wifi_icon(connected: bool, strength_percent: Option<u8>) -> Icon {
    if !connected {
        return Icon::WifiOff;
    }
    match strength_percent {
        None => Icon::Wifi,
        Some(p) if p >= 75 => Icon::Wifi,
        Some(p) if p >= 50 => Icon::WifiHigh,
        Some(p) if p >= 25 => Icon::WifiLow,
        Some(_) => Icon::WifiZero,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_and_roaming_states_are_connected() {
        assert!(is_connected("connected"));
        assert!(is_connected("roaming"));
    }

    #[test]
    fn transitional_and_disconnected_states_are_not_connected() {
        assert!(!is_connected("disconnected"));
        assert!(!is_connected("connecting"));
        assert!(!is_connected("disconnecting"));
    }

    #[test]
    fn unknown_state_strings_are_not_connected() {
        // Defensive: a future iwd release adding a state value should never
        // accidentally read as "live" here.
        assert!(!is_connected("roaming-4ghz-experimental"));
        assert!(!is_connected(""));
    }

    #[test]
    fn the_arc_ladder_steps_at_its_documented_thresholds() {
        assert_eq!(wifi_icon(true, Some(100)), Icon::Wifi);
        assert_eq!(wifi_icon(true, Some(75)), Icon::Wifi);
        assert_eq!(wifi_icon(true, Some(74)), Icon::WifiHigh);
        assert_eq!(wifi_icon(true, Some(50)), Icon::WifiHigh);
        assert_eq!(wifi_icon(true, Some(49)), Icon::WifiLow);
        assert_eq!(wifi_icon(true, Some(25)), Icon::WifiLow);
        assert_eq!(wifi_icon(true, Some(24)), Icon::WifiZero);
        assert_eq!(wifi_icon(true, Some(10)), Icon::WifiZero);
    }

    #[test]
    fn unknown_strength_shows_the_full_glyph_rather_than_guessing_weak() {
        assert_eq!(wifi_icon(true, None), Icon::Wifi);
    }

    #[test]
    fn offline_is_wifi_off_regardless_of_a_stale_strength() {
        assert_eq!(wifi_icon(false, None), Icon::WifiOff);
        // Belt and braces: the worker clears strength whenever the link
        // isn't settled, but the glyph must never show arcs while offline
        // even if some future caller forgets.
        assert_eq!(wifi_icon(false, Some(40)), Icon::WifiOff);
    }

    #[test]
    fn strongest_bucket_is_full_strength() {
        assert_eq!(level_percent(0), 100);
    }

    #[test]
    fn weakest_bucket_is_low_but_never_zero() {
        let weakest = level_percent(SIGNAL_LEVELS.len() as u8);
        assert_eq!(weakest, 10);
        assert!(weakest > 0);
    }

    #[test]
    fn strength_decreases_monotonically_with_the_bucket_index() {
        for level in 1..=SIGNAL_LEVELS.len() as u8 {
            assert!(
                level_percent(level) < level_percent(level - 1),
                "bucket {level} should read weaker than {}",
                level - 1
            );
        }
    }

    #[test]
    fn out_of_range_buckets_clamp_to_the_weakest() {
        // iwd never sends an index past the threshold count, but the
        // arithmetic must not wrap if it ever did.
        assert_eq!(level_percent(200), level_percent(SIGNAL_LEVELS.len() as u8));
    }
}
