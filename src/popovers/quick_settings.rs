//! The quick-settings popover's content — [`crate::popover::PopoverKind::
//! QuickSettings`]'s real body, filling the empty ink shell Stage 16
//! (`crate::popover`) proved the open/close lifecycle with. Top to bottom:
//! the power-profile selector, the battery and Wi-Fi readouts, the volume
//! slider, and the media row, inset inside the popover's opaque ink
//! (`saola_theme::style::container::popover`).
//!
//! Rendered directly by `main.rs::Panel::popover_view` rather than through
//! `crate::popover::view` — this module needs to read `Power`/`Battery`/
//! `Network`/`Volume`/`Media` state that `popover.rs` deliberately never
//! learns about (see that module's doc comment for why keeping the
//! lifecycle manager ignorant of specific bar modules is the point, not an
//! oversight).
//!
//! # Scope (2026-08-01 revision)
//!
//! Stage 17 shipped a 2×2 grid of honest, permanently-disabled placeholder
//! toggles (Wi-Fi / Bluetooth / Do Not Disturb / Airplane Mode) because none
//! of them had a backend. That grid is gone: the popover now shows only
//! controls and readouts with something real behind them —
//!
//! * [`power_row`] — the first real toggle-like control: three segmented
//!   buttons driving power-profiles-daemon through `crate::modules::power`
//!   (the panel's first popover-only module). The active profile is the
//!   terracotta segment, every other one ivory — `style::segmented` is
//!   exactly the "terracotta when active, ivory when inactive" convention
//!   Jordan asked for, and the one rule already demanded.
//! * [`battery_row`] / [`network_row`] — richer readouts than the bar's:
//!   percentage plus time remaining, SSID plus signal strength. Bare icon +
//!   text on the popover's ink, no pill, exactly like their bar
//!   counterparts; the quiet/absent states use the `secondary` role.
//! * [`volume_row`] and [`media_row`] — unchanged in behaviour from Stage
//!   17 (slider + mute toggle; MPRIS transport), but the transport buttons
//!   are now ivory `button::rest` pills rather than `bare` labels (the same
//!   "ivory when inactive" convention as the segments), and the media row
//!   finally sits in its own recessed tile: the `container::tile` helper
//!   saola-theme grew for the gap Stage 17 flagged ("inset, tile-ish
//!   radius" with no helper to express it).
//!
//! The other flagged gap is closed too: the popover's content padding is
//! now the dedicated `sizes.popover_padding` token rather than a re-used
//! screen-margin token.
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
//! actually contradicting each other. The active power-profile segment is
//! the popover's one *solid* terracotta element (the toggler's on-track is
//! a small control accent, not a fill flood), which keeps the accent budget
//! honest.

use iced::widget::{button, column, container, row, slider, text, toggler};
use iced::{Element, Fill};
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};

use crate::icons::{self, Icon};
use crate::modules::battery::{self, Battery};
use crate::modules::bluetooth::{self, Bluetooth};
use crate::modules::brightness::{self, Brightness};
use crate::modules::media::{self, Media};
use crate::modules::network::{self, Network};
use crate::modules::power::{self, Power};
use crate::modules::volume::{self, Volume};

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
    let padding = theme.sizes.popover_padding;
    let gap = theme.sizes.island_gap;
    // The segmented power-profile selector and the two sliders (volume,
    // brightness) are interactive rows, so they get the bar hit-target
    // height; the three status readouts (battery, Wi-Fi, Bluetooth) are
    // plain single-line rows.
    let power_row = theme.sizes.hit_target_bar;
    let status_rows = theme.sizes.list_row * 3.0;
    let slider_rows = theme.sizes.hit_target_bar * 2.0;
    // The media tile wraps the two-line media row (title + artist beside
    // the transport buttons) in its own recessed padding.
    let media_tile = theme.sizes.list_row * 1.6 + theme.sizes.pill_gap * 2.0;
    padding * 2.0 + power_row + status_rows + slider_rows + media_tile + gap * 6.0
}

/// The whole popover body, top to bottom: power profiles, then the three
/// status readouts (battery, Wi-Fi, Bluetooth), then the two sliders
/// (volume, brightness), then media.
#[allow(clippy::too_many_arguments)] // one state ref per module the popover shows — a struct would just rename the list
pub fn view<'a>(
    theme: &Theme,
    power: &Power,
    battery: &Battery,
    network: &Network,
    bluetooth: &Bluetooth,
    volume: &Volume,
    brightness: &Brightness,
    media: &Media,
) -> Element<'a, crate::Message> {
    container(
        column![
            power_row(theme, power),
            battery_row(theme, battery),
            network_row(theme, network),
            bluetooth_row(theme, bluetooth),
            volume_row(theme, volume),
            brightness_row(theme, brightness),
            media_row(theme, media),
        ]
        .spacing(theme.sizes.island_gap),
    )
    .style(style::container::popover(theme))
    .padding(theme.sizes.popover_padding)
    .width(Fill)
    .height(Fill)
    .into()
}

/// A quiet single-line placeholder for a section whose backend is absent —
/// the popover's fixed layout keeps the slot either way, exactly like the
/// bar's "renders nothing" contract translated to a fixed-height panel.
fn quiet_row<'a>(theme: &Theme, label: &'static str, height: f32) -> Element<'a, crate::Message> {
    container(
        text(label)
            .size(theme.typography.size.bar)
            .color(theme.on_ink.secondary.into_iced()),
    )
    .height(height)
    .align_y(iced::Center)
    .into()
}

/// The power-profile selector: one segmented button per profile
/// power-profiles-daemon offers, in the daemon's own order (power-saver,
/// balanced, performance). The active profile is the terracotta segment
/// (`style::segmented::segment` with `is_selected`), every other one an
/// ivory `button::rest`-style key — the "terracotta when active, ivory when
/// inactive" convention, expressed entirely by theme styles.
///
/// Clicking a segment sends `power::Message::SetProfile`; there is
/// deliberately no optimistic local state. The daemon's own
/// `PropertiesChanged` signal moves the terracotta to the new segment, so a
/// refused write (polkit) leaves the popover truthful rather than lying
/// until the next signal.
fn power_row<'a>(theme: &Theme, power: &Power) -> Element<'a, crate::Message> {
    if !power.is_present() {
        return quiet_row(
            theme,
            "Power profiles unavailable",
            theme.sizes.hit_target_bar,
        );
    }

    let active = power.active();
    let segments = power.profiles().iter().map(|profile| {
        let is_selected = profile == active;
        let message = crate::Message::Power(power::Message::SetProfile(profile.clone()));
        button(
            // `super::centered` for the vertical axis (a button never
            // centres its own content — see that helper's teaching note),
            // plus `Fill`/`align_x` so the three labels share the track's
            // width equally.
            super::centered(text(profile_label(profile)).size(theme.typography.size.bar))
                .width(Fill)
                .align_x(iced::Center),
        )
        // Pressing the already-active segment re-sends the same profile —
        // harmless (the daemon treats it as a no-op), and simpler than
        // special-casing the selected key to be inert.
        .on_press(message)
        .width(Fill)
        .height(Fill)
        .style(style::segmented::segment(theme, Surface::Ink, is_selected))
        .into()
    });

    container(row(segments).spacing(theme.sizes.pill_gap))
        .style(style::segmented::track(theme, Surface::Ink))
        // The track's inset: enough for the segments' pill shape to read
        // against the track fill without inventing a new token — the same
        // "derive from an existing gap" move the bar's paddings make.
        .padding(theme.sizes.pill_gap / 2.0)
        .height(theme.sizes.hit_target_bar)
        .width(Fill)
        .into()
}

/// The battery readout: glyph + percentage in primary ivory, then the time
/// remaining and the current power draw `·`-separated in the quiet role
/// ("85% · 2h 14m remaining · 9.5W", with "to full" while charging).
/// Charging tints the glyph and percentage `accent_light`, exactly
/// mirroring the bar readout's one rule — the popover never invents a
/// second battery treatment.
fn battery_row<'a>(theme: &Theme, battery: &Battery) -> Element<'a, crate::Message> {
    if !battery.is_present() {
        return quiet_row(theme, "No battery", theme.sizes.list_row);
    }

    let color = if battery.charging() {
        theme.palette.accent_light
    } else {
        theme.on_ink.primary
    }
    .into_iced();

    // Every cell after the percentage is quiet-role text, including the `·`
    // separators between them — the same middle-dot convention as the
    // Bluetooth row's `device_summary`, just built as widgets here because
    // the percentage cell carries a different (primary/accent) color.
    let quiet = |label: String| {
        text(label)
            .size(theme.typography.size.bar)
            .color(theme.on_ink.secondary.into_iced())
    };

    let mut cells = row![
        // The bar's own leveled glyph (`battery_icon`), not a fixed outline
        // — the popover row is the bar readout plus its numbers, and reusing
        // the mapping keeps the two from ever showing different ladders.
        icons::icon(
            battery::battery_icon(battery.percentage(), battery.charging()),
            theme.sizes.icon_bar,
            color,
        ),
        text(format!("{:.0}%", battery.percentage().clamp(0.0, 100.0)))
            .size(theme.typography.size.bar)
            .color(color),
    ]
    .spacing(theme.sizes.bar_icon_gap)
    .align_y(iced::Center);

    if let Some(remaining) = battery.time_remaining() {
        let suffix = if battery.charging() {
            "to full"
        } else {
            "remaining"
        };
        cells = cells
            .push(quiet("·".to_string()))
            .push(quiet(format!("{remaining} {suffix}")));
    }

    // The instantaneous draw ("9.5W"), when UPower has a reading — quiet
    // like the time estimate: it's context, not the headline number.
    if let Some(draw) = battery.power_draw() {
        cells = cells.push(quiet("·".to_string())).push(quiet(draw));
    }

    container(cells)
        .height(theme.sizes.list_row)
        .align_y(iced::Center)
        .into()
}

/// The Wi-Fi readout: glyph + SSID in primary ivory, then the signal
/// strength `·`-separated in the quiet role ("HomeNet · 87%") — or the
/// quiet "offline" pair. The same two treatments as the bar's own readout
/// (connected is a resting state, never terracotta; see
/// `modules::network`).
fn network_row<'a>(theme: &Theme, network: &Network) -> Element<'a, crate::Message> {
    if !network.is_present() {
        return quiet_row(theme, "Wi-Fi unavailable", theme.sizes.list_row);
    }

    let (color, label, strength) = match network.ssid() {
        Some(ssid) => (
            theme.on_ink.primary.into_iced(),
            ssid.to_string(),
            network.strength_percent(),
        ),
        None => (
            theme.on_ink.secondary.into_iced(),
            "offline".to_string(),
            None,
        ),
    };

    let mut cells = row![
        // The bar's own arc ladder (`wifi_icon`) — same reuse reasoning as
        // `battery_row`'s glyph.
        icons::icon(
            network::wifi_icon(network.ssid().is_some(), network.strength_percent()),
            theme.sizes.icon_bar,
            color,
        ),
        text(label).size(theme.typography.size.bar).color(color),
    ]
    .spacing(theme.sizes.bar_icon_gap)
    .align_y(iced::Center);

    if let Some(percent) = strength {
        // Same quiet `·` separator as `battery_row`'s cells — one joining
        // convention across the popover's readout rows.
        let quiet = |label: String| {
            text(label)
                .size(theme.typography.size.bar)
                .color(theme.on_ink.secondary.into_iced())
        };
        cells = cells
            .push(quiet("·".to_string()))
            .push(quiet(format!("{percent}%")));
    }

    container(cells)
        .height(theme.sizes.list_row)
        .align_y(iced::Center)
        .into()
}

/// The Bluetooth readout: the bar's own glyph mapping
/// (`bluetooth::bluetooth_icon` — same reuse reasoning as `battery_row`'s)
/// beside a device summary. Powered with a device connected shows each
/// connected device's alias, with its battery percentage when the device
/// reports one (BlueZ's `Battery1`), joined quietly; powered but idle and
/// soft-off are the quiet `secondary` states. Status display only — no
/// pairing, no adapter toggle (CLAUDE.md's out-of-scope line for control
/// UI; a toggle would be a separate, opt-in decision like the stage-17
/// Wi-Fi one).
fn bluetooth_row<'a>(theme: &Theme, bt: &Bluetooth) -> Element<'a, crate::Message> {
    if !bt.is_present() {
        return quiet_row(theme, "Bluetooth unavailable", theme.sizes.list_row);
    }

    let any_connected = !bt.devices().is_empty();
    let (color, label) = if !bt.powered() {
        (theme.on_ink.secondary, "Bluetooth off".to_string())
    } else if any_connected {
        (theme.on_ink.primary, device_summary(bt.devices()))
    } else {
        (theme.on_ink.secondary, "No devices connected".to_string())
    };
    let color = color.into_iced();

    container(
        row![
            icons::icon(
                bluetooth::bluetooth_icon(bt.powered(), any_connected),
                theme.sizes.icon_bar,
                color,
            ),
            text(label).size(theme.typography.size.bar).color(color),
        ]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center),
    )
    .height(theme.sizes.list_row)
    .align_y(iced::Center)
    .into()
}

/// The brightness slider: a leveled sun glyph, a slider driving
/// `brightness::Message::SetBrightness`, and the percent. Same shape as
/// [`volume_row`] minus the mute toggle (a backlight has no mute).
///
/// Like the power segments, there is no optimistic local state: the slider
/// jumps when the udev change event confirms the write landed (see
/// `modules::brightness`), so a rejected write leaves the row truthful.
fn brightness_row<'a>(theme: &Theme, brightness: &Brightness) -> Element<'a, crate::Message> {
    if !brightness.is_present() {
        return quiet_row(theme, "Brightness unavailable", theme.sizes.hit_target_bar);
    }

    let percent = brightness.percent();
    let color = theme.on_ink.primary.into_iced();

    row![
        icons::icon(brightness_icon(percent), theme.sizes.icon_bar, color),
        slider(0.0..=100.0, percent as f32, |value| {
            crate::Message::Brightness(brightness::Message::SetBrightness(value.round() as u8))
        })
        .style(style::slider::rest(theme, Surface::Ink))
        .width(Fill),
        text(format!("{percent}%"))
            .size(theme.typography.size.bar)
            .color(color),
    ]
    .spacing(theme.sizes.pill_gap)
    .align_y(iced::Center)
    .height(theme.sizes.hit_target_bar)
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
        return quiet_row(theme, "Volume unavailable", theme.sizes.hit_target_bar);
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

/// The media row, inside its recessed tile (`container::tile` — the
/// "subtle fill on a shell surface" shape at `radii.tile`): transport
/// buttons plus title/artist, or a quiet "nothing playing" placeholder.
/// The tile is kept for the placeholder too, so the popover's shape
/// doesn't jump when playback starts.
fn media_row<'a>(theme: &Theme, media: &Media) -> Element<'a, crate::Message> {
    let tile = |content: Element<'a, crate::Message>| -> Element<'a, crate::Message> {
        container(content)
            .style(style::container::tile(theme, Surface::Ink))
            .padding(theme.sizes.pill_gap)
            .width(Fill)
            .into()
    };

    let Some(now_playing) = media.now_playing() else {
        return tile(
            container(
                text("Nothing playing")
                    .size(theme.typography.size.bar)
                    .color(theme.on_ink.secondary.into_iced()),
            )
            .height(theme.sizes.list_row * 1.6)
            .align_y(iced::Center)
            .into(),
        );
    };

    let bus_name = now_playing.bus_name().to_string();
    let playing = now_playing.playing();

    // A closure rather than three repeated call sites: every transport
    // button is identical chrome around a different glyph and message.
    // `style::button::rest` — an ivory pill, the popover's "inactive"
    // button treatment — is built fresh per call because its returned
    // closure has no `Clone` impl and `button::style` takes it by value
    // (the same reasoning `toggle_grid` documented before this revision).
    // The glyph is tinted ink: an ivory pill is a tiny paper surface, so
    // its content uses the on-paper roles, matching the label color
    // `button::rest` itself would set for text.
    let icon_color: iced::Color = theme.palette.ink.into_iced();
    let transport = move |icon: Icon, on_press: crate::Message| -> Element<'a, crate::Message> {
        button(icons::icon(icon, theme.sizes.icon_bar, icon_color))
            .style(style::button::rest(theme, Surface::Ink))
            .padding(theme.sizes.pill_gap)
            .on_press(on_press)
            .into()
    };

    tile(
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
        .into(),
    )
}

/// Connected devices → the Bluetooth row's one-line summary:
/// `"Buds Pro 85% · MX Master"` — each device's alias with its battery
/// percentage when the device reports one, `·`-joined. One fixed line
/// rather than a per-device list because the popover's surface height is
/// *declared*, not measured (see [`height`]) — a row per device would
/// change the needed height with pairing state.
///
/// Pure function of its argument — unit-tested below.
fn device_summary(devices: &[bluetooth::Device]) -> String {
    devices
        .iter()
        .map(|device| match device.battery_percent() {
            Some(percent) => format!("{} {percent}%", device.alias()),
            None => device.alias().to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Brightness percent → which sun glyph the slider row shows: `sun-dim`
/// below a third, `sun-medium` below two-thirds, full `sun` above. The only
/// leveled ladder defined here rather than in its module: brightness has no
/// bar readout, so the popover *is* the only consumer of the mapping.
///
/// Pure function of its argument — unit-tested below.
fn brightness_icon(percent: u8) -> Icon {
    if percent < 34 {
        Icon::SunDim
    } else if percent < 67 {
        Icon::SunMedium
    } else {
        Icon::Sun
    }
}

/// power-profiles-daemon's profile ids → the human labels the segments
/// show: `"power-saver"` → `"Power Saver"`, `"balanced"` → `"Balanced"`.
/// Generic word-capitalisation rather than a fixed three-name table, so a
/// daemon offering an unexpected profile still gets a readable key instead
/// of a blank one.
///
/// Pure function of its argument — unit-tested below.
fn profile_label(profile: &str) -> String {
    profile
        .split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        let _: Element<'_, crate::Message> = view(
            &theme,
            &Power::default(),
            &Battery::default(),
            &Network::default(),
            &Bluetooth::default(),
            &Volume::default(),
            &Brightness::default(),
            &Media::default(),
        );
    }

    #[test]
    fn device_summaries_join_aliases_and_append_known_batteries() {
        let devices = [
            bluetooth::Device {
                alias: "Buds Pro".to_string(),
                battery_percent: Some(85),
            },
            bluetooth::Device {
                alias: "MX Master".to_string(),
                battery_percent: None,
            },
        ];
        assert_eq!(device_summary(&devices), "Buds Pro 85% · MX Master");
        assert_eq!(device_summary(&devices[1..]), "MX Master");
    }

    #[test]
    fn the_sun_ladder_steps_at_its_documented_thresholds() {
        assert_eq!(brightness_icon(0), Icon::SunDim);
        assert_eq!(brightness_icon(33), Icon::SunDim);
        assert_eq!(brightness_icon(34), Icon::SunMedium);
        assert_eq!(brightness_icon(66), Icon::SunMedium);
        assert_eq!(brightness_icon(67), Icon::Sun);
        assert_eq!(brightness_icon(100), Icon::Sun);
    }

    #[test]
    fn profile_labels_capitalise_each_word() {
        assert_eq!(profile_label("power-saver"), "Power Saver");
        assert_eq!(profile_label("balanced"), "Balanced");
        assert_eq!(profile_label("performance"), "Performance");
    }

    #[test]
    fn unknown_profile_ids_still_get_a_readable_label() {
        assert_eq!(profile_label("ultra-low-latency"), "Ultra Low Latency");
        assert_eq!(profile_label(""), "");
    }
}
