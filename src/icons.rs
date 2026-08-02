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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Volume2,
    Volume1,
    /// The wave-less speaker — the bottom rung of the volume ladder (audible
    /// but effectively silent), below `Volume1`'s single wave.
    Volume,
    VolumeX,
    Play,
    Pause,
    SkipBack,
    SkipForward,
    /// Full-strength Wi-Fi (all arcs) — the top of the strength ladder the
    /// icon-only bar readout climbs; `WifiHigh`/`WifiLow`/`WifiZero` are the
    /// rungs below it, `WifiOff` the disconnected state.
    Wifi,
    WifiHigh,
    WifiLow,
    WifiZero,
    WifiOff,
    /// The empty battery outline — the bottom of the charge ladder;
    /// `BatteryLow`/`BatteryMedium`/`BatteryFull` add fill bars,
    /// `BatteryCharging` is the bolt shown while on AC.
    Battery,
    BatteryLow,
    BatteryMedium,
    BatteryFull,
    BatteryCharging,
    /// The brightness ladder for the quick-settings slider: `SunDim` →
    /// `SunMedium` → `Sun`, dimmest to brightest.
    Sun,
    SunMedium,
    SunDim,
    /// Bluetooth states: the bare rune (powered, idle), `Connected` (a
    /// device is attached), `Off` (adapter soft-off).
    Bluetooth,
    BluetoothConnected,
    BluetoothOff,
    /// The Claude Code module's default bar glyph: the Anthropic "A" mark.
    /// One of the two deliberate brand assets in an otherwise all-Lucide
    /// set (Jordan's call, 2026-08-01 — it replaced the Lucide `sparkle`
    /// stand-in): the module reports on a specific product, so its glyph is
    /// that product's mark. Solid-filled like `Play` (a wordmark letterform
    /// has no stroke to bake a width into), and tinted at view time like
    /// every other icon — the asset carries no brand color. Sits at the
    /// head of the session-dot row and doubles as the usage-popover
    /// trigger's icon.
    Anthropic,
    /// The Claude Code module's alternative bar glyph: Claude Code's own
    /// terminal-window mark, chosen by `claude-icon "claude-code"` in
    /// `panel.kdl` (see `crate::config::ClaudeIcon`). The second brand
    /// asset, same treatment as [`Anthropic`][Icon::Anthropic] above:
    /// solid-filled, no baked-in brand color, theme-tinted at view time.
    ClaudeCode,
    /// The default Saola mark: two splaying strokes ("horns"), style guide §8.
    MarkHorns,
    /// The alternative mark: a broken ring with a dot at the break, style
    /// guide §8. Chosen by `mark "builtin:notch"` in `panel.kdl`: the config
    /// stage delivered on Stage 8's promise, so `crate::config` parses that
    /// string into `MarkSource::BuiltinNotch` and `mark.rs` renders this
    /// variant for it — `MarkHorns` above stays the default when the setting
    /// is absent.
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
            Icon::Volume => include_bytes!("../assets/icons/volume.svg"),
            Icon::VolumeX => include_bytes!("../assets/icons/volume-x.svg"),
            Icon::Play => include_bytes!("../assets/icons/play.svg"),
            Icon::Pause => include_bytes!("../assets/icons/pause.svg"),
            Icon::SkipBack => include_bytes!("../assets/icons/skip-back.svg"),
            Icon::SkipForward => include_bytes!("../assets/icons/skip-forward.svg"),
            Icon::Wifi => include_bytes!("../assets/icons/wifi.svg"),
            Icon::WifiHigh => include_bytes!("../assets/icons/wifi-high.svg"),
            Icon::WifiLow => include_bytes!("../assets/icons/wifi-low.svg"),
            Icon::WifiZero => include_bytes!("../assets/icons/wifi-zero.svg"),
            Icon::WifiOff => include_bytes!("../assets/icons/wifi-off.svg"),
            Icon::Battery => include_bytes!("../assets/icons/battery.svg"),
            Icon::BatteryLow => include_bytes!("../assets/icons/battery-low.svg"),
            Icon::BatteryMedium => include_bytes!("../assets/icons/battery-medium.svg"),
            Icon::BatteryFull => include_bytes!("../assets/icons/battery-full.svg"),
            Icon::BatteryCharging => include_bytes!("../assets/icons/battery-charging.svg"),
            Icon::Sun => include_bytes!("../assets/icons/sun.svg"),
            Icon::SunMedium => include_bytes!("../assets/icons/sun-medium.svg"),
            Icon::SunDim => include_bytes!("../assets/icons/sun-dim.svg"),
            Icon::Bluetooth => include_bytes!("../assets/icons/bluetooth.svg"),
            Icon::BluetoothConnected => include_bytes!("../assets/icons/bluetooth-connected.svg"),
            Icon::BluetoothOff => include_bytes!("../assets/icons/bluetooth-off.svg"),
            Icon::Anthropic => include_bytes!("../assets/icons/anthropic.svg"),
            Icon::ClaudeCode => include_bytes!("../assets/icons/claude-code.svg"),
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
    const ALL: [Icon; 28] = [
        Icon::Volume2,
        Icon::Volume1,
        Icon::Volume,
        Icon::VolumeX,
        Icon::Play,
        Icon::Pause,
        Icon::SkipBack,
        Icon::SkipForward,
        Icon::Wifi,
        Icon::WifiHigh,
        Icon::WifiLow,
        Icon::WifiZero,
        Icon::WifiOff,
        Icon::Battery,
        Icon::BatteryLow,
        Icon::BatteryMedium,
        Icon::BatteryFull,
        Icon::BatteryCharging,
        Icon::Sun,
        Icon::SunMedium,
        Icon::SunDim,
        Icon::Bluetooth,
        Icon::BluetoothConnected,
        Icon::BluetoothOff,
        Icon::Anthropic,
        Icon::ClaudeCode,
        Icon::MarkHorns,
        Icon::MarkNotch,
    ];

    /// Every embedded asset that still uses a stroke at all — i.e. `ALL`
    /// minus the [`SOLID`] pair. Style guide §4: "No filled icons except
    /// play/next/record/stop, which are solid." Stage 9 (the media module)
    /// resolved the fill-style flag Stage 8 left open by solidifying `Play`:
    /// a filled triangle has no stroke to speak of, so it's exempted from
    /// the stroke-width invariant below rather than pretending it still has
    /// one. `SkipForward` ("next") also went solid, but its Lucide glyph is
    /// a filled triangle *plus* a genuinely unfillable straight-line bar —
    /// that bar keeps a real `stroke-width="2.75"` stroke, so the asset as
    /// a whole still satisfies the invariant and stays in this list
    /// unmodified. See `modules::media`'s module doc comment and the Stage
    /// 9 handoff for the full fill-style decision. `Anthropic` and
    /// `ClaudeCode` (the brand-mark exemptions, 2026-08-01) are solid by
    /// nature — see their variant doc comments.
    const STROKE_ONLY: [Icon; 25] = [
        Icon::Volume2,
        Icon::Volume1,
        Icon::Volume,
        Icon::VolumeX,
        Icon::Pause,
        Icon::SkipBack,
        Icon::SkipForward,
        Icon::Wifi,
        Icon::WifiHigh,
        Icon::WifiLow,
        Icon::WifiZero,
        Icon::WifiOff,
        Icon::Battery,
        Icon::BatteryLow,
        Icon::BatteryMedium,
        Icon::BatteryFull,
        Icon::BatteryCharging,
        Icon::Sun,
        Icon::SunMedium,
        Icon::SunDim,
        Icon::Bluetooth,
        Icon::BluetoothConnected,
        Icon::BluetoothOff,
        Icon::MarkHorns,
        Icon::MarkNotch,
    ];

    /// The solid-filled assets — the ones exempted from the stroke-width
    /// invariant, each for its own reason spelled out in `STROKE_ONLY`'s
    /// doc comment.
    const SOLID: [Icon; 3] = [Icon::Play, Icon::Anthropic, Icon::ClaudeCode];

    /// Binding constraint (CLAUDE.md, PLAN.md Stage 8): every embedded
    /// *stroke-based* asset must have `stroke-width="2.75"` baked in at
    /// authoring time — it has to match `saola_tokens::Sizes::icon_stroke`
    /// (2.75) at every size the icon is ever drawn at, and nothing at the
    /// call site enforces that, so this test is what actually guards the
    /// invariant. The [`SOLID`] assets have no stroke at all and are
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

    /// The assets exempted above: confirms each is genuinely solid (a
    /// filled shape, no stroke at all) rather than just having drifted off
    /// 2.75 by accident.
    #[test]
    fn solid_icons_are_filled_with_no_stroke() {
        for icon in SOLID {
            let source = std::str::from_utf8(icon.bytes()).unwrap();
            assert!(
                source.contains("fill=\"currentColor\""),
                "{icon:?}'s asset should be filled solid"
            );
            assert!(
                !source.contains("stroke-width"),
                "{icon:?}'s asset should have no stroke at all"
            );
        }
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
