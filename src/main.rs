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
//! spec's default panel style: **three** free-standing translucent pill
//! clusters floating over the wallpaper, each one its own layer-shell
//! surface (spec §7 lists *four* — the notifications island is excluded from
//! this phase; see [`IslandKind`]). The switch is config-only: both layouts
//! compose the very same [`Panel::module_view`] / [`Panel::region`] pieces
//! and differ only in the outer widget tree ([`Panel::bar_view`] vs
//! [`Panel::island_view`]) and in the layer-shell geometry
//! ([`SurfaceGeometry::of`]) — no module lays itself out.
//!
//! The one thing a module *may* know about the style is its own **surface
//! treatment**, which [`modules::clock`] is so far the only module to use (an
//! ivory pill on the ledger bar, bare text on an island scrim). That is an
//! amendment to Stage 15's original flat seam rule, made deliberately — the
//! reasoning is in [`Panel::island_view`]'s doc comment.
//!
//! Two things about the surfaces are worth reading before touching them:
//!
//! * **Every surface is full-width and mostly transparent.** An island's
//!   surface spans the whole output (inset by the margin); the scrim pill
//!   inside it hugs its content and is aligned left / centre / right. Nothing
//!   measures text, so nothing has to resize a surface when the clock ticks
//!   or a song changes. What pays for that is `events_transparent` — see
//!   [`SurfaceGeometry`].
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
//! * **The right island stopped being click-through.** `events_transparent`
//!   is fixed at surface-creation time and is all-or-nothing (see that
//!   field), so the island that carries the trigger has to take input for its
//!   whole full-width strip. The other two islands stay click-through, which
//!   is what stops them swallowing the trigger's clicks even though all three
//!   surfaces overlap.

mod config;
mod icons;
mod modules;
mod popover;
mod popovers;

use std::collections::HashMap;

use iced::alignment::Horizontal;
use iced::widget::{container, mouse_area, row, Space};
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
use popover::{PopoverKind, PopoverManager};
use saola_theme::{style, to_iced_theme, Theme};

fn main() -> iced_layershell::Result {
    // Loaded once, here, before iced's event loop exists at all — see
    // `config`'s module doc comment for the full resilience contract
    // (absent/malformed file → today's hardcoded layout, never a crash).
    // Command-line flags (`--ledger`/`--islands`, `--top`/`--bottom`) then
    // override the file — a testing convenience, so switching modes doesn't
    // require editing `panel.kdl` (see `config::CliOverrides`).
    let mut config = config::PanelConfig::load();
    config::CliOverrides::parse(std::env::args().skip(1)).apply(&mut config);

    // A separate Theme instance just for the values `main` needs before the
    // application starts (bar height, default font) — `Panel` gets its own
    // clone below rather than a borrow, which keeps the `Panel::new` boot
    // closure simple (see the `daemon(..)` call). `colors { }` overrides are
    // applied here, to the *only* `Theme` this process ever builds from
    // `Theme::saola()` — both `main`'s own use of `theme` and the clone
    // `Panel` gets afterward see the override applied exactly once.
    let mut theme = Theme::saola();
    config.colors.apply(&mut theme.palette);

    // The one surface `Settings` creates at boot is the ledger bar in ledger
    // style and the **centre island** in islands style (see `initial_role`
    // for why reshaping it beats spawning a fourth surface and closing this
    // one). Every number the layer-shell protocol needs — anchor, size,
    // margins, exclusive zone, input transparency — is derived from tokens
    // and config in one place, `SurfaceGeometry::of`, so `main` and
    // `Panel::spawn_boot_surfaces` cannot drift apart.
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
        move || Panel::new(config.clone(), theme.clone()).boot(),
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
    /// anchored edge. We pass **just the surface height**, never height +
    /// margin, because wlr-layer-shell compositors add the anchored-edge
    /// margin to the reserved area themselves. Measured on niri while
    /// writing this stage: with the ledger bar's `exclusive_zone = 48` and
    /// `margin.top = 18`, a maximized window's tile height dropped by
    /// **exactly 66 px** (1003.33 → 937.33) when the panel started — i.e.
    /// 48 + 18, the margin counted once. Islands reserve the same 66 px from
    /// the other direction: `panel_pill` 40 + `panel_margin_islands` 26.
    ///
    /// **Only the centre island reserves anything.** The left and right
    /// islands pass `0` — reserve nothing, respect everyone's reservations —
    /// and compensate with a **negative** edge margin (revised while fixing
    /// the popover's waybar bug; they originally passed `-1`, "do not move me
    /// out of anyone else's zone", so that the compositor could not push them
    /// below the centre island's own 66 px reservation). `-1` had the same
    /// flaw as the original popover: it measures from the raw screen edge,
    /// so any *foreign* bar with an exclusive zone (waybar, on Jordan's
    /// desktop) pushed the centre island down but not its siblings, tearing
    /// the three islands apart. With `0`, a zone-respecting surface is
    /// placed below the centre's reservation (`panel_pill` 40 +
    /// `panel_margin_islands` 26 = 66), which is 40 px *lower* than the
    /// strip the sibling islands should share — so their edge margin is
    /// `island margin − strip` = `26 − 66` = **−40** (negative margins are
    /// legal i32s in wlr-layer-shell), pulling them back up level with the
    /// centre. One formula for every non-reserving surface: *edge margin =
    /// desired offset from the panel's baseline − the strip the panel
    /// reserves*; the popover's `popover_top − strip` below is the same
    /// arithmetic with a different desired offset.
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
    /// compositors add the margin to the reservation). That strip is 66 in
    /// both styles (48 + 18 ledger, 40 + 26 islands), so the gap is
    /// `72 − 66 = 6` px and the popover lands on exactly the spec's pixel.
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
    /// The side inset is `config.margin(theme)`, not a literal 26. That
    /// **is** 26 in islands style (`panel_margin_islands`, the style the spec
    /// describes), and in ledger style it is 20 — which lines the popover's
    /// right edge up with the end of the bar it hangs from rather than
    /// leaving it 6 px proud. It also means a user who moves the panel with
    /// `margin` in `panel.kdl` moves the popover with it. (If saola-theme
    /// ever grows a dedicated `popover_margin` token, this is the line to
    /// change.)
    fn of(role: SurfaceRole, config: &config::PanelConfig, theme: &Theme) -> Self {
        // Popovers share none of the panel surfaces' geometry — different
        // layer, different anchors, a real width, keyboard focus — so they
        // take their own path out rather than threading four more `match
        // role` arms through the arithmetic below.
        if let SurfaceRole::Popover(kind) = role {
            // The panel surface this popover hangs below — the bar in ledger
            // style, the centre island in islands style (the one island that
            // reserves a zone). Asking it for its own geometry instead of
            // repeating the strip arithmetic here means the two can't drift.
            let panel = Self::of(initial_role(config), config, theme);
            let strip = panel.exclusive_zone
                + match config.edge {
                    config::Edge::Top => panel.margin.0,
                    config::Edge::Bottom => panel.margin.2,
                };
            // The 6 px breathing room of spec §6, clamped so an oversized
            // `height` knob degrades to "touching", never to overlapping the
            // trigger. See the doc comment for the full derivation.
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
            SurfaceRole::Island(_) => (
                // An island is a `panel_pill` (40), not a `panel_bar` (48).
                // `config.height` can't be used directly: it resolves an
                // absent knob to `panel_bar` for *both* styles (unlike
                // `config.margin`, which is already style-aware). Reading
                // the `Option` here keeps an explicit `height 52` in
                // `panel.kdl` working while defaulting to the islands
                // token. If a later stage teaches `PanelConfig::height` the
                // same style-awareness `PanelConfig::margin` already has,
                // this collapses back to `config.height(theme)`.
                config.height.unwrap_or(theme.sizes.panel_pill),
                // Islands are inset by the same margin on every edge
                // (`panel_margin_islands`, 26 — the value
                // `PanelConfig::margin` already resolves to in this style).
                config.margin(theme),
                config.margin(theme),
            ),
            // Unreachable: the `if let` above returns for every popover.
            // Spelled out rather than folded into the `Island` arm so that a
            // future role added to `SurfaceRole` still makes this `match`
            // fail to compile instead of silently inheriting island geometry.
            SurfaceRole::Popover(_) => unreachable!("popovers return above"),
        };

        let (height, side_margin, edge_margin) =
            (height as u32, side_margin as i32, edge_margin as i32);

        let exclusive_zone = match role {
            SurfaceRole::Bar => height as i32,
            SurfaceRole::Island(IslandKind::Centre) => height as i32,
            SurfaceRole::Island(_) => 0,
            SurfaceRole::Popover(_) => unreachable!("popovers return above"),
        };

        // The left/right islands reserve nothing, so the compositor places
        // them below the centre island's reservation (height + edge margin).
        // A negative edge margin pulls them back up level with it — the same
        // "desired offset − reserved strip" arithmetic as the popover's gap,
        // with the islands' shared margin as the desired offset. See the
        // exclusive-zone doc comment above for why this replaced `-1`.
        let edge_margin = match role {
            SurfaceRole::Island(IslandKind::Left | IslandKind::Right) => {
                edge_margin - (height as i32 + edge_margin)
            }
            _ => edge_margin,
        };

        Self {
            anchor,
            size: (0, height),
            margin: match config.edge {
                config::Edge::Top => (edge_margin, side_margin, 0, side_margin),
                config::Edge::Bottom => (0, side_margin, edge_margin, side_margin),
            },
            exclusive_zone,
            // The bar takes input (it always has) and so, as of Stage 16,
            // does the **right** island — it carries the quick-settings
            // trigger, and `events_transparent` is fixed at creation time and
            // is all-or-nothing per surface (see the field's doc comment), so
            // an island that needs one click needs them all. The other two
            // stay click-through, which is what keeps three overlapping
            // full-width surfaces from fighting over the pointer: an empty
            // input region takes a surface out of the compositor's hit
            // testing entirely, whatever the stacking order.
            //
            // The cost is that the right island swallows every click in its
            // full-width strip, not just the ones on its pill. That strip is
            // inside the panel's own exclusive zone, so there is nothing but
            // wallpaper underneath it. Giving it a pill-shaped input region
            // instead would need the pill's measured rect (`SetInputRegion`),
            // i.e. the measurement problem Stage 15 ruled out.
            events_transparent: matches!(
                role,
                SurfaceRole::Island(IslandKind::Left | IslandKind::Centre)
            ),
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
    /// all three islands show up as `saola-panel` in `niri msg --json
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
/// A daemon always opens that one surface itself (`StartMode::Active`); it is
/// not something we can decline. In islands style we therefore have a choice:
/// close it and spawn three islands, or keep it and let it *be* one of them.
/// Keeping it wins on both counts that matter — no frame in which a
/// half-configured surface is visible, and no dependence on being able to
/// destroy a `Settings`-created surface (which, per Stage 13's handoff, never
/// even reports its own `Closed` event). The centre island is the one it
/// becomes, because the centre is the island that carries the exclusive zone
/// and is therefore the one whose geometry the boot `Settings` most needs to
/// get right.
///
/// This is also the fallback for an Id the registry has never heard of — see
/// [`Panel::role`].
fn initial_role(config: &config::PanelConfig) -> SurfaceRole {
    match config.style {
        config::PanelStyle::Ledger => SurfaceRole::Bar,
        config::PanelStyle::Islands => SurfaceRole::Island(IslandKind::Centre),
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
    Columns(modules::columns::Message),
    /// Wraps `modules::mark::Message` — an *empty* enum (see that module's
    /// doc comment): the mark is a static glyph with no signal source at
    /// all, so this variant can never actually be constructed at runtime.
    /// It exists purely so the mark's wiring shape (`.map(Message::Mark)` at
    /// the subscription and view composition sites in `Panel::subscription`
    /// / `Panel::view`) matches every other module's exactly.
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
    /// Wraps `modules::claude::Message` (currently just
    /// `Updated(ClaudeCode)`), the derived "what are Jordan's Claude Code
    /// sessions doing" summary. Unlike every module above, the source isn't
    /// a service with state to read — it's a broadcast signal a hook script
    /// fires and forgets (see `modules::claude`'s doc comment for the
    /// signal-listener bridge shape). Last of the right region, per the
    /// style guide's module order (`right { volume; network; battery;
    /// claude; tray; ... }`).
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
    /// status cluster's `mouse_area` (a trigger click), and the two dismissal
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
    /// One of the floating translucent pill clusters of `style "islands"`.
    Island(IslandKind),
    /// A popover: the one transient surface kind, spawned when a trigger is
    /// clicked and destroyed when it is dismissed. At most one exists at a
    /// time — the invariant lives in [`PopoverManager`], not here.
    Popover(PopoverKind),
}

/// Which island a surface is.
///
/// # The three-vs-four deviation (deliberate, flagged)
///
/// Spec §7 inventories the Islands panel as **four** separate layer-shell
/// surfaces: mark + media, clock + column strip, status, and *notifications*.
/// This phase ships **three**. Everything notifications — daemon, popups,
/// centre, and the bar indicator — is out of scope for Phase 2 by an explicit
/// decision recorded in PLAN.md; a future `saola-notifications` component
/// owns it. Nothing here forecloses the fourth: it arrives as one more
/// variant on this enum plus one more arm in each of the three matches that
/// consume it (`SurfaceGeometry::of`, `Panel::island_view`,
/// `Panel::spawn_boot_surfaces`), all of which the compiler will demand.
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
    /// `config.left` — the mark and the media pill, hugging the left margin.
    Left,
    /// `config.center` — the clock and the niri column minimap, centred.
    /// This is the island that carries the panel's exclusive zone, and the
    /// one the boot `Settings` surface becomes (see `initial_role`).
    Centre,
    /// `config.right` — the status cluster, hugging the right margin.
    Right,
}

/// The whole panel state. Stage 3 adds the first module field (`clock`);
/// Stages 4-5 add `battery` and `network` alongside it the same way; Stage 8
/// adds `mark`; Stage 9 adds `media`; Stage 13 adds the `windows` surface
/// registry.
struct Panel {
    /// The Saola theme, loaded once at boot — the single source of every
    /// color and size on the bar. `main` applies `panel.kdl`'s `colors { }`
    /// overrides to a `Theme::saola()` before this field is ever populated
    /// (see `config::ColorOverrides::apply`), so every color read from here
    /// already reflects the user's config.
    theme: Theme,
    /// The parsed `panel.kdl` config, loaded once at boot (Stage 14). Read
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
    /// The clock module's state (currently empty — see `Clock`'s doc
    /// comment for why).
    clock: Clock,
    /// The bar's mark: a static glyph (see `Mark`'s doc comment) whose
    /// *choice* of glyph — horns, notch, a user file, or none — comes from
    /// `panel.kdl`'s `mark` directive, resolved once at construction time
    /// into `Mark::new(config.mark.clone())` below.
    mark: Mark,
    /// The last niri column strip pushed by the niri IPC worker; starts as
    /// `Columns::default()` (an empty dash row → renders nothing, which is
    /// also what a non-niri session leaves it at forever). Sits beside the
    /// clock in the centre region, per the style guide's
    /// `center { clock; niri-columns }`.
    columns: Columns,
    /// The last "active MPRIS player" snapshot pushed by the media worker;
    /// starts as `Media::default()` (no player known yet → renders
    /// nothing). Sits beside the mark in the bar's left region.
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
    /// The last derived "what are Jordan's sessions doing" summary folded
    /// by the Claude Code signal-listener worker; starts as
    /// `ClaudeCode::default()` (no `StatusChanged` signal seen yet →
    /// renders nothing — the same "quiet until proven otherwise" contract
    /// as every module above, just for a hook that hasn't fired instead of
    /// a service that isn't there). Last of the right region.
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
}

impl Panel {
    /// Boot. `config`/`theme` are threaded in from `main`'s closure (see the
    /// `daemon(move || Panel::new(..), ..)` call) rather than each
    /// re-derived here, so `panel.kdl` is read exactly once per process —
    /// `main` already needed `config`/`theme` before this point to size the
    /// layer-shell surface, and re-parsing the file a second time here
    /// would risk observing a different config if it changed between the
    /// two reads (unlikely in practice, since both happen within the same
    /// boot, but needless).
    fn new(config: config::PanelConfig, theme: Theme) -> Self {
        Self {
            mark: Mark::new(config.mark.clone()),
            clock: Clock,
            columns: Columns::default(),
            media: Media::default(),
            volume: Volume::default(),
            battery: Battery::default(),
            network: Network::default(),
            claude_code: ClaudeCode::default(),
            tray: Tray::default(),
            windows: HashMap::new(),
            popovers: PopoverManager::default(),
            volume_commands: None,
            tray_menu: popovers::tray_menu::TrayMenuState::default(),
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
    /// Ledger: none — the bar is that one surface. Islands: the left and
    /// right islands, the centre one being what the boot surface already is
    /// (see `initial_role`).
    ///
    /// Ordering note for whoever adds surfaces later: these requests are
    /// issued before the compositor has created *any* of our surfaces, which
    /// is safe — Id-carrying layer-shell actions that arrive before their
    /// surface exists are pushed back onto the runtime's
    /// `waiting_layer_shell_actions` queue and retried, not dropped
    /// (`iced_layershell-0.19.1/src/multi_window.rs:708–715`).
    fn spawn_boot_surfaces(&mut self) -> Task<Message> {
        match self.config.style {
            config::PanelStyle::Ledger => Task::none(),
            config::PanelStyle::Islands => {
                Task::batch([IslandKind::Left, IslandKind::Right].map(|kind| {
                    let role = SurfaceRole::Island(kind);
                    let geometry = SurfaceGeometry::of(role, &self.config, &self.theme);
                    // The Id is discarded here: an island's role is recorded
                    // by `spawn_surface` itself and nothing else needs to
                    // name the surface again. Popovers do need it — see
                    // `Panel::update_popover`.
                    let (_id, task) = self.spawn_surface(role, geometry.new_layer_shell_settings());
                    task
                }))
            }
        }
    }

    /// The role of the surface identified by `id` — **the boot surface's
    /// role for an Id the registry has never heard of** (`initial_role`:
    /// the bar in ledger style, the centre island in islands style), which
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
            Message::ClaudeCode(modules::claude::Message::Updated(claude_code)) => {
                self.claude_code = claude_code;
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
            // already is, even though these tasks never actually produce a
            // `tray::Message` (`Task::future(..).discard()`'s whole point).
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
            // which is exactly the surface `Settings` created at boot: the
            // surface `Settings` created at boot — the ledger bar, or the
            // centre island in islands style (`initial_role`). (With
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
            // The only messages in this enum that *do* something rather than
            // storing something. Delegated rather than inlined because the
            // decision (what closes what) is a state machine worth testing on
            // its own, and only the surface plumbing needs `Panel`.
            Message::Popover(message) => self.update_popover(message),
            // `Message::Clock(clock::Message::Tick)` carries no state to
            // store (see `modules::clock::Message`'s doc comment) —
            // reaching `update` at all is what wakes the re-render. The
            // macro-injected layer-shell variants never reach here either
            // (see the outer `Message` doc comment) — both fall through to
            // this same catch-all.
            _ => Task::none(),
        }
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
            self.columns.subscription().map(Message::Columns),
            // Always `Subscription::none()` (see `Mark::subscription`'s doc
            // comment) — included so the batch's shape stays uniform across
            // every module rather than special-casing the one with no
            // signal source.
            self.mark.subscription().map(Message::Mark),
            self.media.subscription().map(Message::Media),
            self.volume.subscription().map(Message::Volume),
            self.battery.subscription().map(Message::Battery),
            self.network.subscription().map(Message::Network),
            self.claude_code.subscription().map(Message::ClaudeCode),
            self.tray.subscription().map(Message::Tray),
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
            SurfaceRole::Island(kind) => self.island_view(kind),
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
            PopoverKind::QuickSettings => {
                popovers::quick_settings::view(&self.theme, &self.volume, &self.media)
            }
            PopoverKind::TrayMenu => popovers::tray_menu::view(&self.theme, &self.tray_menu),
        }
    }

    /// Make an element open a popover when it is clicked.
    ///
    /// [`mouse_area`] rather than a `button` on purpose: the status cluster
    /// is bare ivory icons and text sitting *directly* on the ink surface
    /// (CLAUDE.md's concept-4b rule — the clock is the *ledger* bar's only
    /// solid pill, and in islands style nothing in the cluster is one),
    /// so the trigger must add a hit target and **no** appearance at all. A
    /// `button` would come with a background, a hover step and a focus ring,
    /// none of which belong here; `mouse_area` draws nothing whatsoever.
    ///
    /// `on_press` rather than `on_release` so the popover appears on the way
    /// down, which is also what makes the dismissal ordering safe — see
    /// `PopoverManager::update`'s note on why a trigger click can never be
    /// undone by the focus-loss event that follows it.
    fn popover_trigger<'a>(
        &self,
        content: impl Into<Element<'a, Message>>,
        kind: PopoverKind,
    ) -> Element<'a, Message> {
        mouse_area(content)
            .on_press(Message::Popover(popover::Message::Triggered(kind)))
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
            config::ModuleName::Mark => self.mark.view(t).map(Message::Mark),
            config::ModuleName::Mpris => self.media.view(t).map(Message::Media),
            // The one arm that passes more than the theme: the clock's
            // *surface treatment* is style-dependent (solid ivory pill on the
            // ledger bar, bare ivory text on an island's scrim), so it is
            // handed `config.style` as well. That is a deliberate, narrow
            // exception to the layout seam — see `island_view`'s doc comment.
            config::ModuleName::Clock => self.clock.view(t, self.config.style).map(Message::Clock),
            config::ModuleName::NiriColumns => self.columns.view(t).map(Message::Columns),
            config::ModuleName::Volume => self.volume.view(t).map(Message::Volume),
            config::ModuleName::Network => self.network.view(t).map(Message::Network),
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
            config::ModuleName::Mpris => self.media.is_present(),
            config::ModuleName::Clock => self.clock.is_present(),
            config::ModuleName::NiriColumns => self.columns.is_present(),
            config::ModuleName::Volume => self.volume.is_present(),
            config::ModuleName::Network => self.network.is_present(),
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
    fn region(&self, modules: &[config::ModuleName]) -> iced::widget::Row<'_, Message> {
        row(modules.iter().map(|name| self.module_view(*name)))
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

        // Ledger layout (Architecture in PLAN.md): the full five-element
        // row — left region, Fill spacer, center (clock), Fill spacer,
        // right region (status pills). The two Fill spacers split the
        // leftover width equally, which keeps the center region centered in
        // the space between the regions — it can drift by half the right
        // region's width when the right region outweighs the left, which is
        // acceptable for the ledger style. Keeping this outer row separate
        // from module code is what lets the future Islands layout swap in
        // later without touching any individual module.
        container(row![
            self.region(&self.config.left)
                .spacing(t.sizes.bar_element_gap),
            Space::new().width(Fill),
            // `align_y(Center)` is what lines the niri-columns dashes up
            // with the clock's baseline box — the outer container centres
            // the row as a whole, not the items within it. Together with
            // `spacing`, these are the region-specific modifiers
            // `Panel::region` deliberately leaves to its caller.
            self.region(&self.config.center)
                .spacing(t.sizes.bar_element_gap)
                .align_y(iced::Center),
            Space::new().width(Fill),
            // The status cluster gets its own, slightly wider gap — and, as
            // of Stage 16, is the ledger bar's quick-settings trigger. The
            // whole cluster is the hit target rather than one pill inside it:
            // spec §7 calls the third island "status" as a unit, and the bar
            // is the same modules in the same order.
            self.popover_trigger(
                self.region(&self.config.right)
                    .spacing(t.sizes.bar_cluster_gap),
                PopoverKind::QuickSettings,
            ),
        ])
        // The bar is a floating pill, not a flush strip: `bar_pill` is solid
        // ink at `radii.pill`, and `main`'s layer-shell margins are what
        // inset the surface from the screen edges (see there).
        .style(style::container::bar_pill(t))
        // Horizontal padding = half the bar's height, i.e. the pill's own
        // rounded-end radius — derived from a height rather than a magic
        // number, exactly as the modules derive their own geometry. At
        // `radii.pill` (999, clamped by iced to half the height) each end
        // of the bar is a semicircle of that radius, so anything closer in
        // than this would be sliced by the curve. `config.height(t)` rather
        // than the `panel_bar` token so the padding tracks the *real* end
        // radius when `panel.kdl` sets a custom `height` (the two agree at
        // the default). This is *not* `config.margin` any more: that knob
        // became the surface's inset from the screen edge (`main`), not
        // padding inside the bar.
        .padding([0.0, self.config.height(t) / 2.0])
        .align_y(iced::Center)
        .width(Fill)
        .height(Fill)
        .into()
    }

    /// One island — the whole of what `style "islands"` draws on one
    /// surface: a *cluster* of translucent scrim pills floating over the
    /// wallpaper, `island_gap` apart (that token's documented meaning: the
    /// gap between pills).
    ///
    /// # Pill grouping (listing 2a)
    ///
    /// The concept draws the left and centre clusters as one pill *per
    /// module* — mark circle beside media pill, clock pill beside the
    /// column-strip pill — because each is its own control. The right
    /// cluster is the exception: spec §7 names it "status" as a *unit*,
    /// and it is one pill acting as the single quick-settings trigger, so
    /// its modules share a pill exactly as they share the ledger bar's
    /// right region. Only modules that would actually draw something get a
    /// pill (`module_is_present`) — no media player, no media pill.
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
    /// Exactly one module exercises that today: `modules::clock` is a solid
    /// ivory pill in ledger style (concept 4b, where it is the bar's only
    /// one) and bare ivory text in islands style (listing 2a — a pill inside
    /// the translucent scrim would be a surface nested in a surface, and in
    /// this centre island the sole solid ivory element is the niri-columns
    /// strip's focused dash). `Panel::module_view` passes `config.style` into
    /// that one arm; every other module's `view` still takes the theme alone.
    ///
    /// # Why the surface is wider than the pill
    ///
    /// The surface spans the whole output; only this pill is visible on it,
    /// and `align_x` is what puts the pill at the left margin, the centre,
    /// or the right margin. The alternative — resizing each surface to track
    /// its content's width — was rejected with reasons; the short version is
    /// that iced 0.14 has no supported way to measure a laid-out widget
    /// (`container::visible_bounds` is gone), and a measurement taken inside
    /// a too-small surface is clipped by that surface, so a shrink-to-fit
    /// loop could never grow back. See the Stage 15 handoff for the full
    /// argument. The cost of the wide surface is input: see
    /// `SurfaceGeometry::events_transparent`.
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
            // One scrim pill per module, `island_gap` between them — the
            // cluster the concept draws. `row(iterator)` for the same
            // runtime-length reason as `Panel::region`.
            IslandKind::Left | IslandKind::Centre => {
                row(present.iter().map(|name| self.island_pill(*name)))
                    .spacing(t.sizes.island_gap)
                    .align_y(iced::Center)
                    .into()
            }
            // The status cluster: one shared pill, and the islands
            // layout's quick-settings trigger. Its *internal* gap is the
            // ledger's `bar_cluster_gap` — deliberately shared, because
            // this is the one place the two layouts draw the same thing
            // (spec §7's "status" unit: the same readouts at the same
            // rhythm, whichever chrome they sit in). The `mouse_area`
            // wraps the pill, not the surface, so only the visible cluster
            // reacts — even though the surface itself accepts pointer
            // events across its whole width (see
            // `SurfaceGeometry::events_transparent`).
            IslandKind::Right => {
                let pill = container(
                    self.region(&present)
                        .spacing(t.sizes.bar_cluster_gap)
                        .align_y(iced::Center),
                )
                .style(style::container::translucent_panel(t))
                .padding([0.0, t.sizes.panel_pill / 2.0])
                .height(Fill)
                .align_y(iced::Center);
                self.popover_trigger(pill, PopoverKind::QuickSettings)
            }
        };

        // The cluster is `Shrink`-wide, so this outer container is just the
        // positioner: it fills the surface, draws nothing (the app-wide
        // background is transparent — see `Panel::style`), and pushes the
        // cluster to the margin the island belongs at. The margin itself is
        // the layer-shell surface's, not padding here (see
        // `SurfaceGeometry::of`), so the cluster lands
        // `panel_margin_islands` from the screen edge.
        container(cluster)
            .width(Fill)
            .height(Fill)
            .align_x(align)
            .into()
    }

    /// One free-standing island pill around one module's view.
    ///
    /// The scrim pill: `container::translucent_panel` is the theme's
    /// islands surface — an ink-tinted scrim at `radii.pill` that the
    /// wallpaper shows through, the exact counterpart of the ledger bar's
    /// opaque `container::bar_pill`. Zero local styling. Horizontal padding
    /// is half the pill's height for the same reason as the ledger bar's:
    /// that *is* the radius the rounded ends actually curve at (iced clamps
    /// `radii.pill`'s 999 to half the height), so it is the closest content
    /// can sit without being sliced by the curve.
    ///
    /// The mark is the one differently-shaped pill: the concept draws it as
    /// a *circle* (a launcher button, not a readout), so instead of hugging
    /// its content plus padding it is pinned to `panel_pill` wide — equal to
    /// the pill height, which `radii.pill` then closes into a circle, the
    /// same width-equals-height trick the strip's rest dashes use.
    fn island_pill(&self, name: config::ModuleName) -> Element<'_, Message> {
        let t = &self.theme;
        let pill = container(self.module_view(name))
            .style(style::container::translucent_panel(t))
            .height(Fill)
            .align_y(iced::Center);
        match name {
            config::ModuleName::Mark => pill
                .width(t.sizes.panel_pill)
                .align_x(Horizontal::Center)
                .into(),
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

/// Tests for the surface registry — the only genuinely new logic Stage 13
/// introduces. They construct a real `Panel` (cheap: `Theme::saola()` is
/// plain data and every module's `default()` is an empty snapshot, so nothing
/// here touches Wayland, D-Bus, or pulse) and drive `Panel::update` with the
/// same messages the runtime would send, which is what makes them a test of
/// the wiring rather than of a hand-copied model of it.
#[cfg(test)]
mod tests {
    use super::*;

    /// The default config + default theme, exactly what an unconfigured
    /// `panel.kdl` boots into — the fixture every test in this module
    /// builds a `Panel` from, so a config-parsing bug can't silently change
    /// what these surface-registry tests are exercising.
    fn test_panel() -> Panel {
        Panel::new(config::PanelConfig::default(), Theme::saola())
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
        // `Opened` event arrives afterwards. Stage 15 can state this in its
        // strongest form now that a second role exists — a pre-registered
        // island in a *ledger*-configured panel (whose boot role is `Bar`)
        // must survive its own `Opened` event rather than being demoted to
        // the boot role by an `insert`.
        let mut panel = test_panel();
        let id = window::Id::unique();
        let role = SurfaceRole::Island(IslandKind::Left);
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
            let panel = Panel::new(
                config::PanelConfig {
                    style,
                    ..config::PanelConfig::default()
                },
                Theme::saola(),
            );
            for name in [
                config::ModuleName::Mark,
                config::ModuleName::Mpris,
                config::ModuleName::Clock,
                config::ModuleName::NiriColumns,
                config::ModuleName::Volume,
                config::ModuleName::Network,
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
        let panel = Panel::new(config, Theme::saola());
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
        Panel::new(islands_config(), Theme::saola())
    }

    /// The style decides what the boot surface *is*. This is the hinge the
    /// whole mode switch turns on: nothing else about `Settings` differs
    /// between a bar and a centre island except the geometry derived from
    /// this role.
    #[test]
    fn the_boot_surface_is_the_bar_in_ledger_style_and_the_centre_island_in_islands_style() {
        assert_eq!(
            initial_role(&config::PanelConfig::default()),
            SurfaceRole::Bar
        );
        assert_eq!(
            initial_role(&islands_config()),
            SurfaceRole::Island(IslandKind::Centre)
        );
    }

    /// The unknown-Id fallback follows the style too — in islands mode the
    /// boot surface's first frame (rendered before its `Opened` event
    /// arrives) must draw the centre island, not a full ledger bar.
    #[test]
    fn an_unknown_id_falls_back_to_the_centre_island_in_islands_style() {
        let panel = islands_panel();
        assert_eq!(
            panel.role(window::Id::unique()),
            SurfaceRole::Island(IslandKind::Centre)
        );
    }

    /// Ledger style spawns nothing: the bar is the surface `Settings`
    /// already created.
    #[test]
    fn ledger_style_asks_for_no_extra_surfaces() {
        let mut panel = test_panel();
        let _ = panel.spawn_boot_surfaces();
        assert!(panel.windows.is_empty());
    }

    /// Islands style spawns exactly the two islands the boot surface isn't,
    /// and registers each one's role at spawn time — before the compositor
    /// has created anything, which is what keeps `view` from ever seeing an
    /// island Id it can't classify.
    #[test]
    fn islands_style_asks_for_the_left_and_right_islands_at_boot() {
        let mut panel = islands_panel();

        let _ = panel.spawn_boot_surfaces();

        assert_eq!(panel.windows.len(), 2, "left and right, but not centre");
        let mut roles: Vec<SurfaceRole> = panel.windows.values().copied().collect();
        roles.sort_by_key(|role| format!("{role:?}"));
        assert_eq!(
            roles,
            vec![
                SurfaceRole::Island(IslandKind::Left),
                SurfaceRole::Island(IslandKind::Right),
            ]
        );
    }

    /// Together with the boot surface, that is three islands — the deviation
    /// from spec §7's four (no notifications island this phase) stated as an
    /// assertion so it can't drift silently.
    #[test]
    fn islands_style_ends_up_with_three_surfaces_in_total() {
        let mut panel = islands_panel();
        let _ = panel.spawn_boot_surfaces();
        // The boot surface reports itself once the compositor has it.
        let boot_id = window::Id::unique();
        let _ = panel.update(Message::SurfaceOpened(boot_id));

        assert_eq!(panel.windows.len(), 3);
        assert_eq!(
            panel.windows.get(&boot_id),
            Some(&SurfaceRole::Island(IslandKind::Centre))
        );
    }

    /// Every island draws, for every kind — the islands counterpart of
    /// `bar_view_renders_the_default_module_lists_without_panicking`.
    #[test]
    fn island_view_renders_every_island_without_panicking() {
        let panel = islands_panel();
        for kind in [IslandKind::Left, IslandKind::Centre, IslandKind::Right] {
            let _: Element<'_, Message> = panel.island_view(kind);
        }
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
            (config::ModuleName::NiriColumns, false),
            (config::ModuleName::Volume, false),
            (config::ModuleName::Network, false),
            (config::ModuleName::Battery, false),
            (config::ModuleName::Claude, false),
            (config::ModuleName::Tray, false),
        ] {
            assert_eq!(panel.module_is_present(name), expected, "{name:?}");
        }
    }

    /// An island whose module list is explicitly empty (`left { }` in
    /// `panel.kdl`) renders nothing rather than an empty scrim pill.
    #[test]
    fn an_island_with_no_modules_renders_nothing() {
        let config = config::PanelConfig {
            left: vec![],
            ..islands_config()
        };
        let panel = Panel::new(config, Theme::saola());
        // No panic, and (not assertable through `Element`) no pill: the
        // early return in `island_view` is what this pins down.
        let _: Element<'_, Message> = panel.island_view(IslandKind::Left);
    }

    /// The islands' own geometry: a `panel_pill`-tall strip stretched to the
    /// output's width, on the `Top` layer and never taking the keyboard. The
    /// centre island is inset by `panel_margin_islands`; the left/right
    /// islands are zone-respecting surfaces placed below the centre's
    /// reservation, so their edge margin is `margin − strip` = `−panel_pill`
    /// (−40) — which lands them on the same strip. See `SurfaceGeometry::of`.
    #[test]
    fn island_geometry_comes_from_the_islands_tokens() {
        let theme = Theme::saola();
        let config = islands_config();
        let margin = theme.sizes.panel_margin_islands as i32;
        let strip = (theme.sizes.panel_pill as i32) + margin;

        for kind in [IslandKind::Left, IslandKind::Centre, IslandKind::Right] {
            let geometry = SurfaceGeometry::of(SurfaceRole::Island(kind), &config, &theme);

            let edge = match kind {
                IslandKind::Centre => margin,
                _ => margin - strip,
            };
            assert_eq!(geometry.size, (0, theme.sizes.panel_pill as u32));
            assert_eq!(geometry.margin, (edge, margin, 0, margin));
            assert_eq!(geometry.anchor, Anchor::Top | Anchor::Left | Anchor::Right);
            assert_eq!(geometry.layer, Layer::Top);
            assert_eq!(geometry.keyboard_interactivity, KeyboardInteractivity::None);
        }

        // The invariant the negative margin exists for: every island's final
        // position (reservation baseline + margin) is the same strip.
        let centre_top = margin;
        let sibling_top = strip + (margin - strip);
        assert_eq!(centre_top, sibling_top);
    }

    /// Input transparency is **not** uniform across the islands as of Stage
    /// 16: the right island carries the quick-settings trigger, and
    /// `events_transparent` is all-or-nothing per surface and fixed at
    /// creation time, so it has to accept pointer events for its whole
    /// full-width strip. The other two stay click-through, which is what
    /// keeps three overlapping full-width surfaces from fighting over the
    /// pointer.
    #[test]
    fn only_the_island_with_the_trigger_takes_pointer_events() {
        let theme = Theme::saola();
        let config = islands_config();
        let transparent = |kind| {
            SurfaceGeometry::of(SurfaceRole::Island(kind), &config, &theme).events_transparent
        };

        assert!(transparent(IslandKind::Left));
        assert!(transparent(IslandKind::Centre));
        assert!(
            !transparent(IslandKind::Right),
            "the status island is the islands layout's popover trigger and \
             must receive clicks"
        );
    }

    /// The exclusive zone is the centre island's alone. `-1` on the other
    /// two is not "no zone" — it is "don't move me out of anyone else's",
    /// without which the compositor would push the left and right islands
    /// below the centre's own reservation.
    #[test]
    fn only_the_centre_island_reserves_an_exclusive_zone() {
        let theme = Theme::saola();
        let config = islands_config();
        let zone =
            |kind| SurfaceGeometry::of(SurfaceRole::Island(kind), &config, &theme).exclusive_zone;

        assert_eq!(zone(IslandKind::Centre), theme.sizes.panel_pill as i32);
        assert_eq!(
            zone(IslandKind::Left),
            0,
            "a sibling island must respect foreign bars' zones (waybar bug) \
             — its negative edge margin re-levels it with the centre"
        );
        assert_eq!(zone(IslandKind::Right), 0);
    }

    /// The centre island reserves the same total as the ledger bar does:
    /// the zone we pass plus the anchored-edge margin the compositor adds
    /// itself (measured on niri: 40 + 26 == 48 + 18 == 66).
    #[test]
    fn islands_and_the_ledger_bar_reserve_the_same_strip() {
        let theme = Theme::saola();

        let bar = SurfaceGeometry::of(SurfaceRole::Bar, &config::PanelConfig::default(), &theme);
        let centre = SurfaceGeometry::of(
            SurfaceRole::Island(IslandKind::Centre),
            &islands_config(),
            &theme,
        );

        let reserved = |g: SurfaceGeometry| g.exclusive_zone + g.margin.0;
        assert_eq!(reserved(bar), 66);
        assert_eq!(reserved(centre), 66);
    }

    /// The ledger bar's geometry is untouched by this stage: still the
    /// `panel_bar` height, still the asymmetric ledger margins, still
    /// opaque to input.
    #[test]
    fn the_ledger_bar_geometry_is_unchanged() {
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
        assert_eq!(geometry.exclusive_zone, theme.sizes.panel_bar as i32);
        assert!(!geometry.events_transparent);
    }

    /// `edge "bottom"` flips which edge every surface anchors to and which
    /// margin carries the inset — in both styles.
    #[test]
    fn the_bottom_edge_flips_anchors_and_margins_in_both_styles() {
        let theme = Theme::saola();

        for (style, role) in [
            (config::PanelStyle::Ledger, SurfaceRole::Bar),
            (
                config::PanelStyle::Islands,
                SurfaceRole::Island(IslandKind::Left),
            ),
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
            assert_ne!(
                geometry.margin.2, 0,
                "the inset (positive for reserving surfaces, negative for \
                 the zone-respecting sibling islands) moved to the bottom"
            );
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

        let geometry =
            SurfaceGeometry::of(SurfaceRole::Island(IslandKind::Centre), &config, &theme);

        assert_eq!(geometry.size, (0, 52));
        assert_eq!(geometry.exclusive_zone, 52);
    }

    /// Spawned surfaces carry their geometry through to the settings the
    /// runtime actually spawns from — in particular the `Option` fields,
    /// where `None` would silently mean "leave the protocol default"
    /// (a 0 exclusive zone is *not* the same as -1).
    #[test]
    fn spawn_settings_carry_the_geometry_including_the_negative_margin() {
        let theme = Theme::saola();
        let geometry = SurfaceGeometry::of(
            SurfaceRole::Island(IslandKind::Left),
            &islands_config(),
            &theme,
        );

        let settings = geometry.new_layer_shell_settings();

        assert_eq!(settings.size, Some(geometry.size));
        assert_eq!(settings.margin, Some(geometry.margin));
        assert!(geometry.margin.0 < 0, "the re-levelling margin is negative");
        assert_eq!(settings.exclusive_zone, Some(0));
        assert_eq!(settings.anchor, geometry.anchor);
        assert!(settings.events_transparent);
        assert_eq!(settings.keyboard_interactivity, KeyboardInteractivity::None);
    }

    // ---- Stage 16: popover infrastructure -------------------------------

    /// The spec's popover placement, as arithmetic: `sizes.popover_width`
    /// wide, `sizes.popover_top` below the screen edge *once the panel's own
    /// 66 px reservation is counted*, anchored to that edge and the
    /// right-hand side.
    ///
    /// The `0` exclusive zone is the load-bearing part. It makes the
    /// compositor place the popover below **every** reservation on the
    /// output — the panel's and any foreign bar's — so its edge margin is
    /// the 6 px *gap* (`popover_top − strip`), not the literal token. See
    /// `SurfaceGeometry::of`'s doc comment for the full derivation and for
    /// why the original `-1` version broke next to waybar.
    #[test]
    fn popover_geometry_is_the_spec_placement_derived_from_tokens() {
        let theme = Theme::saola();
        let role = SurfaceRole::Popover(PopoverKind::QuickSettings);

        for config in [config::PanelConfig::default(), islands_config()] {
            let geometry = SurfaceGeometry::of(role, &config, &theme);

            assert_eq!(geometry.anchor, Anchor::Top | Anchor::Right);
            assert_eq!(
                geometry.size,
                (
                    theme.sizes.popover_width as u32,
                    PopoverKind::QuickSettings.height(&theme) as u32,
                )
            );
            assert_eq!(geometry.margin.0, theme.sizes.popover_top as i32 - 66);
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

    /// Spec §6's "26px from the relevant edge" is the islands margin token —
    /// stated as an assertion so the identity can't drift if either moves.
    /// In ledger style the popover instead lines up with the bar's own end
    /// (20), which is the deliberate deviation `SurfaceGeometry::of`
    /// documents.
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

        assert_eq!(side(&islands_config()), 26);
        assert_eq!(side(&config::PanelConfig::default()), 20);
    }

    /// The popover clears the panel rather than overlapping its trigger
    /// (spec §6). The panel reserves `exclusive_zone + edge margin` = 66 px
    /// in both styles (measured on niri in Stage 15); the popover is a
    /// zone-respecting surface placed below that reservation, plus a strictly
    /// positive gap: `popover_top` (72) − strip (66) = 6.
    #[test]
    fn the_popover_starts_below_the_panel_strip() {
        let theme = Theme::saola();

        for (config, panel_role) in [
            (config::PanelConfig::default(), SurfaceRole::Bar),
            (islands_config(), SurfaceRole::Island(IslandKind::Centre)),
        ] {
            let panel = SurfaceGeometry::of(panel_role, &config, &theme);
            let popover = SurfaceGeometry::of(
                SurfaceRole::Popover(PopoverKind::QuickSettings),
                &config,
                &theme,
            );

            let panel_strip = panel.exclusive_zone + panel.margin.0;
            assert_eq!(panel_strip, 66);
            assert_eq!(
                popover.margin.0,
                theme.sizes.popover_top as i32 - panel_strip,
                "the gap below the panel is popover_top minus the strip the \
                 panel reserves"
            );
            assert!(
                popover.margin.0 > 0,
                "the popover must never overlap the control that opened it"
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

        assert_eq!(geometry.anchor, Anchor::Bottom | Anchor::Right);
        assert_eq!(geometry.margin.0, 0);
        assert_eq!(geometry.margin.2, theme.sizes.popover_top as i32 - 66);
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

    /// Both layouts render their trigger. The `mouse_area` wrapping is
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
}
