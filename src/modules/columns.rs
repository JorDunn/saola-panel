//! The niri columns minimap — the spec's signature module.
//!
//! niri is a **scrollable strip**, not a grid, so there is no workspace grid
//! to draw and no taskbar to fill. The style guide (§7, "niri specifics")
//! asks instead for a live minimap of the current strip: one dash per column
//! of the focused workspace, the focused column widened, columns beyond the
//! minimap's budget shown as stubs at each end. That is the whole module.
//!
//! # Where the data comes from (changed 2026-08-01)
//!
//! Stage 11 built this module around its own `$NIRI_SOCKET` worker: this file
//! owned the connection, the event fold, and the dedupe. The focused-window
//! title module needs the same fold from the same socket, so all of that
//! moved into **`modules::niri`**, the shared niri bridge — read that file's
//! doc comment for the socket protocol, the fold, the reconnect contract, and
//! why the minimap's "title churn must not redraw the strip" property is
//! still enforced there (it is: the fold's `Placement` projection has no
//! title in it, so a retitle derives a byte-identical row and the bridge
//! stays silent).
//!
//! What is left here is everything the *minimap* owns: the rendered value
//! ([`Columns`]), the dash budget ([`build_dashes`]), and the view. The
//! bridge builds a `Columns` with [`Columns::from_dashes`] and ships it as
//! [`Message::Updated`]; `main.rs` routes it to this module's state exactly
//! as it routes every other module's snapshot.
//!
//! Absent-service rule, as everywhere else: no `$NIRI_SOCKET` (not running
//! under niri) → `Columns::default()` → the module renders nothing and the
//! panel is unaffected.

use iced::widget::{container, row, Space};
use iced::{Element, Subscription};
use saola_theme::style::container::DashState;
use saola_theme::{style, Theme};

/// The minimap's own message type (Stage 7's per-module refactor — see
/// `modules::clock::Message` for the full teaching note). `main.rs` nests it
/// as `Message::Columns(columns::Message)`.
///
/// The payload is the whole derived dash row rather than a single changed
/// dash: the bridge recomputes the row from its folded state after every
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

/// Minimap module state: the last dash row the niri bridge pushed through
/// [`Message::Updated`].
///
/// `Default` is the boot state — an empty row — so the module renders nothing
/// until niri actually reports a workspace. That makes "not running under
/// niri", "niri restarted and we're reconnecting", "the focused workspace is
/// empty", and "the bridge hasn't reported yet" all render identically: as
/// nothing, exactly like the D-Bus modules' `present: false`.
///
/// `PartialEq` is load-bearing, not a convenience derive: the bridge compares
/// each freshly derived row against the last one it sent and suppresses
/// duplicates (see `modules::niri`'s doc comment on title spam).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Columns {
    dashes: Vec<ColumnDash>,
}

impl Columns {
    /// Wraps a derived dash row as this module's state — the bridge's way in
    /// (`modules::niri::run_session`), and the only one.
    ///
    /// `pub(super)` rather than `pub`: the field stays private so nothing
    /// outside `modules` can hand the minimap a row it didn't derive from
    /// real niri state, while the sibling bridge — the one thing that legally
    /// *does* derive rows — can still build one without the field being open
    /// to the whole crate.
    pub(super) fn from_dashes(dashes: Vec<ColumnDash>) -> Self {
        Self { dashes }
    }

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

    /// No subscription of this module's own: the niri socket is shared with
    /// `modules::window_title`, so a single `modules::niri::subscription()`
    /// in `Panel::subscription` feeds both (see that module's doc comment for
    /// why one bridge rather than two connections).
    ///
    /// The method is kept — returning `Subscription::none()` — for the same
    /// reason `mark.rs`'s does: every module answers the same four questions,
    /// so the batch in `main.rs` stays a uniform list instead of special-
    /// casing the modules whose signal arrives by another route.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
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
/// `pub(super)` for the same reason as [`Columns::from_dashes`] — the bridge
/// derives the row, this module owns what a row may look like.
pub(super) fn build_dashes(columns: &[usize], focused: Option<usize>) -> Vec<ColumnDash> {
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

#[cfg(test)]
mod tests {
    use super::*;

    // The fold's tests moved to `modules::niri` alongside the fold itself
    // (2026-08-01); what remains here is the dash budget, which is this
    // module's own.

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

    /// The state wrapper the bridge builds rows through — an empty row is the
    /// module's "nothing to draw" state, and a non-empty one is present.
    #[test]
    fn presence_follows_the_dash_row() {
        assert!(!Columns::default().is_present());
        assert!(Columns::from_dashes(build_dashes(&[1, 2], Some(1))).is_present());
    }
}
