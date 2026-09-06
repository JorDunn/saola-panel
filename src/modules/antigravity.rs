//! The Antigravity session semaphore: one status dot per live `agy`
//! conversation, fed by hooks (and a statusline) broadcasting D-Bus signals.
//!
//! `agy` is Google's Antigravity CLI (`~/.local/bin/agy`, config under
//! `~/.gemini/`). This module is a **deliberate standalone copy** of
//! `modules::claude`, not an abstraction over it (Jordan, 2026-08-14 —
//! CLAUDE.md: "copy the established module pattern", "prefer explicit code
//! over clever abstraction"). Two agents is not enough duplication to earn a
//! shared core; a third would be the moment to revisit that. The two modules
//! ship separate D-Bus interfaces on purpose, so the two agents' sessions can
//! never collide in one fold.
//!
//! # How agy differs from Claude Code (verified 2026-08-14 — load-bearing)
//!
//! The shapes below are why this file exists rather than a `claude.rs` with a
//! second interface constant:
//!
//! 1. **Only five hook events exist**: `PreToolUse`, `PostToolUse`,
//!    `PreInvocation`, `PostInvocation`, `Stop`. There is no `SessionStart`,
//!    `SessionEnd`, `UserPromptSubmit`, `SubagentStart`/`SubagentStop`, or
//!    `Notification` — so several statuses Claude Code's hooks produce simply
//!    have no event to fire from here (see [`fold`]).
//! 2. **agy hooks are not fire-and-forget.** `PreToolUse` and `Stop` require a
//!    `decision` field on stdout, and a hook that prints nothing risks
//!    interfering with agy's own tool-permission flow. `contrib/antigravity/
//!    emit.sh` therefore prints a valid response object (`{}`) on every run,
//!    and the shipped `hooks.json` never wires `PreToolUse`/`PostToolUse` at
//!    all. None of that is visible on this side of the bus — it is why the
//!    emitter looks different from `contrib/claude-code/emit.sh` despite the
//!    signal being identical.
//! 3. **Hook payload fields are camelCase**: `conversationId`,
//!    `transcriptPath`, `workspacePaths`, `artifactDirectoryPath`,
//!    `modelName`. Claude Code's are snake_case. Again an emitter-side
//!    difference — by the time a signal reaches this module the two agents'
//!    wire bodies are the same `sss`.
//! 4. **`hooks.json` has its own schema** (a named wrapper object carrying an
//!    `enabled` flag) and lives in `~/.gemini/config/` or a workspace
//!    `.agents/` directory, **not** in `settings.json`. Source:
//!    <https://antigravity.google/docs/hooks>.
//!
//! Everything else about the bridge is `claude.rs`'s: there is no service on
//! the other end to proxy, only a script that fires one `busctl --user emit`
//! and exits, so the subscription is a [`zbus::MatchRule`] +
//! [`zbus::MessageStream::for_match_rule`] rather than a `#[zbus::proxy]`.
//!
//! Teaching note (signals without a service — why the proxy macro doesn't
//! fit): `#[zbus::proxy]` generates a struct built around *destination +
//! path*, because it exists to make method calls and read properties, both of
//! which need a specific object to talk to. A broadcast from a process that
//! appears for one D-Bus call and disappears has no stable destination to
//! proxy (`busctl emit`'s sender is a transient unique name, different every
//! time — see [`ANTIGRAVITY_INTERFACE`]'s doc comment). What we want is "hand
//! me every message matching this shape, whoever sent it," which is exactly
//! what a `MatchRule` registered with the bus gives: the returned stream
//! yields every matching message, filtered bus-side. There is no "connect"
//! step that can fail because the service is missing — a `MatchRule` on the
//! session bus is always valid to register, whether or not anything ever
//! sends a matching signal. That is *why* absence is silent here in a way it
//! isn't for `battery`/`network`: those render nothing because a proxy call
//! failed; this renders nothing because the stream has simply never yielded.
//!
//! # The bus schema (this module's half of the contract, frozen)
//!
//! - Session bus (`Connection::session()`) — agy runs as Jordan, from
//!   Jordan's terminal; there is no system service anywhere in this picture.
//! - Object path `/io/saola/Antigravity` ([`ANTIGRAVITY_PATH`]).
//! - Interface `io.saola.Antigravity1` ([`ANTIGRAVITY_INTERFACE`]).
//! - Signal `StatusChanged(conversation_id: s, status: s, transcript_path: s)`
//!   ([`STATUS_MEMBER`]), `status` one of `"working"`, `"attention"`,
//!   `"done"`, `"idle"`, `"seen"`, `"ended"` (plus the two forward-compatible
//!   arms [`fold`] documents). `transcript_path` is the conversation's
//!   `transcript.jsonl` — agy materializes it for hooks as `transcriptPath` —
//!   or `""` when the emitter had none to report; the two-argument `ss` body
//!   is also accepted, so a partially-updated emitter keeps its dots and
//!   merely lacks usage data. The transcript is *stored*, never watched: it
//!   is read exactly once per usage-popover open, which is a click, not a
//!   poll.
//! - Signal `UsageChanged(gemini_weekly_pct: d, gemini_weekly_resets_at: t,
//!   third_party_weekly_pct: d, third_party_weekly_resets_at: t)`
//!   ([`USAGE_MEMBER`]) — the account's quota gauges, emitted by
//!   `contrib/antigravity/statusline.sh`. Deliberately **no
//!   `conversation_id`**: quotas are account-wide, so this signal does not
//!   participate in the per-session fold at all. One [`Usage`] snapshot is
//!   kept and the newest signal wins — the emitter is stateless and
//!   re-broadcasts on every statusline refresh, and an idempotent "overwrite
//!   with the latest" fold is what makes that harmless. Shown only in the
//!   usage popover; the bar's dot row never renders it.
//!
//!   **The two slots are both weekly** (capture, 2026-08-14 — agy 1.1.13):
//!   agy's statusline payload carries `quota` as a *server-keyed map*, and
//!   the two keys it actually returns are `gemini-weekly` and `3p-weekly`.
//!   There is no five-hour window; the drafting assumption that agy mirrored
//!   Claude Code's `rate_limits.{five_hour,seven_day}` shape was wrong. The
//!   **wire signature stays `dtdt`** (Jordan's call: two windows is two
//!   windows, and re-cutting the signature to carry key strings buys nothing
//!   while the map has exactly two entries) — only the *meaning* of the two
//!   slots is pinned down, and it is pinned **positionally**: slot 1 is
//!   `gemini-weekly`, slot 2 is `3p-weekly`, an ordering `contrib/
//!   antigravity/statusline.sh` and [`Usage`]'s field names both hold to. If
//!   the server ever keys that map differently, this is the contract to
//!   revisit.
//! - Signal `TokensChanged(conversation_id: s, input: t, output: t,
//!   context_size: t)` ([`TOKENS_MEMBER`]) — one conversation's token
//!   counters, also from `contrib/antigravity/statusline.sh`, out of the
//!   statusline payload's `context_window` block. **Per-session**, unlike
//!   `UsageChanged`: it carries a `conversation_id` and folds onto that
//!   session's entry (see [`fold_tokens`]).
//!
//!   This is the one place agy is *better* instrumented than Claude Code.
//!   Claude Code's popover has to sum `message.usage` out of a transcript
//!   file; agy hands the numbers over on the statusline, so they arrive by
//!   signal like everything else and the popover needs no file read at all
//!   (which is just as well — stage 23 established agy's `transcript.jsonl`
//!   has no usage data in it whatsoever). `context_size` is the model's
//!   context window, which is what lets the popover draw a fill gauge no
//!   Claude Code readout has.
//!
//! Identical signatures to `io.saola.ClaudeCode1`, deliberately — the wire is
//! the one place the two agents genuinely are the same, so the listener is a
//! straight copy and only the interface name differs.
//!
//! # The fold, and the `seen` status agy needs and Claude Code doesn't
//!
//! A `StatusChanged` signal is a delta ("this one conversation's status just
//! became X"), not the whole picture, so the worker keeps a [`Sessions`] list
//! and [`fold`]s each event into it, then ships the *whole* list on every
//! event (`columns.rs`'s module doc comment covers this pattern in depth).
//!
//! The new wrinkle here is `"seen"`. agy has **no `SessionStart` hook**, so
//! nothing fires when a conversation opens; its statusline command, which
//! runs on every session update (~300 ms throttle), is the only agy surface
//! that runs at session open at all. Emitting a plain `"idle"` from there
//! would stomp a live `working` dot several times a second, so the statusline
//! emits `"seen"` instead and [`fold`] applies it **conditionally**: insert as
//! [`SessionStatus::Idle`] only when the conversation isn't tracked yet, never
//! overwrite. Same conditional shape as `claude.rs`'s `"subagent-done"` arm,
//! for a completely different reason.
//!
//! Teaching note (why a `Vec`, not a `HashMap`): the dots are *positional* —
//! Jordan reads "the third dot went red" — so the row has to be stable across
//! status changes. A `HashMap` has no order at all (its iteration order is
//! deliberately randomized per process), so a status change would be free to
//! shuffle the whole row. [`Sessions`] is therefore a plain `Vec` in
//! **first-seen order**: a status change rewrites an entry in place and never
//! moves it, and only `"ended"` ever removes one. Linear scan by id is the
//! lookup, which is the right call at this size.
//!
//! # Render: one dot per conversation, five colors, two of them breathing
//!
//! Each tracked conversation draws one `sizes.dash_height` circle through
//! [`style::container::status_dot`] — the same geometry vocabulary as
//! `columns.rs`'s dashes and `claude.rs`'s dots, so all three readouts look
//! like relatives. The color comes from the theme's [`SessionStatus`] (amber
//! = working, violet = subagents, red = attention, blue = done, green =
//! idle).
//!
//! **This is the panel's documented exception to "three colors, never a
//! fourth"** (Jordan's decision, 2026-07-31, scoped to Claude Code and
//! widened to agent sessions generally on 2026-08-14 — the exception was
//! reworded, not reopened). Five mutually exclusive states have to be told
//! apart at a glance, at 16 px, with no text. The exception is scoped to
//! these semaphore dots: the status hues never fill a control, a pill, a
//! border, or any text elsewhere.
//!
//! The two "still running" states breathe — their fill alpha animates between
//! `motion.breathe_min_opacity` and 1.0 over `motion.breathe` ms (see
//! [`breath_at`]) — and the three settled states are steady. That split is
//! itself information: **movement on the bar always means work in progress.**
//! See [`breathes`] for the predicate and [`Antigravity::subscription`] for
//! the animation timer and why it is allowed to exist.
//!
//! # No session-end event: the stale dot is worse here, and still not swept
//!
//! `claude.rs` documents a stale-session quirk — a session that dies without
//! ever sending `"ended"` leaves its last known status in the list forever.
//! For agy that is not an edge case but the **normal path**: there is no
//! `SessionEnd`-equivalent hook in agy's five events, so nothing routinely
//! emits `"ended"` at all. Antigravity dots therefore linger until the panel
//! restarts. (The `"ended"` arm still exists and works — a future agy hook,
//! or a hand-rolled wrapper, can use it — nothing emits it today.)
//!
//! This is **accepted, not fixed** (Jordan, 2026-08-14). The obvious remedy, a
//! TTL sweep dropping entries older than N minutes, needs a *timer* driving
//! the worker, which is exactly the poll CLAUDE.md forbids: every other
//! module's reconnect backoff only paces retries *while disconnected* and
//! never ticks while healthy, and a sweep has no equivalent "only while
//! unhealthy" excuse — it would have to wake on a schedule regardless of
//! whether anything changed. (The breath timer below is not a
//! counter-example: it is sanctioned, purely cosmetic, and gated on there
//! being something to animate.) A lingering dot is cosmetic, and it
//! self-heals the moment that same conversation id reports any status again.
//! Documented in the README's known-limitations section rather than swept.

use std::path::PathBuf;
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::stream::StreamExt;
use iced::futures::{SinkExt, Stream};
use iced::time::Instant;
use iced::widget::{container, row, Space};
use iced::{Element, Subscription};
use saola_theme::convert::ColorExt;
use saola_theme::style::container::SessionStatus;
use saola_theme::{style, Theme};
use zbus::{Connection, MatchRule, MessageStream};

use crate::icons::{self, Icon};

/// The Antigravity module's own message type (the per-module refactor's
/// shape — see `modules::clock::Message` for the full teaching note).
/// `main.rs` nests this as `Message::Antigravity(antigravity::Message)` and
/// delegates the whole thing to [`Antigravity::update`] rather than matching
/// the inner variants itself.
#[derive(Debug, Clone)]
pub enum Message {
    /// A fresh session list from the D-Bus worker (the full folded picture,
    /// not a delta — see the module doc comment).
    Updated(Sessions),
    /// A fresh rate-limit snapshot from the same worker — a `UsageChanged`
    /// signal's body, already parsed. Unlike [`Message::Updated`] this *is*
    /// the whole picture on its own (one account-wide snapshot, no fold), so
    /// applying it is a plain overwrite: newest signal wins.
    UsageUpdated(Usage),
    /// One frame of the breathing animation. Carries the tick's own
    /// [`Instant`] rather than being a unit variant like
    /// `clock::Message::Tick`: the phase is derived from *when* the tick
    /// happened, and taking that from the runtime's own timer means the
    /// module never reads a clock of its own (which would make
    /// [`Antigravity::update`] untestable and non-deterministic).
    Tick(Instant),
}

/// The object path `contrib/antigravity/emit.sh` targets and this module
/// listens on. Not a service path in the UPower/iwd sense (nothing is
/// *hosted* there) — it's just the fixed address a broadcast signal claims to
/// have come from. Deliberately distinct from `claude.rs`'s
/// `/io/saola/ClaudeCode`.
const ANTIGRAVITY_PATH: &str = "/io/saola/Antigravity";

/// The signal's interface. Versioned (`1`) per the D-Bus convention of baking
/// a revision number into a custom interface name, so a future breaking
/// change to the argument shape can ship as `...1` + `...2` coexisting rather
/// than an in-place break.
///
/// A **separate interface** from `io.saola.ClaudeCode1` rather than a shared
/// one with an agent-name argument (Jordan, 2026-08-14): two agents' session
/// ids are unrelated namespaces, and separate interfaces make it structurally
/// impossible for one agent's conversation to land in the other's fold.
///
/// Teaching note (no `.sender(...)` on the match rule built from this):
/// `busctl --user emit` doesn't request a well-known bus name before emitting
/// — it sends from whatever transient unique name (`:1.234`, different every
/// invocation) the session bus hands its short-lived connection. There is no
/// stable sender to filter on, by design; the interface + path + member triple
/// is the whole identity of this signal.
const ANTIGRAVITY_INTERFACE: &str = "io.saola.Antigravity1";

/// The per-conversation status signal: `StatusChanged(conversation_id: s,
/// status: s, transcript_path: s)`.
const STATUS_MEMBER: &str = "StatusChanged";

/// The account-wide quota signal: `UsageChanged(gemini_weekly_pct: d,
/// gemini_weekly_resets_at: t, third_party_weekly_pct: d,
/// third_party_weekly_resets_at: t)` — signature `dtdt`, slot 1 =
/// `gemini-weekly`, slot 2 = `3p-weekly`. See the module doc comment's schema
/// section for why it carries no conversation id, and why both slots are
/// weekly despite the signature outliving the names it was drafted with.
const USAGE_MEMBER: &str = "UsageChanged";

/// The per-conversation token signal: `TokensChanged(conversation_id: s,
/// input: t, output: t, context_size: t)`, from the statusline payload's
/// `context_window` block. Folded onto the session entry by id
/// ([`fold_tokens`]) — the only one of the two statusline-borne signals that
/// participates in the per-session fold at all.
const TOKENS_MEMBER: &str = "TokensChanged";

/// How often the breathing animation is redrawn while it is running.
///
/// A **frame budget, not a design token** — which is why it lives here as a
/// named constant rather than in saola-theme (the theme owns the breath's
/// *duration* and *depth*, `motion.breathe` and `motion.breathe_min_opacity`;
/// how finely the panel samples that curve is the panel's business). 100 ms
/// is roughly a twenty-fourth of the 2400 ms cycle, which is far more than
/// enough for a 16 px dot fading between two alphas: the eye reads the *rate*
/// of a slow fade, not its step count.
const BREATH_TICK: Duration = Duration::from_millis(100);

/// One tracked conversation: the id the emitter reports it under, and its
/// last known state.
///
/// [`SessionStatus`] is saola-theme's own enum ([`style::container::
/// status_dot`] takes it), deliberately reused rather than mirrored locally —
/// exactly as `claude.rs` and `columns.rs` reuse the theme's enums. The theme
/// is the authority on what states a semaphore dot can be in, and a parallel
/// enum here would be one more thing to keep in sync. It is reused **as-is**:
/// this module adds no variant and needs no theme bump.
///
/// `"ended"` has no variant in it — it doesn't produce a status, it removes
/// the entry entirely (see [`fold`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// agy's `conversationId` from the hook payload. Never displayed on the
    /// bar — it is the key the fold rewrites entries by (the usage popover
    /// shows a shortened form of it as a row label).
    id: String,
    /// What that conversation is doing, as of its last `StatusChanged`.
    status: SessionStatus,
    /// The conversation's `transcript.jsonl`, from the signal's third
    /// argument (agy's `transcriptPath`). `None` until a signal actually
    /// carries one — and a later empty payload never *clears* a known path
    /// (see [`Sessions::set`]): the transcript's location never changes
    /// within a conversation, so the last known value is always the best one.
    transcript: Option<PathBuf>,
    /// The conversation's token counters, from its last `TokensChanged`
    /// signal. `None` until one arrives — a conversation whose statusline
    /// isn't chained (or that hasn't refreshed yet) has a dot and no numbers,
    /// and the popover simply renders the row without them. Never cleared
    /// once set: like [`Session::transcript`], the last known value beats no
    /// value, and the statusline only ever revises these upward within a
    /// conversation.
    tokens: Option<Tokens>,
}

/// One conversation's token counters, as of its last `TokensChanged` signal —
/// agy's statusline payload `context_window.{total_input_tokens,
/// total_output_tokens, context_window_size}`, already parsed.
///
/// `Copy` for the same reason [`Usage`] is: three integers handed to the
/// popover by value is simpler than lending it a borrow across a `view`
/// boundary.
///
/// Cumulative session totals, **not** a delta — the statusline reports the
/// running counters on every refresh, so folding one of these is a plain
/// overwrite (see [`fold_tokens`]) rather than an addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    /// Tokens sent to the model this conversation, cumulative.
    pub input: u64,
    /// Tokens generated by the model this conversation, cumulative.
    pub output: u64,
    /// The model's context window in tokens — the denominator of the
    /// popover's context-fill gauge. `0` is possible on paper (a hand-typed
    /// `busctl` call, or a payload that omitted the field) and the popover
    /// treats it as "no gauge to draw" rather than dividing by it.
    pub context_size: u64,
}

/// One conversation's usage-popover row: enough to draw the dot, a shortened
/// id beside its status label, and — when a `TokensChanged` signal has
/// arrived for it — that conversation's token counters and context-fill
/// gauge. Owned values, deliberately (the popover's `view` is handed a
/// snapshot rather than borrowing `Sessions`' private entries directly — see
/// [`Antigravity::usage_targets`]).
///
/// **No `transcript` field** — unlike `modules::claude::UsageTarget`, which
/// this type mirrored before Stage 26. Stage 23's recon settled that agy's
/// `transcript.jsonl` carries no `usage` object and no model name at any
/// depth (flat step records, not Claude Code's `{"message": {...}}`
/// envelope), so `popovers::antigravity_usage` has nothing to read a
/// transcript path *for* — a field that's populated but never consumed for
/// its declared purpose is exactly the dead-looking-live code CLAUDE.md
/// warns against. That finding still stands and is *why* [`Tokens`] rides a
/// signal instead: the numbers come off the statusline, not off disk.
/// [`Session::transcript`] itself is untouched (stage 25's storage stays, for
/// wire schema symmetry with `io.saola.ClaudeCode1` — see the module doc
/// comment's schema section); only this row-request type stops carrying it
/// onward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageTarget {
    pub id: String,
    pub status: SessionStatus,
    /// This conversation's token counters, or `None` if no `TokensChanged`
    /// has ever named it. Absence is silent: the popover row renders without
    /// numbers rather than showing a zero it can't stand behind.
    pub tokens: Option<Tokens>,
}

/// One account-wide quota snapshot — a `UsageChanged` signal's body, as of
/// the moment agy's statusline last refreshed. Two windows, same shape each;
/// `Copy` because it is four numbers, and handing the popover its own copy is
/// simpler than lending it a borrow across a `view` boundary.
///
/// The field names are the capture's (2026-08-14, agy 1.1.13): agy's `quota`
/// map came back keyed `gemini-weekly` and `3p-weekly`, **both weekly**, and
/// these two fields are those two buckets in the frozen wire order. The
/// earlier `five_hour`/`seven_day` names were a drafting assumption borrowed
/// from Claude Code's payload and are retired; the `dtdt` signature they were
/// drafted alongside is *not* (see the module doc comment's schema section).
///
/// No timestamp field of its own: freshness is judged from
/// [`UsageWindow::resets_at`] (a reset in the past means the snapshot
/// predates it), which is what lets the stale case be detected at render time
/// without this module ever reading a clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    /// The `gemini-weekly` bucket — wire slot 1.
    pub gemini_weekly: UsageWindow,
    /// The `3p-weekly` bucket (third-party models) — wire slot 2.
    pub third_party_weekly: UsageWindow,
}

/// One quota window's gauge: how much of it is used, and when it resets.
///
/// Both fields are already in the panel's units, converted emitter-side: agy
/// reports `remaining_fraction` (0–1) and `reset_in_seconds`, and
/// `contrib/antigravity/statusline.sh` sends `(1 - remaining_fraction) * 100`
/// and `now + reset_in_seconds` so nothing on this side has to know the
/// payload's conventions or parse its ISO-8601 `reset_time` string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsageWindow {
    /// Used share of the window, `0.0..=100.0`.
    pub used_pct: f64,
    /// Unix epoch seconds of the window's next reset.
    pub resets_at: u64,
}

/// Every conversation this module currently knows about, in **first-seen
/// order** (see the module doc comment for why order is load-bearing and why
/// this is a `Vec` rather than a map).
///
/// A newtype rather than a bare `Vec<Session>` so the ordering invariant has
/// somewhere to be documented and enforced: [`Sessions::set`] is the only way
/// to add or update an entry, and it appends rather than reordering.
///
/// `PartialEq` is load-bearing, not a convenience derive: the worker compares
/// each freshly folded list against the last one it sent and suppresses
/// duplicates. That matters more here than it does for Claude Code — agy's
/// statusline re-emits `"seen"` several times a second, and after the first
/// one every single re-emit folds to an identical list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sessions {
    entries: Vec<Session>,
}

impl Sessions {
    /// Sets one conversation's status: rewrites the existing entry **in
    /// place** (keeping its position in the row) or appends a new one at the
    /// end. A `Some` transcript updates the stored path; `None` (a two-
    /// argument `ss` signal, or an empty-string payload) leaves whatever was
    /// already known — see [`Session::transcript`].
    fn set(&mut self, id: String, status: SessionStatus, transcript: Option<PathBuf>) {
        match self.entries.iter_mut().find(|session| session.id == id) {
            Some(session) => {
                session.status = status;
                if transcript.is_some() {
                    session.transcript = transcript;
                }
            }
            None => self.entries.push(Session {
                id,
                status,
                transcript,
                tokens: None,
            }),
        }
    }

    /// Attaches one conversation's token counters, overwriting whatever was
    /// there. A no-op for an id that isn't tracked — [`fold_tokens`] is the
    /// only caller and it guarantees the entry exists first, which is what
    /// keeps the "append in first-seen order" invariant in exactly one place
    /// ([`Sessions::set`]) instead of two.
    fn set_tokens(&mut self, id: &str, tokens: Tokens) {
        if let Some(session) = self.entries.iter_mut().find(|session| session.id == id) {
            session.tokens = Some(tokens);
        }
    }

    /// Drops a conversation entirely — its dot disappears and everything to
    /// its right shifts left. Removing an id that isn't tracked is a no-op.
    fn remove(&mut self, id: &str) {
        self.entries.retain(|session| session.id != id);
    }

    /// The tracked status of a conversation, if any — [`fold`]'s `"seen"` and
    /// `"subagent-done"` arms read this to decide whether their signal should
    /// do anything at all, without reaching past `Sessions` into `entries`
    /// itself.
    fn status_of(&self, id: &str) -> Option<SessionStatus> {
        self.entries
            .iter()
            .find(|session| session.id == id)
            .map(|session| session.status)
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Seeds one entry directly, for tests **outside this module** —
    /// specifically `popovers::antigravity_usage`, whose `view` needs a
    /// populated row list and can't build one: `entries` is private on
    /// purpose (the ordering invariant lives behind [`Sessions::set`]) and
    /// [`fold`] is private with it.
    ///
    /// `#[cfg(test)]` so it exists in no shipped binary, and deliberately
    /// *not* a way around the fold for production code: everything real still
    /// goes through [`fold`]/[`fold_tokens`].
    #[cfg(test)]
    pub(crate) fn seed(&mut self, id: &str, status: SessionStatus, tokens: Option<Tokens>) {
        self.set(id.to_string(), status, None);
        if let Some(tokens) = tokens {
            self.set_tokens(id, tokens);
        }
    }

    /// Whether anything in the row wants the animation timer running — the
    /// gate [`Antigravity::subscription`] consults. `false` for an empty
    /// list, so a panel with no conversations runs no timer at all.
    fn any_breathing(&self) -> bool {
        self.entries.iter().any(|session| breathes(session.status))
    }
}

/// Antigravity module state: the last session list the worker pushed, plus
/// the two fields that drive the breathing animation.
///
/// Unlike `Battery`/`Network`, there's no separate `present` flag: an empty
/// session list already means "render nothing," whatever the reason (no
/// signal ever received, or every conversation ended). Settled conversations
/// still render — a blue "done" or green "idle" dot is information.
///
/// Note what is *absent* compared to `modules::claude::ClaudeCode`: there is
/// no `icon` field, because there is no `antigravity-icon` config knob to
/// store. `claude-icon` exists only because Claude Code ships two brand marks
/// worth choosing between; Antigravity ships one, so [`Antigravity::view`]
/// names [`Icon::Antigravity`] directly and `Antigravity::default()` is the
/// whole of construction (no `new`, no `set_icon`, nothing for a config
/// reload to swap).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Antigravity {
    /// The row, in first-seen order.
    sessions: Sessions,
    /// The last rate-limit snapshot the worker pushed, if any ever arrived —
    /// deliberately separate from the per-session list (rate limits are
    /// account-wide; see the module doc comment's schema section). `None`
    /// until the first `UsageChanged` signal, which is the same "no signal
    /// yet renders nothing" contract the session list follows: an agy config
    /// with no statusline chained simply never populates this.
    usage: Option<Usage>,
    /// When the current run of breathing started. `None` while nothing is
    /// animating; set from the first [`Message::Tick`] after the timer
    /// starts, and cleared again when the last breathing conversation settles
    /// so the next run begins from a fresh, dim phase rather than resuming
    /// mid-fade.
    ///
    /// Teaching note (why an epoch rather than an accumulator): the obvious
    /// implementation — `phase += tick_interval` every frame, wrapped into
    /// range — accumulates the error of every single tick, and a timer that
    /// fires a millisecond late (which every OS timer does) drifts a little
    /// further out of step forever. Storing the instant the run *started* and
    /// subtracting means each frame's phase is computed from scratch against
    /// real elapsed time, so a late or dropped tick costs that one frame and
    /// nothing after it.
    breath_epoch: Option<Instant>,
    /// How long the current breathing run has been going, as of the last
    /// tick. Turned into an opacity at render time by [`phase_of`] +
    /// [`breath_at`] — those need `motion.breathe` and
    /// `motion.breathe_min_opacity`, and the theme is only in hand inside
    /// [`Antigravity::view`].
    breath_elapsed: Duration,
}

impl Antigravity {
    /// Folds one of this module's messages into its state. `main.rs`'s
    /// `Panel::update` unwraps the outer `Message::Antigravity(..)` and hands
    /// the inner value straight here, so all of this module's update logic
    /// lives in this file rather than being spread across match arms in
    /// `main.rs` (the per-module refactor's whole point).
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Updated(sessions) => {
                self.sessions = sessions;
                // Nothing left to animate: drop the epoch so the timer's next
                // run starts from phase 0 (the dim end) instead of resuming
                // wherever the previous run happened to stop. Harmless either
                // way — the subscription has already gone quiet — but it
                // keeps the state honest.
                if !self.sessions.any_breathing() {
                    self.breath_epoch = None;
                    self.breath_elapsed = Duration::ZERO;
                }
            }
            // Newest snapshot wins, unconditionally — the emitter is
            // stateless and rebroadcasts on every statusline refresh, so this
            // arm has to be an idempotent overwrite (see the module doc
            // comment's schema section). It deliberately doesn't touch the
            // breath state: usage is popover data, not bar animation.
            Message::UsageUpdated(usage) => self.usage = Some(usage),
            Message::Tick(now) => {
                // `get_or_insert` is what makes the *first* tick of a run
                // establish the epoch: no separate "start the animation"
                // message is needed, because the first frame after the
                // subscription starts is exactly the moment the run began.
                let epoch = *self.breath_epoch.get_or_insert(now);
                // `saturating_duration_since` rather than `now - epoch`:
                // subtracting `Instant`s panics if the result would be
                // negative, and while the runtime's timer should never hand
                // us a tick older than the epoch, a panic on the UI thread is
                // not the way to find out otherwise.
                self.breath_elapsed = now.saturating_duration_since(epoch);
            }
        }
    }

    /// Whether this module would draw anything right now — the presence
    /// question `main.rs` asks before spending a bar group (ledger) or an
    /// island pill (islands) on it. Asks the same emptiness as `view`'s early
    /// return, so the two cannot drift apart.
    pub fn is_present(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// The last rate-limit snapshot, for the usage popover — a copy, not a
    /// borrow, for the same detached-render reason as [`Self::usage_targets`]'s
    /// owned values. `None` means no `UsageChanged` signal has ever arrived
    /// (no statusline chained, or no response yet) and the popover simply
    /// omits its gauges.
    ///
    /// Called from `popovers::antigravity_usage::view` (Stage 26) and this
    /// module's tests.
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    /// One usage-popover row per tracked conversation, in row order: the id,
    /// its current dot, and its token counters if any have arrived. Snapshot
    /// semantics — `popovers::
    /// antigravity_usage::view` reads this straight off `self` on every
    /// render (there is no fetch task to own the values; see that module's
    /// doc comment for why a synchronous view over this module's own state
    /// is the whole story, unlike Claude Code's transcript-reading
    /// popover), so a session list that changes between clicks simply shows
    /// the row as of the render that just happened.
    pub fn usage_targets(&self) -> Vec<UsageTarget> {
        self.sessions
            .entries
            .iter()
            .map(|session| UsageTarget {
                id: session.id.clone(),
                status: session.status,
                tokens: session.tokens,
            })
            .collect()
    }

    /// Renders the module's glyph and the dot row — or nothing at all when no
    /// conversation is tracked (`Space::new()` with no size is a zero-area
    /// widget — the region simply closes up around it).
    ///
    /// The leading brand mark ([`Icon::Antigravity`], named directly: unlike
    /// `modules::claude` there is no icon knob to switch on, because
    /// Antigravity has exactly one mark) is a bare ivory bar icon, exactly
    /// like `network`/`battery`'s glyphs: `icon_bar`-sized, `on_ink.primary`,
    /// steady — it never breathes and never takes a status color; the dots
    /// alone carry state. It is what makes the row read as *agy's* dots now
    /// that the module sits in its own group beside Claude Code's, and it is
    /// the visible face of the usage-popover trigger `main.rs` wraps this
    /// view in (the module itself stays click-ignorant, like every module).
    ///
    /// Built exactly like `columns.rs`'s dash strip, deliberately: a
    /// `container` draws its background across its own bounds, so the dot *is*
    /// the container and the zero-area `Space` is just the child it needs to
    /// have. `sizes.dash_height` square plus `status_dot`'s `radii.pill`
    /// closes it into a circle, and the row is spaced with the same
    /// `sizes.dash_gap` the minimap uses. Every value is a token; the only
    /// number this module contributes is the breath opacity, which is an
    /// animation phase rather than a color or a size.
    ///
    /// Not a `canvas`: hand-drawing the dots would mean reaching for raw
    /// `Color` values, which is precisely the local restyling CLAUDE.md
    /// forbids.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if self.sessions.is_empty() {
            return Space::new().into();
        }

        // Computed once for the whole row, not per dot: every breathing
        // conversation shares one phase, so the row pulses together rather
        // than shimmering out of step.
        let phase = phase_of(self.breath_elapsed, theme.motion.breathe);
        let breath = breath_at(phase, theme.motion.breathe_min_opacity);

        let dots = self.sessions.entries.iter().map(|session| {
            let opacity = if breathes(session.status) {
                breath
            } else {
                1.0
            };
            container(Space::new())
                .width(theme.sizes.dash_height)
                .height(theme.sizes.dash_height)
                .style(style::container::status_dot(theme, session.status, opacity))
                .into()
        });

        // The function form of `row` (not the `row!` macro) because the
        // children are a runtime-length iterator, not a fixed list. The dots
        // keep their own `dash_gap` rhythm; the glyph sits a `bar_icon_gap`
        // ahead of them, the same icon-to-content gap the other bare status
        // readouts use.
        iced::widget::row![
            icons::icon(
                Icon::Antigravity,
                theme.sizes.icon_bar,
                theme.on_ink.primary.into_iced(),
            ),
            row(dots)
                .spacing(theme.sizes.dash_gap)
                .align_y(iced::Center),
        ]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center)
        .into()
    }

    /// The Antigravity signal feed, plus — only while something is actually
    /// breathing — the animation timer.
    ///
    /// **Teaching note (the sanctioned exception to "nothing ticks faster
    /// than the clock").** CLAUDE.md's architecture rule is that every module
    /// maps to a signal and never to a poll. Jordan sanctioned this animation
    /// on 2026-07-31 for the Claude Code dots; this module inherits the same
    /// exception with the same boundary:
    ///
    /// - It is **not a poll.** A poll asks a source "has anything changed?"
    ///   on a schedule. This timer asks nothing and reads nothing — the
    ///   session *state* still arrives only by D-Bus signal. The timer only
    ///   advances an opacity that is, by definition, a function of time. Same
    ///   distinction that lets `columns.rs` and `volume.rs` sleep between
    ///   reconnect attempts without breaking the rule.
    /// - It is **gated.** The `iced::time::every` subscription exists only
    ///   while at least one conversation is `Working` or `Subagents` (see
    ///   [`Sessions::any_breathing`]); with a settled row — or no
    ///   conversations at all, which is the overwhelmingly common case — this
    ///   returns the bus worker alone and the panel is fully idle again. An
    ///   always-on 10 Hz timer would be the poll the rule forbids, wearing an
    ///   animation's clothes.
    ///
    /// Teaching note (subscription identity): iced recomputes
    /// `Panel::subscription` after every message and diffs the result.
    /// `Subscription::run(antigravity_stream)` keys on the *fn pointer*, so
    /// the bus worker is spawned once and survives every recomputation —
    /// including the ones where the branch below adds or drops the timer
    /// beside it. Because it keys on the function, `modules::claude`'s worker
    /// and this one are distinct subscriptions even though their bodies are
    /// near-identical; both run, each on its own interface.
    /// `iced::time::every` keys on its `Duration`, which is a constant here —
    /// and the *same* constant `claude.rs` uses, so iced treats the two
    /// modules' breath timers as one subscription and both rows pulse off it
    /// (which is what a viewer would expect anyway: one bar, one breath).
    pub fn subscription(&self) -> Subscription<Message> {
        let worker = Subscription::run(antigravity_stream);

        if !self.sessions.any_breathing() {
            return worker;
        }

        Subscription::batch([worker, iced::time::every(BREATH_TICK).map(Message::Tick)])
    }
}

/// Whether a status animates. The two "still running" states breathe; the
/// three settled ones are steady — movement on the bar always means work in
/// progress (see the module doc comment).
///
/// A free function rather than a method because [`SessionStatus`] is
/// saola-theme's type, not this crate's: Rust's orphan rule means only the
/// defining crate may add inherent impls to it. Exhaustive `match` (no `_`
/// arm) on purpose — a sixth status added to the theme becomes a compile
/// error here, which is the only place that would otherwise silently guess.
fn breathes(status: SessionStatus) -> bool {
    match status {
        SessionStatus::Working | SessionStatus::Subagents => true,
        SessionStatus::Attention | SessionStatus::Done | SessionStatus::Idle => false,
    }
}

/// Where in the breath cycle `elapsed` lands, as a fraction in `0.0..1.0`.
///
/// The modulo is done in **integer milliseconds** before any float division:
/// that is what keeps a run that has been breathing for an hour as precise as
/// one that started a second ago, instead of losing bits to an ever-growing
/// `f32`.
///
/// A zero (or absurd) `cycle_ms` would be a division by zero, so it degrades
/// to phase 0 — a steady dot at the dim end rather than a `NaN` the theme
/// would have to defend against.
fn phase_of(elapsed: Duration, cycle_ms: u32) -> f32 {
    if cycle_ms == 0 {
        return 0.0;
    }
    let cycle_ms = u128::from(cycle_ms);
    (elapsed.as_millis() % cycle_ms) as f32 / cycle_ms as f32
}

/// The opacity multiplier at a given phase: `min` at the ends of the cycle,
/// `1.0` at its midpoint, moving as a cosine in between.
///
/// Teaching note (why a cosine and not a triangle): the obvious "count up,
/// then count down" ramp is a *sawtooth in velocity* — the dot changes
/// brightness at a constant rate and then reverses direction instantly at
/// each end, which the eye reads as a flicker or a twitch, not a breath.
/// `(1 - cos(2πp)) / 2` eases in and out: its slope is zero at both turning
/// points, so the fade slows to a stop and reverses smoothly. The result is
/// then mapped from `0.0..=1.0` onto `min..=1.0`, so the dot never disappears
/// entirely at the dim end.
fn breath_at(phase: f32, min: f32) -> f32 {
    let eased = (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0;
    min + (1.0 - min) * eased
}

/// Folds one `StatusChanged` event into the session list.
///
/// `"ended"` removes the entry outright rather than setting it to some
/// "ended" variant — a conversation with no entry and one that reported
/// `"ended"` must render identically (as nothing), and deleting the entry is
/// what makes that true for free instead of needing the view to filter out an
/// `Ended` state on every render. **Nothing emits `"ended"` today**: agy has
/// no `SessionEnd`-equivalent hook, so in practice dots linger until the
/// panel restarts (see the module doc comment's last section — accepted, no
/// TTL sweep). The arm is kept because it is the *only* way an entry can ever
/// leave the row, and a future agy hook (or a wrapper script) can use it.
///
/// An unrecognized `status` string (a future emitter revision, a typo in a
/// hand-edited `hooks.json` command) is ignored — the entry, if any, keeps
/// its last *known* value rather than being overwritten with something this
/// module can't render, and an unknown conversation isn't conjured into
/// existence by a status nobody can draw. Pure function of its arguments (no
/// D-Bus, no clock), which is what makes it unit-testable below without a
/// bus.
///
/// The wire's vocabulary is frozen with `contrib/antigravity/emit.sh`:
/// `working` (agy is generating — the `PreInvocation` hook), `attention`
/// (`Stop` with an `error`), `done` (`Stop` without one), `idle` (an explicit
/// "open, nothing happening"), `seen` (the statusline's session-discovery
/// signal, below), `ended` (gone).
///
/// **`"seen"` is folded conditionally**, and it is the one arm with no
/// counterpart in `modules::claude`. agy has no `SessionStart` hook, so
/// nothing fires when a conversation opens; the statusline command — which
/// re-broadcasts several times a second — is the only agy surface that runs
/// at session open. A plain `"idle"` from there would stomp a live `working`
/// dot on every refresh, so the statusline emits `"seen"` and this arm treats
/// it as "make sure this conversation has a dot, and otherwise keep out of
/// the way": insert as [`SessionStatus::Idle`] only when the conversation
/// isn't tracked yet, and **never** overwrite a tracked one. Structurally the
/// same conditional shape as `claude.rs`'s `"subagent-done"` arm, for an
/// entirely different reason.
///
/// **`"subagent"` and `"subagent-done"` are kept for forward compatibility
/// and nothing emits them today** — agy's five hook events include no
/// `SubagentStart`/`SubagentStop` equivalent, so no agy configuration can
/// produce either string. They are deliberately *not* dead code to be swept:
/// if agy grows subagent hooks the wire vocabulary is already in place and
/// matches Claude Code's. Equally, do not assume they are exercised — a
/// change to either arm ships untested by any live emitter. `"subagent-done"`
/// keeps `claude.rs`'s conditional semantics verbatim (flip `Subagents` back
/// to `Working`, no-op against anything else) so the two agents' folds don't
/// quietly diverge if agy ever does grow the hooks.
///
/// `transcript` is the signal's third argument with `""` already mapped to
/// `None` by the caller — the empty string is the emitter's "I had no payload
/// to read", not a real path. Both conditional arms route through
/// [`Sessions::set`] exactly like every other status, so a transcript path
/// riding along on one of those signals is captured the same way.
fn fold(
    sessions: &mut Sessions,
    conversation_id: String,
    status: &str,
    transcript: Option<PathBuf>,
) {
    match status {
        "working" => sessions.set(conversation_id, SessionStatus::Working, transcript),
        // Forward compatibility only — no agy hook emits this today (see the
        // doc comment above).
        "subagent" => sessions.set(conversation_id, SessionStatus::Subagents, transcript),
        // Forward compatibility only — no agy hook emits this today.
        "subagent-done" => {
            if sessions.status_of(&conversation_id) == Some(SessionStatus::Subagents) {
                sessions.set(conversation_id, SessionStatus::Working, transcript);
            }
        }
        "attention" => sessions.set(conversation_id, SessionStatus::Attention, transcript),
        "done" => sessions.set(conversation_id, SessionStatus::Done, transcript),
        "idle" => sessions.set(conversation_id, SessionStatus::Idle, transcript),
        "seen" => {
            // Insert as Idle only if this conversation isn't tracked yet; a
            // tracked session keeps whatever status its hooks last set. The
            // statusline is stateless and re-broadcasts constantly, so this
            // arm must be idempotent and must never overwrite.
            if sessions.status_of(&conversation_id).is_none() {
                sessions.set(conversation_id, SessionStatus::Idle, transcript);
            }
        }
        "ended" => sessions.remove(&conversation_id),
        _ => {}
    }
}

/// Parses a `UsageChanged` message's body into a [`Usage`] snapshot, or `None`
/// for anything that doesn't match the declared `dtdt` signature (a hand-typed
/// `busctl` call with the wrong types) — the same skip-don't-die posture as
/// the `StatusChanged` body parse in [`watch_antigravity`].
///
/// A free function taking the [`zbus::Message`] itself (rather than four loose
/// floats and ints) so the tests below can exercise the *real* parse,
/// wrong-body rejection included, against messages built with
/// [`zbus::Message::signal`] — no bus required: a `Message` is just a
/// serialized frame, and building one is pure.
fn parse_usage(message: &zbus::Message) -> Option<Usage> {
    let (gemini_pct, gemini_resets_at, third_party_pct, third_party_resets_at) =
        message.body().deserialize::<(f64, u64, f64, u64)>().ok()?;
    Some(Usage {
        gemini_weekly: UsageWindow {
            used_pct: gemini_pct,
            resets_at: gemini_resets_at,
        },
        third_party_weekly: UsageWindow {
            used_pct: third_party_pct,
            resets_at: third_party_resets_at,
        },
    })
}

/// Parses a `TokensChanged` message's body into the conversation id it names
/// and its [`Tokens`], or `None` for anything that doesn't match the declared
/// `sttt` signature — same skip-don't-die posture as [`parse_usage`], and the
/// same reason for taking the [`zbus::Message`] itself: the tests below
/// exercise the real parse (wrong-body rejection included) against frames
/// built with [`zbus::Message::signal`], which needs no bus at all.
///
/// Note `t` (`u64`) for all three counters, not `u32`: `context_size` is
/// already 1 048 576 on the model Jordan runs, and cumulative input on a long
/// conversation is not a number worth being clever about the width of.
fn parse_tokens(message: &zbus::Message) -> Option<(String, Tokens)> {
    let (conversation_id, input, output, context_size) = message
        .body()
        .deserialize::<(String, u64, u64, u64)>()
        .ok()?;
    Some((
        conversation_id,
        Tokens {
            input,
            output,
            context_size,
        },
    ))
}

/// Folds one `TokensChanged` event onto its conversation's entry.
///
/// **Creates the entry if it doesn't exist yet**, as [`SessionStatus::Idle`]
/// — the same insert `fold`'s `"seen"` arm makes, for a related reason.
/// agy's statusline runs at session open and refreshes several times a
/// second, while the hooks only fire around an actual turn, so tokens can
/// legitimately arrive for a conversation before anything has ever reported a
/// status for it. Dropping them (or worse, dropping the whole conversation)
/// because no hook has fired yet would lose the row agy's *only* at-open
/// surface just told us about.
///
/// Unlike `"seen"`, the numbers themselves are an unconditional
/// **last-write-wins overwrite**: they are cumulative counters the emitter
/// re-broadcasts in full on every refresh, not deltas to accumulate, so the
/// newest report is by definition the most complete one. That also makes this
/// arm idempotent — a repeat of the same numbers folds to a byte-identical
/// [`Sessions`], which the worker's dedupe then keeps off the UI thread
/// entirely.
///
/// A status this conversation already had is never touched: tokens and status
/// are independent facts arriving on independent signals.
///
/// Pure function of its arguments (no D-Bus, no clock), like [`fold`] — which
/// is what makes it unit-testable below without a bus.
fn fold_tokens(sessions: &mut Sessions, conversation_id: String, tokens: Tokens) {
    if sessions.status_of(&conversation_id).is_none() {
        sessions.set(conversation_id.clone(), SessionStatus::Idle, None);
    }
    sessions.set_tokens(&conversation_id, tokens);
}

/// Which of the interface's three signals just arrived — the worker's
/// dispatch, resolved from the message's member name before its body is
/// touched (the body type differs per signal, so there is nothing to read
/// until this is known).
///
/// A tiny private enum rather than the pair of booleans a third signal would
/// have turned the old `is_usage` flag into: the compiler checks the match on
/// this is exhaustive, so a fourth signal added here can't be silently
/// forgotten in [`watch_antigravity`].
enum Incoming {
    /// `StatusChanged` — per-session, folds a dot.
    Status,
    /// `UsageChanged` — account-wide quota snapshot, no fold.
    Usage,
    /// `TokensChanged` — per-session token counters, folds onto the entry.
    Tokens,
}

/// Builds the async stream the subscription runs. See `battery.rs`'s
/// `battery_stream` for the full bridge teaching note (the channel, the
/// runtime it runs on). Every failure path here — no session bus, the match
/// rule failing to register, the message stream ending — funnels into "send
/// the hidden/default state, worker ends quietly," same contract as every
/// other module: the panel never goes down because agy's hooks aren't wired
/// up (or haven't fired yet).
fn antigravity_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        if watch_antigravity(&mut sender).await.is_err() {
            let _ = sender.send(Message::Updated(Sessions::default())).await;
        }
    })
}

/// The worker proper: register the match rule, then fold and re-ship the
/// session list on every matching signal, forever.
///
/// Teaching note (async ownership): `sender` is borrowed mutably for the
/// whole call rather than moved, because the caller above needs it back to
/// send the default state if this returns `Err`. Everything else the worker
/// touches — the connection, the rule, the stream, the two dedupe caches — is
/// owned locally and dropped when the task ends, which is exactly what makes
/// "worker ends quietly" a complete cleanup story.
///
/// Unlike `columns.rs`'s niri socket, there's no reconnect-with-backoff loop
/// here — a lost session-bus connection is a rare, session-ending event (on
/// par with the desktop session itself dying), not something this module
/// retries through, matching `battery`/`network`/`media`'s "one connection
/// attempt, worker ends quietly on loss" shape rather than `columns`' "keep
/// trying" one.
async fn watch_antigravity(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    // Session bus: agy's hooks and statusline are per-user processes (they run
    // as Jordan, from Jordan's terminal), not a system service — same
    // reasoning as `media.rs`'s MPRIS players and `claude.rs`'s hooks.
    let connection = Connection::session().await?;

    // Teaching note (`MatchRule`, not a proxy): see the module doc comment for
    // why a proxy doesn't fit here. `msg_type(Signal)` restricts the rule to
    // signals (as opposed to method calls/returns/errors, which share the same
    // bus but are irrelevant here); `path` + `interface` narrow it to this
    // module's broadcasts. No `.sender(...)` — see `ANTIGRAVITY_INTERFACE`'s
    // doc comment.
    //
    // Deliberately no `.member(...)`: one interface-wide rule feeding one
    // worker that dispatches on the member name reads better than two parallel
    // rule/stream/loop stacks differing only in their body types — and a match
    // rule is the bus-side filter anyway, so the only messages reaching the
    // dispatch below are this interface's own. Note that "this interface's
    // own" is what keeps agy's conversations out of `claude.rs`'s fold and
    // vice versa: the two workers are watching two different interfaces on the
    // same bus.
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(ANTIGRAVITY_PATH)?
        .interface(ANTIGRAVITY_INTERFACE)?
        .build();

    // `for_match_rule` registers the rule with the bus and returns a `Stream`
    // of every message matching it — filtered bus-side, so this task is never
    // woken for a signal it doesn't care about. `Some(8)` matches every other
    // worker's channel capacity in this crate.
    let mut signals = MessageStream::for_match_rule(rule, &connection, Some(8)).await?;

    let mut sessions = Sessions::default();
    // Dedupe against the last sent value. Load-bearing here in a way it only
    // half was for Claude Code: agy's statusline emits `"seen"` *and*
    // `TokensChanged` on every refresh (~300 ms while generating), and both
    // fold — by design, see `fold`'s `"seen"` arm and `fold_tokens` — to a
    // byte-identical list whenever nothing actually moved. Without this, each
    // of those would wake the UI thread for a no-op and re-enter
    // `Antigravity::update`, whose `Updated` arm resets the breath epoch.
    let mut last_sent: Option<Sessions> = None;
    // The usage snapshot's own dedupe, load-bearing for the same reason:
    // `statusline.sh` re-emits `UsageChanged` on every refresh, and the numbers
    // only actually move once per API response — so most of those signals are
    // exact repeats this suppression keeps off the UI thread.
    let mut last_usage: Option<Usage> = None;

    while let Some(message) = signals.next().await {
        // A message that fails to arrive cleanly (a transport-level error) is
        // skipped, not fatal — one bad frame shouldn't tear down every other
        // conversation's status. `MatchRule` already guarantees anything that
        // *does* arrive here matches interface/path, so the member name is the
        // only thing left to look at before reading the body.
        let Ok(message) = message else {
            continue;
        };

        // Dispatch on the member name — the one branch point the widened match
        // rule buys, and the reason the rule deliberately carries no
        // `.member(...)` filter. An unknown member (a future fourth signal
        // this build predates) is skipped, same posture as an unknown status
        // string in `fold`.
        //
        // Two of the three arms end in the same "re-ship the folded list if it
        // changed" tail below, so this resolves the member first and branches
        // once, rather than duplicating that tail per signal.
        let incoming = match message.header().member().map(|member| member.as_str()) {
            Some(USAGE_MEMBER) => Incoming::Usage,
            Some(TOKENS_MEMBER) => Incoming::Tokens,
            Some(STATUS_MEMBER) => Incoming::Status,
            _ => continue,
        };

        match incoming {
            // Account-wide: never touches `sessions`, so it has its own
            // dedupe and its own message, and returns to the top of the loop
            // without going near the session tail below.
            Incoming::Usage => {
                let Some(usage) = parse_usage(&message) else {
                    continue;
                };
                if last_usage == Some(usage) {
                    continue;
                }
                if sender.send(Message::UsageUpdated(usage)).await.is_err() {
                    return Ok(());
                }
                last_usage = Some(usage);
                continue;
            }
            // Per-session: folds onto the session list exactly like a status
            // does, and rides the same `Message::Updated` (the whole folded
            // picture) and the same dedupe. No token-specific message exists
            // for the same reason there's no transcript-specific one — the
            // session list *is* the picture.
            Incoming::Tokens => {
                let Some((conversation_id, tokens)) = parse_tokens(&message) else {
                    continue;
                };
                fold_tokens(&mut sessions, conversation_id, tokens);
            }
            Incoming::Status => {
                // The signal's signature is `sss` (`conversation_id`,
                // `status`, `transcript_path`) per the bus schema — with a
                // two-argument `ss` also accepted so a partially-updated
                // emitter keeps its status dots (it merely reports no
                // transcript). Any other body (a hand-typed `busctl` call with
                // the wrong types) is skipped the same way a malformed niri
                // event line is in `columns.rs`: keep the session alive rather
                // than tearing the whole worker down over one bad payload.
                let body = message.body();
                let (conversation_id, status, transcript) = match body
                    .deserialize::<(String, String, String)>()
                {
                    Ok(parsed) => parsed,
                    Err(_) => match body.deserialize::<(String, String)>() {
                        Ok((conversation_id, status)) => (conversation_id, status, String::new()),
                        Err(_) => continue,
                    },
                };
                // `""` is the emitter's "no payload to read", not a path.
                let transcript = (!transcript.is_empty()).then(|| PathBuf::from(transcript));

                fold(&mut sessions, conversation_id, &status, transcript);
            }
        }

        if last_sent.as_ref() == Some(&sessions) {
            continue;
        }
        if sender
            .send(Message::Updated(sessions.clone()))
            .await
            .is_err()
        {
            // Receiving side gone (subscription dropped) — stop quietly.
            return Ok(());
        }
        last_sent = Some(sessions.clone());
    }

    // The stream ended — the session bus connection itself is gone. Rare (see
    // this fn's doc comment), and not retried; returning `Ok` here (rather than
    // an error the wrapper would turn into a default-state send) matches
    // `battery`/`network`'s "worker ends quietly" contract for this same case.
    // The accepted cost is the same lingering-dot one the module doc comment
    // already describes at length, which for agy is the normal ending anyway.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transcript-less fold — the shape most status tests in this module want,
    /// kept as a shim so those read as the status-fold tests they are. The
    /// transcript argument's own behavior is covered separately below.
    fn fold(sessions: &mut Sessions, conversation_id: String, status: &str) {
        super::fold(sessions, conversation_id, status, None);
    }

    /// The statuses in the row, in order — what the dots actually look like,
    /// minus the ids nobody sees.
    fn statuses(sessions: &Sessions) -> Vec<SessionStatus> {
        sessions
            .entries
            .iter()
            .map(|session| session.status)
            .collect()
    }

    /// The ids in the row, in order — the ordering assertions' subject.
    fn ids(sessions: &Sessions) -> Vec<&str> {
        sessions
            .entries
            .iter()
            .map(|session| session.id.as_str())
            .collect()
    }

    // -- the fold ----------------------------------------------------------

    #[test]
    fn fold_maps_every_wire_status_to_its_dot() {
        // The frozen wire vocabulary, one string per theme variant. `seen` is
        // not here because it isn't a status of its own — it maps to `Idle`
        // conditionally, which its own tests below cover.
        for (wire, expected) in [
            ("working", SessionStatus::Working),
            ("subagent", SessionStatus::Subagents),
            ("attention", SessionStatus::Attention),
            ("done", SessionStatus::Done),
            ("idle", SessionStatus::Idle),
        ] {
            let mut sessions = Sessions::default();
            fold(&mut sessions, "a".to_string(), wire);
            assert_eq!(statuses(&sessions), vec![expected], "wire status {wire:?}");
        }
    }

    #[test]
    fn fold_rewrites_a_known_conversation_in_place() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "a".to_string(), "done");

        // One conversation, not three — the id is the key.
        assert_eq!(ids(&sessions), vec!["a"]);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Done]);
    }

    #[test]
    fn fold_ended_removes_the_conversation() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        assert_eq!(ids(&sessions), vec!["a"]);

        fold(&mut sessions, "a".to_string(), "ended");
        assert!(sessions.is_empty());
    }

    #[test]
    fn fold_ending_an_unknown_conversation_is_a_no_op() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "never-seen".to_string(), "ended");
        assert!(sessions.is_empty());
    }

    #[test]
    fn fold_ignores_unrecognized_status_strings() {
        let mut sessions = Sessions::default();
        // A future emitter revision (or a typo) sending a status this module
        // doesn't know: the conversation is left untracked rather than getting
        // a guessed-at dot color.
        fold(&mut sessions, "a".to_string(), "compacting");
        assert!(sessions.is_empty());

        // Same, but for an already-tracked conversation: the last known-good
        // status survives rather than being clobbered.
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "a".to_string(), "compacting");
        assert_eq!(statuses(&sessions), vec![SessionStatus::Working]);
    }

    // -- `seen`: agy's substitute for a SessionStart hook -------------------

    #[test]
    fn fold_seen_inserts_an_untracked_conversation_as_idle() {
        // The whole reason this status exists: agy has no `SessionStart` hook,
        // so the statusline's `seen` is what puts a dot on the bar when a
        // conversation opens. An untracked id becomes a green idle dot.
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "seen");
        assert_eq!(ids(&sessions), vec!["a"]);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Idle]);

        // And it is idempotent: the statusline re-broadcasts several times a
        // second, and every repeat after the first must be a no-op — one dot,
        // still idle, no second entry.
        fold(&mut sessions, "a".to_string(), "seen");
        fold(&mut sessions, "a".to_string(), "seen");
        assert_eq!(ids(&sessions), vec!["a"]);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Idle]);
    }

    #[test]
    fn fold_seen_never_overwrites_a_tracked_conversation() {
        // The failure this arm's conditionality prevents: the statusline fires
        // several times a second *while agy is generating*, so an
        // unconditional `Idle` write would stomp the amber working dot on
        // every refresh. Whatever the hooks last set must survive — checked
        // against all three statuses a hook can produce.
        for hook_status in ["working", "done", "attention"] {
            let mut sessions = Sessions::default();
            fold(&mut sessions, "a".to_string(), hook_status);
            let before = statuses(&sessions);

            fold(&mut sessions, "a".to_string(), "seen");
            assert_eq!(
                statuses(&sessions),
                before,
                "after seen over {hook_status:?}"
            );

            // Still one entry — `seen` must not append a duplicate either.
            assert_eq!(ids(&sessions), vec!["a"]);
        }
    }

    #[test]
    fn fold_seen_leaves_the_row_order_alone() {
        // `seen` for an already-tracked conversation is a no-op, so it must not
        // move that conversation's dot — Jordan reads the row positionally.
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "b".to_string(), "done");
        fold(&mut sessions, "a".to_string(), "seen");
        assert_eq!(ids(&sessions), vec!["a", "b"]);
    }

    // -- the forward-compatible subagent arms (nothing emits these today) ---

    #[test]
    fn fold_subagent_done_while_subagents_flips_to_working() {
        // Unreachable by any agy configuration today (no subagent hooks
        // exist); kept in lockstep with `modules::claude`'s semantics so the
        // two folds can't diverge if agy ever grows them.
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "subagent");
        fold(&mut sessions, "a".to_string(), "subagent-done");
        assert_eq!(statuses(&sessions), vec![SessionStatus::Working]);
    }

    #[test]
    fn fold_subagent_done_against_a_settled_dot_is_a_no_op() {
        // Same forward-compatibility caveat as above. The conditionality is
        // what stops a late straggler from stomping a settled dot back to
        // amber with nothing left to correct it.
        for settled in ["done", "idle", "attention"] {
            let mut sessions = Sessions::default();
            fold(&mut sessions, "a".to_string(), settled);
            let before = statuses(&sessions);
            fold(&mut sessions, "a".to_string(), "subagent-done");
            assert_eq!(
                statuses(&sessions),
                before,
                "subagent-done over {settled:?}"
            );
        }
    }

    #[test]
    fn fold_subagent_done_for_an_unknown_conversation_creates_no_entry() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "never-seen".to_string(), "subagent-done");
        assert!(sessions.is_empty());
    }

    // -- the transcript path -----------------------------------------------

    #[test]
    fn a_signal_with_a_transcript_records_it() {
        let mut sessions = Sessions::default();
        super::fold(
            &mut sessions,
            "a".to_string(),
            "working",
            Some(PathBuf::from("/t/a.jsonl")),
        );
        assert_eq!(
            sessions.entries[0].transcript,
            Some(PathBuf::from("/t/a.jsonl"))
        );
    }

    #[test]
    fn a_transcriptless_signal_keeps_the_known_path() {
        // A two-argument emitter (or an empty-payload hook run) reporting a
        // later status must not erase the path an earlier signal delivered —
        // the transcript's location never changes within a conversation.
        let mut sessions = Sessions::default();
        super::fold(
            &mut sessions,
            "a".to_string(),
            "working",
            Some(PathBuf::from("/t/a.jsonl")),
        );
        super::fold(&mut sessions, "a".to_string(), "done", None);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Done]);
        assert_eq!(
            sessions.entries[0].transcript,
            Some(PathBuf::from("/t/a.jsonl"))
        );
    }

    #[test]
    fn seen_records_a_transcript_on_the_insert_it_does_make() {
        // The `seen` insert routes through `Sessions::set` like every other
        // status, so the statusline's `transcriptPath` (when it has one) is
        // captured on the very first sighting rather than waiting for a hook.
        let mut sessions = Sessions::default();
        super::fold(
            &mut sessions,
            "a".to_string(),
            "seen",
            Some(PathBuf::from("/t/a.jsonl")),
        );
        assert_eq!(
            sessions.entries[0].transcript,
            Some(PathBuf::from("/t/a.jsonl"))
        );
    }

    #[test]
    fn usage_targets_snapshot_the_row_in_order() {
        let mut antigravity = Antigravity::default();
        let mut sessions = Sessions::default();
        super::fold(
            &mut sessions,
            "a".to_string(),
            "working",
            Some(PathBuf::from("/t/a.jsonl")),
        );
        super::fold(&mut sessions, "b".to_string(), "idle", None);
        antigravity.update(Message::Updated(sessions));

        let targets = antigravity.usage_targets();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].id, "a");
        assert_eq!(targets[0].status, SessionStatus::Working);
        assert_eq!(targets[1].id, "b");
        assert_eq!(targets[1].status, SessionStatus::Idle);
    }

    // -- the usage snapshot ------------------------------------------------

    /// A `UsageChanged` frame with the given body, built without a bus — see
    /// [`parse_usage`]'s doc comment for why this is possible.
    fn usage_signal<B>(body: &B) -> zbus::Message
    where
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        zbus::Message::signal(ANTIGRAVITY_PATH, ANTIGRAVITY_INTERFACE, USAGE_MEMBER)
            .expect("valid signal coordinates")
            .build(body)
            .expect("serializable body")
    }

    #[test]
    fn parse_usage_reads_the_dtdt_body() {
        let message = usage_signal(&(23.5f64, 1_738_425_600u64, 41.2f64, 1_738_857_600u64));
        let usage = parse_usage(&message).expect("a well-formed body");
        assert_eq!(usage.gemini_weekly.used_pct, 23.5);
        assert_eq!(usage.gemini_weekly.resets_at, 1_738_425_600);
        assert_eq!(usage.third_party_weekly.used_pct, 41.2);
        assert_eq!(usage.third_party_weekly.resets_at, 1_738_857_600);
    }

    #[test]
    fn parse_usage_rejects_a_mistyped_body() {
        // A hand-typed `busctl` call with the wrong signature — strings where
        // numbers belong, or too few arguments — parses to `None` (skipped by
        // the worker), never to a garbage snapshot.
        assert_eq!(parse_usage(&usage_signal(&("23.5", "soon"))), None);
        assert_eq!(parse_usage(&usage_signal(&(23.5f64, 1u64))), None);
    }

    #[test]
    fn a_usage_update_overwrites_the_snapshot() {
        let mut antigravity = Antigravity::default();
        assert_eq!(antigravity.usage(), None);

        let first = Usage {
            gemini_weekly: UsageWindow {
                used_pct: 10.0,
                resets_at: 100,
            },
            third_party_weekly: UsageWindow {
                used_pct: 20.0,
                resets_at: 200,
            },
        };
        antigravity.update(Message::UsageUpdated(first));
        assert_eq!(antigravity.usage(), Some(first));

        // Newest wins — the fold is a plain overwrite, no merging.
        let second = Usage {
            gemini_weekly: UsageWindow {
                used_pct: 12.5,
                resets_at: 150,
            },
            ..first
        };
        antigravity.update(Message::UsageUpdated(second));
        assert_eq!(antigravity.usage(), Some(second));
    }

    #[test]
    fn usage_alone_does_not_put_the_module_on_the_bar() {
        // Rate limits are popover data: a snapshot with no tracked
        // conversations must not conjure the dot row (or its trigger) into
        // existence. Especially relevant here, since agy's statusline emits
        // `UsageChanged` on every refresh whether or not `seen` seeding is
        // wired.
        let mut antigravity = Antigravity::default();
        antigravity.update(Message::UsageUpdated(Usage {
            gemini_weekly: UsageWindow {
                used_pct: 50.0,
                resets_at: 100,
            },
            third_party_weekly: UsageWindow {
                used_pct: 50.0,
                resets_at: 200,
            },
        }));
        assert!(!antigravity.is_present());
    }

    // -- the per-session token counters ------------------------------------

    /// A `TokensChanged` frame with the given body, built without a bus —
    /// same trick as [`usage_signal`], see [`parse_tokens`]'s doc comment.
    fn tokens_signal<B>(body: &B) -> zbus::Message
    where
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        zbus::Message::signal(ANTIGRAVITY_PATH, ANTIGRAVITY_INTERFACE, TOKENS_MEMBER)
            .expect("valid signal coordinates")
            .build(body)
            .expect("serializable body")
    }

    /// One conversation's tokens out of the row, for the assertions below.
    fn tokens_of(sessions: &Sessions, id: &str) -> Option<Tokens> {
        sessions
            .entries
            .iter()
            .find(|session| session.id == id)
            .and_then(|session| session.tokens)
    }

    #[test]
    fn parse_tokens_reads_the_sttt_body() {
        // The shape of a real statusline emit: the conversation id, then
        // `context_window`'s two running totals and the model's window size.
        let message = tokens_signal(&("conv-1", 58_067u64, 11_972u64, 1_048_576u64));
        let (id, tokens) = parse_tokens(&message).expect("a well-formed body");
        assert_eq!(id, "conv-1");
        assert_eq!(
            tokens,
            Tokens {
                input: 58_067,
                output: 11_972,
                context_size: 1_048_576,
            }
        );
    }

    #[test]
    fn parse_tokens_rejects_a_mistyped_body() {
        // A hand-typed `busctl` call with the wrong signature — strings where
        // counters belong, signed numbers, or too few arguments — parses to
        // `None` (skipped by the worker), never to a garbage row.
        assert_eq!(parse_tokens(&tokens_signal(&("conv-1", "58067"))), None);
        assert_eq!(
            parse_tokens(&tokens_signal(&("conv-1", 1i32, 2i32, 3i32))),
            None
        );
        assert_eq!(parse_tokens(&tokens_signal(&(1u64, 2u64, 3u64))), None);
    }

    #[test]
    fn tokens_attach_to_a_tracked_conversation() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 100,
                output: 20,
                context_size: 1_000,
            },
        );

        // One entry, still working, now with numbers on it.
        assert_eq!(ids(&sessions), vec!["a"]);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Working]);
        assert_eq!(
            tokens_of(&sessions, "a"),
            Some(Tokens {
                input: 100,
                output: 20,
                context_size: 1_000,
            })
        );
    }

    #[test]
    fn tokens_for_an_unknown_conversation_create_it_as_idle() {
        // The statusline can legitimately report tokens for a conversation no
        // hook has fired for yet — it runs at session open and the hooks only
        // fire around a turn. That must produce a row, not a dropped signal.
        let mut sessions = Sessions::default();
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 1,
                output: 2,
                context_size: 3,
            },
        );
        assert_eq!(ids(&sessions), vec!["a"]);
        assert_eq!(statuses(&sessions), vec![SessionStatus::Idle]);
        assert_eq!(
            tokens_of(&sessions, "a"),
            Some(Tokens {
                input: 1,
                output: 2,
                context_size: 3,
            })
        );

        // And the insert happens exactly once — a re-broadcast (which the
        // statusline does several times a second) must not append a twin.
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 1,
                output: 2,
                context_size: 3,
            },
        );
        assert_eq!(ids(&sessions), vec!["a"]);
    }

    #[test]
    fn newer_tokens_overwrite_older_ones() {
        // Cumulative counters, not deltas: the newest report replaces the
        // previous one outright rather than being added to it.
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 100,
                output: 20,
                context_size: 1_000,
            },
        );
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 250,
                output: 60,
                context_size: 1_000,
            },
        );
        assert_eq!(
            tokens_of(&sessions, "a"),
            Some(Tokens {
                input: 250,
                output: 60,
                context_size: 1_000,
            })
        );
    }

    #[test]
    fn tokens_survive_a_later_status_change() {
        // The two signals carry independent facts about the same session, and
        // they arrive interleaved several times a second. A status fold must
        // not wipe the numbers (nor must it move the row).
        let mut sessions = Sessions::default();
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 100,
                output: 20,
                context_size: 1_000,
            },
        );
        fold(&mut sessions, "b".to_string(), "working");

        for status in ["working", "seen", "attention", "done", "idle"] {
            fold(&mut sessions, "a".to_string(), status);
            assert_eq!(
                tokens_of(&sessions, "a"),
                Some(Tokens {
                    input: 100,
                    output: 20,
                    context_size: 1_000,
                }),
                "after {status:?}"
            );
            assert_eq!(ids(&sessions), vec!["a", "b"], "after {status:?}");
        }

        // Ending the conversation does take them with it — the entry is gone,
        // numbers and all, and a conversation that comes back starts fresh.
        fold(&mut sessions, "a".to_string(), "ended");
        fold(&mut sessions, "a".to_string(), "idle");
        assert_eq!(tokens_of(&sessions, "a"), None);
    }

    #[test]
    fn usage_targets_carry_the_token_counters() {
        // The accessor the popover actually reads: tokens present on the
        // sessions that have them, `None` on the ones that don't.
        let mut antigravity = Antigravity::default();
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "b".to_string(), "idle");
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 58_067,
                output: 11_972,
                context_size: 1_048_576,
            },
        );
        antigravity.update(Message::Updated(sessions));

        let targets = antigravity.usage_targets();
        assert_eq!(
            targets[0].tokens,
            Some(Tokens {
                input: 58_067,
                output: 11_972,
                context_size: 1_048_576,
            })
        );
        assert_eq!(targets[1].tokens, None);
    }

    #[test]
    fn tokens_alone_do_put_the_module_on_the_bar() {
        // Deliberately unlike `UsageChanged` (account-wide, popover-only): a
        // `TokensChanged` names a conversation, and a named conversation is a
        // dot. This is the `seen`-seeding contract, reached by the other
        // statusline signal.
        let mut antigravity = Antigravity::default();
        let mut sessions = Sessions::default();
        fold_tokens(
            &mut sessions,
            "a".to_string(),
            Tokens {
                input: 1,
                output: 2,
                context_size: 3,
            },
        );
        antigravity.update(Message::Updated(sessions));
        assert!(antigravity.is_present());
    }

    // -- ordering ----------------------------------------------------------

    #[test]
    fn conversations_render_in_first_seen_order() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "c".to_string(), "idle");
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "b".to_string(), "idle");

        // Insertion order, emphatically *not* sorted by id.
        assert_eq!(ids(&sessions), vec!["c", "a", "b"]);
    }

    #[test]
    fn a_status_change_never_reorders_the_row() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "b".to_string(), "idle");
        fold(&mut sessions, "c".to_string(), "idle");

        // The middle conversation cycles through every other state; its dot
        // must stay the middle dot throughout, because Jordan reads the row
        // positionally ("the second one needs me").
        for status in ["working", "subagent", "attention", "done", "idle", "seen"] {
            fold(&mut sessions, "b".to_string(), status);
            assert_eq!(ids(&sessions), vec!["a", "b", "c"], "after {status:?}");
        }
    }

    #[test]
    fn ending_a_conversation_closes_the_row_up_around_it() {
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        fold(&mut sessions, "b".to_string(), "attention");
        fold(&mut sessions, "c".to_string(), "done");

        fold(&mut sessions, "b".to_string(), "ended");
        assert_eq!(ids(&sessions), vec!["a", "c"]);
        assert_eq!(
            statuses(&sessions),
            vec![SessionStatus::Working, SessionStatus::Done]
        );

        // A conversation that comes back after ending is a *new* arrival: it
        // goes to the end of the row, not back to the middle.
        fold(&mut sessions, "b".to_string(), "idle");
        assert_eq!(ids(&sessions), vec!["a", "c", "b"]);
    }

    // -- presence ----------------------------------------------------------

    #[test]
    fn settled_conversations_still_render() {
        let mut antigravity = Antigravity::default();
        assert!(!antigravity.is_present());

        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "idle");
        fold(&mut sessions, "b".to_string(), "done");
        antigravity.update(Message::Updated(sessions));
        assert!(antigravity.is_present());
    }

    #[test]
    fn an_empty_row_renders_nothing() {
        let mut antigravity = Antigravity::default();
        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        antigravity.update(Message::Updated(sessions.clone()));
        assert!(antigravity.is_present());

        fold(&mut sessions, "a".to_string(), "ended");
        antigravity.update(Message::Updated(sessions));
        assert!(!antigravity.is_present());
    }

    // -- the breath gate ---------------------------------------------------

    #[test]
    fn only_the_running_states_breathe() {
        assert!(breathes(SessionStatus::Working));
        assert!(breathes(SessionStatus::Subagents));
        assert!(!breathes(SessionStatus::Attention));
        assert!(!breathes(SessionStatus::Done));
        assert!(!breathes(SessionStatus::Idle));
    }

    #[test]
    fn the_timer_is_gated_on_a_running_conversation() {
        let mut sessions = Sessions::default();
        // Nothing tracked at all — the overwhelmingly common case, and the one
        // where an ungated timer would be an outright poll.
        assert!(!sessions.any_breathing());

        // A row of settled dots is still steady: colored, but no animation —
        // including the `seen`-seeded idle dots agy's statusline creates, which
        // is the state a freshly-opened conversation sits in.
        fold(&mut sessions, "a".to_string(), "attention");
        fold(&mut sessions, "b".to_string(), "done");
        fold(&mut sessions, "c".to_string(), "seen");
        assert!(!sessions.any_breathing());

        // One working conversation anywhere in the row opens the gate.
        fold(&mut sessions, "d".to_string(), "working");
        assert!(sessions.any_breathing());

        // And it closes again the moment that conversation settles.
        fold(&mut sessions, "d".to_string(), "done");
        assert!(!sessions.any_breathing());
    }

    // -- the breath curve --------------------------------------------------

    #[test]
    fn phase_wraps_at_the_cycle_boundary() {
        let cycle = 2400;
        assert_eq!(phase_of(Duration::ZERO, cycle), 0.0);
        assert_eq!(phase_of(Duration::from_millis(600), cycle), 0.25);
        assert_eq!(phase_of(Duration::from_millis(1200), cycle), 0.5);
        // Second cycle: same phase as the first, no drift.
        assert_eq!(phase_of(Duration::from_millis(3600), cycle), 0.5);
        // An hour in, still exact — the modulo happens in integer ms.
        assert_eq!(
            phase_of(
                Duration::from_secs(3600) + Duration::from_millis(1200),
                cycle
            ),
            0.5
        );
    }

    #[test]
    fn phase_degrades_instead_of_dividing_by_zero() {
        assert_eq!(phase_of(Duration::from_millis(500), 0), 0.0);
    }

    #[test]
    fn the_breath_spans_the_themes_range_smoothly() {
        let min = 0.45;
        // Dim at both ends of the cycle, bright dead centre.
        assert!((breath_at(0.0, min) - min).abs() < 1e-5);
        assert!((breath_at(1.0, min) - min).abs() < 1e-5);
        assert!((breath_at(0.5, min) - 1.0).abs() < 1e-5);
        // Symmetric about the midpoint, and never outside the range.
        assert!((breath_at(0.25, min) - breath_at(0.75, min)).abs() < 1e-5);
        for step in 0..=100 {
            let value = breath_at(step as f32 / 100.0, min);
            assert!(
                (min..=1.0).contains(&value),
                "breath left its range at step {step}: {value}"
            );
        }
    }

    #[test]
    fn the_breath_eases_rather_than_ramping() {
        // A triangle wave would put the quarter-cycle value exactly halfway
        // between the two ends; a cosine's ease means it is exactly there only
        // at the quarter point but *slower* near the turns — so the first
        // eighth covers less ground than the second.
        let min = 0.45;
        let first_eighth = breath_at(0.125, min) - breath_at(0.0, min);
        let second_eighth = breath_at(0.25, min) - breath_at(0.125, min);
        assert!(
            first_eighth < second_eighth,
            "expected an eased start, got {first_eighth} then {second_eighth}"
        );
    }

    // -- the animation clock -----------------------------------------------

    #[test]
    fn the_first_tick_establishes_the_epoch() {
        let mut antigravity = Antigravity::default();
        let start = Instant::now();

        antigravity.update(Message::Tick(start));
        // The run has just begun: zero elapsed, phase 0, dot at its dimmest.
        assert_eq!(antigravity.breath_elapsed, Duration::ZERO);

        antigravity.update(Message::Tick(start + Duration::from_millis(600)));
        assert_eq!(antigravity.breath_elapsed, Duration::from_millis(600));

        // Elapsed is measured from the epoch, not accumulated per tick — a
        // tick that arrives late (or after several were dropped) reports real
        // elapsed time rather than a running total of tick intervals.
        antigravity.update(Message::Tick(start + Duration::from_millis(5000)));
        assert_eq!(antigravity.breath_elapsed, Duration::from_millis(5000));
    }

    #[test]
    fn settling_resets_the_breath_so_the_next_run_starts_dim() {
        let mut antigravity = Antigravity::default();
        let start = Instant::now();

        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        antigravity.update(Message::Updated(sessions.clone()));
        antigravity.update(Message::Tick(start));
        antigravity.update(Message::Tick(start + Duration::from_millis(900)));
        assert_eq!(antigravity.breath_elapsed, Duration::from_millis(900));

        // The turn finishes: nothing breathes, so the clock is put away.
        fold(&mut sessions, "a".to_string(), "done");
        antigravity.update(Message::Updated(sessions.clone()));
        assert_eq!(antigravity.breath_epoch, None);
        assert_eq!(antigravity.breath_elapsed, Duration::ZERO);

        // A later run re-establishes its own epoch from its own first tick.
        fold(&mut sessions, "a".to_string(), "working");
        antigravity.update(Message::Updated(sessions));
        let restart = start + Duration::from_secs(60);
        antigravity.update(Message::Tick(restart));
        assert_eq!(antigravity.breath_elapsed, Duration::ZERO);
    }

    #[test]
    fn an_update_while_breathing_leaves_the_animation_alone() {
        let mut antigravity = Antigravity::default();
        let start = Instant::now();

        let mut sessions = Sessions::default();
        fold(&mut sessions, "a".to_string(), "working");
        antigravity.update(Message::Updated(sessions.clone()));
        antigravity.update(Message::Tick(start));
        antigravity.update(Message::Tick(start + Duration::from_millis(900)));

        // A second conversation joining must not restart the pulse mid-fade —
        // the whole row breathes together, on one clock.
        fold(&mut sessions, "b".to_string(), "working");
        antigravity.update(Message::Updated(sessions));
        assert_eq!(antigravity.breath_epoch, Some(start));
        assert_eq!(antigravity.breath_elapsed, Duration::from_millis(900));
    }
}
