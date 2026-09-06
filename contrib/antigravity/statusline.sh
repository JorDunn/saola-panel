#!/usr/bin/env bash
# contrib/antigravity/statusline.sh — the statusline-side half of the
# panel's Antigravity bus schema (see src/modules/antigravity.rs for the
# listening half, and emit.sh in this directory for the hook-side sibling).
# Structural model: contrib/claude-code/statusline.sh — read that first if
# this looks unfamiliar. The differences below are real, not just cosmetic:
# agy's payload shape, its two quota buckets, and a third signal Claude
# Code has no equivalent of.
#
# Wired in as agy's `statusLine` command — a top-level settings.json object,
# not a hook (see hooks.json.example in this directory for the separate
# PreInvocation/Stop wiring) — and chained AHEAD of Jordan's existing
# oh-my-posh wrapper so this script's own bus signals never depend on that
# wrapper having already run or exited 0 (see settings.json.example's header
# for the full chain: stdin is read once into a variable and fanned out with
# two `printf`s, because a second command in a shell pipeline would otherwise
# see an already-drained stdin).
#
# agy invokes the statusline command on every session update (throttled to
# roughly 300 ms), passing a JSON payload on stdin. The shape below is from a
# live capture (2026-08-14, agy 1.1.13 — PLAN.md's "Phase 3 context" /
# "The real statusline payload — CAPTURED" section has the full story and
# the procedure), not a guess from the `agy` binary's struct tags the way an
# earlier draft of this phase was (that draft's `rate_limits.
# {five_hour,seven_day}` premise was simply wrong — agy sends neither key).
# NEVER copy a raw captured payload, or its `email`/`plan_tier` values, into
# this repo or a commit — field NAMES only, below:
#
#   {
#     "conversation_id": "<uuid>", "session_id": "<same uuid>",
#     "transcript_path": "/home/jordan/.gemini/antigravity/brain/<uuid>/...",
#     "agent_state": "idle",
#     "context_window": {
#       "total_input_tokens": 58067, "total_output_tokens": 11972,
#       "context_window_size": 1048576, ...
#     },
#     "quota": {
#       "gemini-weekly": { "remaining_fraction": 0.078, "reset_time": "<iso8601>", "reset_in_seconds": 599532 },
#       "3p-weekly":     { "remaining_fraction": 1.0,   "reset_time": "<iso8601>", "reset_in_seconds": 604778 }
#     }
#   }
#
# Three signals fire per invocation, all on the session bus, object path
# /io/saola/Antigravity, interface io.saola.Antigravity1 (must match
# antigravity.rs's ANTIGRAVITY_PATH/ANTIGRAVITY_INTERFACE constants):
#
# 1. StatusChanged(conversation_id: s, status: s, transcript_path: s).
#    Keyed off `.conversation_id` (the same UUID as `.session_id` in this
#    payload — `conversation_id` is used to match the hook payload's
#    `conversationId`, see emit.sh). `.transcript_path` is present directly
#    on this payload (unlike the hook payload, which nests it as
#    `transcriptPath`) and is passed straight through.
#
#    `status` is no longer a hardcoded "seen" — agy's docs
#    (<https://antigravity.google/docs/cli/statusline>) document
#    `agent_state`'s vocabulary as **five** values (`idle`, `thinking`,
#    `working`, `tool_use`, `initializing` — documented, but may still grow;
#    the fallback below exists for exactly that reason) plus two fields the
#    hooks have no equivalent of at all — agy has no `Notification` hook, so
#    "blocked waiting on the user" was previously unreachable from this
#    directory:
#      `tool_confirmation_pending` (bool) — a permission prompt is up
#      `pending_input_count` (number) — turns queued waiting on the user
#    Both are `omitempty` and were absent from the 2026-08-14 capture (it was
#    taken at rest); absent means false/0, not unknown. This script maps
#    them to the semaphore with attention checked **first**, because a
#    session sitting on a permission prompt can still report
#    `agent_state: "tool_use"` underneath it:
#
#      1. tool_confirmation_pending == true, or pending_input_count > 0
#                                                            → "attention"
#      2. agent_state in {thinking, working, tool_use}      → "working"
#      3. agent_state == idle                                → "idle"
#      4. agent_state == initializing                        → "seen"
#      5. agent_state missing or an unrecognized value        → "seen"
#
#    Cases 4–5 deliberately land on "seen", not a guessed-at real status:
#    antigravity.rs's fold treats "seen" specially (inserts an untracked
#    conversation as Idle, never overwrites a tracked one — see that
#    module's doc comment, "the `seen` status agy needs and Claude Code
#    doesn't"), which is exactly the harmless arm an unrecognized future
#    `agent_state` value should fall into rather than clobbering a good dot.
#    Cases 1–3 are real-time information from agy itself, not a guess, so
#    they're allowed to overwrite outright the same as a hook's status
#    would. This is a real improvement over the hook-only design: `attention`
#    was otherwise unreachable outside a failed `Stop`, and status now
#    updates continuously (every ~300 ms) rather than only at invocation
#    boundaries. The hooks in emit.sh/hooks.json.example are NOT redundant
#    with this — `Stop` still uniquely distinguishes "turn finished, output
#    awaiting review" (`done`) from a merely idle `agent_state`, and the two
#    sources compose fine because every arm here is either a real overwrite
#    or the non-destructive "seen".
#
# 2. UsageChanged(gemini_weekly_pct: d, gemini_weekly_resets_at: t,
#    threep_weekly_pct: d, threep_weekly_resets_at: t) — account-wide quota.
#    agy's `quota` map turned out to hold two *weekly* buckets, not the
#    five-hour/seven-day pair PLAN.md originally guessed at — there is no
#    5-hour window at all. The bucket key strings are `gemini-weekly` and
#    `3p-weekly`. The wire signature stays `dtdt` (Jordan's call, so
#    antigravity.rs's listener needed no change), with slot 1 =
#    gemini-weekly and slot 2 = 3p-weekly. Per bucket, converted right here
#    (the only place in this script that does math):
#      used_pct  = (1 - remaining_fraction) * 100
#      resets_at = <now, epoch seconds> + reset_in_seconds
#    `reset_in_seconds` is used deliberately instead of the sibling
#    `reset_time` field (an ISO-8601 *string*) — see
#    contrib/claude-code/statusline.sh's header for why this directory
#    avoids date parsing in bash entirely. All four values or nothing: a
#    partial snapshot has no honest rendering, and the panel's parse would
#    reject a mistyped body anyway. If either bucket is missing from
#    `.quota` — agy renaming or dropping one server-side is not something
#    this script can rule out — the whole signal is skipped rather than
#    emitting a zero for the missing side: a zero used_pct would read as
#    "nothing used," and a zero resets_at would read as "reset in the past,"
#    neither of which is what "we don't know" means.
#
# 3. TokensChanged(conversation_id: s, input: t, output: t,
#    context_size: t) — NEW, no Claude Code equivalent. Per-session (unlike
#    UsageChanged), so it carries the conversation id, from
#    `.context_window.{total_input_tokens,total_output_tokens,
#    context_window_size}`. This is the one place agy is better instrumented
#    than Claude Code: the token counts arrive right here on the statusline,
#    where Claude Code's equivalent numbers would need a transcript read —
#    and agy's own transcript.jsonl has no `usage` object at all (see
#    antigravity.rs's `UsageTarget` doc comment). Skipped if any of the
#    three fields, or the conversation id itself, is absent.
#
# `agent_state` is also present in the payload (`"idle"` in the capture, the
# only value ever observed) and is a **future opportunity this script does
# NOT act on**: if its vocabulary turns out to map cleanly onto the
# semaphore, a later revision could drive status straight from this field
# and retire both "seen" and the emit.sh hooks entirely. One sample is not a
# vocabulary — see PLAN.md's Phase 3 context, load-bearing on this point.
#
# Stdout is deliberately EMPTY on every path (unlike emit.sh, which must
# print the literal `{}` — the statusLine command has no decision/
# injectSteps contract to satisfy the way a hook does; whatever a
# statusLine command prints becomes agy's own in-terminal status text).
# Staying silent here is what lets settings.json.example's chain fan the
# same stdin payload out to Jordan's oh-my-posh wrapper afterward: this
# script owns the bus signals only and leaves the visible statusline text to
# that second command in the chain.
#
# Stateless and idempotent — no cache of last-sent values, every invocation
# re-derives and re-emits from scratch — and every failure is swallowed
# (`|| true` on every `busctl`, missing `jq`/`awk` degrading to "emit
# nothing" rather than an error). jq-only for field extraction, no regex
# fallback like emit.sh's `json_field`: every field this script reads is
# nested two or three objects deep and at least one is numeric, which is
# past what a permissive grep can be trusted with — no jq means no signal,
# not a best-effort parse.

set -uo pipefail
# Deliberately not `-e`, unlike contrib/claude-code/statusline.sh: that
# script only ever has one signal to emit, so aborting on the first failure
# is fine. This one has three independent signals per invocation, and a
# failure deriving one (a missing quota bucket, a malformed context_window)
# must not stop the others from being attempted — same reasoning as emit.sh's
# `set -uo pipefail` for the same reason.

# The statusline payload only ever arrives on stdin; the `-t 0` guard keeps
# a bare interactive `statusline.sh` from blocking on a terminal that will
# never send JSON (same posture as emit.sh's and claude-code/statusline.sh's
# stdin reads).
PAYLOAD=""
if [[ ! -t 0 ]]; then
    PAYLOAD="$(cat)" || true
fi

# No jq, no signal — the panel just never shows Antigravity usage/token
# data, which is the correct degraded state (same posture as
# claude-code/statusline.sh).
if ! command -v jq >/dev/null 2>&1; then
    exit 0
fi

# Pull one field out of the payload via a full jq expression (so callers can
# pass bracket lookups for the hyphenated/digit-leading quota bucket keys,
# e.g. `.quota["3p-weekly"].remaining_fraction`, which dot notation can't
# express). `// empty` collapses absent, null, or unparseable to an empty
# string, so every caller's own presence check is the only gate that
# matters. The `|| true` keeps `set -o pipefail` from turning a malformed
# stdin's jq failure into anything louder than an empty result.
field() {
    local expr="$1" value=""
    value="$(printf '%s' "$PAYLOAD" | jq -r "$expr // empty" 2>/dev/null)" || true
    printf '%s' "$value"
}

CONVERSATION_ID="$(field '.conversation_id')"
TRANSCRIPT_PATH="$(field '.transcript_path')"
AGENT_STATE="$(field '.agent_state')"
TOOL_CONFIRMATION_PENDING="$(field '.tool_confirmation_pending')"
PENDING_INPUT_COUNT="$(field '.pending_input_count')"

# --- Signal 1: StatusChanged ----------------------------------------------
#
# See the header for the full precedence table this implements. `attention`
# is checked first and independently of `agent_state` — a session sitting on
# a permission prompt can still report `agent_state: "tool_use"` underneath
# it. `PENDING_INPUT_POSITIVE` guards the arithmetic comparison behind a
# regex match so an absent/non-numeric `pending_input_count` (both `field`'s
# `// empty` and a future schema change could produce one) never reaches
# `(( ))`, which would be a hard error under `set -u`.
PENDING_INPUT_POSITIVE="false"
if [[ "$PENDING_INPUT_COUNT" =~ ^[0-9]+$ ]] && ((PENDING_INPUT_COUNT > 0)); then
    PENDING_INPUT_POSITIVE="true"
fi

if [[ "$TOOL_CONFIRMATION_PENDING" == "true" || "$PENDING_INPUT_POSITIVE" == "true" ]]; then
    STATUS="attention"
else
    case "$AGENT_STATE" in
        thinking | working | tool_use) STATUS="working" ;;
        idle) STATUS="idle" ;;
        initializing) STATUS="seen" ;;
        # Missing, or a future value this script predates — the
        # non-destructive arm, deliberately (see header).
        *) STATUS="seen" ;;
    esac
fi

# No conversation id, no fold key, no signal — same "nothing to key on"
# posture as emit.sh's own missing-id path.
if [[ -n "$CONVERSATION_ID" ]]; then
    busctl --user emit /io/saola/Antigravity io.saola.Antigravity1 StatusChanged sss \
        "$CONVERSATION_ID" "$STATUS" "$TRANSCRIPT_PATH" || true
fi

# --- Signal 2: UsageChanged (account-wide quota) -------------------------
#
# Bucket keys are string literals, not something derived from the payload —
# `gemini-weekly` and `3p-weekly` are what the live capture found. See the
# header for why slot 1 = gemini-weekly and slot 2 = 3p-weekly.
GEMINI_FRAC="$(field '.quota["gemini-weekly"].remaining_fraction')"
GEMINI_RESET_IN="$(field '.quota["gemini-weekly"].reset_in_seconds')"
THREEP_FRAC="$(field '.quota["3p-weekly"].remaining_fraction')"
THREEP_RESET_IN="$(field '.quota["3p-weekly"].reset_in_seconds')"

# Converts one bucket's raw fields into `used_pct resets_at` (space-
# separated, read by the caller below). awk, not bash arithmetic: `remaining_
# fraction` is a float and bash's `$(( ))` is integer-only, and awk fails
# quietly into an empty/garbage line on unparseable input rather than
# aborting the script the way a bad `$(( ))` expression would.
quota_signal() {
    local frac="$1" reset_in="$2" now="$3"
    awk -v f="$frac" -v r="$reset_in" -v n="$now" \
        'BEGIN { printf "%.4f %d", (1 - f) * 100, n + r }' 2>/dev/null
}

# All four or nothing: a partial snapshot has no honest rendering, and the
# panel's parse would reject a mistyped body anyway (see header). This is
# also what keeps a missing/renamed bucket from emitting a zero — a zero
# used_pct reads as "nothing used," which "we don't know" is not.
if [[ -n "$GEMINI_FRAC" && -n "$GEMINI_RESET_IN" && -n "$THREEP_FRAC" && -n "$THREEP_RESET_IN" ]]; then
    NOW="$(date +%s)"
    read -r GEMINI_PCT GEMINI_RESETS_AT <<<"$(quota_signal "$GEMINI_FRAC" "$GEMINI_RESET_IN" "$NOW")"
    read -r THREEP_PCT THREEP_RESETS_AT <<<"$(quota_signal "$THREEP_FRAC" "$THREEP_RESET_IN" "$NOW")"

    if [[ -n "$GEMINI_PCT" && -n "$GEMINI_RESETS_AT" && -n "$THREEP_PCT" && -n "$THREEP_RESETS_AT" ]]; then
        busctl --user emit /io/saola/Antigravity io.saola.Antigravity1 UsageChanged dtdt \
            "$GEMINI_PCT" "$GEMINI_RESETS_AT" "$THREEP_PCT" "$THREEP_RESETS_AT" || true
    fi
fi

# --- Signal 3: TokensChanged (per-session, NEW) ---------------------------
INPUT_TOKENS="$(field '.context_window.total_input_tokens')"
OUTPUT_TOKENS="$(field '.context_window.total_output_tokens')"
CONTEXT_SIZE="$(field '.context_window.context_window_size')"

# Needs the conversation id too — TokensChanged's `sttt` signature carries
# it (unlike UsageChanged, which is account-wide) — so a payload with token
# counts but no id has nothing to fold onto and is skipped just like a
# payload missing any one of the three counts.
if [[ -n "$CONVERSATION_ID" && -n "$INPUT_TOKENS" && -n "$OUTPUT_TOKENS" && -n "$CONTEXT_SIZE" ]]; then
    busctl --user emit /io/saola/Antigravity io.saola.Antigravity1 TokensChanged sttt \
        "$CONVERSATION_ID" "$INPUT_TOKENS" "$OUTPUT_TOKENS" "$CONTEXT_SIZE" || true
fi

# Stdout stays empty — see the header's "stdout is deliberately EMPTY" note.
