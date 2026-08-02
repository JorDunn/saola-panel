//! The Claude Code session semaphore: one status dot per live session, fed
//! by hooks broadcasting D-Bus signals.
//!
//! Every other zbus module so far (`battery`, `network`, `media`) talks to a
//! *service*: something sitting on the bus, advertising properties, that we
//! ask questions of via a generated proxy. Claude Code has no such thing —
//! there is no `io.saola.ClaudeCode1` process to connect to, no object to
//! proxy, no property to read. `contrib/claude-code/emit.sh` (run from a
//! Claude Code hook, see that file) fires a one-shot `busctl --user emit`
//! and exits; nothing stays resident on the bus, on either end.
//!
//! Teaching note (signals without a service — the proxy macro doesn't fit):
//! `#[zbus::proxy]` generates a struct built around *destination + path*,
//! because it exists to make method calls and read properties — both of
//! which require a specific object to talk to. A broadcast signal from a
//! process that appears for one D-Bus call and disappears again has no
//! stable destination to proxy (`busctl emit`'s sender is a transient unique
//! name, different every time, and this module deliberately never filters
//! on it — see [`CLAUDE_CODE_INTERFACE`]'s doc comment). What we actually
//! want is "hand me every message matching this shape, whoever sent it,"
//! which is exactly [`zbus::MatchRule`] plus
//! [`zbus::MessageStream::for_match_rule`]: a `MatchRule` is registered with
//! the bus and the returned stream yields every message matching it,
//! filtered bus-side. There is no "connect" step that can fail because the
//! service is missing — a `MatchRule` on the session bus is always valid to
//! register, whether or not anything will ever send a matching signal. That
//! is *why* absence is silent here in a way it isn't for `battery`/
//! `network`: those modules render nothing because a proxy call failed;
//! this one renders nothing because the stream has simply never yielded a
//! message. Same visible outcome, different reason underneath.
//!
//! # The bus schema (this module's half of the contract)
//!
//! - Session bus (`Connection::session()`, like `media.rs` — a per-user
//!   hook process, not a system service).
//! - Object path `/io/saola/ClaudeCode` ([`CLAUDE_CODE_PATH`]).
//! - Interface `io.saola.ClaudeCode1` ([`CLAUDE_CODE_INTERFACE`]).
//! - Signal `StatusChanged(session_id: s, status: s, transcript_path: s)`
//!   ([`STATUS_MEMBER`]), `status` one of `"working"`, `"subagent"`,
//!   `"attention"`, `"done"`, `"idle"`, `"ended"` (see [`fold`]).
//!   `transcript_path` (2026-08-01, for the usage popover) is the session's
//!   JSONL transcript file, or `""` when the hook had none to report; the
//!   worker also still accepts the original two-argument `ss` body, so an
//!   un-updated `emit.sh` keeps its dots and merely lacks usage data. The
//!   transcript is *stored*, never watched — it is read exactly once per
//!   usage-popover open (`popovers::claude_usage`), which is a click, not a
//!   poll.
//! - Signal `UsageChanged(five_hour_pct: d, five_hour_resets_at: t,
//!   seven_day_pct: d, seven_day_resets_at: t)` ([`USAGE_MEMBER`]; added
//!   2026-08-01) — the account's rate-limit gauges, emitted by
//!   `contrib/claude-code/statusline.sh` (Claude Code's `statusLine`
//!   command, the only place Claude Code exposes these numbers — hooks
//!   never receive them). Deliberately **no `session_id`**: rate limits
//!   are account-wide, not per-session, so this signal does not
//!   participate in the per-session fold at all. The module keeps exactly
//!   one [`Usage`] snapshot and the newest signal wins — the emitter is
//!   stateless and re-broadcasts on every statusline refresh, and an
//!   idempotent "overwrite with the latest" fold is what makes that
//!   harmless. Shown only in the usage popover ([`popovers::
//!   claude_usage`]); the bar's dot row never renders it.
//!
//! # The fold (state, not a snapshot — same shape as `columns.rs`)
//!
//! A `StatusChanged` signal is a delta ("this one session's status just
//! became X"), not the whole picture, so the worker keeps a [`Sessions`]
//! list and [`fold`]s each event into it, then ships the *whole* list on
//! every event. `columns.rs`'s module doc comment covers the general shape
//! of this pattern in more depth; the one new wrinkle here is that
//! `"ended"` doesn't update an entry, it deletes one — a session that ended
//! has nothing left to report, which is the same "not tracked" state as a
//! session this module has never heard from.
//!
//! Teaching note (why a `Vec`, not a `HashMap`): the dots are *positional* —
//! Jordan reads "the third dot went red" — so the row has to be stable
//! across status changes. A `HashMap` has no order at all (its iteration
//! order is deliberately randomized per process), so a status change would
//! be free to shuffle the whole row. [`Sessions`] is therefore a plain
//! `Vec` in **first-seen order**: a status change rewrites an entry in
//! place and never moves it, and only `"ended"` ever removes one. Linear
//! scan by id is the lookup, which is the right call at this size — Jordan
//! runs a handful of sessions, not thousands, and a `Vec` scan of ten
//! entries beats a hash of one string.
//!
//! # Render: one dot per session, five colors, two of them breathing
//!
//! Each tracked session draws one `sizes.dash_height` circle through
//! [`style::container::status_dot`] — the same geometry vocabulary as
//! `columns.rs`'s dashes, deliberately, so the two readouts look like
//! relatives rather than two unrelated inventions. The color comes from the
//! theme's [`SessionStatus`] (amber = working, violet = subagents, red =
//! attention, blue = done, green = idle).
//!
//! **This is the panel's documented exception to "three colors, never a
//! fourth"** (Jordan's decision, 2026-07-31; the theme states its scope in
//! `status_dot`'s own docs). Five mutually exclusive states have to be told
//! apart at a glance, at 16 px, with no text — ivory-vs-terracotta cannot
//! carry that. The exception is scoped to these semaphore dots: the status
//! hues never fill a control, a pill, a border, or any text elsewhere.
//!
//! The two "still running" states breathe — their fill alpha animates
//! between `motion.breathe_min_opacity` and 1.0 over `motion.breathe` ms
//! (see [`breath_at`]) — and the three settled states are steady. That
//! split is itself information: **movement on the bar always means work in
//! progress.** See [`breathes`] for the predicate, and
//! [`ClaudeCode::subscription`] for the animation timer and why it is
//! allowed to exist at all.
//!
//! # The stale-session quirk (accepted, v0.2)
//!
//! A session that dies without ever sending `"ended"` — Jordan closes the
//! terminal mid-task, `kill -9`s the process, the machine sleeps mid-hook —
//! leaves its last known status in the list forever; nothing ever removes
//! it. A TTL sweep (drop entries older than N minutes) would fix this, but
//! doing that here means a *timer* driving the worker, which is exactly the
//! poll CLAUDE.md forbids on the panel side — every other module's
//! reconnect backoff (`columns.rs`, `volume.rs`) only ever paces retry
//! attempts while disconnected, never ticks while healthy, and a TTL sweep
//! has no equivalent "only while unhealthy" excuse: it would have to wake
//! up on a schedule regardless of whether anything changed. (The breath
//! timer below is *not* a counter-example: it is sanctioned, purely
//! cosmetic, and gated on there being something to animate — see
//! [`ClaudeCode::subscription`].) So this module doesn't add a sweep.
//!
//! A linger is now more visible than it was in the aggregate-pill design —
//! an orphaned session leaves a dot on the bar rather than inflating a
//! count — but it is still cosmetic, and it self-heals the next time that
//! same `session_id` reports any status, including `"ended"`. Session ids
//! are UUIDs and are never reused, so a genuinely orphaned entry otherwise
//! only clears on panel restart. Flagged for the handoff, not fixed here.

use std::path::PathBuf;
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::stream::StreamExt;
use iced::futures::{SinkExt, Stream};
use iced::time::Instant;
use iced::widget::{container, row, Space};
use iced::{Element, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::style::container::SessionStatus;
use saola_theme::{style, Theme};
use zbus::{Connection, MatchRule, MessageStream};

use crate::config::ClaudeIcon;
use crate::icons::{self, Icon};

/// The Claude Code module's own message type (Stage 7's per-module refactor
/// — see `modules::clock::Message` for the full teaching note). `main.rs`
/// nests this as `Message::ClaudeCode(claude::Message)` and delegates the
/// whole thing to [`ClaudeCode::update`] rather than matching the inner
/// variants itself.
#[derive(Debug, Clone)]
pub enum Message {
    /// A fresh session list from the D-Bus worker (the full folded picture,
    /// not a delta — see the module doc comment).
    Updated(Sessions),
    /// A fresh rate-limit snapshot from the same worker — a `UsageChanged`
    /// signal's body, already parsed. Unlike [`Message::Updated`] this *is*
    /// the whole picture on its own (one account-wide snapshot, no fold),
    /// so applying it is a plain overwrite: newest signal wins.
    UsageUpdated(Usage),
    /// One frame of the breathing animation. Carries the tick's own
    /// [`Instant`] rather than being a unit variant like
    /// `clock::Message::Tick`: the phase is derived from *when* the tick
    /// happened, and taking that from the runtime's own timer means the
    /// module never reads a clock of its own (which would make
    /// [`ClaudeCode::update`] untestable and non-deterministic).
    Tick(Instant),
}

/// The object path `emit.sh` targets and this module listens on. Not a
/// service path in the UPower/iwd sense (nothing is *hosted* there) — it's
/// just the fixed address a broadcast signal claims to have come from.
const CLAUDE_CODE_PATH: &str = "/io/saola/ClaudeCode";

/// The signal's interface. Versioned (`1`) per the D-Bus convention of
/// baking a revision number into a custom interface name, so a future
/// breaking change to the signal's argument shape can ship as `...1` +
/// `...2` coexisting rather than an in-place break.
///
/// Teaching note (no `.sender(...)` on the match rule built from this):
/// `busctl --user emit` doesn't request a well-known bus name before
/// emitting — it sends the signal from whatever transient unique name
/// (`:1.234`, different every invocation) the session bus hands its
/// short-lived connection. There is no stable sender to filter on, by
/// design (PLAN.md: "no bus-name ownership on either side") — the
/// interface + path + member triple is the whole identity of this signal.
const CLAUDE_CODE_INTERFACE: &str = "io.saola.ClaudeCode1";

/// The per-session status signal: `StatusChanged(session_id: s, status: s,
/// transcript_path: s)`.
const STATUS_MEMBER: &str = "StatusChanged";

/// The account-wide rate-limit signal: `UsageChanged(five_hour_pct: d,
/// five_hour_resets_at: t, seven_day_pct: d, seven_day_resets_at: t)`.
/// See the module doc comment's schema section for why it carries no
/// `session_id`.
const USAGE_MEMBER: &str = "UsageChanged";

/// How often the breathing animation is redrawn while it is running.
///
/// A **frame budget, not a design token** — which is why it lives here as a
/// named constant rather than in saola-theme (the theme owns the breath's
/// *duration* and *depth*, `motion.breathe` and `motion.breathe_min_opacity`;
/// how finely the panel samples that curve is the panel's business, the same
/// way `columns.rs`'s `MAX_FULL_DASHES` is a panel-side count rather than a
/// theme size). 100 ms is roughly a twenty-fourth of the 2400 ms cycle,
/// which is far more than enough for a 16 px dot fading between two alphas:
/// the eye reads the *rate* of a slow fade, not its step count, and a
/// coarser sampling here costs nothing visually while waking the runtime an
/// order of magnitude less often than a 60 fps redraw would.
const BREATH_TICK: Duration = Duration::from_millis(100);

/// One tracked session: the id the hook reports it under, and its last
/// known state.
///
/// [`SessionStatus`] is saola-theme's own enum ([`style::container::status_dot`]
/// takes it), deliberately reused rather than mirrored locally — exactly as
/// `columns.rs` reuses `DashState`. The theme is the authority on what
/// states a semaphore dot can be in, and a parallel enum here would be one
/// more thing to keep in sync.
///
/// `"ended"` has no variant in it — it doesn't produce a status, it removes
/// the entry entirely (see [`fold`]), so by the time anything reads a
/// `SessionStatus` out of [`Sessions`], "ended" has already happened and
/// left no trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The hook's `$CLAUDE_SESSION_ID` (a UUID). Never displayed on the bar
    /// — it is the key the fold rewrites entries by (the usage popover shows
    /// a shortened form of it as a row label).
    id: String,
    /// What that session is doing, as of its last `StatusChanged`.
    status: SessionStatus,
    /// The session's JSONL transcript file, from the signal's third
    /// argument. `None` until a signal actually carries one (a legacy
    /// two-argument emitter, or an empty-string payload) — and a later
    /// empty payload never *clears* a known path (see [`Sessions::set`]):
    /// the transcript's location never changes within a session, so the
    /// last known value is always the best one.
    transcript: Option<PathBuf>,
}

/// One session's usage-popover coordinates — what [`ClaudeCode::
/// usage_targets`] hands `popovers::claude_usage`'s fetch task: enough to
/// label a row (id + dot) and to find the transcript whose token totals the
/// row shows. Owned values, deliberately (the fetch runs as a detached
/// `Task` and can't borrow the panel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageTarget {
    pub id: String,
    pub status: SessionStatus,
    pub transcript: Option<PathBuf>,
}

/// One account-wide rate-limit snapshot — a `UsageChanged` signal's body,
/// as of the moment Claude Code's statusline last refreshed. Two windows,
/// same shape each; `Copy` because it is four numbers, and handing the
/// popover its own copy is simpler than lending it a borrow across a
/// `view` boundary.
///
/// No timestamp field of its own: freshness is judged from
/// [`UsageWindow::resets_at`] (a reset in the past means the snapshot
/// predates it — see `popovers::claude_usage`), which is what lets the
/// stale case be detected at render time without this module ever reading
/// a clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    pub five_hour: UsageWindow,
    pub seven_day: UsageWindow,
}

/// One rate-limit window's gauge: how much of it is used, and when it
/// resets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsageWindow {
    /// Used share of the window, `0.0..=100.0` as Claude Code reports it
    /// (already a percentage, not a fraction).
    pub used_pct: f64,
    /// Unix epoch seconds of the window's next reset.
    pub resets_at: u64,
}

/// Every session this module currently knows about, in **first-seen order**
/// (see the module doc comment for why order is load-bearing and why this
/// is a `Vec` rather than a map).
///
/// A newtype rather than a bare `Vec<Session>` so the ordering invariant
/// has somewhere to be documented and enforced: [`Sessions::set`] is the
/// only way to add or update an entry, and it appends rather than
/// reordering.
///
/// `PartialEq` is load-bearing, not a convenience derive: the worker
/// compares each freshly folded list against the last one it sent and
/// suppresses duplicates (same dedupe as `columns.rs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sessions {
    entries: Vec<Session>,
}

impl Sessions {
    /// Sets one session's status: rewrites the existing entry **in place**
    /// (keeping its position in the row) or appends a new one at the end.
    /// A `Some` transcript updates the stored path; `None` (a legacy `ss`
    /// signal, or an empty-string payload) leaves whatever was already
    /// known — see [`Session::transcript`].
    fn set(&mut self, id: String, status: SessionStatus, transcript: Option<PathBuf>) {
        match self.entries.iter_mut().find(|session| session.id == id) {
            Some(session) => {
                session.status = status;
                if transcript.is_some() {
                    session.transcript = transcript;
                }
            }
            None => self.entries.push(Session {
                id,
                status,
                transcript,
            }),
        }
    }

    /// Drops a session entirely — its dot disappears and everything to its
    /// right shifts left. Removing an id that isn't tracked is a no-op.
    fn remove(&mut self, id: &str) {
        self.entries.retain(|session| session.id != id);
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether anything in the row wants the animation timer running — the
    /// gate [`ClaudeCode::subscription`] consults. `false` for an empty
    /// list, so a panel with no sessions runs no timer at all.
    fn any_breathing(&self) -> bool {
        self.entries.iter().any(|session| breathes(session.status))
    }
}

/// Claude Code module state: the last session list the worker pushed, plus
/// the two fields that drive the breathing animation.
///
/// Unlike `Battery`/`Network`, there's no separate `present` flag: an empty
/// session list already means "render nothing," whatever the reason (no
/// signal ever received, or every session ended cleanly). Note that —
/// unlike the pill this replaced — *settled* sessions still render: a blue
/// "done" or green "idle" dot is information Jordan asked for, so only a
/// genuinely empty list draws nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeCode {
    /// Which brand mark heads the dot row — the `claude-icon` knob from
    /// `panel.kdl`, resolved at boot ([`ClaudeCode::new`]) and swapped in
    /// place on a live config reload ([`ClaudeCode::set_icon`]). The
    /// same "config picks, module renders" split as `modules::mark`'s
    /// `source` field; defaults to the Anthropic mark.
    icon: ClaudeIcon,
    /// The row, in first-seen order.
    sessions: Sessions,
    /// The last rate-limit snapshot the worker pushed, if any ever arrived
    /// — deliberately separate from the per-session map (rate limits are
    /// account-wide; see the module doc comment's schema section). `None`
    /// until the first `UsageChanged` signal, which is the same "no signal
    /// yet renders nothing" contract the session list follows: a Claude
    /// Code config with no statusline wired simply never populates this.
    usage: Option<Usage>,
    /// When the current run of breathing started. `None` while nothing is
    /// animating; set from the first [`Message::Tick`] after the timer
    /// starts, and cleared again when the last breathing session settles so
    /// the next run begins from a fresh, dim phase rather than resuming
    /// mid-fade.
    ///
    /// Teaching note (why an epoch rather than an accumulator): the obvious
    /// implementation — `phase += tick_interval` every frame, wrapped into
    /// range — accumulates the error of every single tick, and a timer that
    /// fires a millisecond late (which every OS timer does) drifts a little
    /// further out of step forever. Storing the instant the run *started*
    /// and subtracting means each frame's phase is computed from scratch
    /// against real elapsed time, so a late or dropped tick costs that one
    /// frame and nothing after it.
    breath_epoch: Option<Instant>,
    /// How long the current breathing run has been going, as of the last
    /// tick. Turned into an opacity at render time by [`phase_of`] +
    /// [`breath_at`] — those need `motion.breathe` and
    /// `motion.breathe_min_opacity`, and the theme is only in hand inside
    /// [`ClaudeCode::view`].
    breath_elapsed: Duration,
}

impl ClaudeCode {
    /// Boot-time construction: `main.rs`'s `Panel::new` hands over the
    /// config's `claude-icon` choice, exactly as it hands `Mark::new` the
    /// `mark` source. Everything else starts at its default (no sessions,
    /// no breath running).
    pub fn new(icon: ClaudeIcon) -> Self {
        Self {
            icon,
            ..Self::default()
        }
    }

    /// Swap the brand mark on a live config reload, keeping everything the
    /// signals built up — the session list, the usage snapshot, and the
    /// breathing run's phase. `main.rs`'s reload arm calls this instead of
    /// rebuilding with [`Self::new`], which would empty the dot row until
    /// the next `StatusChanged` signal happened to arrive.
    pub fn set_icon(&mut self, icon: ClaudeIcon) {
        self.icon = icon;
    }

    /// Folds one of this module's messages into its state. `main.rs`'s
    /// `Panel::update` unwraps the outer `Message::ClaudeCode(..)` and
    /// hands the inner value straight here, so all of this module's
    /// update logic lives in this file rather than being spread across
    /// match arms in `main.rs` (the per-module refactor's whole point).
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Updated(sessions) => {
                self.sessions = sessions;
                // Nothing left to animate: drop the epoch so the timer's
                // next run starts from phase 0 (the dim end) instead of
                // resuming wherever the previous run happened to stop.
                // Harmless either way — the subscription has already gone
                // quiet — but it keeps the state honest.
                if !self.sessions.any_breathing() {
                    self.breath_epoch = None;
                    self.breath_elapsed = Duration::ZERO;
                }
            }
            // Newest snapshot wins, unconditionally — the emitter is
            // stateless and rebroadcasts on every statusline refresh, so
            // this arm has to be an idempotent overwrite (see the module
            // doc comment's schema section). It deliberately doesn't touch
            // the breath state: usage is popover data, not bar animation.
            Message::UsageUpdated(usage) => self.usage = Some(usage),
            Message::Tick(now) => {
                // `get_or_insert` is what makes the *first* tick of a run
                // establish the epoch: no separate "start the animation"
                // message is needed, because the first frame after the
                // subscription starts is exactly the moment the run began.
                let epoch = *self.breath_epoch.get_or_insert(now);
                // `saturating_duration_since` rather than `now - epoch`:
                // subtracting `Instant`s panics if the result would be
                // negative, and while the runtime's timer should never hand
                // us a tick older than the epoch, a panic on the UI thread
                // is not the way to find out otherwise.
                self.breath_elapsed = now.saturating_duration_since(epoch);
            }
        }
    }

    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Asks the same emptiness as `view`'s early return, so the
    /// two cannot drift apart.
    pub fn is_present(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// One usage-popover row request per tracked session, in row order:
    /// the id, its current dot, and the transcript file to sum usage from
    /// (`None` renders as that row's quiet "no usage data" state). Snapshot
    /// semantics — `popovers::claude_usage`'s fetch task owns the returned
    /// values, so a session list that changes while the read is in flight
    /// simply makes the popover show the moment the click happened, which
    /// is what a click-driven readout should do.
    /// The last rate-limit snapshot, for the usage popover — a copy, not a
    /// borrow, for the same detached-render reason as [`Self::
    /// usage_targets`]'s owned values. `None` means no `UsageChanged`
    /// signal has ever arrived (no statusline wired, or no API response
    /// yet this session) and the popover simply omits its gauges.
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    pub fn usage_targets(&self) -> Vec<UsageTarget> {
        self.sessions
            .entries
            .iter()
            .map(|session| UsageTarget {
                id: session.id.clone(),
                status: session.status,
                transcript: session.transcript.clone(),
            })
            .collect()
    }

    /// Renders the module's glyph and the dot row — or nothing at all when
    /// no session is tracked (`Space::new()` with no size is a zero-area
    /// widget — the region simply closes up around it).
    ///
    /// The leading brand mark (2026-08-01, alongside the usage popover;
    /// [`Icon::Anthropic`] by default, [`Icon::ClaudeCode`] via the
    /// `claude-icon` knob — see [`ClaudeIcon`]) is a bare ivory bar icon,
    /// exactly like `network`/`battery`'s glyphs: `icon_bar`-sized,
    /// `on_ink.primary`, steady — it never breathes and never takes a
    /// status color; the dots alone carry state.
    /// It is what makes the row read as *Claude Code's* dots now that the
    /// module sits in its own group beside the status cluster, and it is
    /// the visible face of the usage-popover trigger `main.rs` wraps this
    /// view in (the module itself stays click-ignorant, like every module).
    ///
    /// Built exactly like `columns.rs`'s dash strip, deliberately: a
    /// `container` draws its background across its own bounds, so the dot
    /// *is* the container and the zero-area `Space` is just the child it
    /// needs to have. `sizes.dash_height` square plus `status_dot`'s
    /// `radii.pill` closes it into a circle, and the row is spaced with the
    /// same `sizes.dash_gap` the minimap uses. Every value is a token; the
    /// only number this module contributes is the breath opacity, which is
    /// an animation phase rather than a color or a size.
    ///
    /// Not a `canvas`: hand-drawing the dots would mean reaching for raw
    /// `Color` values, which is precisely the local restyling CLAUDE.md
    /// forbids.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if self.sessions.is_empty() {
            return Space::new().into();
        }

        // Computed once for the whole row, not per dot: every breathing
        // session shares one phase, so the row pulses together rather than
        // shimmering out of step.
        let phase = phase_of(self.breath_elapsed, theme.motion.breathe);
        let breath = breath_at(phase, theme.motion.breathe_min_opacity);

        let dots = self.sessions.entries.iter().map(|session| {
            let opacity = if breathes(session.status) {
                breath
            } else {
                1.0
            };
            container(Space::new())
                .width(theme.sizes.dash_height)
                .height(theme.sizes.dash_height)
                .style(style::container::status_dot(theme, session.status, opacity))
                .into()
        });

        // The function form of `row` (not the `row!` macro) because the
        // children are a runtime-length iterator, not a fixed list. The
        // dots keep their own `dash_gap` rhythm; the glyph sits a
        // `bar_icon_gap` ahead of them, the same icon-to-content gap the
        // other bare status readouts use.
        let glyph = match self.icon {
            ClaudeIcon::Anthropic => Icon::Anthropic,
            ClaudeIcon::ClaudeCode => Icon::ClaudeCode,
        };

        iced::widget::row![
            icons::icon(
                glyph,
                theme.sizes.icon_bar,
                theme.on_ink.primary.into_iced(),
            ),
            row(dots)
                .spacing(theme.sizes.dash_gap)
                .align_y(iced::Center),
        ]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center)
        .into()
    }

    /// The Claude Code signal feed, plus — only while something is actually
    /// breathing — the animation timer.
    ///
    /// **Teaching note (the sanctioned exception to "nothing ticks faster
    /// than the clock").** CLAUDE.md's architecture rule is that every
    /// module maps to a signal and never to a poll; the clock's
    /// once-a-minute tick is the fastest thing on the bar. Jordan
    /// explicitly sanctioned this animation on 2026-07-31, and it is an
    /// exception on purpose, with a boundary:
    ///
    /// - It is **not a poll.** A poll asks a source "has anything changed?"
    ///   on a schedule. This timer asks nothing and reads nothing — the
    ///   session *state* still arrives only by D-Bus signal. The timer only
    ///   advances an opacity that is, by definition, a function of time.
    ///   That is the same distinction that lets `columns.rs` and `volume.rs`
    ///   sleep between reconnect attempts without breaking the rule.
    /// - It is **gated.** The `iced::time::every` subscription exists only
    ///   while at least one session is `Working` or `Subagents` (see
    ///   [`Sessions::any_breathing`]); with a settled row — or no sessions
    ///   at all, which is the overwhelmingly common case — this returns the
    ///   bus worker alone and the panel is fully idle again. An always-on
    ///   10 Hz timer would be the poll the rule forbids, wearing an
    ///   animation's clothes.
    ///
    /// Teaching note (subscription identity): iced recomputes
    /// `Panel::subscription` after every message and diffs the result.
    /// `Subscription::run(claude_code_stream)` keys on the *fn pointer*, so
    /// the bus worker is spawned once and survives every recomputation —
    /// including the ones where the branch below adds or drops the timer
    /// beside it (see `battery.rs`'s `subscription` for the full note).
    /// `iced::time::every` keys on its `Duration`, which is a constant here,
    /// so the timer likewise isn't restarted by unrelated messages; it is
    /// created when the gate opens and torn down when it closes, which is
    /// exactly the lifecycle wanted.
    pub fn subscription(&self) -> Subscription<Message> {
        let worker = Subscription::run(claude_code_stream);

        if !self.sessions.any_breathing() {
            return worker;
        }

        Subscription::batch([worker, iced::time::every(BREATH_TICK).map(Message::Tick)])
    }
}

/// Whether a status animates. The two "still running" states breathe; the
/// three settled ones are steady — movement on the bar always means work in
/// progress (see the module doc comment).
///
/// A free function rather than a method because [`SessionStatus`] is
/// saola-theme's type, not this crate's: Rust's orphan rule means only the
/// defining crate may add inherent impls to it. Exhaustive `match` (no `_`
/// arm) on purpose — a sixth status added to the theme becomes a compile
/// error here, which is the only place that would otherwise silently guess.
fn breathes(status: SessionStatus) -> bool {
    match status {
        SessionStatus::Working | SessionStatus::Subagents => true,
        SessionStatus::Attention | SessionStatus::Done | SessionStatus::Idle => false,
    }
}

/// Where in the breath cycle `elapsed` lands, as a fraction in `0.0..1.0`.
///
/// The modulo is done in **integer milliseconds** before any float division:
/// that is what keeps a run that has been breathing for an hour as precise
/// as one that started a second ago, instead of losing bits to an
/// ever-growing `f32`.
///
/// A zero (or absurd) `cycle_ms` would be a division by zero, so it degrades
/// to phase 0 — a steady dot at the dim end rather than a `NaN` the theme
/// would have to defend against.
fn phase_of(elapsed: Duration, cycle_ms: u32) -> f32 {
    if cycle_ms == 0 {
        return 0.0;
    }
    let cycle_ms = u128::from(cycle_ms);
    (elapsed.as_millis() % cycle_ms) as f32 / cycle_ms as f32
}

/// The opacity multiplier at a given phase: `min` at the ends of the cycle,
/// `1.0` at its midpoint, moving as a cosine in between.
///
/// Teaching note (why a cosine and not a triangle): the obvious "count up,
/// then count down" ramp is a *sawtooth in velocity* — the dot changes
/// brightness at a constant rate and then reverses direction instantly at
/// each end, which the eye reads as a flicker or a twitch, not a breath.
/// `(1 - cos(2πp)) / 2` eases in and out: its slope is zero at both
/// turning points, so the fade slows to a stop and reverses smoothly. The
/// result is then mapped from `0.0..=1.0` onto `min..=1.0`, so the dot
/// never disappears entirely at the dim end.
fn breath_at(phase: f32, min: f32) -> f32 {
    let eased = (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0;
    min + (1.0 - min) * eased
}

/// Folds one `StatusChanged` event into the session list.
///
/// `"ended"` removes the entry outright rather than setting it to some
/// "ended" variant — a session with no entry and a session that reported
/// `"ended"` must render identically (as nothing), and deleting the entry
/// is what makes that true for free instead of needing the view to filter
/// out an `Ended` state on every render.
///
/// An unrecognized `status` string (a future hook revision, a typo in a
/// hand-edited `settings.json` command) is ignored — the entry, if any,
/// keeps its last *known* value rather than being overwritten with
/// something this module can't render, and an unknown session isn't
/// conjured into existence by a status nobody can draw. Pure function of
/// its arguments (no D-Bus, no clock), which is what makes it
/// unit-testable below without a bus.
///
/// The wire's vocabulary is frozen with `contrib/claude-code/emit.sh`:
/// `working` (Claude is generating), `subagent` (subagents are running
/// under this session), `attention` (blocked on Jordan), `done` (finished,
/// awaiting review — the `Stop` hook), `idle` (session open, nothing
/// happening — the `SessionStart` hook), `ended` (gone).
///
/// `transcript` is the signal's third argument with `""` already mapped to
/// `None` by the caller — the empty string is the emitter's "I had no
/// payload to read", not a real path.
fn fold(sessions: &mut Sessions, session_id: String, status: &str, transcript: Option<PathBuf>) {
    match status {
        "working" => sessions.set(session_id, SessionStatus::Working, transcript),
        "subagent" => sessions.set(session_id, SessionStatus::Subagents, transcript),
        "attention" => sessions.set(session_id, SessionStatus::Attention, transcript),
        "done" => sessions.set(session_id, SessionStatus::Done, transcript),
        "idle" => sessions.set(session_id, SessionStatus::Idle, transcript),
        "ended" => sessions.remove(&session_id),
        _ => {}
    }
}

/// Parses a `UsageChanged` message's body into a [`Usage`] snapshot, or
/// `None` for anything that doesn't match the declared `dtdt` signature (a
/// hand-typed `busctl` call with the wrong types) — the same skip-don't-die
/// posture as the `StatusChanged` body parse in [`watch_claude_code`].
///
/// A free function taking the [`zbus::Message`] itself (rather than four
/// loose floats and ints) so the tests below can exercise the *real* parse,
/// wrong-body rejection included, against messages built with
/// [`zbus::Message::signal`] — no bus required: a `Message` is just a
/// serialized frame, and building one is pure.
fn parse_usage(message: &zbus::Message) -> Option<Usage> {
    let (five_hour_pct, five_hour_resets_at, seven_day_pct, seven_day_resets_at) =
        message.body().deserialize::<(f64, u64, f64, u64)>().ok()?;
    Some(Usage {
        five_hour: UsageWindow {
            used_pct: five_hour_pct,
            resets_at: five_hour_resets_at,
        },
        seven_day: UsageWindow {
            used_pct: seven_day_pct,
            resets_at: seven_day_resets_at,
        },
    })
}

/// Builds the async stream the subscription runs. See `battery.rs`'s
/// `battery_stream` for the full bridge teaching note (the channel, the
/// runtime it runs on). Every failure path here — no session bus, the
/// match rule failing to register, the message stream ending — funnels
/// into "send the hidden/default state, worker ends quietly," same
/// contract as every other module: the panel never goes down because
/// Claude Code hooks aren't wired up (or haven't fired yet).
fn claude_code_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        if watch_claude_code(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(Sessions::default())).await;
        }
    })
}

/// The worker proper: register the match rule, then fold and re-ship the
/// session list on every matching signal, forever.
///
/// Unlike `columns.rs`'s niri socket, there's no reconnect-with-backoff
/// loop here — a lost session-bus connection is a rare, session-ending
/// event (on par with the desktop session itself dying), not something
/// this module retries through, matching `battery`/`network`/`media`'s
/// "one connection attempt, worker ends quietly on loss" shape rather than
/// `columns`' "keep trying" one.
async fn watch_claude_code(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // Session bus: Claude Code hooks are per-user processes (`emit.sh` runs
    // as Jordan, from Jordan's terminal), not a system service — same
    // reasoning as `media.rs`'s MPRIS players.
    let connection = Connection::session().await?;

    // Teaching note (`MatchRule`, not a proxy): see the module doc comment
    // for why a proxy doesn't fit here. `msg_type(Signal)` restricts the
    // rule to signals (as opposed to method calls/returns/errors, which
    // share the same bus but are irrelevant here); `path` + `interface`
    // narrow it to this module's broadcasts. No `.sender(...)` — see
    // `CLAUDE_CODE_INTERFACE`'s doc comment.
    //
    // Deliberately no `.member(...)` since the interface grew its second
    // signal (`UsageChanged`, 2026-08-01): one interface-wide rule feeding
    // one worker that dispatches on the member name reads better here than
    // two parallel rule/stream/loop stacks that would differ only in their
    // body types — and a match rule is the bus-side filter anyway, so the
    // only messages reaching the dispatch below are this interface's own.
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(CLAUDE_CODE_PATH)?
        .interface(CLAUDE_CODE_INTERFACE)?
        .build();

    // `for_match_rule` registers the rule with the bus and returns a
    // `Stream` of every message matching it — filtered bus-side, so this
    // task is never woken for a signal it doesn't care about. `Some(8)`
    // matches every other worker's channel capacity in this crate; nothing
    // about this signal's volume calls for a different number.
    let mut signals = MessageStream::for_match_rule(rule, &connection, Some(8)).await?;

    let mut sessions = Sessions::default();
    // Dedupe against the last sent value — cheap, and it means a hook
    // double-firing (or a session re-reporting the status it already has)
    // doesn't wake the UI for a no-op. Not load-bearing the way
    // `columns.rs`'s dedupe is (there's no property-cache warm-fire or
    // title-spam source here to suppress) — just free correctness reusing
    // a pattern this crate already has. It matters slightly more now than
    // it did for the old pill: a redundant `Updated` would re-enter
    // `ClaudeCode::update`, and the gate there resets the breath epoch.
    let mut last_sent: Option<Sessions> = None;
    // The usage snapshot's own dedupe, and here it *is* load-bearing:
    // `statusline.sh` re-emits on every statusline refresh (throttled to
    // roughly 300 ms while Claude is generating), and the numbers only
    // actually move once per API response — so most of those signals are
    // exact repeats this suppression keeps off the UI thread.
    let mut last_usage: Option<Usage> = None;

    while let Some(message) = signals.next().await {
        // A message that fails to arrive cleanly (a transport-level error)
        // is skipped, not fatal — one bad frame shouldn't tear down every
        // other session's status. `MatchRule` already guarantees anything
        // that *does* arrive here matches interface/path, so the member
        // name is the only thing left to look at before reading the body.
        let Ok(message) = message else {
            continue;
        };

        // Dispatch on the member name — the one branch point the widened
        // match rule buys. An unknown member (a future third signal this
        // build predates) is skipped, same posture as an unknown status
        // string in `fold`.
        let is_usage = match message.header().member().map(|member| member.as_str()) {
            Some(USAGE_MEMBER) => true,
            Some(STATUS_MEMBER) => false,
            _ => continue,
        };

        if is_usage {
            let Some(usage) = parse_usage(&message) else {
                continue;
            };
            if last_usage == Some(usage) {
                continue;
            }
            if sender.send(Message::UsageUpdated(usage)).await.is_err() {
                return Ok(());
            }
            last_usage = Some(usage);
            continue;
        }

        // The signal's signature is `sss` (`session_id`, `status`,
        // `transcript_path`) per the bus schema — with the original
        // two-argument `ss` still accepted so a not-yet-updated `emit.sh`
        // keeps its status dots (it merely reports no transcript). Any
        // other body (a hand-typed `busctl` call with the wrong types) is
        // skipped the same way a malformed niri event line is in
        // `columns.rs`: keep the session alive rather than tearing the
        // whole worker down over one bad payload.
        let body = message.body();
        let (session_id, status, transcript) = match body.deserialize::<(String, String, String)>()
        {
            Ok((session_id, status, transcript)) => (session_id, status, transcript),
            Err(_) => match body.deserialize::<(String, String)>() {
                Ok((session_id, status)) => (session_id, status, String::new()),
                Err(_) => continue,
            },
        };
        // `""` is the emitter's "no payload to read", not a path.
        let transcript = (!transcript.is_empty()).then(|| PathBuf::from(transcript));

        fold(&mut sessions, session_id, &status, transcript);

        if last_sent.as_ref() == Some(&sessions) {
            continue;
        }
        if sender
            .send(Message::Updated(sessions.clone()))
            .await
            .is_err()
        {
            // Receiving side gone (subscription dropped) — stop quietly.
            return Ok(());
        }
        last_sent = Some(sessions.clone());
    }

    // The stream ended — the session bus connection itself is gone. Rare
    // (see this fn's doc comment), and not retried; returning `Ok` here
    // (rather than an error the wrapper would turn into a default-state
    // send) matches `battery`/`network`'s "worker ends quietly" contract
    // for this same case. The one accepted cost: if this ever happens with
    // dots on screen, they linger until the panel restarts — a second,
    // rarer flavor of the stale-session quirk the module doc comment
    // already flags. (The breath subscription keeps running in that case
    // too, which is the one place the animation outlives its source; it
    // costs a 10 Hz redraw of a frozen row until restart.)
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transcript-less fold — the shape every pre-usage-popover test in
    /// this module was written against, kept as a shim so those tests read
    /// as the status-fold tests they are. The transcript argument's own
    /// behavior is covered separately below.
    fn fold(sessions: &mut Sessions, session_id: String, status: &str) {
        super::fold(sessions, session_id, status, None);
    }

    /// The statuses in the row, in order — what the dots actually look
    /// like, minus the ids nobody sees.
    fn statuses(sessions: &Sessions) -> Vec<SessionStatus> {
        sessions
            .entries
            .iter()
            .map(|session| session.status)
            .collect()
    }

    /// The ids in the row, in order — the ordering assertions' subject.
    fn ids(sessions: &Sessions) -> Vec<&str> {
        sessions
            .entries
            .iter()
            .map(|session| session.id.as_str())
            .collect()
    }

    // -- the fold ----------------------------------------------------------

    #[test]
    fn fold_maps_every_wire_status_to_its_dot() {
        // The frozen wire vocabulary, one string per theme variant.
        for (wire, expected) in [
            ("working", SessionStatus::Working),
            ("subagent", SessionStatus::Subagents),
            ("attention", SessionStatus::Attention),
            ("done", SessionStatus::Done),
            ("idle", SessionStatus::Idle),
        ] {
            let mut sessions = Sessions::default();
            fold(&mut sessions, "a".to_string(), wire);
            assert_eq!(statuses(&sessions), vec![expected], "wire status {wire:?}");
        }
    }

    #[test]
    fn fold_rewrites_a_known_session_in_place() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "a".to_string(), "done");

        // One session, not three — the id is the key.
        assert_eq!(ids(&sessions), vec!["a"]);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Done]);
    }

    #[test]
    fn fold_ended_removes_the_session() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        assert_eq!(ids(&sessions), vec!["a"]);

        fold(&mut sessions, "a".to_string(), "ended");
        assert!(sessions.is_empty());
    }

    #[test]
    fn fold_ending_an_unknown_session_is_a_no_op() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "never-seen".to_string(), "ended");
        assert!(sessions.is_empty());
    }

    #[test]
    fn fold_ignores_unrecognized_status_strings() {
        let mut sessions = Sessions::default();
        // A future hook revision (or a typo) sending a status this module
        // doesn't know: the session is left untracked rather than getting
        // a guessed-at dot color.
        fold(&mut sessions, "a".to_string(), "compacting");
        assert!(sessions.is_empty());

        // Same, but for an already-tracked session: the last known-good
        // status survives rather than being clobbered.
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "a".to_string(), "compacting");
        assert_eq!(statuses(&sessions), vec![SessionStatus::Working]);
    }

    // -- the transcript path -----------------------------------------------

    #[test]
    fn a_signal_with_a_transcript_records_it() {
        let mut sessions = Sessions::default();
        super::fold(
            &mut sessions,
            "a".to_string(),
            "working",
            Some(PathBuf::from("/t/a.jsonl")),
        );
        assert_eq!(
            sessions.entries[0].transcript,
            Some(PathBuf::from("/t/a.jsonl"))
        );
    }

    #[test]
    fn a_transcriptless_signal_keeps_the_known_path() {
        // A legacy `ss` emitter (or an empty-payload hook run) reporting a
        // later status must not erase the path an earlier signal delivered
        // — the transcript's location never changes within a session.
        let mut sessions = Sessions::default();
        super::fold(
            &mut sessions,
            "a".to_string(),
            "working",
            Some(PathBuf::from("/t/a.jsonl")),
        );
        super::fold(&mut sessions, "a".to_string(), "done", None);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Done]);
        assert_eq!(
            sessions.entries[0].transcript,
            Some(PathBuf::from("/t/a.jsonl"))
        );
    }

    #[test]
    fn usage_targets_snapshot_the_row_in_order() {
        let mut claude = ClaudeCode::default();
        let mut sessions = Sessions::default();
        super::fold(
            &mut sessions,
            "a".to_string(),
            "working",
            Some(PathBuf::from("/t/a.jsonl")),
        );
        super::fold(&mut sessions, "b".to_string(), "idle", None);
        claude.update(Message::Updated(sessions));

        let targets = claude.usage_targets();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].id, "a");
        assert_eq!(targets[0].status, SessionStatus::Working);
        assert_eq!(targets[0].transcript, Some(PathBuf::from("/t/a.jsonl")));
        assert_eq!(targets[1].id, "b");
        assert_eq!(targets[1].transcript, None);
    }

    // -- the usage snapshot ------------------------------------------------

    /// A `UsageChanged` frame with the given body, built without a bus —
    /// see [`parse_usage`]'s doc comment for why this is possible.
    fn usage_signal<B>(body: &B) -> zbus::Message
    where
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        zbus::Message::signal(CLAUDE_CODE_PATH, CLAUDE_CODE_INTERFACE, USAGE_MEMBER)
            .expect("valid signal coordinates")
            .build(body)
            .expect("serializable body")
    }

    #[test]
    fn parse_usage_reads_the_dtdt_body() {
        let message = usage_signal(&(23.5f64, 1_738_425_600u64, 41.2f64, 1_738_857_600u64));
        let usage = parse_usage(&message).expect("a well-formed body");
        assert_eq!(usage.five_hour.used_pct, 23.5);
        assert_eq!(usage.five_hour.resets_at, 1_738_425_600);
        assert_eq!(usage.seven_day.used_pct, 41.2);
        assert_eq!(usage.seven_day.resets_at, 1_738_857_600);
    }

    #[test]
    fn parse_usage_rejects_a_mistyped_body() {
        // A hand-typed `busctl` call with the wrong signature — strings
        // where numbers belong, or too few arguments — parses to `None`
        // (skipped by the worker), never to a garbage snapshot.
        assert_eq!(parse_usage(&usage_signal(&("23.5", "soon"))), None);
        assert_eq!(parse_usage(&usage_signal(&(23.5f64, 1u64))), None);
    }

    #[test]
    fn a_usage_update_overwrites_the_snapshot() {
        let mut claude = ClaudeCode::default();
        assert_eq!(claude.usage(), None);

        let first = Usage {
            five_hour: UsageWindow {
                used_pct: 10.0,
                resets_at: 100,
            },
            seven_day: UsageWindow {
                used_pct: 20.0,
                resets_at: 200,
            },
        };
        claude.update(Message::UsageUpdated(first));
        assert_eq!(claude.usage(), Some(first));

        // Newest wins — the fold is a plain overwrite, no merging.
        let second = Usage {
            five_hour: UsageWindow {
                used_pct: 12.5,
                resets_at: 150,
            },
            ..first
        };
        claude.update(Message::UsageUpdated(second));
        assert_eq!(claude.usage(), Some(second));
    }

    #[test]
    fn usage_alone_does_not_put_the_module_on_the_bar() {
        // Rate limits are popover data: a snapshot with no tracked
        // sessions must not conjure the dot row (or its trigger) into
        // existence.
        let mut claude = ClaudeCode::default();
        claude.update(Message::UsageUpdated(Usage {
            five_hour: UsageWindow {
                used_pct: 50.0,
                resets_at: 100,
            },
            seven_day: UsageWindow {
                used_pct: 50.0,
                resets_at: 200,
            },
        }));
        assert!(!claude.is_present());
    }

    // -- ordering ----------------------------------------------------------

    #[test]
    fn sessions_render_in_first_seen_order() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "c".to_string(), "idle");
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "b".to_string(), "idle");

        // Insertion order, emphatically *not* sorted by id.
        assert_eq!(ids(&sessions), vec!["c", "a", "b"]);
    }

    #[test]
    fn a_status_change_never_reorders_the_row() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "b".to_string(), "idle");
        fold(&mut sessions, "c".to_string(), "idle");

        // The middle session cycles through every other state; its dot must
        // stay the middle dot throughout, because Jordan reads the row
        // positionally ("the second one needs me").
        for status in ["working", "subagent", "attention", "done", "idle"] {
            fold(&mut sessions, "b".to_string(), status);
            assert_eq!(ids(&sessions), vec!["a", "b", "c"], "after {status:?}");
        }
    }

    #[test]
    fn ending_a_session_closes_the_row_up_around_it() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "b".to_string(), "attention");
        fold(&mut sessions, "c".to_string(), "done");

        fold(&mut sessions, "b".to_string(), "ended");
        assert_eq!(ids(&sessions), vec!["a", "c"]);
        assert_eq!(
            statuses(&sessions),
            vec![SessionStatus::Working, SessionStatus::Done]
        );

        // A session that comes back after ending is a *new* arrival: it
        // goes to the end of the row, not back to the middle.
        fold(&mut sessions, "b".to_string(), "idle");
        assert_eq!(ids(&sessions), vec!["a", "c", "b"]);
    }

    // -- presence ----------------------------------------------------------

    #[test]
    fn settled_sessions_still_render() {
        // The behavioral break from the old aggregate pill: `done` and
        // `idle` used to reduce to "show nothing". They are dots now.
        let mut claude = ClaudeCode::default();
        assert!(!claude.is_present());

        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "b".to_string(), "done");
        claude.update(Message::Updated(sessions));
        assert!(claude.is_present());
    }

    #[test]
    fn an_empty_row_renders_nothing() {
        let mut claude = ClaudeCode::default();
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        claude.update(Message::Updated(sessions.clone()));
        assert!(claude.is_present());

        fold(&mut sessions, "a".to_string(), "ended");
        claude.update(Message::Updated(sessions));
        assert!(!claude.is_present());
    }

    // -- the breath gate ---------------------------------------------------

    #[test]
    fn only_the_running_states_breathe() {
        assert!(breathes(SessionStatus::Working));
        assert!(breathes(SessionStatus::Subagents));
        assert!(!breathes(SessionStatus::Attention));
        assert!(!breathes(SessionStatus::Done));
        assert!(!breathes(SessionStatus::Idle));
    }

    #[test]
    fn the_timer_is_gated_on_a_running_session() {
        let mut sessions = Sessions::default();
        // Nothing tracked at all — the overwhelmingly common case, and the
        // one where an ungated timer would be an outright poll.
        assert!(!sessions.any_breathing());

        // A row of settled dots is still steady: colored, but no animation.
        fold(&mut sessions, "a".to_string(), "attention");
        fold(&mut sessions, "b".to_string(), "done");
        fold(&mut sessions, "c".to_string(), "idle");
        assert!(!sessions.any_breathing());

        // One working session anywhere in the row opens the gate.
        fold(&mut sessions, "d".to_string(), "working");
        assert!(sessions.any_breathing());

        // As does a subagent run, on its own.
        fold(&mut sessions, "d".to_string(), "subagent");
        assert!(sessions.any_breathing());

        // And it closes again the moment that session settles.
        fold(&mut sessions, "d".to_string(), "done");
        assert!(!sessions.any_breathing());
    }

    // -- the breath curve --------------------------------------------------

    #[test]
    fn phase_wraps_at_the_cycle_boundary() {
        let cycle = 2400;
        assert_eq!(phase_of(Duration::ZERO, cycle), 0.0);
        assert_eq!(phase_of(Duration::from_millis(600), cycle), 0.25);
        assert_eq!(phase_of(Duration::from_millis(1200), cycle), 0.5);
        // Second cycle: same phase as the first, no drift.
        assert_eq!(phase_of(Duration::from_millis(3600), cycle), 0.5);
        // An hour in, still exact — the modulo happens in integer ms.
        assert_eq!(
            phase_of(
                Duration::from_secs(3600) + Duration::from_millis(1200),
                cycle
            ),
            0.5
        );
    }

    #[test]
    fn phase_degrades_instead_of_dividing_by_zero() {
        assert_eq!(phase_of(Duration::from_millis(500), 0), 0.0);
    }

    #[test]
    fn the_breath_spans_the_themes_range_smoothly() {
        let min = 0.45;
        // Dim at both ends of the cycle, bright dead centre.
        assert!((breath_at(0.0, min) - min).abs() < 1e-5);
        assert!((breath_at(1.0, min) - min).abs() < 1e-5);
        assert!((breath_at(0.5, min) - 1.0).abs() < 1e-5);
        // Symmetric about the midpoint, and never outside the range.
        assert!((breath_at(0.25, min) - breath_at(0.75, min)).abs() < 1e-5);
        for step in 0..=100 {
            let value = breath_at(step as f32 / 100.0, min);
            assert!(
                (min..=1.0).contains(&value),
                "breath left its range at step {step}: {value}"
            );
        }
    }

    #[test]
    fn the_breath_eases_rather_than_ramping() {
        // A triangle wave would put the quarter-cycle value exactly halfway
        // between the two ends; a cosine's ease means it is exactly there
        // only at the quarter point but *slower* near the turns — so the
        // first eighth covers less ground than the second.
        let min = 0.45;
        let first_eighth = breath_at(0.125, min) - breath_at(0.0, min);
        let second_eighth = breath_at(0.25, min) - breath_at(0.125, min);
        assert!(
            first_eighth < second_eighth,
            "expected an eased start, got {first_eighth} then {second_eighth}"
        );
    }

    // -- the animation clock -----------------------------------------------

    #[test]
    fn the_first_tick_establishes_the_epoch() {
        let mut claude = ClaudeCode::default();
        let start = Instant::now();

        claude.update(Message::Tick(start));
        // The run has just begun: zero elapsed, phase 0, dot at its dimmest.
        assert_eq!(claude.breath_elapsed, Duration::ZERO);

        claude.update(Message::Tick(start + Duration::from_millis(600)));
        assert_eq!(claude.breath_elapsed, Duration::from_millis(600));

        // Elapsed is measured from the epoch, not accumulated per tick — a
        // tick that arrives late (or after several were dropped) reports
        // real elapsed time rather than a running total of tick intervals.
        claude.update(Message::Tick(start + Duration::from_millis(5000)));
        assert_eq!(claude.breath_elapsed, Duration::from_millis(5000));
    }

    #[test]
    fn settling_resets_the_breath_so_the_next_run_starts_dim() {
        let mut claude = ClaudeCode::default();
        let start = Instant::now();

        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        claude.update(Message::Updated(sessions.clone()));
        claude.update(Message::Tick(start));
        claude.update(Message::Tick(start + Duration::from_millis(900)));
        assert_eq!(claude.breath_elapsed, Duration::from_millis(900));

        // The session finishes: nothing breathes, so the clock is put away.
        fold(&mut sessions, "a".to_string(), "done");
        claude.update(Message::Updated(sessions.clone()));
        assert_eq!(claude.breath_epoch, None);
        assert_eq!(claude.breath_elapsed, Duration::ZERO);

        // A later run re-establishes its own epoch from its own first tick.
        fold(&mut sessions, "a".to_string(), "working");
        claude.update(Message::Updated(sessions));
        let restart = start + Duration::from_secs(60);
        claude.update(Message::Tick(restart));
        assert_eq!(claude.breath_elapsed, Duration::ZERO);
    }

    #[test]
    fn an_update_while_breathing_leaves_the_animation_alone() {
        let mut claude = ClaudeCode::default();
        let start = Instant::now();

        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        claude.update(Message::Updated(sessions.clone()));
        claude.update(Message::Tick(start));
        claude.update(Message::Tick(start + Duration::from_millis(900)));

        // A second session joining must not restart the pulse mid-fade —
        // the whole row breathes together, on one clock.
        fold(&mut sessions, "b".to_string(), "subagent");
        claude.update(Message::Updated(sessions));
        assert_eq!(claude.breath_epoch, Some(start));
        assert_eq!(claude.breath_elapsed, Duration::from_millis(900));
    }
}
