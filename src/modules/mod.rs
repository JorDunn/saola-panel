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
//! `niri.rs` is the zbus bridge's other non-D-Bus case, and the reason the
//! table above has two rows rather than "D-Bus vs. everything else": niri's
//! `$NIRI_SOCKET` is newline-delimited JSON over a Unix socket, which tokio
//! hands back as a `Stream` of lines, so it is a *zbus-shaped* worker despite
//! never touching D-Bus. Pick the thread bridge only for a source that
//! insists on driving its own blocking loop. It also introduces the third
//! worker responsibility, beyond "watch" and "reconnect": **fold**. Its
//! source reports deltas rather than snapshots, so the worker keeps state and
//! derives the rendered values from it — read that module before writing any
//! other event-sourced module.
//!
//! `niri.rs` is furthermore the one file here that is **not a bar module at
//! all**: it has no state on `Panel`, no view, and no `config::ModuleName`.
//! It is a *shared source* — one socket feeding two modules (`columns` and
//! `window_title`), each still owning its own `Message`, view and state,
//! with `main.rs` routing the bridge's two-arm enum to them. Reach for that
//! shape only when two modules genuinely read the same connection; a module
//! with a source of its own still owns its own worker, as every other file
//! here does.
//!
//! `claude.rs` (Stage 12) is a D-Bus module that folds, and the first
//! **signal-listener** zbus bridge: it subscribes with a `zbus::MatchRule` +
//! `MessageStream::for_match_rule` instead of a `#[zbus::proxy]`, because
//! there is no service on the other end to proxy — only a hook script that
//! broadcasts one signal and exits. Read that module's doc comment before
//! writing any other module fed by a broadcast signal rather than a proxied
//! service.
//!
//! `claude.rs` is also the first module to add two things to the standard
//! surface above, both of which the next module with logic of its own should
//! copy rather than reinvent. It exposes an `fn update(&mut self, message:
//! Message)` — `main.rs` unwraps only the outer `Message::ClaudeCode(..)` and
//! delegates, instead of destructuring the inner variants in `Panel::update` —
//! and it is the one module whose `subscription` is *conditional on its own
//! state*: an `iced::time::every` animation timer runs beside the bus worker
//! only while a session is actually working. That timer is Jordan's explicitly
//! sanctioned exception (2026-07-31) to CLAUDE.md's "nothing ticks faster than
//! the clock"; the gating is what keeps it an animation rather than a poll,
//! and the reasoning is written out in full at that `subscription`.
//!
//! `bluetooth.rs` is the zbus bridge's **fan-in** case: BlueZ reports the
//! state this module renders through three different signals
//! (`InterfacesAdded`, `InterfacesRemoved`, and a `MatchRule`-filtered
//! `PropertiesChanged` covering every object the service owns), so all three
//! are merged, normalized to `()`, and *any* of them re-reads
//! `GetManagedObjects()` and rebuilds the whole snapshot. Read its doc
//! comment before writing another module whose source is a *tree* of objects
//! that comes and goes, rather than one fixed object with properties: the
//! rebuild-everything choice (and the dedupe that pays for it) is the point.
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
//!
//! `power.rs` (Stage 17) is the first **popover-only** module: it has the
//! standard surface *minus* `view` — a state struct, a `Message`, a
//! `subscription`, and a command-out `Task` builder — because nothing about
//! power profiles belongs on the bar. It is deliberately still a module
//! rather than popover-local state: its source is a signal like any other,
//! so it gets the same worker shape, the same "absent service renders
//! nothing" contract, and the same one-snapshot-on-`Panel` storage. Not
//! having a bar presence is exactly why it has no `config::ModuleName` and
//! never appears in `main.rs`'s `module_view`/`module_is_present` — those two
//! functions are about *bar regions*, and this module has no slot in one.
//!
//! `brightness.rs` (Stage 22) is the second popover-only module, and the
//! second **thread bridge** — the first one that isn't driven by a foreign C
//! mainloop. Its source is a udev netlink socket: a non-blocking fd libudev
//! tells you to wait on with `poll(2)`, which is a blocking syscall and so
//! wants a thread of its own, exactly like `volume.rs`'s pulse loop. It is
//! also the panel's first module whose read path, write path and change
//! signal are three *different* mechanisms (sysfs, logind D-Bus, udev) —
//! read its doc comment's table before assuming any other module's single
//! source of truth generalises.

pub mod battery;
pub mod bluetooth;
pub mod brightness;
pub mod claude;
pub mod clock;
pub mod columns;
pub mod mark;
pub mod media;
pub mod network;
pub mod niri;
pub mod power;
pub mod tray;
pub mod volume;
pub mod window_title;
