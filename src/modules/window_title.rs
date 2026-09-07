//! The focused window's title — the left region's ambient text.
//!
//! Style guide §7 ("Focused window title", 2026-08-01): the title sits to the
//! right of the mark — bare text on the ledger bar, an island of its own
//! beside the mark's in islands style — and it is **context, not a state**.
//! So it is drawn in
//! the quiet `on_ink.secondary` role at the bar type size, never in a pill,
//! never in terracotta, and with no icon of its own. No focused window, or a
//! window whose title is empty, renders nothing: the same absent-service
//! treatment every other module gives a service that isn't there.
//!
//! # Where the data comes from
//!
//! niri IPC, through the shared bridge in `modules::niri` — the same socket
//! and the same fold that feed the columns minimap. This module has no worker
//! of its own, and its only subscription is the opt-in marquee's animation
//! timer below (see [`WindowTitle::subscription`]); the
//! bridge derives "which window has focus, and what is it called", suppresses
//! every event that leaves that string unchanged, and sends
//! [`Message::Updated`] only when the title text actually moved. That dedupe
//! is what keeps a terminal spinner retitling several times a second from
//! re-rendering the panel at its rate — read `modules::niri`'s doc comment
//! before changing anything about it.
//!
//! # Overflow
//!
//! A title is capped at a configurable character limit (`max-chars`, default
//! 50) — characters rather than pixels, because that is the knob a human can
//! reason about under a proportional face. The default overflow mode
//! [`truncate`](crate::config::TitleOverflow::Truncate)s with a single `…`;
//! the second mode, `marquee`, is the style guide's opt-in ping-pong sweep
//! (§5): dwell 2 s at the head, sweep left at 24 px/s until the tail is
//! fully visible, dwell 2 s, sweep back, repeat. Translation only — no fade,
//! no scale, no colour change, and the text never wraps.
//!
//! # The marquee's two halves
//!
//! The animation is split so that all of its *logic* is pure and all of its
//! *measurement* is in one small widget:
//!
//! - [`phase_at`] / [`offset_at`] are pure functions of elapsed time and the
//!   overflow distance in pixels — the whole `DwellHead → SweepLeft →
//!   DwellTail → SweepRight → repeat` state machine, unit-tested below
//!   without a compositor, a theme, or a running iced app.
//! - [`Marquee`] is a small custom widget that measures the title with the
//!   renderer's own text stack (`Plain<Paragraph>`, the same cache the stock
//!   `text` widget keeps), draws it translated by [`offset_at`]'s pixels,
//!   and clips to its own bounds. That is what makes the sweep *pixel*-true
//!   rather than a character-at-a-time slide: 24 px/s is a pixel rate, and
//!   the only place pixels for a given string and font actually exist is the
//!   renderer.
//!
//! The visible window is `max-chars` wide, in the one sense that reconciles a
//! character knob with a pixel sweep — see [`window_width`].
//!
//! Like `modules::claude`'s breath, the loop is **gated**: the timer exists
//! only while marquee mode is configured *and* the title on screen actually
//! overflows (see [`WindowTitle::subscription`]).
//!
//! # Where the timing numbers come from
//!
//! `saola-tokens` v0.8.0 (bundled from `saola-theme` v0.14.0) added
//! `motion.marquee_dwell`, `motion.marquee_speed`, and `motion.frame`, so the
//! two §5 numbers and the tick cadence are theme tokens now, not local
//! constants — the module doc used to note that the theme had nothing to
//! offer here; it does now. The wrinkle is that [`WindowTitle::subscription`]
//! and the widget's draw path (see [`Marquee`]) have no `&Theme` to read
//! from — the same problem `modules::claude`'s `breath_elapsed` doc comment
//! names for the breath. The fix here is the same shape: [`WindowTitle`]
//! caches the three values, converted to the units the animation math wants
//! (`Duration` for the two durations, `f32` px/s for the rate), when the
//! module is constructed ([`WindowTitle::new`]) or the theme is rebuilt on a
//! config reload ([`WindowTitle::set_config`]) — both of which *do* have a
//! `&Theme` in hand — and every other method reads the cached fields rather
//! than reaching for the theme itself.

use std::time::Duration;

// The advanced text stack is imported under a name of its own because
// `iced::widget::text` (the stock widget the truncate path uses, just below)
// occupies the plain `text` name in this file.
use iced::advanced::text as advanced_text;
use iced::advanced::text::paragraph::Plain;
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{layout, mouse, renderer, Layout, Widget};
use iced::alignment;
use iced::time::Instant;
use iced::widget::text::Wrapping;
use iced::widget::{text, Space};
use iced::{Color, Element, Length, Pixels, Point, Rectangle, Size, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::Theme;

use crate::config::{TitleOverflow, WindowTitleConfig};

/// The character this module truncates with: **one** glyph, not three dots.
/// A literal `...` would eat three of the user's `max-chars` budget and read
/// as punctuation rather than as elision.
const ELLIPSIS: char = '…';

/// This module's own message type (the per-module refactor — see
/// `modules::clock::Message` for the full teaching note). `main.rs` nests it
/// as `Message::WindowTitle(window_title::Message)`.
///
/// `None` is the single absent case: no focused window *or* a focused window
/// with a blank title. The bridge collapses the two (see
/// `modules::niri::Strip::focused_title`) precisely so this side has one
/// thing to render as nothing rather than two.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Option<String>),
    /// One frame of the marquee sweep, carrying the tick's own [`Instant`]
    /// — the shape `claude::Message::Tick` established, and for the same
    /// reason: the position is derived from *when* the tick happened, so
    /// taking that from the runtime's timer keeps [`WindowTitle::update`]
    /// deterministic and testable rather than having it read a clock.
    ///
    /// Only ever sent while the marquee is actually running (see
    /// [`WindowTitle::subscription`]); in `truncate` mode, or with a title
    /// that fits, no timer exists to send it.
    Tick(Instant),
}

/// Module state: the focused window's title as the bridge last reported it,
/// plus the `panel.toml` knobs that decide how it is capped.
///
/// `Default` is the boot state (`None` — nothing focused yet, so nothing
/// drawn), which is also where a session that isn't running under niri stays
/// forever.
///
/// `Eq` is dropped from the derive here (every other field still supports
/// it) because `marquee_speed` is an `f32` — floats aren't `Eq`, only
/// `PartialEq`, and nothing in this crate ever needed to hash or
/// exhaustively-compare a whole `WindowTitle`.
// `Default` exists only so `new` can spell `..Self::default()`: a bare
// `WindowTitle::default()` leaves the cached motion values below at zero,
// and a zero `frame_tick` handed to `iced::time::every` would spin the
// runtime. Always construct through [`WindowTitle::new`], which fills them
// from the theme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowTitle {
    title: Option<String>,
    /// The `panel.toml` `[window-title]` block's knobs: resolved at boot
    /// ([`Self::new`]) and swapped in place on a live config reload
    /// ([`Self::set_config`]), same as every other config-fed module
    /// (`mark`, `claude`).
    config: WindowTitleConfig,
    /// When the current marquee run started. `None` while nothing is
    /// sweeping; set from the first [`Message::Tick`] after the timer starts,
    /// and cleared whenever the title changes so a new window's title begins
    /// at its head dwell rather than resuming the previous one's phase.
    ///
    /// Teaching note (why an epoch rather than an accumulator): identical to
    /// `modules::claude`'s `breath_epoch` — a `position += tick` accumulator
    /// banks the error of every late timer fire forever, whereas recomputing
    /// from a stored start instant costs a single frame per late tick and
    /// nothing after it.
    sweep_epoch: Option<Instant>,
    /// How long the current marquee run has been going, as of the last tick.
    /// Turned into a pixel offset at *draw* time by [`offset_at`], because
    /// the travel distance it is measured against only exists once the
    /// renderer has measured the text (see [`Marquee`]).
    sweep_elapsed: Duration,
    /// `motion.marquee_dwell`, converted from the token's milliseconds to a
    /// `Duration` once, at the point where a `&Theme` is in hand — see the
    /// module doc comment's "Where the timing numbers come from". Read by
    /// [`Self::view`] (by way of [`Marquee`]) instead of the theme itself.
    marquee_dwell: Duration,
    /// `motion.marquee_speed`, in logical pixels per second — already the
    /// unit [`offset_at`] wants, so no conversion beyond the copy.
    marquee_speed: f32,
    /// `motion.frame`, converted to a `Duration` for [`iced::time::every`].
    /// This is the value [`Self::subscription`] exists to read: it is the
    /// one method here with no `&Theme` argument anywhere in its call chain.
    frame_tick: Duration,
}

impl WindowTitle {
    /// Boot state for the configured knobs. Called from `Panel::new` with
    /// `config.window_title` and the theme built at boot, the same
    /// "config picks, module renders" split `Mark::new`/`ClaudeCode::new`
    /// already use — `theme` is only read here for the three motion values
    /// above ([`Self::cache_motion`]).
    pub fn new(config: WindowTitleConfig, theme: &Theme) -> Self {
        Self {
            config,
            ..Self::default()
        }
        .with_motion(theme)
    }

    /// Swap in freshly reloaded knobs, keeping the title that's on screen —
    /// `main.rs`'s live-reload arm calls this instead of rebuilding the
    /// module with [`Self::new`], which would blank the title until niri's
    /// next focus event happened to re-announce it.
    ///
    /// Any marquee run in progress restarts from its head dwell: a changed
    /// `max-chars` moves the sweep's travel distance and a changed `overflow`
    /// may stop the sweep entirely, so resuming the old run's phase against
    /// the new geometry would show the title from some arbitrary middle
    /// position. The `!=` guard keeps the no-op reload (some *other* knob
    /// changed) from resetting a sweep for no reason.
    ///
    /// `theme` is the freshly rebuilt one `main.rs`'s reload arm just built
    /// via `config::build_theme` — re-caching the motion values from it on
    /// every reload costs nothing (`[colors]` overrides never touch
    /// `motion`, so the three values never actually change), and it means
    /// this module never has to assume that stays true.
    pub fn set_config(&mut self, config: WindowTitleConfig, theme: &Theme) {
        if config != self.config {
            self.config = config;
            self.sweep_epoch = None;
            self.sweep_elapsed = Duration::ZERO;
        }
        self.cache_motion(theme);
    }

    /// Resolves the three motion tokens from `theme` into this module's own
    /// units and stores them — the one place that reads `theme.motion` in
    /// this file, called from both [`Self::new`] and [`Self::set_config`].
    fn cache_motion(&mut self, theme: &Theme) {
        self.marquee_dwell = Duration::from_millis(u64::from(theme.motion.marquee_dwell));
        self.marquee_speed = theme.motion.marquee_speed;
        self.frame_tick = Duration::from_millis(u64::from(theme.motion.frame));
    }

    /// Builder-style wrapper around [`Self::cache_motion`] for [`Self::new`],
    /// which constructs its value with struct-update syntax before there is
    /// a `&mut self` to call a setter on.
    fn with_motion(mut self, theme: &Theme) -> Self {
        self.cache_motion(theme);
        self
    }

    /// Folds one message into the state. `main.rs` unwraps only the outer
    /// `Message::WindowTitle(..)` and delegates here (the shape `claude.rs`
    /// established) rather than destructuring this module's variants in
    /// `Panel::update`.
    pub fn update(&mut self, message: Message) {
        match message {
            // A *different* title restarts the marquee at its head dwell:
            // focus moving to another window (or the same window renaming
            // itself) is new text, and picking it up mid-sweep would show
            // its middle first. The `!=` guard is what keeps a repeat of the
            // title already on screen from doing that — the niri bridge
            // already suppresses those (see the module doc comment), so this
            // is belt-and-braces rather than the primary defence.
            Message::Updated(title) => {
                if title != self.title {
                    self.title = title;
                    self.sweep_epoch = None;
                    self.sweep_elapsed = Duration::ZERO;
                }
            }
            // `get_or_insert` makes the *first* tick of a run establish its
            // epoch, so no separate "start" message is needed — the first
            // frame after the subscription opens is the moment the run began.
            // `saturating_duration_since` rather than `now - epoch`:
            // subtracting `Instant`s panics on a negative result, and a panic
            // on the UI thread is not how to discover a timer went backwards.
            Message::Tick(now) => {
                let epoch = *self.sweep_epoch.get_or_insert(now);
                self.sweep_elapsed = now.saturating_duration_since(epoch);
            }
        }
    }

    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` and `Panel::region` ask before spending
    /// a pill (or a gap) on a module. Reads the same field as `view`'s early
    /// return, so the two cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.title.is_some()
    }

    /// Whether the title on screen is longer than its character budget — the
    /// question both overflow modes turn on. `false` with no focused window,
    /// which is what makes "focus lost" stop the marquee for free.
    fn overflows(&self) -> bool {
        self.title
            .as_deref()
            .is_some_and(|title| title.chars().count() > self.config.max_chars)
    }

    /// The marquee gate: opted in *and* actually overflowing. This is the
    /// single predicate [`Self::subscription`] consults and the single
    /// predicate [`Self::view`] branches on, so the timer can never run
    /// without motion on screen, nor motion appear without a timer.
    ///
    /// Deliberately a character comparison rather than a pixel one, and that
    /// costs nothing in exactness: the visible window is defined as
    /// `max_chars` × the title's own average advance ([`window_width`]), so
    /// "wider than the window in pixels" and "longer than the budget in
    /// characters" are the *same* condition by construction.
    fn is_marqueeing(&self) -> bool {
        matches!(self.config.overflow, TitleOverflow::Marquee) && self.overflows()
    }

    /// Renders the capped title as bare quiet text, or nothing at all when
    /// no window is focused (`Space::new()` with no size is a zero-area
    /// widget — the row simply closes up around it).
    ///
    /// Every value is a token: `typography.size.bar` for the size,
    /// `on_ink.secondary` for the color. `secondary` rather than `primary` is
    /// the whole design point — this is ambient context sitting beside the
    /// mark, and it must not compete with the status readouts on the other
    /// side of the bar (§7). It is never terracotta: terracotta means *live*,
    /// and a window title is not a state.
    ///
    /// Teaching note (why the color is explicit): iced's `text` inherits the
    /// application theme's default text color, not the color of whatever
    /// container it was placed in, so a module can't pick up "the bar's text
    /// color" by being nested in the bar. Every module therefore names its
    /// role and converts it with `ColorExt::into_iced`.
    ///
    /// `Wrapping::None` for the same reason `media.rs` sets it: a long title
    /// must never wrap into a second line inside a 48px bar. The character
    /// cap below is what keeps it from stretching the region instead — as a
    /// cut in truncate mode, and as the width of the window the text sweeps
    /// through in marquee mode ([`Marquee`]), which is the same budget spent
    /// two ways.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        let Some(title) = self.title.as_deref() else {
            return Space::new().into();
        };

        let size = theme.typography.size.bar;
        let color = theme.on_ink.secondary.into_iced();

        // The one branch: an overflowing title in `marquee` mode sweeps
        // (style guide §5), and *everything else* — every truncate-mode
        // title, and a marquee-mode title that fits — renders through the
        // stock `text` widget exactly as it always has. `truncate` is a no-op
        // on a title within its budget, so the fits case needs no second
        // path of its own.
        if self.is_marqueeing() {
            return Element::new(Marquee {
                content: title,
                max_chars: self.config.max_chars,
                size,
                color,
                elapsed: self.sweep_elapsed,
                dwell: self.marquee_dwell,
                speed: self.marquee_speed,
            });
        }

        text(truncate(title, self.config.max_chars))
            .size(size)
            .color(color)
            .wrapping(Wrapping::None)
            .into()
    }

    /// No *signal* subscription of this module's own: the niri socket is
    /// shared with `modules::columns`, so a single
    /// `modules::niri::subscription()` in `Panel::subscription` feeds both
    /// (see that module's doc comment for why one bridge rather than two
    /// connections). What this returns is the marquee's animation timer, and
    /// only while there is something to animate.
    ///
    /// **Teaching note (the panel's second sanctioned loop).** CLAUDE.md's
    /// rule is that every module maps to a signal, never a poll, with the
    /// Claude Code breath as the one sanctioned exception. The style guide
    /// (§5) sanctions this one as the second — "the shell's second sanctioned
    /// loop after the session status breath" — and it is held to exactly the
    /// same boundary as the first (see `modules::claude::ClaudeCode::
    /// subscription` for the full argument):
    ///
    /// - It is **not a poll.** Nothing is asked and nothing is read; the
    ///   title still arrives only over the niri bridge. The timer advances a
    ///   translation that is, by definition, a function of time.
    /// - It is **gated twice over.** The mode has to be opted into in
    ///   `panel.toml` (a stock desktop never reaches this branch at all), and
    ///   even then the title on screen has to actually overflow. A title that
    ///   fits, a window that loses focus, a focused window with a blank title
    ///   — each closes [`Self::is_marqueeing`] and this drops straight back
    ///   to `Subscription::none()` on the very next recomputation.
    ///
    /// Teaching note (subscription identity): iced recomputes
    /// `Panel::subscription` after every message and diffs the result.
    /// `iced::time::every` keys on its `Duration` — [`Self::frame_tick`],
    /// cached from `motion.frame` and effectively constant for the process's
    /// whole lifetime (see [`Self::cache_motion`]) — so an unrelated message
    /// never restarts the timer mid-sweep; it is created when the gate opens
    /// and torn down when it closes, which is exactly the lifecycle wanted.
    /// The `Instant` the ticks carry is the runtime's, so a torn-down-and-
    /// recreated timer still can't rewind the animation: the epoch is
    /// re-established from the next tick.
    ///
    /// Frames (`window::frames()`) would be the other tick source; the
    /// interval timer is used because `claude.rs` set that precedent and
    /// because it decouples the animation's rate from the compositor's — a
    /// 30 Hz sweep should cost 30 wakes a second on a 144 Hz screen too.
    pub fn subscription(&self) -> Subscription<Message> {
        if !self.is_marqueeing() {
            return Subscription::none();
        }

        iced::time::every(self.frame_tick).map(Message::Tick)
    }
}

/// Caps `title` at `max_chars` **characters**, ending in a single [`ELLIPSIS`]
/// when anything was cut.
///
/// Two things are deliberate:
///
/// 1. **The ellipsis is inside the budget.** A title cut at `max_chars = 50`
///    renders as 49 characters plus `…`, never 51 — the knob names the width
///    the user gets, not the width before an extra glyph is bolted on.
/// 2. **Characters, never bytes.** `&title[..max_chars]` would panic the
///    panel the first time a window title contained a non-ASCII character
///    (an em dash in an editor's title, a CJK filename, an emoji) and the cut
///    landed mid-codepoint. `char_indices` gives the byte offset of a real
///    boundary, so the slice below is always valid. Note this counts
///    `char`s — Unicode scalar values — not grapheme clusters, so a
///    family-emoji sequence counts as several; getting that right needs a
///    segmentation crate, and the cap is an approximation of visual width
///    either way (style guide §7 says as much).
///
/// Pure function of its arguments — which is what makes it unit-testable
/// without a compositor or a theme.
fn truncate(title: &str, max_chars: usize) -> String {
    // Fast path: nothing to cut. `chars().count()` walks the string, but so
    // does rendering it, and titles are short.
    if title.chars().count() <= max_chars {
        return title.to_owned();
    }
    // A budget of one leaves room for the ellipsis alone; zero (which the
    // config parser rejects, but which a future caller could still pass)
    // leaves room for nothing. Both are defensive, not reachable from
    // `panel.toml`.
    if max_chars == 0 {
        return String::new();
    }

    // The byte offset where the (max_chars - 1)th character starts — i.e.
    // where the kept prefix ends. `nth` is safe here because the fast path
    // above proved there are more than `max_chars` characters.
    let cut = title
        .char_indices()
        .nth(max_chars - 1)
        .map_or(title.len(), |(offset, _char)| offset);

    let mut capped = String::with_capacity(cut + ELLIPSIS.len_utf8());
    capped.push_str(&title[..cut]);
    capped.push(ELLIPSIS);
    capped
}

/// Where the ping-pong is, as of `elapsed` into the run — the whole state
/// machine of style guide §5, and a pure function of two numbers.
///
/// `overflow_px` is how far the text has to travel: the measured width of the
/// full title minus the width of the window it is being shown through. Zero
/// (or negative — a title that fits) means there is nothing to sweep, and the
/// machine parks at [`Phase::DwellHead`] forever, which is what makes "the
/// loop stops the moment the focused title fits" fall out of the arithmetic
/// rather than needing a special case at the call site.
///
/// The cycle is `dwell + sweep + dwell + sweep`, where `sweep = overflow_px /
/// speed` — `speed` is `motion.marquee_speed` (style guide §5's `24px/s`
/// default), a *rate*, not a duration, so a longer title takes proportionally
/// longer to cross. `dwell` is `motion.marquee_dwell`, converted to a
/// `Duration` by the caller ([`WindowTitle::cache_motion`]) since both ends
/// of the ping-pong rest for the same length of time.
///
/// Teaching note (why `f64` inside): the modulo below is what keeps a title
/// that has been sweeping for an hour in the same step as one that started a
/// second ago. `f32` has ~7 significant digits, so at an hour's elapsed
/// seconds it can no longer resolve a frame; `f64` has ~16 and stays exact
/// for far longer than any window keeps focus. `modules::claude`'s
/// `phase_of` solves the same problem with an integer-millisecond modulo; it
/// can, because its cycle length is an integer number of milliseconds, and
/// this one's is a division by a pixel rate.
fn phase_at(elapsed: Duration, overflow_px: f32, dwell: Duration, speed: f32) -> Phase {
    // The `is_finite` guard is for a degenerate measurement (a NaN width):
    // NaN fails every comparison, so without it the machine would fall
    // through to the sweep arms and propagate NaN into an offset. Parking is
    // the right answer for "no travel to do" however that arises.
    if !overflow_px.is_finite() || overflow_px <= 0.0 {
        return Phase::DwellHead;
    }

    let sweep = f64::from(overflow_px) / f64::from(speed);
    let dwell = dwell.as_secs_f64();
    let cycle = 2.0 * (dwell + sweep);
    let at = elapsed.as_secs_f64() % cycle;

    if at < dwell {
        Phase::DwellHead
    } else if at < dwell + sweep {
        Phase::SweepLeft {
            progress: ((at - dwell) / sweep) as f32,
        }
    } else if at < dwell + sweep + dwell {
        Phase::DwellTail
    } else {
        Phase::SweepRight {
            progress: ((at - (dwell + sweep + dwell)) / sweep) as f32,
        }
    }
}

/// How far left the text is drawn, in logical pixels, at `elapsed` into the
/// run — `0.0` at the head, `overflow_px` at the tail, linear in between.
///
/// This is the only number [`Marquee::draw`] takes from the animation, and
/// the only thing the state machine is *for*: the sweep is a translation and
/// nothing else (style guide §5 — no fade, no scale, no colour change).
fn offset_at(elapsed: Duration, overflow_px: f32, dwell: Duration, speed: f32) -> f32 {
    match phase_at(elapsed, overflow_px, dwell, speed) {
        Phase::DwellHead => 0.0,
        Phase::SweepLeft { progress } => overflow_px * progress,
        Phase::DwellTail => overflow_px,
        Phase::SweepRight { progress } => overflow_px * (1.0 - progress),
    }
}

/// One step of the §5 ping-pong. `progress` runs `0.0..1.0` across a sweep
/// (never reaching 1.0 — that instant is the next phase's start), which is
/// what lets [`offset_at`] be a plain interpolation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Parked at the head of the title, 2 s.
    DwellHead,
    /// Travelling left, revealing the tail.
    SweepLeft { progress: f32 },
    /// Parked with the tail fully visible, 2 s.
    DwellTail,
    /// Travelling back right, returning to the head.
    SweepRight { progress: f32 },
}

/// The width of the window the title is shown through: `max_chars`
/// characters, where "a character" is the measured average advance of *this
/// title in this font* (`full_width / chars`).
///
/// Teaching note (why an average, and why that is the honest answer here):
/// `max-chars` is a count, the sweep rate is pixels, and something has to
/// translate between them. Under a proportional face there is no single
/// pixel width for "50 characters" — `WWWWW` and `iiiii` differ by a factor
/// of three — so any translation is an approximation, which the style guide
/// says outright of the cap itself (§7: "measured in characters, not pixels
/// — approximate under a proportional face"). Taking the average from the
/// title actually on screen keeps the approximation *self-consistent*: the
/// window is exactly `max_chars/chars` of the full width, so it is narrower
/// than the title if and only if the title is longer than the budget — which
/// is precisely the character-count gate [`WindowTitle::is_marqueeing`]
/// applies. The alternative (measuring a reference glyph once) would let the
/// two disagree, and a timer ticking against a text that turned out to fit
/// is exactly the standing-timer failure the gate exists to prevent.
///
/// Note this is the width of the *window*, not of any truncation: nothing is
/// cut in marquee mode, the full title is drawn and clipped.
fn window_width(full_width: f32, chars: usize, max_chars: usize) -> f32 {
    if chars == 0 {
        return 0.0;
    }
    let average_advance = full_width / chars as f32;
    (average_advance * max_chars as f32).min(full_width)
}

/// The marquee's rendering half: a fixed-width window onto a title that is
/// laid out in full and drawn translated.
///
/// Teaching note (why a custom widget rather than composing stock ones):
/// §5's sweep is specified in pixels per second, so the animation needs the
/// title's width in pixels — and text has a width only once a renderer has
/// shaped it with a real font. iced exposes that measurement to widgets and
/// nowhere else: [`Plain`] is the same paragraph cache the stock `text`
/// widget keeps in its own tree state, `min_bounds()` is the measurement,
/// and `fill_paragraph`'s `clip_bounds` is the clipping. Everything this
/// widget adds on top is two lines — a narrower layout node, and an `x`
/// shifted by [`offset_at`].
///
/// It draws *only* text, in the colour and size the module already resolved
/// from tokens, so it introduces no styling of its own — nothing here is a
/// local restyle of the sort CLAUDE.md forbids.
struct Marquee<'a> {
    /// The whole title, uncut — the cap becomes the window's width, not a
    /// slice of the string.
    content: &'a str,
    /// The window's width, in characters (see [`window_width`]).
    max_chars: usize,
    /// `typography.size.bar`, passed in rather than read here: this widget
    /// never sees a `saola_theme::Theme`.
    size: f32,
    /// `on_ink.secondary`, already converted — same reasoning as `size`.
    color: Color,
    /// How long the current run has been going ([`WindowTitle::
    /// sweep_elapsed`]); turned into pixels by [`offset_at`] in `draw`, once
    /// the travel distance is known.
    elapsed: Duration,
    /// `motion.marquee_dwell`, already resolved to a `Duration` by
    /// [`WindowTitle::cache_motion`] — same reasoning as `size` and `color`:
    /// this widget never sees a `saola_theme::Theme` to read it from itself.
    dwell: Duration,
    /// `motion.marquee_speed`, in logical pixels per second.
    speed: f32,
}

/// The widget's tree state: the renderer's laid-out copy of the title.
///
/// Cached in the tree (rather than re-shaped every frame) for the same
/// reason the stock `text` widget caches it — shaping is the expensive part,
/// and [`Plain::update`] re-shapes only when the content or format actually
/// changed. At 30 frames a second that difference is the whole cost of the
/// animation.
struct MarqueeState<P: advanced_text::Paragraph> {
    paragraph: Plain<P>,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Marquee<'_>
where
    Renderer: advanced_text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<MarqueeState<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(MarqueeState::<Renderer::Paragraph> {
            paragraph: Plain::default(),
        })
    }

    /// Shrink in both axes: the window is as wide as `max_chars` asks for and
    /// no wider, so the module occupies the same kind of space truncate mode
    /// would and the row closes up around it.
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    /// Measures the title at its natural width, then hands back a node only
    /// as wide as the window.
    ///
    /// `Size::INFINITE` bounds with `Wrapping::None` is what "measure this
    /// string as one line, however long that is" looks like — the resulting
    /// `min_bounds().width` is the full pixel width the sweep travels across.
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree
            .state
            .downcast_mut::<MarqueeState<Renderer::Paragraph>>();

        let _ = state.paragraph.update(advanced_text::Text {
            content: self.content,
            bounds: Size::INFINITE,
            size: Pixels(self.size),
            line_height: advanced_text::LineHeight::default(),
            font: renderer.default_font(),
            align_x: advanced_text::Alignment::Default,
            align_y: alignment::Vertical::Top,
            // Advanced shaping, not `Auto`: window titles are exactly where
            // non-ASCII shows up (an em dash in an editor's title, a CJK
            // filename), and `truncate`'s tests already treat that as the
            // normal case rather than the exotic one.
            shaping: advanced_text::Shaping::Advanced,
            wrapping: Wrapping::None,
        });

        let full = state.paragraph.min_bounds();
        let window = window_width(full.width, self.content.chars().count(), self.max_chars)
            // Never wider than the space the layout actually offered — a
            // narrow screen clips further, it does not push the row over.
            .min(limits.max().width);

        layout::Node::new(limits.resolve(
            Length::Shrink,
            Length::Shrink,
            Size::new(window, full.height),
        ))
    }

    /// Draws the whole title, shifted left by the animation's offset and
    /// clipped to the window.
    ///
    /// `clip_bounds` is the mechanism the renderer already uses for scrolled
    /// and overflowing text, so the tail (and, mid-sweep, the head) is simply
    /// not rasterized outside the window — no fade at the edges, which §5
    /// forbids anyway.
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree
            .state
            .downcast_ref::<MarqueeState<Renderer::Paragraph>>();
        let bounds = layout.bounds();

        // Entirely scrolled or clipped out of view: nothing to draw, and
        // `fill_paragraph` would take an empty clip anyway.
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };

        // The travel distance, recomputed from the measurement rather than
        // stored: the window and the text are both laid out above, so this
        // is always the *current* overflow, including the frame in which the
        // title changed length.
        let overflow = (state.paragraph.min_bounds().width - bounds.width).max(0.0);
        let offset = offset_at(self.elapsed, overflow, self.dwell, self.speed);

        renderer.fill_paragraph(
            state.paragraph.raw(),
            Point::new(bounds.x - offset, bounds.y),
            self.color,
            clip,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stock theme, used everywhere a test needs *some* `&Theme` to
    /// construct a module or call the timing math against. Reading
    /// `motion.marquee_dwell`/`marquee_speed` off this (below) rather than
    /// re-declaring the old literal constants is what these tests were
    /// switched to do when the theme grew tokens for them — see the module
    /// doc comment's "Where the timing numbers come from".
    fn test_theme() -> Theme {
        Theme::saola()
    }

    /// `motion.marquee_dwell`/`marquee_speed`, resolved once for the tests
    /// below that call [`phase_at`]/[`offset_at`] directly (rather than
    /// through a [`WindowTitle`], which caches them itself).
    fn test_motion() -> (Duration, f32) {
        let motion = test_theme().motion;
        (
            Duration::from_millis(u64::from(motion.marquee_dwell)),
            motion.marquee_speed,
        )
    }

    #[test]
    fn a_title_within_the_limit_is_untouched() {
        assert_eq!(truncate("nvim", 50), "nvim");
        // Exactly at the limit is still untouched — the cap is inclusive.
        assert_eq!(truncate("abcde", 5), "abcde");
    }

    #[test]
    fn an_overlong_title_is_cut_with_one_ellipsis() {
        assert_eq!(truncate("abcdef", 5), "abcd…");
        // The ellipsis is one character and lives *inside* the budget, so
        // the result is never longer than the limit.
        assert_eq!(truncate("abcdef", 5).chars().count(), 5);
        assert_eq!(truncate("abcdef", 5).matches('…').count(), 1);
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // Every one of these is multi-byte UTF-8; a byte slice at 5 would
        // land mid-codepoint and panic. Em dashes are ordinary in real
        // titles ("nvim — src/main.rs"), CJK and emoji less so but perfectly
        // legal.
        let em_dashes = "————————";
        assert_eq!(truncate(em_dashes, 5).chars().count(), 5);
        assert_eq!(truncate(em_dashes, 5), "————…");

        let cjk = "設定ウィンドウのタイトル";
        assert_eq!(truncate(cjk, 4), "設定ウ…");

        let emoji = "🦌🦌🦌🦌🦌";
        assert_eq!(truncate(emoji, 3), "🦌🦌…");

        // A cut that lands *between* a multi-byte character and an ASCII one
        // is a boundary too.
        assert_eq!(truncate("é-abc", 3), "é-…");
    }

    #[test]
    fn tiny_limits_degrade_to_the_ellipsis_alone() {
        assert_eq!(truncate("abcdef", 1), "…");
        // Not reachable from `panel.toml` (the parser rejects a non-positive
        // `max-chars`), but it must not panic or produce a stray ellipsis.
        assert_eq!(truncate("abcdef", 0), "");
    }

    /// The module's absent-service contract: no focused window → nothing to
    /// draw, and the presence seam agrees with it.
    #[test]
    fn no_title_means_nothing_is_present() {
        let module = WindowTitle::new(WindowTitleConfig::default(), &test_theme());
        assert!(!module.is_present());
    }

    /// Only a real title makes the module present — and `update` is the one
    /// way state ever changes.
    #[test]
    fn an_updated_title_becomes_present() {
        let mut module = WindowTitle::new(WindowTitleConfig::default(), &test_theme());
        module.update(Message::Updated(Some("Alacritty".to_owned())));
        assert!(module.is_present());
        assert_eq!(module.title.as_deref(), Some("Alacritty"));

        // Focus leaving every window clears it again.
        module.update(Message::Updated(None));
        assert!(!module.is_present());
    }

    /// A live config reload keeps the title on screen (the whole point of
    /// `set_config` over rebuilding with `new`), restarts an in-flight
    /// marquee run when the knobs actually changed, and leaves the run
    /// alone when they didn't (a reload triggered by some *other* knob in
    /// `panel.toml`).
    #[test]
    fn set_config_keeps_the_title_and_resets_the_sweep_only_on_change() {
        let mut module = showing(TitleOverflow::Marquee, 4, "a long overflowing title");
        module.update(Message::Tick(Instant::now()));
        assert!(module.sweep_epoch.is_some(), "the run is under way");

        // The same knobs again — an unrelated reload — must not reset it.
        module.set_config(
            WindowTitleConfig {
                max_chars: 4,
                overflow: TitleOverflow::Marquee,
            },
            &test_theme(),
        );
        assert!(module.sweep_epoch.is_some());

        // Changed knobs restart the run from its head dwell…
        module.set_config(
            WindowTitleConfig {
                max_chars: 8,
                overflow: TitleOverflow::Marquee,
            },
            &test_theme(),
        );
        assert_eq!(module.sweep_epoch, None);
        assert_eq!(module.sweep_elapsed, Duration::ZERO);
        // …and the title survives the swap.
        assert_eq!(module.title.as_deref(), Some("a long overflowing title"));
        assert_eq!(module.config.max_chars, 8);
    }

    // -- the marquee's window ----------------------------------------------

    /// A module in one of the two overflow modes, showing `title`.
    fn showing(overflow: TitleOverflow, max_chars: usize, title: &str) -> WindowTitle {
        let mut module = WindowTitle::new(
            WindowTitleConfig {
                max_chars,
                overflow,
            },
            &test_theme(),
        );
        module.update(Message::Updated(Some(title.to_owned())));
        module
    }

    /// Seconds as a `Duration`, for readable timeline assertions.
    fn at(seconds: f64) -> Duration {
        Duration::from_secs_f64(seconds)
    }

    #[test]
    fn the_window_is_max_chars_of_the_measured_title() {
        // Ten characters measuring 100 px: one character averages 10 px, so a
        // four-character window is 40 px wide and the text has 60 px to
        // travel.
        assert_eq!(window_width(100.0, 10, 4), 40.0);
        assert_eq!(window_width(100.0, 10, 10), 100.0);
        // Never wider than the text itself — a budget larger than the title
        // is a window the whole title already fits in, not empty space the
        // module would reserve.
        assert_eq!(window_width(100.0, 10, 40), 100.0);
        // An empty string has no advance to average; zero width, no travel.
        assert_eq!(window_width(0.0, 0, 50), 0.0);
    }

    #[test]
    fn the_pixel_window_and_the_character_gate_agree() {
        // The property that lets the subscription gate on characters while
        // the sweep runs on pixels: the window is narrower than the text
        // exactly when the title is longer than the budget, for any measured
        // width. Anything else would let a timer tick with nothing moving.
        for chars in 1..40usize {
            for max_chars in 1..40usize {
                let full = 7.25 * chars as f32; // any average advance
                let window = window_width(full, chars, max_chars);
                assert_eq!(
                    window < full,
                    chars > max_chars,
                    "{chars} chars in a {max_chars}-char window"
                );
            }
        }
    }

    // -- the state machine -------------------------------------------------

    /// 48 px of travel is exactly 2 s of sweep at the §5 rate
    /// (`motion.marquee_speed`, 24 px/s in the stock theme), which makes
    /// every boundary in the cycle a whole second: dwell 0–2, sweep 2–4,
    /// dwell 4–6, sweep back 6–8.
    const TRAVEL: f32 = 48.0;

    #[test]
    fn the_cycle_runs_head_sweep_tail_sweep_and_repeats() {
        let (dwell, speed) = test_motion();

        // Dwell at the head, parked at the start of the title.
        assert_eq!(phase_at(at(0.0), TRAVEL, dwell, speed), Phase::DwellHead);
        assert_eq!(offset_at(at(0.0), TRAVEL, dwell, speed), 0.0);
        assert_eq!(phase_at(at(1.999), TRAVEL, dwell, speed), Phase::DwellHead);
        assert_eq!(offset_at(at(1.999), TRAVEL, dwell, speed), 0.0);

        // Sweeping left, linearly: half the sweep is half the travel.
        assert_eq!(
            phase_at(at(2.0), TRAVEL, dwell, speed),
            Phase::SweepLeft { progress: 0.0 }
        );
        assert_eq!(offset_at(at(2.0), TRAVEL, dwell, speed), 0.0);
        assert_eq!(
            phase_at(at(3.0), TRAVEL, dwell, speed),
            Phase::SweepLeft { progress: 0.5 }
        );
        assert_eq!(offset_at(at(3.0), TRAVEL, dwell, speed), 24.0);

        // Dwell at the tail, with the end of the title fully visible.
        assert_eq!(phase_at(at(4.0), TRAVEL, dwell, speed), Phase::DwellTail);
        assert_eq!(offset_at(at(4.0), TRAVEL, dwell, speed), TRAVEL);
        assert_eq!(phase_at(at(5.999), TRAVEL, dwell, speed), Phase::DwellTail);
        assert_eq!(offset_at(at(5.999), TRAVEL, dwell, speed), TRAVEL);

        // Sweeping back the same way, at the same rate.
        assert_eq!(
            phase_at(at(6.0), TRAVEL, dwell, speed),
            Phase::SweepRight { progress: 0.0 }
        );
        assert_eq!(offset_at(at(6.0), TRAVEL, dwell, speed), TRAVEL);
        assert_eq!(
            phase_at(at(7.0), TRAVEL, dwell, speed),
            Phase::SweepRight { progress: 0.5 }
        );
        assert_eq!(offset_at(at(7.0), TRAVEL, dwell, speed), 24.0);

        // And straight back into the head dwell — the loop closes with no
        // pause of its own beyond the two specced ones.
        assert_eq!(phase_at(at(8.0), TRAVEL, dwell, speed), Phase::DwellHead);
        assert_eq!(offset_at(at(8.0), TRAVEL, dwell, speed), 0.0);
        // A second lap is the first lap, exactly (and a hundredth is too —
        // the modulo happens in f64, so a long-focused window doesn't drift).
        assert_eq!(
            phase_at(at(11.0), TRAVEL, dwell, speed),
            Phase::SweepLeft { progress: 0.5 }
        );
        assert_eq!(offset_at(at(803.0), TRAVEL, dwell, speed), 24.0);
    }

    #[test]
    fn the_sweep_takes_its_time_from_the_distance_not_a_duration() {
        let (dwell, speed) = test_motion();

        // 24 px/s is a rate: half the travel is half the sweep, so the whole
        // cycle is shorter for a title that only just overflows. 24 px = 1 s
        // each way, so this cycle is 6 s rather than 8.
        assert_eq!(
            phase_at(at(2.5), 24.0, dwell, speed),
            Phase::SweepLeft { progress: 0.5 }
        );
        assert_eq!(offset_at(at(2.5), 24.0, dwell, speed), 12.0);
        assert_eq!(phase_at(at(3.0), 24.0, dwell, speed), Phase::DwellTail);
        assert_eq!(phase_at(at(6.0), 24.0, dwell, speed), Phase::DwellHead);

        // A very long title takes proportionally longer to cross, and is
        // still dwelling at its head for exactly 2 s first.
        assert_eq!(phase_at(at(1.9), 2400.0, dwell, speed), Phase::DwellHead);
        assert_eq!(offset_at(at(52.0), 2400.0, dwell, speed), 1200.0);
    }

    #[test]
    fn the_offset_never_leaves_the_travel_and_the_sweeps_are_linear() {
        let (dwell, speed) = test_motion();
        // The frame cadence (`motion.frame`) the real subscription ticks at
        // — used below only to pick a realistic sampling step, not because
        // `offset_at` itself depends on it.
        let frame_tick = Duration::from_millis(u64::from(test_theme().motion.frame));

        // Sampled at the real frame rate across two full cycles: the text is
        // never pulled past either end, and each sweep advances by the same
        // distance every frame (linear, per §5 — no easing anywhere).
        let mut previous = 0.0f32;
        let mut elapsed = Duration::ZERO;
        while elapsed < at(16.0) {
            let offset = offset_at(elapsed, TRAVEL, dwell, speed);
            assert!(
                (0.0..=TRAVEL).contains(&offset),
                "offset left the travel at {elapsed:?}: {offset}"
            );
            // Nothing ever jumps more than a frame's worth of travel.
            assert!(
                (offset - previous).abs() <= speed * frame_tick.as_secs_f32() + 1e-3,
                "offset jumped at {elapsed:?}: {previous} → {offset}"
            );
            previous = offset;
            elapsed += frame_tick;
        }

        // Linearity, stated directly: equal time steps inside one sweep cover
        // equal ground (the breath's cosine ease is the *other* animation).
        let first =
            offset_at(at(2.5), TRAVEL, dwell, speed) - offset_at(at(2.0), TRAVEL, dwell, speed);
        let second =
            offset_at(at(3.0), TRAVEL, dwell, speed) - offset_at(at(2.5), TRAVEL, dwell, speed);
        assert!((first - second).abs() < 1e-4, "{first} vs {second}");
    }

    #[test]
    fn nothing_moves_when_there_is_nothing_to_reveal() {
        let (dwell, speed) = test_motion();

        // A title that fits has no travel, so the machine parks at the head
        // for all time rather than sweeping zero pixels back and forth.
        for seconds in [0.0, 2.0, 4.5, 900.0] {
            assert_eq!(phase_at(at(seconds), 0.0, dwell, speed), Phase::DwellHead);
            assert_eq!(offset_at(at(seconds), 0.0, dwell, speed), 0.0);
            // Negative travel (a window wider than its text) is the same
            // case, and a degenerate measurement must not produce a NaN
            // offset either.
            assert_eq!(offset_at(at(seconds), -12.0, dwell, speed), 0.0);
            assert_eq!(offset_at(at(seconds), f32::NAN, dwell, speed), 0.0);
        }
    }

    // -- the gate ----------------------------------------------------------

    #[test]
    fn truncate_mode_never_animates() {
        // The default mode has no loop at all, however long the title: this
        // is what keeps a stock Saola desktop at exactly one animation
        // (CLAUDE.md; style guide §5).
        let module = showing(TitleOverflow::Truncate, 5, "a title far past the limit");
        assert!(module.overflows());
        assert!(!module.is_marqueeing());
    }

    #[test]
    fn the_marquee_ticks_only_while_an_overflowing_title_is_on_screen() {
        // Opted in and overflowing: the one case that runs a timer.
        let mut module = showing(TitleOverflow::Marquee, 5, "abcdefgh");
        assert!(module.is_marqueeing());

        // A title within its budget — nothing to reveal, so nothing ticks.
        module.update(Message::Updated(Some("abc".to_owned())));
        assert!(!module.is_marqueeing());

        // Exactly at the limit is *within* it: the cap is inclusive, the
        // same boundary truncate's own tests pin.
        module.update(Message::Updated(Some("abcde".to_owned())));
        assert!(!module.is_marqueeing());

        // One character past it is the first case that moves.
        module.update(Message::Updated(Some("abcdef".to_owned())));
        assert!(module.is_marqueeing());

        // Focus leaving every window stops it too, with no separate path:
        // there is no title, so there is nothing overflowing.
        module.update(Message::Updated(None));
        assert!(!module.is_marqueeing());
    }

    #[test]
    fn a_marquee_title_that_fits_is_left_alone() {
        // The view's fallback for the fits case is truncate's, and truncate
        // is a no-op within the budget — so the text on screen is the plain
        // title, not a capped one.
        let module = showing(TitleOverflow::Marquee, 50, "nvim — src/main.rs");
        assert!(!module.is_marqueeing());
        assert_eq!(
            truncate(module.title.as_deref().unwrap(), 50),
            "nvim — src/main.rs"
        );
    }

    // -- the animation clock -----------------------------------------------

    #[test]
    fn the_first_tick_establishes_the_epoch() {
        let mut module = showing(TitleOverflow::Marquee, 5, "abcdefgh");
        let start = Instant::now();

        module.update(Message::Tick(start));
        assert_eq!(module.sweep_epoch, Some(start));
        assert_eq!(module.sweep_elapsed, Duration::ZERO);

        module.update(Message::Tick(start + at(2.5)));
        assert_eq!(module.sweep_elapsed, at(2.5));

        // Elapsed is measured from the epoch, never accumulated per tick — a
        // late tick (or a run of dropped ones) reports real elapsed time.
        module.update(Message::Tick(start + at(30.0)));
        assert_eq!(module.sweep_elapsed, at(30.0));
    }

    #[test]
    fn a_new_title_restarts_at_the_head_dwell() {
        let mut module = showing(TitleOverflow::Marquee, 5, "abcdefgh");
        let start = Instant::now();
        module.update(Message::Tick(start));
        module.update(Message::Tick(start + at(3.0)));
        assert_eq!(module.sweep_elapsed, at(3.0));

        // Focus moves to another window: its title is new text, and showing
        // its middle first would be nonsense.
        module.update(Message::Updated(Some("a different long title".to_owned())));
        assert_eq!(module.sweep_epoch, None);
        assert_eq!(module.sweep_elapsed, Duration::ZERO);
        assert_eq!(
            offset_at(
                module.sweep_elapsed,
                TRAVEL,
                module.marquee_dwell,
                module.marquee_speed
            ),
            0.0
        );

        // The next run establishes its own epoch from its own first tick.
        let restart = start + at(60.0);
        module.update(Message::Tick(restart));
        assert_eq!(module.sweep_epoch, Some(restart));
        assert_eq!(module.sweep_elapsed, Duration::ZERO);
    }

    #[test]
    fn a_repeat_of_the_same_title_does_not_restart_the_sweep() {
        // The niri bridge already suppresses these; if one ever got through,
        // it must not yank the text back to the head mid-sweep.
        let mut module = showing(TitleOverflow::Marquee, 5, "abcdefgh");
        let start = Instant::now();
        module.update(Message::Tick(start));
        module.update(Message::Tick(start + at(3.0)));

        module.update(Message::Updated(Some("abcdefgh".to_owned())));
        assert_eq!(module.sweep_epoch, Some(start));
        assert_eq!(module.sweep_elapsed, at(3.0));
    }

    #[test]
    fn a_title_that_stops_overflowing_stops_the_animation_immediately() {
        let mut module = showing(TitleOverflow::Marquee, 5, "abcdefgh");
        let start = Instant::now();
        module.update(Message::Tick(start));
        module.update(Message::Tick(start + at(3.0)));
        assert!(module.is_marqueeing());

        // A shorter title closes the gate — the subscription goes back to
        // `none()` on the next recomputation — and the stored phase is put
        // away with it, so a later overflowing title starts from its head
        // rather than resuming this one's position.
        module.update(Message::Updated(Some("nvim".to_owned())));
        assert!(!module.is_marqueeing());
        assert_eq!(module.sweep_epoch, None);
        assert_eq!(module.sweep_elapsed, Duration::ZERO);
    }
}
