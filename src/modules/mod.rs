//! Bar modules.
//!
//! Each module is one file (with one flagged exception — see `tray` below)
//! exposing a state struct, its own `pub enum
//! Message`, `fn view(&self, theme: &Theme) -> Element<'_, Self::Message>`,
//! and `fn subscription(&self) -> Subscription<Self::Message>` (see the
//! Architecture section of `PLAN.md` and CLAUDE.md for the binding rules
//! this must follow — zero hardcoded colors/sizes, three-color rule, etc.).
//!
//! Stage 7's per-module message refactor: each module owns its `Message`
//! (even single-variant ones); `main.rs`'s panel-level `Message` nests each
//! module's type as one variant (`Message::Battery(battery::Message)`), and
//! composes module subscriptions/views with `.map` at the point they join
//! the panel (`Subscription::map`, `Element::map`) — see `clock::Message`'s
//! doc comment for the full teaching note and `main.rs` for the call sites.
//!
//! Two worker shapes feed those subscriptions. Most modules are **zbus
//! bridges** (an async task owns a D-Bus proxy and pushes snapshots through
//! `iced::stream::channel` — `battery.rs` establishes it). `volume.rs` is the
//! first **thread bridge**: a dedicated `std::thread` owns a foreign C
//! mainloop and pushes snapshots through a `futures::channel::mpsc::
//! unbounded` channel whose receiver *is* the subscription's stream. Read
//! `volume.rs`'s module doc comment before writing any other non-D-Bus
//! signal source.
//!
//! `columns.rs` (Stage 11) is the zbus bridge's other non-D-Bus case, and the
//! reason the table above has two rows rather than "D-Bus vs. everything
//! else": niri's `$NIRI_SOCKET` is newline-delimited JSON over a Unix socket,
//! which tokio hands back as a `Stream` of lines, so it is a *zbus-shaped*
//! worker despite never touching D-Bus. Pick the thread bridge only for a
//! source that insists on driving its own blocking loop. `columns.rs` also
//! introduces the third worker responsibility, beyond "watch" and
//! "reconnect": **fold**. Its source reports deltas rather than snapshots, so
//! the worker keeps state and derives the rendered value from it — read that
//! module before writing any other event-sourced module.
//!
//! `claude.rs` (Stage 12) is a D-Bus module that folds, and the first
//! **signal-listener** zbus bridge: it subscribes with a `zbus::MatchRule` +
//! `MessageStream::for_match_rule` instead of a `#[zbus::proxy]`, because
//! there is no service on the other end to proxy — only a hook script that
//! broadcasts one signal and exits. Read that module's doc comment before
//! writing any other module fed by a broadcast signal rather than a proxied
//! service.
//!
//! `tray/` (Stage 18) is the **one sanctioned deviation from one file per
//! module**: a module *directory* whose `mod.rs` exposes exactly the
//! standard surface above (`Tray`, `Message`, `view`, `subscription`) and
//! keeps the D-Bus half (`watcher.rs`) and the item model (`item.rs`) out of
//! sight behind it — the same shape `src/popovers/` uses for popover
//! content. It is also the first module that **serves** a D-Bus interface
//! (`#[zbus::interface]` + `ObjectServer`) rather than only consuming one;
//! read `tray/watcher.rs`'s doc comment before writing any other module that
//! has to own a bus name.

pub mod battery;
pub mod claude;
pub mod clock;
pub mod columns;
pub mod mark;
pub mod media;
pub mod network;
pub mod tray;
pub mod volume;
