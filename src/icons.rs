//! SVG icon infrastructure: Lucide icons at stroke-width 2.75
//! (`saola_theme::tokens::Sizes::icon_stroke`), embedded at compile time and
//! tinted at *view* time from theme colors.
//!
//! The style guide (§4 Geometry) requires every icon in the shell to be a
//! [Lucide](https://lucide.dev) icon at stroke-width 2.75, at every size —
//! never a size- or state-dependent stroke. Rather than trust every future
//! call site to remember that, the stroke width is baked into each `.svg`
//! asset once, at authoring time (see `assets/icons/*.svg`), and the test
//! module below asserts it stayed that way. It is the *one* sanctioned
//! literal in this file — everything else (size, color) comes from the
//! theme, per CLAUDE.md's "zero hardcoded colors or sizes" rule.
//!
//! Teaching note (`include_bytes!` and static handles): `include_bytes!`
//! runs at *compile* time — it reads the file at the given path (relative
//! to this source file) and embeds its bytes directly into the compiled
//! binary as a `&'static [u8]`. There is no filesystem access whatsoever at
//! runtime, and a missing or renamed asset is a compile error, not something
//! that can fail while the bar is running. Each `Icon` variant maps to
//! exactly one embedded byte string; [`Icon::handle`] wraps those bytes in
//! an [`iced::widget::svg::Handle`] (`Handle::from_memory`), which the
//! renderer parses and rasterizes lazily, the first time it's actually drawn.
//!
//! Teaching note (tinting mechanics — where the color actually comes from):
//! every asset's own `stroke="currentColor"` is cosmetic and never what
//! appears on screen. `iced::widget::svg::Style { color }` (set by
//! [`icon`]'s `.style()` closure below) is read by the `iced_tiny_skia`
//! renderer's vector cache, which — when a `color` is present — rewrites
//! *every non-transparent pixel* of the rasterized icon to that color,
//! ignoring whatever the source SVG drew (see
//! `iced_tiny_skia::vector::Cache::draw`, the `if let Some([r, g, b, _]) =
//! key.color` branch, for the exact mechanism). That recolor is also part of
//! the rasterization cache key (`RasterKey { id, color, size }`), so drawing
//! the same icon in two different tints (e.g. ivory at rest vs. terracotta
//! live) rasterizes and caches each tint independently rather than fighting
//! over one cached bitmap. Concretely: call sites never set a color inside
//! an SVG file — they always pass a `theme.on(Surface)....` role (or
//! `theme.palette.accent`), converted with `saola_theme::convert::ColorExt::
//! into_iced`, as this module's `color` argument.

use iced::widget::svg::{Handle, Style};
use iced::widget::{svg, Svg};
use iced::Color;

/// One embedded icon asset. Each variant is one 24×24 Lucide SVG (or, for
/// the two `Mark*` variants, the Saola mark from style guide §8) living
/// under `assets/icons/`.
///
/// Adding an icon: drop the `.svg` file in `assets/icons/` with
/// `stroke-width="2.75"` baked in, add a variant here, add its `bytes()`
/// arm, and add it to `tests::ALL` so the asset tests cover it automatically.
///
/// `#[allow(dead_code)]`: this enum embeds the whole asset set the panel's
/// stages need (per PLAN.md Stage 8), and the modules claim them one stage
/// at a time — media took `Play`/`Pause`/`SkipBack`/`SkipForward`, volume
/// the `Volume*` ladder, and the bare-status restyle took `Wifi` and
/// `Battery`. `ChevronDown` and `MarkNotch` are still waiting on the
/// popover and config stages. Without this attribute, `cargo clippy
/// -D warnings` would fail the build over icons that are correctly unused
/// *today* and wired up a stage later — the asset + test coverage is the
/// point of embedding them now, not premature use.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Volume2,
    Volume1,
    VolumeX,
    Play,
    Pause,
    SkipBack,
    SkipForward,
    Wifi,
    Battery,
    Music,
    ChevronDown,
    /// The default Saola mark: two splaying strokes ("horns"), style guide §8.
    MarkHorns,
    /// The alternative mark: a broken ring with a dot at the break, style
    /// guide §8. Not wired to any UI yet — `mark.rs` uses `MarkHorns` — kept
    /// here because the infrastructure (and the asset itself) is what this
    /// stage is building; a later config stage (KDL `mark` setting) is what
    /// will actually let something choose `MarkNotch`.
    MarkNotch,
}

impl Icon {
    /// The embedded SVG source bytes for this icon. `include_bytes!` reads
    /// the file at compile time (path relative to *this* source file) —
    /// see the module doc comment for why that means no runtime I/O at all.
    fn bytes(self) -> &'static [u8] {
        match self {
            Icon::Volume2 => include_bytes!("../assets/icons/volume-2.svg"),
            Icon::Volume1 => include_bytes!("../assets/icons/volume-1.svg"),
            Icon::VolumeX => include_bytes!("../assets/icons/volume-x.svg"),
            Icon::Play => include_bytes!("../assets/icons/play.svg"),
            Icon::Pause => include_bytes!("../assets/icons/pause.svg"),
            Icon::SkipBack => include_bytes!("../assets/icons/skip-back.svg"),
            Icon::SkipForward => include_bytes!("../assets/icons/skip-forward.svg"),
            Icon::Wifi => include_bytes!("../assets/icons/wifi.svg"),
            Icon::Battery => include_bytes!("../assets/icons/battery.svg"),
            Icon::Music => include_bytes!("../assets/icons/music.svg"),
            Icon::ChevronDown => include_bytes!("../assets/icons/chevron-down.svg"),
            Icon::MarkHorns => include_bytes!("../assets/icons/mark-horns.svg"),
            Icon::MarkNotch => include_bytes!("../assets/icons/mark-notch.svg"),
        }
    }

    /// An SVG handle for this icon.
    ///
    /// `Handle::from_memory` hashes the bytes it's given to build the
    /// handle's id (see `iced_core::svg::Handle::from_data`), and that id is
    /// what the renderer's rasterization cache keys on — so calling this
    /// repeatedly for the same variant (once per `view`, say) is cheap: the
    /// same static bytes hash the same way every time, and the renderer
    /// recognizes the cache hit instead of re-parsing the SVG each frame.
    fn handle(self) -> Handle {
        Handle::from_memory(self.bytes())
    }
}

/// Builds a sized, tinted [`Svg`] widget for `icon`.
///
/// `size` is a theme size token, in logical pixels — e.g.
/// `theme.sizes.icon_bar` (15.0) for the panel bar, `theme.sizes.icon_bare`
/// for a bare-icon menu — never a literal number (CLAUDE.md: zero hardcoded
/// sizes). `color` is likewise always a theme role — a `theme.on(Surface)
/// .primary`/`.secondary`/etc, or `theme.palette.accent` for the terracotta
/// "live" treatment — converted to `iced::Color` with `saola_theme::convert
/// ::ColorExt::into_iced` before it reaches this function.
///
/// See the module doc comment for exactly how `color` ends up as the pixels
/// on screen (it is not the SVG's own stroke color).
pub fn icon<'a>(kind: Icon, size: f32, color: Color) -> Svg<'a> {
    svg(kind.handle())
        .width(size)
        .height(size)
        .style(move |_theme, _status| Style { color: Some(color) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every embedded icon, so the tests below can walk the whole asset set
    /// without a second hand-maintained list of bytes.
    const ALL: [Icon; 13] = [
        Icon::Volume2,
        Icon::Volume1,
        Icon::VolumeX,
        Icon::Play,
        Icon::Pause,
        Icon::SkipBack,
        Icon::SkipForward,
        Icon::Wifi,
        Icon::Battery,
        Icon::Music,
        Icon::ChevronDown,
        Icon::MarkHorns,
        Icon::MarkNotch,
    ];

    /// Every embedded asset that still uses a stroke at all — i.e. `ALL`
    /// minus `Play`. Style guide §4: "No filled icons except play/next/
    /// record/stop, which are solid." Stage 9 (the media module) resolved
    /// the fill-style flag Stage 8 left open by solidifying `Play`: a
    /// filled triangle has no stroke to speak of, so it's exempted from the
    /// stroke-width invariant below rather than pretending it still has
    /// one. `SkipForward` ("next") also went solid, but its Lucide glyph is
    /// a filled triangle *plus* a genuinely unfillable straight-line bar —
    /// that bar keeps a real `stroke-width="2.75"` stroke, so the asset as
    /// a whole still satisfies the invariant and stays in this list
    /// unmodified. See `modules::media`'s module doc comment and the Stage
    /// 9 handoff for the full fill-style decision.
    const STROKE_ONLY: [Icon; 12] = [
        Icon::Volume2,
        Icon::Volume1,
        Icon::VolumeX,
        Icon::Pause,
        Icon::SkipBack,
        Icon::SkipForward,
        Icon::Wifi,
        Icon::Battery,
        Icon::Music,
        Icon::ChevronDown,
        Icon::MarkHorns,
        Icon::MarkNotch,
    ];

    /// Binding constraint (CLAUDE.md, PLAN.md Stage 8): every embedded
    /// *stroke-based* asset must have `stroke-width="2.75"` baked in at
    /// authoring time — it has to match `saola_tokens::Sizes::icon_stroke`
    /// (2.75) at every size the icon is ever drawn at, and nothing at the
    /// call site enforces that, so this test is what actually guards the
    /// invariant. `Play` is solid-filled (no stroke at all) and is
    /// deliberately not walked here — see `STROKE_ONLY`'s doc comment.
    #[test]
    fn every_asset_bakes_in_the_theme_stroke_width() {
        for icon in STROKE_ONLY {
            let source = std::str::from_utf8(icon.bytes())
                .unwrap_or_else(|_| panic!("{icon:?}'s asset is not valid UTF-8"));
            assert!(
                source.contains("stroke-width=\"2.75\""),
                "{icon:?}'s asset is missing stroke-width=\"2.75\""
            );
        }
    }

    /// The one asset exempted above: confirms `Play` is genuinely solid
    /// (a filled shape, no stroke at all) rather than just having drifted
    /// off 2.75 by accident.
    #[test]
    fn play_icon_is_solid_fill_with_no_stroke() {
        let source = std::str::from_utf8(Icon::Play.bytes()).unwrap();
        assert!(
            source.contains("fill=\"currentColor\""),
            "Play's asset should be filled solid per style guide §4"
        );
        assert!(
            !source.contains("stroke-width"),
            "Play's asset should have no stroke at all now that it's solid-filled"
        );
    }

    /// Cheap well-formedness check: catches a mismatched/truncated asset
    /// (e.g. a copy-paste mistake while authoring one of these files) at
    /// `cargo test` time instead of leaving it for a runtime SVG-parse
    /// failure inside `resvg`, which `include_bytes!` itself can't catch
    /// (it embeds bytes, it doesn't parse them).
    #[test]
    fn every_asset_is_a_24x24_svg() {
        for icon in ALL {
            let source = std::str::from_utf8(icon.bytes()).unwrap();
            assert!(
                source.contains("viewBox=\"0 0 24 24\""),
                "{icon:?}'s asset is not a 24x24 viewBox"
            );
        }
    }
}
