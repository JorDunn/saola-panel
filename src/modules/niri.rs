//! The niri IPC bridge — **one socket, two modules**.
//!
//! Everything the panel knows about the compositor arrives on a single
//! `$NIRI_SOCKET` connection: newline-delimited JSON, one [`Event`] per line
//! after the `EventStream` handshake. Two bar modules are fed from it —
//! `modules::columns` (the strip minimap) and `modules::window_title` (the
//! focused window's title) — and this file is the one place that talks to
//! niri, folds its events into state, and decides when either derived value
//! actually changed.
//!
//! # Why a shared bridge rather than a worker per module (read this first)
//!
//! Stage 11 put the socket worker inside `columns.rs`, because the minimap
//! was the only consumer. The focused-title module (2026-08-01) is the
//! second, and it needs the *same* fold: "which window is focused" is
//! already tracked here, and the titles ride along on the very same
//! `WindowOpenedOrChanged` events. Three ways to arrange that were on the
//! table:
//!
//! 1. A second socket connection with a second copy of the reconnect logic —
//!    honest, but it parses every event twice and doubles the code that has
//!    to stay correct across a niri restart.
//! 2. Teach `columns::Message` to also carry titles — one connection, but a
//!    message named for the minimap would be delivering something the
//!    minimap has no opinion about.
//! 3. **This.** The worker moves out of the module it used to serve and into
//!    a bridge that owns the socket, the fold, and both dedupes, then emits
//!    each module's *own* `Message` wrapped in [`Message`]. `main.rs`
//!    unwraps one level and routes each half to the module it belongs to
//!    (`Panel::subscription`), so both modules keep the per-module message
//!    types CLAUDE.md's module pattern asks for.
//!
//! Neither `Columns` nor `WindowTitle` therefore runs a subscription of its
//! own: each keeps the method (returning `Subscription::none()`) so the batch
//! in `main.rs` still reads uniformly across every module, exactly as
//! `mark.rs`'s signal-less one does.
//!
//! # Two derived values, two dedupes
//!
//! niri emits `WindowOpenedOrChanged` on every *title* change — a terminal
//! running a spinner produces several per second. Those events matter to one
//! consumer and not the other, so each derived value is compared against the
//! last one sent and suppressed when it is unchanged:
//!
//! - the **dash row** ignores titles entirely (a title change folds into an
//!   identical [`Placement`], see [`Strip::dashes`]), so spinner churn is
//!   silent for the minimap exactly as it was before this refactor;
//! - the **focused title** ignores everything *but* the focused window's
//!   title text, so a window opening on another workspace, a layout reshuffle
//!   or a keyboard-layout switch never wakes it, and a spinner retitling to
//!   the same string twice sends once.
//!
//! Without those two comparisons the panel would re-render at the rate of
//! the chattiest window on screen — the poll CLAUDE.md forbids, wearing a
//! signal's clothes.
//!
//! # The state is folded, not snapshotted
//!
//! UPower/iwd/pulse each answer "what is the value right now?"; niri's event
//! stream answers "what just changed?". So the worker keeps a [`Strip`] —
//! which windows exist, where they sit, what they are called, what is focused
//! — and *folds* each event into it, then derives both rendered values. The
//! fold and the two derivations are pure functions of state + event, which is
//! what makes them unit-testable against synthetic event lines without a
//! compositor (see the tests at the bottom).
//!
//! Absent-service rule, as everywhere else: no `$NIRI_SOCKET` (not running
//! under niri) → the worker ends immediately, both modules stay at their
//! defaults, and the panel is unaffected.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream};
use iced::Subscription;
use niri_ipc::socket::SOCKET_PATH_ENV;
use niri_ipc::{Event, Reply, Request, Response, Window};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::columns::{self, ColumnDash, Columns};
use super::window_title;

/// What the bridge produces: one of its two consumers' own messages.
///
/// Deliberately *not* a niri-shaped enum (`FocusChanged`, `WindowRetitled`,
/// …). By the time a value leaves this file it is already the finished thing
/// a module renders — a dash row, or a title string — so `main.rs`'s routing
/// is a two-arm `match` that adds no niri knowledge to the UI side. The same
/// reasoning as `columns::Message` carrying the whole derived row rather than
/// a single changed dash.
#[derive(Debug, Clone)]
pub enum Message {
    /// A freshly derived minimap row for `modules::columns`.
    Columns(columns::Message),
    /// A freshly derived focused-window title for `modules::window_title`.
    WindowTitle(window_title::Message),
}

/// The niri event feed as an iced subscription — the panel's single
/// connection to the compositor.
///
/// Same identity mechanics as every module's own subscription:
/// `Subscription::run` keys on the `niri_stream` **fn pointer**, so the
/// worker is spawned once no matter how often `Panel::subscription` is
/// recomputed, and the `.map(..)` applied in `main.rs` composes onto the
/// already-keyed subscription without disturbing that key.
///
/// A free function rather than a method, because there is no state to hang
/// it off: the bridge belongs to the panel as a whole, not to either module
/// it feeds.
pub fn subscription() -> Subscription<Message> {
    Subscription::run(niri_stream)
}

/// How long the worker waits before its first reconnection attempt, and the
/// ceiling that doubling walks up to. niri restarting (or a config reload
/// that replaces the socket) is a normal event on a compositor under active
/// configuration, so the worker has to be patient rather than hammer the
/// socket path.
///
/// As in `volume.rs`: this sleep is **not** the poll CLAUDE.md forbids. It
/// only ever runs while disconnected, and it never reads any state — it just
/// paces reconnect attempts. A connected session is purely event-driven.
const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The fold
// ---------------------------------------------------------------------------

/// Where one window sits, as far as the minimap cares.
///
/// niri's `Window` carries a dozen fields (title, app id, pid, urgency, tile
/// geometry, focus timestamp…); the minimap needs two. Projecting down to
/// this at fold time — rather than storing whole `Window`s — is what makes
/// the minimap's "did anything change?" comparison cheap *and* correct: a
/// title change produces an identical `Placement`, so it cannot ripple into a
/// redraw of the strip. The title deliberately lives in a *separate* map on
/// [`Strip`] for exactly that reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    /// Which workspace the window is on, if any.
    workspace: Option<u64>,
    /// The window's 1-based column index, or `None` when it has no place in
    /// the scrolling layout — which is how niri reports **floating** windows.
    /// Floating windows are deliberately invisible to the minimap: they are
    /// not part of the strip, so they get no dash.
    column: Option<usize>,
}

impl Placement {
    fn of(window: &Window) -> Self {
        Self {
            workspace: window.workspace_id,
            // `pos_in_scrolling_layout` is `(column index, tile index within
            // the column)`, both 1-based. Two windows stacked in one column
            // differ only in the tile index — which the minimap drops on the
            // floor, because a column is one dash however many tiles it holds.
            column: window
                .layout
                .pos_in_scrolling_layout
                .map(|(column, _tile_in_column)| column),
        }
    }
}

/// The folded view of niri's state: everything needed to derive the dash row
/// and the focused title, and nothing else.
///
/// niri's event stream sends the full current state up front (a
/// `WorkspacesChanged` and a `WindowsChanged` arrive immediately after the
/// `EventStream` request is acknowledged), so this starts empty and is always
/// correct from the first pair of events onward — there is no separate
/// "prime it with a `Request::Windows` query" phase to get wrong.
#[derive(Debug, Default, PartialEq, Eq)]
struct Strip {
    /// The single focused workspace across all outputs, if any.
    focused_workspace: Option<u64>,
    /// The focused window's id, if any. niri guarantees at most one (zero
    /// when a layer-shell surface holds focus — which, note, is what happens
    /// if this very panel ever takes keyboard focus).
    focused_window: Option<u64>,
    /// Every known window's placement, by id.
    windows: HashMap<u64, Placement>,
    /// Every known window's title, by id — the same key space as `windows`,
    /// kept as a second map rather than a field on [`Placement`] so that the
    /// minimap's inputs stay title-free (see `Placement`'s doc comment).
    /// Windows with no title at all simply have no entry.
    titles: HashMap<u64, String>,
}

impl Strip {
    /// Folds one event into the state.
    ///
    /// Only six of niri 26.04's nineteen event variants matter here; the rest
    /// (keyboard layouts, screencasts, overview, config reloads, urgency,
    /// focus timestamps) are absorbed by the catch-all. Because the worker
    /// dedupes on the *derived* values, an ignored event costs exactly two
    /// no-op comparisons and never wakes the UI.
    fn apply(&mut self, event: Event) {
        match event {
            // A full replacement of the workspace set — so a workspace that
            // vanished from the list was deleted. Recomputing focus from the
            // list (rather than patching it) is what keeps a deleted focused
            // workspace from lingering as a stale id.
            Event::WorkspacesChanged { workspaces } => {
                self.focused_workspace = workspaces
                    .iter()
                    .find(|workspace| workspace.is_focused)
                    .map(|workspace| workspace.id);
            }
            // A workspace became *active on its output*, which is not the
            // same as focused (each output has its own active workspace);
            // only the `focused` flag moves the minimap.
            Event::WorkspaceActivated { id, focused } => {
                if focused {
                    self.focused_workspace = Some(id);
                }
            }
            // Full replacement of the window set, same reasoning as above.
            Event::WindowsChanged { windows } => {
                self.windows = windows
                    .iter()
                    .map(|window| (window.id, Placement::of(window)))
                    .collect();
                self.titles = windows
                    .iter()
                    .filter_map(|window| window.title.clone().map(|title| (window.id, title)))
                    .collect();
                self.focused_window = windows
                    .iter()
                    .find(|window| window.is_focused)
                    .map(|window| window.id);
            }
            // Open *or* change: the same variant carries a brand-new window
            // and a window whose title just ticked. niri documents that if
            // this window is focused then every other window is not, which is
            // exactly what storing a single id expresses. The `else if`
            // handles the mirror case — this window used to be the focused
            // one and is telling us it no longer is.
            Event::WindowOpenedOrChanged { window } => {
                self.windows.insert(window.id, Placement::of(&window));
                // A window that dropped its title (`title: null`) must lose
                // its entry, not keep the previous string: `insert`-or-
                // `remove` rather than `insert` alone.
                match &window.title {
                    Some(title) => {
                        self.titles.insert(window.id, title.clone());
                    }
                    None => {
                        self.titles.remove(&window.id);
                    }
                }
                if window.is_focused {
                    self.focused_window = Some(window.id);
                } else if self.focused_window == Some(window.id) {
                    self.focused_window = None;
                }
            }
            Event::WindowClosed { id } => {
                self.windows.remove(&id);
                self.titles.remove(&id);
                if self.focused_window == Some(id) {
                    self.focused_window = None;
                }
            }
            Event::WindowFocusChanged { id } => {
                self.focused_window = id;
            }
            // The event that actually moves dashes around: niri batches new
            // layout for every window whose position changed (opening a
            // column shifts everything to its right by one index).
            //
            // An id we've never seen is ignored rather than inserted: without
            // a matching `WindowOpenedOrChanged` we would not know its
            // workspace, and a window with an unknown workspace can only add
            // a phantom dash to whichever workspace we guessed.
            Event::WindowLayoutsChanged { changes } => {
                for (id, layout) in changes {
                    if let Some(placement) = self.windows.get_mut(&id) {
                        placement.column = layout
                            .pos_in_scrolling_layout
                            .map(|(column, _tile_in_column)| column);
                    }
                }
            }
            _ => {}
        }
    }

    /// Derives the dash row for the focused workspace.
    ///
    /// Pure function of the folded state — no I/O, no clock, no globals —
    /// which is what lets the tests drive whole event sequences through
    /// [`Strip::apply`] and assert on the picture that comes out.
    fn dashes(&self) -> Vec<ColumnDash> {
        // No focused workspace (nothing folded yet, or every output went
        // away) → nothing to draw.
        let Some(workspace) = self.focused_workspace else {
            return Vec::new();
        };

        // One dash per *distinct* column: a column holding three stacked
        // tiles is still one column, so sort + dedup rather than counting
        // windows. niri's indices are positional and contiguous today, but
        // deriving the row from the set that actually exists means a gap
        // would degrade into "one fewer dash" instead of a panic.
        let mut columns: Vec<usize> = self
            .windows
            .values()
            .filter(|placement| placement.workspace == Some(workspace))
            .filter_map(|placement| placement.column)
            .collect();
        columns.sort_unstable();
        columns.dedup();

        // The focused *column*, not the focused window: a focused window on
        // another workspace, or a focused floating window (no column), leaves
        // the strip with no terracotta dash — correct, because in both cases
        // no column on this strip is the live one.
        let focused = self
            .focused_window
            .and_then(|id| self.windows.get(&id))
            .filter(|placement| placement.workspace == Some(workspace))
            .and_then(|placement| placement.column);

        columns::build_dashes(&columns, focused)
    }

    /// The focused window's title, or `None` when there is no focused window
    /// (a layer-shell surface has focus, or nothing is open) or its title is
    /// empty.
    ///
    /// Blank-but-present titles are folded into `None` rather than rendered
    /// as an empty string: the style guide (§7) wants "no focused window" and
    /// "a window with an empty title" to look the same — nothing at all — and
    /// deciding that here means `window_title::WindowTitle` has exactly one
    /// absent case to handle. `trim` because a title of pure whitespace is
    /// empty to a reader even though it isn't to `str::is_empty`.
    ///
    /// Unlike [`Self::dashes`] this is *not* filtered to the focused
    /// workspace: the focused window is by definition on the focused
    /// workspace, and a floating window (which has no dash) still has a title
    /// worth showing.
    fn focused_title(&self) -> Option<String> {
        let id = self.focused_window?;
        let title = self.titles.get(&id)?;
        let trimmed = title.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }
}

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

/// How a niri session ended — which decides what the worker does next.
/// Mirrors `volume.rs`'s enum of the same shape; the reconnect contract is
/// meant to be identical across every module that can lose its source.
enum SessionEnd {
    /// The iced side dropped the receiver (panel shutting down). Stop.
    ChannelClosed,
    /// No socket, a handshake that failed, or a stream that ended.
    /// `saw_events` records whether this attempt ever actually delivered a
    /// parsed event: if it did, the loss is a *fresh* outage and the backoff
    /// restarts from the bottom instead of inheriting an old ceiling. Keying
    /// it on a delivered event rather than on the handshake matters — a niri
    /// that accepts the connection and immediately drops it would otherwise
    /// reset the backoff forever and never actually back off.
    Lost { saw_events: bool },
}

/// Builds the async stream the subscription runs. Identical bridge shape to
/// `battery.rs`: `iced::stream::channel` hands the async closure the `Sender`
/// half and gives iced the `Receiver` as a `Stream`, polled on the tokio
/// runtime iced's own `tokio` feature provides.
fn niri_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        niri_worker(&mut sender).await;
    })
}

/// The worker's whole life: connect, feed derived values until something
/// breaks, back off, try again.
async fn niri_worker(sender: &mut mpsc::Sender<Message>) {
    // Not running under niri (a different compositor, or a bare TTY): there
    // is no strip to mirror and no focused window to name, so the worker
    // simply ends. The channel closes, the subscription completes, and both
    // modules stay at their boot defaults — rendering nothing. Same
    // absent-service outcome as a missing UPower or a missing pulse server.
    let Some(socket_path) = std::env::var_os(SOCKET_PATH_ENV) else {
        return;
    };

    let mut backoff = RECONNECT_BACKOFF_START;

    loop {
        match run_session(&socket_path, sender).await {
            SessionEnd::ChannelClosed => return,
            SessionEnd::Lost { saw_events } => {
                if saw_events {
                    backoff = RECONNECT_BACKOFF_START;
                }
            }
        }

        // Clear both derived values while there is no compositor behind
        // them — a frozen minimap would claim columns that may not exist by
        // the time niri comes back, and a frozen title would name a window
        // that may already be gone. Sending is also how we notice the UI
        // side went away without waiting for niri.
        if sender
            .send(Message::Columns(columns::Message::Updated(
                Columns::default(),
            )))
            .await
            .is_err()
        {
            return;
        }
        if sender
            .send(Message::WindowTitle(window_title::Message::Updated(None)))
            .await
            .is_err()
        {
            return;
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }
}

/// One connection attempt, from `connect` to whatever ends it.
///
/// The protocol, straight from niri-ipc's own docs: write a JSON `Request` on
/// one line, read one JSON `Reply` line, and — for `EventStream` — keep
/// reading `Event` lines. Half-closing the write end after the request is
/// what niri-ipc's blocking `Socket::read_events` does too; niri takes it as
/// "no more requests are coming on this connection".
async fn run_session(socket_path: &OsStr, sender: &mut mpsc::Sender<Message>) -> SessionEnd {
    let Ok(stream) = UnixStream::connect(socket_path).await else {
        return SessionEnd::Lost { saw_events: false };
    };

    // Split into owned halves so the write end can be shut down and dropped
    // while the read end lives on for the rest of the session. The halves
    // share the socket, so dropping the writer after `shutdown` closes only
    // the write direction.
    let (reader, mut writer) = stream.into_split();

    // Serialized rather than hand-written (`"EventStream"`) so the wire form
    // always matches whatever `Request` this niri-ipc version defines.
    let Ok(mut request) = serde_json::to_string(&Request::EventStream) else {
        return SessionEnd::Lost { saw_events: false };
    };
    request.push('\n');
    if writer.write_all(request.as_bytes()).await.is_err() {
        return SessionEnd::Lost { saw_events: false };
    }
    if writer.shutdown().await.is_err() {
        return SessionEnd::Lost { saw_events: false };
    }
    drop(writer);

    let mut lines = BufReader::new(reader).lines();

    // Exactly one `Reply` line comes back before the event flood. Anything
    // other than `Ok(Handled)` — including a niri that answered with an error
    // string — means this connection will never carry events.
    match lines.next_line().await {
        Ok(Some(line)) => match serde_json::from_str::<Reply>(&line) {
            Ok(Ok(Response::Handled)) => {}
            _ => return SessionEnd::Lost { saw_events: false },
        },
        _ => return SessionEnd::Lost { saw_events: false },
    }

    let mut strip = Strip::default();
    let mut saw_events = false;
    // Both comparisons are seeded with the value the panel is *already*
    // showing rather than with `None`-meaning-unknown — deliberately. At the
    // instant a session starts, both modules hold their defaults: either it
    // is still the boot state, or the previous session's loss cleared them
    // (see `niri_worker`). niri opens the stream with a `WorkspacesChanged`
    // that arrives *before* the first `WindowsChanged`, so the first derived
    // row is always empty and the first derived title always `None`; seeding
    // this way suppresses those redundant "still nothing" messages and lets
    // the panel's first repaint be the real strip. (Runtime-verified against
    // niri 26.04: without the seed the first message on the channel was `[]`.)
    let mut last_columns = Columns::default();
    let mut last_title: Option<String> = None;

    loop {
        // `Ok(None)` is a clean EOF (niri exited); `Err` is a broken socket.
        // Both mean the same thing to us: this session is over, reconnect.
        let Ok(Some(line)) = lines.next_line().await else {
            return SessionEnd::Lost { saw_events };
        };

        let Ok(event) = serde_json::from_str::<Event>(&line) else {
            // A niri newer than the pinned niri-ipc can send event variants
            // this crate has never heard of. Skipping the line keeps the
            // session (and every event we *do* understand) alive instead of
            // tearing down both modules over an event about screencasts.
            continue;
        };
        saw_events = true;

        strip.apply(event);

        // The two dedupes (see the module doc comment). Title churn, focus
        // timestamps, keyboard-layout switches and screencast events all land
        // here producing values identical to the last, and stop dead —
        // separately, so a title change that leaves the strip alone wakes
        // only the title module and vice versa.
        let columns = Columns::from_dashes(strip.dashes());
        if last_columns != columns {
            if sender
                .send(Message::Columns(columns::Message::Updated(columns.clone())))
                .await
                .is_err()
            {
                return SessionEnd::ChannelClosed;
            }
            last_columns = columns;
        }

        let title = strip.focused_title();
        if last_title != title {
            if sender
                .send(Message::WindowTitle(window_title::Message::Updated(
                    title.clone(),
                )))
                .await
                .is_err()
            {
                return SessionEnd::ChannelClosed;
            }
            last_title = title;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saola_theme::style::container::DashState;

    /// Parses one JSON line exactly as the worker does.
    ///
    /// The tests drive real wire-format lines rather than hand-built
    /// `niri_ipc` structs, which buys two things for the price of one: the
    /// fold is exercised *and* the assumption that these payloads deserialize
    /// against the pinned niri-ipc version is checked. The shapes below are
    /// copied from live `niri msg --json event-stream` output on niri 26.04.
    fn event(line: &str) -> Event {
        serde_json::from_str(line).expect("test event should parse against the pinned niri-ipc")
    }

    /// A workspace object. `focused` also implies active, as niri reports it.
    fn workspace_json(id: u64, focused: bool) -> String {
        format!(
            r#"{{"id":{id},"idx":{id},"name":null,"output":"eDP-1","is_urgent":false,
               "is_active":{focused},"is_focused":{focused},"active_window_id":null}}"#
        )
    }

    /// A window object with an explicit title. `column: None` models a
    /// floating window, which is how niri reports one:
    /// `pos_in_scrolling_layout` absent. `title` is written raw into the
    /// JSON, so `"null"` models a window with no title at all.
    fn titled_window_json(
        id: u64,
        workspace: u64,
        column: Option<usize>,
        focused: bool,
        title: &str,
    ) -> String {
        let position = match column {
            Some(column) => format!("[{column},1]"),
            None => "null".to_owned(),
        };
        format!(
            r#"{{"id":{id},"title":{title},"app_id":"a","pid":1,"workspace_id":{workspace},
               "is_focused":{focused},"is_floating":{floating},"is_urgent":false,
               "layout":{{"pos_in_scrolling_layout":{position},
                          "tile_size":[100.0,100.0],"window_size":[96,96],
                          "tile_pos_in_workspace_view":null,
                          "window_offset_in_tile":[2.0,2.0]}},
               "focus_timestamp":null}}"#,
            floating = column.is_none(),
        )
    }

    /// The common case: a window titled `"t"`.
    fn window_json(id: u64, workspace: u64, column: Option<usize>, focused: bool) -> String {
        titled_window_json(id, workspace, column, focused, r#""t""#)
    }

    fn windows_changed(windows: &[String]) -> Event {
        event(&format!(
            r#"{{"WindowsChanged":{{"windows":[{}]}}}}"#,
            windows.join(",")
        ))
    }

    fn opened_or_changed(window: &str) -> Event {
        event(&format!(
            r#"{{"WindowOpenedOrChanged":{{"window":{window}}}}}"#
        ))
    }

    /// The two events niri always sends up front: workspace 1 focused, and
    /// three single-window columns on it with column 1 focused.
    fn primed_strip() -> Strip {
        let mut strip = Strip::default();
        strip.apply(event(&format!(
            r#"{{"WorkspacesChanged":{{"workspaces":[{},{}]}}}}"#,
            workspace_json(1, true),
            workspace_json(2, false)
        )));
        strip.apply(windows_changed(&[
            window_json(10, 1, Some(1), true),
            window_json(11, 1, Some(2), false),
            window_json(12, 1, Some(3), false),
        ]));
        strip
    }

    fn states(strip: &Strip) -> Vec<DashState> {
        strip.dashes().iter().map(|dash| dash.state).collect()
    }

    fn columns_of(strip: &Strip) -> Vec<usize> {
        strip.dashes().iter().map(|dash| dash.column).collect()
    }

    // -- the fold ----------------------------------------------------------

    #[test]
    fn empty_state_renders_nothing() {
        assert!(Strip::default().dashes().is_empty());
        assert_eq!(Strip::default().focused_title(), None);
    }

    #[test]
    fn one_dash_per_column_with_the_focused_one_live() {
        let strip = primed_strip();
        assert_eq!(columns_of(&strip), vec![1, 2, 3]);
        assert_eq!(
            states(&strip),
            vec![DashState::Focused, DashState::Rest, DashState::Rest]
        );
    }

    #[test]
    fn stacked_windows_share_one_column_dash() {
        let mut strip = primed_strip();
        // A second tile in column 2: same column, so still three dashes.
        strip.apply(opened_or_changed(&window_json(13, 1, Some(2), false)));
        assert_eq!(columns_of(&strip), vec![1, 2, 3]);
    }

    #[test]
    fn floating_windows_get_no_dash() {
        let mut strip = primed_strip();
        strip.apply(opened_or_changed(&window_json(14, 1, None, true)));
        // Three columns still, and nothing is live: the focused window is
        // floating, so no column on the strip is the focused one.
        assert_eq!(columns_of(&strip), vec![1, 2, 3]);
        assert_eq!(
            states(&strip),
            vec![DashState::Rest, DashState::Rest, DashState::Rest]
        );
    }

    #[test]
    fn windows_on_other_workspaces_are_ignored() {
        let mut strip = primed_strip();
        strip.apply(opened_or_changed(&window_json(20, 2, Some(1), false)));
        assert_eq!(columns_of(&strip), vec![1, 2, 3]);
    }

    #[test]
    fn closing_a_window_drops_its_column() {
        let mut strip = primed_strip();
        strip.apply(event(r#"{"WindowClosed":{"id":12}}"#));
        assert_eq!(columns_of(&strip), vec![1, 2]);
        assert_eq!(states(&strip), vec![DashState::Focused, DashState::Rest]);
    }

    #[test]
    fn closing_the_focused_window_leaves_nothing_live() {
        let mut strip = primed_strip();
        strip.apply(event(r#"{"WindowClosed":{"id":10}}"#));
        assert_eq!(columns_of(&strip), vec![2, 3]);
        assert_eq!(states(&strip), vec![DashState::Rest, DashState::Rest]);
    }

    #[test]
    fn focus_change_moves_the_live_dash() {
        let mut strip = primed_strip();
        strip.apply(event(r#"{"WindowFocusChanged":{"id":12}}"#));
        assert_eq!(
            states(&strip),
            vec![DashState::Rest, DashState::Rest, DashState::Focused]
        );

        // Focus leaving every window (a layer-shell surface took it) leaves
        // the strip intact with nothing live.
        strip.apply(event(r#"{"WindowFocusChanged":{"id":null}}"#));
        assert_eq!(
            states(&strip),
            vec![DashState::Rest, DashState::Rest, DashState::Rest]
        );
    }

    #[test]
    fn layout_changes_reindex_the_columns() {
        let mut strip = primed_strip();
        // A new column opened at the left: everything shifted right by one.
        strip.apply(event(
            r#"{"WindowLayoutsChanged":{"changes":[
                 [10,{"pos_in_scrolling_layout":[2,1],"tile_size":[1.0,1.0],
                      "window_size":[1,1],"tile_pos_in_workspace_view":null,
                      "window_offset_in_tile":[0.0,0.0]}],
                 [11,{"pos_in_scrolling_layout":[3,1],"tile_size":[1.0,1.0],
                      "window_size":[1,1],"tile_pos_in_workspace_view":null,
                      "window_offset_in_tile":[0.0,0.0]}],
                 [12,{"pos_in_scrolling_layout":[4,1],"tile_size":[1.0,1.0],
                      "window_size":[1,1],"tile_pos_in_workspace_view":null,
                      "window_offset_in_tile":[0.0,0.0]}]]}}"#,
        ));
        assert_eq!(columns_of(&strip), vec![2, 3, 4]);
        // Window 10 is still focused, and it is still the leftmost dash.
        assert_eq!(
            states(&strip),
            vec![DashState::Focused, DashState::Rest, DashState::Rest]
        );
    }

    #[test]
    fn layout_changes_for_unknown_windows_are_ignored() {
        let mut strip = primed_strip();
        strip.apply(event(
            r#"{"WindowLayoutsChanged":{"changes":[
                 [999,{"pos_in_scrolling_layout":[9,1],"tile_size":[1.0,1.0],
                       "window_size":[1,1],"tile_pos_in_workspace_view":null,
                       "window_offset_in_tile":[0.0,0.0]}]]}}"#,
        ));
        assert_eq!(columns_of(&strip), vec![1, 2, 3]);
    }

    #[test]
    fn switching_workspaces_switches_the_strip() {
        let mut strip = primed_strip();
        strip.apply(opened_or_changed(&window_json(20, 2, Some(1), false)));

        strip.apply(event(r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#));
        assert_eq!(columns_of(&strip), vec![1]);

        // Merely *activating* a workspace on another output must not steal
        // the strip.
        strip.apply(event(r#"{"WorkspaceActivated":{"id":1,"focused":false}}"#));
        assert_eq!(columns_of(&strip), vec![1]);
    }

    #[test]
    fn deleting_the_focused_workspace_empties_the_strip() {
        let mut strip = primed_strip();
        strip.apply(event(&format!(
            r#"{{"WorkspacesChanged":{{"workspaces":[{}]}}}}"#,
            workspace_json(2, false)
        )));
        // No workspace is focused any more → nothing to mirror.
        assert!(strip.dashes().is_empty());
    }

    #[test]
    fn title_churn_produces_an_identical_row() {
        // The minimap dedupe's whole justification: niri re-sends a window on
        // every title change (a terminal spinner does this several times a
        // second) and the derived row must come out byte-identical so the
        // worker stays silent.
        let mut strip = primed_strip();
        let before = strip.dashes();

        strip.apply(opened_or_changed(&titled_window_json(
            10,
            1,
            Some(1),
            true,
            r#""⠂""#,
        )));

        assert_eq!(strip.dashes(), before);
    }

    #[test]
    fn unrelated_events_leave_the_strip_alone() {
        let mut strip = primed_strip();
        let before = strip.dashes();
        for line in [
            r#"{"KeyboardLayoutSwitched":{"idx":1}}"#,
            r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#,
            r#"{"ConfigLoaded":{"failed":false}}"#,
            r#"{"CastsChanged":{"casts":[]}}"#,
            r#"{"WindowUrgencyChanged":{"id":11,"urgent":true}}"#,
            r#"{"WorkspaceActiveWindowChanged":{"workspace_id":1,"active_window_id":11}}"#,
        ] {
            strip.apply(event(line));
        }
        assert_eq!(strip.dashes(), before);
    }

    // -- the focused title -------------------------------------------------

    #[test]
    fn the_focused_windows_title_is_the_one_that_shows() {
        let strip = primed_strip();
        assert_eq!(strip.focused_title().as_deref(), Some("t"));
    }

    #[test]
    fn retitling_the_focused_window_changes_the_title() {
        let mut strip = primed_strip();
        strip.apply(opened_or_changed(&titled_window_json(
            10,
            1,
            Some(1),
            true,
            r#""nvim — src/main.rs""#,
        )));
        assert_eq!(strip.focused_title().as_deref(), Some("nvim — src/main.rs"));
    }

    /// The title module's own dedupe justification, mirroring
    /// `title_churn_produces_an_identical_row` for the other consumer: events
    /// that don't touch the focused window's title text must leave the
    /// derived title byte-identical, so the worker's comparison suppresses
    /// them. A *retitle to the same string* is the spinner case — niri sends
    /// the event, the derived value doesn't move, and the module never hears
    /// about it.
    #[test]
    fn events_that_do_not_change_the_title_derive_the_same_title() {
        let mut strip = primed_strip();
        let before = strip.focused_title();

        // The same window re-sent unchanged (niri does this on every
        // property change, titles included).
        strip.apply(opened_or_changed(&window_json(10, 1, Some(1), true)));
        assert_eq!(strip.focused_title(), before);

        // Another window retitling, a layout reshuffle, and an unrelated
        // event: none of them is about the focused window's title.
        strip.apply(opened_or_changed(&titled_window_json(
            11,
            1,
            Some(2),
            false,
            r#""busy spinner ⠂""#,
        )));
        strip.apply(event(r#"{"KeyboardLayoutSwitched":{"idx":1}}"#));
        assert_eq!(strip.focused_title(), before);
    }

    #[test]
    fn focus_moving_moves_the_title() {
        let mut strip = primed_strip();
        strip.apply(opened_or_changed(&titled_window_json(
            12,
            1,
            Some(3),
            false,
            r#""alacritty""#,
        )));
        strip.apply(event(r#"{"WindowFocusChanged":{"id":12}}"#));
        assert_eq!(strip.focused_title().as_deref(), Some("alacritty"));
    }

    #[test]
    fn no_focused_window_has_no_title() {
        let mut strip = primed_strip();
        // A layer-shell surface (this very panel, say) took focus.
        strip.apply(event(r#"{"WindowFocusChanged":{"id":null}}"#));
        assert_eq!(strip.focused_title(), None);

        // …and a closed focused window is the same story.
        let mut strip = primed_strip();
        strip.apply(event(r#"{"WindowClosed":{"id":10}}"#));
        assert_eq!(strip.focused_title(), None);
    }

    /// An untitled window and a blank-titled one both render as nothing —
    /// the style guide's "no focused window, or a window with an empty title,
    /// renders nothing".
    #[test]
    fn an_absent_or_blank_title_reads_as_no_title() {
        let mut strip = primed_strip();
        strip.apply(opened_or_changed(&titled_window_json(
            10,
            1,
            Some(1),
            true,
            "null",
        )));
        assert_eq!(strip.focused_title(), None);

        strip.apply(opened_or_changed(&titled_window_json(
            10,
            1,
            Some(1),
            true,
            r#""   ""#,
        )));
        assert_eq!(strip.focused_title(), None);
    }

    /// A floating window has no dash but still has a title: the two derived
    /// values answer different questions and must not be filtered alike.
    #[test]
    fn a_floating_focused_window_still_names_itself() {
        let mut strip = primed_strip();
        strip.apply(opened_or_changed(&titled_window_json(
            14,
            1,
            None,
            true,
            r#""Volume Control""#,
        )));
        assert!(!strip.dashes().iter().any(|d| d.state == DashState::Focused));
        assert_eq!(strip.focused_title().as_deref(), Some("Volume Control"));
    }
}
