//! saola-panel — the Saola desktop's status bar.
//!
//! One **floating ink pill** — not an edge-to-edge strip. The layer-shell
//! surface still anchors to three edges (so it stretches with the output),
//! but it carries margins: `sizes.panel_margin_ledger` at the sides and
//! `sizes.panel_margin_ledger_top` at the anchored edge, which is what lifts
//! the bar clear of the screen and lets `style::container::bar_pill` round
//! its ends at `radii.pill`. It anchors to the top edge by default (or the
//! bottom edge with `panel.kdl`'s `edge "bottom"` — see "Stage 14" below),
//! reserving an exclusive zone so tiled windows sit beside it. Stage 3 lays
//! the bar out ledger-style — left / center
//! (clock) / right — per the Architecture section of `PLAN.md`; Stages 4-5
//! add the battery and network pills to the right region, Stage 8 adds
//! the mark to the left region, Stage 9 adds the media pill beside it, and
//! Stage 10 puts the volume pill at the head of the right region.
//!
//! The bar is a shell surface and is therefore **always ink** — see the
//! design rules in CLAUDE.md. Every color and size below comes from
//! `saola_theme::tokens`; the panel hardcodes none.
//!
//! # Stage 13: one daemon, many surfaces
//!
//! The panel used to be an `iced_layershell::application` — a runtime that
//! owns exactly one layer-shell surface, with a `view(&self)` that could only
//! ever describe that one surface. Stage 13 converts it to a
//! [`iced_layershell::build_pattern::daemon`]: the same single process, the
//! same single `Panel` state, the same single `Message` enum and `update` —
//! but now `view` and `theme` take an [`iced::window::Id`] and are called
//! **once per surface**, so the process can put more than one surface on the
//! screen and render each differently.
//!
//! Nothing new appears on screen in this stage: the daemon still boots with
//! exactly the one surface `Settings` describes, and every Id renders the
//! ledger bar. What the conversion buys is the machinery Phase 2's remaining
//! surface work needs — the Islands layout (three separate layer-shell
//! surfaces), popovers, and tray menus all spawn surfaces at runtime and need
//! `view` to know *which* surface it is describing. That machinery is the
//! [`SurfaceRole`] registry below.
//!
//! # Stage 14: config owns boot
//!
//! `--bottom` is gone. `main` now loads [`config::PanelConfig`] once, before
//! the daemon even starts, and reads `edge`/`height` off it to build the
//! same `Anchor`/`LayerShellSettings` this stage 13 built from a CLI flag —
//! `edge "bottom"` in `panel.kdl` is the only way to flip the bar now (see
//! `config`'s module doc comment for the full resilience story: an absent
//! or malformed config degrades to exactly today's hardcoded layout, never
//! a crash). `colors { }` overrides are applied to the boot `Theme`'s
//! palette *before* that `Theme` is handed to iced at all, and the module
//! lists (`left`/`center`/`right`) replace the hardcoded regions in
//! `Panel::bar_view` via `Panel::module_view`'s name → view mapping.
//!
//! This stage also fixes the `.default_font(..)` / `.settings(..)` ordering
//! quirk Stage 13 flagged and deliberately left alone: `default_font` now
//! lives *inside* the `Settings` literal instead of a separate builder call
//! that `.settings(..)` was silently undoing (see the comment at that call
//! site below) — the bar now actually renders in the Saola UI font.
//!
//! # Stage 15: two layouts, one seam
//!
//! `style "islands"` in `panel.kdl` swaps the single ledger bar for the
//! spec's default panel style: **three** free-standing solid-ink pill
//! clusters floating over the wallpaper — since 2026-08-01 all drawn on
//! **one** full-width layer-shell surface, the islands strip, as stacked
//! layers ([`Panel::islands_view`]; see [`IslandKind`] for why the
//! original surface-per-cluster design collapsed into one, and for the
//! spec-§7 notifications slot excluded from this phase). The switch is
//! config-only: both layouts compose the very same [`Panel::module_view`]
//! / [`Panel::region`] pieces and differ only in the outer widget tree
//! ([`Panel::bar_view`] vs [`Panel::islands_view`]) and in the layer-shell
//! geometry ([`SurfaceGeometry::of`]) — no module lays itself out.
//!
//! The one thing a module *may* know about the style is its own **surface
//! treatment** — clock and mark use it today (e.g. the clock: an
//! ivory pill on the ledger bar, bare text on an island). That is an
//! amendment to Stage 15's original flat seam rule, made deliberately — the
//! reasoning is in [`Panel::island_view`]'s doc comment.
//!
//! Two things about the surfaces are worth reading before touching them:
//!
//! * **Every surface is full-width and mostly transparent.** The islands
//!   strip spans the whole output (inset by the margin); each cluster's
//!   pills hug their content and are aligned left / centre / right by
//!   their stack layer. Nothing measures text, so nothing has to resize a
//!   surface when the clock ticks or a song changes. The strip takes
//!   pointer input across that whole width, like the ledger bar — see
//!   [`SurfaceGeometry`]'s `events_transparent` field for what the empty
//!   space swallows and why that's acceptable.
//! * **The app-wide background is transparent** (see [`Panel::style`]), which
//!   is what lets the wallpaper show between the islands — and, incidentally,
//!   what finally makes the *ledger* bar's rounded ends visible.
//!
//! # Stage 16: the panel becomes clickable
//!
//! The first interactive thing in the whole project. The status cluster — the
//! bar's right region in ledger style, the right island in islands style —
//! is now a trigger that opens a **popover**: one more layer-shell surface,
//! this one on `Layer::Overlay`, `sizes.popover_width` wide, sitting
//! `sizes.popover_top` below the edge the panel hangs from (6 px below the
//! panel strip — see `SurfaceGeometry::of` for the math), holding an opaque
//! ink panel
//! (see [`popover`]). Stage 16 opens an *empty* one; the point is the
//! lifecycle — open, close on a second click, close on Escape, close when the
//! compositor takes keyboard focus away, and never two at once — proven in
//! both layout styles before Stage 17 puts quick settings inside it.
//!
//! Two consequences ripple outwards from "clickable":
//!
//! * **[`SurfaceGeometry`] grew a `layer` and a `keyboard_interactivity`.**
//!   Both used to be hardcoded (`Top` / `None`) at the two places a surface
//!   is created; a popover needs `Overlay` / `OnDemand`, so they joined the
//!   rest of the per-surface geometry instead.
//! * **The islands' input model became the ledger's.** Stage 16 originally
//!   let only the right of the three per-cluster surfaces take input
//!   (`events_transparent` is all-or-nothing per surface, and overlapping
//!   full-width input regions would have fought over the pointer) — which
//!   later left the mark and media clusters unclickable, and is the story
//!   of why the three surfaces became one strip (see [`IslandKind`]).

mod config;
mod config_watch;
mod icons;
mod modules;
mod popover;
mod popovers;

use std::collections::HashMap;
use std::path::PathBuf;

use iced::alignment::Horizontal;
use iced::widget::{button, container, row, stack, Space};
use iced::window;
use iced::{Element, Fill, Subscription, Task};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use modules::battery::Battery;
use modules::claude::ClaudeCode;
use modules::clock::Clock;
use modules::columns::Columns;
use modules::mark::Mark;
use modules::media::Media;
use modules::network::Network;
use modules::tray::Tray;
use modules::volume::Volume;
use modules::window_title::WindowTitle;
use popover::{PopoverKind, PopoverManager};
use saola_theme::{style, to_iced_theme, Surface, Theme};

fn main() -> iced_layershell::Result {
    // Loaded here, before iced's event loop exists at all — see `config`'s
    // module doc comment for the full resilience contract (absent/malformed
    // file → today's hardcoded layout, never a crash). Command-line flags
    // (`--ledger`/`--islands`, `--top`/`--bottom`, `--config-dir`) then
    // override the file — a testing convenience, so switching modes doesn't
    // require editing `panel.kdl` (see `config::CliOverrides`). The flags
    // are parsed into a value of their own (rather than applied and
    // forgotten) because the config is no longer read exactly once:
    // `config_watch` re-reads `panel.kdl` whenever it changes on disk, and
    // the reload arm in `Panel::update` needs the boot flags in hand to
    // keep them winning over the file on every reload, not just the first
    // read.
    //
    // The *path* is resolved exactly once, here, and threaded to both of
    // its consumers — the boot load below and the `config_watch`
    // subscription (via `Panel`) — so the loader and the watcher cannot
    // disagree about which file is the config. `--config-dir` heads the
    // resolution chain (then `$SAOLA_CONFIG_DIR`, then XDG — see
    // `config::PanelConfig::resolve_path`), which is why the flags must be
    // parsed *before* the load, unlike the style/edge overrides applied
    // after it: this flag decides where the config is, not what's in it.
    let cli = config::CliOverrides::parse(std::env::args().skip(1));
    let config_path = config::PanelConfig::resolve_path(cli.config_dir.as_deref());
    let mut config = config::PanelConfig::load(config_path.as_deref());
    cli.apply(&mut config);

    // A separate Theme instance just for the values `main` needs before the
    // application starts (bar height, default font) — `Panel` gets its own
    // clone below rather than a borrow, which keeps the `Panel::new` boot
    // closure simple (see the `daemon(..)` call). `colors { }` overrides are
    // applied here, to the *only* `Theme` this process ever builds from
    // `Theme::saola()` — both `main`'s own use of `theme` and the clone
    // `Panel` gets afterward see the override applied exactly once.
    let mut theme = Theme::saola();
    config.colors.apply(&mut theme.palette);

    // The one surface `Settings` creates at boot is the whole panel: the
    // ledger bar in ledger style, the islands strip in islands style (see
    // `initial_role`). Every number the layer-shell protocol needs —
    // anchor, size, margins, exclusive zone, input transparency — is
    // derived from tokens and config in one place, `SurfaceGeometry::of`,
    // so `main` and `Panel::spawn_boot_surfaces` cannot drift apart.
    let initial = SurfaceGeometry::of(initial_role(&config), &config, &theme);

    // Computed *before* the `daemon(move || ..)` call below, which moves
    // `theme` into the boot closure — `Font` is a small `Copy`-ish value
    // (cheap to compute once and stash), so there is nothing to gain by
    // deferring this past the point where `theme` stops being directly
    // available in `main`.
    let default_font = saola_theme::convert::ui_font(&theme);

    // iced_layershell 0.19's build-pattern API: free functions for boot /
    // update / view (mirroring `iced::daemon`), then builder methods. The
    // `"saola-panel"` string is the layer-shell namespace — what the
    // compositor (niri) sees this surface identify as.
    //
    // `daemon` differs from the `application` builder this replaced in
    // exactly two places, both about surfaces being plural:
    //   * `view` is `Fn(&Panel, window::Id) -> Element<'_, Message>` rather
    //     than `Fn(&Panel) -> Element<'_, Message>`,
    //   * `theme` is `Fn(&Panel, window::Id) -> iced::Theme` rather than
    //     `Fn(&Panel) -> iced::Theme`.
    // `boot` is a closure rather than a bare `Panel::new` function item as
    // of this stage — `BootFn` accepts any `Fn() -> C` (see Stage 13's
    // handoff for the exact trait shapes), and a closure is what lets
    // `config`/`theme` (read from the environment/filesystem here in
    // `main`, where doing so is straightforward) reach `Panel::new` without
    // `Panel` re-reading `panel.kdl` itself on every boot. `namespace`,
    // `update`, `subscription` and every `Settings` field besides
    // `default_font` are unchanged from Stage 13.
    //
    // The initial surface still comes from `Settings` below, not from any
    // code we write: `daemon` is not "start with zero surfaces". A daemon
    // with `StartMode::Active` opens exactly one; `StartMode::AllScreens`
    // would open one per output (each with its own Id — see `SurfaceRole`).
    //
    // Stage 15 changes the boot closure's *return type*, not its shape: it
    // now hands back `(Panel, Task<Message>)` instead of a bare `Panel`.
    // `BootFn` accepts both — `IntoBoot` is implemented for a bare `State`
    // (task = `Task::none()`) *and* for the `(State, Task<Message>)` tuple
    // (`build_pattern/daemon.rs:109–119`) — and the tuple form is how the
    // islands layout asks for its two extra surfaces the moment the process
    // starts, without needing a "have I spawned them yet?" flag on `Panel`
    // or a first-frame hook. See `Panel::spawn_boot_surfaces`.
    daemon(
        // `cli` and `config_path` ride along as clones so the reload arm
        // can re-apply the boot flags and the watcher can follow the
        // resolved path — see the comment where they were derived above.
        move || {
            Panel::new(
                config.clone(),
                cli.clone(),
                config_path.clone(),
                theme.clone(),
            )
            .boot()
        },
        "saola-panel",
        Panel::update,
        Panel::view,
    )
    .theme(Panel::theme)
    // The app-wide surface background (see `Panel::style`). Unlike
    // `default_font` below, this is a real builder method and `.settings(..)`
    // does not clobber it — `.style(..)` wraps the *program*, while
    // `.settings(..)` replaces the `Settings` value; they touch different
    // fields of the builder.
    .style(Panel::style)
    .subscription(Panel::subscription)
    .settings(Settings {
        // FIXED (Stage 13's flagged KNOWN QUIRK): `default_font` now lives
        // *inside* this literal instead of behind a separate
        // `.default_font(..)` builder call made *before* `.settings(..)`.
        // Both `application` and `daemon`'s `.settings(s)` do
        // `Self { settings: s, ..self }` — a full replacement of the whole
        // `Settings` value — so a `.default_font(..)` call preceding
        // `.settings(..)` was always silently overwritten back to
        // `Font::default()` by the `..Default::default()` in that later
        // literal. Setting the field here, in the literal that actually
        // survives, is what makes the bar render in the Saola UI font
        // (`saola_theme::convert::ui_font`) rather than iced's default.
        default_font,
        layer_settings: LayerShellSettings {
            anchor: initial.anchor,
            // `Top` layer for a panel surface: above normal windows, below
            // lock screens and other overlays. Popovers sit on `Overlay`
            // instead — which is why, as of Stage 16, this is one more field
            // `SurfaceGeometry` decides rather than a constant spelled out
            // here and again at the spawn site.
            layer: initial.layer,
            exclusive_zone: initial.exclusive_zone,
            size: Some(initial.size),
            margin: initial.margin,
            events_transparent: initial.events_transparent,
            // A status bar never takes keyboard focus (`None`); a popover
            // does, on demand. Same story as `layer` above.
            keyboard_interactivity: initial.keyboard_interactivity,
            // Show on the currently active output (the default; spelled
            // out because output selection is a real choice here).
            start_mode: StartMode::Active,
            // No `..Default::default()`: adding `events_transparent` above
            // completed the struct, and clippy's `needless_update` rejects a
            // rest-pattern that can never fill anything in. If a future
            // `iced_layershell` grows a field here, this literal stops
            // compiling — which is the better failure.
        },
        ..Default::default()
    })
    .run()
}

/// The geometry of one layer-shell surface: everything the wlr-layer-shell
/// protocol needs to place it, derived from tokens and `panel.kdl` alone.
///
/// This exists because the same numbers are needed in two places that cannot
/// share a code path: `main` fills an [`LayerShellSettings`] for the surface
/// the runtime creates at boot, while [`Panel::spawn_boot_surfaces`] fills a
/// [`NewLayerShellSettings`](iced_layershell::reexport::NewLayerShellSettings)
/// for each surface we ask for later. The two structs are *nearly* the same
/// shape but not quite (see [`SurfaceGeometry::new_layer_shell_settings`]),
/// and having each site do its own arithmetic is exactly how a bar and an
/// island end up 6 px apart. Being a pure function of `(role, config, theme)`
/// also makes the whole thing unit-testable without a compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SurfaceGeometry {
    /// Which screen edges the surface sticks to. Every Saola panel surface
    /// anchors to three (the panel edge plus left and right), which is what
    /// makes it stretch to the output's width.
    anchor: Anchor,
    /// `(width, height)`. **Width 0 means "stretch between the anchored left
    /// and right edges"** — legal precisely because we anchor to both of
    /// them (a layer surface with a zero dimension that is *not* anchored to
    /// both edges on that axis is a protocol error).
    size: (u32, u32),
    /// `(top, right, bottom, left)` — CSS order, verified in the dependency
    /// source rather than guessed: `multi_window.rs` hands this tuple to
    /// layershellev's `LayerShellWindow::with_margin`, which destructures it
    /// as `(top, right, bottom, left)` before calling the protocol's
    /// `set_margin(top, right, bottom, left)`
    /// (`layershellev-0.19.1/src/lib.rs:1420`).
    ///
    /// Because the surface is anchored to left *and* right, the side margins
    /// shrink the stretched width by that much on each side; the
    /// anchored-edge margin is the gap between the screen edge and the
    /// surface.
    margin: (i32, i32, i32, i32),
    /// Pixels along the anchored edge that the compositor should keep clear
    /// of tiled windows. See [`SurfaceGeometry::of`] for the arithmetic and
    /// the measurement that pinned it down.
    exclusive_zone: i32,
    /// `true` ⇒ the surface is **entirely** click-through.
    ///
    /// Teaching note, because the name oversells it: this is not "let events
    /// through where nothing is drawn". layershellev implements it as
    /// `wl_compositor.create_region()` (an *empty* region) followed by
    /// `wl_surface.set_input_region(Some(&region))`
    /// (`layershellev-0.19.1/src/lib.rs:2216`, `:2269`, `:2332`, `:2653`) —
    /// an empty input region means no pointer or touch event ever lands on
    /// the surface, opaque pixels included. It is all or nothing per
    /// surface, and it is what makes the islands' full-width transparent
    /// overhang harmless: clicks in the empty space beside a pill fall
    /// through to whatever is underneath instead of being swallowed.
    events_transparent: bool,
    /// Which wlr-layer-shell layer the surface lives on. Panel surfaces are
    /// `Top` (above windows, below lock screens); popovers are `Overlay`, so
    /// they sit above the panel that opened them — and, on niri, above a
    /// fullscreen window too, which `Top` would not.
    layer: Layer,
    /// Whether the surface may take keyboard focus. Every panel surface says
    /// `None` — a status bar that stole the keyboard would be a bug. A
    /// popover says `OnDemand`, which is what makes Escape and
    /// dismiss-on-focus-loss possible at all (see [`popover::subscription`]).
    keyboard_interactivity: KeyboardInteractivity,
}

impl SurfaceGeometry {
    /// Derive a surface's geometry from its role plus the config and theme.
    ///
    /// # The exclusive-zone arithmetic (measured, not assumed)
    ///
    /// The zone is the number of pixels the compositor keeps clear along the
    /// anchored edge. wlr-layer-shell compositors add the anchored-edge
    /// margin to the reserved area themselves (measured on niri while
    /// writing Stage 15: with `exclusive_zone = 48` and `margin.top = 18`, a
    /// maximized window's tile height dropped by exactly 66 px — 48 + 18,
    /// the margin counted once). As of 2026-08-01 the zone we pass is
    /// **height + one edge margin**: the extra margin is a bottom gap, so
    /// tiled windows keep the same breathing room below the panel as the
    /// top margin gives it above (Jordan's ask). Total reserved strip =
    /// zone + margin = height + 2 × edge margin: 84 for the ledger bar
    /// (48+18+18), 76 for the islands strip (40+18+18).
    ///
    /// Both panel surfaces reserve their own strip — with the islands a
    /// single surface (2026-08-01, see `IslandKind`'s doc comment for the
    /// history), there is no non-reserving sibling left to re-level. (The
    /// retired mechanics, kept here as the formula the popover still uses:
    /// a zone-respecting surface passing `0` — reserve nothing, respect
    /// everyone's reservations, `-1` would measure from the raw screen
    /// edge and break next to a foreign bar like waybar — is placed below
    /// the panel's reservation, so *edge margin = desired offset from the
    /// panel's baseline − the strip the panel reserves*; the popover's
    /// `popover_top − strip` below is exactly that arithmetic.)
    ///
    /// # The popover's anchor math (Stage 16, corrected)
    ///
    /// Spec §6 places a popover "72px from the screen top, 26px from the
    /// relevant edge". Both numbers are anchor margins on a surface anchored
    /// to two edges — the panel's edge and the right-hand side — and the
    /// question that decides how the first one is realized is:
    ///
    /// **Does the popover surface respect exclusive zones?** *Yes* — it
    /// passes `exclusive_zone: Some(0)`: reserve nothing, but let the
    /// compositor position it inside the *usable* area, i.e. below every
    /// reservation on the output. Its edge margin is therefore not
    /// `popover_top` itself but the **gap**: `popover_top` minus the strip
    /// the panel reserves (exclusive zone + anchored-edge margin — wlr
    /// compositors add the margin to the reservation). Since the bottom-gap
    /// change (2026-08-01) that strip already exceeds `popover_top` — 84 in
    /// ledger style (48+18+18), 76 in islands (40+18+18) — so the gap
    /// clamps to 0 and the popover sits flush against the reserved strip,
    /// i.e. exactly one edge margin (18) below the panel's bottom edge: the
    /// same breathing room tiled windows now get.
    ///
    /// The strip is read back from the panel's own [`SurfaceGeometry`]
    /// (via [`initial_role`]) rather than re-derived from tokens, so a
    /// `height` knob in `panel.kdl` moves the popover with the panel, and
    /// the gap is clamped at 0 so no configuration can make the popover
    /// climb back over its trigger.
    ///
    /// Stage 16 originally passed `Some(-1)` ("do not move me out of
    /// anyone's reserved area") with the literal `margin.top = popover_top`,
    /// which reads more like the spec but measures from the raw screen edge.
    /// That breaks the moment any *other* bar with an exclusive zone is on
    /// the output (waybar, during Jordan's first interactive pass): the
    /// panel gets pushed down below the foreign zone, the popover doesn't,
    /// and the popover sits on top of the panel. Zone `0` makes both
    /// surfaces measure from the same baseline, so they move together under
    /// any neighbour.
    ///
    /// The side inset is `config.margin(theme)` — `panel_margin_ledger`
    /// (20) in both styles since the islands margins were matched to the
    /// ledger's — which lines the popover's right edge up with the end of
    /// the panel it hangs from. It also means a user who moves the panel
    /// with `margin` in `panel.kdl` moves the popover with it. (If
    /// saola-theme ever grows a dedicated `popover_margin` token, this is
    /// the line to change.)
    fn of(role: SurfaceRole, config: &config::PanelConfig, theme: &Theme) -> Self {
        // Popovers share none of the panel surfaces' geometry — different
        // layer, different anchors, a real width, keyboard focus — so they
        // take their own path out rather than threading four more `match
        // role` arms through the arithmetic below.
        if let SurfaceRole::Popover(kind) = role {
            // The panel surface this popover hangs below — the bar in ledger
            // style, the islands strip in islands style. Asking it for its
            // own geometry instead of repeating the strip arithmetic here
            // means the two can't drift.
            let panel = Self::of(initial_role(config), config, theme);
            let strip = panel.exclusive_zone
                + match config.edge {
                    config::Edge::Top => panel.margin.0,
                    config::Edge::Bottom => panel.margin.2,
                };
            // Clamped at 0: with the bottom-gap reservation the strip
            // already passes `popover_top`, so the popover sits flush
            // against the reserved strip — one edge margin below the panel —
            // and can never climb back over its trigger. See the doc
            // comment for the full derivation.
            let gap = (theme.sizes.popover_top as i32 - strip).max(0);
            let side = config.margin(theme) as i32;

            return Self {
                // Two edges only: the panel's edge and the right-hand side.
                // (Every panel surface anchors to three; a popover is not
                // stretched on either axis, which is why both of its size
                // components have to be real numbers — see
                // `PopoverKind::height`.)
                anchor: match config.edge {
                    config::Edge::Top => Anchor::Top | Anchor::Right,
                    config::Edge::Bottom => Anchor::Bottom | Anchor::Right,
                },
                size: (theme.sizes.popover_width as u32, kind.height(theme) as u32),
                // The left margin is meaningless on a surface that isn't
                // anchored to the left edge; it's 0 rather than `side` to
                // say so.
                margin: match config.edge {
                    config::Edge::Top => (gap, side, 0, 0),
                    config::Edge::Bottom => (0, side, gap, 0),
                },
                // "Reserve nothing, but keep me out of everyone's reserved
                // areas" — the whole anchor math above depends on this; see
                // the doc comment.
                exclusive_zone: 0,
                // A popover is the one surface that exists to be clicked.
                events_transparent: false,
                // Above the panel (`Top`) and above a fullscreen window.
                layer: Layer::Overlay,
                // The compositor may give it the keyboard. niri grants this
                // the moment the surface maps (verified in niri's own
                // `handlers/layer_shell.rs` — see `popover::subscription`),
                // which is what makes Escape work without a click first.
                keyboard_interactivity: KeyboardInteractivity::OnDemand,
            };
        }

        // Anchoring to three edges (the panel edge plus left and right)
        // makes the surface span the full width of the output. `config.edge`
        // replaced the old `--bottom` CLI flag in Stage 14.
        let anchor = match config.edge {
            config::Edge::Top => Anchor::Top | Anchor::Left | Anchor::Right,
            config::Edge::Bottom => Anchor::Bottom | Anchor::Left | Anchor::Right,
        };

        // Tokens are f32 logical pixels (GUI convention); the layer-shell
        // protocol speaks integer pixels. The casts below are the one place
        // a size token crosses that boundary. Teaching note: `as u32` /
        // `as i32` truncate, which is fine here — every value is a whole
        // number by construction (a token default, or a KDL integer knob).
        let (height, side_margin, edge_margin) = match role {
            SurfaceRole::Bar => (
                config.height(theme),
                config.margin(theme),
                // The ledger bar is inset by a *different* amount at the
                // anchored edge (18) than at the sides (20) — the concept's
                // proportions, measured off the mockup in Stage 14.5.
                theme.sizes.panel_margin_ledger_top,
            ),
            SurfaceRole::Islands => (
                // The islands strip is a `panel_pill` (40), not a
                // `panel_bar` (48). `config.height` can't be used directly:
                // it resolves an absent knob to `panel_bar` for *both*
                // styles. Reading the `Option` here keeps an explicit
                // `height 52` in `panel.kdl` working while defaulting to
                // the islands token.
                config.height.unwrap_or(theme.sizes.panel_pill),
                // Islands share the ledger bar's insets as of 2026-08-01
                // (Jordan: the islands should match the ledger panel):
                // `panel_margin_ledger` at the sides via `config.margin`,
                // `panel_margin_ledger_top` at the anchored edge. The old
                // uniform `panel_margin_islands` (26) inset is retired.
                config.margin(theme),
                theme.sizes.panel_margin_ledger_top,
            ),
            // Unreachable: the `if let` above returns for every popover.
            // Spelled out rather than folded into the `Island` arm so that a
            // future role added to `SurfaceRole` still makes this `match`
            // fail to compile instead of silently inheriting island geometry.
            SurfaceRole::Popover(_) => unreachable!("popovers return above"),
        };

        let (height, side_margin, edge_margin) =
            (height as u32, side_margin as i32, edge_margin as i32);

        // A reserving surface reserves its own height **plus one more edge
        // margin as a bottom gap** (Jordan, 2026-08-01: tiled windows should
        // keep the same breathing room below the panel as above it). The
        // compositor adds the anchored-edge margin itself, so the total
        // reserved strip is `height + 2 × edge margin` — 48+18+18 = 84 for
        // the ledger bar, 40+18+18 = 76 for the islands strip.
        let exclusive_zone = match role {
            SurfaceRole::Bar | SurfaceRole::Islands => height as i32 + edge_margin,
            SurfaceRole::Popover(_) => unreachable!("popovers return above"),
        };

        Self {
            anchor,
            size: (0, height),
            margin: match config.edge {
                config::Edge::Top => (edge_margin, side_margin, 0, side_margin),
                config::Edge::Bottom => (0, side_margin, edge_margin, side_margin),
            },
            exclusive_zone,
            // Both panel surfaces take input across their whole full-width
            // strip, exactly like each other (2026-08-01 — see
            // `IslandKind`'s doc comment for the history: when the islands
            // were three overlapping surfaces, only the right one could
            // take input without the three fighting over the pointer,
            // which left the mark and media clusters unclickable; the
            // single strip is what fixed that). The strip sits inside the
            // panel's own exclusive zone, so the clicks it swallows beside
            // the pills would otherwise only reach wallpaper. Giving the
            // surface pill-shaped input regions instead would need each
            // pill's measured rect (`SetInputRegion`), i.e. the
            // measurement problem Stage 15 ruled out.
            events_transparent: false,
            // Every panel surface is a `Top`-layer surface that never takes
            // the keyboard. Only popovers differ, and they returned above.
            layer: Layer::Top,
            keyboard_interactivity: KeyboardInteractivity::None,
        }
    }

    /// The same geometry as the settings type used to *spawn* a surface.
    ///
    /// Two field-level differences from [`LayerShellSettings`] are worth
    /// noting, both of them `Option`s that mean "don't call the protocol
    /// request at all": `exclusive_zone: None` leaves the protocol default
    /// (0 — the pushed-around one, see [`SurfaceGeometry::of`]) and
    /// `namespace: None` inherits the daemon's own namespace, which is why
    /// spawned surfaces show up as `saola-panel` in `niri msg --json
    /// layers` rather than needing a name each.
    fn new_layer_shell_settings(self) -> iced_layershell::reexport::NewLayerShellSettings {
        iced_layershell::reexport::NewLayerShellSettings {
            anchor: self.anchor,
            layer: self.layer,
            size: Some(self.size),
            margin: Some(self.margin),
            exclusive_zone: Some(self.exclusive_zone),
            keyboard_interactivity: self.keyboard_interactivity,
            events_transparent: self.events_transparent,
            ..Default::default()
        }
    }
}

/// What the surface the runtime creates from `Settings` at boot is *for*,
/// which depends entirely on the configured layout style.
///
/// A daemon always opens that one surface itself (`StartMode::Active`).
/// Since 2026-08-01 both styles are a single full-width surface — the bar,
/// or the islands strip (see [`IslandKind`]'s doc comment for why the
/// three per-cluster surfaces collapsed into one) — so that boot surface
/// simply *is* the panel and nothing further spawns at boot.
///
/// This is also the fallback for an Id the registry has never heard of — see
/// [`Panel::role`].
fn initial_role(config: &config::PanelConfig) -> SurfaceRole {
    match config.style {
        config::PanelStyle::Ledger => SurfaceRole::Bar,
        config::PanelStyle::Islands => SurfaceRole::Islands,
    }
}

/// The panel's message enum. Stage 7's per-module refactor: instead of each
/// module contributing its own variants directly (the old `ClockTick`,
/// `BatteryUpdated(Battery)`, `NetworkUpdated(Network)` shape), every module
/// now owns its own `Message` type (see `modules::clock::Message` for the
/// full teaching note on why), and this enum just *nests* each one behind a
/// single variant. `Panel::update` delegates by pattern-matching through
/// both layers at once (see below); `Panel::subscription` and `Panel::view`
/// wrap each module's own `Subscription`/`Element` in the matching variant
/// via `.map` at the point they're composed together.
///
/// `#[to_layer_message]` is iced_layershell's attribute macro. It does two
/// things: appends the layer-shell control variants (`AnchorChange`,
/// `SizeChange`, `ExclusiveZoneChange`, …) to this enum, and implements the
/// `TryInto<LayerShellCustomActionWithId>` conversion the runtime requires
/// of every layershell application's message type. The injected variants
/// are intercepted by the runtime (that `TryInto` returns `Ok` for them)
/// and never reach `Panel::update` — our own variants convert to `Err(self)`
/// and flow through to `update` as normal. Nesting the module messages one
/// level deeper doesn't change any of this: the macro only cares that
/// `Message` as a whole derives `Debug`/`Clone`, which the nested types do
/// too, so `#[to_layer_message]` stays on this outer enum unchanged.
///
/// # What Stage 13's `multi` changed (teaching note)
///
/// `#[to_layer_message]` → `#[to_layer_message(multi)]` swaps the injected
/// half of the enum for the multi-surface half. In single-surface mode the
/// control variants are *positional and Id-less* (`AnchorChange(Anchor)`,
/// `SizeChange((u32, u32))`, …) because there is only one surface they could
/// possibly mean. In `multi` mode every one of them becomes a struct variant
/// carrying `id: window::Id`, and five brand-new "make me a surface"
/// variants appear. The full injected list, verbatim from
/// `iced_layershell_macros-0.19.1/src/lib.rs`:
///
/// - `AnchorChange { id, anchor }`
/// - `AnchorSizeChange { id, anchor, size }`
/// - `LayerChange { id, layer }`
/// - `MarginChange { id, margin }` — margin is `(top, right, bottom, left)`
///   (CSS order; this line previously claimed `(top, left, bottom, right)`,
///   which was wrong — see the verification note beside `margin` in `main`)
/// - `SizeChange { id, size }`
/// - `ExclusiveZoneChange { id, zone_size }`
/// - `KeyboardInteractivityChange { id, keyboard_interactivity }`
/// - `SetInputRegion { id, callback }`
/// - `VirtualKeyboardPressed { key }` — the one variant with no Id
/// - `NewLayerShell { settings, id }` — spawn a layer-shell surface
/// - `NewBaseWindow { settings, id }` — spawn an xdg toplevel
/// - `NewPopUp { settings, id }` — spawn an xdg popup on a parent surface
/// - `NewMenu { settings, id }` — a popup positioned at the mouse
/// - `NewInputPanel { settings, id }`
/// - `RemoveWindow(id)` — tuple variant, not a struct one
/// - `ForgetLastOutput`
///
/// All of them are still intercepted by the runtime's `TryInto` and never
/// reach `Panel::update`, exactly as before. The macro *also* generates four
/// inherent constructors on this enum in `multi` mode —
/// `Message::layershell_open(settings)`, `popup_open`, `menu_open`,
/// `base_window_open` — each of which mints a fresh `window::Id`, and hands
/// back `(that Id, Task<Message>)`. That pairing is the whole Id-learning
/// story for surfaces *we* spawn: we know the Id before the surface exists,
/// so we can register its role in the same `update` arm that asks for it
/// (see `Panel::spawn_surface`).
#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    /// Wraps `modules::clock::Message` (currently just `Tick`, emitted
    /// roughly once a minute by the clock's subscription — see
    /// `modules::clock::Clock::subscription`). It carries no data of its
    /// own: `Clock::view` reads `Local::now()` itself on every render, so
    /// the tick's only job is to wake the runtime into rendering again.
    Clock(modules::clock::Message),
    /// Wraps `modules::columns::Message` (currently just `Updated(Columns)`),
    /// the niri strip's dash row. Unlike the D-Bus modules' snapshots, the
    /// payload is *derived* state: the worker folds niri's event stream into
    /// a model of the focused workspace and ships the finished row (see
    /// `modules::columns`). Sits beside the clock in the centre region.
    ///
    /// Its worker is not the module's own: `modules::niri` owns the single
    /// `$NIRI_SOCKET` connection and emits both this and `WindowTitle` below
    /// (see `Panel::subscription`'s routing arm, and that module's doc
    /// comment for why one bridge rather than two connections).
    Columns(modules::columns::Message),
    /// Wraps `modules::window_title::Message` — the focused window's title,
    /// the left region's ambient text beside the mark (style guide §7).
    /// `Updated(None)` is "no focused window, or its title is blank", which
    /// renders as nothing.
    ///
    /// Fed by the same `modules::niri` bridge as `Columns` above, which only
    /// sends this when the title *text* actually changed: niri re-announces a
    /// window on every retitle, and a terminal spinner does that several
    /// times a second. Delegated wholesale to the module in `Panel::update`
    /// (the shape `ClaudeCode` established) rather than destructured here.
    ///
    /// The module's other variant, `Tick`, comes from its own gated animation
    /// timer rather than from niri — the opt-in marquee (style guide §5). It
    /// travels the same arm, since "what a message means to the module's
    /// state" is the module's business either way.
    WindowTitle(modules::window_title::Message),
    /// Wraps `modules::mark::Message`. Originally an *empty* enum (the mark
    /// was a static glyph with no signal source at all, and this variant
    /// existed purely so its wiring shape — `.map(Message::Mark)` at the
    /// subscription and view composition sites in `Panel::subscription` /
    /// `Panel::view` — matched every other module's exactly). The mark
    /// becoming clickable gave it its first real variant,
    /// `mark::Message::Pressed`, handled below in `Panel::update` by
    /// spawning the configured `launcher` command.
    Mark(modules::mark::Message),
    /// Wraps `modules::media::Message` (currently just
    /// `Updated(Media)`), a fresh "who's the active MPRIS player" snapshot
    /// from the media worker task (see `modules::media`). Session-bus
    /// D-Bus state, same shape as `Battery`/`Network` below.
    Media(modules::media::Message),
    /// Wraps `modules::volume::Message` (currently just `Updated(Volume)`),
    /// a fresh default-sink snapshot. Same "store the latest snapshot" shape
    /// as the D-Bus modules, but the worker behind it is the project's first
    /// **thread bridge** — a dedicated OS thread running libpulse's C
    /// mainloop, pushing through an unbounded channel (see
    /// `modules::volume`'s module doc comment).
    Volume(modules::volume::Message),
    /// Wraps `modules::battery::Message` (currently just
    /// `Updated(Battery)`), a fresh snapshot from the UPower worker task
    /// (see `modules::battery`). The battery's state lives on D-Bus, not
    /// somewhere `view` can read synchronously, so the async worker pushes
    /// each new snapshot through this message and `update` stores it on
    /// `Panel` for `view` to render.
    Battery(modules::battery::Message),
    /// Wraps `modules::network::Message` (currently just
    /// `Updated(Network)`), a fresh Wi-Fi snapshot from the iwd worker task
    /// (see `modules::network`). Same shape as `Battery`: the SSID lives on
    /// D-Bus, so the async worker pushes each new snapshot through this
    /// message and `update` stores it on `Panel` for `view` to render.
    Network(modules::network::Message),
    /// Wraps `modules::bluetooth::Message` (just `Updated(Bluetooth)`), a
    /// fresh BlueZ snapshot — adapter powered state plus the connected
    /// devices. Same "store the latest snapshot" shape as `Network` above it
    /// (and it sits right beside it in the right region, the two radios
    /// together). The worker behind it differs in one way worth knowing:
    /// its source is a *tree* of D-Bus objects that comes and goes rather
    /// than one fixed object, so it merges three signal streams and rebuilds
    /// the whole snapshot on any of them (see `modules::bluetooth`).
    Bluetooth(modules::bluetooth::Message),
    /// Wraps `modules::power::Message` — either `Updated(Power)`, a fresh
    /// power-profiles-daemon snapshot, or `SetProfile(name)`, a
    /// quick-settings chip asking to switch profiles (a command-out call,
    /// same shape as `Media::PlayPause` above).
    ///
    /// The one variant here with **no bar presence at all**: `power` is the
    /// panel's first popover-only module (see `modules::power`'s doc
    /// comment), so unlike every module above it there is no
    /// `config::ModuleName::Power`, no region slot, and no arm for it in
    /// `module_view`/`module_is_present` — those are about laying out bar
    /// regions, and this module never appears in one. Everything else about
    /// it is standard: a signal-fed worker pushing snapshots that `update`
    /// stores on `Panel` for the quick-settings popover to read.
    Power(modules::power::Message),
    /// Wraps `modules::brightness::Message` — either `Updated(Brightness)`, a
    /// fresh screen-brightness snapshot, or `SetBrightness(percent)`, the
    /// quick-settings slider asking for a new level (a command-out call, same
    /// shape as `Power::SetProfile` above).
    ///
    /// The second variant here with **no bar presence at all** (see
    /// `Power` just above for what that entails — no
    /// `config::ModuleName::Brightness`, no region slot, no arm in
    /// `module_view`/`module_is_present`). What is new is the worker behind
    /// it: brightness has no D-Bus property to watch, so the module reads
    /// sysfs, writes through logind, and is woken by a udev uevent on a
    /// dedicated OS thread — the panel's second **thread bridge** after
    /// `volume` (see `modules::brightness`'s doc comment).
    Brightness(modules::brightness::Message),
    /// Wraps `modules::claude::Message` — either `Updated(Sessions)`, the
    /// folded "what is each of Jordan's Claude Code sessions doing" list
    /// behind the status-dot row, or `Tick`, one frame of that row's
    /// breathing animation. Unlike every module above, the source isn't a
    /// service with state to read — it's a broadcast signal a hook script
    /// fires and forgets (see `modules::claude`'s doc comment for the
    /// signal-listener bridge shape, and for why the animation timer is a
    /// sanctioned exception to "nothing ticks faster than the clock").
    /// Also the only variant `Panel::update` delegates wholesale to the
    /// module rather than destructuring itself. Last of the right region,
    /// per the style guide's module order (`right { volume; network;
    /// battery; claude; tray; ... }`).
    ClaudeCode(modules::claude::Message),
    /// Wraps `modules::tray::Message` (currently just `Updated(Tray)`), the
    /// current set of registered StatusNotifierItems. The first module whose
    /// worker **serves** a D-Bus interface rather than only consuming one:
    /// on a session with no other tray host, the panel itself owns
    /// `org.kde.StatusNotifierWatcher` and this snapshot is a view of its own
    /// registry (see `modules::tray`'s doc comment). Sits after `claude` in
    /// the right region, per the style guide's module order.
    Tray(modules::tray::Message),
    /// Wraps [`popover::Message`] — the panel's first *interaction* messages
    /// rather than snapshots from a signal source. Three producers: the
    /// status cluster's trigger button (a click on it, in either layout),
    /// and the two dismissal
    /// signals `popover::subscription` filters out of iced's event broadcast
    /// (a surface losing keyboard focus, and Escape). Handled by
    /// `Panel::update_popover`, which is the one place a popover surface is
    /// created or destroyed.
    Popover(popover::Message),
    /// Wraps [`popovers::tray_menu::Message`] — the tray-menu popover's own
    /// interactions (a fetch landing, a row clicked, a submenu toggled).
    /// **Not** nested under `modules::tray::Message`: that type is about the
    /// bar's icons (`Activate`/`Scroll`/`ContextMenu`), while this is about
    /// the *popover* a right-click opens — the same separation
    /// `popover::Message` already keeps from `modules::volume`/`modules::
    /// media`'s own message types for quick settings. Handled by
    /// `Panel::open_tray_menu` (the `ContextMenu` arm below) and the
    /// `TrayMenu(..)` arms right after it.
    TrayMenu(popovers::tray_menu::Message),
    /// Wraps [`popovers::claude_usage::Message`] — the Claude Code usage
    /// popover's one-shot transcript read answering. Same separation as
    /// `TrayMenu` above: `modules::claude::Message` is about the bar's dot
    /// row, while this is about the popover the claude group's trigger
    /// opens. Produced only by the `Task` `Panel::open_claude_usage` kicks
    /// off on the trigger click; handled by storing the result while that
    /// popover is still the open one.
    ClaudeUsage(popovers::claude_usage::Message),
    /// Wraps [`config_watch::Message`] — `panel.kdl` changed on disk and the
    /// watcher worker re-parsed it (inotify is the signal; see that module's
    /// doc comment for the watch-the-directory and debounce mechanics). The
    /// payload is the whole new [`config::PanelConfig`], parsed off the UI
    /// thread; the reload arm in `Panel::update` is what re-applies it to
    /// the running panel — theme palette, config-fed module state, and the
    /// live layer-shell geometry of the panel surface.
    Config(config_watch::Message),
    /// A layer-shell surface has finished opening and now has an
    /// `iced::window::Id`. Emitted by `iced::window::open_events()` (see
    /// `Panel::subscription`), which is the **only** way to learn the Id of
    /// a surface the runtime created for us rather than one we asked for —
    /// i.e. the initial surface built from `Settings` in `main`. Handled by
    /// registering the Id in `Panel::windows`; see that field and
    /// `Panel::update`'s arm for the "don't clobber a pre-registered role"
    /// rule.
    SurfaceOpened(window::Id),
    /// A layer-shell surface has gone away. Emitted by
    /// `iced::window::close_events()`. Handled by dropping the Id from
    /// `Panel::windows` so the registry can't grow stale entries as
    /// popovers/menus come and go (Stages 16 and 21 are the first real
    /// producers of this). See `Panel::update` for the one surface this
    /// never fires for.
    SurfaceClosed(window::Id),
}

/// What a given layer-shell surface *is*, so `Panel::view` knows what to draw
/// on it.
///
/// # Why `view` has to dispatch on an Id at all (teaching note)
///
/// A daemon renders every one of its surfaces from the same `view` function,
/// calling it once per surface with that surface's `window::Id`. The Id is
/// the only thing distinguishing the calls — `&self` is the same `Panel`
/// every time. So a process that wants a bar on one surface and a popover on
/// another needs a map from Id to "which of my surfaces is this", and that
/// map has to live in the application state because `view` takes `&self` and
/// cannot mutate anything. That map is [`Panel::windows`]; this enum is its
/// value type.
///
/// Stage 13 shipped only `Bar`; Stage 15 adds `Island(IslandKind)`. Stage 16
/// adds `Popover(PopoverKind)` the same way — because `Panel::view` matches
/// this enum exhaustively, adding a variant makes the compiler point at every
/// place that has to grow an arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRole {
    /// The ledger bar: the floating ink pill that is the *whole* panel in
    /// `style "ledger"`. Created by `Settings` at boot; never spawned.
    Bar,
    /// The islands strip: the *whole* panel in `style "islands"` — all
    /// three floating pill clusters drawn on one full-width surface (see
    /// `Panel::islands_view`). Created by `Settings` at boot; never
    /// spawned. This was `Island(IslandKind)` — one surface per cluster —
    /// until 2026-08-01; see [`IslandKind`]'s doc comment for why the
    /// three surfaces collapsed into one.
    Islands,
    /// A popover: the one transient surface kind, spawned when a trigger is
    /// clicked and destroyed when it is dismissed. At most one exists at a
    /// time — the invariant lives in [`PopoverManager`], not here.
    Popover(PopoverKind),
}

/// Which island *cluster* an `island_view` call draws — a layer of the
/// single [`SurfaceRole::Islands`] surface, no longer a surface of its own.
///
/// # Why three surfaces became one (2026-08-01)
///
/// Stage 15 gave each island its own full-width layer-shell surface, and
/// Stage 16 could then let only the right one accept pointer events
/// (`events_transparent` is all-or-nothing per surface, and three
/// overlapping full-width input regions would have fought over the pointer
/// — whichever stacked topmost would swallow every click on the strip).
/// The unpaid bill came due when the *left* cluster grew clickable things:
/// the mark's launcher and media's play/pause sat on a surface the
/// compositor never sent a click to, and no assignment of per-surface
/// input flags can fix that — some cluster always loses. Meanwhile the
/// right island's full-width input region was already swallowing the
/// whole strip. Collapsing the three surfaces into one — same pixels,
/// same input footprint the right island already had, the ledger bar's
/// exact input model — is what made every cluster clickable at once. It
/// also retired the negative-margin re-levelling trick the non-reserving
/// sibling surfaces needed (see git history for `SurfaceGeometry::of`).
///
/// # The three-vs-four deviation (deliberate, flagged)
///
/// Spec §7 inventories the Islands panel as **four** clusters: mark +
/// window title, clock + column strip, status, and *notifications*. This phase
/// ships **three**. Everything notifications is out of scope for Phase 2
/// by an explicit decision recorded in PLAN.md; a future
/// `saola-notifications` component owns it. Nothing here forecloses the
/// fourth: it arrives as one more variant on this enum plus one more
/// layer in `Panel::islands_view` and an arm in `Panel::island_view`,
/// which the compiler will demand.
///
/// The variants are named for **position, not payload**, because that is what
/// the config actually configures: each one maps 1:1 onto one of
/// `panel.kdl`'s `left` / `center` / `right` module lists (Stage 14), so the
/// island grouping is whatever the user's config says and there is no second
/// copy of the module lists anywhere in this file. A notifications island
/// would come with its own list — which is the one piece of config plumbing
/// the fourth slot would need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IslandKind {
    /// `config.left` — the mark and the focused window title, hugging the
    /// left margin. Media moved out of this region entirely (style guide
    /// §7, 2026-08-01): it's a status-cluster glyph now, part of `Right`.
    Left,
    /// `config.center` — the clock and the niri column minimap, centred.
    Centre,
    /// `config.right` — the status cluster, hugging the right margin.
    Right,
}

/// The whole panel state. Stage 3 adds the first module field (`clock`);
/// Stages 4-5 add `battery` and `network` alongside it the same way; Stage 8
/// adds `mark`; Stage 9 adds `media`; Stage 13 adds the `windows` surface
/// registry.
struct Panel {
    /// The Saola theme — the single source of every color and size on the
    /// bar. `main` applies `panel.kdl`'s `colors { }` overrides to a
    /// `Theme::saola()` before this field is ever populated (see
    /// `config::ColorOverrides::apply`), so every color read from here
    /// already reflects the user's config; a live config reload rebuilds it
    /// the same way (see the `Message::Config` arm in [`Panel::update`]).
    theme: Theme,
    /// The parsed `panel.kdl` config, loaded at boot (Stage 14) and swapped
    /// wholesale by a live reload (the `Message::Config` arm). Read
    /// by `Panel::bar_view`/`Panel::module_view` for the module lists;
    /// `theme`/`edge`/`height`/`margin` were already consumed in
    /// `main` before `Panel::new` ran (they shape the layer-shell surface
    /// itself — `margin` is now the surface's screen-edge inset, not
    /// content padding — and that surface exists before any `Panel` does)
    /// but are kept here too
    /// since `PanelConfig` is one small, cheaply-cloned value and splitting
    /// "the parts main needed" from "the parts view needs" into two structs
    /// would be needless ceremony.
    config: config::PanelConfig,
    /// The command-line flags `main` parsed at boot, kept so a live config
    /// reload can re-apply them on top of the freshly read file — `cargo run
    /// -- --islands` must keep meaning Islands across every reload of a
    /// `panel.kdl` that says `style "ledger"`, the same precedence the boot
    /// read had. (The flags themselves can't change mid-process: argv is
    /// fixed at exec time, so storing the parsed value is exact, not a
    /// cache that could go stale.)
    cli: config::CliOverrides,
    /// Where `panel.kdl` lives, as `main` resolved it once at boot
    /// (`config::PanelConfig::resolve_path`: `--config-dir` >
    /// `$SAOLA_CONFIG_DIR` > the XDG chain). Held only so
    /// [`Panel::subscription`] can hand it to the `config_watch` worker —
    /// the boot load already happened in `main` — and `None` (nothing in
    /// the chain resolved: no flag, no vars, no `$HOME`) means no watch
    /// subscription at all, the same "no config is possible here"
    /// environment the loader treats as pure defaults. Stable for the
    /// process's life for the same argv-and-env-are-fixed reason `cli` is.
    config_path: Option<PathBuf>,
    /// The clock module's state (currently empty — see `Clock`'s doc
    /// comment for why).
    clock: Clock,
    /// The bar's mark: a static glyph (see `Mark`'s doc comment) whose
    /// *choice* of glyph — horns, notch, a user file, or none — comes from
    /// `panel.kdl`'s `mark` directive, and whose click-to-launch command
    /// comes from the `launcher` directive, both resolved once at
    /// construction time into `Mark::new(config.mark.clone(),
    /// config.launcher.clone())` below.
    mark: Mark,
    /// The launcher process spawned by the last mark click, while we still
    /// believe it may be running. Holding the `Child` here (rather than
    /// handing it to a fire-and-forget reaper thread, as this arm
    /// originally did) is what makes the mark a *toggle*: a second click
    /// while the launcher is up kills it instead of stacking a second
    /// copy. See the `Message::Mark(..Pressed)` arm for the
    /// running-or-exited decision and the `Message::Clock(..Tick)` arm for
    /// how a launcher that exits on its own still gets reaped.
    launcher_child: Option<std::process::Child>,
    /// The last niri column strip pushed by the niri IPC worker; starts as
    /// `Columns::default()` (an empty dash row → renders nothing, which is
    /// also what a non-niri session leaves it at forever). Sits beside the
    /// clock in the centre region, per the style guide's
    /// `center { clock; niri-columns }`.
    columns: Columns,
    /// The focused window's title, as the niri bridge last reported it;
    /// starts as "nothing focused yet" → renders nothing (and stays there
    /// forever outside a niri session). Sits in the left region immediately
    /// right of the mark, per the style guide's `left { mark; window-title }`.
    window_title: WindowTitle,
    /// The last "active MPRIS player" snapshot pushed by the media worker;
    /// starts as `Media::default()` (no player known yet → renders
    /// nothing). A status-cluster glyph as of 2026-08-01 (style guide §7):
    /// sits at the head of the right region's status cluster, per the
    /// style guide's `right { mpris; volume; network; ... }`.
    media: Media,
    /// The last default-sink volume snapshot pushed by the pulse worker
    /// thread; starts as `Volume::default()` (no sink known yet → renders
    /// nothing). First of the right region's pills, per the style guide's
    /// module order (`right { volume; network; battery; ... }`).
    volume: Volume,
    /// The last battery snapshot pushed by the UPower worker; starts as
    /// `Battery::default()` (no battery known yet → renders nothing).
    battery: Battery,
    /// The last Wi-Fi snapshot pushed by the iwd worker; starts as
    /// `Network::default()` (no Station known yet → renders nothing).
    network: Network,
    /// The last BlueZ snapshot pushed by the Bluetooth worker; starts as
    /// `Bluetooth::default()` (no adapter known yet → renders nothing, which
    /// is also where a machine with no Bluetooth hardware stays forever).
    /// Sits beside `network` in the right region.
    bluetooth: modules::bluetooth::Bluetooth,
    /// The last power-profiles-daemon snapshot (active profile + the
    /// profiles this machine supports); starts as `Power::default()` (daemon
    /// not heard from → the quick-settings row draws nothing). Unlike every
    /// field above it this one feeds **no bar region** — it exists solely for
    /// the quick-settings popover, which is why `modules::power` has no
    /// `view` and no `config::ModuleName` (see `Message::Power`).
    power: modules::power::Power,
    /// The last screen-brightness snapshot (percent, plus the device name and
    /// raw ceiling a write needs); starts as `Brightness::default()` (no
    /// backlight found → the quick-settings row draws nothing, which is also
    /// where a desktop with no backlight at all stays forever). Popover-only,
    /// like `power` above it (see `Message::Brightness`).
    brightness: modules::brightness::Brightness,
    /// One status dot per live Claude Code session, folded by the
    /// signal-listener worker (plus the phase of their breathing
    /// animation); starts as `ClaudeCode::default()` (no `StatusChanged`
    /// signal seen yet → renders nothing — the same "quiet until proven
    /// otherwise" contract as every module above, just for a hook that
    /// hasn't fired instead of a service that isn't there). Last of the
    /// right region.
    claude_code: ClaudeCode,
    /// The registered StatusNotifierItems the tray worker last reported;
    /// starts as `Tray::default()` (nothing registered → renders nothing).
    /// Unlike every module above it, the worker behind this one may be
    /// *serving* the session's `org.kde.StatusNotifierWatcher` rather than
    /// reading somebody else's service — see `modules::tray`.
    tray: Tray,
    /// Every layer-shell surface this process currently owns, keyed by the
    /// `window::Id` the runtime identifies it with, valued by what the
    /// surface is for. Starts **empty**, not pre-seeded with the bar: the
    /// initial surface's Id is minted inside the runtime and is not knowable
    /// until it reports itself (see `Message::SurfaceOpened`).
    ///
    /// A `HashMap` rather than a `Vec` of pairs because `view` looks a role
    /// up by Id on every frame of every surface, and because Ids are opaque
    /// (`window::Id` is `Hash + Eq` and nothing else useful).
    windows: HashMap<window::Id, SurfaceRole>,
    /// Which popover is open, if any — the "only one at a time" invariant
    /// (spec §6) held as an `Option`, so there is nowhere to put a second
    /// one. Deliberately *not* folded into `windows`: that map answers "what
    /// should I draw on this surface", while this one answers "what is the
    /// user looking at", and only the second has an invariant to keep. See
    /// [`popover::PopoverManager`].
    popovers: PopoverManager,
    /// The pulse worker's command-channel handle, once
    /// `modules::volume::Message::Ready` has delivered it — `None` until
    /// then (a few frames at most) and forever after if the worker's
    /// self-pipe couldn't be created at all (see that module's doc
    /// comment). `Panel::update`'s `ToggleMute`/`SetVolume` arms read this
    /// to reach the worker; a `None` there means the mute button and the
    /// quick-settings slider are silent no-ops, the same degrade-quietly
    /// contract as every other absent-capability path in the panel.
    volume_commands: Option<modules::volume::CommandSender>,
    /// State for the tray-menu popover's content: which item's menu is open
    /// (if any), the last tree `modules::tray::menu::read_menu` returned for
    /// it, and which submenu rows are expanded inline. Lives directly on
    /// `Panel` — the same "a module defines the type, `Panel` holds an
    /// instance" shape `volume_commands` above already established — rather
    /// than folding into `modules::tray::Tray` (the bar's own rendering
    /// state, which has no notion of a popover). See
    /// `popovers::tray_menu::TrayMenuState`'s doc comment.
    tray_menu: popovers::tray_menu::TrayMenuState,
    /// State for the Claude Code usage popover's content: whether the
    /// transcript reads are still in flight, and the per-session results
    /// once they land. Same shape and reasoning as `tray_menu` above — the
    /// content module (`popovers::claude_usage`) defines the type, `Panel`
    /// holds the instance, and it only means anything while that popover
    /// is open. Reset by every trigger click (see `Panel::
    /// open_claude_usage`), so a reopened popover always re-reads.
    claude_usage: popovers::claude_usage::ClaudeUsageState,
}

impl Panel {
    /// Boot. `config`/`cli`/`theme` are threaded in from `main`'s closure
    /// (see the `daemon(move || Panel::new(..), ..)` call) rather than each
    /// re-derived here, so `panel.kdl` is read exactly once per *boot* —
    /// `main` already needed `config`/`theme` before this point to size the
    /// layer-shell surface, and re-parsing the file a second time here
    /// would risk observing a different config if it changed between the
    /// two reads (unlikely in practice, since both happen within the same
    /// boot, but needless). Later reads are deliberate: the `config_watch`
    /// worker re-parses the file each time it changes on disk, and the
    /// `Message::Config` arm below applies the result.
    fn new(
        config: config::PanelConfig,
        cli: config::CliOverrides,
        config_path: Option<PathBuf>,
        theme: Theme,
    ) -> Self {
        Self {
            cli,
            config_path,
            mark: Mark::new(config.mark.clone(), config.launcher.clone()),
            launcher_child: None,
            clock: Clock,
            columns: Columns::default(),
            window_title: WindowTitle::new(config.window_title),
            media: Media::default(),
            volume: Volume::default(),
            battery: Battery::default(),
            network: Network::default(),
            bluetooth: modules::bluetooth::Bluetooth::default(),
            power: modules::power::Power::default(),
            brightness: modules::brightness::Brightness::default(),
            claude_code: ClaudeCode::new(config.claude_icon),
            tray: Tray::default(),
            windows: HashMap::new(),
            popovers: PopoverManager::default(),
            volume_commands: None,
            tray_menu: popovers::tray_menu::TrayMenuState::default(),
            claude_usage: popovers::claude_usage::ClaudeUsageState::default(),
            theme,
            config,
        }
    }

    /// Boot: the panel plus whatever surfaces the configured style needs
    /// beyond the one `Settings` already asked for.
    ///
    /// Returning a `(State, Task)` tuple from the boot closure is the
    /// documented alternative shape `IntoBoot` accepts (see the `daemon(..)`
    /// call in `main`); the task it carries is run by the runtime before the
    /// first frame.
    fn boot(mut self) -> (Self, Task<Message>) {
        let task = self.spawn_boot_surfaces();
        (self, task)
    }

    /// Ask for the surfaces the configured style needs *in addition to* the
    /// one the runtime creates from `Settings`.
    ///
    /// As of 2026-08-01 that is **none for both styles**: the bar and the
    /// islands strip are each a single full-width surface, and each *is*
    /// the boot surface (see `initial_role`, and `IslandKind`'s doc
    /// comment for why the islands' three per-cluster surfaces collapsed
    /// into one). The method survives as the seam where a future extra
    /// surface — the flagged notifications slot — would be requested.
    ///
    /// Ordering note for whoever adds surfaces later: these requests are
    /// issued before the compositor has created *any* of our surfaces, which
    /// is safe — Id-carrying layer-shell actions that arrive before their
    /// surface exists are pushed back onto the runtime's
    /// `waiting_layer_shell_actions` queue and retried, not dropped
    /// (`iced_layershell-0.19.1/src/multi_window.rs:708–715`).
    fn spawn_boot_surfaces(&mut self) -> Task<Message> {
        Task::none()
    }

    /// The role of the surface identified by `id` — **the boot surface's
    /// role for an Id the registry has never heard of** (`initial_role`:
    /// the bar in ledger style, the islands strip in islands style), which
    /// is the defensive fallback the Phase 2 architecture asks for.
    ///
    /// That fallback is not theoretical: it is the normal path for the boot
    /// surface's own first frame. The runtime builds a surface's user
    /// interface (which calls `view`) *before* it broadcasts that surface's
    /// `Opened` event, so the very first `view(boot_id)` call happens while
    /// `windows` is still empty. Rendering the boot surface's own layout for
    /// an unknown Id is what makes that frame correct instead of blank, and
    /// it also means a surface that somehow never reports itself degrades to
    /// "shows something sensible" rather than "shows nothing".
    ///
    /// Spawned surfaces never take this path — `spawn_surface` records their
    /// role in the same breath as minting their Id.
    fn role(&self, id: window::Id) -> SurfaceRole {
        self.windows
            .get(&id)
            .copied()
            .unwrap_or_else(|| initial_role(&self.config))
    }

    /// Ask the compositor for a new layer-shell surface in the given `role`,
    /// and register the role against the Id the surface will have.
    ///
    /// # How the daemon learns a spawned surface's Id (teaching note)
    ///
    /// It doesn't *learn* it — it chooses it. `Message::layershell_open` is
    /// one of the four constructors `#[to_layer_message(multi)]` generates;
    /// it calls `window::Id::unique()` itself and returns that Id alongside
    /// a `Task` that delivers `Message::NewLayerShell { settings, id }` to
    /// the runtime. Because the Id exists before the surface does, the role
    /// can be recorded in the very same `update` call that requests the
    /// surface — there is no window between "asked for a surface" and "know
    /// which surface it is" in which `view` could be called with an Id we
    /// can't classify.
    ///
    /// The `Opened` event still arrives afterwards and still produces a
    /// `Message::SurfaceOpened`; the `or_insert` in that arm is what keeps it
    /// from overwriting the role recorded here.
    ///
    /// First called in Stage 15, by `spawn_boot_surfaces`. Returns the Id
    /// alongside the task because Stage 16's caller needs to name the surface
    /// afterwards (`PopoverManager::opened`); callers that don't can drop it.
    fn spawn_surface(
        &mut self,
        role: SurfaceRole,
        settings: iced_layershell::reexport::NewLayerShellSettings,
    ) -> (window::Id, Task<Message>) {
        let (id, task) = Message::layershell_open(settings);
        self.windows.insert(id, role);
        (id, task)
    }

    /// Ask the compositor to destroy the surface identified by `id`, and
    /// forget its role.
    ///
    /// `Message::RemoveWindow(id)` is intercepted by the runtime's `TryInto`
    /// and never reaches `update`, so the registry has to be cleaned up here
    /// rather than in a match arm. `Message::SurfaceClosed` (from
    /// `window::close_events()`) removes the entry too — the two are
    /// belt-and-braces, and `HashMap::remove` on an absent key is a no-op, so
    /// the overlap is harmless.
    ///
    /// Unused until Stage 16, whose popover dismissal is its first (and so
    /// far only) caller.
    fn remove_surface(&mut self, id: window::Id) -> Task<Message> {
        self.windows.remove(&id);
        Task::done(Message::RemoveWindow(id))
    }

    /// Delegates by pattern-matching straight through both enum layers at
    /// once: `Message::Battery(battery::Message::Updated(battery))` reaches
    /// into the nested module message in the same match arm that unwraps
    /// the outer one — there's no separate "forward to a per-module
    /// `update` method" indirection, because these two modules have nothing
    /// to do with their message besides storing the payload. A module with
    /// genuinely module-local state transitions (e.g. a future popover
    /// toggling open/closed) would instead forward its nested message to a
    /// `self.<module>.update(msg)` call here — this flat match is the right
    /// shape only while "store the latest snapshot" is the whole story.
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // Store the worker's snapshot; the runtime re-renders after
            // every update, so `view` picks it up immediately.
            Message::Columns(modules::columns::Message::Updated(columns)) => {
                self.columns = columns;
                Task::none()
            }
            // The niri bridge's other half. Delegated to the module (the
            // shape `claude.rs` established) rather than destructured here:
            // what a title message means to the module's state is the
            // module's business.
            Message::WindowTitle(message) => {
                self.window_title.update(message);
                Task::none()
            }
            Message::Media(modules::media::Message::Updated(media)) => {
                self.media = media;
                Task::none()
            }
            Message::Volume(modules::volume::Message::Updated(volume)) => {
                self.volume = volume;
                Task::none()
            }
            // Stage 17's command channel. Recorded exactly once (the
            // worker thread emits `Ready` a single time, at boot) and read
            // by the two arms below.
            Message::Volume(modules::volume::Message::Ready(sender)) => {
                self.volume_commands = Some(sender);
                Task::none()
            }
            // The bar's mute button and the quick-settings mute toggle
            // both land here. The negation is decided *here*, against
            // `self.volume`'s current state, rather than the trigger
            // carrying a target value — see `volume::Message::ToggleMute`'s
            // doc comment for why that's the one place this decision
            // should live. A missing `volume_commands` (pulse never
            // reachable, or its self-pipe failed at boot) makes this a
            // silent no-op, same as every other absent-capability path.
            Message::Volume(modules::volume::Message::ToggleMute) => {
                if let Some(sender) = &self.volume_commands {
                    sender.send(modules::volume::Command::SetMute(!self.volume.muted()));
                }
                Task::none()
            }
            // The quick-settings slider's `on_change`.
            Message::Volume(modules::volume::Message::SetVolume(percent)) => {
                if let Some(sender) = &self.volume_commands {
                    sender.send(modules::volume::Command::SetVolume(percent));
                }
                Task::none()
            }
            // Stage 17's command-out D-Bus calls: each resolves to a
            // one-shot `Task` that connects, calls, and drops the
            // connection (see `modules::media`'s "command-out pattern"
            // section) — `.map(Message::Media)` lifts it the same way
            // every module's subscription/view already is, even though
            // these particular tasks never actually produce a
            // `media::Message` (`Task::future(..).discard()`'s whole
            // point).
            Message::Media(modules::media::Message::PlayPause(bus_name)) => {
                modules::media::play_pause(bus_name).map(Message::Media)
            }
            Message::Media(modules::media::Message::Next(bus_name)) => {
                modules::media::next(bus_name).map(Message::Media)
            }
            Message::Media(modules::media::Message::Previous(bus_name)) => {
                modules::media::previous(bus_name).map(Message::Media)
            }
            Message::Battery(modules::battery::Message::Updated(battery)) => {
                self.battery = battery;
                Task::none()
            }
            Message::Network(modules::network::Message::Updated(network)) => {
                self.network = network;
                Task::none()
            }
            Message::Bluetooth(modules::bluetooth::Message::Updated(bluetooth)) => {
                self.bluetooth = bluetooth;
                Task::none()
            }
            Message::Power(modules::power::Message::Updated(power)) => {
                self.power = power;
                Task::none()
            }
            // A quick-settings profile chip. Command-out, exactly like the
            // media transport arms above: a one-shot `Task` that connects,
            // writes the `ActiveProfile` property, and drops the connection,
            // `.map(Message::Power)`ed for uniformity even though
            // `Task::future(..).discard()` never actually produces a
            // `power::Message`. The UI is *not* updated optimistically here
            // — the daemon's own `PropertiesChanged` is what moves the
            // selected chip, so a write polkit refuses leaves the popover
            // showing the truth rather than a lie.
            Message::Power(modules::power::Message::SetProfile(profile)) => {
                modules::power::set_profile(profile).map(Message::Power)
            }
            Message::Brightness(modules::brightness::Message::Updated(brightness)) => {
                self.brightness = brightness;
                Task::none()
            }
            // The quick-settings brightness slider. Command-out like the two
            // arms above, with one wrinkle of its own: the message carries
            // only a *percent*, because the popover has no business knowing
            // this backlight's driver scale (see
            // `modules::brightness::Message::SetBrightness`). The device name
            // and that scale come off the last snapshot stored just above,
            // which is also what makes the not-present case a genuine no-op
            // rather than a guess — a machine with no backlight has nothing
            // to address the call to, so the slider (which is not drawn at
            // all in that case) degrades quietly, the same contract as
            // `volume_commands` being `None`.
            Message::Brightness(modules::brightness::Message::SetBrightness(percent)) => {
                if self.brightness.is_present() {
                    modules::brightness::set_brightness(
                        self.brightness.device().to_owned(),
                        self.brightness.raw_max(),
                        percent,
                    )
                    .map(Message::Brightness)
                } else {
                    Task::none()
                }
            }
            // The one module that *delegates* rather than storing a
            // snapshot: `claude::Message` carries both a new session list
            // and the frames of the dot row's breathing animation, and
            // folding a tick into an animation epoch is this module's own
            // business, not the panel's. So the outer variant is unwrapped
            // and the inner value handed straight to the module (see
            // `modules::claude::ClaudeCode::update`) — the per-module
            // refactor's endpoint, and the shape the remaining modules
            // would take if they ever grew logic of their own.
            Message::ClaudeCode(message) => {
                self.claude_code.update(message);
                Task::none()
            }
            Message::Tray(modules::tray::Message::Updated(tray)) => {
                self.tray = tray;
                Task::none()
            }
            // Stage 19's command-out D-Bus calls: `modules::tray::activate`/
            // `scroll` each resolve to a one-shot `Task` that connects,
            // calls, and drops the connection (see `modules::tray`'s module
            // doc comment and `modules::media`'s "command-out pattern"
            // section, which this copies verbatim) — `.map(Message::Tray)`
            // lifts it the same way every module's subscription/view
            // already is. `scroll`'s task never produces a message;
            // `activate`'s produces exactly one — `ContextMenu`, routed
            // back through this match to the arm below — when the item
            // answers `UnknownMethod` (a menu-only item that never
            // declared `ItemIsMenu`; see `modules::tray::activate`'s doc
            // comment for the fallback).
            Message::Tray(modules::tray::Message::Activate(id)) => {
                modules::tray::activate(id).map(Message::Tray)
            }
            Message::Tray(modules::tray::Message::Scroll(id, delta)) => {
                modules::tray::scroll(id, delta).map(Message::Tray)
            }
            // A right-click opens (or refreshes, or closes) the tray-menu
            // popover for that item — see `Panel::open_tray_menu`.
            Message::Tray(modules::tray::Message::ContextMenu(id)) => self.open_tray_menu(id),
            // The initial (or a live-refresh) fetch answered. Applied only if
            // it's still for the item the popover currently shows — see
            // `popovers::tray_menu::Message::Loaded`'s doc comment for the
            // race this guards against.
            Message::TrayMenu(popovers::tray_menu::Message::Loaded(item_id, menu)) => {
                if self.tray_menu.item_id() == Some(item_id.as_str()) {
                    self.tray_menu.set_menu(menu);
                }
                Task::none()
            }
            // A leaf row was clicked: tell the application, then close the
            // popover — PLAN.md Stage 21's "click a leaf → `Event(\"clicked\")`
            // + close". The click is fire-and-forget (`send_clicked`'s own
            // contract); the close goes through `PopoverManager::close`
            // rather than re-`Triggered`ing (which would *toggle*, and if a
            // different popover had somehow become the open one in the
            // meantime — it can't, in practice, but see `PopoverManager::
            // close`'s doc comment — would close the wrong one).
            Message::TrayMenu(popovers::tray_menu::Message::RowActivated(node_id)) => {
                let click = self
                    .tray_menu
                    .item_id()
                    .map(|id| {
                        Task::future(modules::tray::menu::send_clicked(id.to_string(), node_id))
                            .discard()
                    })
                    .unwrap_or_else(Task::none);
                let close_action = self.popovers.close(PopoverKind::TrayMenu);
                let close = self.apply_popover_action(close_action);
                self.tray_menu = popovers::tray_menu::TrayMenuState::default();
                Task::batch([click, close])
            }
            // A submenu row was clicked: flip it open/closed inline, fetching
            // its children only in the lazily-populated case (see
            // `TrayMenuState::toggle_expanded`'s doc comment).
            Message::TrayMenu(popovers::tray_menu::Message::ToggleSubmenu(node_id)) => {
                let Some(item_id) = self.tray_menu.item_id().map(str::to_string) else {
                    return Task::none();
                };
                if self.tray_menu.toggle_expanded(node_id) {
                    fetch_tray_submenu(item_id, node_id)
                } else {
                    Task::none()
                }
            }
            // The lazy submenu fetch answered; graft it onto the tree (a
            // no-op if the row is gone, e.g. a refresh replaced the tree
            // meanwhile — see `TrayMenuState::merge_submenu`).
            Message::TrayMenu(popovers::tray_menu::Message::SubmenuLoaded(node_id, menu)) => {
                self.tray_menu.merge_submenu(node_id, menu);
                Task::none()
            }
            // A surface reported itself. `or_insert` rather than `insert` is
            // load-bearing: a surface *we* spawned had its role recorded at
            // spawn time (`Panel::spawn_surface`), and this event arrives
            // afterwards — overwriting would demote every island and popover
            // back to `Bar` the moment it appeared. So the only Id this arm
            // ever actually writes is one nobody could have pre-registered,
            // which is exactly the surface `Settings` created at boot — the
            // ledger bar, or the islands strip in islands style
            // (`initial_role`). (With
            // `StartMode::AllScreens` there would be one such surface per
            // output, and each would land here with that same role — the
            // registry is already shaped for that.)
            Message::SurfaceOpened(id) => {
                // Bound to a local first: `entry(..)` takes `&mut
                // self.windows` for the rest of the statement, and reading
                // `self.config` inside the `or_insert` argument would be a
                // second borrow of `self` in the same expression.
                let role = initial_role(&self.config);
                self.windows.entry(id).or_insert(role);
                Task::none()
            }
            // Drop the role so the registry tracks reality. Verified against
            // `iced_layershell-0.19.1/src/multi_window.rs`: the runtime only
            // broadcasts `Closed` for a surface that carries an Id *binding*,
            // and only surfaces created through `NewLayerShell`/`NewPopUp`/…
            // get one — the initial `Settings`-created surface never does. So
            // this arm will never fire for the bar. That is fine (the bar
            // outlives the process) but it is a real asymmetry worth knowing
            // before relying on `SurfaceClosed` as a general lifecycle hook.
            Message::SurfaceClosed(id) => {
                self.windows.remove(&id);
                // Belt-and-braces for the popover invariant: every dismissal
                // path already cleared the manager before asking for the
                // removal, but a surface that goes away some *other* way (the
                // compositor closing it, an output vanishing) must not leave
                // the manager believing a dead surface is still open. The Id
                // check inside `closed` is what keeps a sibling swap's
                // outgoing `Closed` from clearing the incoming popover.
                self.popovers.closed(id);
                Task::none()
            }
            // `panel.kdl` changed on disk: the watcher already read and
            // parsed it (see `config_watch`); this arm's job is to make the
            // running panel *match* the new config — the live counterpart of
            // everything `main` + `Panel::new` derived from the boot read.
            Message::Config(config_watch::Message::Reloaded(new_config)) => {
                self.reload_config(new_config)
            }
            // The mark was clicked. Only ever reaches here when
            // `self.mark`'s configured launcher is `Some(..)` — `Mark::view`
            // doesn't build a clickable button at all otherwise, so there is
            // no widget that could have sent this.
            //
            // Deliberately synchronous, unlike the D-Bus command-out arms
            // above (`Media::PlayPause`, `Tray::Activate`, …): those return a
            // `Task` because a zbus call is itself async work the runtime
            // needs to drive. Spawning a local child process has no such
            // need — `std::process::Command::spawn` returns as soon as the
            // OS has forked/exec'd the new process, without waiting for it
            // to do anything — so this arm just does the spawn inline and
            // returns `Task::none()`, the same as every other "store some
            // state and move on" arm above.
            Message::Mark(modules::mark::Message::Pressed) => {
                let Some(launcher) = self.mark.launcher() else {
                    // Shouldn't happen (see this arm's opening comment), but
                    // a stale/impossible message is a no-op, not a panic —
                    // the same defensive posture the rest of `update` takes
                    // toward messages that "can't" occur.
                    return Task::none();
                };
                // Toggle: if the launcher we spawned last click is still
                // running, this click *closes* it rather than stacking a
                // second copy on screen.
                //
                // Teaching note (`try_wait` vs `wait`): `wait()` blocks
                // until the child exits — unusable on the UI thread.
                // `try_wait()` is its non-blocking probe: `Ok(Some(status))`
                // means "already exited" (and, crucially, *reaps* it — the
                // kernel drops the process-table entry as a side effect of
                // the successful wait), `Ok(None)` means "still running".
                // So this one call is both our liveness check and our
                // zombie cleanup for a launcher that closed itself (Esc in
                // fuzzel, or picking an app) since the last click.
                if let Some(child) = self.launcher_child.as_mut() {
                    if matches!(child.try_wait(), Ok(None)) {
                        // Still running → this click is the "close" half of
                        // the toggle. `Child::kill` is SIGKILL (std offers
                        // no gentler signal), which is fine for a launcher:
                        // it has no state to save, and the compositor tears
                        // down its surface either way. The follow-up
                        // `wait()` is safe on the UI thread here — a
                        // SIGKILLed process is gone effectively
                        // immediately, and waiting is what reaps it.
                        let mut child = self.launcher_child.take().expect("checked above");
                        let _ = child.kill();
                        let _ = child.wait();
                        return Task::none();
                    }
                    // Exited on its own (or `try_wait` errored, meaning
                    // there is no child to speak of) → forget it and fall
                    // through to a fresh spawn.
                    self.launcher_child = None;
                }
                // Teaching note: no shell, no quoting. This is deliberately
                // the simplest possible command-line split — the first
                // whitespace-separated token is the program,
                // everything after is its arguments verbatim. It cannot run
                // `sh -c "…"` semantics (pipes, quoted arguments containing
                // spaces, `$HOME` expansion, …); `panel.kdl`'s `launcher`
                // directive is documented as a plain argv list for exactly
                // this reason. Good enough for `"fuzzel"` or `"wofi --show
                // drun"`; a user who needs a shell pipeline can always point
                // `launcher` at a small wrapper script instead.
                let mut parts = launcher.split_whitespace();
                let Some(program) = parts.next() else {
                    // An empty (or all-whitespace) `launcher` string. Warn
                    // once per click rather than silently doing nothing —
                    // this is a config problem, not a transient condition.
                    eprintln!("saola-panel: launcher command is empty — nothing to run");
                    return Task::none();
                };
                match std::process::Command::new(program).args(parts).spawn() {
                    Ok(child) => {
                        // A spawned `std::process::Child` becomes a zombie
                        // process (reaped by nobody, entry lingering in the
                        // process table) the moment it exits, unless
                        // something eventually calls `.wait()`/`try_wait()`
                        // on it. This used to be a fire-and-forget reaper
                        // thread; now that the toggle needs to *ask* whether
                        // the launcher is still up, the `Child` lives in
                        // `self.launcher_child` instead, and reaping happens
                        // at the two places that already probe it with
                        // `try_wait`: the next mark click (above) and the
                        // clock's minute tick (the catch-up reap in the
                        // `Message::Clock` arm — no new timer, per the
                        // "every module maps to a signal" rule).
                        self.launcher_child = Some(child);
                    }
                    Err(err) => {
                        eprintln!("saola-panel: failed to launch \"{launcher}\": {err}");
                    }
                }
                Task::none()
            }
            // The claude group's trigger. Routed to its own opener rather
            // than the generic arm below because opening this popover also
            // kicks off the transcript-read task — the same "trigger +
            // fetch in one update" shape `open_tray_menu` established for
            // right-clicks (this specific pattern must stay above the
            // catch-all `Message::Popover(message)` arm to be reachable).
            Message::Popover(popover::Message::Triggered(PopoverKind::ClaudeUsage)) => {
                self.open_claude_usage()
            }
            // The transcript reads answered. Applied only while the usage
            // popover is still the open one — a read that lands after
            // dismissal (or after a reopen already reset the state) must
            // not resurrect a stale snapshot, the same guard as
            // `TrayMenu::Loaded`'s item-id check above.
            Message::ClaudeUsage(popovers::claude_usage::Message::Loaded(sessions)) => {
                if self.popovers.is_open(PopoverKind::ClaudeUsage) {
                    self.claude_usage.set_loaded(sessions);
                }
                Task::none()
            }
            // The only messages in this enum that *do* something rather than
            // storing something. Delegated rather than inlined because the
            // decision (what closes what) is a state machine worth testing on
            // its own, and only the surface plumbing needs `Panel`.
            Message::Popover(message) => self.update_popover(message),
            // `Message::Clock(clock::Message::Tick)` carries no state to
            // store (see `modules::clock::Message`'s doc comment) —
            // reaching `update` at all is what wakes the re-render. It
            // does double duty as the launcher's catch-up reaper: if the
            // launcher exited on its own (Esc in fuzzel), nothing else
            // would `wait()` on it until the next mark click, so it would
            // sit in the process table as a zombie indefinitely. The
            // minute tick already arrives as a signal — piggybacking one
            // non-blocking `try_wait` probe on it (which reaps on
            // `Ok(Some(_))`, see the mark arm's teaching note) bounds that
            // zombie's lifetime to a minute without adding any timer of
            // our own.
            Message::Clock(modules::clock::Message::Tick) => {
                if let Some(child) = self.launcher_child.as_mut() {
                    if !matches!(child.try_wait(), Ok(None)) {
                        self.launcher_child = None;
                    }
                }
                Task::none()
            }
            // The macro-injected layer-shell variants never reach here
            // (see the outer `Message` doc comment) — they fall through to
            // this catch-all.
            _ => Task::none(),
        }
    }

    /// Make the running panel match a freshly reloaded `panel.kdl` — the
    /// live counterpart of everything `main` + [`Panel::new`] derived from
    /// the boot read, in the same order.
    ///
    /// # What has to be re-derived, and what doesn't (teaching note)
    ///
    /// Most of the panel reads `self.config`/`self.theme` **live, on every
    /// frame** — the module lists in `bar_view`/`islands_view`, every color
    /// and size in every `view` — so for those, swapping the two fields *is*
    /// the reload; the next render simply reads the new values. Only three
    /// kinds of state were **copied out of** the config at boot and would
    /// otherwise keep the old values:
    ///
    /// 1. **Config-fed module state** — the mark's glyph/launcher, the
    ///    window title's knobs, the claude row's brand icon, each handed to
    ///    the module at construction. The mark is rebuilt outright (it holds
    ///    nothing but config); the other two get targeted setters so the
    ///    state their *signals* built up — the title on screen, the session
    ///    dots — survives the reload instead of blanking until the next
    ///    event happens to re-announce it.
    /// 2. **The panel surface's layer-shell geometry** — anchor, size,
    ///    margins, exclusive zone, all fixed when the surface was created.
    ///    The `#[to_layer_message(multi)]` control variants exist for
    ///    exactly this: each is intercepted by the runtime (never reaching
    ///    `update`) and re-issues the corresponding protocol request on the
    ///    live surface. A style flip (ledger ↔ islands) additionally
    ///    re-roles the surface in `windows`, which is all `view` needs to
    ///    start drawing the other layout on it — both styles have been one
    ///    full-width surface since 2026-08-01, so no surface is created or
    ///    destroyed.
    /// 3. **An open popover** — its surface geometry was derived from the
    ///    old config at spawn time, so if the panel moved, the popover would
    ///    be left hanging where the panel used to be. Dismissing it (only
    ///    when the geometry actually changed) is simpler and more honest
    ///    than teaching every popover to migrate; reopening is one click.
    ///
    /// The theme is rebuilt **from `Theme::saola()`**, not by mutating
    /// `self.theme`: `ColorOverrides::apply` only writes the fields that are
    /// `Some`, so applying a new config's overrides onto the already-
    /// overridden palette could never *revert* a color the user deleted from
    /// `colors { }`.
    ///
    /// CLI flags are re-applied first — `self.cli` outranks the file on
    /// every read, not just the boot one (see that field's doc comment).
    fn reload_config(&mut self, mut new_config: config::PanelConfig) -> Task<Message> {
        self.cli.apply(&mut new_config);
        // The watcher reloads on any change to the file, including edits
        // that resolve to the identical config (whitespace, comments, a
        // knob rewritten to its default). Nothing below is *unsafe* to run
        // then, but skipping keeps a no-op save from resetting an in-flight
        // marquee sweep or dismissing an open popover.
        if new_config == self.config {
            return Task::none();
        }

        let mut theme = Theme::saola();
        new_config.colors.apply(&mut theme.palette);

        self.mark = Mark::new(new_config.mark.clone(), new_config.launcher.clone());
        self.window_title.set_config(new_config.window_title);
        self.claude_code.set_icon(new_config.claude_icon);

        let old_role = initial_role(&self.config);
        let new_role = initial_role(&new_config);
        let old_geometry = SurfaceGeometry::of(old_role, &self.config, &self.theme);
        let new_geometry = SurfaceGeometry::of(new_role, &new_config, &theme);

        self.theme = theme;
        self.config = new_config;

        if new_geometry == old_geometry && new_role == old_role {
            // A content-only change (module lists, mark, colors, …): the
            // next render picks it up from the swapped fields, and the
            // surface itself is exactly where it should be.
            return Task::none();
        }

        // Collected before the loop below because re-roling mutates
        // `windows` while iterating would be a second borrow. Plural on
        // purpose: with `StartMode::AllScreens` there would be one panel
        // surface per output, and each needs the same re-role + re-geometry.
        let panel_ids: Vec<window::Id> = self
            .windows
            .iter()
            .filter(|(_, role)| matches!(role, SurfaceRole::Bar | SurfaceRole::Islands))
            .map(|(id, _)| *id)
            .collect();

        let mut tasks = Vec::new();
        for id in panel_ids {
            self.windows.insert(id, new_role);
            tasks.push(Task::done(Message::AnchorChange {
                id,
                anchor: new_geometry.anchor,
            }));
            tasks.push(Task::done(Message::SizeChange {
                id,
                size: new_geometry.size,
            }));
            tasks.push(Task::done(Message::MarginChange {
                id,
                margin: new_geometry.margin,
            }));
            tasks.push(Task::done(Message::ExclusiveZoneChange {
                id,
                zone_size: new_geometry.exclusive_zone,
            }));
        }

        // Same no-kind-check dismissal as Escape; see `PopoverManager::
        // close_any`'s doc comment for why a moved panel orphans its
        // popover. `Action::None` (nothing open) makes this a no-op.
        let action = self.popovers.close_any();
        tasks.push(self.apply_popover_action(action));
        // And the popover *content* state goes with the surface: leaving
        // `tray_menu` populated would keep its Id-keyed dbusmenu watcher
        // (see the `run_with` arm in `Panel::subscription`) alive for a
        // popover that no longer exists, and let a straggler `Loaded` fold
        // menu data into dead state. Both resets are no-ops when the
        // dismissed popover wasn't theirs (the fields were already
        // default).
        self.tray_menu = popovers::tray_menu::TrayMenuState::default();
        self.claude_usage = popovers::claude_usage::ClaudeUsageState::default();

        Task::batch(tasks)
    }

    /// Turn a popover decision into surfaces.
    ///
    /// [`PopoverManager::update`] answers *what should happen*; this method
    /// is the only place that answer becomes a real layer-shell surface. The
    /// split is deliberate — see that method's doc comment — and it is what
    /// keeps every dismissal rule testable without a compositor.
    ///
    /// # The three-step open (teaching note)
    ///
    /// Opening is `decide → spawn → record`, all inside this one `update`
    /// call, and the order matters:
    ///
    /// 1. `spawn_surface` calls `Message::layershell_open`, which **mints the
    ///    `window::Id` itself** and hands it back with the `Task` that asks
    ///    for the surface (Stage 13's handoff has the full story). So the Id
    ///    exists before the surface does.
    /// 2. It registers `SurfaceRole::Popover(kind)` against that Id, which is
    ///    what lets `Panel::view` draw the popover on the surface's very
    ///    first frame — the runtime builds a surface's user interface before
    ///    it broadcasts the surface's `Opened` event, so waiting for
    ///    `Message::SurfaceOpened` would be a frame too late.
    /// 3. `PopoverManager::opened` records the same Id, so the next trigger
    ///    click knows what to close.
    ///
    /// A sibling swap issues the removal and the spawn in one `Task::batch`.
    /// Both are Id-carrying layer-shell actions, and the runtime queues any
    /// that arrive before their surface exists rather than dropping them
    /// (`multi_window.rs:708–715`), so there is no ordering hazard between
    /// them.
    fn update_popover(&mut self, message: popover::Message) -> Task<Message> {
        let action = self.popovers.update(message);
        self.apply_popover_action(action)
    }

    /// Turn a [`popover::Action`] into surfaces — the part [`Self::
    /// update_popover`] shares with [`Self::open_tray_menu`], which reaches
    /// an [`popover::Action`] via [`popover::PopoverManager::close`] rather
    /// than [`popover::PopoverManager::update`] (a tray-menu row click closes
    /// the popover as a side effect of content *inside* it, not one of the
    /// trigger/Escape/focus-loss paths `update` itself covers).
    fn apply_popover_action(&mut self, action: popover::Action) -> Task<Message> {
        match action {
            popover::Action::None => Task::none(),
            popover::Action::Close(id) => self.remove_surface(id),
            popover::Action::Open { kind, close } => {
                let close = close
                    .map(|id| self.remove_surface(id))
                    .unwrap_or_else(Task::none);

                let role = SurfaceRole::Popover(kind);
                let geometry = SurfaceGeometry::of(role, &self.config, &self.theme);
                let (id, open) = self.spawn_surface(role, geometry.new_layer_shell_settings());
                self.popovers.opened(id, kind);

                Task::batch([close, open])
            }
        }
    }

    /// A tray item's icon was right-clicked: open, refresh, or close the
    /// tray-menu popover for it.
    ///
    /// # Three cases
    ///
    /// 1. **The tray menu is already open for this same item** — a second
    ///    right-click on the icon whose menu is already showing. Goes
    ///    through the ordinary trigger lifecycle
    ///    (`popover::Message::Triggered`), which toggles the same kind
    ///    *closed* — the same "click again to dismiss" every other popover
    ///    trigger already gives.
    /// 2. **The tray menu is open for a *different* item.** `popover::
    ///    PopoverManager::update`'s same-kind rule would otherwise *close*
    ///    it (same kind re-triggered ⇒ close), forcing a second right-click
    ///    just to see the newly-clicked item's menu. Instead the surface
    ///    stays exactly as it is and only `self.tray_menu`'s content is
    ///    swapped — the spec's one-open-at-a-time rule is about how many
    ///    *surfaces* exist, not about which item's content one of them is
    ///    currently showing.
    /// 3. **Nothing is open, or `QuickSettings` is** — the ordinary trigger
    ///    lifecycle, which closes `QuickSettings` first if it was the
    ///    incumbent (the spec's global rule, unchanged since Stage 16).
    ///
    /// Every case (re)sets `self.tray_menu` and kicks off the initial fetch
    /// via [`fetch_tray_menu`] — case 1 is the exception: closing needs
    /// neither, so it returns before either happens.
    fn open_tray_menu(&mut self, id: String) -> Task<Message> {
        if self.popovers.is_open(PopoverKind::TrayMenu) {
            if self.tray_menu.item_id() == Some(id.as_str()) {
                self.tray_menu = popovers::tray_menu::TrayMenuState::default();
                return self.update_popover(popover::Message::Triggered(PopoverKind::TrayMenu));
            }

            self.tray_menu = popovers::tray_menu::TrayMenuState::opening(id.clone());
            return fetch_tray_menu(id);
        }

        self.tray_menu = popovers::tray_menu::TrayMenuState::opening(id.clone());
        Task::batch([
            self.update_popover(popover::Message::Triggered(PopoverKind::TrayMenu)),
            fetch_tray_menu(id),
        ])
    }

    /// The claude group's trigger was clicked: toggle the usage popover,
    /// and — when the click *opened* it — snapshot the session list and
    /// kick off the transcript reads.
    ///
    /// The toggle itself is the ordinary `PopoverManager` lifecycle
    /// (second click closes, quick settings/tray menus are displaced —
    /// nothing new). What this method adds is the fetch: `open_tray_menu`'s
    /// "trigger + fetch in one update" shape, minus the per-item cases a
    /// tray has and a usage readout doesn't. Ordering note: `is_open` is
    /// asked *after* `update_popover` has run, so it reports the toggle's
    /// outcome — open means this click opened it.
    fn open_claude_usage(&mut self) -> Task<Message> {
        let toggle = self.update_popover(popover::Message::Triggered(PopoverKind::ClaudeUsage));
        if self.popovers.is_open(PopoverKind::ClaudeUsage) {
            self.claude_usage = popovers::claude_usage::ClaudeUsageState::opening();
            Task::batch([toggle, fetch_claude_usage(self.claude_code.usage_targets())])
        } else {
            // The click closed it: drop the stale numbers so the next open
            // starts from its loading line rather than flashing them.
            self.claude_usage = popovers::claude_usage::ClaudeUsageState::default();
            toggle
        }
    }

    /// Bridge the Saola theme to iced's built-in theme type so unstyled or
    /// third-party widgets still land inside the Saola palette. Widgets we
    /// style ourselves use the `saola_theme::style` helpers instead and
    /// never read this.
    ///
    /// Takes a `window::Id` since the daemon conversion (Stage 13) because a
    /// daemon can theme its surfaces differently. The panel deliberately
    /// doesn't: every Saola surface reads the same palette, and the bar's
    /// "always ink" rule (CLAUDE.md) is expressed through
    /// `style::container::bar_pill`, not through swapping themes. Hence
    /// the ignored parameter rather than a lookup in `self.windows`.
    fn theme(&self, _id: window::Id) -> iced::Theme {
        to_iced_theme(&self.theme)
    }

    /// The app-wide surface appearance: what the renderer clears a surface
    /// to *before* any widget draws on it.
    ///
    /// **The background is transparent, in both layout styles.** This is
    /// load-bearing for Islands — an island surface spans the whole output
    /// and only the scrim pill inside it should be visible, so everything
    /// around that pill has to be nothing at all rather than a wall of ink.
    ///
    /// It also fixes a ledger-mode bug shipped by the Stage 14.5 pass, which
    /// this stage caught by screenshot: iced's default `Appearance` clears to
    /// the theme's background colour (`to_iced_theme` maps that to
    /// `palette.ink`), so the *whole* bar surface was painted solid ink and
    /// `style::container::bar_pill`'s `radii.pill` rounding was drawn ink on
    /// ink — invisible. The floating bar was rendering as a square-cornered
    /// slab, contradicting both CLAUDE.md's "everything is a pill" rule and
    /// the mockup Stage 14.5 measured the geometry from. Clearing to
    /// transparent lets the wallpaper reach the four corners the pill's
    /// radius carves out, which is what makes it read as a pill at all.
    ///
    /// Teaching note on the signature: `.style(..)` is the one builder hook
    /// that is *not* per-surface — `Fn(&State, &Theme) -> Appearance`, no
    /// `window::Id` (`build_pattern/daemon.rs:756`). One appearance covers
    /// every surface the daemon owns. That is fine here because the answer
    /// is the same for all of them, but a future surface that needed an
    /// opaque clear could not get one this way; it would have to paint its
    /// own opaque container (which is exactly what `container::popover`,
    /// `bar_pill` and `translucent_panel` all do anyway).
    ///
    /// `text_color` is deliberately left at whatever the theme's default
    /// `Appearance` says, so this changes exactly one thing.
    fn style(&self, theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: iced::Color::TRANSPARENT,
            ..iced::theme::default(theme)
        }
    }

    /// Merges every module's subscription into the panel's subscription
    /// set: the clock's minute tick, the mark's `Subscription::none()`, the
    /// media/battery/network D-Bus feeds, and the volume module's pulse
    /// thread bridge.
    ///
    /// Each module's `subscription()` returns `Subscription<module::
    /// Message>` — a different type per module — so `Subscription::batch`
    /// (which needs one uniform item type) can't take them directly.
    /// `.map(Message::Battery)` lifts `Subscription<battery::Message>` to
    /// `Subscription<Message>` by wrapping every value the stream produces
    /// in that variant; same for the other two. Teaching note (identity
    /// survives `.map`): `battery::Battery::subscription`'s doc comment
    /// explains why `Subscription::run(battery_stream)` is keyed on the
    /// `battery_stream` fn pointer so re-subscribing doesn't spawn a second
    /// worker — `.map` doesn't touch that key at all, it just composes a
    /// mapper function onto the stream the key identifies, so the identity
    /// (and the "one worker" guarantee) passes through unchanged.
    ///
    /// Stage 13 adds the two surface-lifecycle subscriptions at the end.
    /// They are not module signals — they are the daemon telling us about its
    /// own surfaces — but they belong in the same batch because iced has
    /// exactly one subscription set per application, not one per surface.
    /// Neither polls: `window::open_events()` and `window::close_events()`
    /// are filters over the runtime's existing event broadcast, so they emit
    /// only when a surface actually appears or disappears, which satisfies
    /// CLAUDE.md's "every module maps to a signal, never a poll" rule.
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            self.clock.subscription().map(Message::Clock),
            // The panel's single `$NIRI_SOCKET` connection, feeding two
            // modules: `modules::niri` owns the socket and the fold, and
            // hands back each module's *own* message pre-built, so this is
            // pure routing — one arm per consumer, no niri knowledge on this
            // side (see that module's doc comment). Neither module's own
            // `subscription()` carries a niri signal, and both are still
            // listed below so the batch reads uniformly.
            modules::niri::subscription().map(|message| match message {
                modules::niri::Message::Columns(message) => Message::Columns(message),
                modules::niri::Message::WindowTitle(message) => Message::WindowTitle(message),
            }),
            self.columns.subscription().map(Message::Columns),
            // `Subscription::none()` in every default configuration: this one
            // is the opt-in marquee's animation timer (style guide §5), gated
            // exactly like `claude_code`'s breath below — it exists only while
            // `overflow "marquee"` is configured *and* the title on screen
            // actually overflows. See `WindowTitle::subscription`.
            self.window_title.subscription().map(Message::WindowTitle),
            // Always `Subscription::none()` (see `Mark::subscription`'s doc
            // comment) — included so the batch's shape stays uniform across
            // every module rather than special-casing the one with no
            // signal source.
            self.mark.subscription().map(Message::Mark),
            self.media.subscription().map(Message::Media),
            self.volume.subscription().map(Message::Volume),
            self.battery.subscription().map(Message::Battery),
            self.network.subscription().map(Message::Network),
            self.bluetooth.subscription().map(Message::Bluetooth),
            // No bar module produces this one — `power` feeds the
            // quick-settings popover only (see `Message::Power`). It still
            // belongs in this batch rather than somewhere popover-local:
            // iced has exactly one subscription set per *application*, and a
            // worker that only ran while a popover happened to be open would
            // reconnect to D-Bus on every open.
            self.power.subscription().map(Message::Power),
            // Popover-only too (see the note on `power` just above — the same
            // "one subscription set per application" reasoning applies). This
            // one's worker is an OS thread rather than an async task, which
            // makes belonging to this batch load-bearing rather than merely
            // tidy: a subscription torn down and rebuilt per popover-open
            // would spawn a fresh thread each time.
            self.brightness.subscription().map(Message::Brightness),
            self.claude_code.subscription().map(Message::ClaudeCode),
            self.tray.subscription().map(Message::Tray),
            // `panel.kdl` live-reload. Not a module signal either — it feeds
            // the whole panel, not one field — but a signal all the same:
            // inotify pushes file-change events, so an untouched config
            // costs nothing (see `config_watch`'s doc comment, including
            // why the debounce sleep is gated, not standing). Watches the
            // path `main` resolved once at boot; an environment where no
            // config path resolves at all gets no watcher, the same shape
            // as the tray-menu arm below.
            match &self.config_path {
                Some(path) => config_watch::subscription(path).map(Message::Config),
                None => Subscription::none(),
            },
            window::open_events().map(Message::SurfaceOpened),
            window::close_events().map(Message::SurfaceClosed),
            // Stage 16's two dismissal signals (Escape, and a surface losing
            // keyboard focus). Like the two above it, this is a filter over
            // the runtime's existing event broadcast rather than a poll —
            // see `popover::subscription` for the exact event chain and why
            // it has to be a plain `fn`.
            popover::subscription().map(Message::Popover),
            // Stage 21's live-refresh: `LayoutUpdated`/`ItemsPropertiesUpdated`
            // on the tray menu currently open, or nothing at all. Unlike
            // every subscription above (each watching one fixed service for
            // the process's whole life), *which* menu to watch changes at
            // runtime — see `popovers::tray_menu::watch`'s doc comment for
            // how `Subscription::run_with` keys on `item_id`'s value so
            // iced tears the stream down and spins a new one up exactly
            // when the open item changes, rather than polling.
            match self.tray_menu.item_id() {
                Some(id) => popovers::tray_menu::watch(id),
                None => Subscription::none(),
            },
        ])
    }

    /// The daemon's per-surface `view`: called once for **each** surface the
    /// process owns, with that surface's Id, every time the runtime needs to
    /// redraw it.
    ///
    /// The match is exhaustive over [`SurfaceRole`] on purpose — see that
    /// enum's doc comment. `Panel::role` folds the unknown-Id case into the
    /// boot surface's own role before we get here, so there is no "what is
    /// this?" branch to write.
    fn view(&self, id: window::Id) -> Element<'_, Message> {
        match self.role(id) {
            SurfaceRole::Bar => self.bar_view(),
            SurfaceRole::Islands => self.islands_view(),
            SurfaceRole::Popover(kind) => self.popover_view(kind),
        }
    }

    /// One popover's content, by kind — the `main.rs`-side counterpart of
    /// `module_view`'s name → view mapping just below, and exhaustive over
    /// [`PopoverKind`] for the same reason.
    ///
    /// Both kinds render their real content module directly rather than
    /// through `popover.rs` (which has been deleted from this file's
    /// dependency graph entirely as of Stage 21 — see that module's doc
    /// comment): `QuickSettings` needs `Volume`/`Media` state and `TrayMenu`
    /// needs `self.tray_menu`, both of which `popover.rs` deliberately never
    /// learns about (see `popovers::quick_settings`'s doc comment for why
    /// keeping the lifecycle manager ignorant of specific bar/popover state
    /// is the point).
    fn popover_view(&self, kind: PopoverKind) -> Element<'_, Message> {
        match kind {
            PopoverKind::QuickSettings => popovers::quick_settings::view(
                &self.theme,
                &self.power,
                &self.battery,
                &self.network,
                &self.bluetooth,
                &self.volume,
                &self.brightness,
                &self.media,
            ),
            PopoverKind::TrayMenu => popovers::tray_menu::view(&self.theme, &self.tray_menu),
            PopoverKind::ClaudeUsage => {
                // The rate-limit snapshot rides in from the module's state
                // by value (it's `Copy`) — unlike the per-session rows it
                // needs no click-time fetch, having already arrived by
                // signal (see `modules::claude`'s schema section).
                popovers::claude_usage::view(
                    &self.theme,
                    &self.claude_usage,
                    self.claude_code.usage(),
                )
            }
        }
    }

    /// The ledger bar's quick-settings trigger: the status cluster wrapped
    /// in a `bare`-styled button with the same geometry as the mark's
    /// (`mark::Mark::view` — `panel_pill_clock` tall, half-height
    /// horizontal padding), so hovering the cluster reveals the same
    /// subtle pill the mark shows. This replaced what used to be an
    /// invisible `mouse_area` (a `popover_trigger` helper, since removed:
    /// both layouts' triggers are buttons now — Jordan, 2026-08-01: the
    /// cluster was clickable but nothing told you so). The islands
    /// equivalent lives in `island_view`'s right arm, layered differently
    /// because its cluster sits inside a visible ink pill.
    ///
    /// `on_press` rather than `on_release` so the popover appears on the
    /// way down, which is also what makes the dismissal ordering safe —
    /// see `PopoverManager::update`'s note on why a trigger click can
    /// never be undone by the focus-loss event that follows it.
    ///
    /// Teaching note (a button *containing* buttons): the cluster's
    /// modules include real buttons of their own — volume's mute toggle,
    /// tray icons. Nesting them inside another button is safe for exactly
    /// the reason nesting them inside a `mouse_area` was: iced's `button`
    /// updates its content first and skips its own press handling when the
    /// child captured the event (`iced_widget-0.14.2/src/button.rs:297`,
    /// `shell.is_event_captured()` — the mirror image of
    /// `mouse_area.rs:241`). So clicking the volume glyph still only
    /// toggles mute; clicking anywhere else in the cluster opens quick
    /// settings.
    ///
    /// The presence gate exists because a button has a footprint even when
    /// its content is zero-sized: if every status module is absent
    /// (no pulse, no battery, no services at all), wrapping the empty row
    /// would leave an invisible 32px clickable circle at the bar's end
    /// that opens an empty popover — the ledger-bar cousin of the "phantom
    /// pill" `module_is_present` exists to prevent on islands. In that
    /// case the bare (zero-sized, unclickable) row renders instead.
    /// `modules` is the list `cluster` was actually built from — the
    /// *split* right region since 2026-08-01 (see [`Panel::
    /// right_region_split`]), not `config.right` wholesale, which now also
    /// contains the standalone claude and tray groups this gate must not
    /// count.
    fn status_cluster_trigger<'a>(
        &'a self,
        modules: &[config::ModuleName],
        cluster: iced::widget::Row<'a, Message>,
    ) -> Element<'a, Message> {
        let t = &self.theme;
        if !modules.iter().any(|name| self.module_is_present(*name)) {
            return cluster.into();
        }
        button(
            // Same `Fill`/`Center` trick as the mark: the row of bare
            // icons is shorter than the button, and only a container can
            // centre it inside the fixed-height hit area.
            container(cluster).height(Fill).align_y(iced::Center),
        )
        .height(t.sizes.panel_pill_clock)
        .padding([0.0, t.sizes.panel_pill_clock / 2.0])
        .style(style::button::bare(t, Surface::Ink))
        .on_press(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )))
        .into()
    }

    /// The right region, split into its standalone groups (2026-08-01,
    /// Jordan: the Claude Code dots are their own island immediately left
    /// of the tray): everything in `config.right` *except* `claude` and
    /// `tray` forms the status cluster (the quick-settings trigger),
    /// while those two — wherever they appear in the list — each render
    /// as their own group before it, in the fixed order (Jordan,
    /// 2026-08-01, superseding the earlier status-first order):
    /// claude, tray, status cluster. Claude gets its own trigger (the
    /// usage popover) so a click on its dots can't mean quick settings;
    /// tray icons are already their own buttons and need no trigger
    /// wrapper at all.
    ///
    /// Returns `(status modules, claude listed, tray listed)` — the two
    /// flags say "the config asked for it", and the callers still gate
    /// each group on `module_is_present` so an absent module costs no gap
    /// (ledger) and no pill (islands).
    fn right_region_split(&self) -> (Vec<config::ModuleName>, bool, bool) {
        let mut cluster = Vec::new();
        let mut claude = false;
        let mut tray = false;
        for name in &self.config.right {
            match name {
                config::ModuleName::Claude => claude = true,
                config::ModuleName::Tray => tray = true,
                other => cluster.push(*other),
            }
        }
        (cluster, claude, tray)
    }

    /// The ledger bar's claude group: the module's mark-and-dots view
    /// wrapped in the same `bare`-styled, `panel_pill_clock`-tall hover
    /// pill as [`Panel::status_cluster_trigger`] — but opening the usage
    /// popover instead of quick settings. Only built when the module is
    /// present (the caller's gate), so there is never an invisible
    /// clickable pill over an empty spot.
    fn claude_cluster_trigger(&self) -> Element<'_, Message> {
        let t = &self.theme;
        button(
            container(self.module_view(config::ModuleName::Claude))
                .height(Fill)
                .align_y(iced::Center),
        )
        .height(t.sizes.panel_pill_clock)
        .padding([0.0, t.sizes.panel_pill_clock / 2.0])
        .style(style::button::bare(t, Surface::Ink))
        .on_press(Message::Popover(popover::Message::Triggered(
            PopoverKind::ClaudeUsage,
        )))
        .into()
    }

    /// The islands claude pill: its own solid-ink island, layered exactly
    /// like the status island (ink `bar_pill` outside, `bare` trigger
    /// button inside carrying the content padding, so the hover tint
    /// paints over the ink — see `island_view`'s right-arm comment for why
    /// that order matters), opening the usage popover.
    fn claude_island_pill(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let content = container(self.module_view(config::ModuleName::Claude))
            .height(Fill)
            .align_y(iced::Center);
        container(
            button(content)
                .padding([0.0, t.sizes.panel_pill / 2.0])
                .height(Fill)
                .style(style::button::bare(t, Surface::Ink))
                .on_press(Message::Popover(popover::Message::Triggered(
                    PopoverKind::ClaudeUsage,
                ))),
        )
        .style(style::container::bar_pill(t))
        .height(Fill)
        .into()
    }

    /// One module's rendered element, by config name — the "name → view
    /// mapping" `config`'s module doc comment describes. Every arm mirrors
    /// what `bar_view` used to inline directly before Stage 14: the
    /// module's own `view(t)` lifted into the panel's `Message` type via
    /// `.map`, exactly as `subscription` above lifts each module's
    /// subscription. This `match` is exhaustive over [`config::ModuleName`]
    /// on purpose (same reasoning as `SurfaceRole`'s in `view` above) — a
    /// module added to that enum without an arm here is a compile error,
    /// not a silently-blank pill.
    ///
    /// As of Stage 18 there is no placeholder arm left: `tray` renders the
    /// real `modules::tray` module (a module *directory* — see its doc
    /// comment for the one sanctioned deviation from one-file-per-module).
    fn module_view(&self, name: config::ModuleName) -> Element<'_, Message> {
        let t = &self.theme;
        match name {
            // Mark and clock pass more than the theme: their *surface
            // treatments* are style-dependent (the clock: solid ivory pill
            // on the ledger bar, bare ivory text on an island; the mark:
            // pill-shaped hover hit area on the bar, a button filling its
            // island circle), so both are handed `config.style` as well. A
            // deliberate, narrow exception to the layout seam — see
            // `island_view`'s doc comment. `media` used to be the third
            // (its muted-fill pill on the bar vs. a bare island button) —
            // retired 2026-08-01 along with the pill itself: a bare status
            // glyph looks the same either way, so `Media::view` no longer
            // takes `config.style` at all.
            config::ModuleName::Mark => self.mark.view(t, self.config.style).map(Message::Mark),
            // Style-independent: bare quiet text either way. In islands mode
            // the ink pill around it is `island_pill`'s (an island of its
            // own beside the mark's — spec §7, amended 2026-08-01); the
            // module itself draws plain text in both styles, which is why it
            // never needs `config.style`.
            config::ModuleName::WindowTitle => self.window_title.view(t).map(Message::WindowTitle),
            config::ModuleName::Mpris => self.media.view(t).map(Message::Media),
            config::ModuleName::Clock => self.clock.view(t, self.config.style).map(Message::Clock),
            config::ModuleName::NiriColumns => self.columns.view(t).map(Message::Columns),
            config::ModuleName::Volume => self.volume.view(t).map(Message::Volume),
            config::ModuleName::Network => self.network.view(t).map(Message::Network),
            config::ModuleName::Bluetooth => self.bluetooth.view(t).map(Message::Bluetooth),
            config::ModuleName::Battery => self.battery.view(t).map(Message::Battery),
            config::ModuleName::Claude => self.claude_code.view(t).map(Message::ClaudeCode),
            config::ModuleName::Tray => self.tray.view(t).map(Message::Tray),
        }
    }

    /// Whether a module would draw anything if rendered right now — the
    /// "would you draw anything?" question the Stage 15 handoff flagged as
    /// missing (its gotcha 2) and predicted would need exactly this seam.
    /// Islands need it because they wrap modules in their own scrim pills
    /// (`island_pill`): a module whose `view` returns a zero-sized `Space`
    /// is invisible on the ledger bar, but a pill wrapped around that
    /// nothing would still be a visible empty blob. Each module answers
    /// from the same state its `view`'s early return reads, so the two
    /// cannot disagree.
    ///
    /// Exhaustive over [`config::ModuleName`] for the same reason
    /// `module_view` is: a new module without an answer here is a compile
    /// error, not a phantom pill.
    fn module_is_present(&self, name: config::ModuleName) -> bool {
        match name {
            config::ModuleName::Mark => self.mark.is_present(),
            config::ModuleName::WindowTitle => self.window_title.is_present(),
            config::ModuleName::Mpris => self.media.is_present(),
            config::ModuleName::Clock => self.clock.is_present(),
            config::ModuleName::NiriColumns => self.columns.is_present(),
            config::ModuleName::Volume => self.volume.is_present(),
            config::ModuleName::Network => self.network.is_present(),
            config::ModuleName::Bluetooth => self.bluetooth.is_present(),
            config::ModuleName::Battery => self.battery.is_present(),
            config::ModuleName::Claude => self.claude_code.is_present(),
            config::ModuleName::Tray => self.tray.is_present(),
        }
    }

    /// One bar region (left/center/right): the configured module list,
    /// rendered in order. `row(iterator)` (the function form, not the
    /// `row![]` macro) is what lets the number of children vary at runtime
    /// with e.g. `config.left.len()` rather than being fixed at compile
    /// time — the macro can't do that, since it expands to a fixed-arity
    /// constructor. Returns the `Row` itself rather than an already-erased
    /// `Element` so `bar_view` can chain the region-specific modifiers
    /// (`spacing` for every region, plus `align_y` in the center region's
    /// case) before converting.
    ///
    /// **Spacing is deliberately the caller's** rather than baked in here:
    /// the ledger bar uses two different gaps, `bar_element_gap` between
    /// elements along the bar (left and center) and the slightly wider
    /// `bar_cluster_gap` between the right region's status readouts. Both
    /// replace the `island_gap` this method used to apply to all three —
    /// that token is the *islands-mode* gap (between free-standing island
    /// pills) and no longer belongs anywhere in the ledger bar.
    ///
    /// Absent modules are filtered out here (2026-08-01), not just rendered
    /// as zero-sized `Space`s: iced's `Row` puts its `spacing` between
    /// *every* pair of children regardless of their size, so a zero-sized
    /// child still costs a full gap — an absent battery or tray used to
    /// leave a phantom `bar_cluster_gap` in the ledger cluster while the
    /// islands layout (which always filtered via `module_is_present`)
    /// closed it. Filtering in the one method both layouts share is what
    /// keeps the two spacing the same module list identically.
    fn region(&self, modules: &[config::ModuleName]) -> iced::widget::Row<'_, Message> {
        row(modules
            .iter()
            .filter(|name| self.module_is_present(**name))
            .map(|name| self.module_view(*name)))
    }

    /// The ledger bar itself — the `view` body as it stood before the daemon
    /// conversion, moved into its own method so `view` can dispatch. As of
    /// Stage 14 the three regions are config-driven (`Panel::region`) rather
    /// than a hardcoded `row![mark, media]`/`row![clock, columns]`/
    /// `row![volume, network, battery, claude_code]`. The outer five-element
    /// ledger shape (left, spacer, center, spacer, right) is unchanged since
    /// Stage 13; the tokens dressing it are not — the bar is now a floating
    /// `bar_pill` whose content clears its own rounded ends, with the
    /// `bar_element_gap`/`bar_cluster_gap` region gaps in place of the old
    /// islands-mode `island_gap`.
    fn bar_view(&self) -> Element<'_, Message> {
        let t = &self.theme;

        // The right side's three groups (see `right_region_split`), in the
        // fixed order claude, tray, status cluster — the claude and tray
        // groups sit to the *left* of the status cluster (Jordan,
        // 2026-08-01), which keeps the quick-settings trigger at the bar's
        // trailing end. The first two are each gated on presence, so an
        // absent group costs neither space nor a gap. `bar_element_gap`
        // between groups, the same element-scale gap the left and centre
        // regions use; the *cluster's* internal gap stays the wider
        // `bar_cluster_gap`.
        let (status_modules, claude_listed, tray_listed) = self.right_region_split();
        let mut right = row![]
            .spacing(t.sizes.bar_element_gap)
            .align_y(iced::Center);
        if claude_listed && self.claude_code.is_present() {
            right = right.push(self.claude_cluster_trigger());
        }
        if tray_listed && self.tray.is_present() {
            right = right.push(
                self.region(&[config::ModuleName::Tray])
                    .align_y(iced::Center),
            );
        }
        right = right.push(
            self.status_cluster_trigger(
                &status_modules,
                self.region(&status_modules)
                    .spacing(t.sizes.bar_cluster_gap)
                    .align_y(iced::Center),
            ),
        );

        // Ledger layout (Architecture in PLAN.md): the full five-element
        // row — left region, Fill spacer, center (clock), Fill spacer,
        // right region (status pills). The two Fill spacers split the
        // leftover width equally, which keeps the center region centered in
        // the space between the regions — it can drift by half the right
        // region's width when the right region outweighs the left, which is
        // acceptable for the ledger style. Keeping this outer row separate
        // from module code is what lets the future Islands layout swap in
        // later without touching any individual module.
        // Every row here needs its own `align_y(Center)`: the outer
        // container centres the five-element row as a *block*, but a row's
        // cross-axis alignment defaults to `Start`, so without these the
        // shorter regions (bare icons, ~15 px) would top-align against the
        // row's height — which the clock pill (`panel_pill`, 40) sets.
        // That applies at both levels: the outer row centres each region
        // as a block, and each region centres the modules within it
        // (mark's icon next to the window-title text, icon next to text in
        // the status readouts).
        container(
            row![
                self.region(&self.config.left)
                    .spacing(t.sizes.bar_element_gap)
                    .align_y(iced::Center),
                Space::new().width(Fill),
                self.region(&self.config.center)
                    .spacing(t.sizes.bar_element_gap)
                    .align_y(iced::Center),
                Space::new().width(Fill),
                // The split right side built above: status cluster (the
                // quick-settings trigger — the whole cluster is the hit
                // target, per spec §7's "status" unit), then the claude
                // and tray groups.
                right,
            ]
            .align_y(iced::Center),
        )
        // The bar is a floating pill, not a flush strip: `bar_pill` is solid
        // ink at `radii.pill`, and `main`'s layer-shell margins are what
        // inset the surface from the screen edges (see there).
        .style(style::container::bar_pill(t))
        // Horizontal padding = half the height *difference* between the
        // bar and `panel_pill_clock`, not the bar's full rounded-end
        // radius (half its height), because both ends now lead with a
        // pill-shaped hit area of exactly that height — the mark's button
        // on the left (`mark::Mark::view`), the status cluster's
        // quick-settings button on the right (`status_cluster_trigger`) —
        // and a smaller pill can tuck *into* a bigger pill's rounded end
        // concentrically: inset by half the height difference, the two
        // caps share a centre and the gap between their curves is that
        // same half-difference all the way around — exactly the 8px those
        // hit pills already get above and below them (Jordan, 2026-08-01:
        // the old full-radius inset read as extra dead space outside the
        // mark's hit area). The full radius would be needed again only for
        // *square*-cornered content right at the bar's end; everything
        // this bar can lead with is either pill-shaped (the hit-area
        // buttons themselves) or small enough (a bare 15px glyph, centred)
        // to clear the semicircle at this inset with room to spare.
        // `config.height(t)`
        // rather than the `panel_bar` token so the inset tracks the real
        // geometry when `panel.kdl` sets a custom `height` (the two agree
        // at the default). This is *not* `config.margin` any more: that
        // knob became the surface's inset from the screen edge (`main`),
        // not padding inside the bar.
        .padding([
            0.0,
            (self.config.height(t) - t.sizes.panel_pill_clock) / 2.0,
        ])
        .align_y(iced::Center)
        .width(Fill)
        .height(Fill)
        .into()
    }

    /// The whole of what `style "islands"` draws on its one surface: the
    /// three clusters stacked as layers of a single full-width element.
    ///
    /// `stack!` rather than the ledger's five-element row on purpose: each
    /// `island_view` below is already a full-width transparent positioner
    /// with its cluster pushed to the left / centre / right (the exact
    /// layout the three per-surface islands had before they collapsed into
    /// one surface — see `IslandKind`'s doc comment), so layering them
    /// reproduces the old screen positions pixel for pixel. A row would
    /// have re-introduced the ledger's "centre drifts by half the weight
    /// difference" compromise; the stack keeps the centre island truly
    /// centred, as its own surface used to. The clusters can in principle
    /// overlap if a config crams enough modules in — iced hit-tests stack
    /// layers top-down, so the rightmost listed layer wins clicks in any
    /// overlap, matching how the topmost of the old overlapping surfaces
    /// would have.
    fn islands_view(&self) -> Element<'_, Message> {
        stack![
            self.island_view(IslandKind::Left),
            self.island_view(IslandKind::Centre),
            self.island_view(IslandKind::Right),
        ]
        .into()
    }

    /// One island cluster — one layer of [`Panel::islands_view`]'s stack:
    /// solid ink pills floating over the wallpaper, `island_gap` apart
    /// (that token's documented meaning: the gap between pills).
    ///
    /// # Pill grouping (listing 2a)
    ///
    /// The concept draws the left and centre clusters as one pill *per
    /// module* — mark circle beside the window-title text, clock pill
    /// beside the column-strip pill — because each is its own control. The
    /// right cluster is the exception: spec §7 names it "status" as a
    /// *unit*, and it is one pill acting as the single quick-settings
    /// trigger, so its modules (media included, since 2026-08-01 — see
    /// `modules::media`'s doc comment) share a pill exactly as they share
    /// the ledger bar's right region. Only modules that would actually draw
    /// something get a pill at all (`module_is_present`) — a left/centre
    /// cluster whose one module is absent draws nothing; the shared status
    /// pill just loses one glyph, the same way an absent battery or network
    /// already does.
    ///
    /// # Why this can be so short (the layout seam, as amended)
    ///
    /// Every module below the outer container is the *same code* the ledger
    /// bar composes: `Panel::module_view`'s name → view mapping over the
    /// same `config.left` / `config.center` / `config.right` list this
    /// island is named for (via `Panel::region` for the shared status pill,
    /// via `island_pill` for the per-module ones). No module file knows this
    /// *layout* exists — no module builds a row, picks an alignment, or asks
    /// which island it is in. That was the point of keeping the outer row
    /// out of module code all the way back in Stage 3.
    ///
    /// PLAN.md Stage 15 originally stated the seam as a flat check — *if
    /// module view code needs any change for Islands, the layout seam was
    /// violated* — and Stage 15 shipped with zero module edits. **That rule
    /// is now amended (Jordan, 2026-07-31): layout mechanics still stay out
    /// of modules, but a module's *surface treatment* may legitimately differ
    /// per style.** The two are different questions: "where do I sit and how
    /// wide am I" is layout and stays here, while "what am I sitting *on*,
    /// and therefore what may I be styled as" is the module's own to answer.
    ///
    /// Two modules exercise that today. `modules::clock` is a solid
    /// ivory pill in ledger style (concept 4b, where it is the bar's only
    /// one) and bare ivory text in islands style (listing 2a — a pill inside
    /// the island's ink would be a surface nested in a surface, and in
    /// this centre island the sole solid ivory element is the niri-columns
    /// strip's focused dash). `modules::mark`'s launcher button is a
    /// pill-shaped hover hit area on the ledger bar but a `Fill`-sized
    /// button on islands, stretching to the ink circle `island_pill` pins
    /// around it (see `Mark::view`'s doc comment). `Panel::module_view`
    /// passes `config.style` into those two arms; every other module's
    /// `view` — `modules::media` included, since it retired its own
    /// per-style pill/button split on 2026-08-01 (a bare status glyph looks
    /// identical directly on the bar's ink or inside an island's shared
    /// status pill — see `Media::view`'s doc comment) — takes the theme
    /// alone.
    ///
    /// # Why the layer is wider than the pill
    ///
    /// Each cluster's layer spans the whole strip; only its pills are
    /// visible on it, and `align_x` is what puts them at the left margin,
    /// the centre, or the right margin. (This full-width-positioner shape
    /// dates from when each cluster was its own *surface* and sizing a
    /// surface to its content was ruled out — iced 0.14 has no supported
    /// way to measure a laid-out widget, see the Stage 15 handoff. As
    /// stack layers the shape simply costs nothing now.) The strip surface
    /// itself takes pointer input across its full width — see
    /// `SurfaceGeometry`'s `events_transparent` field for what that
    /// swallows and why it's acceptable.
    fn island_view(&self, kind: IslandKind) -> Element<'_, Message> {
        let t = &self.theme;

        // Each island renders exactly one of the config's module lists —
        // the same three lists the ledger bar's three regions render, in the
        // same order. There is deliberately no second copy of the default
        // grouping here: `mark; mpris` / `clock; niri-columns` /
        // `volume; network; battery; claude` are `PanelConfig::default`'s
        // lists (Stage 14) and a user's `panel.kdl` moves modules between
        // islands exactly as it moves them between ledger regions.
        let (modules, align) = match kind {
            IslandKind::Left => (&self.config.left, Horizontal::Left),
            IslandKind::Centre => (&self.config.center, Horizontal::Center),
            IslandKind::Right => (&self.config.right, Horizontal::Right),
        };

        // Only modules that would draw something get a pill. This replaces
        // the Stage 15 caveat about empty scrim blobs: the old single-pill
        // island could only check `modules.is_empty()` (the explicit
        // `left { }` case) and had to hope non-empty lists drew *something*;
        // per-module pills force the honest question, and
        // `module_is_present` is its answer. An island whose every module
        // is absent draws nothing at all.
        let present: Vec<config::ModuleName> = modules
            .iter()
            .copied()
            .filter(|name| self.module_is_present(*name))
            .collect();
        if present.is_empty() {
            return Space::new().width(0).height(0).into();
        }

        let cluster: Element<'_, Message> = match kind {
            // One ink pill per module, `island_gap` between them — the
            // cluster the concept draws. `row(iterator)` for the same
            // runtime-length reason as `Panel::region`.
            //
            // The window title is one of these like any other (amended
            // 2026-08-01: spec §7 now gives it "an island of its own" beside
            // the mark's, retiring the first draft's shared mark + title
            // pill). Its pill comes and goes with window focus — the
            // `present` filter above is what leaves an unfocused desktop
            // showing just the mark's circle, no empty ink beside it.
            IslandKind::Left | IslandKind::Centre => {
                row(present.iter().map(|name| self.island_pill(*name)))
                    .spacing(t.sizes.island_gap)
                    .align_y(iced::Center)
                    .into()
            }
            // The right side's island *row* (2026-08-01, the same split as
            // the ledger's — see `right_region_split`): the claude pill
            // (its own island, its own trigger — the usage popover), then
            // the tray's pill, then the shared status pill (the
            // quick-settings trigger) at the trailing end — claude and
            // tray sit left of the status island, same order as the
            // ledger. `island_gap` apart like the left cluster's
            // per-module pills.
            //
            // The status pill keeps the ledger's `bar_cluster_gap`
            // internally — deliberately shared, because this is the one
            // place the two layouts draw the same thing (spec §7's
            // "status" unit: the same readouts at the same rhythm,
            // whichever chrome they sit in). The trigger button wraps the
            // pill, not the surface, so only the visible cluster reacts —
            // even though the surface itself accepts pointer events across
            // its whole width (see `SurfaceGeometry::events_transparent`).
            //
            // Layering (2026-08-01, hover-affordance parity with the
            // ledger's `status_cluster_trigger`): the outer `container`
            // draws the island's solid ink (`bar_pill`) and nothing else —
            // no padding — while an inner `bare`-styled button fills it
            // and carries the pill's content padding. The order matters:
            // iced paints a container's background first and its content
            // on top, so the button's hover fill (`fill_subtle` at
            // `radii.pill`, the exact same rounded rect) lands *above* the
            // ink and tints the whole pill on hover. Wrapping the ink pill
            // in the button instead would paint the hover fill underneath
            // the ink, where it could never be seen. Nesting the cluster's
            // own buttons (volume, tray) inside this one is safe for the
            // reason `status_cluster_trigger`'s teaching note gives —
            // children update first, and a captured event stops here.
            // `claude_island_pill` copies this exact layering.
            IslandKind::Right => {
                let (status_modules, claude_listed, tray_listed) = self.right_region_split();
                let mut pills: Vec<Element<'_, Message>> = Vec::new();

                if claude_listed && self.claude_code.is_present() {
                    pills.push(self.claude_island_pill());
                }
                if tray_listed && self.tray.is_present() {
                    pills.push(self.island_pill(config::ModuleName::Tray));
                }
                if status_modules
                    .iter()
                    .any(|name| self.module_is_present(*name))
                {
                    let cluster = container(
                        self.region(&status_modules)
                            .spacing(t.sizes.bar_cluster_gap)
                            .align_y(iced::Center),
                    )
                    .height(Fill)
                    .align_y(iced::Center);
                    pills.push(
                        container(
                            button(cluster)
                                .padding([0.0, t.sizes.panel_pill / 2.0])
                                .height(Fill)
                                .style(style::button::bare(t, Surface::Ink))
                                .on_press(Message::Popover(popover::Message::Triggered(
                                    PopoverKind::QuickSettings,
                                ))),
                        )
                        .style(style::container::bar_pill(t))
                        .height(Fill)
                        .into(),
                    );
                }

                row(pills)
                    .spacing(t.sizes.island_gap)
                    .align_y(iced::Center)
                    .into()
            }
        };

        // The cluster is `Shrink`-wide, so this outer container is just the
        // positioner: it fills the surface, draws nothing (the app-wide
        // background is transparent — see `Panel::style`), and pushes the
        // cluster to the margin the island belongs at. The margin itself is
        // the layer-shell surface's, not padding here (see
        // `SurfaceGeometry::of`), so the cluster lands
        // `panel_margin_ledger` from the screen edge (the islands share the
        // ledger's insets).
        container(cluster)
            .width(Fill)
            .height(Fill)
            .align_x(align)
            .into()
    }

    /// One free-standing island pill around one module's view.
    ///
    /// The ink pill: `container::bar_pill` — the same solid ink at
    /// `radii.pill` the ledger bar wears (Jordan, 2026-08-01: the islands
    /// match the ledger panel; the translucent scrim treatment is retired
    /// for the resting panel and kept for overlay states only — recorded in
    /// the style guide's scrims section). Zero local styling. Horizontal
    /// padding is half the pill's height for the same reason as the ledger
    /// bar's: that *is* the radius the rounded ends actually curve at (iced
    /// clamps `radii.pill`'s 999 to half the height), so it is the closest
    /// content can sit without being sliced by the curve.
    ///
    /// The mark is the one differently-shaped pill: the concept draws it as
    /// a *circle* (a launcher button, not a readout), so instead of hugging
    /// its content plus padding it is pinned to `panel_pill` wide — equal to
    /// the pill height, which `radii.pill` then closes into a circle, the
    /// same width-equals-height trick the strip's rest dashes use. In
    /// islands style the module's own view is a `Fill`-sized bare button
    /// (see `Mark::view`), so the whole of this circle is the launcher's
    /// hit target and its hover tint paints exactly over the circle's ink
    /// — which is also why the mark arm sets no padding: the button fills
    /// the pinned width edge to edge.
    fn island_pill(&self, name: config::ModuleName) -> Element<'_, Message> {
        let t = &self.theme;
        let pill = container(self.module_view(name))
            .style(style::container::bar_pill(t))
            .height(Fill)
            .align_y(iced::Center);
        match name {
            config::ModuleName::Mark => pill
                .width(t.sizes.panel_pill)
                .align_x(Horizontal::Center)
                .into(),
            // Media used to special-case here too (a `Fill`-height `bare`
            // button carrying its own content padding, like the mark) —
            // retired 2026-08-01 along with its per-style pill/button split:
            // media is now a status-cluster glyph, rendered through
            // `island_view`'s Right arm rather than through this per-module
            // pill at all (see `modules::media`'s doc comment).
            _ => pill.padding([0.0, t.sizes.panel_pill / 2.0]).into(),
        }
    }
}

/// Read a tray item's whole menu (from its root) — the initial fetch
/// `Panel::open_tray_menu` kicks off on every right-click that doesn't
/// simply close the popover.
///
/// A free function, not a `Panel` method: it needs none of `Panel`'s state,
/// only `item_id` (owned, so the returned `Task` can be `'static`) — the
/// same shape `modules::tray::activate`/`scroll` already use for their own
/// command-out `Task` builders. `modules::tray::menu::read_menu` takes
/// `&str`, so the future is built inside an `async move` block that owns
/// `item_id` itself (per Stage 20's own wiring sketch): the block, not the
/// bare function call, is what makes the future `'static`.
fn fetch_tray_menu(item_id: String) -> Task<Message> {
    let for_message = item_id.clone();
    Task::perform(
        async move { modules::tray::menu::read_menu(&item_id, 0).await },
        move |menu| Message::TrayMenu(popovers::tray_menu::Message::Loaded(for_message, menu)),
    )
}

/// Read one submenu row's own subtree — the lazy-fetch `Panel::update`'s
/// `ToggleSubmenu` arm issues when `TrayMenuState::toggle_expanded` says the
/// row's children haven't arrived yet. Same shape as [`fetch_tray_menu`],
/// parked at `node_id` instead of the root.
fn fetch_tray_submenu(item_id: String, node_id: i32) -> Task<Message> {
    Task::perform(
        async move { modules::tray::menu::read_menu(&item_id, node_id).await },
        move |menu| Message::TrayMenu(popovers::tray_menu::Message::SubmenuLoaded(node_id, menu)),
    )
}

/// Read every session's transcript usage — the one-shot task
/// `Panel::open_claude_usage` issues on the trigger click. Free function
/// for the same reason as [`fetch_tray_menu`]: it needs nothing of `Panel`
/// beyond the owned snapshot it is handed, which is what lets the returned
/// `Task` be `'static`.
fn fetch_claude_usage(targets: Vec<modules::claude::UsageTarget>) -> Task<Message> {
    Task::perform(popovers::claude_usage::read_usage(targets), |sessions| {
        Message::ClaudeUsage(popovers::claude_usage::Message::Loaded(sessions))
    })
}

/// Tests for the surface registry — the only genuinely new logic Stage 13
/// introduces. They construct a real `Panel` (cheap: `Theme::saola()` is
/// plain data and every module's `default()` is an empty snapshot, so nothing
/// here touches Wayland, D-Bus, or pulse) and drive `Panel::update` with the
/// same messages the runtime would send, which is what makes them a test of
/// the wiring rather than of a hand-copied model of it.
#[cfg(test)]
mod tests {
    use super::*;

    /// `Panel::new` with the given config, no CLI flags, no config path,
    /// and the default theme — the constructor every test here goes
    /// through, so the reload stage's extra parameters (runtime concerns
    /// most tests don't exercise) are spelled out exactly once.
    fn panel_with(config: config::PanelConfig) -> Panel {
        Panel::new(
            config,
            config::CliOverrides::default(),
            None,
            Theme::saola(),
        )
    }

    /// The default config + default theme, exactly what an unconfigured
    /// `panel.kdl` boots into — the fixture every test in this module
    /// builds a `Panel` from, so a config-parsing bug can't silently change
    /// what these surface-registry tests are exercising.
    fn test_panel() -> Panel {
        panel_with(config::PanelConfig::default())
    }

    #[test]
    fn a_fresh_panel_knows_of_no_surfaces() {
        let panel = test_panel();
        assert!(
            panel.windows.is_empty(),
            "the registry must start empty — the bar's Id is minted inside \
             the runtime and can only arrive via SurfaceOpened"
        );
    }

    #[test]
    fn an_unknown_id_falls_back_to_the_bar() {
        let panel = test_panel();
        // This is the bar's own first frame: `view` runs before the `Opened`
        // event has been processed, so the registry cannot yet classify it.
        assert_eq!(panel.role(window::Id::unique()), SurfaceRole::Bar);
    }

    #[test]
    fn surface_opened_registers_the_initial_surface_as_the_bar() {
        let mut panel = test_panel();
        let id = window::Id::unique();

        let _ = panel.update(Message::SurfaceOpened(id));

        assert_eq!(panel.windows.get(&id), Some(&SurfaceRole::Bar));
    }

    #[test]
    fn surface_opened_does_not_clobber_a_pre_registered_role() {
        // Stage 15/16 shape: the role is recorded at spawn time, and the
        // `Opened` event arrives afterwards. Stated with a role that
        // differs from the boot role — a pre-registered islands strip in a
        // *ledger*-configured panel (whose boot role is `Bar`) must
        // survive its own `Opened` event rather than being demoted to the
        // boot role by an `insert`.
        let mut panel = test_panel();
        let id = window::Id::unique();
        let role = SurfaceRole::Islands;
        panel.windows.insert(id, role);

        let _ = panel.update(Message::SurfaceOpened(id));

        assert_eq!(panel.windows.len(), 1);
        assert_eq!(panel.windows.get(&id), Some(&role));
    }

    #[test]
    fn surface_closed_forgets_the_surface() {
        let mut panel = test_panel();
        let id = window::Id::unique();
        let _ = panel.update(Message::SurfaceOpened(id));

        let _ = panel.update(Message::SurfaceClosed(id));

        assert!(panel.windows.is_empty());
    }

    #[test]
    fn surface_closed_for_an_unknown_id_is_a_no_op() {
        // `remove_surface` also removes the entry, so a close event for an
        // already-forgotten surface is expected traffic, not a bug.
        let mut panel = test_panel();
        let known = window::Id::unique();
        let _ = panel.update(Message::SurfaceOpened(known));

        let _ = panel.update(Message::SurfaceClosed(window::Id::unique()));

        assert_eq!(panel.windows.get(&known), Some(&SurfaceRole::Bar));
    }

    #[test]
    fn several_surfaces_are_tracked_independently() {
        // The `StartMode::AllScreens` shape (one initial surface per output)
        // and, later, the Islands shape.
        let mut panel = test_panel();
        let first = window::Id::unique();
        let second = window::Id::unique();

        let _ = panel.update(Message::SurfaceOpened(first));
        let _ = panel.update(Message::SurfaceOpened(second));
        let _ = panel.update(Message::SurfaceClosed(first));

        assert_eq!(panel.windows.len(), 1);
        assert_eq!(panel.role(second), SurfaceRole::Bar);
    }

    /// Stage 14's module-list → region mapping, exercised at the `Panel`
    /// layer (complementing `config::tests::module_list_maps_names_to_module_name_variants`,
    /// which only checks the pure name → `ModuleName` parse). Every variant
    /// `Panel::module_view`'s `match` handles must build a real `Element`
    /// without panicking; this is what would break immediately if a config
    /// name resolved to a module state field that didn't actually exist.
    ///
    /// Both panel styles are exercised, because `module_view` is no longer
    /// style-independent: the clock arm passes `config.style` down and the
    /// clock renders a different widget per style (see `island_view`'s doc
    /// comment on the amended seam).
    #[test]
    fn module_view_handles_every_known_module_name_in_both_styles() {
        for style in [config::PanelStyle::Ledger, config::PanelStyle::Islands] {
            let panel = panel_with(config::PanelConfig {
                style,
                ..config::PanelConfig::default()
            });
            for name in [
                config::ModuleName::Mark,
                config::ModuleName::WindowTitle,
                config::ModuleName::Mpris,
                config::ModuleName::Clock,
                config::ModuleName::NiriColumns,
                config::ModuleName::Volume,
                config::ModuleName::Network,
                config::ModuleName::Bluetooth,
                config::ModuleName::Battery,
                config::ModuleName::Claude,
                config::ModuleName::Tray,
            ] {
                let _: Element<'_, Message> = panel.module_view(name);
            }
        }
    }

    /// The default `panel.kdl` (no file at all) must render the same three
    /// regions the bar always has — proof that `Panel::region`/`bar_view`
    /// actually consult `self.config` rather than a leftover hardcoded
    /// list.
    #[test]
    fn bar_view_renders_the_default_module_lists_without_panicking() {
        let panel = test_panel();
        let _: Element<'_, Message> = panel.bar_view();
    }

    /// A custom module list (a reordered, trimmed right region) renders
    /// without panicking too — config actually drives the region, it isn't
    /// just read and ignored.
    #[test]
    fn bar_view_renders_a_custom_module_list_without_panicking() {
        let config = config::PanelConfig {
            right: vec![config::ModuleName::Battery, config::ModuleName::Volume],
            ..config::PanelConfig::default()
        };
        let panel = panel_with(config);
        let _: Element<'_, Message> = panel.bar_view();
    }

    // ---- Stage 15: the Islands layout -----------------------------------

    /// `style "islands"` and nothing else — the config an islands session
    /// actually boots from, since every other knob's default is shared with
    /// ledger style.
    fn islands_config() -> config::PanelConfig {
        config::PanelConfig {
            style: config::PanelStyle::Islands,
            ..config::PanelConfig::default()
        }
    }

    fn islands_panel() -> Panel {
        panel_with(islands_config())
    }

    /// The style decides what the boot surface *is*. This is the hinge the
    /// whole mode switch turns on: nothing else about `Settings` differs
    /// between the bar and the islands strip except the geometry derived
    /// from this role.
    #[test]
    fn the_boot_surface_is_the_bar_in_ledger_style_and_the_strip_in_islands_style() {
        assert_eq!(
            initial_role(&config::PanelConfig::default()),
            SurfaceRole::Bar
        );
        assert_eq!(initial_role(&islands_config()), SurfaceRole::Islands);
    }

    /// The unknown-Id fallback follows the style too — in islands mode the
    /// boot surface's first frame (rendered before its `Opened` event
    /// arrives) must draw the islands strip, not a full ledger bar.
    #[test]
    fn an_unknown_id_falls_back_to_the_islands_strip_in_islands_style() {
        let panel = islands_panel();
        assert_eq!(panel.role(window::Id::unique()), SurfaceRole::Islands);
    }

    /// Neither style spawns anything at boot: the bar and the islands
    /// strip are each the single surface `Settings` already created
    /// (2026-08-01 — the islands' two extra per-cluster surfaces are gone;
    /// see `IslandKind`'s doc comment for why the three collapsed into
    /// one).
    #[test]
    fn neither_style_asks_for_extra_surfaces_at_boot() {
        for mut panel in [test_panel(), islands_panel()] {
            let _ = panel.spawn_boot_surfaces();
            assert!(panel.windows.is_empty());
        }
    }

    /// Every island cluster draws, for every kind — and the stacked
    /// composition of all three (the islands strip's whole view) draws too:
    /// the islands counterpart of
    /// `bar_view_renders_the_default_module_lists_without_panicking`.
    #[test]
    fn island_view_renders_every_cluster_without_panicking() {
        let panel = islands_panel();
        for kind in [IslandKind::Left, IslandKind::Centre, IslandKind::Right] {
            let _: Element<'_, Message> = panel.island_view(kind);
        }
        let _: Element<'_, Message> = panel.islands_view();
    }

    /// The presence seam at boot: every service-backed module starts
    /// absent (no D-Bus signal, no pulse snapshot, no niri event has
    /// arrived yet), so none of them earns an island pill — while the two
    /// modules with nothing to wait for (mark, clock) are present
    /// immediately. This is what keeps a freshly-booted islands panel from
    /// flashing a row of empty scrim blobs before the services report in.
    #[test]
    fn only_serviceless_modules_are_present_at_boot() {
        let panel = islands_panel();
        for (name, expected) in [
            (config::ModuleName::Mark, true),
            (config::ModuleName::Clock, true),
            (config::ModuleName::Mpris, false),
            // No niri event has arrived, so no window is focused yet.
            (config::ModuleName::WindowTitle, false),
            (config::ModuleName::NiriColumns, false),
            (config::ModuleName::Volume, false),
            (config::ModuleName::Network, false),
            (config::ModuleName::Bluetooth, false),
            (config::ModuleName::Battery, false),
            (config::ModuleName::Claude, false),
            (config::ModuleName::Tray, false),
        ] {
            assert_eq!(panel.module_is_present(name), expected, "{name:?}");
        }
    }

    /// A panel with a focused window draws its title in both layouts — the
    /// ledger bar's left region, and the islands' left cluster as an island
    /// of its own beside the mark's (spec §7, amended 2026-08-01). `Element`
    /// can't be introspected, so what this pins is that both composition
    /// paths build without panicking once the module is actually present
    /// (the boot state, where it isn't, is covered by
    /// `only_serviceless_modules_are_present_at_boot`).
    #[test]
    fn a_focused_title_draws_in_both_layouts() {
        for config in [config::PanelConfig::default(), islands_config()] {
            let mut panel = panel_with(config);
            panel
                .window_title
                .update(modules::window_title::Message::Updated(Some(
                    "nvim — src/main.rs".to_owned(),
                )));
            assert!(panel.module_is_present(config::ModuleName::WindowTitle));
            let _: Element<'_, Message> = panel.bar_view();
            let _: Element<'_, Message> = panel.island_view(IslandKind::Left);
        }
    }

    /// A config that asks for a title but no mark (`mark "none"`) still
    /// draws the title. Once a fallback (when the title rode inside the
    /// mark's pill and needed somewhere to go without one), now just the
    /// normal path — the title's island never depended on the mark's being
    /// present — but the config shape is real enough to keep pinned.
    #[test]
    fn a_titled_island_with_no_mark_still_draws_the_title() {
        let mut panel = panel_with(config::PanelConfig {
            mark: config::MarkSource::None,
            ..islands_config()
        });
        panel
            .window_title
            .update(modules::window_title::Message::Updated(Some(
                "Alacritty".to_owned(),
            )));
        assert!(!panel.module_is_present(config::ModuleName::Mark));
        let _: Element<'_, Message> = panel.island_view(IslandKind::Left);
    }

    /// An island whose module list is explicitly empty (`left { }` in
    /// `panel.kdl`) renders nothing rather than an empty scrim pill.
    #[test]
    fn an_island_with_no_modules_renders_nothing() {
        let config = config::PanelConfig {
            left: vec![],
            ..islands_config()
        };
        let panel = panel_with(config);
        // No panic, and (not assertable through `Element`) no pill: the
        // early return in `island_view` is what this pins down.
        let _: Element<'_, Message> = panel.island_view(IslandKind::Left);
    }

    /// The islands strip's own geometry: a `panel_pill`-tall strip
    /// stretched to the output's width, on the `Top` layer and never
    /// taking the keyboard, sharing the **ledger** insets —
    /// `panel_margin_ledger` (20) at the sides, `panel_margin_ledger_top`
    /// (18) at the anchored edge (the uniform `panel_margin_islands` 26 is
    /// retired, as is the per-cluster negative-margin arithmetic the old
    /// three-surface layout needed). See `SurfaceGeometry::of`.
    #[test]
    fn island_geometry_matches_the_ledger_insets() {
        let theme = Theme::saola();
        let config = islands_config();
        let side = theme.sizes.panel_margin_ledger as i32;
        let edge_margin = theme.sizes.panel_margin_ledger_top as i32;

        let geometry = SurfaceGeometry::of(SurfaceRole::Islands, &config, &theme);

        assert_eq!(geometry.size, (0, theme.sizes.panel_pill as u32));
        assert_eq!(geometry.margin, (edge_margin, side, 0, side));
        assert_eq!(geometry.anchor, Anchor::Top | Anchor::Left | Anchor::Right);
        assert_eq!(geometry.layer, Layer::Top);
        assert_eq!(geometry.keyboard_interactivity, KeyboardInteractivity::None);
    }

    /// The islands strip accepts pointer events across its full width,
    /// exactly like the ledger bar — the point of collapsing the three
    /// per-cluster surfaces into one (2026-08-01, see `IslandKind`'s doc
    /// comment): when only the right of three overlapping surfaces could
    /// take input, the mark's launcher and media's play/pause sat on
    /// surfaces the compositor never sent a click to.
    #[test]
    fn the_islands_strip_takes_pointer_events() {
        let theme = Theme::saola();
        let geometry = SurfaceGeometry::of(SurfaceRole::Islands, &islands_config(), &theme);
        assert!(
            !geometry.events_transparent,
            "every cluster's controls — mark, media, quick-settings trigger \
             — live on this one surface, so it must receive clicks"
        );
    }

    /// The islands strip reserves its own zone the same way the bar does:
    /// height plus one edge margin as the bottom gap. (The old per-cluster
    /// surfaces split this — centre reserved, siblings rode along at zone
    /// 0 with a negative-margin re-level; one surface needs none of that.)
    #[test]
    fn the_islands_strip_reserves_an_exclusive_zone() {
        let theme = Theme::saola();
        let geometry = SurfaceGeometry::of(SurfaceRole::Islands, &islands_config(), &theme);
        assert_eq!(
            geometry.exclusive_zone,
            theme.sizes.panel_pill as i32 + theme.sizes.panel_margin_ledger_top as i32,
            "the strip reserves its height plus the bottom gap"
        );
    }

    /// Both panel surfaces keep a bottom gap equal to their top margin:
    /// the reserved strip (zone we pass + the anchored-edge margin the
    /// compositor adds itself) is `height + 2 × edge margin` — 84 for the
    /// ledger bar (48+18+18), 76 for the islands strip (40+18+18). The gap
    /// below the panel therefore equals the gap above it, which is the
    /// point of the 2026-08-01 change.
    #[test]
    fn panel_surfaces_reserve_a_bottom_gap_equal_to_the_top_margin() {
        let theme = Theme::saola();

        let bar = SurfaceGeometry::of(SurfaceRole::Bar, &config::PanelConfig::default(), &theme);
        let islands = SurfaceGeometry::of(SurfaceRole::Islands, &islands_config(), &theme);

        let reserved = |g: SurfaceGeometry| g.exclusive_zone + g.margin.0;
        assert_eq!(reserved(bar), 84);
        assert_eq!(reserved(islands), 76);

        // Stated as the invariant, not just the numbers: strip − (top
        // margin + height) == top margin, i.e. bottom gap == top gap.
        for g in [bar, islands] {
            assert_eq!(reserved(g) - (g.margin.0 + g.size.1 as i32), g.margin.0);
        }
    }

    /// The ledger bar's geometry: the `panel_bar` height, the asymmetric
    /// ledger margins, opaque to input — and, since 2026-08-01, a zone of
    /// height + one edge margin (the bottom gap).
    #[test]
    fn the_ledger_bar_geometry_uses_the_ledger_tokens() {
        let theme = Theme::saola();
        let geometry =
            SurfaceGeometry::of(SurfaceRole::Bar, &config::PanelConfig::default(), &theme);

        assert_eq!(geometry.size, (0, theme.sizes.panel_bar as u32));
        assert_eq!(
            geometry.margin,
            (
                theme.sizes.panel_margin_ledger_top as i32,
                theme.sizes.panel_margin_ledger as i32,
                0,
                theme.sizes.panel_margin_ledger as i32,
            )
        );
        assert_eq!(
            geometry.exclusive_zone,
            theme.sizes.panel_bar as i32 + theme.sizes.panel_margin_ledger_top as i32
        );
        assert!(!geometry.events_transparent);
    }

    /// `edge "bottom"` flips which edge every surface anchors to and which
    /// margin carries the inset — in both styles.
    #[test]
    fn the_bottom_edge_flips_anchors_and_margins_in_both_styles() {
        let theme = Theme::saola();

        for (style, role) in [
            (config::PanelStyle::Ledger, SurfaceRole::Bar),
            (config::PanelStyle::Islands, SurfaceRole::Islands),
        ] {
            let config = config::PanelConfig {
                style,
                edge: config::Edge::Bottom,
                ..config::PanelConfig::default()
            };
            let geometry = SurfaceGeometry::of(role, &config, &theme);

            assert_eq!(
                geometry.anchor,
                Anchor::Bottom | Anchor::Left | Anchor::Right
            );
            assert_eq!(geometry.margin.0, 0, "nothing above a bottom panel");
            assert_ne!(geometry.margin.2, 0, "the edge inset moved to the bottom");
        }
    }

    /// An explicit `height` knob still wins in islands style (it just
    /// defaults to `panel_pill` instead of `panel_bar` there).
    #[test]
    fn an_explicit_height_knob_overrides_the_island_pill_height() {
        let theme = Theme::saola();
        let config = config::PanelConfig {
            height: Some(52.0),
            ..islands_config()
        };

        let geometry = SurfaceGeometry::of(SurfaceRole::Islands, &config, &theme);

        assert_eq!(geometry.size, (0, 52));
        assert_eq!(
            geometry.exclusive_zone,
            52 + theme.sizes.panel_margin_ledger_top as i32
        );
    }

    /// Spawned surfaces carry their geometry through to the settings the
    /// runtime actually spawns from — in particular the `Option` fields,
    /// where `None` would silently mean "leave the protocol default"
    /// rather than "pass this value".
    #[test]
    fn spawn_settings_carry_the_geometry() {
        let theme = Theme::saola();
        let geometry = SurfaceGeometry::of(SurfaceRole::Islands, &islands_config(), &theme);

        let settings = geometry.new_layer_shell_settings();

        assert_eq!(settings.size, Some(geometry.size));
        assert_eq!(settings.margin, Some(geometry.margin));
        assert_eq!(settings.exclusive_zone, Some(geometry.exclusive_zone));
        assert_eq!(settings.anchor, geometry.anchor);
        assert!(!settings.events_transparent);
        assert_eq!(settings.keyboard_interactivity, KeyboardInteractivity::None);
    }

    // ---- Stage 16: popover infrastructure -------------------------------

    /// The popover placement, as arithmetic: `sizes.popover_width` wide,
    /// anchored to the panel's edge and the right-hand side, its edge
    /// margin the clamped gap `max(0, popover_top − strip)` — 0 since the
    /// bottom-gap change, because the panel's reserved strip (84 ledger /
    /// 76 islands) already passes `popover_top` (72), so the popover sits
    /// flush against the strip: one edge margin below the panel.
    ///
    /// The `0` exclusive zone is the load-bearing part. It makes the
    /// compositor place the popover below **every** reservation on the
    /// output — the panel's and any foreign bar's. See
    /// `SurfaceGeometry::of`'s doc comment for the full derivation and for
    /// why the original `-1` version broke next to waybar.
    #[test]
    fn popover_geometry_is_the_spec_placement_derived_from_tokens() {
        let theme = Theme::saola();
        let role = SurfaceRole::Popover(PopoverKind::QuickSettings);

        for (config, panel_role) in [
            (config::PanelConfig::default(), SurfaceRole::Bar),
            (islands_config(), SurfaceRole::Islands),
        ] {
            let geometry = SurfaceGeometry::of(role, &config, &theme);
            let panel = SurfaceGeometry::of(panel_role, &config, &theme);
            let strip = panel.exclusive_zone + panel.margin.0;

            assert_eq!(geometry.anchor, Anchor::Top | Anchor::Right);
            assert_eq!(
                geometry.size,
                (
                    theme.sizes.popover_width as u32,
                    PopoverKind::QuickSettings.height(&theme) as u32,
                )
            );
            assert_eq!(
                geometry.margin.0,
                (theme.sizes.popover_top as i32 - strip).max(0)
            );
            assert_eq!(geometry.margin.1, config.margin(&theme) as i32);
            assert_eq!(geometry.margin.2, 0);
            assert_eq!(
                geometry.exclusive_zone, 0,
                "a popover must respect every exclusive zone on the output, \
                 or a foreign bar pushes the panel down without moving the \
                 popover and the two overlap"
            );
            assert_eq!(geometry.layer, Layer::Overlay);
            assert_eq!(
                geometry.keyboard_interactivity,
                KeyboardInteractivity::OnDemand
            );
            assert!(!geometry.events_transparent);
        }
    }

    /// The popover lines up with the end of the panel it hangs from —
    /// `panel_margin_ledger` (20) in both styles now that the islands share
    /// the ledger's insets.
    #[test]
    fn the_popover_side_inset_follows_the_panel_it_hangs_from() {
        let theme = Theme::saola();
        let side = |config: &config::PanelConfig| {
            SurfaceGeometry::of(
                SurfaceRole::Popover(PopoverKind::QuickSettings),
                config,
                &theme,
            )
            .margin
            .1
        };

        assert_eq!(side(&islands_config()), 20);
        assert_eq!(side(&config::PanelConfig::default()), 20);
    }

    /// The popover clears the panel rather than overlapping its trigger
    /// (spec §6). The panel's reserved strip now *contains* the bottom gap,
    /// so the popover (a zone-respecting surface placed below that
    /// reservation, gap clamped to 0) starts exactly one edge margin below
    /// the panel's bottom edge — the same separation tiled windows get.
    #[test]
    fn the_popover_starts_below_the_panel_strip() {
        let theme = Theme::saola();

        for (config, panel_role) in [
            (config::PanelConfig::default(), SurfaceRole::Bar),
            (islands_config(), SurfaceRole::Islands),
        ] {
            let panel = SurfaceGeometry::of(panel_role, &config, &theme);
            let popover = SurfaceGeometry::of(
                SurfaceRole::Popover(PopoverKind::QuickSettings),
                &config,
                &theme,
            );

            let panel_strip = panel.exclusive_zone + panel.margin.0;
            let popover_top = panel_strip + popover.margin.0;
            let panel_bottom = panel.margin.0 + panel.size.1 as i32;
            assert!(
                popover_top >= panel_bottom + panel.margin.0,
                "the popover must sit at least one edge margin below the \
                 panel, never overlapping the control that opened it"
            );
        }
    }

    /// An oversized `height` knob in `panel.kdl` can push the panel strip
    /// past `popover_top`. The gap clamps at 0 — the popover degrades to
    /// touching the panel, never to climbing back over its trigger.
    #[test]
    fn an_oversized_panel_clamps_the_popover_gap_at_zero() {
        let theme = Theme::saola();
        let config = config::PanelConfig {
            // Strip = 90 + 18 > popover_top (72).
            height: Some(90.0),
            ..config::PanelConfig::default()
        };

        let popover = SurfaceGeometry::of(
            SurfaceRole::Popover(PopoverKind::QuickSettings),
            &config,
            &theme,
        );

        assert_eq!(popover.margin.0, 0);
    }

    /// `edge "bottom"` flips the popover with the panel: it hangs off the
    /// bottom edge and grows upward.
    #[test]
    fn the_bottom_edge_flips_the_popover_too() {
        let theme = Theme::saola();
        let config = config::PanelConfig {
            edge: config::Edge::Bottom,
            ..config::PanelConfig::default()
        };

        let geometry = SurfaceGeometry::of(
            SurfaceRole::Popover(PopoverKind::QuickSettings),
            &config,
            &theme,
        );

        let panel = SurfaceGeometry::of(SurfaceRole::Bar, &config, &theme);
        let strip = panel.exclusive_zone + panel.margin.2;
        assert_eq!(geometry.anchor, Anchor::Bottom | Anchor::Right);
        assert_eq!(geometry.margin.0, 0);
        assert_eq!(
            geometry.margin.2,
            (theme.sizes.popover_top as i32 - strip).max(0)
        );
    }

    /// The popover's distinguishing settings survive the trip into the
    /// struct the runtime actually spawns from — `layer` and
    /// `keyboard_interactivity` in particular, which were hardcoded at this
    /// site before Stage 16 and would otherwise silently come out as a
    /// `Top`-layer, keyboard-less surface.
    #[test]
    fn popover_spawn_settings_carry_the_layer_and_keyboard_interactivity() {
        let theme = Theme::saola();
        let geometry = SurfaceGeometry::of(
            SurfaceRole::Popover(PopoverKind::QuickSettings),
            &config::PanelConfig::default(),
            &theme,
        );

        let settings = geometry.new_layer_shell_settings();

        assert_eq!(settings.layer, Layer::Overlay);
        assert_eq!(
            settings.keyboard_interactivity,
            KeyboardInteractivity::OnDemand
        );
        assert_eq!(settings.exclusive_zone, Some(0));
        assert_eq!(settings.size, Some(geometry.size));
        assert!(!settings.events_transparent);
    }

    /// A popover surface draws the popover, not a bar — the registry
    /// dispatch `Panel::view` does for every surface it owns.
    #[test]
    fn a_registered_popover_surface_renders_the_popover() {
        let mut panel = test_panel();
        let id = window::Id::unique();
        panel
            .windows
            .insert(id, SurfaceRole::Popover(PopoverKind::QuickSettings));

        assert_eq!(
            panel.role(id),
            SurfaceRole::Popover(PopoverKind::QuickSettings)
        );
        let _: Element<'_, Message> = panel.view(id);
    }

    /// The end-to-end open: a trigger click registers exactly one new
    /// surface, in the popover role.
    #[test]
    fn a_trigger_click_spawns_one_popover_surface() {
        let mut panel = test_panel();

        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));

        assert_eq!(panel.windows.len(), 1);
        assert_eq!(
            panel.windows.values().next(),
            Some(&SurfaceRole::Popover(PopoverKind::QuickSettings))
        );
    }

    /// …and the end-to-end close. That the second click closes rather than
    /// opening a second popover is the proof that the Id minted at spawn time
    /// actually reached `PopoverManager::opened` — the one step in the
    /// three-step open that nothing else would catch.
    #[test]
    fn a_second_trigger_click_removes_the_popover_surface() {
        let mut panel = test_panel();
        let trigger = Message::Popover(popover::Message::Triggered(PopoverKind::QuickSettings));

        let _ = panel.update(trigger.clone());
        let _ = panel.update(trigger);

        assert!(
            panel.windows.is_empty(),
            "the second click must close the popover, not open another"
        );
    }

    /// Escape and focus loss reach the same place as a second trigger click.
    /// Both are driven through `Panel::update` rather than the manager
    /// directly, so this also pins the `Message::Popover(..)` routing.
    #[test]
    fn escape_and_focus_loss_close_the_popover_surface() {
        for dismissal in [
            popover::Message::Escaped(window::Id::unique()),
            popover::Message::Unfocused(window::Id::unique()),
        ] {
            let mut panel = test_panel();
            let _ = panel.update(Message::Popover(popover::Message::Triggered(
                PopoverKind::QuickSettings,
            )));
            assert_eq!(panel.windows.len(), 1);

            let _ = panel.update(Message::Popover(dismissal));

            assert!(panel.windows.is_empty());
        }
    }

    /// A dismissal with nothing open must not touch the registry — the bar's
    /// own surface is registered there too, and an over-eager close would
    /// take the panel down.
    #[test]
    fn a_dismissal_with_nothing_open_leaves_the_panel_surfaces_alone() {
        let mut panel = test_panel();
        let bar = window::Id::unique();
        let _ = panel.update(Message::SurfaceOpened(bar));

        let _ = panel.update(Message::Popover(popover::Message::Unfocused(bar)));

        assert_eq!(panel.windows.get(&bar), Some(&SurfaceRole::Bar));
    }

    /// A popover surface reporting `Closed` (rather than being closed by us)
    /// clears the manager as well as the registry, so the next trigger click
    /// opens rather than trying to close a surface that is already gone.
    #[test]
    fn a_popover_that_closes_itself_leaves_the_manager_ready_to_reopen() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));
        let id = *panel.windows.keys().next().expect("one popover surface");

        let _ = panel.update(Message::SurfaceClosed(id));
        assert!(panel.windows.is_empty());

        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));

        assert_eq!(panel.windows.len(), 1, "the next click must reopen it");
    }

    /// Both layouts render their trigger. The trigger-button wrapping is
    /// invisible to the widget tree's *type*, so what this really pins is
    /// that neither `bar_view` nor `island_view` panics once the status
    /// cluster stops being a plain row.
    #[test]
    fn both_layouts_render_with_the_trigger_wired() {
        let ledger = test_panel();
        let _: Element<'_, Message> = ledger.bar_view();

        let islands = islands_panel();
        for kind in [IslandKind::Left, IslandKind::Centre, IslandKind::Right] {
            let _: Element<'_, Message> = islands.island_view(kind);
        }
    }

    // ---- The Claude Code usage popover ----------------------------------

    /// One loaded row, transcript-less — enough shape for the apply/discard
    /// tests below.
    fn usage_fixture() -> Vec<popovers::claude_usage::SessionUsage> {
        vec![popovers::claude_usage::SessionUsage {
            target: modules::claude::UsageTarget {
                id: "aaaaaaaa-1111".to_string(),
                status: saola_theme::style::container::SessionStatus::Done,
                transcript: None,
            },
            usage: None,
        }]
    }

    /// The claude group's trigger opens exactly one popover surface, in the
    /// `ClaudeUsage` role — and a second click closes it, same lifecycle as
    /// every other trigger.
    #[test]
    fn the_claude_trigger_toggles_a_usage_popover_surface() {
        let mut panel = test_panel();
        let trigger = || Message::Popover(popover::Message::Triggered(PopoverKind::ClaudeUsage));

        let _ = panel.update(trigger());
        assert_eq!(panel.windows.len(), 1);
        assert_eq!(
            panel.windows.values().next(),
            Some(&SurfaceRole::Popover(PopoverKind::ClaudeUsage))
        );

        let _ = panel.update(trigger());
        assert!(panel.windows.is_empty());
    }

    /// A transcript read applies only while the usage popover is still the
    /// open one — a read landing after dismissal is discarded, the same
    /// staleness guard as `TrayMenu::Loaded`'s.
    #[test]
    fn a_usage_read_applies_only_while_the_popover_is_open() {
        let mut panel = test_panel();
        let trigger = || Message::Popover(popover::Message::Triggered(PopoverKind::ClaudeUsage));

        let _ = panel.update(trigger());
        let _ = panel.update(Message::ClaudeUsage(
            popovers::claude_usage::Message::Loaded(usage_fixture()),
        ));
        assert_eq!(panel.claude_usage.sessions().len(), 1);

        // Close it; a straggler read must not resurrect the state.
        let _ = panel.update(trigger());
        let _ = panel.update(Message::ClaudeUsage(
            popovers::claude_usage::Message::Loaded(usage_fixture()),
        ));
        assert!(panel.claude_usage.sessions().is_empty());
    }

    /// The global one-open-at-a-time rule covers the new kind too.
    #[test]
    fn the_usage_popover_and_quick_settings_displace_each_other() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));

        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::ClaudeUsage,
        )));

        assert_eq!(panel.windows.len(), 1);
        assert_eq!(
            panel.windows.values().next(),
            Some(&SurfaceRole::Popover(PopoverKind::ClaudeUsage))
        );
    }

    /// A usage-popover surface renders its content — the `popover_view`
    /// arm for the new kind.
    #[test]
    fn a_registered_usage_popover_surface_renders() {
        let mut panel = test_panel();
        let id = window::Id::unique();
        panel
            .windows
            .insert(id, SurfaceRole::Popover(PopoverKind::ClaudeUsage));
        let _: Element<'_, Message> = panel.view(id);
    }

    /// The right region's split: `claude` and `tray` leave the status
    /// cluster wherever the config listed them; everything else stays, in
    /// order.
    #[test]
    fn the_right_region_split_extracts_claude_and_tray() {
        let panel = test_panel();
        let (cluster, claude_listed, tray_listed) = panel.right_region_split();
        assert_eq!(
            cluster,
            vec![
                config::ModuleName::Mpris,
                config::ModuleName::Volume,
                config::ModuleName::Network,
                config::ModuleName::Bluetooth,
                config::ModuleName::Battery,
            ]
        );
        assert!(claude_listed);
        assert!(tray_listed);

        // A config that drops them reports them unlisted — no phantom
        // trigger group for a module the user removed.
        let trimmed = panel_with(config::PanelConfig {
            right: vec![config::ModuleName::Battery],
            ..config::PanelConfig::default()
        });
        let (cluster, claude_listed, tray_listed) = trimmed.right_region_split();
        assert_eq!(cluster, vec![config::ModuleName::Battery]);
        assert!(!claude_listed);
        assert!(!tray_listed);
    }

    // ---- Stage 21: tray menus via popovers ------------------------------

    /// A right-click opens exactly one popover surface, in the `TrayMenu`
    /// role, and records the item it belongs to.
    #[test]
    fn a_right_click_opens_a_tray_menu_popover_for_that_item() {
        let mut panel = test_panel();

        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-a".to_string(),
        )));

        assert_eq!(panel.windows.len(), 1);
        assert_eq!(
            panel.windows.values().next(),
            Some(&SurfaceRole::Popover(PopoverKind::TrayMenu))
        );
        assert_eq!(panel.tray_menu.item_id(), Some("item-a"));
    }

    /// A second right-click on the *same* item's icon closes the popover,
    /// same as any other trigger's "click again to dismiss" — and forgets
    /// the item, so a stale `Loaded` can no longer apply (see the next
    /// test).
    #[test]
    fn a_second_right_click_on_the_same_item_closes_the_tray_menu() {
        let mut panel = test_panel();
        let context_menu =
            || Message::Tray(modules::tray::Message::ContextMenu("item-a".to_string()));

        let _ = panel.update(context_menu());
        let _ = panel.update(context_menu());

        assert!(panel.windows.is_empty());
        assert_eq!(panel.tray_menu.item_id(), None);
    }

    /// Right-clicking a *different* item's icon while a tray menu is open
    /// must not cycle the surface through PopoverManager's same-kind
    /// "close" rule (which would just close it and need a second click to
    /// see the new item) — the content swaps in place instead.
    #[test]
    fn right_clicking_a_different_item_swaps_content_without_closing() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-a".to_string(),
        )));

        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-b".to_string(),
        )));

        assert_eq!(
            panel.windows.len(),
            1,
            "the same surface must still be open, not closed-then-reopened"
        );
        assert_eq!(panel.tray_menu.item_id(), Some("item-b"));
    }

    /// The spec's global one-open-at-a-time rule applies across kinds: a
    /// tray menu closes an open quick-settings popover, and vice versa.
    #[test]
    fn a_tray_menu_and_quick_settings_displace_each_other() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));
        assert_eq!(panel.windows.len(), 1);

        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-a".to_string(),
        )));

        assert_eq!(
            panel.windows.len(),
            1,
            "opening the tray menu must close quick settings, not add a second surface"
        );
        assert_eq!(
            panel.windows.values().next(),
            Some(&SurfaceRole::Popover(PopoverKind::TrayMenu))
        );

        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));

        assert_eq!(panel.windows.len(), 1);
        assert_eq!(
            panel.windows.values().next(),
            Some(&SurfaceRole::Popover(PopoverKind::QuickSettings)),
            "and quick settings must likewise close an open tray menu"
        );
    }

    /// A menu tree with one leaf (id 4) and one submenu row (id 3, one
    /// child already present — no lazy fetch needed) — enough shape for
    /// the `Loaded`/`RowActivated`/`ToggleSubmenu` tests below.
    /// `modules::tray::menu::{Menu, MenuNode}`'s fields are `pub(crate)`
    /// specifically so content like this can be built directly, the same
    /// way `popovers::tray_menu`'s own tests do.
    fn fixture_menu() -> modules::tray::menu::Menu {
        use modules::tray::menu::MenuNode;
        modules::tray::menu::Menu {
            revision: 0,
            root: MenuNode {
                children: vec![
                    MenuNode {
                        id: SUBMENU_ROW_ID,
                        has_submenu: true,
                        children: vec![MenuNode {
                            id: 99,
                            ..MenuNode::default()
                        }],
                        ..MenuNode::default()
                    },
                    MenuNode {
                        id: 4,
                        ..MenuNode::default()
                    },
                ],
                ..MenuNode::default()
            },
        }
    }

    const SUBMENU_ROW_ID: i32 = 3;

    /// `Loaded` only applies while it's still an answer for the item the
    /// popover currently shows — a stale reply (the user right-clicked a
    /// different item, or closed the popover, before this one landed) is
    /// silently discarded rather than clobbering newer state.
    #[test]
    fn a_stale_loaded_message_is_discarded() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-a".to_string(),
        )));
        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-b".to_string(),
        )));

        // A reply meant for "item-a" arrives after the user already moved
        // on to "item-b".
        let _ = panel.update(Message::TrayMenu(popovers::tray_menu::Message::Loaded(
            "item-a".to_string(),
            Some(fixture_menu()),
        )));
        assert_eq!(
            panel.tray_menu.menu(),
            None,
            "a reply for an item that's no longer open must not apply"
        );

        let _ = panel.update(Message::TrayMenu(popovers::tray_menu::Message::Loaded(
            "item-b".to_string(),
            Some(fixture_menu()),
        )));
        assert!(
            panel.tray_menu.menu().is_some(),
            "a reply for the item currently open must apply"
        );
    }

    /// Clicking a leaf row closes the popover and forgets the tray-menu
    /// state — PLAN.md Stage 21's "click a leaf → `Event(\"clicked\")` +
    /// close".
    #[test]
    fn row_activated_closes_the_popover_and_clears_state() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-a".to_string(),
        )));
        assert_eq!(panel.windows.len(), 1);

        let _ = panel.update(Message::TrayMenu(
            popovers::tray_menu::Message::RowActivated(4),
        ));

        assert!(panel.windows.is_empty());
        assert_eq!(panel.tray_menu.item_id(), None);
    }

    /// A submenu row toggles into (and back out of) `TrayMenuState::
    /// expanded` without a fetch when its children already arrived with the
    /// initial (`recursion_depth: -1`) read.
    #[test]
    fn toggle_submenu_flips_expanded_state() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-a".to_string(),
        )));
        let _ = panel.update(Message::TrayMenu(popovers::tray_menu::Message::Loaded(
            "item-a".to_string(),
            Some(fixture_menu()),
        )));

        let _ = panel.update(Message::TrayMenu(
            popovers::tray_menu::Message::ToggleSubmenu(SUBMENU_ROW_ID),
        ));
        assert!(panel.tray_menu.is_expanded(SUBMENU_ROW_ID));

        let _ = panel.update(Message::TrayMenu(
            popovers::tray_menu::Message::ToggleSubmenu(SUBMENU_ROW_ID),
        ));
        assert!(!panel.tray_menu.is_expanded(SUBMENU_ROW_ID));
    }

    // ---- Live config reload ---------------------------------------------

    /// Drive a reload the way the watcher would: through `Panel::update`
    /// with the watcher's own message, so these tests pin the routing as
    /// well as `reload_config` itself.
    fn reload(panel: &mut Panel, config: config::PanelConfig) -> Task<Message> {
        panel.update(Message::Config(config_watch::Message::Reloaded(config)))
    }

    /// The config swaps wholesale, and the state that was *copied out of*
    /// it at boot follows: the mark's launcher here (rebuilt), the module
    /// lists by way of `self.config` (read live by `view`, so the swap is
    /// enough).
    #[test]
    fn a_reload_swaps_the_config_and_rebuilds_the_config_fed_modules() {
        let mut panel = test_panel();
        assert_eq!(panel.mark.launcher(), Some(config::DEFAULT_LAUNCHER));

        let _ = reload(
            &mut panel,
            config::PanelConfig {
                launcher: Some("wofi --show drun".to_owned()),
                right: vec![config::ModuleName::Battery],
                ..config::PanelConfig::default()
            },
        );

        assert_eq!(panel.config.right, vec![config::ModuleName::Battery]);
        assert_eq!(panel.mark.launcher(), Some("wofi --show drun"));
        // And the new layout renders — the same no-panic proof the boot
        // config gets.
        let _: Element<'_, Message> = panel.bar_view();
    }

    /// A reloaded `colors { }` reaches the running theme — and one *removed*
    /// from the file reverts to the stock palette, which is why
    /// `reload_config` rebuilds from `Theme::saola()` rather than mutating
    /// the current palette (apply only writes `Some` fields).
    #[test]
    fn a_reload_applies_and_reverts_color_overrides() {
        let mut panel = test_panel();
        let stock_accent = Theme::saola().palette.accent;
        let custom = saola_theme::tokens::Color::parse_hex("#123456").unwrap();

        let _ = reload(
            &mut panel,
            config::PanelConfig {
                colors: config::ColorOverrides {
                    accent: Some(custom),
                    ..config::ColorOverrides::default()
                },
                ..config::PanelConfig::default()
            },
        );
        assert_eq!(panel.theme.palette.accent, custom);

        let _ = reload(&mut panel, config::PanelConfig::default());
        assert_eq!(panel.theme.palette.accent, stock_accent);
    }

    /// The boot CLI flags keep beating the file on every reload, exactly as
    /// they beat it at boot — a `--islands` session stays Islands however
    /// often a `style "ledger"` config is re-saved.
    #[test]
    fn a_reload_keeps_cli_overrides_winning_over_the_file() {
        let mut panel = Panel::new(
            islands_config(),
            config::CliOverrides {
                style: Some(config::PanelStyle::Islands),
                ..config::CliOverrides::default()
            },
            None,
            Theme::saola(),
        );

        let _ = reload(&mut panel, config::PanelConfig::default());

        assert_eq!(panel.config.style, config::PanelStyle::Islands);
        // A knob the flags don't cover still follows the file.
        assert_eq!(panel.config.edge, config::Edge::Top);
    }

    /// A style flip re-roles the boot surface in place — one surface before,
    /// the same one surface after, now drawing the other layout.
    #[test]
    fn a_style_reload_re_roles_the_panel_surface_in_place() {
        let mut panel = test_panel();
        let id = window::Id::unique();
        let _ = panel.update(Message::SurfaceOpened(id));
        assert_eq!(panel.windows.get(&id), Some(&SurfaceRole::Bar));

        let _ = reload(&mut panel, islands_config());

        assert_eq!(panel.windows.len(), 1);
        assert_eq!(panel.windows.get(&id), Some(&SurfaceRole::Islands));
        // The unknown-Id fallback follows too — a second output's surface
        // opening after the reload must come up as the strip.
        assert_eq!(panel.role(window::Id::unique()), SurfaceRole::Islands);
    }

    /// A reload that moves the panel dismisses an open popover (its surface
    /// geometry was derived from the old config at spawn time and cannot be
    /// migrated), while the panel surface itself stays registered.
    #[test]
    fn a_geometry_reload_closes_an_open_popover() {
        let mut panel = test_panel();
        let bar = window::Id::unique();
        let _ = panel.update(Message::SurfaceOpened(bar));
        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));
        assert_eq!(panel.windows.len(), 2);

        let _ = reload(
            &mut panel,
            config::PanelConfig {
                height: Some(64.0),
                ..config::PanelConfig::default()
            },
        );

        assert_eq!(
            panel.windows.keys().collect::<Vec<_>>(),
            vec![&bar],
            "the popover surface must be gone, the bar's must remain"
        );
        assert!(!panel.popovers.is_open(PopoverKind::QuickSettings));
    }

    /// When the popover a geometry reload dismisses is the tray menu, its
    /// content state goes with it — leaving `tray_menu` populated would
    /// keep the Id-keyed dbusmenu watcher alive for a surface that no
    /// longer exists, and let a straggler `Loaded` reply fold menu data
    /// into dead state.
    #[test]
    fn a_geometry_reload_clears_the_dismissed_tray_menus_state() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Tray(modules::tray::Message::ContextMenu(
            "item-a".to_string(),
        )));
        assert_eq!(panel.tray_menu.item_id(), Some("item-a"));

        let _ = reload(
            &mut panel,
            config::PanelConfig {
                height: Some(64.0),
                ..config::PanelConfig::default()
            },
        );

        assert!(panel.windows.is_empty(), "the tray-menu surface is gone");
        assert_eq!(
            panel.tray_menu.item_id(),
            None,
            "its content state (and thereby its keyed watcher) must go too"
        );
    }

    /// A content-only reload (nothing geometric changed) leaves an open
    /// popover alone — dismissal is the price of a *moved* panel, not of
    /// every config edit.
    #[test]
    fn a_content_only_reload_leaves_an_open_popover_alone() {
        let mut panel = test_panel();
        let _ = panel.update(Message::Popover(popover::Message::Triggered(
            PopoverKind::QuickSettings,
        )));

        let _ = reload(
            &mut panel,
            config::PanelConfig {
                claude_icon: config::ClaudeIcon::ClaudeCode,
                ..config::PanelConfig::default()
            },
        );

        assert!(panel.popovers.is_open(PopoverKind::QuickSettings));
        assert_eq!(panel.config.claude_icon, config::ClaudeIcon::ClaudeCode);
    }
}
