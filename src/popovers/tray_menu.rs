//! The tray-menu popover's content — [`crate::popover::PopoverKind::
//! TrayMenu`]'s real body, rendering Stage 20's [`Menu`]/[`MenuNode`] model
//! (`crate::modules::tray::menu`) inside the ink shell `crate::popover`
//! proved the open/close lifecycle with.
//!
//! Same split as `crate::popovers::quick_settings`: the lifecycle manager
//! (`crate::popover::PopoverManager`) stays ignorant of what a popover
//! contains, and this module owns the content plus whatever state that
//! content needs — held on [`crate::Panel`] as [`TrayMenuState`], the same
//! "a module defines the type, `Panel` holds an instance" shape
//! `Panel.volume_commands: Option<modules::volume::CommandSender>` already
//! established (`modules::volume` owns `CommandSender`'s definition; `Panel`
//! just stores it). A tray menu needs that same shape rather than reading
//! straight through an existing bar module's state (the way `quick_settings`
//! reads `Volume`/`Media` directly) because there *is* no persistent bar-
//! module state for "which tray item's menu is currently open" — that only
//! exists for the lifetime of one popover.
//!
//! # The mapping from [`MenuNode`] to widgets
//!
//! - **A row** (`MenuItemKind::Standard`) is a `button` at `sizes.list_row`
//!   height, styled by [`row_style`] below — not one of `saola_theme::
//!   style::button`'s four variants, because none of them is "quiet at rest,
//!   terracotta on hover" (see that function's doc comment for why a new one
//!   was built rather than reused, and the flagged `saola-theme` gap it
//!   leaves).
//! - **A separator** (`MenuItemKind::Separator`) is `iced::widget::rule::
//!   horizontal`, styled with `saola_theme::style::rule::rest` — the theme's
//!   one rule style, unchanged.
//! - **A toggle mark** (`toggle_type != ToggleType::None`) is a leading
//!   glyph: terracotta when `toggle_state == On` (the one rule's "on/
//!   selected/live" case, at element scale — a glyph, never a whole-row
//!   flood, per CLAUDE.md's bar-scale rule applied here to list rows), quiet
//!   otherwise. **No Lucide asset** — see "Flagged: no check/chevron
//!   icons" below. **Not a real `checkbox`/`toggler` widget**, deliberately:
//!   those widgets own their own click (`on_toggle`) and would fight the
//!   *row's* `on_press` for the same click — and would be lying anyway,
//!   since dbusmenu's toggle state is the *application's* to decide
//!   (`Event("clicked")` goes out, and the row's mark only changes once a
//!   `LayoutUpdated`/`ItemsPropertiesUpdated`-triggered re-read comes back
//!   with a new `toggle-state`). What *is* carried over from `saola_theme::
//!   style::toggles` is the color convention those widgets themselves
//!   follow (off = quiet, on = terracotta) — the glyph just paints it
//!   without a second interactive widget underneath the row.
//! - **Disabled** (`!enabled`) rows get no `.on_press` at all, so iced
//!   reports `button::Status::Disabled` on its own and [`row_style`] reads
//!   `on_ink.disabled` for it — the same "honest, unprompted disabled"
//!   pattern `quick_settings::toggle_grid` already uses.
//! - **A submenu row** (`has_submenu`) gets a trailing arrow glyph and, when
//!   expanded, its children render nested directly beneath it, indented by
//!   `sizes.icon_menu` — see "Submenu expansion" below.
//! - **`visible == false` rows are dropped**, and separators are collapsed
//!   so no leading/trailing/doubled one survives — [`rows_to_draw`] is where
//!   Stage 20's handoff left this decision for Stage 21 to make.
//!
//! # Submenu expansion: inline, not drill-in
//!
//! Clicking a `has_submenu` row toggles it into [`TrayMenuState::expanded`]
//! and its children render nested in place — no second popover, no
//! "breadcrumb" replace-the-view navigation. That full drill-in UX (a
//! back button, a view that swaps out for the submenu's own) is explicitly
//! **not** built here; PLAN.md's own Stage 21 text names it future work.
//! [`rows_view`] happens to recurse generically rather than stopping after
//! exactly one level — restricting that artificially would be extra code to
//! prevent a case Stage 20's own depth-cap note says barely occurs in the
//! wild ("real menus are two or three levels"). Reading "inline, one level"
//! as "inline rather than drill-in" (not "capped at exactly one nesting
//! level") is a deliberate interpretation, flagged in the Stage 21 handoff.
//!
//! Because `GetLayout` is always called with `recursion_depth: -1`
//! (`modules::tray::menu`'s doc comment), the overwhelming majority of
//! submenus already have their children by the time the row is drawn —
//! expanding them needs no second D-Bus call at all.
//! [`TrayMenuState::toggle_expanded`] only asks for one (a fresh
//! `read_menu(item_id, row_id)`, `Panel::update`'s job to issue) when a row
//! declares `has_submenu` with **empty** `children` — the lazily-populated
//! case Stage 20's handoff flagged (`AboutToShow`, then nothing, until
//! asked) and that this codebase has never actually observed against a real
//! application.
//!
//! # Flagged: no check/chevron icons
//!
//! The toggle mark and the submenu arrow are plain Unicode glyphs (`"✓"`,
//! `"●"`, `"›"`) drawn with `iced::widget::text`, not `crate::icons`' tinted
//! Lucide SVGs — there is no `Check`/`ChevronRight` asset embedded in this
//! crate, and this stage's tool list excludes WebFetch/WebSearch, so no new
//! Lucide SVG could be fetched. Exactly the same honest-scope call Stage
//! 17's handoff made for the quick-settings placeholder toggles ("no new
//! icons were added... all four render as plain centered text labels").
//! Adding real icons later is a Stage 8-style asset addition, not a
//! rendering change here.
//!
//! # Flagged: no dedicated `saola-theme` tokens for this popover
//!
//! - **The row hover/press style** ([`row_style`]) is built by hand from the
//!   same tokens `style::button::bare`/`active` read
//!   (`palette.accent`/`palette.paper`/`on_ink.primary`/`on_ink.disabled`),
//!   because neither of those two helpers is "quiet at rest, terracotta on
//!   hover only" — `bare` never turns terracotta, `active` is terracotta at
//!   rest. Per CLAUDE.md ("if a needed style doesn't exist, add it to
//!   saola-theme — don't restyle locally") this is a candidate for
//!   promotion (`style::button::selectable`, say) rather than a permanent
//!   local fixture — flagged, not promoted, since this is its only
//!   consumer today.
//! - **The separator's reserved vertical space** reuses `sizes.island_gap`
//!   (10, the closest existing "gap between things" token) rather than a
//!   dedicated `sizes.popover_separator_gap` — [`separator`]'s doc comment.
//! - **Row horizontal inset** reuses `sizes.pill_gap` (8) — the same
//!   "closest existing token, not a bare literal" compromise
//!   `quick_settings`'s own flagged-gaps section documents for its content
//!   padding.
//!
//! # Flagged: the popover's height is a fixed row budget, not an estimate
//!
//! See [`height`]'s doc comment — worse than `quick_settings::height`'s
//! "declared estimate" problem, because the async fetch that would tell us
//! how many rows there actually are has not even started by the time the
//! surface's size must be requested.
//!
//! # Flagged: horizontal placement doesn't track which icon was clicked
//!
//! Every popover — quick settings and every tray menu alike — anchors at
//! the same fixed margin from the panel's own edge (`SurfaceGeometry::of`'s
//! popover arm, unchanged since Stage 16). A tray menu opened from the
//! *first* tray icon and one opened from the *last* therefore appear at
//! the exact same screen position, never actually under the item that was
//! right-clicked. Exact anchor-to-widget positioning would need the
//! measurement iced 0.14 cannot do (Stage 15's finding, restated in every
//! popover stage since); PLAN.md's own Stage 21 text accepts this
//! approximation explicitly ("not worth fighting for in v0.2") rather than
//! asking for it, so this is accepted, not merely deferred.

use std::collections::HashSet;

use iced::futures::channel::mpsc;
use iced::futures::stream::StreamExt;
use iced::futures::{SinkExt, Stream};
use iced::widget::button::Status as ButtonStatus;
use iced::widget::{button, column, container, row, rule, text};
use iced::{Background, Border, Color, Element, Fill, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};
use zbus::Connection;

use crate::modules::tray::menu::{self, Menu, MenuItemKind, MenuNode, ToggleState, ToggleType};

/// The tray-menu popover's own messages — nested as `Message::TrayMenu(..)`
/// in `main.rs`'s panel-level enum, the same shape every bar module's
/// `Message` follows (`modules::clock::Message`'s doc comment has the full
/// teaching note). Unlike a bar module's, none of these come from a
/// `subscription()` snapshot feed *by default* — three come from user
/// interaction inside the popover, and [`Self::Loaded`]/[`Self::
/// SubmenuLoaded`] come from the one-shot `Task`s `Panel::open_tray_menu`
/// and its siblings kick off (plus, after the fact, the live-refresh
/// subscription [`watch`] starts once a menu is open).
#[derive(Debug, Clone)]
pub enum Message {
    /// `modules::tray::menu::read_menu(item_id, 0)` answered. Carries the
    /// `item_id` it was fetched *for* — `Panel::update`'s arm applies it only
    /// if that still matches [`TrayMenuState::item_id`], so a fetch that
    /// lands after the user closed the popover (or right-clicked a different
    /// item before this one answered) is silently discarded rather than
    /// clobbering newer state. `None` is a failed read (no session bus, no
    /// `Menu` property, ...) — the popover shows nothing rather than stale
    /// content.
    Loaded(String, Option<Menu>),
    /// A leaf row (no submenu) was clicked. `Panel::update` sends
    /// `Event(id, "clicked")` to the application and closes the popover —
    /// PLAN.md Stage 21's "click a leaf → `Event(\"clicked\")` + close".
    RowActivated(i32),
    /// A row that opens a submenu was clicked. `Panel::update` flips it in
    /// [`TrayMenuState::expanded`] via [`TrayMenuState::toggle_expanded`],
    /// which also says whether a lazy fetch is needed — see this module's
    /// "Submenu expansion" doc section.
    ToggleSubmenu(i32),
    /// The lazy fetch [`TrayMenuState::toggle_expanded`] asked for answered.
    /// Carries the row's id (not the item id — [`Loaded`]'s race guard isn't
    /// needed here, since a stale graft onto a tree that's since been
    /// replaced entirely is already a no-op — see [`TrayMenuState::
    /// merge_submenu`]).
    SubmenuLoaded(i32, Option<Menu>),
}

/// Everything the tray-menu popover needs beyond [`Menu`] itself: *which*
/// item's menu this is (the registration string `send_clicked`/`read_menu`
/// need to reach the right bus name/path), the last tree read for it, and
/// which submenu rows are expanded inline. See the module doc comment for
/// why this lives on `Panel` as a value of a type this module defines,
/// rather than folding into `modules::tray::Tray` (the bar's own rendering
/// state, which has nothing to do with a popover that may never open).
#[derive(Debug, Default)]
pub struct TrayMenuState {
    item_id: Option<String>,
    menu: Option<Menu>,
    expanded: HashSet<i32>,
}

impl TrayMenuState {
    /// A fresh "requested, not answered yet" state for `item_id`.
    /// `Panel::open_tray_menu` sets this the instant a right-click is seen —
    /// before the async fetch it kicks off has even started — so [`view`]
    /// has something to render (a quiet "Loading…" line) on the very first
    /// frame the popover surface exists, rather than nothing at all.
    pub fn opening(item_id: String) -> Self {
        Self {
            item_id: Some(item_id),
            menu: None,
            expanded: HashSet::new(),
        }
    }

    /// The tray item this menu belongs to, or `None` when no tray menu is
    /// open (or being opened) right now.
    pub fn item_id(&self) -> Option<&str> {
        self.item_id.as_deref()
    }

    /// The last tree read for [`Self::item_id`], or `None` while the fetch
    /// is still in flight (or failed). Exposed alongside `item_id` so
    /// `main.rs`'s `Panel`-level tests can assert on fetch results without
    /// reaching into a private field.
    pub fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }

    /// Whether `node_id` is currently expanded inline.
    pub fn is_expanded(&self, node_id: i32) -> bool {
        self.expanded.contains(&node_id)
    }

    /// Store a freshly-read (or refreshed) menu tree, replacing whatever was
    /// there. Also clears [`Self::expanded`]: dbusmenu ids "are not stable
    /// across a `LayoutUpdated`" (`modules::tray::menu`'s own doc comment),
    /// so an expanded-row id from the *previous* tree could otherwise
    /// coincidentally name an unrelated row in the new one. This only
    /// matters for a live refresh (the initial `opening` call already starts
    /// with an empty set) — a genuinely rare event this codebase has never
    /// observed against a real application, but cheap to get right anyway.
    pub fn set_menu(&mut self, menu: Option<Menu>) {
        self.menu = menu;
        self.expanded.clear();
    }

    /// Flip one submenu row's expanded/collapsed state.
    ///
    /// Returns whether the caller now needs to fetch that row's children:
    /// **only** when the row was just expanded and its `children` are still
    /// empty — see the module doc comment's "Submenu expansion" section.
    /// Collapsing never needs a fetch, and neither does expanding a row
    /// whose children already arrived with the initial (`recursion_depth:
    /// -1`) read.
    pub fn toggle_expanded(&mut self, node_id: i32) -> bool {
        if self.expanded.remove(&node_id) {
            return false;
        }
        self.expanded.insert(node_id);
        self.menu
            .as_ref()
            .and_then(|menu| find_node(&menu.root, node_id))
            .is_some_and(|node| node.children.is_empty())
    }

    /// Graft a lazily-fetched subtree onto the row that requested it.
    ///
    /// `fetched`'s `root` **is** that row — `GetLayout(parentId, ..)`
    /// replies with a node whose own id is `parentId`
    /// (`modules::tray::menu`'s doc comment) — so only its `children` are
    /// grafted on; the row's own properties (label, enabled, ...) keep
    /// whatever the original tree already said, which is also whatever the
    /// user is currently looking at, so there is nothing to visibly change
    /// about the row itself.
    ///
    /// A `None` result, or a `node_id` no longer present in the tree (the
    /// menu was replaced by a refresh in the meantime), is dropped silently:
    /// the row simply stays expanded-but-empty, the same quiet-failure
    /// contract as every other D-Bus read in this panel.
    pub fn merge_submenu(&mut self, node_id: i32, fetched: Option<Menu>) {
        let Some(fetched) = fetched else {
            return;
        };
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        if let Some(node) = find_node_mut(&mut menu.root, node_id) {
            node.children = fetched.root.children;
        }
    }
}

fn find_node(node: &MenuNode, id: i32) -> Option<&MenuNode> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, id))
}

fn find_node_mut(node: &mut MenuNode, id: i32) -> Option<&mut MenuNode> {
    if node.id == id {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, id))
}

/// How many rows the popover surface budgets space for — a fixed count, not
/// a per-menu estimate.
///
/// A wlr-layer-shell surface must declare its size *before* the compositor
/// creates it, which happens the instant the right-click is seen — before
/// the async `read_menu` fetch that would tell us how many rows there
/// actually are has even started, let alone answered. There is no honest
/// way to answer "how tall is this menu" at the moment the question has to
/// be answered, which is a strictly harder version of the problem
/// `quick_settings::height`'s doc comment already describes (that popover's
/// *content* is fixed at compile time; this one's is not known until
/// runtime, and not even then until a round-trip completes).
///
/// So this is a generous fixed budget, chosen to comfortably hold the one
/// real menu this codebase has ever captured (`modules::tray::menu`'s
/// tailscale fixture: 9 rows, two of them separators) with headroom for
/// deeper real-world menus. **The honest limitation**: a menu with more
/// visible rows than this budget has its later rows pushed below the
/// popover's visible/clickable area — a layer-shell surface cannot draw
/// outside its own declared bounds, so unlike `quick_settings::height`'s
/// mismatch (a sliver of blank ink at the bottom, never missing content)
/// this is genuinely lost content, not merely imprecise. Flagged in the
/// Stage 21 handoff. A real fix needs either a scrollable row list backed
/// by a `saola-theme` maximum-menu-height token, or resizing the surface
/// after the fetch lands (`AnchorSizeChange`/`SizeChange` — already
/// available, injected by `#[to_layer_message(multi)]`, see `main.rs`'s
/// `Message` doc comment — but not wired here: a popover changing size
/// after it opens is its own interaction question this stage does not
/// take on).
const BUDGETED_ROWS: f32 = 12.0;

/// The popover surface's declared height — see [`BUDGETED_ROWS`] for why
/// this is a fixed budget rather than an estimate of one specific menu's
/// content.
pub fn height(theme: &Theme) -> f32 {
    theme.sizes.panel_margin_ledger * 2.0 + theme.sizes.list_row * BUDGETED_ROWS
}

/// The whole popover body: the fetched menu's rows, or a quiet placeholder
/// while the fetch is still in flight (or failed).
pub fn view<'a>(theme: &Theme, state: &'a TrayMenuState) -> Element<'a, crate::Message> {
    let content: Element<'a, crate::Message> = match state.menu() {
        Some(menu) => rows_view(theme, &menu.root.children, state),
        None => container(
            text("Loading…")
                .size(theme.typography.size.bar)
                .color(theme.on_ink.secondary.into_iced()),
        )
        .height(theme.sizes.list_row)
        .align_y(iced::Center)
        .into(),
    };

    container(content)
        .style(style::container::popover(theme))
        .padding(theme.sizes.panel_margin_ledger)
        .width(Fill)
        .height(Fill)
        .into()
}

/// Filter and collapse a raw children list into what should actually be
/// drawn — the decision Stage 20's handoff explicitly left open ("what to
/// do with the separators either side of a hidden row (and with leading/
/// trailing/doubled separators generally)").
///
/// The rule this settles on: invisible rows are dropped outright
/// (`MenuNode::visible == false` means "do not draw", per that field's own
/// doc comment), and separators are collapsed so that no leading or
/// trailing separator survives and no two consecutive drawn rows are both
/// separators — the shape a hidden row leaves behind when it sat next to
/// one. Pure function; unit-tested below, including against a shape that
/// mirrors the real captured tailscale menu (an invisible row between a
/// visible one and a separator).
fn rows_to_draw(nodes: &[MenuNode]) -> Vec<&MenuNode> {
    let mut rows: Vec<&MenuNode> = Vec::new();

    for node in nodes.iter().filter(|node| node.visible) {
        let is_separator = node.kind == MenuItemKind::Separator;
        let previous_is_separator = rows
            .last()
            .is_some_and(|row| row.kind == MenuItemKind::Separator);
        if is_separator && (rows.is_empty() || previous_is_separator) {
            continue;
        }
        rows.push(node);
    }

    if rows
        .last()
        .is_some_and(|row| row.kind == MenuItemKind::Separator)
    {
        rows.pop();
    }

    rows
}

/// Render one level of rows (and, recursively, the children of whichever of
/// them are expanded submenus) — see the module doc comment's "Submenu
/// expansion" section for why this recurses generically rather than
/// stopping after one level.
fn rows_view<'a>(
    theme: &Theme,
    nodes: &'a [MenuNode],
    state: &TrayMenuState,
) -> Element<'a, crate::Message> {
    let mut items: Vec<Element<'a, crate::Message>> = Vec::new();

    for node in rows_to_draw(nodes) {
        match node.kind {
            MenuItemKind::Separator => items.push(separator(theme)),
            MenuItemKind::Standard => {
                let is_expanded = state.is_expanded(node.id);
                items.push(row_view(theme, node, is_expanded));
                if node.has_submenu && is_expanded && !node.children.is_empty() {
                    items.push(
                        container(rows_view(theme, &node.children, state))
                            .padding(iced::padding::left(theme.sizes.icon_menu))
                            .into(),
                    );
                }
            }
        }
    }

    column(items).into()
}

/// A divider row. Reserves `sizes.island_gap` of vertical space (see the
/// module doc comment's flagged-gaps section) around a hairline drawn at
/// full width — `style::rule::rest` is the theme's one rule style, applied
/// unchanged.
fn separator<'a>(theme: &Theme) -> Element<'a, crate::Message> {
    container(rule::horizontal(1.0).style(style::rule::rest(theme, Surface::Ink)))
        .padding(iced::padding::vertical(theme.sizes.island_gap / 2.0))
        .width(Fill)
        .into()
}

/// One clickable (or disabled) row: an optional leading toggle mark, the
/// label, and an optional trailing submenu arrow, wrapped in a `button`
/// styled by [`row_style`].
fn row_view<'a>(theme: &Theme, node: &'a MenuNode, expanded: bool) -> Element<'a, crate::Message> {
    let text_size = theme.typography.size.bar;
    let gap = theme.sizes.bar_icon_gap;

    let mut children: Vec<Element<'a, crate::Message>> = Vec::new();

    if node.toggle_type != ToggleType::None {
        let on = node.toggle_state == ToggleState::On;
        let glyph = if on {
            match node.toggle_type {
                ToggleType::Radio => "●",
                _ => "✓",
            }
        } else {
            ""
        };
        let color = if on {
            theme.palette.accent
        } else {
            theme.on_ink.secondary
        };
        children.push(text(glyph).size(text_size).color(color.into_iced()).into());
    }

    let label_color = if node.enabled {
        theme.on_ink.primary
    } else {
        theme.on_ink.disabled
    };
    children.push(
        text(node.label.clone())
            .size(text_size)
            .color(label_color.into_iced())
            .width(Fill)
            .into(),
    );

    if node.has_submenu {
        children.push(
            text(if expanded { "⌄" } else { "›" })
                .size(text_size)
                .color(theme.on_ink.secondary.into_iced())
                .into(),
        );
    }

    let content = row(children).spacing(gap).align_y(iced::Center);

    let element = button(content)
        .width(Fill)
        .height(theme.sizes.list_row)
        .padding(iced::padding::horizontal(theme.sizes.pill_gap))
        .style(row_style(theme, node.enabled));

    if node.enabled {
        let message = if node.has_submenu {
            crate::Message::TrayMenu(Message::ToggleSubmenu(node.id))
        } else {
            crate::Message::TrayMenu(Message::RowActivated(node.id))
        };
        element.on_press(message).into()
    } else {
        element.into()
    }
}

/// A menu row's own style: quiet (no fill) at rest, terracotta with an
/// ivory label on hover/press — the one rule's "selected" case, read
/// literally for a list row (the item the pointer is over is the one about
/// to be activated). Disabled rows never receive a hover/press status at
/// all (no `.on_press` means iced reports `Status::Disabled`
/// unconditionally — verified against `iced_widget-0.14.2/src/button.rs`,
/// the same fact `quick_settings::toggle_grid` already leans on), so the
/// `enabled` parameter only has to pick the *resting* label color.
///
/// Not one of `saola_theme::style::button`'s four variants: `bare` never
/// turns terracotta on hover, and `active` is terracotta *at rest* (an
/// always-on state, not a transient one), so neither expresses "quiet
/// until hovered". Built directly from the same token fields those helpers
/// read (`palette.accent`, `palette.paper`, `on_ink.primary`/`.disabled`) —
/// per CLAUDE.md, this is a flagged `saola-theme` candidate
/// (`style::button::selectable`, say), not a permanent local fixture; see
/// the module doc comment's flagged-gaps section.
///
/// Built fresh per row (not hoisted and shared) for the same reason
/// `quick_settings.rs`'s `cell`/`transport` closures are — see that
/// module's gotcha 1: the returned `impl Fn` closure has no `Copy`/`Clone`
/// impl, and `button::style` takes it by value, so a single instance
/// couldn't be reused across a list of rows anyway.
fn row_style(theme: &Theme, enabled: bool) -> impl Fn(&iced::Theme, ButtonStatus) -> button::Style {
    let radius = theme.radii.selection;
    let accent = theme.palette.accent.into_iced();
    let on_accent = theme.palette.paper.into_iced();
    let rest_color = if enabled {
        theme.on_ink.primary.into_iced()
    } else {
        theme.on_ink.disabled.into_iced()
    };

    move |_, status| {
        let (background, text_color) = match status {
            ButtonStatus::Hovered | ButtonStatus::Pressed => (Some(accent), on_accent),
            ButtonStatus::Active | ButtonStatus::Disabled => (None, rest_color),
        };
        button::Style {
            background: background.map(Background::Color),
            text_color,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: radius.into(),
            },
            ..button::Style::default()
        }
    }
}

/// The tray-menu popover's live-refresh subscription: `modules::tray::
/// menu::watch_menu`'s `LayoutUpdated`/`ItemsPropertiesUpdated` stream,
/// wired for real (Stage 20 left it `#[allow(dead_code)]`).
///
/// `Subscription::run_with(item_id, ..)` rather than `Subscription::run` —
/// the fn-pointer-identity subscriptions every other module uses
/// (`battery.rs`'s teaching note) all watch one fixed service for the whole
/// life of the process, but *which* menu to watch here changes at runtime
/// (whichever tray item's popover is currently open, or none). `run_with`
/// keys the subscription's identity on **both** the function pointer *and*
/// `item_id`'s value, so `Panel::subscription` can call this with whatever
/// `self.tray_menu.item_id()` currently is on every frame: iced diffs the
/// returned identity against the previous frame's and tears down/spins up
/// the underlying stream exactly when the item changes (including "was
/// `Some`, now `None`" via the `Subscription::none()` fallback
/// `Panel::subscription` uses when nothing is open) — never continuously
/// polling, satisfying CLAUDE.md's "every module maps to a signal, never a
/// poll" the same way every other subscription in this crate does.
pub fn watch(item_id: &str) -> Subscription<crate::Message> {
    Subscription::run_with(item_id.to_string(), stream_for)
}

/// The stream `watch`'s subscription runs — `fn(&String) -> impl Stream`,
/// the shape `Subscription::run_with` requires (a bare fn, part of the
/// subscription's identity, so it cannot capture — same constraint
/// `popover::subscription`'s `listen_with` filter documents).
///
/// The `&String` parameter (clippy would rather see `&str`) is not a
/// stylistic slip: [`Subscription::run_with`]'s `builder` parameter is
/// `fn(&D) -> S`, and `D` here is `String` (the type [`watch`] hands
/// `run_with` as `data`) — a `fn(&str) -> S` is a different, non-matching
/// type, so `&str` would not compile.
#[allow(
    clippy::ptr_arg,
    reason = "must match Subscription::run_with's fn(&D) -> S exactly, where D = String"
)]
fn stream_for(item_id: &String) -> impl Stream<Item = crate::Message> {
    let item_id = item_id.clone();
    iced::stream::channel(1, async move |mut sender: mpsc::Sender<crate::Message>| {
        let Ok(connection) = Connection::session().await else {
            return;
        };
        let mut changes = menu::watch_menu(connection, &item_id).await;

        // `watch_menu`'s own doc comment: both signals collapse to `()` —
        // re-read everything on either. One round-trip per refresh, the
        // same "re-read everything" idiom `battery.rs`'s `watch_upower`
        // already uses.
        while changes.next().await.is_some() {
            let fetched = menu::read_menu(&item_id, 0).await;
            let message = crate::Message::TrayMenu(Message::Loaded(item_id.clone(), fetched));
            if sender.send(message).await.is_err() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tray::menu::MenuNode;

    fn node(id: i32) -> MenuNode {
        MenuNode {
            id,
            ..MenuNode::default()
        }
    }

    fn separator_node(id: i32) -> MenuNode {
        MenuNode {
            id,
            kind: MenuItemKind::Separator,
            ..MenuNode::default()
        }
    }

    fn invisible(mut n: MenuNode) -> MenuNode {
        n.visible = false;
        n
    }

    #[test]
    fn height_is_positive_and_derived_from_tokens() {
        let theme = Theme::saola();
        assert!(height(&theme) > 0.0);
    }

    #[test]
    fn view_renders_without_panicking_with_no_menu_yet() {
        let theme = Theme::saola();
        let state = TrayMenuState::opening("dummy".to_string());
        let _: Element<'_, crate::Message> = view(&theme, &state);
    }

    #[test]
    fn view_renders_without_panicking_with_a_real_tree() {
        let theme = Theme::saola();
        let mut state = TrayMenuState::opening("dummy".to_string());
        state.set_menu(Some(Menu {
            revision: 0,
            root: MenuNode {
                id: 0,
                children: vec![
                    MenuNode {
                        toggle_type: ToggleType::Checkmark,
                        toggle_state: ToggleState::On,
                        label: "Show notifications".to_string(),
                        ..node(1)
                    },
                    separator_node(2),
                    MenuNode {
                        label: "More".to_string(),
                        has_submenu: true,
                        children: vec![MenuNode {
                            label: "Nested".to_string(),
                            ..node(4)
                        }],
                        ..node(3)
                    },
                    MenuNode {
                        label: "Quit".to_string(),
                        enabled: false,
                        ..node(5)
                    },
                ],
                ..node(0)
            },
        }));
        state.expanded.insert(3);
        let _: Element<'_, crate::Message> = view(&theme, &state);
    }

    #[test]
    fn rows_to_draw_keeps_visible_rows_in_order() {
        let nodes = vec![node(1), node(2), node(3)];
        let rows = rows_to_draw(&nodes);
        assert_eq!(rows.iter().map(|n| n.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn rows_to_draw_drops_invisible_rows() {
        let nodes = vec![node(1), invisible(node(2)), node(3)];
        let rows = rows_to_draw(&nodes);
        assert_eq!(rows.iter().map(|n| n.id).collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn rows_to_draw_drops_a_leading_separator() {
        let nodes = vec![separator_node(1), node(2)];
        let rows = rows_to_draw(&nodes);
        assert_eq!(rows.iter().map(|n| n.id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn rows_to_draw_drops_a_trailing_separator() {
        let nodes = vec![node(1), separator_node(2)];
        let rows = rows_to_draw(&nodes);
        assert_eq!(rows.iter().map(|n| n.id).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn rows_to_draw_collapses_doubled_separators() {
        let nodes = vec![node(1), separator_node(2), separator_node(3), node(4)];
        let rows = rows_to_draw(&nodes);
        assert_eq!(
            rows.iter().map(|n| (n.id, n.kind)).collect::<Vec<_>>(),
            vec![
                (1, MenuItemKind::Standard),
                (2, MenuItemKind::Separator),
                (4, MenuItemKind::Standard)
            ]
        );
    }

    /// The shape of the real captured tailscale menu (Stage 20's fixture):
    /// an invisible row sitting between a visible row and a separator. Once
    /// the invisible row is dropped, the separator must not become doubled
    /// or orphaned.
    #[test]
    fn rows_to_draw_handles_an_invisible_row_beside_a_separator() {
        let nodes = vec![node(1), invisible(node(2)), separator_node(3), node(4)];
        let rows = rows_to_draw(&nodes);
        assert_eq!(rows.iter().map(|n| n.id).collect::<Vec<_>>(), vec![1, 3, 4]);
    }

    #[test]
    fn tray_menu_state_starts_closed() {
        let state = TrayMenuState::default();
        assert_eq!(state.item_id(), None);
        assert!(state.menu.is_none());
    }

    #[test]
    fn opening_records_the_item_and_clears_any_previous_menu() {
        let state = TrayMenuState::opening("item-a".to_string());
        assert_eq!(state.item_id(), Some("item-a"));
        assert!(state.menu.is_none());
        assert!(state.expanded.is_empty());
    }

    #[test]
    fn set_menu_clears_expanded_rows() {
        let mut state = TrayMenuState::opening("item-a".to_string());
        state.expanded.insert(7);
        state.set_menu(Some(Menu::default()));
        assert!(state.expanded.is_empty());
    }

    #[test]
    fn toggle_expanded_flips_and_needs_no_fetch_when_children_already_present() {
        let mut state = TrayMenuState::opening("item-a".to_string());
        state.set_menu(Some(Menu {
            revision: 0,
            root: MenuNode {
                children: vec![MenuNode {
                    has_submenu: true,
                    children: vec![node(99)],
                    ..node(1)
                }],
                ..node(0)
            },
        }));

        assert!(!state.toggle_expanded(1), "children are already present");
        assert!(state.expanded.contains(&1));

        assert!(!state.toggle_expanded(1), "collapsing never needs a fetch");
        assert!(!state.expanded.contains(&1));
    }

    #[test]
    fn toggle_expanded_asks_for_a_fetch_when_children_are_empty() {
        let mut state = TrayMenuState::opening("item-a".to_string());
        state.set_menu(Some(Menu {
            revision: 0,
            root: MenuNode {
                children: vec![MenuNode {
                    has_submenu: true,
                    ..node(1)
                }],
                ..node(0)
            },
        }));

        assert!(
            state.toggle_expanded(1),
            "a submenu row with no children yet is the lazily-populated case"
        );
    }

    #[test]
    fn merge_submenu_grafts_children_onto_the_matching_row() {
        let mut state = TrayMenuState::opening("item-a".to_string());
        state.set_menu(Some(Menu {
            revision: 0,
            root: MenuNode {
                children: vec![MenuNode {
                    has_submenu: true,
                    ..node(1)
                }],
                ..node(0)
            },
        }));

        state.merge_submenu(
            1,
            Some(Menu {
                revision: 0,
                root: MenuNode {
                    children: vec![node(2)],
                    ..node(1)
                },
            }),
        );

        let grafted = find_node(&state.menu.as_ref().unwrap().root, 1).expect("row 1 exists");
        assert_eq!(grafted.children.len(), 1);
        assert_eq!(grafted.children[0].id, 2);
    }

    #[test]
    fn merge_submenu_ignores_a_failed_fetch() {
        let mut state = TrayMenuState::opening("item-a".to_string());
        state.set_menu(Some(Menu::default()));

        state.merge_submenu(1, None);

        assert_eq!(state.menu, Some(Menu::default()));
    }

    #[test]
    fn find_node_locates_a_nested_child() {
        let tree = MenuNode {
            children: vec![MenuNode {
                children: vec![node(5)],
                ..node(2)
            }],
            ..node(0)
        };
        assert_eq!(find_node(&tree, 5).map(|n| n.id), Some(5));
        assert_eq!(find_node(&tree, 42), None);
    }
}
