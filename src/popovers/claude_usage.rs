//! The Claude Code usage popover's content — [`crate::popover::PopoverKind::
//! ClaudeUsage`]'s real body: one row per tracked session (the same row, in
//! the same order, as the bar's status dots), each showing the session's
//! dot, a shortened id, its status, the model it last used, and the token
//! totals summed from its transcript, with an aggregate line underneath —
//! and, below a separator, the account's two rate-limit gauges (5-hour and
//! 7-day windows) when a fresh snapshot is in hand (see "The rate-limit
//! gauges" below).
//!
//! Same split as `crate::popovers::tray_menu`: the lifecycle manager
//! (`crate::popover::PopoverManager`) stays ignorant of what a popover
//! contains, and this module owns the content plus the state it needs —
//! held on [`crate::Panel`] as [`ClaudeUsageState`], the "a module defines
//! the type, `Panel` holds an instance" shape `TrayMenuState` established.
//!
//! # Where the numbers come from (and when)
//!
//! Claude Code appends one JSON object per line to each session's
//! transcript file; every assistant turn's line carries a
//! `message.usage` object (`input_tokens`, `output_tokens`,
//! `cache_read_input_tokens`, `cache_creation_input_tokens`) and a
//! `message.model`. [`fold_transcript`] sums those — nothing more; there is
//! deliberately no cost estimation here, because pricing tables go stale
//! and a wrong dollar figure is worse than none.
//!
//! The transcript path arrives on the `StatusChanged` signal itself (the
//! third argument `emit.sh` now sends — see `modules::claude`'s bus
//! schema), and the files are read **once per popover open**, inside the
//! one-shot `Task` `Panel::open_claude_usage` kicks off on the click. No
//! file watcher, no refresh timer: CLAUDE.md's "every module maps to a
//! signal, never a poll" rule stands — a click is an event, and the popover
//! shows the moment it was opened. (Reopening it re-reads, which is the
//! refresh.) The reads themselves run under `tokio::task::spawn_blocking`,
//! the same way the tray's freedesktop icon lookup keeps synchronous
//! filesystem I/O off the async workers.
//!
//! # The rate-limit gauges (2026-08-01)
//!
//! Unlike the token totals, the rate-limit numbers don't come from a file
//! read at click time: they arrive on the bus as `UsageChanged` signals
//! (emitted by `contrib/claude-code/statusline.sh` — see `modules::claude`'s
//! schema section) and sit on the Claude Code module's state as one
//! account-wide [`crate::modules::claude::Usage`] snapshot, which `main.rs`
//! hands [`view`] by value. Two [`saola_theme::style::progress::bar`]
//! gauges render it — the theme's existing progress primitive, terracotta
//! fill on the ink track, nothing invented here.
//!
//! **Staleness**: a snapshot only ever describes the moment the statusline
//! last refreshed, and nothing re-emits after Claude Code exits — so a
//! snapshot whose `resets_at` is already in the past is describing a
//! window that no longer exists. [`view`] reads `Local::now()` once per
//! render (the `modules::clock` pattern: the clock read at the edge, the
//! decision in a pure function — [`is_stale`]) and *drops* the gauges for
//! a stale snapshot rather than graying them out: the theme's progress bar
//! deliberately has no quiet/disabled variant (a gauge is never "at rest"),
//! and absence-is-silent is already this module's idiom for every other
//! missing number. No countdown, no tick — the popover shows the moment it
//! was opened, and reopening is the refresh, exactly as for the transcript
//! sums.
//!
//! # Flagged: the popover's height is a fixed row budget
//!
//! Same limitation as `tray_menu::height`, same reason: the surface's size
//! must be declared at click time, before the transcript reads have even
//! started — and before anything could count how many sessions will fit.
//! [`BUDGETED_SESSIONS`] rows are budgeted; a busier session list than that
//! has its later rows pushed below the visible area (genuinely lost, not
//! clipped-with-scrollbar — see `tray_menu`'s doc comment for the shared
//! analysis and the eventual fixes).

use chrono::{DateTime, Local, Utc};
use iced::widget::{column, container, progress_bar, row, rule, text, Space};
use iced::{Element, Fill};
use saola_theme::convert::ColorExt;
use saola_theme::style::container::SessionStatus;
use saola_theme::{style, Surface, Theme};

use crate::modules::claude::{Usage, UsageTarget, UsageWindow};

/// The usage popover's own messages — nested as `Message::ClaudeUsage(..)`
/// in `main.rs`'s panel-level enum. One variant: the read-everything task
/// answering. There is nothing to interact with *inside* this popover (it
/// is a readout, not a control surface), so unlike `tray_menu::Message`
/// there are no row-click variants.
#[derive(Debug, Clone)]
pub enum Message {
    /// `read_usage` answered with one entry per session that was tracked at
    /// click time. Applied by `Panel::update` only while the usage popover
    /// is still the open one, so a read that lands after dismissal doesn't
    /// resurrect stale state.
    Loaded(Vec<SessionUsage>),
}

/// One session's row: the bar-side coordinates it was requested with, plus
/// what the transcript said — `None` when the session never reported a
/// transcript path (a legacy emitter), the file couldn't be read, or no
/// line in it carried a `message.usage` object yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUsage {
    pub target: UsageTarget,
    pub usage: Option<TranscriptUsage>,
}

/// Token totals summed over every `message.usage` object in one transcript,
/// plus the model the last such line named. Cache reads/writes are kept
/// separate from plain input on purpose: cached tokens dominate any real
/// session's totals and would drown the interesting number if merged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// How many usage-carrying lines were summed — assistant turns, in
    /// practice.
    pub replies: u64,
    pub model: Option<String>,
}

/// Everything the usage popover needs beyond the theme: whether the
/// per-session reads are still in flight, and their results once landed.
/// Lives on `Panel` (see the module doc comment); reset to
/// [`Self::opening`] on every trigger click, so a reopened popover never
/// shows the previous open's numbers as if they were fresh.
#[derive(Debug, Default)]
pub struct ClaudeUsageState {
    loading: bool,
    sessions: Vec<SessionUsage>,
}

impl ClaudeUsageState {
    /// A fresh "requested, not answered yet" state — set the instant the
    /// trigger is clicked, before the read task has started, so [`view`]
    /// has a quiet loading line for the surface's very first frame.
    pub fn opening() -> Self {
        Self {
            loading: true,
            sessions: Vec::new(),
        }
    }

    /// Store the read task's answer.
    pub fn set_loaded(&mut self, sessions: Vec<SessionUsage>) {
        self.loading = false;
        self.sessions = sessions;
    }

    /// The loaded rows — exposed so `main.rs`'s `Panel`-level tests can
    /// assert on fetch results without reaching into a private field.
    pub fn sessions(&self) -> &[SessionUsage] {
        &self.sessions
    }
}

/// How many session rows the popover surface budgets space for — a fixed
/// count, for the same declare-before-you-know reason as
/// `tray_menu::BUDGETED_ROWS` (see the module doc comment). Six comfortably
/// holds Jordan's "a handful of sessions, not thousands" working set.
const BUDGETED_SESSIONS: f32 = 6.0;

/// The popover surface's declared height: content padding, a header line,
/// the budgeted session rows, the totals line, and the two rate-limit
/// gauge rows (each separator rides in the same `island_gap` allowance
/// `tray_menu`'s separators use).
///
/// The gauge rows are budgeted *unconditionally* — the surface's size is
/// declared before anyone knows whether a fresh usage snapshot exists (the
/// same declare-before-you-know constraint as the session rows), so a
/// popover with no gauges to show simply has their two rows of blank ink
/// at the bottom. The known-acceptable cost: "a mismatch only costs some
/// blank ink, never a panic or clipped content" (`PopoverKind::height`).
pub fn height(theme: &Theme) -> f32 {
    theme.sizes.popover_padding * 2.0
        + theme.sizes.list_row * (BUDGETED_SESSIONS + 4.0)
        + theme.sizes.island_gap * 2.0
}

/// Read every target's transcript and sum its usage — the body of the
/// one-shot `Task` `Panel::open_claude_usage` spawns on the trigger click.
///
/// Files are read via `spawn_blocking` (synchronous filesystem I/O does not
/// belong on the async executor's worker threads — the same reasoning as
/// the tray's icon lookup), one at a time: the list is a handful of local
/// files, and reading them concurrently would buy nothing measurable.
/// Every failure path — no transcript reported, unreadable file, a join
/// error — degrades to that one row's `usage: None`, never to a missing
/// row or a failed task.
pub async fn read_usage(targets: Vec<UsageTarget>) -> Vec<SessionUsage> {
    let mut sessions = Vec::with_capacity(targets.len());
    for target in targets {
        let usage = match target.transcript.clone() {
            Some(path) => tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok())
                .await
                .ok()
                .flatten()
                .and_then(|text| fold_transcript(&text)),
            None => None,
        };
        sessions.push(SessionUsage { target, usage });
    }
    sessions
}

/// Sum one transcript's usage lines. `None` when no line carried a
/// `message.usage` object at all — a brand-new session whose first turn
/// hasn't finished, or a file that isn't actually a transcript.
///
/// Pure function of the file's text (no I/O, no clock) — unit-tested below.
/// Lines that fail to parse as JSON, or parse but carry no usage object,
/// are skipped rather than aborting the sum: a transcript is an append-only
/// log that may legitimately end mid-write, and one torn line shouldn't
/// blank the whole session's numbers.
fn fold_transcript(text: &str) -> Option<TranscriptUsage> {
    let mut totals = TranscriptUsage::default();
    let mut saw_usage = false;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(usage) = value.pointer("/message/usage") else {
            continue;
        };
        let count = |field: &str| usage.get(field).and_then(|v| v.as_u64()).unwrap_or(0);
        totals.input_tokens += count("input_tokens");
        totals.output_tokens += count("output_tokens");
        totals.cache_read_tokens += count("cache_read_input_tokens");
        totals.cache_creation_tokens += count("cache_creation_input_tokens");
        totals.replies += 1;
        if let Some(model) = value.pointer("/message/model").and_then(|v| v.as_str()) {
            totals.model = Some(model.to_string());
        }
        saw_usage = true;
    }

    saw_usage.then_some(totals)
}

/// The whole popover body: header, one row per session, and the aggregate
/// line — or the quiet loading line while the reads are in flight — plus,
/// when a fresh rate-limit snapshot is in hand, the two gauge rows under
/// their own separator. The gauges don't wait on `state.loading`: the
/// snapshot arrived by signal long before the click, so they render from
/// the first frame even while the transcript reads are still in flight.
pub fn view<'a>(
    theme: &Theme,
    state: &'a ClaudeUsageState,
    usage: Option<Usage>,
) -> Element<'a, crate::Message> {
    // Read once per render and handed down to pure functions — the
    // `modules::clock` testability pattern (`view` owns the `Local::now()`
    // call; everything that *decides* takes the timestamp as an argument).
    let now = Local::now();

    let sessions: Element<'a, crate::Message> = if state.loading {
        quiet_line(theme, "Reading transcripts…")
    } else if state.sessions().is_empty() {
        // Only reachable if every session ended between the click and the
        // read answering — the trigger doesn't render without sessions.
        quiet_line(theme, "No sessions")
    } else {
        let mut lines: Vec<Element<'a, crate::Message>> = vec![header(theme)];
        for session in state.sessions() {
            lines.push(session_row(theme, session));
        }
        lines.push(separator(theme));
        lines.push(totals_row(theme, state.sessions()));
        column(lines).into()
    };

    let mut lines: Vec<Element<'a, crate::Message>> = vec![sessions];
    if let Some(usage) = usage {
        if !is_stale(&usage, now.timestamp()) {
            lines.push(separator(theme));
            lines.push(gauge_row(theme, "5-hour", usage.five_hour, now));
            lines.push(gauge_row(theme, "7-day", usage.seven_day, now));
        }
    }

    container(column(lines))
        .style(style::container::popover(theme))
        .padding(theme.sizes.popover_padding)
        .width(Fill)
        .height(Fill)
        .into()
}

/// Whether a snapshot is too old to show: the moment *either* window's
/// declared reset passes, the snapshot predates a reset and its
/// percentages describe windows that no longer exist. In practice the
/// five-hour window always expires first, so this is effectively "is the
/// snapshot from before the last five-hour reset" — exactly the "last
/// session ended hours ago" case. `<=` rather than `<`: at the reset
/// instant itself the old percentage is already obsolete.
///
/// Pure function of its arguments (the clock read lives in [`view`]), so
/// the render decision is unit-testable below.
fn is_stale(usage: &Usage, now_epoch: i64) -> bool {
    let passed = |window: &UsageWindow| {
        i64::try_from(window.resets_at).is_ok_and(|resets_at| resets_at <= now_epoch)
    };
    passed(&usage.five_hour) || passed(&usage.seven_day)
}

/// One window's gauge line: the window's name quietly on the left, the
/// theme's progress bar filling the middle (terracotta on the ink track —
/// `style::progress::bar`, the existing primitive), and the percentage
/// with its absolute reset time on the right. The bar's girth is
/// `dash_height` — the same 16 px vocabulary as the dots above it, closed
/// into a pill by the style's own `radii.pill`.
fn gauge_row<'a>(
    theme: &Theme,
    label: &'static str,
    window: UsageWindow,
    now: DateTime<Local>,
) -> Element<'a, crate::Message> {
    let text_size = theme.typography.size.bar;

    let caption = match reset_local(window.resets_at) {
        Some(reset) => format!("{:.0}% · {}", window.used_pct, format_reset(reset, now)),
        // Unrepresentable epoch (only a hand-typed emit can produce one):
        // keep the percentage, drop the time claim.
        None => format!("{:.0}%", window.used_pct),
    };

    container(
        row![
            text(label)
                .size(text_size)
                .color(theme.on_ink.secondary.into_iced()),
            progress_bar(0.0..=100.0, window.used_pct as f32)
                .girth(theme.sizes.dash_height)
                .style(style::progress::bar(theme, Surface::Ink)),
            text(caption)
                .size(text_size)
                .color(theme.on_ink.primary.into_iced()),
        ]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center),
    )
    .height(theme.sizes.list_row)
    .align_y(iced::Center)
    .into()
}

/// A window's reset instant as local wall-clock time. `None` only for an
/// epoch value outside chrono's representable range — which no genuine
/// Claude Code payload produces, but the wire is `t` (u64) and a
/// hand-typed `busctl` call can put anything there.
fn reset_local(resets_at: u64) -> Option<DateTime<Local>> {
    let secs = i64::try_from(resets_at).ok()?;
    Some(DateTime::<Utc>::from_timestamp(secs, 0)?.with_timezone(&Local))
}

/// `"resets 14:00"` — absolute local time, no countdown (a countdown needs
/// a tick; an absolute time is true for as long as the popover is open).
/// A reset on a different calendar day (routine for the 7-day window)
/// gains the weekday: `"resets Thu 09:00"` — a bare clock time would
/// silently read as "later today".
fn format_reset(reset: DateTime<Local>, now: DateTime<Local>) -> String {
    if reset.date_naive() == now.date_naive() {
        format!("resets {}", reset.format("%H:%M"))
    } else {
        format!("resets {}", reset.format("%a %H:%M"))
    }
}

/// A single quiet line in the `secondary` role, holding a `list_row` slot —
/// the same placeholder shape `quick_settings::quiet_row` uses.
fn quiet_line<'a>(theme: &Theme, label: &'static str) -> Element<'a, crate::Message> {
    container(
        text(label)
            .size(theme.typography.size.bar)
            .color(theme.on_ink.secondary.into_iced()),
    )
    .height(theme.sizes.list_row)
    .align_y(iced::Center)
    .into()
}

/// The header line: the module's name in primary ivory, with the column
/// meaning spelled out quietly on the right so the per-row numbers don't
/// need their own labels.
fn header<'a>(theme: &Theme) -> Element<'a, crate::Message> {
    container(
        row![
            text("Claude Code")
                .size(theme.typography.size.bar)
                .color(theme.on_ink.primary.into_iced()),
            Space::new().width(Fill),
            text("tokens in / out")
                .size(theme.typography.size.bar)
                .color(theme.on_ink.secondary.into_iced()),
        ]
        .align_y(iced::Center),
    )
    .height(theme.sizes.list_row)
    .align_y(iced::Center)
    .into()
}

/// One session's line: its dot (the same `status_dot` the bar draws, at
/// full opacity — the popover doesn't breathe), the shortened id in primary
/// ivory, status and model quietly beside it, and the token pair
/// right-aligned. A session with no readable usage gets a quiet "no usage
/// data" in the numbers column instead — the row itself always renders, so
/// the popover's list always matches the bar's dots one for one.
fn session_row<'a>(theme: &Theme, session: &'a SessionUsage) -> Element<'a, crate::Message> {
    let text_size = theme.typography.size.bar;

    let mut annotation = status_label(session.target.status).to_string();
    if let Some(model) = session
        .usage
        .as_ref()
        .and_then(|usage| usage.model.as_deref())
    {
        annotation.push_str(" · ");
        annotation.push_str(&short_model(model));
    }

    let numbers: Element<'a, crate::Message> = match &session.usage {
        Some(usage) => text(format!(
            "{} / {}",
            format_tokens(
                usage.input_tokens + usage.cache_read_tokens + usage.cache_creation_tokens
            ),
            format_tokens(usage.output_tokens),
        ))
        .size(text_size)
        .color(theme.on_ink.primary.into_iced())
        .into(),
        None => text("no usage data")
            .size(text_size)
            .color(theme.on_ink.secondary.into_iced())
            .into(),
    };

    container(
        row![
            container(Space::new())
                .width(theme.sizes.dash_height)
                .height(theme.sizes.dash_height)
                .style(style::container::status_dot(
                    theme,
                    session.target.status,
                    1.0,
                )),
            text(short_id(&session.target.id))
                .size(text_size)
                .color(theme.on_ink.primary.into_iced()),
            text(annotation)
                .size(text_size)
                .color(theme.on_ink.secondary.into_iced()),
            Space::new().width(Fill),
            numbers,
        ]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center),
    )
    .height(theme.sizes.list_row)
    .align_y(iced::Center)
    .into()
}

/// The aggregate line under the separator: every session's totals summed,
/// with the cache volume called out separately (it dominates the raw input
/// number and deserves to be seen as what it is).
fn totals_row<'a>(theme: &Theme, sessions: &[SessionUsage]) -> Element<'a, crate::Message> {
    let text_size = theme.typography.size.bar;

    let mut input = 0u64;
    let mut output = 0u64;
    let mut cached = 0u64;
    for usage in sessions.iter().filter_map(|session| session.usage.as_ref()) {
        input += usage.input_tokens + usage.cache_read_tokens + usage.cache_creation_tokens;
        output += usage.output_tokens;
        cached += usage.cache_read_tokens;
    }

    container(
        row![
            text("Total")
                .size(text_size)
                .color(theme.on_ink.primary.into_iced()),
            text(format!("{} cached", format_tokens(cached)))
                .size(text_size)
                .color(theme.on_ink.secondary.into_iced()),
            Space::new().width(Fill),
            text(format!(
                "{} / {}",
                format_tokens(input),
                format_tokens(output)
            ))
            .size(text_size)
            .color(theme.on_ink.primary.into_iced()),
        ]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center),
    )
    .height(theme.sizes.list_row)
    .align_y(iced::Center)
    .into()
}

/// A divider — identical to `tray_menu::separator` (the same flagged
/// `island_gap` reservation; promoting a dedicated token is that module's
/// already-recorded `saola-theme` candidate, not a new one here).
fn separator<'a>(theme: &Theme) -> Element<'a, crate::Message> {
    container(rule::horizontal(1.0).style(style::rule::rest(theme, Surface::Ink)))
        .padding(iced::padding::vertical(theme.sizes.island_gap / 2.0))
        .width(Fill)
        .into()
}

/// Session ids are UUIDs; the first block (eight characters) is plenty to
/// tell a handful of concurrent sessions apart, and matches how the id
/// reads in Claude Code's own transcript filenames.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The human word for each dot — the theme's own enum, spelled out.
/// Exhaustive on purpose: a sixth status added to saola-theme becomes a
/// compile error here, same as `modules::claude::breathes`.
fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Working => "working",
        SessionStatus::Subagents => "subagents",
        SessionStatus::Attention => "needs attention",
        SessionStatus::Done => "done",
        SessionStatus::Idle => "idle",
    }
}

/// `"claude-fable-5"` → `"fable-5"`; `"claude-haiku-4-5-20251001"` →
/// `"haiku-4-5"`. Strips the constant `claude-` prefix and a trailing
/// datestamp segment, leaving the part that distinguishes models from one
/// another. An id that matches neither pattern passes through unchanged —
/// showing a verbose id beats hiding an unrecognized one.
fn short_model(model: &str) -> String {
    let trimmed = model.strip_prefix("claude-").unwrap_or(model);
    match trimmed.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.starts_with("20") => head.to_string(),
        _ => trimmed.to_string(),
    }
}

/// Token counts read at a glance: exact below a thousand, one decimal of
/// `k`/`M` above. The popover is a gauge, not an invoice — trailing
/// precision would just be noise at bar-text size.
fn format_tokens(count: u64) -> String {
    if count < 1_000 {
        count.to_string()
    } else if count < 1_000_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str, status: SessionStatus) -> UsageTarget {
        UsageTarget {
            id: id.to_string(),
            status,
            transcript: None,
        }
    }

    #[test]
    fn height_is_positive_and_derived_from_tokens() {
        let theme = Theme::saola();
        assert!(height(&theme) > 0.0);
    }

    #[test]
    fn view_renders_without_panicking_while_loading() {
        let theme = Theme::saola();
        let state = ClaudeUsageState::opening();
        let _: Element<'_, crate::Message> = view(&theme, &state, None);
    }

    #[test]
    fn view_renders_without_panicking_with_rows() {
        let theme = Theme::saola();
        let mut state = ClaudeUsageState::opening();
        state.set_loaded(vec![
            SessionUsage {
                target: target("aaaaaaaa-1111", SessionStatus::Working),
                usage: Some(TranscriptUsage {
                    input_tokens: 1200,
                    output_tokens: 400,
                    cache_read_tokens: 90_000,
                    cache_creation_tokens: 5_000,
                    replies: 3,
                    model: Some("claude-fable-5".to_string()),
                }),
            },
            SessionUsage {
                target: target("bbbbbbbb-2222", SessionStatus::Idle),
                usage: None,
            },
        ]);
        // With a fresh snapshot beside the rows (`u64::MAX` resets are
        // far-future but unrepresentable as local times, so this also
        // walks the caption's no-time fallback; the fresh-path caption is
        // covered by the rendering test below with sane epochs).
        let _: Element<'_, crate::Message> = view(
            &theme,
            &state,
            Some(Usage {
                five_hour: UsageWindow {
                    used_pct: 23.5,
                    resets_at: u64::MAX,
                },
                seven_day: UsageWindow {
                    used_pct: 41.2,
                    resets_at: u64::MAX,
                },
            }),
        );
    }

    /// Two assistant lines and the noise around them: a user line (no
    /// usage), a torn trailing line (invalid JSON) — only the usage lines
    /// count, and the *last* one's model wins.
    #[test]
    fn fold_transcript_sums_usage_lines_and_skips_the_rest() {
        let transcript = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-fable-5","usage":{"input_tokens":10,"output_tokens":50,"cache_read_input_tokens":1000,"cache_creation_input_tokens":200}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","usage":{"input_tokens":5,"output_tokens":25,"cache_read_input_tokens":1500,"cache_creation_input_tokens":0}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-fab"#,
        );

        let usage = fold_transcript(transcript).expect("two usage lines present");

        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.output_tokens, 75);
        assert_eq!(usage.cache_read_tokens, 2500);
        assert_eq!(usage.cache_creation_tokens, 200);
        assert_eq!(usage.replies, 2);
        assert_eq!(usage.model.as_deref(), Some("claude-haiku-4-5-20251001"));
    }

    #[test]
    fn fold_transcript_with_no_usage_lines_is_none() {
        assert_eq!(fold_transcript(""), None);
        assert_eq!(
            fold_transcript(r#"{"type":"user","message":{"role":"user"}}"#),
            None
        );
        assert_eq!(fold_transcript("not json at all"), None);
    }

    /// A usage object with missing fields (older transcript formats) sums
    /// what it has rather than being skipped.
    #[test]
    fn fold_transcript_tolerates_missing_usage_fields() {
        let line = r#"{"message":{"usage":{"output_tokens":7}}}"#;
        let usage = fold_transcript(line).expect("a usage line, if a sparse one");
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.replies, 1);
        assert_eq!(usage.model, None);
    }

    #[test]
    fn short_ids_are_the_uuids_first_block() {
        assert_eq!(short_id("a1b2c3d4-e5f6-7890"), "a1b2c3d4");
        assert_eq!(short_id("tiny"), "tiny");
    }

    #[test]
    fn short_models_drop_the_prefix_and_datestamp() {
        assert_eq!(short_model("claude-fable-5"), "fable-5");
        assert_eq!(short_model("claude-haiku-4-5-20251001"), "haiku-4-5");
        assert_eq!(short_model("claude-opus-5"), "opus-5");
        assert_eq!(short_model("some-future-id"), "some-future-id");
    }

    #[test]
    fn token_counts_format_at_a_glance() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(45_600), "45.6k");
        assert_eq!(format_tokens(1_230_000), "1.2M");
    }

    // -- the rate-limit gauges ---------------------------------------------

    fn usage(five_hour_resets_at: u64, seven_day_resets_at: u64) -> Usage {
        Usage {
            five_hour: UsageWindow {
                used_pct: 23.5,
                resets_at: five_hour_resets_at,
            },
            seven_day: UsageWindow {
                used_pct: 41.2,
                resets_at: seven_day_resets_at,
            },
        }
    }

    /// The stale-snapshot render decision: both resets in the future is
    /// the only fresh shape.
    #[test]
    fn a_snapshot_is_fresh_only_while_both_resets_are_ahead() {
        let now = 1_000_000;
        assert!(!is_stale(&usage(now as u64 + 60, now as u64 + 86_400), now));

        // The five-hour reset passing is the common staleness ("the last
        // session ended hours ago") — the whole snapshot goes.
        assert!(is_stale(&usage(now as u64 - 1, now as u64 + 86_400), now));
        // Either window suffices, and the boundary itself counts.
        assert!(is_stale(&usage(now as u64 + 60, now as u64 - 1), now));
        assert!(is_stale(&usage(now as u64, now as u64 + 86_400), now));
    }

    /// An epoch too large for chrono never marks the snapshot stale (it is
    /// arbitrarily far in the future) — it just loses its formatted time
    /// (see `gauge_row`'s fallback).
    #[test]
    fn an_unrepresentable_reset_is_not_stale() {
        assert!(!is_stale(&usage(u64::MAX, u64::MAX), 1_000_000));
        assert_eq!(reset_local(u64::MAX), None);
    }

    /// Same-day resets show the bare clock time; a different calendar day
    /// (routine for the 7-day window) gains the weekday. Both built with
    /// `with_ymd_and_hms` — the `modules::clock` tests' timezone-safe
    /// construction — rather than epoch literals whose local rendering
    /// would depend on the machine running the tests.
    #[test]
    fn reset_times_are_absolute_local_clock_readings() {
        use chrono::TimeZone;
        let now = Local.with_ymd_and_hms(2026, 8, 1, 10, 30, 0).unwrap();

        let today = Local.with_ymd_and_hms(2026, 8, 1, 14, 0, 0).unwrap();
        assert_eq!(format_reset(today, now), "resets 14:00");

        let thursday = Local.with_ymd_and_hms(2026, 8, 6, 9, 0, 0).unwrap();
        assert_eq!(format_reset(thursday, now), "resets Thu 09:00");
    }

    /// The gauge row itself renders for a plain, representable window —
    /// the fresh-path caption the `view` smoke test's `u64::MAX` epochs
    /// can't reach.
    #[test]
    fn gauge_rows_render_without_panicking() {
        use chrono::TimeZone;
        let theme = Theme::saola();
        let now = Local.with_ymd_and_hms(2026, 8, 1, 10, 30, 0).unwrap();
        let window = UsageWindow {
            used_pct: 23.5,
            resets_at: now.timestamp() as u64 + 3_600,
        };
        let _: Element<'_, crate::Message> = gauge_row(&theme, "5-hour", window, now);
    }

    #[test]
    fn the_state_lifecycle_matches_the_open_flow() {
        let mut state = ClaudeUsageState::opening();
        assert!(state.loading);
        assert!(state.sessions().is_empty());

        state.set_loaded(vec![SessionUsage {
            target: target("aaaaaaaa", SessionStatus::Done),
            usage: None,
        }]);
        assert!(!state.loading);
        assert_eq!(state.sessions().len(), 1);
    }
}
