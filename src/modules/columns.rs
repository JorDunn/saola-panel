//! The niri columns minimap — the spec's signature module.
//!
//! niri is a **scrollable strip**, not a grid, so there is no workspace grid
//! to draw and no taskbar to fill. The style guide (§7, "niri specifics")
//! asks instead for a live minimap of the current strip: one dash per column
//! of the focused workspace, the focused column widened, columns beyond the
//! minimap's budget shown as stubs at each end. That is the whole module.
//!
//! Three things make it different from the pills around it:
//!
//! 1. **The source is a Unix socket, not D-Bus.** `$NIRI_SOCKET` speaks
//!    newline-delimited JSON: write one [`Request`] line, read one
//!    `Reply` line, and after `Request::EventStream` niri keeps writing
//!    [`Event`] lines forever. That is a `Stream` shape, so this is a
//!    *zbus-shaped* worker (`iced::stream::channel` + an async task), not the
//!    thread bridge `volume.rs` needed — see `modules/mod.rs`. The only new
//!    ingredient is `tokio::net::UnixStream`, and Cargo.toml explains at
//!    length why that is a feature flag on iced's existing runtime and not a
//!    second executor.
//!
//! 2. **The state is folded, not snapshotted.** UPower/iwd/pulse each answer
//!    "what is the value right now?"; niri's event stream answers "what just
//!    changed?". So the worker keeps a [`Strip`] — which windows exist, where
//!    they sit, what is focused — and *folds* each event into it, then derives
//!    the dash row. The fold and the derivation are both pure functions of
//!    state + event, which is what makes them unit-testable against synthetic
//!    event lines without a compositor (see the tests at the bottom).
//!
//! 3. **It must dedupe.** niri emits `WindowOpenedOrChanged` on every *title*
//!    change — a terminal running a spinner produces several per second. The
//!    minimap does not care about titles, so the worker compares the derived
//!    dash row against the last one it sent and stays silent when nothing
//!    moved. Without this the panel would re-render at the rate of the
//!    chattiest window on screen, which is exactly the poll CLAUDE.md forbids,
//!    just wearing a signal's clothes.
//!
//! Absent-service rule, as everywhere else: no `$NIRI_SOCKET` (not running
//! under niri) → `Columns::default()` → the module renders nothing and the
//! panel is unaffected.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream};
use iced::widget::{container, row, Space};
use iced::{Element, Subscription};
use niri_ipc::socket::SOCKET_PATH_ENV;
use niri_ipc::{Event, Reply, Request, Response, Window};
use saola_theme::style::container::DashState;
use saola_theme::{style, Theme};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// The minimap's own message type (Stage 7's per-module refactor — see
/// `modules::clock::Message` for the full teaching note). `main.rs` nests it
/// as `Message::Columns(columns::Message)`.
///
/// The payload is the whole derived dash row rather than a single changed
/// dash: the worker recomputes the row from its folded state after every
/// event anyway, and shipping the finished row keeps *all* the niri knowledge
/// on the worker side of the channel. The UI never learns what a workspace is.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Columns),
}

/// How many columns get a full-size dash. Columns outside this window become
/// stubs (see [`build_dashes`]).
///
/// This is a **count, not a size** — no saola-theme token governs it (the
/// theme owns the dash *geometry*: `sizes.dash_width_rest` and friends). Odd
/// so the focused dash can sit dead centre with equal context either side.
/// Seven full dashes plus two stubs per end is ~318 px of strip, which fits
/// beside the clock in the centre region without crowding it.
///
/// Stage 18 (KDL config) is the natural place to make this a user knob; until
/// then a single named constant beats a magic number buried in the algorithm.
const MAX_FULL_DASHES: usize = 7;

/// How many off-window columns are shown as stubs at *each* end. Stubs say
/// "the strip continues"; they are not a census, so two is plenty and a
/// forty-column workspace still cannot push the clock off the bar.
const MAX_STUBS_PER_END: usize = 2;

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

/// One dash in the minimap: which niri column it stands for, and how it is
/// drawn.
///
/// [`DashState`] is saola-theme's own enum (`style::container::dash` takes
/// it), deliberately reused rather than mirrored locally — the theme is the
/// authority on what states a dash can be in, and a parallel enum here would
/// be one more thing to keep in sync.
///
/// `column` is niri's **1-based** column index within the workspace. Nothing
/// reads it yet; it is carried because a future click-to-focus dash needs
/// exactly this number (it is the argument `Action::FocusColumn` takes), and
/// deriving it after the fact would mean re-deriving the whole strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnDash {
    /// niri's 1-based column index within the focused workspace.
    pub column: usize,
    /// How this dash is drawn: full-size at rest, widened + terracotta when
    /// focused, small when it stands for an off-window column.
    pub state: DashState,
}

/// Minimap module state: the last dash row the niri worker pushed through
/// [`Message::Updated`].
///
/// `Default` is the boot state — an empty row — so the module renders nothing
/// until niri actually reports a workspace. That makes "not running under
/// niri", "niri restarted and we're reconnecting", "the focused workspace is
/// empty", and "the worker hasn't reported yet" all render identically: as
/// nothing, exactly like the D-Bus modules' `present: false`.
///
/// `PartialEq` is load-bearing, not a convenience derive: the worker compares
/// each freshly derived row against the last one it sent and suppresses
/// duplicates (see the module doc comment on title spam).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Columns {
    dashes: Vec<ColumnDash>,
}

impl Columns {
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Asks the same emptiness as `view`'s early return, so the
    /// two cannot drift apart. Empty means no `$NIRI_SOCKET` (or no columns
    /// yet): the strip pill simply doesn't exist until niri speaks.
    pub fn is_present(&self) -> bool {
        !self.dashes.is_empty()
    }

    /// Renders the strip: one small pill per dash, or nothing at all when the
    /// row is empty (`Space::new()` with no size is a zero-area widget — the
    /// centre row simply closes up around it).
    ///
    /// Every value here is a token: `sizes.dash_height`, the three
    /// `sizes.dash_width_*` widths, `sizes.dash_gap`, and the fills that
    /// `style::container::dash` picks per state (ivory stepped through the
    /// tertiary/quaternary alpha roles at rest and for stubs, full terracotta
    /// for the focused one — the one rule, with the focused column as the
    /// single "live" element on the strip).
    ///
    /// Not a `canvas`: hand-drawing the strip would mean reaching for raw
    /// `Color` values and re-implementing the pill radius, which is precisely
    /// the local restyling CLAUDE.md forbids. A row of styled containers gets
    /// the same picture out of the theme's own vocabulary.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if self.dashes.is_empty() {
            return Space::new().into();
        }

        // A `container` draws its background across its own bounds, so the
        // dash is the container itself; the zero-area `Space` is just the
        // child it needs to have. Width comes from the state, height is one
        // token for every dash — stubs are shorter *in length*, not thinner,
        // so the strip reads as one continuous rule.
        let dashes = self.dashes.iter().map(|dash| {
            container(Space::new())
                .width(dash_width(theme, dash.state))
                .height(theme.sizes.dash_height)
                .style(style::container::dash(theme, dash.state))
                .into()
        });

        // The function form of `row` (not the `row!` macro) because the
        // children are a runtime-length iterator, not a fixed list.
        row(dashes)
            .spacing(theme.sizes.dash_gap)
            .align_y(iced::Center)
            .into()
    }

    /// The niri event feed as an iced subscription.
    ///
    /// Same identity mechanics as every other module: `Subscription::run`
    /// keys on the `columns_stream` **fn pointer**, so the worker is spawned
    /// once no matter how often `Panel::subscription` is recomputed, and the
    /// `.map(crate::Message::Columns)` applied in `main.rs` composes onto the
    /// already-keyed subscription without disturbing that key.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(columns_stream)
    }
}

/// Token width for a dash in a given state. Kept as one function so the
/// state→token mapping lives in exactly one place.
fn dash_width(theme: &Theme, state: DashState) -> f32 {
    match state {
        DashState::Rest => theme.sizes.dash_width_rest,
        DashState::Focused => theme.sizes.dash_width_focused,
        DashState::Stub => theme.sizes.dash_width_stub,
    }
}

// ---------------------------------------------------------------------------
// The fold
// ---------------------------------------------------------------------------

/// Where one window sits, as far as the minimap cares.
///
/// niri's `Window` carries a dozen fields (title, app id, pid, urgency, tile
/// geometry, focus timestamp…); the minimap needs two. Projecting down to
/// this at fold time — rather than storing whole `Window`s — is what makes
/// the "did anything change?" comparison cheap *and* correct: a title change
/// produces an identical `Placement`, so it cannot ripple into a redraw.
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

/// The folded view of niri's state: everything needed to derive the dash row,
/// and nothing else.
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
    /// Every known window, by id.
    windows: HashMap<u64, Placement>,
}

impl Strip {
    /// Folds one event into the state.
    ///
    /// Only six of niri 26.04's nineteen event variants matter here; the rest
    /// (keyboard layouts, screencasts, overview, config reloads, urgency,
    /// focus timestamps) are absorbed by the catch-all. Because the worker
    /// dedupes on the *derived* row, an ignored event costs exactly one
    /// no-op comparison and never wakes the UI.
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
                if window.is_focused {
                    self.focused_window = Some(window.id);
                } else if self.focused_window == Some(window.id) {
                    self.focused_window = None;
                }
            }
            Event::WindowClosed { id } => {
                self.windows.remove(&id);
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

        build_dashes(&columns, focused)
    }
}

/// Turns a sorted, deduplicated column list plus the focused column into the
/// dash row, applying the minimap's budget.
///
/// The rule: at most [`MAX_FULL_DASHES`] full-size dashes, as a window
/// centred on the focused column and clamped to the ends of the strip; up to
/// [`MAX_STUBS_PER_END`] columns immediately outside that window become
/// stubs, and anything further out is not drawn at all.
///
/// **Why a budget rather than niri's own idea of "off-screen"** (worth
/// knowing before changing this): the style guide describes the stubs as
/// *off-screen* columns, and niri's `WindowLayout` does carry a
/// `tile_pos_in_workspace_view` field that sounds like the answer. It is an
/// `Option`, documented as "unset for some windows", and on niri 26.04 it
/// comes back `null` for every ordinary tiled window — so it cannot decide
/// visibility today. There is also no view-scroll-offset anywhere in the IPC
/// surface to compute it from. A fixed dash budget gives the same picture
/// (a bounded strip that stubs out at both ends and keeps the focused column
/// in view) from information that actually exists.
///
/// Pure: no theme, no state, no I/O. Sizes enter only at render time.
fn build_dashes(columns: &[usize], focused: Option<usize>) -> Vec<ColumnDash> {
    if columns.is_empty() {
        return Vec::new();
    }

    // Position *within the list*, which is what the window maths needs — as
    // opposed to `focused`, which is niri's column number.
    let focused_at = focused.and_then(|column| columns.iter().position(|&c| c == column));

    // Half-open range of full-size dashes.
    let (start, end) = if columns.len() <= MAX_FULL_DASHES {
        (0, columns.len())
    } else {
        // Centre on the focus; with no focus (focused window floating or on
        // another workspace) show the head of the strip.
        let centre = focused_at.unwrap_or(0);
        let start = centre.saturating_sub(MAX_FULL_DASHES / 2);
        // Clamp the *end* into range first, then re-derive the start from it,
        // so a focus near the right edge slides the whole window left instead
        // of shrinking it. `columns.len() > MAX_FULL_DASHES` on this branch,
        // so `end >= MAX_FULL_DASHES` and the subtraction cannot underflow.
        let end = (start + MAX_FULL_DASHES).min(columns.len());
        (end - MAX_FULL_DASHES, end)
    };

    let leading_stubs = start.saturating_sub(MAX_STUBS_PER_END);
    let trailing_stubs = (end + MAX_STUBS_PER_END).min(columns.len());

    let mut dashes = Vec::with_capacity(trailing_stubs - leading_stubs);
    dashes.extend(
        columns[leading_stubs..start]
            .iter()
            .map(|&column| ColumnDash {
                column,
                state: DashState::Stub,
            }),
    );
    dashes.extend(
        columns[start..end]
            .iter()
            .enumerate()
            .map(|(offset, &column)| ColumnDash {
                column,
                state: if focused_at == Some(start + offset) {
                    DashState::Focused
                } else {
                    DashState::Rest
                },
            }),
    );
    dashes.extend(
        columns[end..trailing_stubs]
            .iter()
            .map(|&column| ColumnDash {
                column,
                state: DashState::Stub,
            }),
    );
    dashes
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
fn columns_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        niri_worker(&mut sender).await;
    })
}

/// The worker's whole life: connect, feed dash rows until something breaks,
/// back off, try again.
async fn niri_worker(sender: &mut mpsc::Sender<Message>) {
    // Not running under niri (a different compositor, or a bare TTY): there
    // is no strip to mirror, so the worker simply ends. The channel closes,
    // the subscription completes, and `Columns::default()` — already the
    // panel's boot state — keeps the module rendering nothing. Same
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

        // Clear the strip while there is no compositor to mirror — a frozen
        // minimap would be worse than an empty one, since it would claim
        // columns that may not exist by the time niri comes back. Sending is
        // also how we notice the UI side went away without waiting for niri.
        if sender
            .send(Message::Updated(Columns::default()))
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
    // Seeded with the empty row rather than `None` — deliberately. At the
    // instant a session starts, the panel's `Columns` is *always* the default
    // one: either it is still the boot state, or the previous session's loss
    // cleared it (see `niri_worker`). niri opens the stream with a
    // `WorkspacesChanged` that arrives *before* the first `WindowsChanged`,
    // so the first derived row is always empty; seeding the comparison this
    // way suppresses that redundant "still nothing" message and lets the
    // panel's first repaint be the real strip. (Runtime-verified against
    // niri 26.04: without the seed the first message on the channel was `[]`.)
    let mut last_sent = Columns::default();

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
            // tearing down the minimap over an event about screencasts.
            continue;
        };
        saw_events = true;

        strip.apply(event);
        let columns = Columns {
            dashes: strip.dashes(),
        };

        // The dedupe. Title churn, focus timestamps, keyboard-layout
        // switches and screencast events all land here producing a row
        // identical to the last, and stop dead.
        if last_sent == columns {
            continue;
        }
        if sender
            .send(Message::Updated(columns.clone()))
            .await
            .is_err()
        {
            return SessionEnd::ChannelClosed;
        }
        last_sent = columns;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A window object. `column: None` models a floating window, which is how
    /// niri reports one: `pos_in_scrolling_layout` absent.
    fn window_json(id: u64, workspace: u64, column: Option<usize>, focused: bool) -> String {
        let position = match column {
            Some(column) => format!("[{column},1]"),
            None => "null".to_owned(),
        };
        format!(
            r#"{{"id":{id},"title":"t","app_id":"a","pid":1,"workspace_id":{workspace},
               "is_focused":{focused},"is_floating":{floating},"is_urgent":false,
               "layout":{{"pos_in_scrolling_layout":{position},
                          "tile_size":[100.0,100.0],"window_size":[96,96],
                          "tile_pos_in_workspace_view":null,
                          "window_offset_in_tile":[2.0,2.0]}},
               "focus_timestamp":null}}"#,
            floating = column.is_none(),
        )
    }

    fn windows_changed(windows: &[String]) -> Event {
        event(&format!(
            r#"{{"WindowsChanged":{{"windows":[{}]}}}}"#,
            windows.join(",")
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
        strip.apply(event(&format!(
            r#"{{"WindowOpenedOrChanged":{{"window":{}}}}}"#,
            window_json(13, 1, Some(2), false)
        )));
        assert_eq!(columns_of(&strip), vec![1, 2, 3]);
    }

    #[test]
    fn floating_windows_get_no_dash() {
        let mut strip = primed_strip();
        strip.apply(event(&format!(
            r#"{{"WindowOpenedOrChanged":{{"window":{}}}}}"#,
            window_json(14, 1, None, true)
        )));
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
        strip.apply(event(&format!(
            r#"{{"WindowOpenedOrChanged":{{"window":{}}}}}"#,
            window_json(20, 2, Some(1), false)
        )));
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
        strip.apply(event(&format!(
            r#"{{"WindowOpenedOrChanged":{{"window":{}}}}}"#,
            window_json(20, 2, Some(1), false)
        )));

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
        // The dedupe's whole justification: niri re-sends a window on every
        // title change (a terminal spinner does this several times a second)
        // and the derived row must come out byte-identical so the worker
        // stays silent.
        let mut strip = primed_strip();
        let before = strip.dashes();

        let spinner = window_json(10, 1, Some(1), true).replace(r#""title":"t""#, r#""title":"⠂""#);
        strip.apply(event(&format!(
            r#"{{"WindowOpenedOrChanged":{{"window":{spinner}}}}}"#
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

    // -- the budget --------------------------------------------------------

    #[test]
    fn short_strips_have_no_stubs() {
        let columns: Vec<usize> = (1..=MAX_FULL_DASHES).collect();
        let dashes = build_dashes(&columns, Some(1));
        assert_eq!(dashes.len(), MAX_FULL_DASHES);
        assert!(!dashes.iter().any(|dash| dash.state == DashState::Stub));
    }

    #[test]
    fn long_strips_stub_out_at_both_ends() {
        // 21 columns, focus dead centre: a full window either side, both
        // stub allowances used.
        let columns: Vec<usize> = (1..=21).collect();
        let dashes = build_dashes(&columns, Some(11));

        assert_eq!(dashes.len(), MAX_FULL_DASHES + 2 * MAX_STUBS_PER_END);
        assert_eq!(
            dashes.iter().map(|d| d.column).collect::<Vec<_>>(),
            vec![6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(dashes[0].state, DashState::Stub);
        assert_eq!(dashes[1].state, DashState::Stub);
        assert_eq!(
            dashes[MAX_STUBS_PER_END + MAX_FULL_DASHES / 2].state,
            DashState::Focused
        );
        assert_eq!(dashes[dashes.len() - 1].state, DashState::Stub);
    }

    #[test]
    fn focus_at_the_left_edge_clamps_the_window() {
        let columns: Vec<usize> = (1..=21).collect();
        let dashes = build_dashes(&columns, Some(1));

        // No leading stubs — there is nothing to the left of column 1.
        assert_eq!(dashes[0].column, 1);
        assert_eq!(dashes[0].state, DashState::Focused);
        assert_eq!(dashes.len(), MAX_FULL_DASHES + MAX_STUBS_PER_END);
        assert_eq!(dashes[dashes.len() - 1].state, DashState::Stub);
    }

    #[test]
    fn focus_at_the_right_edge_slides_the_window_instead_of_shrinking_it() {
        let columns: Vec<usize> = (1..=21).collect();
        let dashes = build_dashes(&columns, Some(21));

        assert_eq!(dashes.len(), MAX_FULL_DASHES + MAX_STUBS_PER_END);
        let last = dashes[dashes.len() - 1];
        assert_eq!(last.column, 21);
        assert_eq!(last.state, DashState::Focused);
        // Still a full window of live dashes, just pushed to the right end.
        assert_eq!(
            dashes.iter().filter(|d| d.state != DashState::Stub).count(),
            MAX_FULL_DASHES
        );
    }

    #[test]
    fn a_long_strip_with_no_focus_shows_its_head() {
        let columns: Vec<usize> = (1..=21).collect();
        let dashes = build_dashes(&columns, None);

        assert_eq!(dashes[0].column, 1);
        assert!(!dashes.iter().any(|dash| dash.state == DashState::Focused));
        assert_eq!(dashes.len(), MAX_FULL_DASHES + MAX_STUBS_PER_END);
    }

    #[test]
    fn a_focus_outside_the_column_list_is_not_drawn() {
        // Defensive: a focused window whose column we haven't folded yet.
        let dashes = build_dashes(&[1, 2, 3], Some(9));
        assert_eq!(dashes.len(), 3);
        assert!(!dashes.iter().any(|dash| dash.state == DashState::Focused));
    }
}
