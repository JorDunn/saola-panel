//! The bar's Bluetooth readout, fed by BlueZ (`org.bluez`) over D-Bus.
//!
//! Status **display only**: which adapter state the machine is in, and which
//! devices are connected. There is deliberately no adapter on/off toggle and
//! no pairing UI here — turning the radio off is a quick-settings decision
//! (and pairing is `bluetoothctl`/a settings app's job), so this module is
//! the same shape as `battery.rs`/`network.rs`: a worker that watches, a
//! snapshot that is rendered, and nothing that writes back to the bus.
//!
//! # Discovery (the same ObjectManager shape `network.rs` uses)
//!
//! BlueZ has no fixed "the adapter" object the way UPower has
//! `DisplayDevice` — adapters are `/org/bluez/hci0`, `hci1`, … and devices
//! hang below them (`/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF`). So, exactly
//! like iwd's Station in `network.rs`, this worker has to *find* what it
//! wants first, through the standard `org.freedesktop.DBus.ObjectManager`
//! interface BlueZ publishes at the bus root (`/`). Read that module's
//! `watch_iwd` for the fuller teaching note; it applies here almost verbatim.
//!
//! # Why the whole tree is rebuilt on every event
//!
//! The interesting state is spread across three interfaces on two levels of
//! the tree (`Adapter1.Powered`, `Device1.Connected`/`Alias`,
//! `Battery1.Percentage`), and BlueZ reports changes through three different
//! signals (`InterfacesAdded`, `InterfacesRemoved`, and per-object
//! `PropertiesChanged`). Applying each of those as a *partial* update would
//! mean hand-maintaining a mirror of BlueZ's object tree, with a separate
//! bug lurking behind each signal shape. Instead, all three streams are
//! merged and normalized to `()`, and **any** event re-runs
//! `GetManagedObjects()` and rebuilds the snapshot from scratch
//! ([`snapshot`]). A BlueZ tree is a handful of objects, so one extra
//! round-trip per event is cheap, and the rebuilt snapshot is immune to
//! partial-update bugs by construction.
//!
//! This is still **signal-driven, never a poll** (CLAUDE.md): every rebuild
//! is a *response to a bus signal*. Nothing here ticks. The one cost of the
//! fan-in is noise — a `PropertiesChanged` on a nearby device's `RSSI`
//! wakes the worker even though nothing the bar shows changed — so the
//! worker dedupes against the last snapshot it sent (hence `PartialEq` on
//! [`Bluetooth`]) rather than re-rendering the UI for radio chatter.
//!
//! # Design language
//!
//! Like every other right-region status module (see `battery.rs` for the
//! full note), this renders as a **bare Lucide glyph directly on the ink
//! bar** — no pill, no fill, no text. Three states, three glyphs:
//!
//! - adapter present but powered off → `bluetooth-off`, quiet
//!   (`on_ink.secondary`),
//! - powered, nothing connected → `bluetooth`, also quiet: an idle radio is
//!   a resting state, not news,
//! - at least one device connected → `bluetooth-connected` in full-emphasis
//!   ivory (`on_ink.primary`).
//!
//! **Never terracotta.** "Headphones are connected" is the resting,
//! expected condition of a laptop in exactly the way "connected to HomeNet"
//! is — `network.rs`'s doc comment spells the reasoning out: the style guide
//! allows at most one terracotta element per surface, and a permanent state
//! must not spend that budget. Connected is plain ivory, same as the clock.
//!
//! If BlueZ isn't on the bus, or there is no adapter at all, the module
//! renders nothing and the panel carries on.

use std::collections::HashMap;

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, StreamExt};
use iced::futures::{SinkExt, Stream};
use iced::widget::Space;
use iced::{Element, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;
use zbus::fdo::{ManagedObjects, ObjectManagerProxy};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, MatchRule, MessageStream};

use crate::icons::{self, Icon};

/// The Bluetooth module's own message type (Stage 7's per-module refactor —
/// see `modules::clock::Message` for the full teaching note). `main.rs`
/// nests this as `Message::Bluetooth(bluetooth::Message)`; `Panel::update`
/// delegates by matching through both layers.
///
/// Single variant: this module is status-only, so unlike `power`/`media`
/// there is no command-out variant to carry a click back to the bus.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Bluetooth),
}

/// BlueZ's D-Bus service name — a **system** service, like UPower and iwd.
const BLUEZ_SERVICE: &str = "org.bluez";

/// The interface an object implements to be an adapter (a radio). Matched as
/// a plain string against `ObjectManager`'s reply, same as `network.rs` does
/// for iwd's Station: we only need to spot which object it is, not proxy it.
const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";

/// The interface a *remote* device object implements (paired, or merely
/// seen). Only the connected ones are listed — see [`snapshot`].
const DEVICE_INTERFACE: &str = "org.bluez.Device1";

/// The optional interface BlueZ adds to a device object that reports its own
/// battery level (most modern headsets and mice do; plenty of things
/// don't). Same object path as the `Device1` interface, which is why
/// [`snapshot`] reads both out of the *same* per-object interface map.
const BATTERY_INTERFACE: &str = "org.bluez.Battery1";

/// One connected Bluetooth device, as much of it as the panel cares about.
///
/// Only ever built for devices whose `Connected` property is true — a
/// remembered-but-absent pair of headphones is not something the bar or the
/// popover has anything to say about.
/// Fields are `pub(crate)` so `popovers::quick_settings`' tests can build
/// fixture devices directly — the same reasoning `modules::tray::menu::
/// MenuNode` documents for its own crate-visible fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// BlueZ's `Alias`: the user-facing name, which is the device's own
    /// `Name` unless the user renamed it. Preferring `Alias` means a rename
    /// in `bluetoothctl` shows up here, which is the whole point of the
    /// property existing.
    pub(crate) alias: String,
    /// `org.bluez.Battery1.Percentage`, 0–100 — `None` for the many devices
    /// that don't implement that interface at all. `u8` because that is
    /// literally BlueZ's D-Bus type here (`y`).
    pub(crate) battery_percent: Option<u8>,
}

impl Device {
    /// The device's user-facing name.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// The device's own battery level, when it reports one.
    pub fn battery_percent(&self) -> Option<u8> {
        self.battery_percent
    }
}

/// Bluetooth module state: the last snapshot the D-Bus worker pushed through
/// [`Message::Updated`]. Like `Battery`/`Network`, this module caches —
/// reading D-Bus during `view` would block the UI thread.
///
/// `Default` is the boot state: `present: false` — "no adapter known yet" —
/// so boot, a machine with no Bluetooth hardware, and a `bluetoothd` that
/// isn't running all render identically (nothing). Same contract as
/// `Battery::default()`.
///
/// `PartialEq` is load-bearing here rather than merely derived: the worker
/// compares each rebuilt snapshot against the last one it sent and skips the
/// send when they match (see the module doc comment's note on fan-in noise).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bluetooth {
    /// Whether a BlueZ adapter was found on the bus at all. `false` covers
    /// "bluetoothd isn't running" *and* "no Bluetooth hardware" — the module
    /// renders nothing regardless of the other two fields (mirrors
    /// `Battery::present` / `Network::present`).
    present: bool,
    /// The adapter's `Powered` property: is the radio on? A powered-off
    /// adapter still *exists*, which is why this is a separate field rather
    /// than folded into `present` — "off" is a state worth drawing (the
    /// `bluetooth-off` glyph), "absent" is not.
    powered: bool,
    /// Every currently **connected** device, ordered by object path so the
    /// list is stable across rebuilds (`GetManagedObjects` returns a
    /// `HashMap`, whose iteration order is deliberately randomized — an
    /// unsorted list would make two identical trees compare unequal and
    /// defeat the worker's dedupe, as well as reshuffling the popover's rows
    /// for no reason).
    devices: Vec<Device>,
}

impl Bluetooth {
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Reads the same field as `view`'s early return, so the two
    /// cannot drift apart. Gated on the *adapter* having been found, exactly
    /// as `Network::is_present` gates on a Station: a powered-off radio is
    /// still present and still draws its glyph.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// Whether the adapter's radio is on.
    pub fn powered(&self) -> bool {
        self.powered
    }

    /// The connected devices, in a stable order. A borrow rather than a
    /// clone — the caller only ever needs to display them.
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// Renders the Bluetooth glyph, or nothing when no adapter was ever
    /// found (`Space::new()` with no size is a zero-area widget — the row
    /// simply closes up around it). Bare icon straight on the ink bar, no
    /// pill, no text — see the module doc comment.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if !self.present {
            return Space::new().into();
        }

        let any_connected = !self.devices.is_empty();

        // Full-emphasis ivory only once something is actually connected;
        // both quieter states (radio off, radio idle) take the `secondary`
        // role — not a custom gray, and never the accent (see the module
        // doc comment on why "connected" is not a terracotta state).
        let color = if any_connected {
            theme.on_ink.primary
        } else {
            theme.on_ink.secondary
        }
        .into_iced();

        icons::icon(
            bluetooth_icon(self.powered, any_connected),
            theme.sizes.icon_bar,
            color,
        )
        .into()
    }

    /// The module's D-Bus feed as an iced subscription. See battery.rs's
    /// `subscription` for the function-pointer-identity teaching note (why
    /// `Subscription::run`'s identity survives the `.map(crate::Message::
    /// Bluetooth)` `main.rs` applies) — the same reasoning applies verbatim.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(bluetooth_stream)
    }
}

/// Builds the async stream the subscription runs. See battery.rs's
/// `battery_stream` for the full bridge teaching note (the channel, the
/// runtime it runs on). Every failure path here — no system bus, BlueZ not
/// running, no adapter object found — funnels into "send the hidden/default
/// state, worker ends quietly": the panel never goes down because Bluetooth
/// isn't there.
fn bluetooth_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        if watch_bluez(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(Bluetooth::default())).await;
        }
    })
}

/// The worker proper: find the adapter, read the tree once, then rebuild and
/// push a snapshot on every BlueZ signal, forever.
async fn watch_bluez(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // System bus: bluetoothd (like UPower and iwd) is a system service.
    let connection = Connection::system().await?;

    // BlueZ publishes `org.freedesktop.DBus.ObjectManager` at the bus root
    // — see the module doc comment, and `network.rs`'s `watch_iwd` for the
    // longer teaching note on what that interface is for.
    let object_manager = ObjectManagerProxy::builder(&connection)
        .destination(BLUEZ_SERVICE)?
        .path("/")?
        .build()
        .await?;

    // The first read doubles as the presence check: no adapter in the tree
    // means this machine has no Bluetooth to show, so the worker gives up
    // here and the wrapper above pushes the hidden default state — exactly
    // what `watch_iwd` does when it finds no Station. (A USB dongle plugged
    // in *later* won't revive the module until the panel restarts; same
    // limitation every other absent-service module has, and not worth a
    // reconnect loop for hardware that rarely appears mid-session.)
    let initial = snapshot(&object_manager.get_managed_objects().await?);
    if !initial.present {
        return Err(zbus::Error::Failure("no BlueZ adapter found".to_string()));
    }

    // Teaching note (the three-way fan-in): BlueZ announces the two kinds of
    // change this module cares about through different mechanisms, so all
    // three streams are merged and normalized to `()` — `stream::select`
    // needs one item type, and we only need the *fact* that something
    // happened, since the loop below re-reads the whole tree anyway.
    //
    // 1. `InterfacesAdded` — a device appeared (or gained `Battery1`).
    // 2. `InterfacesRemoved` — a device went away (or lost an interface).
    // 3. `PropertiesChanged` — the interesting one: `Adapter1.Powered`
    //    flipping, or a device's `Connected`/`Alias`/`Percentage` changing.
    //
    // The third has no proxy behind it on purpose. A `#[zbus::proxy]`'s
    // `receive_*_changed()` stream is bound to *one* object path, and the
    // set of device paths changes at runtime — so watching them that way
    // would mean spawning and dropping proxies as devices come and go.
    // A `MatchRule` filtered on `sender=org.bluez` instead catches
    // `PropertiesChanged` from every BlueZ object at once, adapter and
    // devices alike, with the bus doing the filtering (see `claude.rs`'s
    // doc comment for the `MatchRule`/`MessageStream` shape). The rule is
    // deliberately not narrowed by arg0 to a specific interface: the cost of
    // the extra wakeups is one dedupe comparison, and the cost of getting an
    // arg0 filter subtly wrong is a readout that silently stops updating.
    let added = object_manager.receive_interfaces_added().await?;
    let removed = object_manager.receive_interfaces_removed().await?;

    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(BLUEZ_SERVICE)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .build();
    let properties = MessageStream::for_match_rule(rule, &connection, Some(8)).await?;

    let mut events = stream::select(
        stream::select(added.map(|_| ()), removed.map(|_| ())),
        properties.map(|_| ()),
    );

    let mut current = initial;
    // Dedupe against the last value sent. Not a nicety here the way it is in
    // `claude.rs`: the `PropertiesChanged` rule above fires for plenty of
    // things the bar doesn't render (a nearby device's `RSSI` while
    // scanning, say), and without this every one of them would wake the UI
    // to draw the identical glyph.
    let mut last_sent: Option<Bluetooth> = None;

    loop {
        if last_sent.as_ref() != Some(&current) {
            // A send error means the receiving side is gone (the
            // subscription was dropped) — stop quietly rather than erroring.
            if sender
                .send(Message::Updated(current.clone()))
                .await
                .is_err()
            {
                return Ok(());
            }
            last_sent = Some(current.clone());
        }

        // Park until BlueZ says *something* happened; all three sources
        // ending means the connection is gone, so the worker ends too.
        if events.next().await.is_none() {
            return Ok(());
        }

        current = snapshot(&object_manager.get_managed_objects().await?);
    }
}

/// One object's interfaces, as `GetManagedObjects` hands them back: interface
/// name → that interface's properties. A local alias purely so the helpers
/// below read as English rather than as three nested generics.
type Interfaces = HashMap<zbus::names::OwnedInterfaceName, HashMap<String, OwnedValue>>;

/// BlueZ's whole object tree → the snapshot the bar renders.
///
/// The single place D-Bus data becomes panel state, and deliberately a
/// **pure function of its argument**: no connection, no `await`, nothing to
/// mock — which is what makes the tree-shape rules below (which object is
/// "the" adapter, which devices count, where the battery level comes from)
/// unit-testable without a bus.
///
/// Returns `Bluetooth::default()` (i.e. `present: false`) when the tree
/// contains no adapter at all.
fn snapshot(objects: &ManagedObjects) -> Bluetooth {
    // Teaching note (`min_by`, not `find`): a `HashMap`'s iteration order is
    // randomized per process *and* unstable between rebuilds, so "the first
    // adapter" would be a coin toss on a machine with two radios — and the
    // snapshot would then flip between them, defeating the worker's dedupe.
    // Taking the lexicographically smallest path makes the choice
    // deterministic (`hci0` beats `hci1`, which is also the intuitive
    // answer). v0.1 shows one adapter; a two-radio machine would need a
    // richer module, exactly as `network.rs` says of two wifi radios.
    let adapter = objects
        .iter()
        .filter(|(_, interfaces)| interface(interfaces, ADAPTER_INTERFACE).is_some())
        .min_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));

    let Some((adapter_path, adapter_interfaces)) = adapter else {
        return Bluetooth::default();
    };

    let powered = interface(adapter_interfaces, ADAPTER_INTERFACE)
        .and_then(|properties| property_bool(properties, "Powered"))
        // A missing `Powered` shouldn't happen (BlueZ always publishes it),
        // and if it ever did, "off" is the honest reading of "we can't tell
        // that the radio is on" — it never claims a capability we haven't
        // seen evidence for.
        .unwrap_or(false);

    // Devices are children of the adapter's path
    // (`/org/bluez/hci0/dev_AA_…`), so the prefix test is what keeps a
    // second adapter's headphones out of this adapter's list. The trailing
    // slash matters: without it, `/org/bluez/hci10` would look like a child
    // of `/org/bluez/hci1`.
    let prefix = format!("{}/", adapter_path.as_str());

    let mut devices: Vec<(&str, Device)> = objects
        .iter()
        .filter(|(path, _)| path.as_str().starts_with(&prefix))
        .filter_map(|(path, interfaces)| {
            let properties = interface(interfaces, DEVICE_INTERFACE)?;

            // Only *connected* devices are listed — a remembered pair of
            // headphones sitting in a drawer is not something the bar has
            // anything to say about. A device object with no `Connected`
            // property at all reads as not connected, same conservative
            // default as `powered` above.
            if !property_bool(properties, "Connected").unwrap_or(false) {
                return None;
            }

            // `Alias` is the user-facing name (`Name` unless it's been
            // renamed); `Name` is the fallback for the rare device that
            // publishes one and not the other, and the last resort is a
            // placeholder rather than dropping a genuinely-connected device
            // out of the list over a missing string.
            let alias = property_str(properties, "Alias")
                .or_else(|| property_str(properties, "Name"))
                .unwrap_or("Unknown device")
                .to_string();

            // The battery level lives on a *different interface of the same
            // object* — BlueZ adds `org.bluez.Battery1` alongside `Device1`
            // for devices that report one, which is why this reads out of
            // the same per-object interface map rather than needing another
            // lookup.
            let battery_percent = interface(interfaces, BATTERY_INTERFACE)
                .and_then(|battery| property_u8(battery, "Percentage"));

            Some((
                path.as_str(),
                Device {
                    alias,
                    battery_percent,
                },
            ))
        })
        .collect();

    // Sort by object path, not by alias: paths are unique and never change
    // for a given device, so the order is stable even when two devices share
    // a name (two identical mice) or one gets renamed mid-session.
    devices.sort_by_key(|(path, _)| *path);

    Bluetooth {
        present: true,
        powered,
        devices: devices.into_iter().map(|(_, device)| device).collect(),
    }
}

/// One interface's property map out of an object's interface map, by name.
///
/// A linear scan rather than a `HashMap::get`: the keys are
/// `OwnedInterfaceName`, so looking one up by `&str` would lean on a
/// `Borrow` impl that may or may not exist, and an object here has under a
/// dozen interfaces. Comparing `.as_str()` is the same "spot the interface
/// by name" move `network.rs` makes on `ObjectManager`'s reply.
fn interface<'a>(
    interfaces: &'a Interfaces,
    name: &str,
) -> Option<&'a HashMap<String, OwnedValue>> {
    interfaces
        .iter()
        .find(|(interface_name, _)| interface_name.as_str() == name)
        .map(|(_, properties)| properties)
}

/// Teaching note (`downcast_ref`): a D-Bus property arrives as an
/// `OwnedValue` — a dynamically-typed variant. `downcast_ref` both checks
/// that the variant really holds the type asked for *and* transparently
/// unwraps a nested `Value::Value` (a variant inside a variant, which is
/// exactly what `a{sv}` property maps carry). Matching on `Value::Bool(..)`
/// by hand would miss that second case. `tray/menu.rs`'s property helpers
/// are the same three functions for the same reason.
fn property_bool(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    properties.get(name)?.downcast_ref::<bool>().ok()
}

fn property_str<'a>(properties: &'a HashMap<String, OwnedValue>, name: &str) -> Option<&'a str> {
    properties.get(name)?.downcast_ref::<&str>().ok()
}

fn property_u8(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u8> {
    properties.get(name)?.downcast_ref::<u8>().ok()
}

/// Adapter state → which Lucide glyph the readout shows. Powered-off wins
/// over everything (a radio that's off can't have connections); a powered
/// radio with something on it gets the "connected" rune, and an idle one the
/// bare rune.
///
/// `pub(crate)` for the same reason `battery_icon`/`wifi_icon` are: the
/// quick-settings popover's Bluetooth row shows this exact glyph beside its
/// device list, and a second hand-copied mapping there could drift from the
/// bar's.
///
/// Pure function of its arguments — unit-tested below; the D-Bus plumbing
/// above is not.
pub(crate) fn bluetooth_icon(powered: bool, any_connected: bool) -> Icon {
    if !powered {
        Icon::BluetoothOff
    } else if any_connected {
        Icon::BluetoothConnected
    } else {
        Icon::Bluetooth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::names::OwnedInterfaceName;
    use zbus::zvariant::{OwnedObjectPath, Value};

    #[test]
    fn a_powered_off_adapter_shows_the_off_rune() {
        assert_eq!(bluetooth_icon(false, false), Icon::BluetoothOff);
        // Defensive: connections can't outlive the radio, but the glyph must
        // never claim a link on an adapter we've been told is off.
        assert_eq!(bluetooth_icon(false, true), Icon::BluetoothOff);
    }

    #[test]
    fn a_powered_idle_adapter_shows_the_bare_rune() {
        assert_eq!(bluetooth_icon(true, false), Icon::Bluetooth);
    }

    #[test]
    fn a_connected_adapter_shows_the_connected_rune() {
        assert_eq!(bluetooth_icon(true, true), Icon::BluetoothConnected);
    }

    /// The boot/absent contract every service-backed module shares: nothing
    /// known yet renders nothing, and claims nothing.
    #[test]
    fn the_default_snapshot_is_absent_and_empty() {
        let bluetooth = Bluetooth::default();
        assert!(!bluetooth.is_present());
        assert!(!bluetooth.powered());
        assert!(bluetooth.devices().is_empty());
    }

    /// Builds one entry of a `GetManagedObjects` reply: an object path, and
    /// the interfaces it implements with their properties.
    fn object(
        path: &str,
        interfaces: &[(&str, &[(&str, Value<'static>)])],
    ) -> (
        OwnedObjectPath,
        HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>>,
    ) {
        let interfaces = interfaces
            .iter()
            .map(|(name, properties)| {
                let properties = properties
                    .iter()
                    .map(|(key, value)| {
                        (
                            (*key).to_string(),
                            OwnedValue::try_from(value.try_clone().expect("cloneable fixture"))
                                .expect("fixture value converts"),
                        )
                    })
                    .collect();
                (
                    OwnedInterfaceName::try_from(*name).expect("valid interface name"),
                    properties,
                )
            })
            .collect();
        (
            OwnedObjectPath::try_from(path).expect("valid object path"),
            interfaces,
        )
    }

    /// The tree BlueZ actually reports on this machine, plus two devices:
    /// one connected headset that reports its own battery, one connected
    /// mouse that doesn't, and one remembered-but-disconnected device.
    fn managed_objects() -> ManagedObjects {
        ManagedObjects::from_iter([
            object("/org/bluez", &[("org.bluez.AgentManager1", &[])]),
            object(
                "/org/bluez/hci0",
                &[(
                    "org.bluez.Adapter1",
                    &[
                        ("Powered", Value::from(true)),
                        ("Alias", Value::from("nt-14589")),
                    ],
                )],
            ),
            object(
                "/org/bluez/hci0/dev_AA",
                &[
                    (
                        "org.bluez.Device1",
                        &[
                            ("Connected", Value::from(true)),
                            ("Alias", Value::from("Headset")),
                            ("Name", Value::from("WH-1000XM4")),
                        ],
                    ),
                    ("org.bluez.Battery1", &[("Percentage", Value::from(80u8))]),
                ],
            ),
            object(
                "/org/bluez/hci0/dev_BB",
                &[(
                    "org.bluez.Device1",
                    &[
                        ("Connected", Value::from(true)),
                        ("Alias", Value::from("MX Master")),
                    ],
                )],
            ),
            object(
                "/org/bluez/hci0/dev_CC",
                &[(
                    "org.bluez.Device1",
                    &[
                        ("Connected", Value::from(false)),
                        ("Alias", Value::from("Old speaker")),
                    ],
                )],
            ),
        ])
    }

    #[test]
    fn an_adapter_with_connected_devices_builds_a_present_snapshot() {
        let bluetooth = snapshot(&managed_objects());

        assert!(bluetooth.is_present());
        assert!(bluetooth.powered());
        // Path order: dev_AA before dev_BB, and the disconnected dev_CC is
        // not listed at all.
        let aliases: Vec<&str> = bluetooth.devices().iter().map(Device::alias).collect();
        assert_eq!(aliases, vec!["Headset", "MX Master"]);
    }

    #[test]
    fn only_devices_that_report_one_carry_a_battery_level() {
        let bluetooth = snapshot(&managed_objects());
        assert_eq!(bluetooth.devices()[0].battery_percent(), Some(80));
        assert_eq!(bluetooth.devices()[1].battery_percent(), None);
    }

    /// The rename rule: `Alias` wins over `Name` when both are present, and
    /// `Name` is the fallback when only it is.
    #[test]
    fn alias_wins_over_name_and_name_is_the_fallback() {
        let objects = ManagedObjects::from_iter([
            object(
                "/org/bluez/hci0",
                &[("org.bluez.Adapter1", &[("Powered", Value::from(true))])],
            ),
            object(
                "/org/bluez/hci0/dev_AA",
                &[(
                    "org.bluez.Device1",
                    &[
                        ("Connected", Value::from(true)),
                        ("Name", Value::from("Only a name")),
                    ],
                )],
            ),
        ]);
        let bluetooth = snapshot(&objects);
        assert_eq!(bluetooth.devices()[0].alias(), "Only a name");
    }

    /// A powered-off adapter is still *present* — the module draws the
    /// `bluetooth-off` glyph rather than disappearing (the distinction
    /// `present` and `powered` exist to keep apart).
    #[test]
    fn a_powered_off_adapter_is_present_but_not_powered() {
        let objects = ManagedObjects::from_iter([object(
            "/org/bluez/hci0",
            &[("org.bluez.Adapter1", &[("Powered", Value::from(false))])],
        )]);
        let bluetooth = snapshot(&objects);
        assert!(bluetooth.is_present());
        assert!(!bluetooth.powered());
        assert!(bluetooth.devices().is_empty());
    }

    /// A tree with no adapter at all (bluetoothd running, no radio) is the
    /// absent case — identical to boot, so the module renders nothing.
    #[test]
    fn a_tree_with_no_adapter_is_absent() {
        let objects =
            ManagedObjects::from_iter([object("/org/bluez", &[("org.bluez.AgentManager1", &[])])]);
        assert_eq!(snapshot(&objects), Bluetooth::default());
    }

    /// A device hanging off a *different* adapter never lands in this
    /// adapter's list — the prefix rule, and the reason it carries a
    /// trailing slash.
    #[test]
    fn devices_under_another_adapter_are_not_listed() {
        let objects = ManagedObjects::from_iter([
            object(
                "/org/bluez/hci0",
                &[("org.bluez.Adapter1", &[("Powered", Value::from(true))])],
            ),
            object(
                "/org/bluez/hci1",
                &[("org.bluez.Adapter1", &[("Powered", Value::from(true))])],
            ),
            object(
                "/org/bluez/hci1/dev_AA",
                &[(
                    "org.bluez.Device1",
                    &[
                        ("Connected", Value::from(true)),
                        ("Alias", Value::from("Other adapter's headset")),
                    ],
                )],
            ),
        ]);
        // `hci0` is the deterministic winner (lexicographically smallest),
        // and it has no devices of its own.
        let bluetooth = snapshot(&objects);
        assert!(bluetooth.is_present());
        assert!(bluetooth.devices().is_empty());
    }

    /// Rebuilding the same tree twice must produce equal snapshots — the
    /// property the worker's dedupe rests on, and the reason the device list
    /// is sorted rather than left in `HashMap` order.
    #[test]
    fn rebuilding_the_same_tree_produces_an_equal_snapshot() {
        assert_eq!(snapshot(&managed_objects()), snapshot(&managed_objects()));
    }

    // ---------------------------------------------------------------
    // The live-bus test. `#[ignore]`d, so `cargo test` (and CI) skip it:
    //
    //     cargo test --bin saola-panel -- --ignored bluetooth
    //
    // Unlike `tray/watcher.rs`'s live test (which claims a well-known name
    // and therefore needs a *private* bus), this one is strictly read-only
    // against the real **system** bus — it introspects BlueZ and registers
    // signal match rules, nothing more — so it is safe to run on a working
    // desktop. It needs `bluetoothd` running with at least one adapter, and
    // is skipped in CI precisely because neither is guaranteed there.
    //
    // What it covers that the unit tests above cannot: the D-Bus plumbing
    // itself — that BlueZ really does answer `GetManagedObjects` at `/`,
    // that a real reply flows through `snapshot` to a `present` result, and
    // that all three legs of the event fan-in can actually be established
    // (the `MatchRule` in particular, which is the one construct here the
    // compiler cannot check). It deliberately does **not** wait for an
    // event: the whole point of the module is that nothing happens until
    // the user's hardware does, so a test that awaited one would hang.
    // ---------------------------------------------------------------
    #[test]
    #[ignore = "needs a system bus with bluetoothd and an adapter"]
    fn a_real_bluez_tree_builds_a_present_snapshot_and_the_fan_in_registers() {
        let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
        runtime.block_on(async {
            let connection = Connection::system().await.expect("a system bus");
            let object_manager = ObjectManagerProxy::builder(&connection)
                .destination(BLUEZ_SERVICE)
                .expect("a valid destination")
                .path("/")
                .expect("a valid path")
                .build()
                .await
                .expect("BlueZ answers at the bus root");

            let objects = object_manager
                .get_managed_objects()
                .await
                .expect("BlueZ enumerates its objects");
            let bluetooth = snapshot(&objects);
            assert!(
                bluetooth.is_present(),
                "expected at least one org.bluez.Adapter1 in the live tree"
            );

            // All three legs of the fan-in, established against the real
            // bus. Constructed and dropped without being polled — see the
            // comment above on why nothing is awaited.
            let _added = object_manager
                .receive_interfaces_added()
                .await
                .expect("InterfacesAdded subscribes");
            let _removed = object_manager
                .receive_interfaces_removed()
                .await
                .expect("InterfacesRemoved subscribes");
            let rule = MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(BLUEZ_SERVICE)
                .expect("a valid sender name")
                .interface("org.freedesktop.DBus.Properties")
                .expect("a valid interface name")
                .member("PropertiesChanged")
                .expect("a valid member name")
                .build();
            let _properties = MessageStream::for_match_rule(rule, &connection, Some(8))
                .await
                .expect("the bus accepts the match rule");
        });
    }
}
