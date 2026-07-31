//! The tray's D-Bus half: be the `org.kde.StatusNotifierWatcher` if nobody
//! else is, consume whoever is if somebody already is, register as a host
//! either way, and keep the item registry in step with the bus.
//!
//! # The first interface this panel *serves*
//!
//! Every module before this one only ever *consumed* D-Bus: it built a
//! `#[zbus::proxy]` for somebody else's service and read it. The tray can't
//! work that way, because on a bare niri session **nothing provides
//! `org.kde.StatusNotifierWatcher`**. That name is normally owned by a full
//! desktop shell (plasmashell, gnome-shell's appindicator extension, another
//! bar); niri ships no such thing. And SNI applications don't fall back —
//! they check for the watcher, find nothing, and silently show no icon at
//! all. So a panel that wants a tray on niri has to be prepared to *be* the
//! watcher.
//!
//! Serving means the opposite side of zbus: `#[zbus::interface]` on an
//! `impl` block instead of `#[zbus::proxy]` on a trait, and an
//! [`ObjectServer`](zbus::ObjectServer) that owns the object and dispatches
//! incoming method calls to it. The macro pair is deliberately symmetric —
//! `#[zbus(property)]`, `#[zbus(signal)]` mean the same things on both sides
//! — but the ownership is inverted: a proxy is a value *we* poke, an
//! interface is a value *the bus* pokes, so its state lives inside zbus
//! (behind a lock) rather than in our worker's stack frame.
//!
//! # The decision tree, as implemented
//!
//! ```text
//!   connect to the session bus
//!   subscribe to NameOwnerChanged            (before anything else, so no
//!                                             ownership change is missed)
//!   serve /StatusNotifierWatcher             (object first, name second)
//!   request org.kde.StatusNotifierWatcher
//!     ├── PrimaryOwner / AlreadyOwner  →  SERVING
//!     │     • our own object is the registry
//!     │     • items reach the worker through an internal channel
//!     │     • mark ourselves host-registered directly (no self-call)
//!     └── Err(NameTaken) / InQueue     →  CONSUMING
//!           • take our unused object back down
//!           • proxy the incumbent watcher: subscribe to its two item
//!             signals, then read its item list
//!           • call RegisterStatusNotifierHost on it
//!   claim org.kde.StatusNotifierHost-<pid>   (either way — that name's mere
//!                                             existence is what tells items
//!                                             a tray is on screen)
//!   loop over the merged event stream until the watcher name changes hands
//! ```
//!
//! **What happens when another bar owns the name** is therefore: we don't
//! fight it. We become an ordinary host of *its* watcher, and the two bars
//! show the same items. If that bar exits, `NameOwnerChanged` tells us, this
//! session ends, and the supervisor loop starts a new one — which claims the
//! now-free name and promotes us to watcher. The reverse (a bar starting
//! later and taking the name off us) works because we ask for the name with
//! `AllowReplacement`; the same `NameOwnerChanged` watch notices we were
//! replaced and demotes us to consumer on the next session. Neither
//! direction loses items, because every item re-registers when the watcher
//! name changes hands (that is the protocol's own recovery rule).

use std::collections::HashSet;
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, BoxStream, StreamExt};
use iced::futures::{SinkExt, Stream};
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::Connection;

use super::item::{self, ItemRegistry};
use super::{Message, Tray};

/// The well-known name the whole protocol hangs off. Note the `org.kde.`
/// prefix: the freedesktop draft spells it `org.freedesktop.
/// StatusNotifierWatcher`, but *nothing* in the wild implements that name —
/// KDE's original is what every item, host and bar actually uses. Guessing
/// the spec's name here would produce a watcher no application ever finds.
const WATCHER_BUS_NAME: &str = "org.kde.StatusNotifierWatcher";

/// Where the watcher object lives, on whichever connection owns the name.
const WATCHER_OBJECT_PATH: &str = "/StatusNotifierWatcher";

/// The `ProtocolVersion` property's value. 0 is what KDE's own watcher
/// reports and what every host expects; the field exists for a future
/// revision of the protocol that never happened.
const PROTOCOL_VERSION: i32 = 0;

/// How long one item gets to answer a property read before we give up on
/// it.
///
/// Without this, a wedged tray application would stall the *whole* tray for
/// as long as the bus daemon's own reply timeout (25s by default): the
/// worker is a single task, and it is inside `read_item` awaiting a reply
/// that isn't coming. Two seconds is far longer than a healthy item needs
/// and short enough that a sick one is merely absent rather than
/// paralysing.
const ITEM_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Backoff between sessions, as in `columns.rs`: not a poll of anything
/// (there is nothing to sample), just a floor on how fast the worker may
/// retry after the session bus or the watcher name goes away.
const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The `org.kde.StatusNotifierWatcher` implementation this panel serves when
/// it wins the name.
///
/// Teaching note (where this value lives): once handed to
/// `ObjectServer::at`, this struct is owned by zbus, behind a read-write
/// lock, and every field access happens either inside one of the
/// `#[zbus::interface]` methods below (called by the bus) or through an
/// [`InterfaceRef`] the worker keeps. That is why registrations reach the
/// worker through `events` — a channel — rather than the worker simply
/// reading a field: the worker is a different task and has no synchronous
/// path into an object the bus owns.
struct Watcher {
    /// Registration strings, in registration order — the
    /// `RegisteredStatusNotifierItems` property, and the authoritative
    /// registry as far as *other* hosts on this bus are concerned.
    items: Vec<String>,
    /// Whether any `StatusNotifierHost` has registered. Items read this
    /// (`IsStatusNotifierHostRegistered`) to decide whether showing an icon
    /// is worth the trouble at all, so a watcher that never sets it is
    /// worse than no watcher.
    host_registered: bool,
    /// The wire into our own worker. Unbounded because the sending side is
    /// a D-Bus method handler that must not be made to wait on the UI, and
    /// because the traffic is a handful of messages per application launch.
    events: mpsc::UnboundedSender<Event>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// An application announcing its item.
    ///
    /// The `service` argument is SNI's famously ambiguous one — see
    /// `item.rs`'s module doc comment. `#[zbus(header)]` is how a served
    /// method reaches the raw message it is answering, which is the only
    /// place the caller's unique name (needed for the object-path form of
    /// the argument) can be read from.
    async fn register_status_notifier_item(
        &mut self,
        service: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header.sender().map(|name| name.to_string());
        let Some(id) = item::registration_id(service, sender.as_deref()) else {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "{service:?} is not a bus name or object path"
            )));
        };

        // Re-registration is normal (see `ItemRegistry::upsert`) — answer
        // it successfully, but don't announce the same item twice.
        if !self.items.iter().any(|held| held == &id) {
            self.items.push(id.clone());
            Self::status_notifier_item_registered(&emitter, &id).await?;
        }

        // Sent unconditionally: even for a re-registration the worker wants
        // to re-read the item's title, since re-registering is one of the
        // ways an item signals that something about it changed.
        let _ = self.events.unbounded_send(Event::ItemRegistered(id));
        Ok(())
    }

    /// A host announcing itself. Ours doesn't come through here (see
    /// [`claim_watcher`] — we set the flag directly rather than calling
    /// ourselves over the bus), so in practice this fires only when a
    /// *second* bar or shell decides to display our items too.
    async fn register_status_notifier_host(
        &mut self,
        service: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        if service.trim().is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "a host must name itself".to_string(),
            ));
        }
        self.host_registered = true;
        Self::status_notifier_host_registered(&emitter).await?;
        Ok(())
    }

    /// Every registered item, in the concatenated `"<bus><path>"` form (see
    /// `item.rs`). This is what a host reads once at startup before it
    /// starts listening to the two signals below.
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.clone()
    }

    /// True once at least one host is displaying items.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        self.host_registered
    }

    /// Always [`PROTOCOL_VERSION`].
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        PROTOCOL_VERSION
    }

    /// Teaching note (signals on the serving side): a `#[zbus(signal)]`
    /// declaration in an `interface` block is a method *without a body* —
    /// the macro writes the body, which marshals the arguments and emits the
    /// signal from this object's path. Its first parameter is the
    /// [`SignalEmitter`], which is why these are called as associated
    /// functions (`Self::status_notifier_item_registered(&emitter, ..)`)
    /// rather than on `self`: emitting doesn't need the interface's state,
    /// only somewhere to emit *from*. Inside a method the emitter arrives as
    /// a `#[zbus(signal_emitter)]` argument; outside one, the worker builds
    /// it from the [`InterfaceRef`] it holds.
    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    /// Emitted when an item's bus name dies. Note that there is no
    /// `UnregisterStatusNotifierItem` method in the protocol: an item never
    /// says goodbye, it just stops existing, and noticing that is the
    /// watcher's job (see [`Event::NameOwner`] in the worker loop).
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    /// Emitted when the first host registers. Items listen for this to know
    /// a tray appeared after they started.
    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// A zbus proxy for somebody *else's* watcher — the consuming half.
///
/// Teaching note (both sides of one interface in one file): the trait below
/// and the `impl` above describe the same D-Bus interface from opposite
/// ends. They can't be the same Rust item (a proxy is generated from a
/// trait, an interface from an inherent impl), and they can't share a name
/// either, since `#[zbus::proxy]` mints `StatusNotifierWatcherProxy` from
/// the trait's name while `Watcher` is the struct the ObjectServer holds.
#[zbus::proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait StatusNotifierWatcher {
    /// Announce ourselves as a host. The argument is our host bus name.
    fn register_status_notifier_host(&self, service: &str) -> zbus::Result<()>;

    /// The incumbent's current item list.
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn status_notifier_item_registered(&self, service: String);

    #[zbus(signal)]
    fn status_notifier_item_unregistered(&self, service: String);
}

/// One thing the worker's merged stream can produce.
///
/// The three sources that feed it (our own served interface, a foreign
/// watcher's two signals, and the bus's `NameOwnerChanged`) are normalized
/// to this one type so the loop below doesn't branch on serving-vs-consuming
/// at every step — that decision is made once, when the streams are built.
#[derive(Debug)]
enum Event {
    /// A registration string appeared.
    ItemRegistered(String),
    /// A registration string was withdrawn by the watcher we're consuming.
    /// (Never produced while serving: we are the one who decides this, and
    /// we do it from `NameOwner` below.)
    ItemUnregistered(String),
    /// One already-registered item told us (via `NewIcon`/`NewStatus`/
    /// `NewTitle`) that something about it changed — Stage 19's addition.
    /// See [`item::watch_item`] for where this comes from; the response is
    /// the same as `ItemRegistered`'s: re-read the item and, if it
    /// answers, upsert it.
    ItemChanged(String),
    /// A bus name gained (`Some`) or lost (`None`) its owner.
    NameOwner { name: String, owner: Option<String> },
}

/// Who we are on this bus, decided once per session by [`claim_watcher`].
enum Claim {
    /// We own the watcher name and serve the interface.
    Serving {
        /// A handle to our own served object, for the two things a method
        /// handler can't do: dropping items whose app died, and announcing
        /// that we ourselves are a host.
        iface: InterfaceRef<Watcher>,
        /// Registrations arriving at that object.
        registrations: mpsc::UnboundedReceiver<Event>,
    },
    /// Somebody else owns it; we are an ordinary host.
    Consuming,
}

/// Builds the async stream the subscription runs. See `battery.rs`'s
/// `battery_stream` for the bridge teaching note (the channel, the runtime
/// it runs on).
pub(super) fn tray_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        tray_worker(&mut sender).await;
    })
}

/// The worker's whole life: run a session, and when it ends, clear the tray
/// and start another one.
///
/// A session ends when the *watcher name changes hands* — either the bar we
/// were consuming exited (so the name is free and we should try to claim
/// it), or somebody replaced us (so we should demote ourselves to a plain
/// host). Both are handled by simply starting over, which is why the two
/// cases don't need separate recovery code.
async fn tray_worker(sender: &mut mpsc::Sender<Message>) {
    let mut backoff = RECONNECT_BACKOFF_START;

    loop {
        match run_session(sender).await {
            SessionEnd::ChannelClosed => return,
            SessionEnd::Restart { made_progress } => {
                if made_progress {
                    backoff = RECONNECT_BACKOFF_START;
                }
            }
        }

        // Clear the tray while there is no watcher relationship to mirror.
        // A frozen row of pills would claim applications that may not be
        // running any more; an empty row is honest. This send is also how
        // we notice the UI side went away.
        if sender
            .send(Message::Updated(Tray::default()))
            .await
            .is_err()
        {
            return;
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }
}

/// Why a session stopped.
enum SessionEnd {
    /// The UI side is gone — stop for good.
    ChannelClosed,
    /// Try again. `made_progress` is true when the session got far enough to
    /// be worth resetting the backoff for (i.e. it actually established a
    /// watcher relationship), so a session bus that is missing entirely
    /// backs off instead of spinning.
    Restart { made_progress: bool },
}

/// One session: connect, decide serving-vs-consuming, then react to events
/// until the watcher name changes hands.
async fn run_session(sender: &mut mpsc::Sender<Message>) -> SessionEnd {
    // Session bus: SNI is a desktop protocol between user applications and
    // the user's shell, not a system service (contrast `battery.rs`'s
    // `Connection::system()`).
    let Ok(connection) = Connection::session().await else {
        return SessionEnd::Restart {
            made_progress: false,
        };
    };
    // Every bus connection has one; the `Option` covers peer-to-peer
    // connections, which this isn't.
    let us = connection.unique_name().map(|name| name.to_string());

    let Ok(dbus) = DBusProxy::new(&connection).await else {
        return SessionEnd::Restart {
            made_progress: false,
        };
    };

    // Subscribed *first*, before the name request below, for two reasons:
    // it can't miss the moment the watcher name changes hands, and it is
    // also how we find out an item's application died (there is no
    // "unregister" call in the protocol — see the signal's doc comment).
    let Ok(owner_changed) = dbus.receive_name_owner_changed().await else {
        return SessionEnd::Restart {
            made_progress: false,
        };
    };

    let mut events: stream::SelectAll<BoxStream<'static, Event>> = stream::SelectAll::new();
    events.push(
        owner_changed
            .filter_map(|signal| async move {
                let args = signal.args().ok()?;
                Some(Event::NameOwner {
                    name: args.name().to_string(),
                    owner: args.new_owner().as_ref().map(|owner| owner.to_string()),
                })
            })
            .boxed(),
    );

    let serving = match claim_watcher(&connection).await {
        Ok(Claim::Serving {
            iface,
            registrations,
        }) => {
            events.push(registrations.boxed());
            Some(iface)
        }
        Ok(Claim::Consuming) => None,
        Err(error) => {
            eprintln!("saola-panel: tray: could not set up the SNI watcher: {error}");
            return SessionEnd::Restart {
                made_progress: false,
            };
        }
    };

    // Built in both modes so the binding outlives the streams taken from it
    // below (a dropped proxy is not something to risk a subscription on),
    // but only *used* when consuming — while serving, talking to our own
    // watcher would mean calling ourselves over the bus, which the two
    // direct paths in `claim_watcher`/below avoid entirely.
    //
    // Uncached for the same reason `item.rs`'s proxy is: the incumbent is
    // some other project's watcher, and relying on it to implement
    // `Properties.GetAll` *and* emit `PropertiesChanged` for its item list is
    // relying on the parts of D-Bus that SNI implementations most often skip.
    // One plain `Get` per read has no such requirement.
    let proxy = match StatusNotifierWatcherProxy::builder(&connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
    {
        Ok(proxy) => proxy,
        Err(error) => {
            eprintln!("saola-panel: tray: no watcher proxy: {error}");
            return SessionEnd::Restart {
                made_progress: false,
            };
        }
    };

    let mut registry = ItemRegistry::default();
    // Which ids already have a Stage 19 change-watch stream pushed into
    // `events` — see `watch_item_for_changes`. Without this, a re-
    // registration (normal traffic — see `Watcher::register_status_
    // notifier_item`'s doc comment) would push a second, third, ... copy of
    // the same item's `NewIcon`/`NewStatus`/`NewTitle` stream every time it
    // re-registers, each producing a duplicate (harmless but wasteful)
    // `Event::ItemChanged` on every future signal.
    let mut watched: HashSet<String> = HashSet::new();

    if serving.is_none() {
        // Consuming: subscribe *before* reading the list, so an item that
        // registers during the read isn't lost in the gap between the two.
        match (
            proxy.receive_status_notifier_item_registered().await,
            proxy.receive_status_notifier_item_unregistered().await,
        ) {
            (Ok(registered), Ok(unregistered)) => {
                events.push(
                    registered
                        .filter_map(|signal| async move {
                            Some(Event::ItemRegistered(signal.args().ok()?.service().clone()))
                        })
                        .boxed(),
                );
                events.push(
                    unregistered
                        .filter_map(|signal| async move {
                            Some(Event::ItemUnregistered(
                                signal.args().ok()?.service().clone(),
                            ))
                        })
                        .boxed(),
                );
            }
            _ => {
                return SessionEnd::Restart {
                    made_progress: false,
                }
            }
        }

        for id in proxy
            .registered_status_notifier_items()
            .await
            .unwrap_or_default()
        {
            // **Strict** here, deliberately: these entries are of unknown
            // age, and a watcher that has been up for hours accumulates
            // stale ones (there is no unregister call in the protocol — an
            // item leaves only when its bus name dies, and a watcher that
            // missed that never cleans up). An entry that answers nothing is
            // treated as one of those and skipped, rather than becoming a
            // pill named after a dead unique bus name. Contrast the
            // registration path in the loop below.
            // Subscribed *before* the read, same "signals before the read"
            // principle the consuming branch already applies to the
            // foreign watcher's item list above — a change firing in the
            // gap between reading this item and subscribing to its future
            // changes would otherwise be missed forever (D-Bus never
            // replays a signal to a subscriber that registered too late).
            watch_item_for_changes(&mut events, &mut watched, &connection, &id).await;
            if let Some(probe) = resolve_item(&connection, &id).await {
                if probe.answered {
                    registry.upsert(probe.item);
                }
            }
        }
    }

    register_as_host(&connection, &proxy, serving.as_ref()).await;

    if sender
        .send(Message::Updated(registry.snapshot()))
        .await
        .is_err()
    {
        return SessionEnd::ChannelClosed;
    }

    loop {
        let Some(event) = events.next().await else {
            // Every source ended — nothing left to hear from.
            return SessionEnd::Restart {
                made_progress: true,
            };
        };

        let changed = match event {
            // **Lenient**, unlike the sweep above: an application just told
            // us it exists, so it exists. If it declines to answer `Title`
            // and `Id` (minimal implementations that only publish an icon
            // are a real thing in the wild) it still gets a pill, labelled
            // with its bus name — a bad label beats a missing tray icon.
            // `None` here means the registration string named no bus at all,
            // which our own `registration_id` cannot produce and only a
            // misbehaving foreign watcher can.
            Event::ItemRegistered(id) => {
                // Subscribed before the read — see the sweep loop above for
                // why the order matters.
                watch_item_for_changes(&mut events, &mut watched, &connection, &id).await;
                match resolve_item(&connection, &id).await {
                    Some(probe) => {
                        registry.upsert(probe.item);
                        true
                    }
                    None => false,
                }
            }
            // Stage 19: `NewIcon`/`NewStatus`/`NewTitle` all land here —
            // `item::watch_item` already reduced whichever one fired down
            // to just the id, so the response is the same full re-read as
            // a fresh registration. No `watch_item_for_changes` call here:
            // this event could only have arrived from a stream that call
            // already pushed, so the id is in `watched` by construction.
            Event::ItemChanged(id) => match resolve_item(&connection, &id).await {
                Some(probe) => {
                    registry.upsert(probe.item);
                    true
                }
                None => false,
            },
            Event::ItemUnregistered(id) => registry.remove(&id),
            Event::NameOwner { name, owner } => {
                if name == WATCHER_BUS_NAME {
                    // The one condition that ends a session. `owner == us`
                    // is *our own* acquisition of the name arriving on the
                    // stream we deliberately subscribed to first — the
                    // normal serving case, and emphatically not a reason to
                    // tear the session down.
                    if owner.is_some() && owner == us {
                        continue;
                    }
                    return SessionEnd::Restart {
                        made_progress: true,
                    };
                }

                if owner.is_some() {
                    // A name gaining an owner is only interesting for items,
                    // and an item announces itself by registering.
                    continue;
                }

                let dropped = registry.remove_owned_by(&name);
                if dropped.is_empty() {
                    continue;
                }
                // Serving: tell the rest of the bus too. Consuming: the
                // incumbent watcher will send its own Unregistered signal,
                // which `remove` above already made idempotent.
                if let Some(iface) = &serving {
                    drop_items(iface, &dropped).await;
                }
                true
            }
        };

        if changed
            && sender
                .send(Message::Updated(registry.snapshot()))
                .await
                .is_err()
        {
            return SessionEnd::ChannelClosed;
        }
    }
}

/// Try to become the watcher; fall back to consuming the incumbent.
///
/// Teaching note (**object first, name second**): the object is registered
/// with the ObjectServer *before* the name is requested, because between
/// acquiring a well-known name and having something answer at
/// `/StatusNotifierWatcher` there is a window in which an application's
/// `RegisterStatusNotifierItem` call would come back "unknown object" — and
/// SNI items generally do not retry. zbus considers this important enough to
/// log a warning if you request a name before setting up an object server at
/// all.
///
/// Teaching note (**the flags**): `request_name`'s defaults are
/// `AllowReplacement | ReplaceExisting | DoNotQueue` — note the middle one.
/// Using them would mean *stealing* the name from another bar that politely
/// allowed replacement, which is the opposite of what a well-behaved second
/// bar should do. So the flags are spelled out:
///
/// - `AllowReplacement` — a shell that starts later and genuinely wants the
///   name (plasmashell, say) can take it; we would rather demote ourselves
///   than break the session's tray.
/// - `DoNotQueue` — without it, a failed request silently parks us in the
///   bus's ownership queue, and we would become the watcher at some
///   unobservable later moment while still behaving like a consumer. An
///   immediate, visible "no" is the only answer worth having here.
/// - **not** `ReplaceExisting` — see above. This is also what makes the
///   `AllowReplacement` choice safe from ping-pong: two saola-panels can
///   never take the name off each other, because neither ever asks to.
///
/// Teaching note (**the reply is an error, not a variant**): zbus turns
/// `RequestNameReply::Exists` into `Err(zbus::Error::NameTaken)` rather than
/// returning it, so "somebody else is the watcher" — the *expected* case on
/// a machine already running another bar — arrives as an error and must be
/// matched for explicitly.
async fn claim_watcher(connection: &Connection) -> zbus::Result<Claim> {
    let (events, registrations) = mpsc::unbounded();
    let watcher = Watcher {
        items: Vec::new(),
        host_registered: false,
        events,
    };

    connection
        .object_server()
        .at(WATCHER_OBJECT_PATH, watcher)
        .await?;

    let claimed = connection
        .request_name_with_flags(
            WATCHER_BUS_NAME,
            RequestNameFlags::AllowReplacement | RequestNameFlags::DoNotQueue,
        )
        .await;

    match claimed {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            let iface = connection
                .object_server()
                .interface::<_, Watcher>(WATCHER_OBJECT_PATH)
                .await?;
            Ok(Claim::Serving {
                iface,
                registrations,
            })
        }
        // zbus turns `Exists` into `Err(NameTaken)` before we ever see it,
        // and `InQueue` cannot happen with `DoNotQueue` — but matching all
        // three keeps this honest rather than relying on an implementation
        // detail of zbus's error mapping to stay put.
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
            // Take our unused watcher object back down: another process is
            // answering for this interface now, and leaving a second,
            // invisible one on our unique name would only confuse anything
            // that introspects us.
            connection
                .object_server()
                .remove::<Watcher, _>(WATCHER_OBJECT_PATH)
                .await?;
            Ok(Claim::Consuming)
        }
        Err(error) => Err(error),
    }
}

/// Register as a `StatusNotifierHost`, both ways it has to be done.
///
/// The protocol asks for two distinct things and applications check *both*,
/// so neither is optional:
///
/// 1. own a bus name of the form `org.kde.StatusNotifierHost-<something
///    unique>` — the mere existence of that name is the advertisement, and
///    the host serves no interface on it (the spec is explicit that a host
///    needs no properties, methods or signals of its own);
/// 2. tell the watcher, so it flips `IsStatusNotifierHostRegistered` and
///    emits `StatusNotifierHostRegistered` — the flag many items consult
///    before bothering to publish an icon at all.
///
/// Step 2 is where serving and consuming differ, and the difference is the
/// whole reason this function takes `serving`: calling our *own* method over
/// the bus would work (the daemon would route the message straight back to
/// us), but it is a needless round-trip through the very object we already
/// hold a handle to, and one that has to complete before the worker's loop
/// starts. Setting the flag directly is the same state change without the
/// re-entrancy.
///
/// Failures are logged and swallowed: a host name we couldn't claim, or an
/// incumbent watcher that refuses the call, both leave the tray in the
/// degraded-but-working state of "we still see the items, some applications
/// may decide not to publish one" — much better than no panel.
async fn register_as_host(
    connection: &Connection,
    proxy: &StatusNotifierWatcherProxy<'_>,
    serving: Option<&InterfaceRef<Watcher>>,
) {
    let name = host_bus_name(std::process::id());
    if let Err(error) = connection
        .request_name_with_flags(name.clone(), RequestNameFlags::DoNotQueue.into())
        .await
    {
        eprintln!("saola-panel: tray: could not claim {name}: {error}");
    }

    match serving {
        Some(iface) => {
            iface.get_mut().await.host_registered = true;
            if let Err(error) =
                Watcher::status_notifier_host_registered(iface.signal_emitter()).await
            {
                eprintln!("saola-panel: tray: could not announce the host: {error}");
            }
        }
        None => {
            if let Err(error) = proxy.register_status_notifier_host(&name).await {
                eprintln!("saola-panel: tray: the watcher refused our host registration: {error}");
            }
        }
    }
}

/// Our host bus name. A pure function of the process id so it can be
/// unit-tested without a bus; the pid is what makes it unique between
/// concurrently-running panels, which is all the protocol asks of the suffix.
fn host_bus_name(pid: u32) -> String {
    format!("org.kde.StatusNotifierHost-{pid}")
}

/// Read an item's name, under a timeout. See [`ITEM_READ_TIMEOUT`] for why
/// the timeout is not optional.
async fn resolve_item(connection: &Connection, id: &str) -> Option<item::ItemProbe> {
    tokio::time::timeout(ITEM_READ_TIMEOUT, item::read_item(connection, id))
        .await
        .ok()
        .flatten()
}

/// Subscribe to `id`'s Stage 19 change signals (`item::watch_item`) and push
/// the resulting stream into `events`, unless it's already there.
///
/// Called from both places an item is *about to be* resolved (the initial
/// sweep of a foreign watcher's list, and a fresh `ItemRegistered`) —
/// **before** the corresponding `resolve_item` call, never after. See
/// `item::watch_item`'s doc comment for why the order is load-bearing: this
/// function's `.await` only returns once the three `AddMatch` calls
/// underneath it have round-tripped to the bus daemon, so awaiting it
/// first is what guarantees a change firing right after registration can't
/// be missed.
///
/// The `watched` set is what keeps a re-registering item from accumulating
/// a second, third, ... copy of its own `NewIcon`/`NewStatus`/`NewTitle`
/// stream (see `watched`'s doc comment at its declaration in
/// `run_session`).
///
/// Wrapped in [`ITEM_READ_TIMEOUT`] for the same reason [`resolve_item`]
/// is: three unbounded `AddMatch` round-trips are exactly the kind of
/// per-item cost that must not be allowed to stall the whole (single-task)
/// worker if one item's connection is wedged. A timeout here degrades to
/// "this item just won't refresh on its own signals" — the tray still
/// shows it, still updates on its next re-registration — never a hung
/// worker.
async fn watch_item_for_changes(
    events: &mut stream::SelectAll<BoxStream<'static, Event>>,
    watched: &mut HashSet<String>,
    connection: &Connection,
    id: &str,
) {
    if !watched.insert(id.to_string()) {
        return;
    }
    let changes = tokio::time::timeout(
        ITEM_READ_TIMEOUT,
        item::watch_item(connection.clone(), id.to_string()),
    )
    .await
    .unwrap_or_else(|_| stream::empty().boxed());
    events.push(changes.map(Event::ItemChanged).boxed());
}

/// Drop items from our *served* registry and tell the bus (serving mode
/// only).
///
/// The lock on the interface is taken and released around the mutation
/// alone, before any `await` on the emit: an `InterfaceRef` guard held
/// across an await would block the ObjectServer from dispatching any further
/// method call to this object — including the `RegisterStatusNotifierItem`
/// that is quite likely arriving at that exact moment, because applications
/// re-register when they see the tray change.
async fn drop_items(iface: &InterfaceRef<Watcher>, ids: &[String]) {
    {
        let mut watcher = iface.get_mut().await;
        watcher.items.retain(|held| !ids.contains(held));
    }

    for id in ids {
        if let Err(error) =
            Watcher::status_notifier_item_unregistered(iface.signal_emitter(), id).await
        {
            eprintln!("saola-panel: tray: could not announce {id} leaving: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stage 20's model, which this file's live-bus test both *serves*
    // (`FakeMenu`) and *consumes* (`menu::read_menu`). Not brought in at the
    // top of the file because nothing outside these tests touches it —
    // `menu.rs` is wired to `Panel::update` by Stage 21, not by this module.
    use crate::modules::tray::menu;

    // ---------------------------------------------------------------
    // Pure tests. Everything above this line runs in `cargo test`.
    // ---------------------------------------------------------------

    #[test]
    fn the_host_bus_name_follows_the_protocols_naming_convention() {
        assert_eq!(host_bus_name(4077), "org.kde.StatusNotifierHost-4077");
        // Two panels on one session must not collide, which is the only
        // thing the protocol asks of the suffix.
        assert_ne!(host_bus_name(4077), host_bus_name(4078));
    }

    #[test]
    fn the_protocol_names_are_the_kde_ones_every_implementation_actually_uses() {
        // Guarding a decision, not an implementation detail: the
        // freedesktop draft says `org.freedesktop.StatusNotifierWatcher`,
        // and using that name would produce a watcher no application in the
        // world looks for. See `WATCHER_BUS_NAME`'s doc comment.
        assert_eq!(WATCHER_BUS_NAME, "org.kde.StatusNotifierWatcher");
        assert_eq!(WATCHER_OBJECT_PATH, "/StatusNotifierWatcher");
    }

    // ---------------------------------------------------------------
    // The live-bus test. `#[ignore]`d, so `cargo test` (and CI) skip it.
    //
    //     dbus-run-session -- cargo test --bin saola-panel -- --ignored
    //
    // **Run it on a private bus, never on a real session** — it claims
    // `org.kde.StatusNotifierWatcher`, and taking that name off a running
    // desktop would break that desktop's tray for as long as the test runs.
    // `dbus-run-session` starts a throwaway daemon and points
    // `$DBUS_SESSION_BUS_ADDRESS` at it, which is exactly what's wanted.
    //
    // This is the only place any of the D-Bus plumbing in this file is
    // exercised at all — everything else here needs a bus to mean anything,
    // and it is precisely the part most likely to be wrong.
    //
    // It is deliberately **one** test covering every phase in sequence
    // rather than several: they would all contend for the one watcher name
    // on the one bus, and `cargo test` runs tests in parallel by default. A
    // second test here would not be a second scenario, it would be a
    // coin-flip. Add phases to this one instead.
    // ---------------------------------------------------------------

    /// A minimal `org.kde.StatusNotifierItem`, standing in for a real
    /// application's tray icon.
    ///
    /// `title`/`status` are plain fields, not `Mutex`-wrapped: the
    /// ObjectServer already synchronizes access to this struct behind its
    /// own lock (that's what `InterfaceRef::get_mut()` is), so a second lock
    /// inside it would be redundant. Stage 19's phase 2.5 mutates both
    /// through that same `get_mut()` handle, from outside, to simulate an
    /// application changing its own title/status.
    ///
    /// Deliberately does **not** implement `IconName` — the point is to
    /// exercise `resolve_icon`'s fallback path (`item.rs`'s module doc
    /// comment): a real client proxy calling an unimplemented property
    /// getter gets an error reply, `resolve_icon` treats that as "no name",
    /// and falls through to `IconPixmap` below, which this item *does*
    /// answer.
    struct FakeItem {
        title: String,
        status: String,
        /// Set by a real `Activate` call landing on this object — proves
        /// `item::send_activate` reaches a real item over the bus, and
        /// with the documented `(0, 0)` coordinates.
        activated: Option<(i32, i32)>,
    }

    /// Where [`FakeMenu`] is served, and what [`FakeItem`]'s `Menu` property
    /// points at. Not `item::DEFAULT_ITEM_PATH` and deliberately not a
    /// well-known name either: the whole point of the `Menu` property is that
    /// the path is the application's business, so a test that used a
    /// predictable one would prove less.
    const FAKE_MENU_PATH: &str = "/org/saola/test/FakeMenu";

    impl Default for FakeItem {
        fn default() -> Self {
            Self {
                title: "Fake Item".to_string(),
                status: "Active".to_string(),
                activated: None,
            }
        }
    }

    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl FakeItem {
        #[zbus(property)]
        fn title(&self) -> String {
            self.title.clone()
        }

        #[zbus(property)]
        fn id(&self) -> String {
            "fake-item".to_string()
        }

        /// One 1×1 ARGB32-network-byte-order pixel: `A=0x11 R=0x22 G=0x33
        /// B=0x44`. Round-tripping this through the real `a(iiay)` wire
        /// type is something `item.rs`'s pure byte-shuffle unit tests
        /// cannot exercise on their own — they operate on already-decoded
        /// Rust values, never on bytes that actually crossed the bus.
        #[zbus(property)]
        fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            vec![(1, 1, vec![0x11, 0x22, 0x33, 0x44])]
        }

        #[zbus(property)]
        fn status(&self) -> String {
            self.status.clone()
        }

        /// Stage 20: where this item's `com.canonical.dbusmenu` object lives.
        /// The path is on *this* connection's bus name — that pairing is the
        /// thing `item::read_menu_address` has to get right.
        #[zbus(property)]
        fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
            zbus::zvariant::OwnedObjectPath::try_from(FAKE_MENU_PATH)
                .expect("a valid object path literal")
        }

        async fn activate(&mut self, x: i32, y: i32) -> zbus::fdo::Result<()> {
            self.activated = Some((x, y));
            Ok(())
        }

        /// Emitted by phase 2.5 to simulate the application's own title
        /// changing after registration — the gap Stage 18's handoff
        /// flagged ("this stage reads the title once ... and never
        /// refreshes it"), closed by Stage 19's `item::watch_item`.
        #[zbus(signal)]
        async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

        /// Emitted by phase 2.5 to simulate a `Status` change.
        #[zbus(signal)]
        async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
    }

    /// A minimal `com.canonical.dbusmenu` server — Stage 20's half of the
    /// live-bus test, standing in for a real application's context menu.
    ///
    /// The menu it serves (see [`Self::get_layout`]) is shaped to exercise
    /// every decoding rule at once over a *real bus*, which is what the pure
    /// fixture tests in `menu.rs` cannot do: a mnemonic label, a row that
    /// omits every property it can, a separator, a nested submenu, an
    /// invisible row, and a checkmark that is on.
    ///
    /// The three recorded fields are the assertions: `about_to_show` and
    /// `events` prove the two outgoing calls land, and
    /// `about_to_show_when_layout_read` is how the test proves the *order* —
    /// `AboutToShow` before `GetLayout`, which is the entire reason
    /// `AboutToShow` exists (see `menu.rs`'s module doc comment).
    #[derive(Default)]
    struct FakeMenu {
        about_to_show: Vec<i32>,
        events: Vec<(i32, String)>,
        about_to_show_when_layout_read: Option<usize>,
    }

    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl FakeMenu {
        async fn get_layout(
            &mut self,
            parent_id: i32,
            recursion_depth: i32,
            _property_names: Vec<String>,
        ) -> zbus::fdo::Result<(u32, menu::RawMenuNode)> {
            self.about_to_show_when_layout_read = Some(self.about_to_show.len());
            // The panel always asks for the whole tree from the root; if that
            // ever changes silently, this is where it shows up.
            assert_eq!(parent_id, 0, "the panel reads the menu from its root");
            assert_eq!(recursion_depth, -1, "the panel asks for the whole tree");

            // Property keys as *string literals*, not `menu.rs`'s private
            // constants: this is the sibling module's independent statement
            // of what the wire spelling is, so a typo'd constant over there
            // would fail here rather than agree with itself.
            let layout = menu::fixture_node(
                0,
                &[],
                vec![
                    // Sends nothing at all — every property defaults.
                    menu::fixture_node(1, &[], Vec::new()),
                    // A mnemonic label, and a checkmark that is on.
                    menu::fixture_node(
                        2,
                        &[
                            ("label", zbus::zvariant::Value::from("_Show __Everything")),
                            ("toggle-type", zbus::zvariant::Value::from("checkmark")),
                            ("toggle-state", zbus::zvariant::Value::from(1_i32)),
                        ],
                        Vec::new(),
                    ),
                    menu::fixture_node(
                        3,
                        &[("type", zbus::zvariant::Value::from("separator"))],
                        Vec::new(),
                    ),
                    // A submenu, two levels deep, with an invisible and a
                    // disabled row inside it.
                    menu::fixture_node(
                        4,
                        &[
                            ("label", zbus::zvariant::Value::from("More")),
                            ("children-display", zbus::zvariant::Value::from("submenu")),
                        ],
                        vec![
                            menu::fixture_node(
                                5,
                                &[
                                    ("label", zbus::zvariant::Value::from("Hidden")),
                                    ("visible", zbus::zvariant::Value::from(false)),
                                ],
                                Vec::new(),
                            ),
                            menu::fixture_node(
                                6,
                                &[
                                    ("label", zbus::zvariant::Value::from("Greyed")),
                                    ("enabled", zbus::zvariant::Value::from(false)),
                                ],
                                Vec::new(),
                            ),
                        ],
                    ),
                ],
            );

            Ok((11, layout))
        }

        async fn about_to_show(&mut self, id: i32) -> zbus::fdo::Result<bool> {
            self.about_to_show.push(id);
            // `true` ("you should re-read") on purpose: the panel ignores the
            // answer and reads the layout unconditionally, and answering the
            // *interesting* value is how that stays deliberate.
            Ok(true)
        }

        async fn event(
            &mut self,
            id: i32,
            event_id: String,
            _data: zbus::zvariant::OwnedValue,
            _timestamp: u32,
        ) -> zbus::fdo::Result<()> {
            self.events.push((id, event_id));
            Ok(())
        }

        /// Emitted by the test to stand in for "the application rebuilt its
        /// menu" — the structural half of what `menu::watch_menu` listens
        /// for.
        #[zbus(signal)]
        async fn layout_updated(
            emitter: &SignalEmitter<'_>,
            revision: u32,
            parent: i32,
        ) -> zbus::Result<()>;

        /// The cheap half: properties moved, structure didn't. Declared with
        /// the real `a(ia{sv})` / `a(ias)` argument types, because getting
        /// those wrong on *either* side is exactly the kind of mistake a
        /// same-process fixture would hide and a real bus won't.
        #[zbus(signal)]
        async fn items_properties_updated(
            emitter: &SignalEmitter<'_>,
            updated: Vec<(
                i32,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            )>,
            removed: Vec<(i32, Vec<String>)>,
        ) -> zbus::Result<()>;
    }

    /// A [`TrayIcon::Pixmap`]'s decoded pixels, as a plain, comparable
    /// value — `None` for every other case (no icon, or a variant this
    /// live-bus test never produces).
    ///
    /// Exists because [`iced::widget::image::Handle`]'s own `PartialEq`
    /// isn't useful for this test: `Handle::from_rgba` mints a fresh
    /// [`iced::widget::image::Id`] on *every call*, even for byte-identical
    /// pixels (see that function's doc comment) — it is an identity for
    /// the renderer's rasterization cache, not a content hash. Two
    /// independent resolutions of the exact same `IconPixmap` wire data
    /// (one per panel, in phase 5 below) therefore build two `Handle`s
    /// that are never `==` to each other, despite decoding to identical
    /// bytes. Comparing what this function extracts is what "the same
    /// icon" actually means here.
    fn icon_pixels(icon: Option<&item::TrayIcon>) -> Option<(u32, u32, Vec<u8>)> {
        match icon {
            Some(item::TrayIcon::Pixmap(iced::widget::image::Handle::Rgba {
                width,
                height,
                pixels,
                ..
            })) => Some((*width, *height, pixels.to_vec())),
            _ => None,
        }
    }

    /// Wait for the next tray snapshot satisfying `wanted`, or fail loudly
    /// rather than hanging the suite.
    async fn next_tray_where(
        receiver: &mut mpsc::Receiver<Message>,
        wanted: impl Fn(&Tray) -> bool,
    ) -> Tray {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let Some(Message::Updated(tray)) = receiver.next().await else {
                    panic!("the tray worker hung up");
                };
                if wanted(&tray) {
                    return tray;
                }
            }
        })
        .await
        .expect("timed out waiting for the expected tray snapshot")
    }

    /// Read one property off the watcher the way an application would: over
    /// the bus, from the outside, through `org.freedesktop.DBus.Properties`.
    ///
    /// `OwnedValue` rather than `Value<'_>`: a borrowed variant would keep
    /// the reply message alive, and the reply is a temporary here.
    async fn watcher_property<T>(app: &Connection, name: &str) -> T
    where
        T: TryFrom<zbus::zvariant::OwnedValue>,
        <T as TryFrom<zbus::zvariant::OwnedValue>>::Error: std::fmt::Debug,
    {
        let reply = app
            .call_method(
                Some(WATCHER_BUS_NAME),
                WATCHER_OBJECT_PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.kde.StatusNotifierWatcher", name),
            )
            .await
            .expect("the watcher answers property reads");

        let value: zbus::zvariant::OwnedValue =
            reply.body().deserialize().expect("a variant came back");

        T::try_from(value).expect("the property's declared type")
    }

    #[test]
    #[ignore = "needs a private session bus — see the comment above"]
    fn the_whole_watcher_host_item_lifecycle_over_a_real_bus() {
        let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
        runtime.block_on(async {
            // -- Phase 1: nothing provides the watcher, so we become it. --

            let (mut server_sender, mut server) = mpsc::channel(8);
            let serving = tokio::spawn(async move { run_session(&mut server_sender).await });

            // The first snapshot is the empty tray the worker sends once it
            // has settled into a watcher relationship — i.e. proof it got
            // all the way through `claim_watcher` and `register_as_host`.
            next_tray_where(&mut server, |tray| tray.items.is_empty()).await;

            let app = Connection::session().await.expect("a session bus");
            app.object_server()
                .at(item::DEFAULT_ITEM_PATH, FakeItem::default())
                .await
                .expect("export the fake item");
            // Stage 20: the item's context menu, at the path its `Menu`
            // property names — a *second* object on the same connection,
            // which is exactly how a real application publishes one.
            app.object_server()
                .at(FAKE_MENU_PATH, FakeMenu::default())
                .await
                .expect("export the fake menu");
            let app_name = app
                .unique_name()
                .expect("a bus connection has a unique name")
                .to_string();

            assert!(
                watcher_property::<bool>(&app, "IsStatusNotifierHostRegistered").await,
                "the panel must advertise itself as a host, or items won't publish at all"
            );

            // -- Phase 2: an item registers, in the bare bus name form. --

            app.call_method(
                Some(WATCHER_BUS_NAME),
                WATCHER_OBJECT_PATH,
                Some("org.kde.StatusNotifierWatcher"),
                "RegisterStatusNotifierItem",
                &(app_name.as_str(),),
            )
            .await
            .expect("the watcher accepts a bus-name-form registration");

            let expected = format!("{app_name}{}", item::DEFAULT_ITEM_PATH);
            let tray = next_tray_where(&mut server, |tray| !tray.items.is_empty()).await;
            assert_eq!(tray.items.len(), 1);
            assert_eq!(tray.items[0].label(), "Fake Item");
            assert_eq!(
                tray.items[0].id(),
                expected,
                "the watcher stores the normalized bus+path form"
            );
            // Stage 19: `FakeItem` implements no `IconName` at all, so
            // `resolve_icon` must fall through to `IconPixmap` — proving the
            // real `a(iiay)` wire type round-trips through the proxy (the
            // pure byte-shuffle tests in `item.rs` only ever see
            // already-decoded Rust values, never bytes that crossed a bus).
            assert!(
                matches!(tray.items[0].icon(), Some(item::TrayIcon::Pixmap(_))),
                "IconName is unanswered, so the item's IconPixmap should resolve instead"
            );
            // `FakeItem::status` defaults to "Active" — see phase 2.5 below
            // for the changed-status case.
            assert_eq!(tray.items[0].status(), item::ItemStatus::Active);
            assert_eq!(
                watcher_property::<Vec<String>>(&app, "RegisteredStatusNotifierItems").await,
                vec![expected],
                "and reports that same form to every other host on the bus"
            );

            // -- Phase 2.5 (Stage 19): the application changes its own
            //    title and status, and a real left-click activates it.
            //    Closes the exact gap Stage 18's handoff flagged ("this
            //    stage reads the title once ... and never refreshes it"),
            //    and proves `item::send_activate` reaches a real item over
            //    the bus with the documented `(0, 0)` coordinates. --

            let item_id = tray.items[0].id().to_string();
            let item_ref = app
                .object_server()
                .interface::<_, FakeItem>(item::DEFAULT_ITEM_PATH)
                .await
                .expect("the fake item is still served on this connection");

            {
                // Scoped, and dropped before any `await` below — the same
                // rule `drop_items` documents: holding an `InterfaceRef`
                // guard across an `await` would block the ObjectServer from
                // dispatching any other call to this object.
                let mut fake = item_ref.get_mut().await;
                fake.title = "Renamed Fake Item".to_string();
                fake.status = "NeedsAttention".to_string();
            }
            FakeItem::new_title(item_ref.signal_emitter())
                .await
                .expect("emitting NewTitle");
            FakeItem::new_status(item_ref.signal_emitter(), "NeedsAttention")
                .await
                .expect("emitting NewStatus");

            let refreshed = next_tray_where(&mut server, |tray| {
                tray.items
                    .first()
                    .is_some_and(|item| item.label() == "Renamed Fake Item")
            })
            .await;
            assert_eq!(
                refreshed.items[0].status(),
                item::ItemStatus::NeedsAttention,
                "NewStatus should trigger the same full re-read as NewTitle"
            );

            item::send_activate(item_id.clone()).await;
            assert_eq!(
                item_ref.get_mut().await.activated,
                Some((0, 0)),
                "Activate must reach the item, with the documented (0, 0) coordinates"
            );

            // -- Phase 2.6 (Stage 20): the item's dbusmenu. Everything in
            //    `menu.rs` that a fixture test cannot reach — the `Menu`
            //    property lookup, the real `(ia{sv}av)` wire type in both
            //    directions, `AboutToShow` landing *before* `GetLayout`, and
            //    `Event(id, "clicked", ...)` reaching the application. --

            let menu_ref = app
                .object_server()
                .interface::<_, FakeMenu>(FAKE_MENU_PATH)
                .await
                .expect("the fake menu is served on this connection");

            let opened = menu::read_menu(&item_id, 0)
                .await
                .expect("the item publishes a Menu path and the menu answers GetLayout");

            assert_eq!(opened.revision, 11, "the revision comes back with the tree");
            let rows = &opened.root.children;
            assert_eq!(rows.len(), 4);

            // Row 1 sent no properties at all: every field is the spec's
            // default, proven here over a real bus rather than over a
            // hand-built `HashMap`.
            assert_eq!(rows[0].id, 1);
            assert_eq!(
                rows[0],
                menu::MenuNode {
                    id: 1,
                    ..menu::MenuNode::default()
                }
            );

            // Row 2: mnemonic stripping and a toggle, end to end.
            assert_eq!(rows[1].label, "Show _Everything");
            assert_eq!(rows[1].toggle_type, menu::ToggleType::Checkmark);
            assert_eq!(rows[1].toggle_state, menu::ToggleState::On);

            assert_eq!(rows[2].kind, menu::MenuItemKind::Separator);

            // Row 4: the nested submenu, with its two awkward children.
            assert!(rows[3].has_submenu);
            assert_eq!(rows[3].children.len(), 2);
            assert_eq!(rows[3].children[0].label, "Hidden");
            assert!(!rows[3].children[0].visible);
            assert_eq!(rows[3].children[1].label, "Greyed");
            assert!(!rows[3].children[1].enabled);
            assert!(rows[3].children[1].visible, "only `visible` hides a row");

            {
                let served = menu_ref.get_mut().await;
                assert_eq!(
                    served.about_to_show,
                    vec![0],
                    "AboutToShow must be called for the node being opened"
                );
                assert_eq!(
                    served.about_to_show_when_layout_read,
                    Some(1),
                    "AboutToShow must land BEFORE GetLayout — a lazily-populating \
                     application has no other chance to fill the menu in"
                );
                assert!(served.events.is_empty(), "no click yet");
            }

            // A click on the last row. Same command-out shape as
            // `send_activate`: fresh connection, one call, drop.
            menu::send_clicked(item_id.clone(), 4).await;
            assert_eq!(
                menu_ref.get_mut().await.events,
                vec![(4, "clicked".to_string())],
                "Event must reach the application with the row's id and \"clicked\""
            );

            // Both refresh signals, over the real bus. `watch_menu` is
            // awaited to completion *before* anything is emitted — the same
            // subscribe-then-act rule `item::watch_item` documents, and the
            // exact race that made Stage 19's phase 2.5 hang when it was
            // deferred into a lazily-polled stream.
            let panel_side = Connection::session().await.expect("a session bus");
            let mut changes = menu::watch_menu(panel_side, &item_id).await;

            FakeMenu::layout_updated(menu_ref.signal_emitter(), 12, 0)
                .await
                .expect("emitting LayoutUpdated");
            tokio::time::timeout(Duration::from_secs(10), changes.next())
                .await
                .expect("LayoutUpdated should reach the menu watch")
                .expect("the watch stream is still open");

            FakeMenu::items_properties_updated(menu_ref.signal_emitter(), Vec::new(), Vec::new())
                .await
                .expect("emitting ItemsPropertiesUpdated");
            tokio::time::timeout(Duration::from_secs(10), changes.next())
                .await
                .expect("ItemsPropertiesUpdated should reach the same watch")
                .expect("the watch stream is still open");

            // Same rule as `item_ref` below: an `InterfaceRef` keeps its own
            // clone of `app`'s connection alive, so it has to go before
            // phase 3 pretends the application exited.
            drop(menu_ref);

            // `InterfaceRef` holds its own clone of the connection it came
            // from (needed to emit signals) — dropping `app` alone would
            // *not* actually close the socket while this is still around,
            // which would silently defeat phase 3 below (an application
            // "exiting" that secretly keeps its connection alive). Drop it
            // first, explicitly.
            drop(item_ref);

            // -- Phase 3: the application exits. There is no unregister call
            //    in the protocol — the bus name simply loses its owner, and
            //    noticing that is the watcher's whole job. --

            drop(app);
            next_tray_where(&mut server, |tray| tray.items.is_empty()).await;

            // -- Phase 4: a second panel starts. The name is taken, so it
            //    must fall through to CONSUMING — which is also the only
            //    path that exercises the served `RegisterStatusNotifierHost`
            //    method, since the serving panel sets that flag directly.
            //
            //    Expect one line on stderr here: both workers are in *this*
            //    process, so they derive the same `org.kde.
            //    StatusNotifierHost-<pid>` name and the second one can't
            //    claim it. That is a test artifact (two real panels are two
            //    processes with two pids) — and incidentally a free check
            //    that a failed host-name claim degrades quietly instead of
            //    taking the worker down. --

            let (mut client_sender, mut client) = mpsc::channel(8);
            let consuming = tokio::spawn(async move { run_session(&mut client_sender).await });
            next_tray_where(&mut client, |tray| tray.items.is_empty()).await;

            // -- Phase 5: an item registers in the *other* form of the
            //    quirk — a bare object path, which means nothing until the
            //    watcher resolves it against the message sender — and both
            //    panels must end up with identical item models (see
            //    `icon_pixels`'s doc comment for why that's asserted
            //    field-by-field rather than as one whole-`TrayItem`
            //    `assert_eq!`, as it was pre-Stage 19). --

            let app = Connection::session().await.expect("a session bus");
            app.object_server()
                .at("/org/ayatana/NotificationItem/fake", FakeItem::default())
                .await
                .expect("export the fake item");
            let app_name = app
                .unique_name()
                .expect("a bus connection has a unique name")
                .to_string();

            app.call_method(
                Some(WATCHER_BUS_NAME),
                WATCHER_OBJECT_PATH,
                Some("org.kde.StatusNotifierWatcher"),
                "RegisterStatusNotifierItem",
                &("/org/ayatana/NotificationItem/fake",),
            )
            .await
            .expect("the watcher accepts a path-form registration");

            let served = next_tray_where(&mut server, |tray| !tray.items.is_empty()).await;
            assert_eq!(
                served.items[0].id(),
                format!("{app_name}/org/ayatana/NotificationItem/fake"),
                "the path form resolves against the caller's unique name"
            );
            assert_eq!(served.items[0].label(), "Fake Item");

            let consumed = next_tray_where(&mut client, |tray| !tray.items.is_empty()).await;
            assert_eq!(consumed.items.len(), served.items.len());
            for (c, s) in consumed.items.iter().zip(served.items.iter()) {
                assert_eq!(
                    c.id(),
                    s.id(),
                    "a consuming and a serving panel must agree on id"
                );
                assert_eq!(c.label(), s.label(), "and on label");
                assert_eq!(c.status(), s.status(), "and on status");
                // Not a whole-`TrayItem` `assert_eq!` (as it was pre-Stage
                // 19): `image::Handle::from_rgba` mints a fresh `Id` on
                // *every* call (its own doc comment), even for
                // byte-identical pixels — the consuming and serving panels
                // each independently call `pixmap_icon` against the same
                // wire data, so their handles' `Id`s always differ even
                // though they decoded the exact same bytes. Comparing the
                // decoded pixels themselves is what "the same tray" means.
                assert_eq!(
                    icon_pixels(c.icon()),
                    icon_pixels(s.icon()),
                    "and on the icon's decoded pixels"
                );
            }

            // -- Phase 6: both notice the application leaving, by different
            //    routes: the server from `NameOwnerChanged`, the client from
            //    the `StatusNotifierItemUnregistered` the server emits. --

            drop(app);
            next_tray_where(&mut server, |tray| tray.items.is_empty()).await;
            next_tray_where(&mut client, |tray| tray.items.is_empty()).await;

            serving.abort();
            consuming.abort();
        });
    }
}
