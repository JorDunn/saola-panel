//! Popover **content** — one file per [`crate::popover::PopoverKind`], as
//! opposed to `popover.rs`'s open/close lifecycle manager.
//!
//! This is the one sanctioned "module directory" outside `src/modules/`
//! (mirroring the deviation `src/modules/tray/` will be, per PLAN.md Stage
//! 18): a popover's content is real enough to want its own file per kind,
//! but it isn't a bar module (no `subscription()`, no bar-region presence
//! question) and doesn't belong under `modules/`, so it gets its own small
//! directory instead. `mod.rs` here re-exports the content modules and
//! holds [`centered`], the one view helper they share.

pub mod claude_usage;
pub mod quick_settings;
pub mod tray_menu;

/// Vertically centre a widget inside a button that is taller than it.
///
/// Teaching note (why this exists at all): iced's `button` does *not*
/// centre its content — its layout places the content at the padding's
/// top-left (`iced_widget-0.14.2/src/button.rs`, `layout::padded`). A
/// shrink-height label inside a fixed-height button therefore floats to
/// the top. The fix is always the same sandwich: wrap the content in a
/// container that `Fill`s the button's height and centres on the cross
/// axis — the exact trick the bar's `status_cluster_trigger` and the
/// mark's view already use inline. Width is deliberately left alone: a
/// `Container` adopts its content's width hint, so a `Fill`-wide row
/// still fills and a bare label still shrinks; callers who also want
/// horizontal centring chain `.width(Fill).align_x(Center)` themselves.
pub fn centered<'a, Message: 'a>(
    content: impl Into<iced::Element<'a, Message>>,
) -> iced::widget::Container<'a, Message> {
    iced::widget::container(content)
        .height(iced::Fill)
        .align_y(iced::Center)
}
