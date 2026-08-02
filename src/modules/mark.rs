//! The Saola mark: the horns glyph (style guide §8), rendered bare — no pill
//! behind it, ivory at rest — as the first occupant of the bar's left
//! region (previously an empty `Space::new()` placeholder in `main.rs`).
//!
//! Teaching note (a module with no signal at all): every other module so
//! far (`clock`, `battery`, `network`) has *something* driving it — a
//! timer, a D-Bus property stream. The mark has neither: it's a static
//! glyph that never changes at runtime, so it has no state to cache and
//! nothing to subscribe to. The five-part module pattern (state struct,
//! `Message`, `view`, `subscription`, `main.rs` wiring) still applies, it
//! just degrades gracefully at every step: the state struct is a
//! zero-field unit struct (same shape as `Clock`, which also caches
//! nothing between renders — see `clock::Clock`'s doc comment), `Message`
//! is an *empty* enum (there is no event this module could ever produce —
//! not even a single unit variant like `Clock::Tick`, because nothing ever
//! drives one), and `subscription()` returns `Subscription::none()`. Wiring
//! it into `main.rs` still follows the exact same shape as every other
//! module (`Message::Mark(modules::mark::Message)`, `.map(Message::Mark)` at
//! both the subscription and view composition sites) even though, for this
//! particular module, the subscription side of that wiring will never
//! actually produce a value — keeping the shape identical is what makes the
//! *next* module (one that starts static and later grows a real message)
//! a pattern-copy instead of a re-plumb.
//!
//! # The mark becomes clickable
//!
//! This turned out to be that next module. `Message` is no longer empty:
//! [`Message::Pressed`] fires when the glyph is clicked, and `Mark` now
//! carries a second field (`launcher: Option<String>`, resolved from
//! `panel.kdl`'s `launcher` directive — see `crate::config::read_launcher`)
//! alongside `source`. The doc comment above is left in place rather than
//! rewritten away: it is the accurate history of *why* the empty-enum
//! wiring shape existed in the first place, and this section is the
//! predicted "next module" arriving.
//!
//! `view` only builds an `iced::widget::button` around the glyph when
//! `launcher` is `Some(..)` (`launcher "none"` in `panel.kdl`, or a
//! genuinely absent config, resolve to `None` — see `PanelConfig::launcher`'s
//! doc comment for why absence itself still defaults to `Some("fuzzel")`,
//! so `None` only ever means an explicit opt-out). This isn't just
//! tidiness: an iced 0.14 `button` with no `.on_press` renders as
//! `Status::Disabled` (there is no third "just inert, please" status), so
//! building one in the `None` case would visibly dim the glyph for no
//! reason. Keeping the plain, button-free glyph for that case is what
//! makes "no launcher configured" look identical to "the mark isn't
//! clickable at all" pre-this-change, rather than "a broken-looking button
//! that doesn't do anything." Either way the rendered glyph is pixel- and
//! color-identical: `style::button::bare` (see that helper's doc comment)
//! draws no background at `Status::Active`, so wrapping the bare svg in a
//! button changes nothing about the mark's footprint at rest — the same
//! trick `modules::volume`'s mute toggle already relies on.
//!
//! `main.rs`'s `Message::Mark(mark::Message::Pressed)` arm is where the
//! actual process spawn happens (`std::process::Command`, first
//! whitespace-split token as the program, the rest as arguments) — and
//! where the click acts as a *toggle*: if the launcher spawned by the last
//! click is still running, a second click kills it instead of stacking
//! another copy. All of that is kept out of this module because `mark.rs`
//! has no business owning a child process; see that arm's own comment for
//! the zombie-process teaching note.
//!
//! Cross-repo flag (recorded, not acted on — see PLAN.md Stage 8 and the
//! Stage 8 handoff): this icon, like the rest of `src/icons.rs`, is embedded
//! panel-locally for now. The launcher, greeter, and lock screen will all
//! need the same mark asset eventually, at which point it (and the other
//! Lucide assets) belong in `saola-theme` or a dedicated `saola-icons`
//! crate — moving them there later is a mechanical import swap, not a
//! redesign, so this stage doesn't attempt it speculatively.
//!
//! # Stage 14: the mark becomes configurable
//!
//! `mark "builtin:horns" | "builtin:notch" | "file:…" | "none"` in
//! `panel.kdl` (see `crate::config::MarkSource`) picks which glyph this
//! module draws — the state struct went from a zero-field unit struct to
//! one field (`source`) to hold that choice. `BuiltinHorns`/`BuiltinNotch`
//! still route through `crate::icons`' embedded-asset machinery exactly as
//! before; `File` is the first thing in this crate to build an
//! `iced::widget::svg::Handle` from a *runtime* path
//! (`Handle::from_path`) rather than `include_bytes!` — see `view`'s doc
//! comment for why that means going around `icons::icon` for this one case
//! (that helper only knows about compile-time-embedded assets, and a
//! user-supplied path is the opposite of that by definition) while still
//! reusing the exact same tinting trick (`svg::Style { color }`).

use iced::widget::{button, container, svg, Space};
use iced::{Center, Element, Fill, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};

use crate::config::{MarkSource, PanelStyle, DEFAULT_LAUNCHER};
use crate::icons::{self, Icon};

/// The mark module's own message type (per-module `Message` pattern — see
/// `clock::Message`'s doc comment for the full rationale). One variant,
/// [`Message::Pressed`], fired by `view`'s button when a `launcher` is
/// configured (see the module doc comment's "The mark becomes clickable"
/// section) — `main.rs`'s `Panel::update` is where that press actually
/// spawns a process; this module only ever reports that the click
/// happened.
#[derive(Debug, Clone)]
pub enum Message {
    /// The glyph was clicked. Only ever produced when `Mark::launcher` is
    /// `Some(..)` — `view` doesn't build a button at all otherwise, so
    /// there is no widget left to click that could send this.
    Pressed,
}

/// Mark module state: which glyph (if any) to draw, resolved at
/// construction from `panel.kdl`'s `mark` directive
/// (`crate::config::PanelConfig::mark`), plus which command (if any) a
/// click on it should spawn, resolved from the `launcher` directive
/// (`crate::config::PanelConfig::launcher`) — neither changes for the life
/// of the instance (there is still no signal source of any kind — a
/// config-driven choice of *what* to render, or *what to run on click*,
/// doesn't change that the mark itself has no ongoing state to track
/// between renders). A live config reload simply constructs a fresh
/// `Mark`: with nothing here but config, rebuilding *is* the reload (see
/// `main.rs`'s `reload_config`).
pub struct Mark {
    source: MarkSource,
    /// `Some(command)` — the raw command line to hand to
    /// `std::process::Command` in `main.rs` (whitespace-split there, not
    /// here: parsing it early would just mean threading a `Vec<String>`
    /// through this struct for no benefit, since nothing else ever reads
    /// it). `None` means `launcher "none"` — the glyph renders exactly as
    /// it did before this module could be clicked at all.
    launcher: Option<String>,
}

impl Default for Mark {
    /// The pre-Stage-14 hardcoded behavior, with today's default launcher
    /// (`fuzzel`) layered on top: the built-in horns mark, clickable.
    fn default() -> Self {
        Self::new(MarkSource::default(), Some(DEFAULT_LAUNCHER.to_string()))
    }
}

impl Mark {
    pub fn new(source: MarkSource, launcher: Option<String>) -> Self {
        Self { source, launcher }
    }

    /// Renders the configured mark bare: no pill, no background treatment
    /// at all (style guide §6, "bare-icon menu" — icons directly on the
    /// surface), tinted ivory (`on_ink.primary`, full emphasis) since the
    /// mark itself has no on/off or live state to distinguish with
    /// terracotta — the one rule only applies to controls that *have* an
    /// active state.
    ///
    /// Sized at `sizes.icon_bar` (15px) — CLAUDE.md's "zero hardcoded
    /// sizes" rule applies here exactly as it does to colors.
    ///
    /// `MarkSource::File` is the one branch that doesn't go through
    /// `icons::icon`: that helper's `Icon` enum is a closed set of
    /// `include_bytes!`-embedded assets resolved at *compile* time, and a
    /// user's `file:~/.icons/arch.svg` path is only known at *runtime* (it
    /// isn't even guaranteed to exist). `svg::Handle::from_path` is the
    /// direct iced equivalent of `Handle::from_memory` for that case; the
    /// same `.style(|_, _| svg::Style { color: Some(color) })` tinting
    /// trick `icons::icon` uses applies unchanged (see that module's doc
    /// comment for the exact rasterizer mechanics), so a file-based mark
    /// still respects the theme's ivory tint rather than whatever color the
    /// SVG file itself happens to specify. A path that doesn't resolve to a
    /// valid SVG is a render-time decode failure inside resvg, not
    /// something this module can validate ahead of time.
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. `mark "none"` in `panel.kdl` is the one absent case; every
    /// other source always draws.
    pub fn is_present(&self) -> bool {
        !matches!(self.source, MarkSource::None)
    }

    /// The configured launcher command, if any — `main.rs`'s
    /// `Message::Mark(Message::Pressed)` arm reads this to know what to
    /// spawn. `Some` exactly when `view` built a clickable button, `None`
    /// exactly when `launcher "none"` was configured (see `Mark::launcher`'s
    /// field doc comment) — the two always agree, since both read this same
    /// field.
    pub fn launcher(&self) -> Option<&str> {
        self.launcher.as_deref()
    }

    /// Like `clock::Clock::view`, this takes the panel style because the
    /// mark's *surface treatment* legitimately differs per style (the
    /// amended layout seam — see `Panel::island_view`'s doc comment): on
    /// the ledger bar the button *is* the visible affordance (a
    /// pill-shaped hit area revealed on hover), while on islands the
    /// module already sits inside a solid ink circle drawn by
    /// `Panel::island_pill`, so the button instead fills that circle —
    /// making the whole circle the launcher's hit target and letting the
    /// hover tint wash the circle itself rather than drawing a second,
    /// wider pill inside (or overflowing) it.
    pub fn view(&self, theme: &Theme, style: PanelStyle) -> Element<'_, Message> {
        let color = theme.on_ink.primary.into_iced();
        let size = theme.sizes.icon_bar;
        let glyph: Element<'_, Message> = match &self.source {
            MarkSource::BuiltinHorns => icons::icon(Icon::MarkHorns, size, color).into(),
            MarkSource::BuiltinNotch => icons::icon(Icon::MarkNotch, size, color).into(),
            MarkSource::File(path) => svg(svg::Handle::from_path(path))
                .width(size)
                .height(size)
                .style(move |_theme, _status| svg::Style { color: Some(color) })
                .into(),
            // `mark "none"`: the left region simply has one fewer widget.
            // An early return of a zero-sized `Space` (rather than falling
            // through to the button wrapping below) matters more now that
            // the button has a real footprint: wrapping "nothing" in a
            // pill-sized hit area would leave an invisible 32px-tall
            // clickable region on the bar with no glyph in it.
            MarkSource::None => return Space::new().width(0).height(0).into(),
        };

        // Only wrap the glyph in a button when a launcher is actually
        // configured. This isn't just an optimization: an iced 0.14
        // `button` with no `.on_press` renders as `Status::Disabled` (see
        // the module doc comment's "The mark becomes clickable" section),
        // so building one here for the `None` case would visibly dim the
        // glyph even though there is nothing for it to do. Skipping the
        // button entirely keeps `launcher "none"` pixel-identical to the
        // mark's pre-clickable rendering.
        //
        // Both styles use `style::button::bare`, which keeps the button
        // *visually* identical to the unwrapped glyph at rest ("bare"'s
        // `Status::Active` arm draws no background at all); only
        // hover/press reveal it, as a subtle fill. What differs is the
        // button's geometry (see `view`'s doc comment):
        //
        // - Ledger: the hit area is sized like the clock's ledger pill —
        //   same `panel_pill_clock` height, same half-height horizontal
        //   padding (see `clock::Clock::ledger_pill`) — because a bare
        //   15px glyph directly on the bar is a fiddly click target. The
        //   inner `container` + `Fill`/`Center` is how a fixed-size svg
        //   gets vertically centred inside the taller button (an svg,
        //   unlike `text`, has no `align_y` of its own).
        //
        // - Islands: `Fill` both ways, so the button stretches to
        //   whatever `Panel::island_pill` pinned the mark's ink circle
        //   at — the hover fill (at `radii.pill`, closing to the same
        //   circle) then paints exactly over the circle's own ink, and a
        //   click anywhere on the circle launches. Sizing here would be
        //   wrong twice over: the module can't know the circle's width
        //   (that's layout), and a fixed-width button wider than the
        //   circle would overflow it.
        match &self.launcher {
            Some(_) => {
                let bare = style::button::bare(theme, Surface::Ink);
                match style {
                    PanelStyle::Ledger => button(container(glyph).height(Fill).align_y(Center))
                        .height(theme.sizes.panel_pill_clock)
                        .padding([0.0, theme.sizes.panel_pill_clock / 2.0])
                        .style(bare)
                        .on_press(Message::Pressed)
                        .into(),
                    PanelStyle::Islands => button(container(glyph).center(Fill))
                        .padding(0)
                        .width(Fill)
                        .height(Fill)
                        .style(bare)
                        .on_press(Message::Pressed)
                        .into(),
                }
            }
            None => glyph,
        }
    }

    /// No signal source exists for this module — see the module doc comment
    /// — so this is `Subscription::none()`, not a worker of any kind.
    /// Included anyway (rather than main.rs simply omitting a `mark`
    /// subscription) so the module's public shape stays identical to every
    /// other module's, which is what lets `main.rs` compose it into
    /// `Subscription::batch` with the same `.map(Message::Mark)` call every
    /// other module gets.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }
}
