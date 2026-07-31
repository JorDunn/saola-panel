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

use iced::widget::{svg, Space};
use iced::{Element, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;

use crate::config::MarkSource;
use crate::icons::{self, Icon};

/// The mark module's own message type (per-module `Message` pattern — see
/// `clock::Message`'s doc comment for the full rationale). Empty: nothing
/// ever drives this module, so there is no event it could produce. An outer
/// `Message::Mark(modules::mark::Message)` variant still exists in
/// `main.rs` for wiring-shape consistency, but because this type is
/// uninhabited, a `Message::Mark(_)` value can never actually be
/// constructed at runtime — it always falls through `Panel::update`'s
/// catch-all arm, the same way `Message::Clock(clock::Message::Tick)` does
/// today (see that match arm's comment in `main.rs`).
#[derive(Debug, Clone)]
pub enum Message {}

/// Mark module state: which glyph (if any) to draw, resolved once at boot
/// from `panel.kdl`'s `mark` directive (`crate::config::PanelConfig::mark`)
/// and never changed afterward (there is still no signal source of any
/// kind — a config-driven choice of *what* to render statically doesn't
/// change that the mark itself is inert once chosen).
pub struct Mark {
    source: MarkSource,
}

impl Default for Mark {
    /// The pre-Stage-14 hardcoded behavior: the built-in horns mark.
    fn default() -> Self {
        Self::new(MarkSource::default())
    }
}

impl Mark {
    pub fn new(source: MarkSource) -> Self {
        Self { source }
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

    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        let color = theme.on_ink.primary.into_iced();
        let size = theme.sizes.icon_bar;
        match &self.source {
            MarkSource::BuiltinHorns => icons::icon(Icon::MarkHorns, size, color).into(),
            MarkSource::BuiltinNotch => icons::icon(Icon::MarkNotch, size, color).into(),
            MarkSource::File(path) => svg(svg::Handle::from_path(path))
                .width(size)
                .height(size)
                .style(move |_theme, _status| svg::Style { color: Some(color) })
                .into(),
            // `mark "none"`: the left region simply has one fewer widget.
            // A zero-sized `Space` rather than omitting the element
            // entirely keeps `view`'s return type uniform across branches
            // with no visible effect (it occupies no layout space).
            MarkSource::None => Space::new().width(0).height(0).into(),
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
