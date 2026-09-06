//! The Antigravity usage popover's content — [`crate::popover::PopoverKind::
//! AntigravityUsage`]'s real body: one row per tracked `agy` conversation
//! (the same row, in the same order, as the bar's status dots), each
//! showing the session's dot, a shortened id, its status, a context-window
//! fill gauge and its token counters — and, below a separator, the
//! account's two quota gauges when a fresh `UsageChanged` snapshot is in
//! hand. Copied from `popovers::claude_usage` (CLAUDE.md: "copy the
//! established module pattern"), with one deliberate addition, one
//! deliberate subtraction and one deliberate simplification — all three
//! explained below.
//!
//! # Where the token numbers come from (the addition, and the subtraction)
//!
//! Claude Code's popover sums `message.usage` out of each session's
//! transcript **file**. This one never opens a file, because agy's
//! transcripts have nothing to sum: stage 23's recon
//! (`.claude/handoffs/handoff_stage_23.md`) read a real `transcript.jsonl`
//! field-by-field and found flat step records — `type`, `step_index`,
//! `status`, `source`, `created_at`, `content`, `thinking`, `exit_code`,
//! `tool_calls[]` — with **no `usage` object and no per-line model name at
//! any depth** (the only `Model` anywhere is
//! `tool_calls[].args.Subagents[].Model`, the model requested *for a
//! spawned subagent*, not the session's own). The other candidate store,
//! `~/.gemini/antigravity-cli/conversations/<uuid>.db`, is SQLite whose
//! payload columns are opaque protobuf blobs with no token columns either.
//! So `claude_usage.rs`'s `fold_transcript`, `TranscriptUsage`, and
//! `read_usage`'s file-reading machinery are still not copied here — that
//! is the subtraction, and it is why this popover needs no fetch task (see
//! the next section).
//!
//! The numbers themselves arrive **by signal instead**: agy's statusline
//! payload carries a `context_window` block, so
//! `contrib/antigravity/statusline.sh` broadcasts `TokensChanged` and
//! [`crate::modules::antigravity`] folds it onto the session entry. What
//! reaches [`view`] is therefore already-parsed state, not a file read that
//! has to be scheduled.
//!
//! That block carries something Claude Code's transcripts don't — the
//! model's **context window size** — which is what the per-row gauge draws:
//! `input + output` against `context_size`, the same
//! `saola_theme::style::progress::bar` the quota gauges below it use, so
//! "how full is this conversation" reads at a glance in the same visual
//! language as "how much quota is left". **The Claude Code popover has no
//! equivalent**; this is the one readout where agy is better instrumented.
//!
//! A session with no `TokensChanged` yet (statusline not chained, or not
//! refreshed since the panel started) renders its row **without** gauge or
//! numbers rather than with a zero it can't stand behind — the module's
//! existing absence-is-silent idiom, and the reason [`session_row`] takes an
//! `Option<Tokens>` rather than a defaulted struct.
//!
//! # Why this view is synchronous (the simplification)
//!
//! `claude_usage.rs` needs a `ClaudeUsageState` (loading flag + results) and
//! a one-shot `Task` because its numbers come from a file read that has to
//! happen off the click. This popover's rows are just
//! [`crate::modules::antigravity::Antigravity::usage_targets`] — data
//! already sitting in the module's state, folded there by every
//! `StatusChanged` signal as it arrives. Reading it is a synchronous method
//! call, not I/O, so there is no fetch to kick off, nothing that can land
//! late, and therefore no popover-local state to reset on open/close or
//! stray-answer to guard against. [`view`] takes `&Antigravity` directly and
//! reads through it on every render — the same "click is the render" shape
//! as the loaded path in `claude_usage.rs`, minus the loading state that
//! only made sense while something was in flight. Inventing a
//! `AntigravityUsageState`/`Message::Loaded` pair here to mirror Claude
//! Code's shape would be symmetry for its own sake over a fetch that
//! doesn't exist.
//!
//! # The quota gauges, and where their labels come from
//!
//! Same idiom as `claude_usage.rs`: [`crate::modules::antigravity::Usage`]
//! arrives by `UsageChanged` signal (`contrib/antigravity/statusline.sh`)
//! and is read once per render, dropped rather than grayed when
//! [`is_stale`] says the snapshot's windows have already reset (see that
//! function's doc comment — identical reasoning to the Claude Code
//! popover's).
//!
//! **The two labels are the real bucket names** as of the 2026-08-14
//! capture (agy 1.1.13), not the `5-hour`/`7-day` placeholders stage 26
//! shipped: agy's `quota` map came back keyed **`gemini-weekly`** and
//! **`3p-weekly`**, and **both are weekly** — there is no five-hour window
//! anywhere in agy's payload, and the drafting assumption that it mirrored
//! Claude Code's window pair was simply wrong. Slot 1 is `gemini-weekly`
//! and slot 2 is `3p-weekly`, an ordering fixed by
//! `contrib/antigravity/statusline.sh` and mirrored in [`Usage`]'s field
//! names.
//!
//! Captured fact, then — but **not a frozen protocol**: the payload's
//! `quota` is a `map[string]` whose keys the *server* supplies at runtime,
//! so a plan change or a server-side rename could produce different (or
//! differently many) buckets on some future day. [`GEMINI_WEEKLY_LABEL`]
//! and [`THIRD_PARTY_WEEKLY_LABEL`] stay pulled out as named consts for
//! that reason: correcting them stays a one-line edit rather than a hunt
//! through the render code.

use chrono::{DateTime, Local, Utc};
use iced::widget::{column, container, progress_bar, row, rule, text, Space};
use iced::{Element, Fill};
use saola_theme::convert::ColorExt;
use saola_theme::style::container::SessionStatus;
use saola_theme::{style, Surface, Theme};

use crate::modules::antigravity::{Antigravity, Tokens, Usage, UsageWindow};

/// The `gemini-weekly` quota bucket's row label — wire slot 1. See the
/// module doc comment's "quota gauges" section: captured fact as of agy
/// 1.1.13, but the bucket names come from a server-supplied map and could
/// change.
const GEMINI_WEEKLY_LABEL: &str = "gemini weekly";

/// The `3p-weekly` quota bucket's row label — wire slot 2, third-party
/// models. Same captured-not-frozen status as [`GEMINI_WEEKLY_LABEL`].
const THIRD_PARTY_WEEKLY_LABEL: &str = "3p weekly";

/// How many session rows the popover surface budgets space for — see
/// `claude_usage::BUDGETED_SESSIONS` for the shared declare-before-you-know
/// reasoning (`tray_menu::BUDGETED_ROWS`'s doc comment has the full
/// analysis). Same count as the Claude Code popover: Jordan's "a handful of
/// sessions, not thousands" working set applies equally to agy.
const BUDGETED_SESSIONS: f32 = 6.0;

/// The popover surface's declared height: content padding, one header line,
/// the budgeted session rows, and the two quota gauge rows (each separator
/// riding the same `island_gap` allowance `tray_menu`'s separators use).
///
/// **Unchanged by stage 26b's token column.** The per-session gauge and
/// numbers land *inside* the existing row — one `sizes.list_row` line, the
/// same allowance the quota gauges already prove a `progress_bar` fits in —
/// so the row count is what it was and this budget doesn't move.
///
/// **No totals-row allowance** — the one structural difference from
/// `claude_usage::height`, and it survives having numbers to show:
/// `TokensChanged` reports *per-session* counters against *per-session*
/// context windows, and summing those would produce a number ("2.1M of
/// 3.1M") describing no real limit anywhere. The Claude Code popover's
/// totals row aggregates spend against one account; there is no equivalent
/// question here, and the account-wide one is exactly what the quota gauges
/// below the separator already answer. The gauge rows are still budgeted
/// unconditionally, for the same declare-before-you-know reason
/// `claude_usage::height` documents: a popover with no fresh snapshot to
/// show simply has two rows of blank ink at the bottom, never a panic or
/// clipped content.
pub fn height(theme: &Theme) -> f32 {
    theme.sizes.popover_padding * 2.0
        + theme.sizes.list_row * (BUDGETED_SESSIONS + 3.0)
        + theme.sizes.island_gap * 2.0
}

/// The whole popover body: header, one row per tracked conversation — or a
/// quiet "No sessions" line, unreachable in practice since the trigger
/// itself is gated on [`Antigravity::is_present`] — plus, when a fresh
/// rate-limit snapshot is in hand, the two gauge rows under their own
/// separator.
///
/// Reads `antigravity` directly rather than through any popover-local
/// state — see the module doc comment's "why this view is synchronous"
/// section for why there is nothing else to thread through here.
pub fn view<'a>(theme: &Theme, antigravity: &'a Antigravity) -> Element<'a, crate::Message> {
    // Read once per render and handed to the pure staleness/formatting
    // functions below — the `modules::clock` testability pattern
    // `claude_usage::view` already uses.
    let now = Local::now();

    let targets = antigravity.usage_targets();
    let sessions: Element<'a, crate::Message> = if targets.is_empty() {
        // Only reachable if every conversation ended between the click and
        // this render — the trigger doesn't render without sessions.
        quiet_line(theme, "No sessions")
    } else {
        let mut lines: Vec<Element<'a, crate::Message>> = vec![header(theme)];
        for target in &targets {
            lines.push(session_row(
                theme,
                target.id.clone(),
                target.status,
                target.tokens,
            ));
        }
        column(lines).into()
    };

    let mut lines: Vec<Element<'a, crate::Message>> = vec![sessions];
    if let Some(usage) = antigravity.usage() {
        if !is_stale(&usage, now.timestamp()) {
            lines.push(separator(theme));
            lines.push(gauge_row(
                theme,
                GEMINI_WEEKLY_LABEL,
                usage.gemini_weekly,
                now,
            ));
            lines.push(gauge_row(
                theme,
                THIRD_PARTY_WEEKLY_LABEL,
                usage.third_party_weekly,
                now,
            ));
        }
    }

    container(column(lines))
        .style(style::container::popover(theme))
        .padding(theme.sizes.popover_padding)
        .width(Fill)
        .height(Fill)
        .into()
}

/// Whether a snapshot is too old to show — identical rule and reasoning to
/// `claude_usage::is_stale`: the moment either window's declared reset
/// passes, the percentages describe windows that no longer exist. `<=`
/// rather than `<` for the same reset-instant-is-already-obsolete reason.
///
/// Pure function of its arguments, unit-tested below.
fn is_stale(usage: &Usage, now_epoch: i64) -> bool {
    let passed = |window: &UsageWindow| {
        i64::try_from(window.resets_at).is_ok_and(|resets_at| resets_at <= now_epoch)
    };
    passed(&usage.gemini_weekly) || passed(&usage.third_party_weekly)
}

/// One window's gauge line — identical layout to `claude_usage::gauge_row`
/// (the label quietly on the left, the theme's progress bar filling the
/// middle, the percentage and absolute reset time on the right).
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
/// epoch value outside chrono's representable range, same caveat as
/// `claude_usage::reset_local`.
fn reset_local(resets_at: u64) -> Option<DateTime<Local>> {
    let secs = i64::try_from(resets_at).ok()?;
    Some(DateTime::<Utc>::from_timestamp(secs, 0)?.with_timezone(&Local))
}

/// `"resets 14:00"` / `"resets Thu 09:00"` — identical to
/// `claude_usage::format_reset`.
fn format_reset(reset: DateTime<Local>, now: DateTime<Local>) -> String {
    if reset.date_naive() == now.date_naive() {
        format!("resets {}", reset.format("%H:%M"))
    } else {
        format!("resets {}", reset.format("%a %H:%M"))
    }
}

/// A single quiet line in the `secondary` role, holding a `list_row` slot —
/// identical to `claude_usage::quiet_line`.
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

/// The header line: the module's name in primary ivory, with the two
/// right-hand columns named quietly beside it — exactly `claude_usage::
/// header`'s shape, and for the same reason: labelling the columns once up
/// here is what lets every row below be bare numbers. `context` names the
/// fill gauge, which has no caption of its own; `tokens in / out` names the
/// pair after it.
fn header<'a>(theme: &Theme) -> Element<'a, crate::Message> {
    container(
        row![
            text("Antigravity")
                .size(theme.typography.size.bar)
                .color(theme.on_ink.primary.into_iced()),
            Space::new().width(Fill),
            text("context · tokens in / out")
                .size(theme.typography.size.bar)
                .color(theme.on_ink.secondary.into_iced()),
        ]
        .align_y(iced::Center),
    )
    .height(theme.sizes.list_row)
    .align_y(iced::Center)
    .into()
}

/// One conversation's line: its dot (the same `status_dot` the bar draws,
/// at full opacity — the popover doesn't breathe, matching
/// `claude_usage::session_row`), the shortened id in primary ivory, the
/// status quietly beside it, then the context-fill gauge and the token pair
/// on the right.
///
/// The status label moved from the row's right edge to just after the id
/// when the numbers arrived (stage 26b), which is where `claude_usage::
/// session_row` has always kept it: the right edge is the numbers column in
/// both popovers now, and the two readouts should be scannable the same way.
///
/// **A session with no [`Tokens`] yet renders neither gauge nor numbers** —
/// the middle `Space` simply expands where the gauge would have gone and the
/// row ends after the status. Absence is silent (the module's idiom
/// throughout), and it keeps the row list matching the bar's dots one for
/// one whatever the statusline has or hasn't reported. Same for a session
/// whose `context_size` is `0`: [`context_fill`] returns `None` rather than
/// dividing by it, so the numbers still show and only the gauge is dropped.
fn session_row<'a>(
    theme: &Theme,
    id: String,
    status: SessionStatus,
    tokens: Option<Tokens>,
) -> Element<'a, crate::Message> {
    let text_size = theme.typography.size.bar;

    // The gauge, or the plain spacer that stood there before there were any
    // numbers to gauge. Either way this is the row's one flexible element,
    // so the id sits left and the numbers sit right regardless.
    let filler: Element<'a, crate::Message> = match tokens.and_then(context_fill) {
        Some(fill) => progress_bar(0.0..=100.0, fill)
            .girth(theme.sizes.dash_height)
            .style(style::progress::bar(theme, Surface::Ink))
            .into(),
        None => Space::new().width(Fill).into(),
    };

    let mut line = row![
        container(Space::new())
            .width(theme.sizes.dash_height)
            .height(theme.sizes.dash_height)
            .style(style::container::status_dot(theme, status, 1.0)),
        text(short_id(&id).to_string())
            .size(text_size)
            .color(theme.on_ink.primary.into_iced()),
        text(status_label(status))
            .size(text_size)
            .color(theme.on_ink.secondary.into_iced()),
        filler,
    ];

    if let Some(tokens) = tokens {
        line = line.push(
            text(format!(
                "{} / {}",
                format_tokens(tokens.input),
                format_tokens(tokens.output)
            ))
            .size(text_size)
            .color(theme.on_ink.primary.into_iced()),
        );
    }

    container(line.spacing(theme.sizes.bar_icon_gap).align_y(iced::Center))
        .height(theme.sizes.list_row)
        .align_y(iced::Center)
        .into()
}

/// How full this conversation's context window is, as a percentage for
/// [`session_row`]'s gauge — `input + output` against `context_size`.
///
/// `None` when there is no denominator to divide by (`context_size == 0`,
/// only reachable from a hand-typed `busctl` emit or a payload that omitted
/// the field): the gauge is dropped rather than drawn at an invented value.
///
/// Clamped to `0.0..=100.0` because the counters are cumulative across a
/// conversation while the window is not — a long conversation that has been
/// compacted can legitimately have *sent* more tokens than the window holds,
/// and a bar overflowing its track would read as a bug rather than as the
/// "this session is full" it actually means. Saturating arithmetic on the
/// sum for the same defensive reason `phase_of` guards its divisor:
/// `u64::MAX`-shaped garbage on the wire should degrade, not panic.
///
/// Pure function of its argument, unit-tested below.
fn context_fill(tokens: Tokens) -> Option<f32> {
    if tokens.context_size == 0 {
        return None;
    }
    let used = tokens.input.saturating_add(tokens.output) as f64;
    let fill = used / tokens.context_size as f64 * 100.0;
    Some(fill.clamp(0.0, 100.0) as f32)
}

/// Token counts read at a glance: exact below a thousand, one decimal of
/// `k`/`M` above.
///
/// A verbatim copy of `claude_usage::format_tokens` (private there, and that
/// file is not this stage's to edit), kept identical on purpose — the two
/// popovers sit one click apart on the same bar and a number must not mean
/// two different things between them. If a third agent module ever wants it,
/// that is the moment to lift one shared helper rather than keep a third
/// copy.
fn format_tokens(count: u64) -> String {
    if count < 1_000 {
        count.to_string()
    } else if count < 1_000_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    }
}

/// A divider — identical to `claude_usage::separator`.
fn separator<'a>(theme: &Theme) -> Element<'a, crate::Message> {
    container(rule::horizontal(1.0).style(style::rule::rest(theme, Surface::Ink)))
        .padding(iced::padding::vertical(theme.sizes.island_gap / 2.0))
        .width(Fill)
        .into()
}

/// Conversation ids are UUIDs; the first block is plenty to tell a handful
/// of concurrent conversations apart — identical to
/// `claude_usage::short_id`.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The human word for each dot — identical to `claude_usage::status_label`.
/// Exhaustive on purpose: a sixth status added to saola-theme becomes a
/// compile error here.
fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Working => "working",
        SessionStatus::Subagents => "subagents",
        SessionStatus::Attention => "needs attention",
        SessionStatus::Done => "done",
        SessionStatus::Idle => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::antigravity::Message;

    #[test]
    fn height_is_positive() {
        let theme = Theme::saola();
        assert!(height(&theme) > 0.0);
    }

    #[test]
    fn view_renders_without_panicking_with_no_sessions() {
        let theme = Theme::saola();
        let antigravity = Antigravity::default();
        let _: Element<'_, crate::Message> = view(&theme, &antigravity);
    }

    /// Rows *and* gauges in one render — the whole popover as it looks in
    /// practice, the way `claude_usage`'s equivalent test does it.
    ///
    /// `Sessions`' entries are private to `modules::antigravity` (by design —
    /// see that module's doc comment on why order/identity is guarded behind
    /// its fold), so the row list is built through that module's
    /// `#[cfg(test)]` `Sessions::seed` door rather than by reaching into
    /// `entries`.
    #[test]
    fn view_renders_without_panicking_with_rows_and_gauges() {
        use crate::modules::antigravity::Sessions;

        let theme = Theme::saola();
        let mut antigravity = Antigravity::default();
        let mut sessions = Sessions::default();
        sessions.seed("aaaaaaaa-1111", SessionStatus::Working, None);
        sessions.seed("bbbbbbbb-2222", SessionStatus::Done, None);
        antigravity.update(Message::Updated(sessions));
        antigravity.update(Message::UsageUpdated(Usage {
            gemini_weekly: UsageWindow {
                used_pct: 23.5,
                resets_at: u64::MAX,
            },
            third_party_weekly: UsageWindow {
                used_pct: 41.2,
                resets_at: u64::MAX,
            },
        }));
        let _: Element<'_, crate::Message> = view(&theme, &antigravity);
    }

    /// The per-row rendering itself, exercised directly (pure function, no
    /// `Antigravity` state needed) across every status the theme defines —
    /// exhaustiveness the compiler already enforces in `status_label`, this
    /// just proves none of the five panics on render, with and without the
    /// token column.
    #[test]
    fn session_rows_render_for_every_status_without_panicking() {
        let theme = Theme::saola();
        for status in [
            SessionStatus::Working,
            SessionStatus::Subagents,
            SessionStatus::Attention,
            SessionStatus::Done,
            SessionStatus::Idle,
        ] {
            for tokens in [
                None,
                Some(Tokens {
                    input: 58_067,
                    output: 11_972,
                    context_size: 1_048_576,
                }),
                // A session with numbers but no window to measure them
                // against: numbers, no gauge.
                Some(Tokens {
                    input: 10,
                    output: 2,
                    context_size: 0,
                }),
            ] {
                let _: Element<'_, crate::Message> =
                    session_row(&theme, "aaaaaaaa-1111".to_string(), status, tokens);
            }
        }
    }

    // -- the per-session token column and context gauge ---------------------

    #[test]
    fn context_fill_is_the_used_share_of_the_window() {
        assert_eq!(
            context_fill(Tokens {
                input: 250,
                output: 250,
                context_size: 1_000,
            }),
            Some(50.0)
        );
        assert_eq!(
            context_fill(Tokens {
                input: 0,
                output: 0,
                context_size: 1_000,
            }),
            Some(0.0)
        );
    }

    #[test]
    fn context_fill_has_no_gauge_without_a_window() {
        // Only a hand-typed emit (or a payload missing the field) gets here —
        // the gauge is dropped rather than dividing by zero.
        assert_eq!(
            context_fill(Tokens {
                input: 100,
                output: 20,
                context_size: 0,
            }),
            None
        );
    }

    #[test]
    fn context_fill_saturates_instead_of_overflowing_its_track() {
        // Cumulative counters against a window that compaction resets: more
        // sent than the window holds is a real state, and it reads as "full".
        assert_eq!(
            context_fill(Tokens {
                input: 4_000,
                output: 1_000,
                context_size: 1_000,
            }),
            Some(100.0)
        );
        // And the sum itself can't panic on garbage.
        assert_eq!(
            context_fill(Tokens {
                input: u64::MAX,
                output: u64::MAX,
                context_size: 1_000,
            }),
            Some(100.0)
        );
    }

    #[test]
    fn token_counts_are_abbreviated_the_way_claude_usage_abbreviates_them() {
        // Kept byte-identical to `claude_usage::format_tokens`' own test: the
        // two popovers are one click apart and a number must not mean two
        // different things between them.
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(45_600), "45.6k");
        assert_eq!(format_tokens(1_230_000), "1.2M");
    }

    /// The whole popover over a *populated* row list — the path the two
    /// `view` tests above can't reach, since `Sessions`' entries are private
    /// by design (see `modules::antigravity`'s doc comment on why order and
    /// identity are guarded behind its fold). `Sessions::seed` is that
    /// module's `#[cfg(test)]` door for exactly this.
    #[test]
    fn view_renders_rows_carrying_tokens_without_panicking() {
        use crate::modules::antigravity::Sessions;

        let theme = Theme::saola();
        let mut antigravity = Antigravity::default();
        let mut sessions = Sessions::default();
        // `a` gets numbers, `b` never does — a mixed row list is the case
        // worth rendering, since absence has to stay silent *per row*.
        sessions.seed(
            "aaaaaaaa-1111",
            SessionStatus::Working,
            Some(Tokens {
                input: 58_067,
                output: 11_972,
                context_size: 1_048_576,
            }),
        );
        sessions.seed("bbbbbbbb-2222", SessionStatus::Idle, None);
        antigravity.update(Message::Updated(sessions));

        assert_eq!(antigravity.usage_targets().len(), 2);
        let _: Element<'_, crate::Message> = view(&theme, &antigravity);
    }

    // -- the quota gauges (identical rules to `claude_usage`'s) ------------

    fn usage(gemini_resets_at: u64, third_party_resets_at: u64) -> Usage {
        Usage {
            gemini_weekly: UsageWindow {
                used_pct: 23.5,
                resets_at: gemini_resets_at,
            },
            third_party_weekly: UsageWindow {
                used_pct: 41.2,
                resets_at: third_party_resets_at,
            },
        }
    }

    #[test]
    fn a_snapshot_is_fresh_only_while_both_resets_are_ahead() {
        let now = 1_000_000;
        assert!(!is_stale(&usage(now as u64 + 60, now as u64 + 86_400), now));
        assert!(is_stale(&usage(now as u64 - 1, now as u64 + 86_400), now));
        assert!(is_stale(&usage(now as u64 + 60, now as u64 - 1), now));
        assert!(is_stale(&usage(now as u64, now as u64 + 86_400), now));
    }

    #[test]
    fn an_unrepresentable_reset_is_not_stale() {
        assert!(!is_stale(&usage(u64::MAX, u64::MAX), 1_000_000));
        assert_eq!(reset_local(u64::MAX), None);
    }

    #[test]
    fn reset_times_are_absolute_local_clock_readings() {
        use chrono::TimeZone;
        let now = Local.with_ymd_and_hms(2026, 8, 1, 10, 30, 0).unwrap();

        let today = Local.with_ymd_and_hms(2026, 8, 1, 14, 0, 0).unwrap();
        assert_eq!(format_reset(today, now), "resets 14:00");

        let thursday = Local.with_ymd_and_hms(2026, 8, 6, 9, 0, 0).unwrap();
        assert_eq!(format_reset(thursday, now), "resets Thu 09:00");
    }

    #[test]
    fn gauge_rows_render_without_panicking() {
        use chrono::TimeZone;
        let theme = Theme::saola();
        let now = Local.with_ymd_and_hms(2026, 8, 1, 10, 30, 0).unwrap();
        let window = UsageWindow {
            used_pct: 23.5,
            resets_at: now.timestamp() as u64 + 3_600,
        };
        for label in [GEMINI_WEEKLY_LABEL, THIRD_PARTY_WEEKLY_LABEL] {
            let _: Element<'_, crate::Message> = gauge_row(&theme, label, window, now);
        }
    }

    #[test]
    fn short_ids_are_the_uuids_first_block() {
        assert_eq!(short_id("a1b2c3d4-e5f6-7890"), "a1b2c3d4");
        assert_eq!(short_id("tiny"), "tiny");
    }
}
