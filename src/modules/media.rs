//! The bar's now-playing status glyph, fed by MPRIS over D-Bus.
//!
//! Teaching note (session bus vs. system bus): battery (UPower) and network
//! (iwd) are both **system** services — one UPower/iwd process serves every
//! user on the machine, so `Connection::system()` is correct for them. MPRIS
//! players (Spotify, a browser tab, `mpv`, ...) are **per-user desktop
//! applications** — each one owns a well-known bus name on the *session*
//! bus, the per-user D-Bus instance started with the user's graphical
//! session. `Connection::session()` below is this module's one real
//! difference from `battery`/`network`'s bridge shape; everything else
//! (proxy macro, `iced::stream::channel`, `Subscription::run` on a fn
//! pointer) copies verbatim.
//!
//! Teaching note (tracking a *set* of players, not one fixed object):
//! UPower always has `DisplayDevice`; iwd's Station is discovered once via
//! `ObjectManager`. MPRIS has neither — any number of players can come and
//! go over the bar's lifetime, each announcing itself as a well-known bus
//! name `org.mpris.MediaPlayer2.<name>`. This worker tracks that set by
//! watching `org.freedesktop.DBus.NameOwnerChanged` (filtered to the MPRIS
//! prefix) for appearance/disappearance, plus an initial `ListNames` sweep
//! so a player already running when the panel starts isn't missed (
//! `NameOwnerChanged` only fires for *future* ownership changes). Each
//! tracked player gets its own proxy on `/org/mpris/MediaPlayer2`'s
//! `org.mpris.MediaPlayer2.Player` interface, watched for
//! `PlaybackStatus`/`Metadata` property changes exactly like `battery`'s
//! single proxy is. All of it — the name-owner stream and every player's
//! property-change stream — is merged into one dynamic
//! `futures::stream::SelectAll` so the worker can `.next().await` a single
//! unified event stream instead of hand-rolling a `tokio::select!` over an
//! unknown number of branches.
//!
//! # Design language (2026-08-01: media becomes a status glyph)
//!
//! Style guide §7, "Media is a status glyph": now-playing moved out of the
//! bar's left region entirely and into the status cluster, where it renders
//! as one more bare glyph beside `volume`/`battery` — no pill, no title
//! text, no fill of its own, and (unlike `clock`/`mark`) no per-style
//! surface treatment either, since a bare glyph looks the same directly on
//! the ledger bar's ink or inside an island's shared status pill; `view`
//! no longer takes `PanelStyle` at all.
//!
//! [`Media::view`] draws nothing when [`pick_active_player`] has no
//! candidate — the same "renders nothing" contract as `Battery`/`Network`
//! with no service present — and otherwise a single solid `Icon::Play`
//! glyph. CLAUDE.md's element-scale terracotta rule, in its bare-status
//! form, colors it `palette.accent` while the shown player's
//! `PlaybackStatus` is `Playing`, and the ordinary `on_ink.primary` ivory
//! while `Paused` — see [`Media::view`]'s own doc comment for why the raw
//! `accent` token is right here where `battery.rs`'s charging glyph needs
//! the higher-luminance `accent_light` instead. The glyph never swaps shape
//! (no `Icon::Pause` the way the old transport pill's icon did): it's a
//! readout now, not a control that tells you what pressing it will do, and
//! it carries no `on_press` of its own, so a click passes through to the
//! status cluster's shared trigger exactly like every glyph beside it.
//!
//! Title, artist, and the transport buttons all live in the quick-settings
//! media row (`popovers::quick_settings::media_row`) — unaffected by this
//! change, and still reading [`Media::now_playing`] directly. This retires
//! the bar's one subtle-fill pill: in ledger style the clock is now the
//! bar's only pill, full stop.
//!
//! # The command-out pattern (Stage 17, the project's first)
//!
//! Everything above is **command-in**: the worker reads MPRIS state and
//! pushes snapshots, and nothing here ever calls a method that changes
//! anything. The quick-settings transport row
//! (`popovers::quick_settings::media_row`) — the bar glyph itself has no
//! transport control of its own, see "Design language" above — is the
//! panel's first *command-out* D-Bus caller, and the shape is deliberately
//! not another long-lived worker: [`play_pause`]/[`next`]/[`previous`] each
//! open a **fresh, one-shot session-bus connection**, make one method call,
//! and let the connection drop. `Task::future(..).discard()` is what expresses "run
//! this to completion, I don't need the result" — `Panel::update` has
//! nothing useful to do with success/failure beyond the `eprintln!` already
//! inside [`send_player_command`].
//!
//! Reusing the worker's own long-lived proxy was considered and rejected:
//! that proxy lives inside `watch_mpris`'s `stream::unfold` state (a
//! `Stream`, not something `Panel::update` can reach into), and exposing it
//! would mean threading "give me the currently-active player's proxy" out
//! of a subscription that only knows how to run once — real complexity for
//! a control a human clicks occasionally, not once per frame. A fresh
//! connection per click costs one extra handshake, the same cost
//! `watch_mpris` itself already pays once at startup — negligible next to a
//! human reaction time.

use std::collections::HashMap;

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, BoxStream, StreamExt};
use iced::futures::{SinkExt, Stream};
use iced::widget::Space;
use iced::{Element, Subscription, Task};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;
use zbus::fdo::DBusProxy;
use zbus::zvariant::OwnedValue;
use zbus::Connection;

use crate::icons::{self, Icon};

/// The media module's own message type (Stage 7's per-module refactor —
/// see `modules::clock::Message` for the full teaching note). `main.rs`
/// nests this as `Message::Media(media::Message)`; `Panel::update`
/// delegates by matching through both layers:
/// `Message::Media(media::Message::Updated(m))`.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Media),
    /// A quick-settings media row transport button was clicked (the bar
    /// glyph itself has no transport control of its own — see the module
    /// doc comment's "Design language" section). Each carries the *target*
    /// player's bus name, read off `NowPlaying::bus_name` at the moment the
    /// button was drawn — see the module doc comment's "command-out
    /// pattern" section for why this resolves to a fresh one-shot
    /// connection rather than reaching into the worker's own proxy.
    PlayPause(String),
    Next(String),
    Previous(String),
}

/// D-Bus bus names of every MPRIS player start with this well-known
/// prefix (`org.mpris.MediaPlayer2.spotify`, `...vlc`, `...firefox.instance_1`,
/// ...) — see the MPRIS spec's "Bus Name Policy" section. Used both to
/// filter the initial `ListNames` sweep and to filter
/// `NameOwnerChanged` signals down to players only.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// A zbus proxy for one player's `org.mpris.MediaPlayer2.Player` interface.
///
/// Teaching note (no `default_service`): unlike UPower's fixed
/// `DisplayDevice` (`battery.rs`), *which* bus name this proxy talks to is
/// only known at runtime — it's whichever `org.mpris.MediaPlayer2.*` name
/// the worker is currently watching. Leaving `default_service` off the
/// macro attribute (while keeping `default_path`, which every MPRIS player
/// exposes at the same fixed path) is what makes the generated
/// `MediaPlayer2PlayerProxy::new` take the destination as a second
/// argument instead of assuming one — see `zbus_macros`' `(Some(path),
/// None(service))` codegen branch, the mirror image of `network.rs`'s
/// Station proxy (which has a default *service* but no default *path*).
#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MediaPlayer2Player {
    /// "Playing", "Paused", or "Stopped" (MPRIS spec, `Player` interface).
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;

    /// `a{sv}` — a dictionary of variant-typed metadata fields. The MPRIS
    /// metadata guidelines (spec, "Metadata guidelines") define
    /// `xesam:title` as a plain string and `xesam:artist` as a string
    /// *array* (an artist list, not one name) — see [`extract_title_artist`]
    /// for the zvariant gymnastics that pulls both back out.
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// MPRIS `Player.PlayPause` — toggles between playing and paused (a
    /// player that can't pause is free to always resume playing instead;
    /// that's the player's call, per the spec, not ours). Unlike
    /// `playback_status`/`metadata` above, this isn't `#[zbus(property)]` —
    /// it's a plain D-Bus *method* call, called only from [`send_player_command`]
    /// (Stage 17's command-out path), never from the long-lived worker.
    /// zbus's proxy macro derives the member name `PlayPause` from
    /// `play_pause` the same automatic way it derives `PlaybackStatus` from
    /// `playback_status` above — no explicit `#[zbus(name = "...")]` needed.
    async fn play_pause(&self) -> zbus::Result<()>;

    /// MPRIS `Player.Next` — skip to the next track, if the player has one.
    async fn next(&self) -> zbus::Result<()>;

    /// MPRIS `Player.Previous` — skip to the previous track (or restart the
    /// current one; MPRIS leaves that choice to the player).
    async fn previous(&self) -> zbus::Result<()>;
}

/// Media module state: the last "who should the pill show" decision the
/// D-Bus worker computed. Like `Battery`/`Network`, this module caches —
/// reading D-Bus during `view` would block the UI thread.
///
/// `Default` (`now_playing: None`) is the boot state *and* the "no
/// candidate player" state — both render nothing, same contract as
/// `Battery::default()`/`Network::default()`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Media {
    now_playing: Option<NowPlaying>,
}

/// The one player the status glyph and the quick-settings media row are
/// currently showing, already reduced from however many MPRIS players are
/// running to "here's the label and whether it's live" — `view` only reads
/// [`Self::playing`] and [`Self::bus_name`] of this; the popover row is what
/// uses the rest.
///
/// `pub(crate)`, not private: `popovers::quick_settings::media_row` (Stage
/// 17) reads this through [`Media::now_playing`] to lay out its own,
/// richer media row. The fields themselves stay private; only the
/// accessor methods below are `pub(crate)`, so the popover module reads
/// exactly what it needs and no more.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NowPlaying {
    title: String,
    artist: String,
    /// Terracotta (live) iff true — only `PlaybackStatus::Playing` sets
    /// this; `Paused` renders at rest, per the one rule.
    playing: bool,
    /// This player's own MPRIS bus name, carried through from
    /// [`PlayerSnapshot::bus_name`] — see that field's doc comment for why.
    bus_name: String,
}

impl NowPlaying {
    /// The bare track title — the quick-settings media row's own line;
    /// the bar's status glyph shows neither this nor [`Self::artist`] (see
    /// the module doc comment's "Design language" section).
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn artist(&self) -> &str {
        &self.artist
    }

    /// Terracotta-glyph-or-not, per the one rule — see the module doc
    /// comment's "one terracotta accent" note.
    pub(crate) fn playing(&self) -> bool {
        self.playing
    }

    /// Which MPRIS player a `PlayPause`/`Next`/`Previous` click should
    /// target.
    pub(crate) fn bus_name(&self) -> &str {
        &self.bus_name
    }
}

impl Media {
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Asks the same `Option` as `view`'s `let-else`, so the two
    /// cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.now_playing.is_some()
    }

    /// The player the status glyph and the quick-settings media row are
    /// currently showing, or `None` when nothing counts as a candidate (see
    /// [`pick_active_player`]). `pub(crate)` so
    /// `popovers::quick_settings::media_row` can read the same state this
    /// module's own `view` renders, rather than a second copy.
    pub(crate) fn now_playing(&self) -> Option<&NowPlaying> {
        self.now_playing.as_ref()
    }

    /// Renders the status glyph, or nothing at all when there's no
    /// candidate player (see [`pick_active_player`]) — the same
    /// "renders nothing" contract every other status module gives an
    /// absent service.
    ///
    /// One bare `Icon::Play` glyph, exactly like `battery`/`volume`'s bare
    /// status icons: no button, no pill, no `on_press`. The glyph never
    /// swaps to `Icon::Pause` the way the old transport pill's did — this
    /// is a readout, not a control that previews what a click will do, so
    /// only its *color* carries the state: `palette.accent` (this module's
    /// one live, element-scale terracotta accent, per CLAUDE.md) while the
    /// shown player's `PlaybackStatus` is `Playing`, the ordinary
    /// `on_ink.primary` ivory while `Paused`.
    ///
    /// Raw `accent`, not `accent_light`: `battery.rs`'s charging glyph
    /// needs the higher-luminance `accent_light` token because it's drawn
    /// with a *stroke* (`#C67139` text/strokes on ink fail contrast, per
    /// the style guide's accent ramp) — but `Icon::Play` is a solid
    /// *filled* shape, the same class of thing as a pill's fill, where the
    /// style guide's own §1 table already pairs raw `#C67139` with ivory
    /// content directly.
    ///
    /// No button wrapping the glyph is what makes it a readout rather than
    /// a control: a bare `Svg` widget doesn't capture the click, so it
    /// falls through to whatever wraps the status cluster
    /// (`main.rs::Panel::status_cluster_trigger`'s "child updates first"
    /// note), opening quick settings exactly like clicking any other bare
    /// status icon beside it.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        let Some(now_playing) = &self.now_playing else {
            return Space::new().into();
        };

        let color = glyph_color(theme, now_playing.playing).into_iced();

        icons::icon(Icon::Play, theme.sizes.icon_bar, color).into()
    }

    /// The MPRIS D-Bus feed as an iced subscription. See `battery.rs`'s
    /// `subscription` for the function-pointer-identity teaching note —
    /// identical reasoning applies verbatim.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(media_stream)
    }
}

/// Which color the status glyph takes — this module's one live terracotta
/// accent while the shown player is playing, the ordinary ivory role while
/// paused. See [`Media::view`]'s doc comment for why the raw `accent` token
/// (not `accent_light`) is the right one for a *filled* glyph.
///
/// Pure function of its arguments, unit-tested below — `view` only wires
/// this into the rendered `Svg`, the same "logic here, wiring there" split
/// `battery_icon`/`wifi_icon` use for their own leveled glyphs (this one
/// just has two states to pick between, not a ladder).
fn glyph_color(theme: &Theme, playing: bool) -> saola_theme::tokens::Color {
    if playing {
        theme.palette.accent
    } else {
        theme.on_ink.primary
    }
}

/// MPRIS `PlaybackStatus` (spec: "May be 'Playing', 'Paused' or
/// 'Stopped'"), reduced to the three states the active-player heuristic
/// cares about. Any unrecognized string (a future spec value, a
/// nonconformant player) maps to `Stopped` defensively — treated as "not a
/// candidate" rather than accidentally winning the "most recently
/// appeared" tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    fn from_mpris(status: &str) -> Self {
        match status {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// One tracked player's last-read state — the worker's internal bookkeeping
/// unit (not what `view` renders; see [`NowPlaying`] for that).
///
/// `appeared_at` is a monotonically increasing sequence number assigned
/// once, when the worker first starts watching this bus name (at initial
/// sweep or first `NameOwnerChanged` appearance) — *not* updated on every
/// property change. It's what "most-recently-appeared" means in the
/// active-player heuristic below: order of first appearance, not order of
/// last update.
#[derive(Debug, Clone, PartialEq)]
struct PlayerSnapshot {
    status: PlaybackStatus,
    title: String,
    artist: String,
    /// This player's own MPRIS bus name (`org.mpris.MediaPlayer2.<name>`) —
    /// threaded through to [`NowPlaying`] (Stage 17) so a transport click
    /// knows *which* player to send `PlayPause`/`Next`/`Previous` to
    /// without the worker re-resolving "who's active" a second time.
    bus_name: String,
    appeared_at: u64,
}

/// The active-player heuristic (PLAN.md Stage 9's testable core): among
/// every currently tracked player, `Playing` beats `Paused` beats
/// most-recently-appeared. A player reporting `Stopped` ("no track
/// currently playing", per the MPRIS spec) is never a candidate at all —
/// filtered out before the comparison, so a room full of stopped players
/// (or none tracked yet) correctly yields `None`, the same "render
/// nothing" state as no players existing.
///
/// Pure function of its argument (no D-Bus, no globals) — the whole reason
/// this is split out from the worker, which is not unit-tested.
fn pick_active_player(players: &[PlayerSnapshot]) -> Option<&PlayerSnapshot> {
    players
        .iter()
        .filter(|p| p.status != PlaybackStatus::Stopped)
        .max_by_key(|p| (p.status == PlaybackStatus::Playing, p.appeared_at))
}

/// Builds the async stream the subscription runs. See `battery.rs`'s
/// `battery_stream` for the full bridge teaching note (the channel, the
/// runtime it runs on). Every failure path — no session bus, a `Metadata`
/// read failing, the connection itself dying — funnels into "send the
/// no-players state, worker ends quietly": the panel never goes down
/// because MPRIS isn't there or a player misbehaves.
fn media_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        if watch_mpris(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(Media::default())).await;
        }
    })
}

/// One event out of the merged stream `watch_mpris` polls: either a
/// bus-name ownership change (a player appearing/disappearing) or a fresh
/// read from one already-tracked player.
enum WorkerEvent {
    /// A `NameOwnerChanged` signal already filtered to the MPRIS prefix.
    /// `has_owner` is `false` when the name's owner was cleared (the
    /// player quit or crashed) — the D-Bus-level fact, independent of
    /// whether that player's own property stream ever notices.
    Owner { name: String, has_owner: bool },
    /// A snapshot read from `bus_name`'s player proxy — `None` means the
    /// read failed (the player vanished mid-read) or its property stream
    /// ended, both of which mean "stop treating this name as a tracked
    /// player."
    Player {
        bus_name: String,
        data: Option<PlayerSnapshot>,
    },
}

/// The worker proper: discover players, then merge every one's
/// property-change feed with the name-owner-changed feed into one
/// `SelectAll`, and react to whichever event arrives next, forever.
async fn watch_mpris(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // Session bus: MPRIS players are per-user desktop applications, not
    // system services — see the module doc comment's teaching note.
    let connection = Connection::session().await?;
    let dbus = DBusProxy::new(&connection).await?;

    // Teaching note (`SelectAll`, not a fixed `stream::select` pair):
    // battery/network each merge a small, *fixed* number of property
    // streams with `futures::stream::select`. Media's player set is
    // unbounded and changes at runtime, so it needs a collection that
    // supports pushing new streams in after construction —
    // `futures::stream::SelectAll` (re-exported through `iced::futures`,
    // since `iced_futures` re-exports the whole `futures` crate) is that:
    // every member stream is boxed to a uniform `BoxStream<'static,
    // WorkerEvent>` so heterogeneous sources (the one name-owner stream,
    // and one stream per tracked player) can live in the same set.
    let mut events: stream::SelectAll<BoxStream<'static, WorkerEvent>> = stream::SelectAll::new();

    // Permanent member: filtered straight down to "does this MPRIS name
    // have an owner right now" — that's the one fact this worker needs
    // from the raw signal.
    let owner_changed = dbus.receive_name_owner_changed().await?;
    events.push(
        owner_changed
            .filter_map(|signal| async move {
                let args = signal.args().ok()?;
                let name = args.name().to_string();
                if !name.starts_with(MPRIS_PREFIX) {
                    return None;
                }
                Some(WorkerEvent::Owner {
                    name,
                    has_owner: args.new_owner().is_some(),
                })
            })
            .boxed(),
    );

    let mut players: HashMap<String, PlayerSnapshot> = HashMap::new();
    // Monotonic "appearance order" counter — see `PlayerSnapshot::appeared_at`.
    let mut next_seq: u64 = 0;

    // Initial sweep: `NameOwnerChanged` only fires for *future* ownership
    // changes, so a player already running before the panel started would
    // otherwise be invisible until it restarts. `ListNames` catches it.
    for name in dbus.list_names().await? {
        let name = name.to_string();
        if name.starts_with(MPRIS_PREFIX) {
            events.push(player_stream(connection.clone(), name, next_seq));
            next_seq += 1;
        }
    }

    loop {
        let event = match events.next().await {
            Some(event) => event,
            // `owner_changed` is a permanent member that never itself
            // ends (short of the connection dying), so in practice this
            // only fires if the whole set is empty *and* the owner stream
            // already ended — treated the same as any other worker exit.
            None => return Ok(()),
        };

        match event {
            WorkerEvent::Owner { name, has_owner } => {
                if has_owner {
                    // A new player claimed this name. (MPRIS bus names
                    // essentially never change unique-owner without a
                    // disappearance in between, so "already tracked" here
                    // mainly guards against a duplicate initial-sweep +
                    // signal race, not a real reassignment.)
                    if !players.contains_key(&name) {
                        events.push(player_stream(connection.clone(), name, next_seq));
                        next_seq += 1;
                    }
                } else {
                    players.remove(&name);
                    if !send_active(&players, sender).await {
                        return Ok(());
                    }
                }
            }
            WorkerEvent::Player { bus_name, data } => {
                match data {
                    Some(snapshot) => {
                        players.insert(bus_name, snapshot);
                    }
                    None => {
                        players.remove(&bus_name);
                    }
                }
                if !send_active(&players, sender).await {
                    return Ok(());
                }
            }
        }
    }
}

/// Recomputes the active player from the tracked set and sends it.
/// Returns `false` when the receiving side is gone (the subscription was
/// dropped), the same "stop quietly" signal `battery.rs`/`network.rs`
/// check inline after every `.send(...)`.
async fn send_active(
    players: &HashMap<String, PlayerSnapshot>,
    sender: &mut mpsc::Sender<Message>,
) -> bool {
    // `pick_active_player` takes a slice; the tracked set rarely holds
    // more than one or two players, so collecting it fresh on every event
    // is cheap — no reason to keep a parallel `Vec` in sync by hand.
    let candidates: Vec<PlayerSnapshot> = players.values().cloned().collect();
    let now_playing = pick_active_player(&candidates).map(|p| NowPlaying {
        title: p.title.clone(),
        artist: p.artist.clone(),
        playing: p.status == PlaybackStatus::Playing,
        bus_name: p.bus_name.clone(),
    });
    sender
        .send(Message::Updated(Media { now_playing }))
        .await
        .is_ok()
}

/// One tracked player's event stream: an initial read, then one fresh read
/// per `PlaybackStatus`/`Metadata` change, forever — structurally the same
/// "read+send first, then park on the change stream" loop as
/// `battery.rs`'s `watch_upower`, just expressed with `stream::unfold`
/// instead of a bare `loop` so it can be one member of the outer
/// `SelectAll` instead of its own top-level task.
///
/// Gotcha (documented, not fixed here): if this player's name loses its
/// owner *without* a final property-changed signal (a hard crash, say),
/// this stream has no way to notice on its own — it just sits forever
/// waiting on a change that will never come. Correctness doesn't depend on
/// that: `watch_mpris`'s `Owner` branch removes the name from `players`
/// (and sends the updated active-player state) the instant
/// `NameOwnerChanged` reports the name ownerless, regardless of whether
/// this stream ever ends. The cost is one permanently-idle stream sitting
/// in `SelectAll` per player that ever appeared during the bar's lifetime
/// — bounded by how many distinct MPRIS bus names a session ever sees, not
/// something that grows unbounded in practice. Tearing it down cleanly
/// would need a cancellation signal raced against the change stream (e.g.
/// a `oneshot` per player); flagged here rather than built, since it adds
/// real complexity for a leak this small.
fn player_stream(
    connection: Connection,
    bus_name: String,
    appeared_at: u64,
) -> BoxStream<'static, WorkerEvent> {
    stream::unfold(
        PlayerWatch::Init {
            connection,
            bus_name,
            appeared_at,
        },
        player_watch_step,
    )
    .boxed()
}

/// [`player_stream`]'s state machine: `Init` builds the proxy and its
/// merged change-stream on first poll; `Watching` holds both across
/// iterations; `Done` ends the stream (an `unfold` step returning `None`).
enum PlayerWatch {
    Init {
        connection: Connection,
        bus_name: String,
        appeared_at: u64,
    },
    Watching {
        proxy: MediaPlayer2PlayerProxy<'static>,
        changed: BoxStream<'static, ()>,
        bus_name: String,
        appeared_at: u64,
    },
    Done,
}

async fn player_watch_step(state: PlayerWatch) -> Option<(WorkerEvent, PlayerWatch)> {
    match state {
        PlayerWatch::Init {
            connection,
            bus_name,
            appeared_at,
        } => match MediaPlayer2PlayerProxy::new(&connection, bus_name.clone()).await {
            Ok(proxy) => {
                let changed = merged_changed_stream(&proxy).await;
                let data = read_player_snapshot(&proxy, &bus_name, appeared_at)
                    .await
                    .ok();
                Some((
                    WorkerEvent::Player {
                        bus_name: bus_name.clone(),
                        data,
                    },
                    PlayerWatch::Watching {
                        proxy,
                        changed,
                        bus_name,
                        appeared_at,
                    },
                ))
            }
            // The name disappeared between `ListNames`/`NameOwnerChanged`
            // reporting it and this proxy actually being built — treat
            // exactly like any other "player's gone" signal.
            Err(_) => Some((
                WorkerEvent::Player {
                    bus_name,
                    data: None,
                },
                PlayerWatch::Done,
            )),
        },
        PlayerWatch::Watching {
            proxy,
            mut changed,
            bus_name,
            appeared_at,
        } => {
            if changed.next().await.is_none() {
                return Some((
                    WorkerEvent::Player {
                        bus_name,
                        data: None,
                    },
                    PlayerWatch::Done,
                ));
            }
            let data = read_player_snapshot(&proxy, &bus_name, appeared_at)
                .await
                .ok();
            Some((
                WorkerEvent::Player {
                    bus_name: bus_name.clone(),
                    data,
                },
                PlayerWatch::Watching {
                    proxy,
                    changed,
                    bus_name,
                    appeared_at,
                },
            ))
        }
        PlayerWatch::Done => None,
    }
}

/// One merged "something changed" stream for a single player's
/// `PlaybackStatus`/`Metadata` properties — same normalize-to-`()`-and-
/// `stream::select` shape as `battery.rs`/`network.rs`, boxed so
/// `PlayerWatch::Watching` doesn't need to name the concrete combinator
/// type.
async fn merged_changed_stream(proxy: &MediaPlayer2PlayerProxy<'static>) -> BoxStream<'static, ()> {
    let status = proxy.receive_playback_status_changed().await;
    let metadata = proxy.receive_metadata_changed().await;
    stream::select(status.map(|_| ()), metadata.map(|_| ())).boxed()
}

/// One fresh read of a player's `PlaybackStatus` + `Metadata`.
async fn read_player_snapshot(
    proxy: &MediaPlayer2PlayerProxy<'static>,
    bus_name: &str,
    appeared_at: u64,
) -> zbus::Result<PlayerSnapshot> {
    let status = proxy.playback_status().await?;
    let metadata = proxy.metadata().await?;
    let (title, artist) = extract_title_artist(metadata);
    Ok(PlayerSnapshot {
        status: PlaybackStatus::from_mpris(&status),
        title,
        artist,
        bus_name: bus_name.to_string(),
        appeared_at,
    })
}

/// Pulls `xesam:title` (a plain string) and `xesam:artist` (a string
/// *array* — an artist list, per the MPRIS metadata guidelines) out of a
/// freshly-read `Metadata` map.
///
/// Teaching note (zvariant extraction gotchas): `Metadata`'s D-Bus type is
/// `a{sv}` — a dict of `String` keys to *variant*-typed values — so each
/// value in the map is an `OwnedValue`, not a plain `String`/`Vec<String>`
/// directly; getting the concrete type back out means `TryFrom`, and it
/// must be picked with the right ownership:
/// - `String: TryFrom<OwnedValue>` exists (a by-*value* conversion,
///   consuming the `OwnedValue`) — fine for `xesam:title` since we own the
///   whole `metadata` map here and can `.remove(...)` a key out of it.
/// - `Vec<T>: TryFrom<OwnedValue>` also exists, but *only* by value — there
///   is no `TryFrom<&OwnedValue>` impl for `Vec<T>` in zvariant 5 (only
///   scalar/string/path types get the by-reference conversion). Trying to
///   `downcast_ref::<Vec<String>>()` on a borrowed value, which reads
///   naturally at a `.get(key)` call site, does not compile — this is the
///   gotcha that actually matters here. `.remove(key)` instead of
///   `.get(key)` sidesteps it entirely by handing over ownership, which is
///   free anyway since `metadata` isn't read again after this call.
fn extract_title_artist(mut metadata: HashMap<String, OwnedValue>) -> (String, String) {
    let title = metadata
        .remove("xesam:title")
        .and_then(|value| String::try_from(value).ok())
        .unwrap_or_default();

    let artist = metadata
        .remove("xesam:artist")
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .filter(|artists| !artists.is_empty())
        // MPRIS allows more than one artist; join them rather than only
        // ever showing the first and silently dropping the rest.
        .map(|artists| artists.join(", "))
        .unwrap_or_default();

    (title, artist)
}

/// One MPRIS transport call. See the module doc comment's "command-out
/// pattern" section for why this connects fresh rather than reusing the
/// worker's proxy.
#[derive(Debug, Clone, Copy)]
enum PlayerCommand {
    PlayPause,
    Next,
    Previous,
}

/// Connects to the session bus, calls one MPRIS transport method on
/// `bus_name`'s player, and lets the connection drop. Every failure —
/// no session bus, the player having vanished between the button being
/// drawn and the click landing, a player that doesn't implement a given
/// control — is logged and swallowed: there is no message the panel does
/// anything useful with beyond that, the same "best-effort, quietly do
/// nothing further" contract as every other D-Bus call in this file.
async fn send_player_command(bus_name: String, command: PlayerCommand) {
    let Ok(connection) = Connection::session().await else {
        return;
    };
    let Ok(proxy) = MediaPlayer2PlayerProxy::new(&connection, bus_name).await else {
        return;
    };
    let result = match command {
        PlayerCommand::PlayPause => proxy.play_pause().await,
        PlayerCommand::Next => proxy.next().await,
        PlayerCommand::Previous => proxy.previous().await,
    };
    if let Err(error) = result {
        eprintln!("saola-panel: media control failed: {error}");
    }
}

/// Toggles play/pause on `bus_name`'s player — the bar pill's own click and
/// the quick-settings row's centre transport button both resolve here via
/// [`Message::PlayPause`]. `Task::future(..).discard()` runs the call to
/// completion and throws away its `()` result; nothing in `Panel::update`
/// needs it (see the module doc comment).
pub fn play_pause(bus_name: String) -> Task<Message> {
    Task::future(send_player_command(bus_name, PlayerCommand::PlayPause)).discard()
}

/// Skips to the next track on `bus_name`'s player.
pub fn next(bus_name: String) -> Task<Message> {
    Task::future(send_player_command(bus_name, PlayerCommand::Next)).discard()
}

/// Skips to the previous track on `bus_name`'s player.
pub fn previous(bus_name: String) -> Task<Message> {
    Task::future(send_player_command(bus_name, PlayerCommand::Previous)).discard()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(status: PlaybackStatus, appeared_at: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            status,
            title: "Title".to_string(),
            artist: "Artist".to_string(),
            bus_name: "org.mpris.MediaPlayer2.test".to_string(),
            appeared_at,
        }
    }

    #[test]
    fn mpris_status_strings_map_to_the_three_states() {
        assert_eq!(
            PlaybackStatus::from_mpris("Playing"),
            PlaybackStatus::Playing
        );
        assert_eq!(PlaybackStatus::from_mpris("Paused"), PlaybackStatus::Paused);
        assert_eq!(
            PlaybackStatus::from_mpris("Stopped"),
            PlaybackStatus::Stopped
        );
    }

    #[test]
    fn unrecognized_status_strings_are_treated_as_stopped() {
        // Defensive: a future MPRIS revision (or a nonconformant player)
        // adding/misreporting a status value should never accidentally
        // win the active-player heuristic.
        assert_eq!(PlaybackStatus::from_mpris(""), PlaybackStatus::Stopped);
        assert_eq!(
            PlaybackStatus::from_mpris("playing"),
            PlaybackStatus::Stopped
        );
    }

    #[test]
    fn playing_beats_paused_regardless_of_appearance_order() {
        let players = [
            snapshot(PlaybackStatus::Paused, 5),  // appeared later
            snapshot(PlaybackStatus::Playing, 0), // appeared first, but playing
        ];
        let active = pick_active_player(&players).unwrap();
        assert_eq!(active.status, PlaybackStatus::Playing);
        assert_eq!(active.appeared_at, 0);
    }

    #[test]
    fn among_equal_status_the_most_recently_appeared_wins() {
        let players = [
            snapshot(PlaybackStatus::Paused, 0),
            snapshot(PlaybackStatus::Paused, 7),
            snapshot(PlaybackStatus::Paused, 3),
        ];
        let active = pick_active_player(&players).unwrap();
        assert_eq!(active.appeared_at, 7);
    }

    #[test]
    fn stopped_players_are_never_candidates() {
        let players = [snapshot(PlaybackStatus::Stopped, 9)];
        assert!(pick_active_player(&players).is_none());

        // A mix: the stopped player is skipped even though it "appeared"
        // most recently.
        let players = [
            snapshot(PlaybackStatus::Paused, 0),
            snapshot(PlaybackStatus::Stopped, 99),
        ];
        let active = pick_active_player(&players).unwrap();
        assert_eq!(active.status, PlaybackStatus::Paused);
    }

    #[test]
    fn no_tracked_players_yields_no_active_player() {
        assert!(pick_active_player(&[]).is_none());
    }

    /// Stage 17: a transport click needs to know *which* player to target,
    /// so `bus_name` has to survive the trip from a raw `PlayerSnapshot`
    /// through `pick_active_player` and into the `NowPlaying` `send_active`
    /// builds — this pins the whole chain, not just the heuristic.
    #[test]
    fn active_player_carries_its_own_bus_name() {
        let players = [PlayerSnapshot {
            status: PlaybackStatus::Playing,
            title: "Title".to_string(),
            artist: "Artist".to_string(),
            bus_name: "org.mpris.MediaPlayer2.vlc".to_string(),
            appeared_at: 0,
        }];
        let active = pick_active_player(&players).unwrap();
        assert_eq!(active.bus_name, "org.mpris.MediaPlayer2.vlc");
    }

    #[test]
    fn now_playing_accessors_read_through_to_the_stored_fields() {
        let now_playing = NowPlaying {
            title: "Title".to_string(),
            artist: "Artist".to_string(),
            playing: true,
            bus_name: "org.mpris.MediaPlayer2.vlc".to_string(),
        };
        assert_eq!(now_playing.title(), "Title");
        assert_eq!(now_playing.artist(), "Artist");
        assert!(now_playing.playing());
        assert_eq!(now_playing.bus_name(), "org.mpris.MediaPlayer2.vlc");
    }

    /// The status glyph's one live accent, per CLAUDE.md's element-scale
    /// terracotta rule: playing is terracotta, paused is the ordinary ivory
    /// role — see `glyph_color`'s doc comment for why it's the raw `accent`
    /// token rather than `accent_light`.
    #[test]
    fn glyph_is_terracotta_while_playing() {
        let theme = Theme::saola();
        assert_eq!(glyph_color(&theme, true), theme.palette.accent);
    }

    #[test]
    fn glyph_is_ivory_while_paused() {
        let theme = Theme::saola();
        assert_eq!(glyph_color(&theme, false), theme.on_ink.primary);
    }

    /// `view` renders nothing (a zero-sized `Space`) when there's no
    /// candidate player — same "renders nothing" contract as
    /// `Battery`/`Network` with no service present. `Element` can't be
    /// introspected, so this only pins "doesn't panic"; the absent-glyph
    /// *decision* itself is `Media::is_present`/`Option`-based and already
    /// covered by `pick_active_player`'s own tests above.
    #[test]
    fn view_renders_nothing_without_panicking_when_no_player_is_present() {
        let theme = Theme::saola();
        let media = Media::default();
        assert!(!media.is_present());
        let _: Element<'_, Message> = media.view(&theme);
    }

    /// `view` also renders without panicking once a player is present,
    /// exercising the `Some` arm of `view`'s `let-else` (the `None` arm is
    /// the test just above).
    #[test]
    fn view_renders_without_panicking_when_a_player_is_present() {
        let theme = Theme::saola();
        let media = Media {
            now_playing: Some(NowPlaying {
                title: "Title".to_string(),
                artist: "Artist".to_string(),
                playing: true,
                bus_name: "org.mpris.MediaPlayer2.vlc".to_string(),
            }),
        };
        assert!(media.is_present());
        let _: Element<'_, Message> = media.view(&theme);
    }
}
