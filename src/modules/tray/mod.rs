//! The system tray (StatusNotifierItem), rendered on the bar.
//!
//! # This module is a **directory** — the one sanctioned deviation
//!
//! Every other bar module is one file (`modules/battery.rs`,
//! `modules/volume.rs`, ...), and `modules/mod.rs` says so. The tray is the
//! exception PLAN.md carves out, because it is not one thing:
//!
//! - `watcher.rs` — the D-Bus half: **serving** `org.kde.
//!   StatusNotifierWatcher` when nothing else does, consuming it when
//!   something already does, host registration, and the worker that keeps
//!   the registry in step with the bus.
//! - `item.rs` — the item model: the registration-string quirk (parsing
//!   *and* normalizing), the item registry, and the per-item proxy.
//! - `mod.rs` (this file) — the **standard module surface** every other
//!   module exposes from a single file: the state struct [`Tray`], its
//!   [`Message`], [`Tray::view`] and [`Tray::subscription`]. Nothing outside
//!   `modules::tray` sees the split; `main.rs` wires this module exactly
//!   like `battery` or `media`, and `modules::tray::Tray` is the only name it
//!   needs.
//!
//! Stage 20 grows the directory further (the dbusmenu model, then Stage 21's
//! menus). Splitting it now, rather than letting one file reach four figures
//! of lines, is the whole point of the deviation — and the same shape
//! `src/popovers/` already uses for popover content.
//!
//! # What the tray *is*, in one paragraph
//!
//! An application that wants a tray icon publishes an
//! `org.kde.StatusNotifierItem` object and tells the session's *watcher*
//! about it. The watcher is a registry: it keeps the list, announces
//! additions and removals, and tells items whether anyone is displaying them.
//! A *host* is a thing that displays them — this panel. On a full desktop
//! the shell is both watcher and host; on a bare niri session there is no
//! watcher at all, so the panel has to be prepared to be both. Read
//! `watcher.rs`'s module doc comment for the decision tree; read `item.rs`'s
//! for the registration-string quirk that is the protocol's most famous
//! wart, and for the icon-source precedence Stage 19 adds.
//!
//! # Design language
//!
//! Stage 18's title pills are gone. Each item is now its **real icon,
//! bare on ink** — no pill fill, no container — per the concept's
//! element-scale rule (CLAUDE.md: "status modules are bare ivory icon +
//! text directly on ink"). An item whose icon couldn't be resolved at all
//! (see [`item::TrayIcon`]'s doc comment) falls back to its label as plain
//! `on_ink.primary` text, the same bare treatment `battery.rs`/`network.rs`
//! give their own readouts.
//!
//! **Icons are never tinted.** This is the one place in the panel that
//! deliberately breaks from `crate::icons`' tint-everything convention:
//! those are this codebase's own Lucide glyphs, recolored to an ivory/
//! terracotta role on purpose, but a tray icon is a *third-party
//! application's own identity* — Slack's icon has to look like Slack's
//! icon. So `Tray::view` draws every resolved icon with whatever colors its
//! source asset defines, full stop.
//!
//! **`Status` maps to presentation, never a fourth color** (PLAN.md Stage
//! 19): `Passive` items are filtered out of the row entirely — the spec's
//! own words are "likely that visualizations will choose to hide it", and
//! hiding is also the quietest presentation there is, needing no new visual
//! language at all. `Active` (and an item that never answers `Status`,
//! which defaults to `Active` — see [`item::ItemStatus`]) draws with no
//! special treatment. `NeedsAttention` gets a thin 2px terracotta ring
//! around its icon — the same "a ring, not a flood, no fourth color, no
//! animation" idiom `saola_theme::style::container::card_urgent` uses for
//! the urgent notification card, at bar-icon scale. That ring is a
//! one-off closure in this file, not a `saola-theme` addition: nothing else
//! in the panel needs a bar-icon-scaled ring yet, so promoting it to the
//! shared crate is deferred until a second consumer shows up (flagged in
//! the Stage 19 handoff).

mod item;
// `pub(crate)`, not `mod`: Stage 21's popover content (`popovers::tray_menu`)
// and `main.rs` both need `menu::{Menu, MenuNode, ...}` and the three entry
// points (`read_menu`/`send_clicked`/`watch_menu`) from outside this
// directory — the one exception to "nothing outside `modules::tray` sees the
// split" the module doc comment above describes, made because the dbusmenu
// model is exactly what a popover renders, the same way `modules::volume`
// exposes `CommandSender` for `Panel` to hold.
pub(crate) mod menu;
mod watcher;

use iced::mouse::ScrollDelta;
use iced::widget::text::Wrapping;
use iced::widget::{container, image, mouse_area, row, svg, text, Space};
use iced::{Border, Element, Subscription, Task};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;

use item::{ItemStatus, TrayIcon, TrayItem};

/// The tray module's own message type (Stage 7's per-module refactor — see
/// `modules::clock::Message` for the teaching note). `main.rs` nests this as
/// `Message::Tray(tray::Message)`.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Tray),
    /// Left-click on one item's icon — SNI's primary interaction. Carries
    /// the *target* item's registration string, read off `TrayItem::id` at
    /// the moment the icon was drawn — the same "resolve the target from a
    /// string captured at draw time" shape as `media::Message::PlayPause`'s
    /// bus name. `Panel::update` turns this into [`activate`]'s one-shot
    /// D-Bus call.
    Activate(String),
    /// A scroll over one item's icon — SNI's `Scroll` method. Carries the
    /// item's id plus the wheel event iced itself reported;
    /// [`scroll_units`] is what reduces that to the `(delta, orientation)`
    /// pair the D-Bus call wants, called from [`scroll`] rather than here so
    /// the reduction stays a plain, unit-tested function.
    Scroll(String, ScrollDelta),
    /// A right-click on one item's icon. **Stage 21's hook**: the dbusmenu
    /// popover Stage 20/21 build lands here. This stage only wires the
    /// *event* — see `Panel::update`'s arm for the explicit, documented
    /// no-op.
    ContextMenu(String),
}

/// Tray module state: every registered item, in registration order.
///
/// `Default` — an empty list — is the boot state *and* the "no watcher
/// relationship" state *and* the "nothing has registered" state, all of
/// which render identically: as nothing. That is the same
/// quiet-until-proven-otherwise contract as every other module, and it is
/// what makes a session with no tray applications (or no session bus at all)
/// cost the bar exactly zero pixels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tray {
    items: Vec<TrayItem>,
}

impl Tray {
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Reads the same filter `view`'s early return does (a `Tray`
    /// whose every item is `Passive` counts as absent, same as an empty
    /// one), so the two cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.status() != ItemStatus::Passive)
    }

    /// One bare icon per visible item (see the module doc comment for the
    /// full design-language rationale: no pill, no tint, `Status`-driven
    /// presentation).
    ///
    /// `row(iterator)` rather than the `row![]` macro because the number of
    /// children varies at runtime — the same reason `Panel::region` uses the
    /// function form. `Passive` items are filtered out before the row is
    /// even built, per the spec's own "likely ... hidden" wording — see
    /// [`item::ItemStatus`]'s doc comment.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        let visible: Vec<&TrayItem> = self
            .items
            .iter()
            .filter(|item| item.status() != ItemStatus::Passive)
            .collect();

        if visible.is_empty() {
            return Space::new().into();
        }

        row(visible.into_iter().map(|item| item_element(theme, item)))
            // Items are separate elements along the bar, not parts of one
            // readout: `bar_element_gap`, the same gap the bar's regions put
            // between neighbouring modules.
            .spacing(theme.sizes.bar_element_gap)
            .align_y(iced::Center)
            .into()
    }

    /// The tray's D-Bus feed as an iced subscription. See `battery.rs`'s
    /// `subscription` for the function-pointer-identity teaching note —
    /// identical reasoning applies verbatim, and it matters more here than
    /// anywhere else in the panel: a second copy of this worker would mean a
    /// second attempt to own `org.kde.StatusNotifierWatcher`.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(watcher::tray_stream)
    }
}

/// One item's bare-on-ink presentation, plus its interactions.
///
/// `mouse_area` rather than a `button` — same reasoning as
/// `Panel::popover_trigger`: this sits directly on the ink surface with no
/// fill, and a `button` would draw a background/hover-step/focus ring that
/// don't belong here. `.interaction(Pointer)` is the one hover affordance
/// this gets instead — a cursor change, not a visual change to the icon
/// itself.
fn item_element<'a>(theme: &Theme, item: &'a TrayItem) -> Element<'a, Message> {
    let id = item.id().to_string();
    let size = theme.sizes.icon_bar;

    let glyph: Element<'_, Message> = match item.icon() {
        Some(TrayIcon::Svg(handle)) => svg(handle.clone()).width(size).height(size).into(),
        Some(TrayIcon::Raster(handle) | TrayIcon::Pixmap(handle)) => {
            image(handle.clone()).width(size).height(size).into()
        }
        // Nothing resolved (see `item::TrayIcon`'s doc comment for exactly
        // which failures land here) — the label is the honest fallback,
        // capped at `pill_max_width` the same way Stage 18's title pill was,
        // so one badly-behaved item can't stretch the bar. No pill fill:
        // this is still bare-on-ink, just text instead of a glyph.
        None => container(
            text(item.label())
                .size(theme.typography.size.bar)
                .wrapping(Wrapping::None)
                .color(theme.on_ink.primary.into_iced()),
        )
        .max_width(theme.sizes.pill_max_width)
        .clip(true)
        .into(),
    };

    // `Status` → presentation (PLAN.md Stage 19); see the module doc
    // comment for the full rationale. `Passive` is unreachable here in
    // practice (`Tray::view` already filtered it out), but the match stays
    // exhaustive so a future `ItemStatus` variant is a compile error here
    // rather than silently falling through as "looks like Active".
    let glyph = match item.status() {
        ItemStatus::NeedsAttention => {
            let accent = theme.palette.accent.into_iced();
            let radius = theme.radii.pill;
            container(glyph)
                .style(move |_: &iced::Theme| container::Style {
                    border: Border {
                        color: accent,
                        width: 2.0,
                        radius: radius.into(),
                    },
                    ..container::Style::default()
                })
                .into()
        }
        ItemStatus::Passive | ItemStatus::Active => glyph,
    };

    mouse_area(glyph)
        .interaction(iced::mouse::Interaction::Pointer)
        .on_press(Message::Activate(id.clone()))
        .on_right_press(Message::ContextMenu(id.clone()))
        .on_scroll(move |delta| Message::Scroll(id.clone(), delta))
        .into()
}

/// Reduce iced's wheel event to the `(delta, orientation)` pair SNI's
/// `Scroll` method wants: whichever axis moved further wins the
/// orientation, and its (rounded) magnitude is the delta. Ties default to
/// vertical — the far more common scroll axis, and an arbitrary but
/// harmless choice for the degenerate `(0, 0)` case.
///
/// Pure function, unit-tested below; `Lines` and `Pixels` are treated
/// identically since SNI's `Scroll` has no notion of units at all, only a
/// signed magnitude.
fn scroll_units(delta: ScrollDelta) -> (i32, &'static str) {
    let (x, y) = match delta {
        ScrollDelta::Lines { x, y } | ScrollDelta::Pixels { x, y } => (x, y),
    };
    if y.abs() >= x.abs() {
        (y.round() as i32, "vertical")
    } else {
        (x.round() as i32, "horizontal")
    }
}

/// Left-click on `id`'s item. `Task::future(..).discard()` is the same
/// command-out shape `media.rs` established in Stage 17: a fresh one-shot
/// D-Bus call, whose success/failure `Panel::update` has nothing useful to
/// do with beyond the `eprintln!` already inside `item::send_activate`.
pub fn activate(id: String) -> Task<Message> {
    Task::future(item::send_activate(id)).discard()
}

/// A scroll over `id`'s item.
pub fn scroll(id: String, delta: ScrollDelta) -> Task<Message> {
    let (delta, orientation) = scroll_units(delta);
    Task::future(item::send_scroll(id, delta, orientation)).discard()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture: an item keyed by `id`, with its address derived the way the
    /// worker derives it — mirrors `item.rs`'s own private test fixture,
    /// duplicated here (rather than exposed from `item.rs`) because
    /// `TrayIcon`/`ItemStatus` are `pub(super)` and this module is that
    /// `super`.
    fn item_with(id: &str, label: &str, icon: Option<TrayIcon>, status: ItemStatus) -> TrayItem {
        TrayItem::new(
            id.to_string(),
            item::parse_registration(id).expect("test ids parse"),
            label.to_string(),
            icon,
            status,
        )
    }

    #[test]
    fn an_empty_tray_is_absent() {
        let tray = Tray::default();
        assert!(!tray.is_present());
    }

    #[test]
    fn a_tray_with_items_is_present_and_renders_one_element_each() {
        let mut registry = item::ItemRegistry::default();
        for (id, label) in [
            (":1.1/StatusNotifierItem", "Slack"),
            (":1.2/StatusNotifierItem", "nm-applet"),
        ] {
            registry.upsert(item_with(id, label, None, ItemStatus::Active));
        }

        let tray = registry.snapshot();
        assert!(tray.is_present());
        assert_eq!(tray.items.len(), 2);

        // The view builds without panicking (the same smoke-test shape
        // `main.rs`'s `module_view` tests use — iced can't be asked what it
        // drew without a renderer).
        let theme = Theme::saola();
        let _ = tray.view(&theme);
        let _ = Tray::default().view(&theme);
    }

    #[test]
    fn a_tray_of_only_passive_items_is_absent() {
        // The spec's own wording ("likely ... hidden") realized literally:
        // an all-Passive tray must read as absent, same as an empty one, so
        // `Panel::island_view` doesn't spend a scrim pill on nothing.
        let mut registry = item::ItemRegistry::default();
        registry.upsert(item_with(
            ":1.1/StatusNotifierItem",
            "Quiet",
            None,
            ItemStatus::Passive,
        ));
        let tray = registry.snapshot();

        assert!(!tray.is_present());
        let theme = Theme::saola();
        let _ = tray.view(&theme);
    }

    #[test]
    fn passive_items_are_filtered_but_their_siblings_still_render() {
        let mut registry = item::ItemRegistry::default();
        registry.upsert(item_with(
            ":1.1/StatusNotifierItem",
            "Quiet",
            None,
            ItemStatus::Passive,
        ));
        registry.upsert(item_with(
            ":1.2/StatusNotifierItem",
            "Loud",
            None,
            ItemStatus::NeedsAttention,
        ));
        let tray = registry.snapshot();

        assert!(tray.is_present());
        let theme = Theme::saola();
        let _ = tray.view(&theme);
    }

    #[test]
    fn every_icon_variant_renders_without_panicking() {
        // Smoke test, same shape as `main.rs`'s `module_view` tests: iced
        // can't be asked what it drew without a renderer, so "builds an
        // `Element` without panicking" is what's checked — across every
        // `TrayIcon` variant and every `ItemStatus` (so the NeedsAttention
        // ring's container/style closure gets exercised too).
        let theme = Theme::saola();

        let svg_item = item_with(
            ":1.1/StatusNotifierItem",
            "Svg",
            Some(TrayIcon::Svg(iced::widget::svg::Handle::from_memory(
                &b"<svg viewBox=\"0 0 24 24\"></svg>"[..],
            ))),
            ItemStatus::Active,
        );
        let raster_item = item_with(
            ":1.2/StatusNotifierItem",
            "Raster",
            Some(TrayIcon::Raster(iced::widget::image::Handle::from_rgba(
                1,
                1,
                vec![0, 0, 0, 255],
            ))),
            ItemStatus::Active,
        );
        let pixmap_item = item_with(
            ":1.3/StatusNotifierItem",
            "Pixmap",
            Some(TrayIcon::Pixmap(iced::widget::image::Handle::from_rgba(
                1,
                1,
                vec![0, 0, 0, 255],
            ))),
            ItemStatus::NeedsAttention,
        );
        let fallback_item = item_with(
            ":1.4/StatusNotifierItem",
            "No Icon",
            None,
            ItemStatus::Active,
        );

        for item in [&svg_item, &raster_item, &pixmap_item, &fallback_item] {
            let _ = item_element(&theme, item);
        }
    }

    #[test]
    fn scroll_units_picks_the_axis_that_moved_further() {
        assert_eq!(
            scroll_units(ScrollDelta::Lines { x: 0.2, y: -3.0 }),
            (-3, "vertical")
        );
        assert_eq!(
            scroll_units(ScrollDelta::Lines { x: 4.0, y: 1.0 }),
            (4, "horizontal")
        );
        assert_eq!(
            scroll_units(ScrollDelta::Pixels { x: -12.4, y: 2.0 }),
            (-12, "horizontal")
        );
    }

    #[test]
    fn scroll_units_defaults_to_vertical_on_a_tie_or_no_movement() {
        assert_eq!(
            scroll_units(ScrollDelta::Lines { x: 0.0, y: 0.0 }),
            (0, "vertical")
        );
        assert_eq!(
            scroll_units(ScrollDelta::Lines { x: 2.0, y: 2.0 }),
            (2, "vertical")
        );
    }
}
