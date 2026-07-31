//! The quick-settings popover's content — [`crate::popover::PopoverKind::
//! QuickSettings`]'s real body, filling the empty ink shell Stage 16
//! (`crate::popover`) proved the open/close lifecycle with. Spec §6/§7: a
//! 2×2 toggle grid, a slider section, and a media row, top to bottom, inset
//! inside the popover's opaque ink (`saola_theme::style::container::
//! popover`).
//!
//! Rendered directly by `main.rs::Panel::popover_view` rather than through
//! `crate::popover::view` — this module needs to read `Volume`/`Media`
//! state that `popover.rs` deliberately never learns about (see that
//! module's doc comment for why keeping the lifecycle manager ignorant of
//! specific bar modules is the point, not an oversight).
//!
//! # Honest scope (PLAN.md Stage 17)
//!
//! The spec's canonical quick-settings toggles — Wi-Fi, Bluetooth, Do Not
//! Disturb — have no backend anywhere in this codebase: Bluetooth has no
//! module at all (not in this phase's plan), Do Not Disturb belongs to the
//! notifications component CLAUDE.md excludes outright, and a Wi-Fi power
//! toggle would cross the "network is status-display only" line PLAN.md
//! draws (Jordan's call on this stage: default no, not built). [`toggle_grid`]
//! therefore renders all four cells as **honest, permanently disabled**
//! placeholders: `style::button::muted` with no `on_press`, which iced
//! reports as `Status::Disabled` on its own — the muted style's disabled arm
//! is a visibly *dimmer* fill than its resting one, so a disabled cell reads
//! as disabled. That is the opposite convention from every bar-module pill
//! elsewhere in this crate, which pin `Status::Active` specifically so a
//! *real, always-on* readout doesn't look grayed out (see `battery.rs`'s
//! `view`) — these four cells have no backend at all, so looking inert is
//! the honest state, not a bug to work around.
//!
//! Two controls are real. [`volume_row`]'s slider and mute toggle drive
//! `crate::modules::volume`'s pulse worker through the command channel that
//! module's Stage 17 half adds (`volume::Message::SetVolume`/`ToggleMute`,
//! resolved by `Panel::update` via the `CommandSender` stashed from
//! `volume::Message::Ready` — see that module's doc comment for the full
//! two-way-channel story). [`media_row`]'s transport buttons are one-shot
//! `Task`-driven D-Bus calls — see `crate::modules::media`'s "command-out
//! pattern" section.
//!
//! # Why the mute toggle is allowed to be terracotta here
//!
//! `modules::volume`'s own doc comment is emphatic that terracotta never
//! appears on the bar's volume readout — mute there is a *quiet* state, not
//! a live one, per the one rule ("a level readout is not a control that is
//! switched on"). This module's mute control is a different widget class,
//! not a restyle of that readout: [`saola_theme::style::toggles::toggler`]
//! is Saola's generic switch, and the style guide states its own on/off
//! convention independently of any specific module (§6 "Toggle / switch:
//! ... on track = terracotta"). Reading "Mute: on" as a live, currently-in-
//! effect state — the generic switch's own semantics — rather than as the
//! bar's specific "muted is a quiet non-color" convention is what keeps
//! these two deliberately different readings of the same boolean from
//! actually contradicting each other. The bar's own mute button (see
//! `volume::Volume::view`) is untouched: still bare icon + text, still
//! never terracotta.
//!
//! # A flagged theme gap: no "inset tile" container
//!
//! The style guide describes the media row as "inset, tile-ish radius" — a
//! recessed panel distinct from the popover's own ink, at `radii.tile`
//! (13px). No such helper exists in `saola_theme::style::container` today:
//! the closest radius-13 helper, `container::tooltip`, is *also* solid ink
//! with the popover shadow — nested inside another ink surface (this
//! popover) it would be invisible, not recessed, and `translucent_panel`'s
//! scrim (the closest "subtle fill on a shell surface" shape that exists)
//! is built for the wallpaper to show through, not for sitting on ink.
//! CLAUDE.md forbids inventing a local container style to paper over a gap
//! like this ("if a needed style doesn't exist, add it to saola-theme —
//! don't restyle locally"), so [`media_row`] renders directly on the
//! popover's own ink instead of inside a distinct tile. Flagged here and in
//! the Stage 17 handoff as a saola-theme addition — something like
//! `container::tile(t, Surface)`: `on(s).fill_subtle` at `radii.tile`, the
//! same "subtle fill on a shell surface" shape `translucent_panel` already
//! has at `radii.pill`.
//!
//! # A second flagged gap: no popover content-padding token
//!
//! The style guide gives popovers "20–22px padding", but saola-theme has no
//! dedicated token for it. [`view`] reuses `sizes.panel_margin_ledger`
//! (20.0, inside the spec's range) — the wrong token *semantically* (it's a
//! screen-edge margin, not popover content padding) but token-derived
//! rather than a bare literal, which is the CLAUDE.md-compliant compromise
//! until a `sizes.popover_padding` (or similar) lands.

use iced::widget::{button, column, container, row, slider, text, toggler};
use iced::{Element, Fill};
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};

use crate::icons::{self, Icon};
use crate::modules::media::{self, Media};
use crate::modules::volume::{self, Volume};

/// Honest, permanently-disabled placeholder labels for the 2×2 toggle grid,
/// in row-major reading order (top-left, top-right, bottom-left,
/// bottom-right). See the module doc comment's "Honest scope" section for
/// why none of these has a real backend in this phase.
const PLACEHOLDER_TOGGLES: [&str; 4] = ["Wi-Fi", "Bluetooth", "Do Not Disturb", "Airplane Mode"];

/// The popover surface's declared height — the sole implementation behind
/// [`crate::popover::PopoverKind::height`].
///
/// Not a measurement: a wlr-layer-shell surface has to declare its size
/// *before* the compositor creates it, and iced 0.14 has no way to measure a
/// laid-out widget afterwards even if it could (see `popover.rs`'s "why a
/// popover has to declare a height at all" note). This is a declared
/// *estimate* built from the same tokens [`view`] actually lays its rows out
/// with, kept as one function so both sides read the same numbers rather
/// than two hand-copied ones that could drift. If it drifts anyway (a
/// future content tweak here without a matching update), the visible
/// symptom is a sliver of extra or missing ink at the popover's bottom edge
/// — [`view`]'s content is `Shrink`-height inside a `Fill` container, so a
/// mismatch never clips or panics.
pub fn height(theme: &Theme) -> f32 {
    let padding = theme.sizes.panel_margin_ledger;
    let gap = theme.sizes.island_gap;
    let grid_row = theme.sizes.list_row;
    // Two grid rows plus the gap between them.
    let grid = grid_row * 2.0 + gap;
    // The slider row is one control's worth of height; the media row needs
    // room for two lines of text (title + artist) beside the transport
    // buttons, so it's budgeted wider than one row rather than pretending
    // it's the same height as everything else.
    let volume_row = theme.sizes.hit_target_bar;
    let media_row = theme.sizes.list_row * 1.6;
    padding * 2.0 + grid + gap + volume_row + gap + media_row
}

/// The whole popover body: the toggle grid, the volume section, and the
/// media row, top to bottom, inset by the popover's own (flagged, see the
/// module doc comment) padding.
pub fn view<'a>(theme: &Theme, volume: &Volume, media: &Media) -> Element<'a, crate::Message> {
    container(
        column![
            toggle_grid(theme),
            volume_row(theme, volume),
            media_row(theme, media),
        ]
        .spacing(theme.sizes.island_gap),
    )
    .style(style::container::popover(theme))
    .padding(theme.sizes.panel_margin_ledger)
    .width(Fill)
    .height(Fill)
    .into()
}

/// The 2×2 grid of honest, disabled placeholders. See the module doc
/// comment's "Honest scope" section.
fn toggle_grid<'a>(theme: &Theme) -> Element<'a, crate::Message> {
    let cell_height = theme.sizes.list_row;
    let text_size = theme.typography.size.bar;
    let gap = theme.sizes.island_gap;

    // A closure rather than four repeated call sites: every cell is
    // identical chrome around a different label, and this is what keeps
    // that chrome defined exactly once. `style::button::muted(theme, ..)`
    // is built fresh *inside* the closure body (using the captured `&Theme`
    // — a reference, so `Copy`, unlike the closure it returns) rather than
    // hoisted into a shared binding above: that returned closure has no
    // `Copy`/`Clone` impl (an `impl Fn` return type only exposes the trait
    // it names) and `button::style` takes it by value, so a single
    // instance couldn't be moved into more than one button anyway.
    let cell = move |label: &'static str| -> Element<'a, crate::Message> {
        button(
            container(text(label).size(text_size))
                .width(Fill)
                .align_x(iced::Center),
        )
        // No `.on_press`: see the module doc comment for why looking
        // inert (iced's own `Status::Disabled`, unprompted) is the
        // honest choice for a cell with no backend at all.
        .width(Fill)
        .height(cell_height)
        .style(style::button::muted(theme, Surface::Ink))
        .into()
    };

    column![
        row![cell(PLACEHOLDER_TOGGLES[0]), cell(PLACEHOLDER_TOGGLES[1])].spacing(gap),
        row![cell(PLACEHOLDER_TOGGLES[2]), cell(PLACEHOLDER_TOGGLES[3])].spacing(gap),
    ]
    .spacing(gap)
    .into()
}

/// The slider section: the level glyph (the same mapping the bar module
/// uses, via `volume::volume_icon`, so the two can't drift), a slider
/// driving `volume::Message::SetVolume`, the percent, and a real mute
/// toggle driving `volume::Message::ToggleMute`. See the module doc
/// comment's "why the mute toggle is allowed to be terracotta here" section.
///
/// Renders a quiet placeholder line instead when no sink is known — pulse
/// absent, or the worker hasn't reported yet — the same "absent renders
/// nothing/quiet" contract as the bar's own volume readout, just spelled
/// out as text rather than an empty space (this row keeps its slot in the
/// popover's fixed layout either way).
fn volume_row<'a>(theme: &Theme, volume: &Volume) -> Element<'a, crate::Message> {
    if !volume.is_present() {
        return container(
            text("Volume unavailable")
                .size(theme.typography.size.bar)
                .color(theme.on_ink.secondary.into_iced()),
        )
        .height(theme.sizes.hit_target_bar)
        .align_y(iced::Center)
        .into();
    }

    let percent = volume.percent();
    let muted = volume.muted();
    let icon_color = if muted {
        theme.on_ink.secondary
    } else {
        theme.on_ink.primary
    };

    row![
        icons::icon(
            volume::volume_icon(percent, muted),
            theme.sizes.icon_bar,
            icon_color.into_iced(),
        ),
        slider(0.0..=100.0, percent as f32, |value| {
            crate::Message::Volume(volume::Message::SetVolume(value.round() as u32))
        })
        .style(style::slider::rest(theme, Surface::Ink))
        .width(Fill),
        text(format!("{percent}%"))
            .size(theme.typography.size.bar)
            .color(theme.on_ink.primary.into_iced()),
        toggler(muted)
            .label("Mute")
            .on_toggle(|_| crate::Message::Volume(volume::Message::ToggleMute))
            .style(style::toggles::toggler(theme, Surface::Ink)),
    ]
    .spacing(theme.sizes.pill_gap)
    .align_y(iced::Center)
    .height(theme.sizes.hit_target_bar)
    .into()
}

/// The media row: transport buttons plus title/artist, or a quiet "nothing
/// playing" placeholder — see the module doc comment's "flagged theme gap"
/// section for why this has no distinct recessed-tile background yet.
fn media_row<'a>(theme: &Theme, media: &Media) -> Element<'a, crate::Message> {
    let Some(now_playing) = media.now_playing() else {
        return container(
            text("Nothing playing")
                .size(theme.typography.size.bar)
                .color(theme.on_ink.secondary.into_iced()),
        )
        .height(theme.sizes.list_row)
        .align_y(iced::Center)
        .into();
    };

    let bus_name = now_playing.bus_name().to_string();
    let playing = now_playing.playing();
    let icon_color: iced::Color = theme.on_ink.primary.into_iced();

    // A closure rather than three repeated call sites, matching
    // `toggle_grid`'s reasoning above: every transport button is identical
    // chrome around a different glyph and message. `style::button::bare`
    // is built fresh per call for the same not-`Copy`-returned-closure
    // reason `toggle_grid`'s `cell` documents.
    let transport = move |icon: Icon, on_press: crate::Message| -> Element<'a, crate::Message> {
        button(icons::icon(icon, theme.sizes.icon_bar, icon_color))
            .style(style::button::bare(theme, Surface::Ink))
            .on_press(on_press)
            .into()
    };

    row![
        transport(
            Icon::SkipBack,
            crate::Message::Media(media::Message::Previous(bus_name.clone())),
        ),
        transport(
            if playing { Icon::Pause } else { Icon::Play },
            crate::Message::Media(media::Message::PlayPause(bus_name.clone())),
        ),
        transport(
            Icon::SkipForward,
            crate::Message::Media(media::Message::Next(bus_name.clone())),
        ),
        column![
            text(now_playing.title().to_string())
                .size(theme.typography.size.bar)
                .color(theme.on_ink.primary.into_iced()),
            text(now_playing.artist().to_string())
                .size(theme.typography.size.bar)
                .color(theme.on_ink.secondary.into_iced()),
        ]
        .width(Fill),
    ]
    .spacing(theme.sizes.pill_gap)
    .align_y(iced::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_is_positive_and_derived_from_tokens() {
        let theme = Theme::saola();
        assert!(height(&theme) > 0.0);
    }

    #[test]
    fn view_renders_without_panicking_with_no_live_state() {
        let theme = Theme::saola();
        let _: Element<'_, crate::Message> = view(&theme, &Volume::default(), &Media::default());
    }
}
