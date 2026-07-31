//! The bar's now-playing pill, fed by MPRIS over D-Bus.
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
//! Per CLAUDE.md's one rule ("at most one terracotta element per surface"),
//! the pill itself is always the quiet [`style::button::muted`] fill — never
//! a solid terracotta flood, even while playing. The *play glyph* is this
//! pill's one terracotta accent: solid `palette.accent` while the active
//! player's `PlaybackStatus` is `Playing`, `on_ink.secondary` (a quiet status
//! light, not a second color) while `Paused`. No players (or none whose
//! status counts as a candidate — see [`pick_active_player`]) renders
//! nothing, same contract as `Battery`/`Network` with no service present.
//!
//! # The command-out pattern (Stage 17, the project's first)
//!
//! Everything above is **command-in**: the worker reads MPRIS state and
//! pushes snapshots, and nothing here ever calls a method that changes
//! anything. The bar pill's own click and the quick-settings transport row
//! (`popovers::quick_settings::media_row`) are the panel's first
//! *command-out* D-Bus calls, and the shape is deliberately not another
//! long-lived worker: [`play_pause`]/[`next`]/[`previous`] each open a
//! **fresh, one-shot session-bus connection**, make one method call, and let
//! the connection drop. `Task::future(..).discard()` is what expresses "run
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
use iced::widget::text::Wrapping;
use iced::widget::{button, container, row, text, Space};
use iced::{Element, Fill, Subscription, Task};
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};
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
    /// A transport button (bar pill or quick-settings media row) was
    /// clicked. Each carries the *target* player's bus name, read off
    /// `NowPlaying::bus_name` at the moment the button was drawn — see the
    /// module doc comment's "command-out pattern" section for why this
    /// resolves to a fresh one-shot connection rather than reaching into
    /// the worker's own proxy.
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

/// The one player the pill is currently showing, already reduced from
/// however many MPRIS players are running to "here's the label and
/// whether it's live" — `view` never sees the raw player set, only this.
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
    fn label(&self) -> String {
        media_label(&self.title, &self.artist)
    }

    /// The bare track title — `Self::label` already joins this with the
    /// artist for the bar's one-line pill; the quick-settings media row has
    /// room for both on their own line, so it reads them separately.
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
    /// Renders the media pill, or nothing when there's no candidate
    /// player (see [`pick_active_player`]).
    ///
    /// A `button` with a `row![icon, text]` as its content, since this pill
    /// shows a play glyph *and* a label. There's only one pill style here,
    /// `muted` — see the module doc comment for why a solid-fill state was
    /// removed. As of Stage 17 the button has a real `on_press`
    /// ([`Message::PlayPause`]) — before that, with no press handler at
    /// all, iced always reported `Status::Disabled`, so the style closure
    /// had to pin the status to `Active` to keep the pill from looking
    /// grayed out (`battery.rs`'s pill still does, having no click of its
    /// own); that workaround is gone here now that a real status exists to
    /// report.
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Asks the same `Option` as `view`'s `let-else`, so the two
    /// cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.now_playing.is_some()
    }

    /// The player the pill/quick-settings media row is currently showing,
    /// or `None` when nothing counts as a candidate (see
    /// [`pick_active_player`]). `pub(crate)` so
    /// `popovers::quick_settings::media_row` can read the same state this
    /// module's own `view` renders, rather than a second copy.
    pub(crate) fn now_playing(&self) -> Option<&NowPlaying> {
        self.now_playing.as_ref()
    }

    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        let Some(now_playing) = &self.now_playing else {
            return Space::new().into();
        };

        let playing = now_playing.playing;
        let pill_style = style::button::muted(theme, Surface::Ink);

        // The glyph is this pill's one terracotta accent: solid
        // `palette.accent` while `Playing` (the pill's one live element),
        // `on_ink.secondary` while `Paused` (a quiet status light, same
        // treatment as any other at-rest label on ink) — unchanged since
        // before Stage 17. What *did* change: the shape now varies with
        // `playing` too (`Pause` while playing, `Play` while paused), the
        // usual "icon shows what a click will do" convention, now that a
        // click actually does something (see `.on_press` below).
        let icon_color = if playing {
            theme.palette.accent
        } else {
            theme.on_ink.secondary
        }
        .into_iced();
        let icon_kind = if playing { Icon::Pause } else { Icon::Play };

        // The label sets its own color explicitly (`.color(...)` below)
        // rather than taking `muted`'s default: that default is
        // `on_ink.secondary` (a deliberately quiet role, right for its usual
        // off/idle uses), but the concept for *this* pill wants a
        // full-ivory label even though the pill fill itself is quiet — same
        // reasoning as the icon above (a separate `Svg` widget that never
        // inherits a button's `text_color`).
        //
        // The title is also the one place on the bar that uses
        // `ui_font_regular` (weight 400) instead of the bar's usual default
        // (`ui_font`, weight 500, applied automatically by the theme's text
        // styles elsewhere) — the concept mockup specifically draws the
        // media title lighter than every other bar label.
        let title = text(now_playing.label())
            .size(theme.typography.size.bar)
            .font(saola_theme::convert::ui_font_regular(theme))
            // A long track title must not stretch this pill across the bar,
            // so it's capped to `media_title_max_width` (the *text's* cap —
            // `pill_max_width` is the separate, larger cap on the whole
            // pill) and forced onto one line: `Wrapping::None` stops iced
            // from word-wrapping into a second line once the container
            // below constrains its width. iced 0.14 has no text-ellipsis
            // option (`iced::widget::text::Wrapping` is only
            // `None`/`Word`/`Glyph`/`WordOrGlyph` — no "…"-truncate
            // variant), so clipping the overflow via the container's
            // `.clip(true)` is the honest approximation until iced grows a
            // real ellipsis.
            .wrapping(Wrapping::None)
            .color(theme.on_ink.primary.into_iced());
        let title = container(title)
            .max_width(theme.sizes.media_title_max_width)
            .clip(true)
            .height(Fill)
            .align_y(iced::Center);

        button(
            row![
                icons::icon(icon_kind, theme.sizes.icon_bar, icon_color),
                title,
            ]
            // Icon-to-label gap inside the pill: `pill_gap`, not
            // `island_gap` (the gap *between* adjacent bar pills — a
            // previous pass reused it here for lack of a dedicated token,
            // before `pill_gap` landed in saola-theme).
            .spacing(theme.sizes.pill_gap)
            .align_y(iced::Center),
        )
        // `panel_pill_media` (30) is the compact in-bar pill height the
        // concept sketch draws for this pill — smaller than the
        // general-purpose `panel_pill` (40), which is the free-standing
        // islands-mode/popover scale. Same halved-height horizontal padding
        // convention as every other bar pill (half the height becomes the
        // end-cap radius).
        .height(theme.sizes.panel_pill_media)
        .padding([0.0, theme.sizes.panel_pill_media / 2.0])
        .style(pill_style)
        // Real as of Stage 17: a click toggles playback on the active
        // player. Before this the button had no `on_press` at all, so iced
        // always reported `Status::Disabled`, and the style closure above
        // used to pin the status to `Active` to work around that (see
        // `battery.rs`'s pill for the same trick, still needed there). With
        // a real `on_press`, `pill_style` now sees every status honestly —
        // `muted`'s Hovered/Pressed steps show for the first time.
        .on_press(Message::PlayPause(now_playing.bus_name.clone()))
        .into()
    }

    /// The MPRIS D-Bus feed as an iced subscription. See `battery.rs`'s
    /// `subscription` for the function-pointer-identity teaching note —
    /// identical reasoning applies verbatim.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(media_stream)
    }
}

/// "title — artist" pill label. Falls back to whichever side is non-empty
/// when a player only ever sets one `xesam:*` field, rather than showing a
/// bare "— " separator; empty when neither is known yet.
///
/// Pure function, unit-tested below.
fn media_label(title: &str, artist: &str) -> String {
    match (title.is_empty(), artist.is_empty()) {
        (false, false) => format!("{title} — {artist}"),
        (false, true) => title.to_string(),
        (true, false) => artist.to_string(),
        (true, true) => String::new(),
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

    #[test]
    fn label_joins_title_and_artist_with_an_em_dash() {
        assert_eq!(media_label("Song", "Artist"), "Song — Artist");
    }

    #[test]
    fn label_falls_back_to_whichever_side_is_known() {
        assert_eq!(media_label("Song", ""), "Song");
        assert_eq!(media_label("", "Artist"), "Artist");
        assert_eq!(media_label("", ""), "");
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
}
