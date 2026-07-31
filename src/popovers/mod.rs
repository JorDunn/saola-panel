//! Popover **content** — one file per [`crate::popover::PopoverKind`], as
//! opposed to `popover.rs`'s open/close lifecycle manager.
//!
//! This is the one sanctioned "module directory" outside `src/modules/`
//! (mirroring the deviation `src/modules/tray/` will be, per PLAN.md Stage
//! 18): a popover's content is real enough to want its own file per kind,
//! but it isn't a bar module (no `subscription()`, no bar-region presence
//! question) and doesn't belong under `modules/`, so it gets its own small
//! directory instead. `mod.rs` here does nothing but re-export.

pub mod quick_settings;
pub mod tray_menu;
