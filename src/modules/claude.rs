//! The Claude Code status pill, fed by hooks broadcasting D-Bus signals.
//!
//! Every other zbus module so far (`battery`, `network`, `media`) talks to a
//! *service*: something sitting on the bus, advertising properties, that we
//! ask questions of via a generated proxy. Claude Code has no such thing —
//! there is no `io.saola.ClaudeCode1` process to connect to, no object to
//! proxy, no property to read. `contrib/claude-code/emit.sh` (run from a
//! Claude Code hook, see that file) fires a one-shot `busctl --user emit`
//! and exits; nothing stays resident on the bus, on either end.
//!
//! Teaching note (signals without a service — the proxy macro doesn't fit):
//! `#[zbus::proxy]` generates a struct built around *destination + path*,
//! because it exists to make method calls and read properties — both of
//! which require a specific object to talk to. A broadcast signal from a
//! process that appears for one D-Bus call and disappears again has no
//! stable destination to proxy (`busctl emit`'s sender is a transient unique
//! name, different every time, and this module deliberately never filters
//! on it — see [`CLAUDE_CODE_INTERFACE`]'s doc comment). What we actually
//! want is "hand me every message matching this shape, whoever sent it,"
//! which is exactly [`zbus::MatchRule`] plus
//! [`zbus::MessageStream::for_match_rule`]: a `MatchRule` is registered with
//! the bus and the returned stream yields every message matching it,
//! filtered bus-side. There is no "connect" step that can fail because the
//! service is missing — a `MatchRule` on the session bus is always valid to
//! register, whether or not anything will ever send a matching signal. That
//! is *why* absence is silent here in a way it isn't for `battery`/
//! `network`: those modules render nothing because a proxy call failed;
//! this one renders nothing because the stream has simply never yielded a
//! message. Same visible outcome, different reason underneath.
//!
//! # The bus schema (this module's half of the contract)
//!
//! - Session bus (`Connection::session()`, like `media.rs` — a per-user
//!   hook process, not a system service).
//! - Object path `/io/saola/ClaudeCode` ([`CLAUDE_CODE_PATH`]).
//! - Interface `io.saola.ClaudeCode1` ([`CLAUDE_CODE_INTERFACE`]).
//! - Signal `StatusChanged(session_id: s, status: s)`
//!   ([`CLAUDE_CODE_MEMBER`]), `status` one of `"working"`, `"attention"`,
//!   `"idle"`, `"ended"` (see [`fold`]).
//!
//! # The fold (state, not a snapshot — same shape as `columns.rs`)
//!
//! A `StatusChanged` signal is a delta ("this one session's status just
//! became X"), not the whole picture, so the worker keeps a
//! `HashMap<session_id, SessionStatus>` and [`fold`]s each event into it,
//! then derives the pill from the *whole map* on every event (see
//! [`summarize`]). `columns.rs`'s module doc comment covers the general
//! shape of this pattern in more depth; the one new wrinkle here is that
//! `"ended"` doesn't update an entry, it deletes one — a session that ended
//! has nothing left to report, which is the same "not tracked" state as a
//! session this module has never heard from.
//!
//! # Render: priority, not addition, and always-muted pill
//!
//! Per CLAUDE.md's one rule ("at most one terracotta element per surface"),
//! the pill itself is always [`style::button::muted`], the quiet subtle-fill
//! pill — even while a session is `working`. A solid-terracotta pill for
//! every "someone's working" moment would flood the bar, since Claude Code
//! sessions are common and long-running compared to, say, "battery is
//! charging." Instead, "live" is carried entirely by the *label's* color:
//! `working` renders its text in `palette.accent_light` (the accent-ramp
//! token for accent-colored text on ink — see [`ClaudeCode::view`]), while
//! `attention` (Claude wants a decision: `Notification` hook — permission
//! prompt, idle nudge, ...) keeps the pill's ordinary secondary-emphasis
//! label and is conveyed by wording alone ("input?"), never a color at all.
//! `working` always wins the summary if any session has it — a terminal
//! that's mid-generation is more urgent than one waiting on input, so the
//! pill shows `working`'s count and says nothing about how many sessions
//! are also waiting on attention. A tracked set that is entirely `idle` (or
//! empty) renders nothing: `idle` means "Claude answered, waiting on
//! Jordan," which isn't something the bar needs to flag any more than
//! "nothing is happening" needs a pill. See [`summarize`] for the pure
//! reduction this paragraph describes.
//!
//! # The stale-session quirk (accepted, v0.2)
//!
//! A session that dies without ever sending `"ended"` — Jordan closes the
//! terminal mid-task, `kill -9`s the process, the machine sleeps mid-hook —
//! leaves its last known status in the map forever; nothing ever removes
//! it. A TTL sweep (drop entries older than N minutes) would fix this, but
//! doing that here means a *timer* driving the worker, which is exactly the
//! poll CLAUDE.md forbids on the panel side — every other module's
//! reconnect backoff (`columns.rs`, `volume.rs`) only ever paces retry
//! attempts while disconnected, never ticks while healthy, and a TTL sweep
//! has no equivalent "only while unhealthy" excuse: it would have to wake
//! up on a schedule regardless of whether anything changed. So this module
//! doesn't add one. In practice a linger is cosmetic (an extra `1 working`
//! that should say nothing, or a `2 working` that should say `1`) and
//! self-heals the next time that same `session_id` reports any status,
//! including a *future* Claude Code session reusing... well, it won't reuse
//! the id (they're UUIDs), so a genuinely orphaned entry only clears on
//! panel restart. Flagged for the handoff, not fixed here.

use std::collections::HashMap;

use iced::futures::channel::mpsc;
use iced::futures::stream::StreamExt;
use iced::futures::{SinkExt, Stream};
use iced::widget::button::Status as ButtonStatus;
use iced::widget::{button, text, Space};
use iced::{Element, Fill, Subscription};
use saola_theme::{style, ColorExt, Surface, Theme};
use zbus::{Connection, MatchRule, MessageStream};

/// The Claude Code module's own message type (Stage 7's per-module refactor
/// — see `modules::clock::Message` for the full teaching note). `main.rs`
/// nests this as `Message::ClaudeCode(claude::Message)`; `Panel::update`
/// delegates by matching through both layers:
/// `Message::ClaudeCode(claude::Message::Updated(c))`.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(ClaudeCode),
}

/// The object path `emit.sh` targets and this module listens on. Not a
/// service path in the UPower/iwd sense (nothing is *hosted* there) — it's
/// just the fixed address a broadcast signal claims to have come from.
const CLAUDE_CODE_PATH: &str = "/io/saola/ClaudeCode";

/// The signal's interface. Versioned (`1`) per the D-Bus convention of
/// baking a revision number into a custom interface name, so a future
/// breaking change to the signal's argument shape can ship as `...1` +
/// `...2` coexisting rather than an in-place break.
///
/// Teaching note (no `.sender(...)` on the match rule built from this):
/// `busctl --user emit` doesn't request a well-known bus name before
/// emitting — it sends the signal from whatever transient unique name
/// (`:1.234`, different every invocation) the session bus hands its
/// short-lived connection. There is no stable sender to filter on, by
/// design (PLAN.md: "no bus-name ownership on either side") — the
/// interface + path + member triple is the whole identity of this signal.
const CLAUDE_CODE_INTERFACE: &str = "io.saola.ClaudeCode1";

/// The signal name: `StatusChanged(session_id: s, status: s)`.
const CLAUDE_CODE_MEMBER: &str = "StatusChanged";

/// One session's last-known status, as folded from the wire's `status`
/// string (see [`fold`]). Named `SessionStatus` rather than `Status` to
/// keep this out of the way of `iced::widget::button::Status` ([`ButtonStatus`]
/// above), which every module's pill-styling closure also needs in scope.
///
/// `"ended"` has no variant here — it doesn't produce a status, it removes
/// the map entry entirely (see [`fold`]), so by the time anything reads a
/// `SessionStatus` out of the map, "ended" has already happened and left no
/// trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionStatus {
    /// `UserPromptSubmit` hook — Claude is generating.
    Working,
    /// `Notification` hook — Claude wants a decision (permission prompt, an
    /// idle nudge, ...). Conveyed by the pill's wording ("input?"), never a
    /// second color — see the module doc comment's render section.
    Attention,
    /// `Stop` hook — Claude answered, waiting on Jordan. Never shown; see
    /// [`summarize`].
    Idle,
}

/// The derived "what should the pill say" value — what [`summarize`]
/// produces and [`ClaudeCode::view`] renders. `None` (inside
/// `ClaudeCode::summary`) means "render nothing"; this struct is only ever
/// constructed for the two states that *do* render.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Summary {
    /// The pill's text — `"working"` / `"N working"` / `"input?"` /
    /// `"N input?"`. See [`working_label`] / [`attention_label`].
    label: String,
    /// Whether this summary is `working` (as opposed to `attention`). The
    /// pill itself is always [`style::button::muted`] either way — `live`
    /// only decides the *label's* color, `palette.accent_light` when true,
    /// left at `muted`'s own secondary-emphasis default otherwise. See the
    /// module doc comment's render section for why.
    live: bool,
}

/// Claude Code module state: the last derived [`Summary`] the worker
/// pushed through [`Message::Updated`] (or `None`, the boot state and the
/// "no sessions worth flagging" state alike — see the module doc comment's
/// render section for what folds into `None`).
///
/// Unlike `Battery`/`Network`, there's no separate `present` flag: a
/// `Summary`-less state already means "render nothing," whatever the
/// reason (no signal ever received, every tracked session is idle, or the
/// map is empty because every session ended cleanly) — one absent state
/// covers all three, the same way `Battery::present: false` covers "no
/// battery," "UPower missing," and "worker hasn't reported yet" as one
/// outcome.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClaudeCode {
    summary: Option<Summary>,
}

impl ClaudeCode {
    /// Renders the pill, or nothing when `summary` is `None`.
    ///
    /// Built the same way as `battery.rs`'s pill (a `button` with no
    /// `on_press`, `ButtonStatus` pinned to `Active` so the theme's helper
    /// doesn't gray it out as disabled — see that module's `view` doc
    /// comment for the full teaching note). Unlike `battery.rs`, there's
    /// only one pill style, `muted` — see the module doc comment's render
    /// section for why a solid-terracotta `working` pill was removed.
    /// `live` instead only picks the *label's* color: `palette.accent_light`
    /// (terracotta-ramp text-on-ink) while `working`, left unset (falling
    /// back to `muted`'s own secondary-emphasis default) while `attention`.
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Asks the same `Option` as `view`'s `let-else`, so the two
    /// cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.summary.is_some()
    }

    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        let Some(summary) = &self.summary else {
            return Space::new().into();
        };

        let pill_style = style::button::muted(theme, Surface::Ink);

        let label = text(summary.label.clone())
            .size(theme.typography.size.bar)
            .height(Fill)
            .align_y(iced::Center);
        // Only override the color for `working`; `attention` keeps
        // `muted`'s default secondary label rather than repeating that same
        // color here explicitly — one fewer place for it to drift out of
        // sync with the theme's own default.
        let label = if summary.live {
            label.color(theme.palette.accent_light.into_iced())
        } else {
            label
        };

        button(label)
            // `panel_pill_media` (30) is the compact in-bar pill height —
            // named for its primary consumer, the media pill, but this pill
            // sits in the same 48px bar and matches it for the same reason:
            // a 40px `panel_pill` pill is the free-standing
            // islands-mode/popover scale, not this one.
            .height(theme.sizes.panel_pill_media)
            .padding([0.0, theme.sizes.panel_pill_media / 2.0])
            .style(move |iced_theme, _status| pill_style(iced_theme, ButtonStatus::Active))
            .into()
    }

    /// The Claude Code signal feed as an iced subscription. See
    /// `battery.rs`'s `subscription` for the function-pointer-identity
    /// teaching note — identical reasoning applies verbatim.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(claude_code_stream)
    }
}

/// Folds one `StatusChanged` event into the session map.
///
/// `"ended"` removes the entry outright rather than setting it to some
/// "ended" variant — a session with no entry and a session that reported
/// `"ended"` must reduce identically in [`summarize`], and deleting the key
/// is what makes that true for free instead of needing `summarize` to
/// special-case an `Ended` state it would otherwise have to filter out
/// every time.
///
/// An unrecognized `status` string (a future hook revision, a typo in a
/// hand-edited `settings.json` command) is ignored — the entry, if any,
/// keeps its last *known* value rather than being overwritten with
/// something this module can't render. Pure function of its arguments (no
/// D-Bus, no clock), which is what makes it unit-testable below without a
/// bus.
fn fold(sessions: &mut HashMap<String, SessionStatus>, session_id: String, status: &str) {
    match status {
        "working" => {
            sessions.insert(session_id, SessionStatus::Working);
        }
        "attention" => {
            sessions.insert(session_id, SessionStatus::Attention);
        }
        "idle" => {
            sessions.insert(session_id, SessionStatus::Idle);
        }
        "ended" => {
            sessions.remove(&session_id);
        }
        _ => {}
    }
}

/// `"working"` for a single session, `"N working"` for more than one.
fn working_label(count: usize) -> String {
    if count == 1 {
        "working".to_string()
    } else {
        format!("{count} working")
    }
}

/// `"input?"` for a single session, `"N input?"` for more than one.
fn attention_label(count: usize) -> String {
    if count == 1 {
        "input?".to_string()
    } else {
        format!("{count} input?")
    }
}

/// The priority reduction described in the module doc comment's render
/// section, split out from [`summarize`] so it's testable directly against
/// plain counts rather than having to build a `HashMap` for every case:
/// any `working` sessions win outright (their count, live/terracotta); with
/// no `working`, any `attention` sessions show next (their count, muted);
/// with neither, `None` — nothing to flag, whether that's because every
/// tracked session is idle or because there are no tracked sessions at
/// all.
fn summary_from_counts(working: usize, attention: usize) -> Option<Summary> {
    if working > 0 {
        return Some(Summary {
            label: working_label(working),
            live: true,
        });
    }
    if attention > 0 {
        return Some(Summary {
            label: attention_label(attention),
            live: false,
        });
    }
    None
}

/// Reduces the whole session map to the one [`Summary`] the pill shows (or
/// `None`). Pure function of the map — no I/O, no clock — same
/// testability shape as `columns.rs`'s `Strip::dashes`.
fn summarize(sessions: &HashMap<String, SessionStatus>) -> Option<Summary> {
    let working = sessions
        .values()
        .filter(|status| **status == SessionStatus::Working)
        .count();
    let attention = sessions
        .values()
        .filter(|status| **status == SessionStatus::Attention)
        .count();
    summary_from_counts(working, attention)
}

/// Builds the async stream the subscription runs. See `battery.rs`'s
/// `battery_stream` for the full bridge teaching note (the channel, the
/// runtime it runs on). Every failure path here — no session bus, the
/// match rule failing to register, the message stream ending — funnels
/// into "send the hidden/default state, worker ends quietly," same
/// contract as every other module: the panel never goes down because
/// Claude Code hooks aren't wired up (or haven't fired yet).
fn claude_code_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        if watch_claude_code(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(ClaudeCode::default())).await;
        }
    })
}

/// The worker proper: register the match rule, then fold and re-summarize
/// on every matching signal, forever.
///
/// Unlike `columns.rs`'s niri socket, there's no reconnect-with-backoff
/// loop here — a lost session-bus connection is a rare, session-ending
/// event (on par with the desktop session itself dying), not something
/// this module retries through, matching `battery`/`network`/`media`'s
/// "one connection attempt, worker ends quietly on loss" shape rather than
/// `columns`' "keep trying" one.
async fn watch_claude_code(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // Session bus: Claude Code hooks are per-user processes (`emit.sh` runs
    // as Jordan, from Jordan's terminal), not a system service — same
    // reasoning as `media.rs`'s MPRIS players.
    let connection = Connection::session().await?;

    // Teaching note (`MatchRule`, not a proxy): see the module doc comment
    // for why a proxy doesn't fit here. `msg_type(Signal)` restricts the
    // rule to signals (as opposed to method calls/returns/errors, which
    // share the same bus but are irrelevant here); `path` + `interface` +
    // `member` narrow it to exactly this one broadcast. No `.sender(...)`
    // — see `CLAUDE_CODE_INTERFACE`'s doc comment.
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(CLAUDE_CODE_PATH)?
        .interface(CLAUDE_CODE_INTERFACE)?
        .member(CLAUDE_CODE_MEMBER)?
        .build();

    // `for_match_rule` registers the rule with the bus and returns a
    // `Stream` of every message matching it — filtered bus-side, so this
    // task is never woken for a signal it doesn't care about. `Some(8)`
    // matches every other worker's channel capacity in this crate; nothing
    // about this signal's volume calls for a different number.
    let mut signals = MessageStream::for_match_rule(rule, &connection, Some(8)).await?;

    let mut sessions: HashMap<String, SessionStatus> = HashMap::new();
    // Dedupe against the last sent value — cheap, and it means a hook
    // double-firing (or two sessions' events reducing to the same summary)
    // doesn't wake the UI for a no-op. Not load-bearing the way
    // `columns.rs`'s dedupe is (there's no property-cache warm-fire or
    // title-spam source here to suppress) — just free correctness reusing
    // a pattern this crate already has.
    let mut last_sent: Option<ClaudeCode> = None;

    while let Some(message) = signals.next().await {
        // A message that fails to arrive cleanly (a transport-level error)
        // is skipped, not fatal — one bad frame shouldn't tear down every
        // other session's status. `MatchRule` already guarantees anything
        // that *does* arrive here matches interface/path/member, so no
        // further filtering is needed before reading the body.
        let Ok(message) = message else {
            continue;
        };

        // The signal's signature is `ss` (`session_id`, `status`) per the
        // bus schema — a body that doesn't match (a stale emitter build, a
        // hand-typed `busctl` call with the wrong types) is skipped the
        // same way a malformed niri event line is in `columns.rs`: keep
        // the session alive rather than tearing the whole worker down over
        // one bad payload.
        let Ok((session_id, status)) = message.body().deserialize::<(String, String)>() else {
            continue;
        };

        fold(&mut sessions, session_id, &status);

        let claude_code = ClaudeCode {
            summary: summarize(&sessions),
        };
        if last_sent.as_ref() == Some(&claude_code) {
            continue;
        }
        if sender
            .send(Message::Updated(claude_code.clone()))
            .await
            .is_err()
        {
            // Receiving side gone (subscription dropped) — stop quietly.
            return Ok(());
        }
        last_sent = Some(claude_code);
    }

    // The stream ended — the session bus connection itself is gone. Rare
    // (see this fn's doc comment), and not retried; returning `Ok` here
    // (rather than an error the wrapper would turn into a default-state
    // send) matches `battery`/`network`'s "worker ends quietly" contract
    // for this same case. The one accepted cost: if this ever happens with
    // a non-empty summary on screen, that summary lingers until the panel
    // restarts — a second, rarer flavor of the stale-session quirk the
    // module doc comment already flags.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_inserts_recognized_statuses() {
        let mut sessions = HashMap::new();
        fold(&mut sessions, "a".to_string(), "working");
        assert_eq!(sessions.get("a"), Some(&SessionStatus::Working));

        fold(&mut sessions, "a".to_string(), "attention");
        assert_eq!(sessions.get("a"), Some(&SessionStatus::Attention));

        fold(&mut sessions, "a".to_string(), "idle");
        assert_eq!(sessions.get("a"), Some(&SessionStatus::Idle));
    }

    #[test]
    fn fold_ended_removes_the_session() {
        let mut sessions = HashMap::new();
        fold(&mut sessions, "a".to_string(), "working");
        assert!(sessions.contains_key("a"));

        fold(&mut sessions, "a".to_string(), "ended");
        assert!(!sessions.contains_key("a"));
    }

    #[test]
    fn fold_ending_an_unknown_session_is_a_no_op() {
        let mut sessions = HashMap::new();
        fold(&mut sessions, "never-seen".to_string(), "ended");
        assert!(sessions.is_empty());
    }

    #[test]
    fn fold_ignores_unrecognized_status_strings() {
        let mut sessions = HashMap::new();
        // A future hook revision (or a typo) sending a status this module
        // doesn't know: the session is left untracked rather than getting
        // a guessed-at status.
        fold(&mut sessions, "a".to_string(), "sleeping");
        assert!(sessions.is_empty());

        // Same, but for an already-tracked session: the last known-good
        // status survives rather than being clobbered.
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "a".to_string(), "sleeping");
        assert_eq!(sessions.get("a"), Some(&SessionStatus::Working));
    }

    #[test]
    fn summary_from_counts_working_wins_over_attention() {
        let summary = summary_from_counts(1, 3).unwrap();
        assert_eq!(summary.label, "working");
        assert!(summary.live);
    }

    #[test]
    fn summary_from_counts_pluralizes_working() {
        assert_eq!(summary_from_counts(1, 0).unwrap().label, "working");
        assert_eq!(summary_from_counts(2, 0).unwrap().label, "2 working");
        assert_eq!(summary_from_counts(11, 0).unwrap().label, "11 working");
    }

    #[test]
    fn summary_from_counts_attention_only_reads_input_question() {
        let summary = summary_from_counts(0, 1).unwrap();
        assert_eq!(summary.label, "input?");
        assert!(!summary.live);

        assert_eq!(summary_from_counts(0, 2).unwrap().label, "2 input?");
    }

    #[test]
    fn summary_from_counts_no_working_or_attention_is_none() {
        assert_eq!(summary_from_counts(0, 0), None);
    }

    #[test]
    fn summarize_reduces_the_whole_map() {
        let mut sessions = HashMap::new();
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "b".to_string(), "working");
        let summary = summarize(&sessions).unwrap();
        assert_eq!(summary.label, "2 working");
        assert!(summary.live);
    }

    #[test]
    fn summarize_working_beats_attention_in_a_mixed_map() {
        let mut sessions = HashMap::new();
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "b".to_string(), "attention");
        let summary = summarize(&sessions).unwrap();
        // Only the working count is shown — the attention session is real
        // (still in the map) but doesn't change what the pill says.
        assert_eq!(summary.label, "working");
        assert!(summary.live);
    }

    #[test]
    fn summarize_all_idle_renders_nothing() {
        let mut sessions = HashMap::new();
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "b".to_string(), "idle");
        assert_eq!(summarize(&sessions), None);
    }

    #[test]
    fn summarize_empty_map_renders_nothing() {
        let sessions: HashMap<String, SessionStatus> = HashMap::new();
        assert_eq!(summarize(&sessions), None);
    }

    #[test]
    fn summarize_ended_session_stops_counting_toward_the_pill() {
        let mut sessions = HashMap::new();
        fold(&mut sessions, "a".to_string(), "working");
        assert!(summarize(&sessions).is_some());

        fold(&mut sessions, "a".to_string(), "ended");
        assert_eq!(summarize(&sessions), None);
    }
}
