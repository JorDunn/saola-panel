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
//! # Design language
//!
//! Like every other right-region status module (see `battery.rs` for the full
//! note), this renders as a **bare Lucide glyph + label directly on the ink
//! bar** — no pill, no fill.
//!
//! Being connected is deliberately **not** a terracotta state, which is the
//! one place this module diverges from what an earlier stage built. Terracotta
//! marks something that is switched *on* or actively live, and the style guide
//! allows at most one terracotta element per surface; "connected to HomeNet"
//! is the resting, expected condition of a laptop, so painting it accent would
//! spend the bar's whole accent budget on a permanent state. Connected is
//! therefore plain ivory (`on_ink.primary`), exactly like the clock.
//!
//! Disconnected-but-present is *quieter* still: the same bare glyph + label,
//! both dropped to the `on_ink.secondary` role — not a custom gray, and not a
//! dimmed pill. If iwd isn't on the bus at all (or no Station object is
//! found), the module renders nothing, same as battery with no UPower.

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, StreamExt};
use iced::futures::{SinkExt, Stream};
use iced::widget::{row, text, Space};
use iced::{Element, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;
use zbus::fdo::ObjectManagerProxy;
use zbus::zvariant::OwnedObjectPath;
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
    /// Whether a Station object was found on the bus at all. `false` covers
    /// "iwd isn't running" *and* "no wifi adapter" — the module renders
    /// nothing regardless of `ssid` (mirrors `Battery::present`).
    present: bool,
}

impl Network {
    /// Renders the Wi-Fi glyph + label: nothing if no Station was ever found;
    /// the SSID in plain ivory when connected; the quiet "offline" label
    /// otherwise. Never a pill, and never terracotta — see the module doc
    /// comment for why "connected" is a resting state, not a live one.
    ///
    /// Both states share one row and differ only in the *role* the glyph and
    /// the label are tinted with, so the module never changes shape when the
    /// link comes and goes — the bar's right region doesn't reflow around a
    /// roaming laptop. `on_ink.secondary` for offline is CLAUDE.md's rule
    /// verbatim: quiet states use the secondary/disabled roles, never a
    /// custom gray.
    ///
    /// Teaching note (one color, two widgets): an iced `Svg` never inherits a
    /// text color, so `icons::icon` has to be handed the tint explicitly
    /// while `text` needs its own `.color(..)`. Deriving the role once and
    /// giving the same `iced::Color` to both is what keeps the glyph and the
    /// label from drifting apart. `ColorExt::into_iced` is the hop from a
    /// `saola_theme` color to iced's own `Color` type.
    ///
    /// Lifetime note: `network_label` returns a `&str` borrowed *from
    /// `self.ssid`*, and `Element<'_, Message>` picks up that same borrow of
    /// `&self` — which is why the SSID needs no `.clone()` here. The element
    /// simply may not outlive the `Network` it was rendered from, and iced's
    /// `view` contract already guarantees that.
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Reads the same field as `view`'s early return, so the two
    /// cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.present
    }

    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if !self.present {
            return Space::new().into();
        }

        let color = match self.ssid {
            // Connected: full-emphasis ivory. Resting state, no accent.
            Some(_) => theme.on_ink.primary,
            // Offline: the quiet role, for both halves of the row.
            None => theme.on_ink.secondary,
        }
        .into_iced();

        row![
            icons::icon(Icon::Wifi, theme.sizes.icon_bar, color),
            text(network_label(self.ssid.as_deref()))
                .size(theme.typography.size.bar)
                .color(color),
        ]
        // `sizes.bar_icon_gap` (7.0) is the icon↔value gap inside one status
        // readout. The `island_gap` is for gaps between modules, not within them.
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center)
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

    // One merged "something changed" stream, exactly like battery's —
    // different item types per property, normalized to `()` so
    // `stream::select` can merge them; we re-read the full snapshot on any
    // change rather than trying to apply a partial update.
    let mut changed = {
        let state = station.receive_state_changed().await;
        let connected_network = station.receive_connected_network_changed().await;
        stream::select(state.map(|_| ()), connected_network.map(|_| ()))
    };

    loop {
        let state = station.state().await?;

        // `ConnectedNetwork` is an *optional* D-Bus property (iwd's own
        // docs: absent from the object entirely while disconnected, not
        // present-with-a-null-path) — reading it then fails at the D-Bus
        // level, which is expected, not a worker-ending error. `.ok()`
        // turns "property doesn't exist right now" into `None` instead of
        // propagating a `zbus::Error` through `?` and killing the loop.
        let ssid = match station.connected_network().await.ok() {
            Some(path) => match IwdNetworkProxy::new(&connection, path).await {
                Ok(network) => network.name().await.ok(),
                Err(_) => None,
            },
            None => None,
        };

        let snapshot = Network {
            // Only a settled "connected"/"roaming" state shows the SSID —
            // "connecting"/"disconnecting" are transitional, so (like
            // battery's "only *live* is charging" rule) they still render
            // as offline rather than flashing a stale or half-set SSID.
            ssid: if is_connected(&state) { ssid } else { None },
            present: true,
        };

        if sender.send(Message::Updated(snapshot)).await.is_err() {
            return Ok(());
        }

        if changed.next().await.is_none() {
            return Ok(());
        }
    }
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

/// The row's text content for a given SSID: the SSID itself when known, or
/// the fixed "offline" label otherwise.
///
/// Pure function of its argument — unit-tested below.
fn network_label(ssid: Option<&str>) -> &str {
    ssid.unwrap_or("offline")
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
    fn label_shows_the_ssid_when_known() {
        assert_eq!(network_label(Some("HomeNet")), "HomeNet");
    }

    #[test]
    fn label_falls_back_to_offline_when_unknown() {
        assert_eq!(network_label(None), "offline");
    }
}
