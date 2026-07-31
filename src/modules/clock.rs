//! The bar's centered clock: a short date, a middle dot, then 24-hour time.
//! Refreshed about once a minute.
//!
//! **The clock's surface treatment depends on the panel style** (decided by
//! Jordan, 2026-07-31):
//!
//! - `style "ledger"` (concept 4b): the clock is the **only solid ivory pill
//!   on the bar**. Everything else renders directly on the ink surface in
//!   quiet text. The pill is a passive indicator and uses the `rest` button
//!   style pinned to the active state to keep a solid ivory fill even though
//!   it is not interactive.
//! - `style "islands"` (listing 2a, "Ink & ivory"): the clock is plain ivory
//!   **text on the island's own scrim**, exactly like every other module's bar
//!   text. An ivory pill nested inside the translucent ink island would be a
//!   surface inside a surface — and in the centre island the one solid ivory
//!   element is the niri-columns strip's focused dash, not the clock.
//!
//! Teaching note (why the module knows this at all): layout *mechanics* —
//! rows, spacers, alignment, which island a module lands in — stay entirely in
//! `main.rs`, and no module has ever needed to know about them. A module's
//! *surface treatment* is a different thing: what a widget sits on changes
//! what it may be styled as, and only the module can answer that about itself.
//! So `view` takes the style as an argument instead of the panel reaching into
//! this file's rendering. See `Panel::island_view`'s doc comment in `main.rs`
//! for the seam rule this amends.
//!
//! Teaching note (testability): `view` reads `Local::now()` once per
//! render and hands the timestamp to `format_clock`, a pure function of
//! its argument. A test can't control the wall clock, but it *can* call a
//! pure function with a fixed `DateTime` — that's the whole reason
//! `format_clock` doesn't call `Local::now()` itself (see the tests below).

use std::time::Duration;

use chrono::{DateTime, Local, Timelike};
use iced::widget::button::Status;
use iced::widget::{button, text};
use iced::{Center, Element, Fill, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};

use crate::config::PanelStyle;

/// The clock module's own message type (Stage 7's per-module refactor).
///
/// Teaching note (enum nesting): every bar module now owns its `Message`
/// instead of contributing variants directly to the panel's flat enum.
/// `main.rs` nests this whole type as one variant of its own `Message`
/// (`Message::Clock(clock::Message)`) and *delegates*: it never inspects
/// `Tick` itself, it just unwraps the outer variant and hands the inner
/// value back to this module's own logic (here, there's nothing to store,
/// so the delegation in `Panel::update` is just "fall through to
/// `Task::none()`" — see that match arm's comment). This scales far better
/// than the old flat enum once the module count grows past a handful: each
/// module's variants, and the logic that handles them, live next to each
/// other in one file instead of being scattered across `main.rs`.
///
/// Single unit variant, same as before the refactor — the clock still
/// carries no data (see `view`'s doc comment for why).
#[derive(Debug, Clone)]
pub enum Message {
    Tick,
}

/// Clock module state. Empty: unlike `Battery`/`Network` in later stages
/// (which cache the last value read off D-Bus), the clock has nothing to
/// cache between renders — every render reads the system clock fresh.
pub struct Clock;

impl Clock {
    /// Renders the clock in the treatment the active panel style calls for
    /// (see this module's doc comment for the design rule): a solid ivory
    /// pill in ledger style, bare ivory text in islands style. The reading
    /// itself — `format_clock`'s "date · time" — is identical either way, and
    /// so is the subscription that refreshes it.
    ///
    /// Teaching note (why the style is an argument, not state): `Clock` holds
    /// no state at all, and the style can only change on restart, so caching
    /// it in the struct would buy nothing and add a second place for it to go
    /// stale. `PanelStyle` is `Copy` (a two-variant fieldless enum), so
    /// passing it by value here is a byte, not a borrow — that is also why
    /// `Panel::module_view` can hand out `self.config.style` freely while it
    /// is still holding `&self.config` for the module lists.
    pub fn view(&self, theme: &Theme, style: PanelStyle) -> Element<'_, Message> {
        let reading = format_clock(Local::now());
        match style {
            PanelStyle::Ledger => self.ledger_pill(theme, reading),
            PanelStyle::Islands => self.islands_text(theme, reading),
        }
    }

    /// Ledger treatment: the solid ivory pill, the bar's only one.
    ///
    /// Teaching note (passive indicator): the clock is never "live" or
    /// "selected" — there's no terracotta state. The ivory fill is per the
    /// settled concept; the active-state pin in the style closure is a
    /// workaround because a disabled button would gray out (we want no
    /// interaction but solid color). The text inside gets the ink label role
    /// from the button style itself (ink text on ivory fill), which is why —
    /// unlike the islands branch below — nothing here sets a color.
    ///
    /// The pill height uses `sizes.panel_pill_clock` (32px), a compact token
    /// designed for smaller indicators inside the 48px bar.
    fn ledger_pill(&self, theme: &Theme, reading: String) -> Element<'_, Message> {
        let pill = style::button::rest(theme, Surface::Ink);
        button(
            text(reading)
                .size(theme.typography.size.bar)
                .height(Fill)
                .align_y(Center),
        )
        .height(theme.sizes.panel_pill_clock)
        .padding([0.0, theme.sizes.panel_pill_clock / 2.0])
        .style(move |iced_theme, _status| pill(iced_theme, Status::Active))
        .into()
    }

    /// Islands treatment: bare ivory text straight onto the island's scrim —
    /// no button, no fill, no padding, no local styling. This is the *exact*
    /// idiom every status module already uses for its label (see
    /// `battery::Battery::view` / `network::Network::view`): the bar type size
    /// plus a color taken from the ink surface's own roles, `primary` being
    /// the full-emphasis ivory those modules use at rest. The clock has no
    /// quiet or live state, so it is always `primary` — never `secondary`,
    /// never terracotta.
    ///
    /// Teaching note (why the color is explicit): iced's `text` inherits the
    /// theme's default text color, not the color of whatever container it was
    /// placed in, so "the island's text color" is not something a widget can
    /// pick up by being nested. Every module on this bar therefore names its
    /// role and converts it with `ColorExt::into_iced` — the hop from a
    /// `saola_theme` color to iced's own `Color` type.
    fn islands_text(&self, theme: &Theme, reading: String) -> Element<'_, Message> {
        text(reading)
            .size(theme.typography.size.bar)
            .color(theme.on_ink.primary.into_iced())
            .into()
    }

    /// Whether this module would draw anything right now. The wall clock
    /// has no service to be absent — this is constant `true`, and exists
    /// only so the module's public shape matches every other module's
    /// (the same uniformity argument as `mark`'s no-op `subscription`).
    pub fn is_present(&self) -> bool {
        true
    }

    /// A tick roughly once a minute, timed to land near the wall-clock
    /// minute boundary rather than 60s after whatever instant the app
    /// happened to boot at (which would leave the displayed minute stale
    /// for up to 59s after boot, and again after any drift).
    ///
    /// Teaching note (subscription wiring): iced calls `Panel::subscription`
    /// again after every message, and `iced::time::every`'s underlying
    /// `Subscription::run_with` restarts its inner timer whenever the value
    /// it's keyed on changes. Recomputing `duration_until_next_minute()`
    /// fresh from `Local::now()` on every call means each restart just
    /// re-targets the *actual* next boundary — the timer self-corrects
    /// instead of needing separate "first tick" / "steady state" phases.
    /// The tick carries no data (`Message::Tick` is a unit variant); its
    /// only job is to wake the runtime so `view` re-reads the clock.
    ///
    /// Returns `Subscription<Message>` — this module's own `Message`, not
    /// the panel's. `main.rs`'s `Panel::subscription` composes this with
    /// `.map(crate::Message::Clock)`, wrapping every tick in the outer
    /// variant before merging it into the panel's subscription set.
    pub fn subscription(&self) -> Subscription<Message> {
        iced::time::every(duration_until_next_minute()).map(|_instant| Message::Tick)
    }
}

/// Seconds remaining until the next minute boundary (e.g. :37 past the
/// minute returns 23s). Never zero — landing exactly on the boundary
/// rounds up to a full 60s rather than arming a zero-duration timer.
fn duration_until_next_minute() -> Duration {
    let seconds_into_minute = Local::now().second() as u64;
    Duration::from_secs(60 - (seconds_into_minute % 60))
}

/// Formats a timestamp as the bar shows it: short "Day DD Mon" date, a
/// middle dot, then 24-hour "HH:MM" time. Both formats live in this one
/// function so there's a single place to tweak later.
///
/// Pure function of `now` — no `Local::now()` call inside — which is
/// what makes it unit-testable without depending on the system clock or
/// its timezone.
fn format_clock(now: DateTime<Local>) -> String {
    format!("{} · {}", now.format("%a %d %b"), now.format("%H:%M"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn formats_short_date_and_24_hour_time() {
        let now = Local.with_ymd_and_hms(2026, 7, 26, 14, 5, 0).unwrap();
        assert_eq!(format_clock(now), "Sun 26 Jul · 14:05");
    }

    #[test]
    fn pads_single_digit_hour_and_minute() {
        let now = Local.with_ymd_and_hms(2026, 1, 5, 9, 3, 0).unwrap();
        assert_eq!(format_clock(now), "Mon 05 Jan · 09:03");
    }
}
