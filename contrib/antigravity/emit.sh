#!/usr/bin/env bash
# contrib/antigravity/emit.sh — the hook-side half of the panel's Antigravity
# bus schema (see src/modules/antigravity.rs for the listening half).
#
# Wired in as an agy hook `command` (see hooks.json.example in this
# directory), this script fires one `StatusChanged` broadcast signal on the
# *session* bus and exits — it holds nothing open, owns no bus name, and is
# not a daemon. Same shape as contrib/claude-code/emit.sh, its structural
# model, but agy's hook contract differs from Claude Code's in ways that
# change real lines of this script, not just its comments:
#
#   1. Hook payload fields are camelCase: `conversationId`, `transcriptPath`
#      (NOT Claude Code's snake_case `session_id`/`transcript_path`).
#   2. There is no CLAUDE_CODE_SESSION_ID equivalent env var. Claude Code's
#      emit.sh has an env-var fast path that wins over stdin because it needs
#      no parsing to trust; agy gives this script nothing in the environment
#      at all. stdin is the ONLY source of the conversation id here — if it's
#      empty or unparseable, there is no fallback, only the same "exit
#      quietly" this script already does for that case.
#   3. **This is the big one.** agy hooks are not fire-and-forget the way
#      Claude Code's are. `Stop`'s stdout is read for a `decision` field:
#      per the official docs (<https://antigravity.google/docs/hooks>,
#      confirmed by stage 23's recon), setting `decision` to the string
#      `"continue"` re-enters agy's execution loop instead of letting the
#      turn end; "any other value allows the stop." `PreInvocation` has no
#      `decision` field in its response schema at all — its only recognized
#      key is the optional `injectSteps` (which would inject steps into
#      Jordan's live conversation if this script ever emitted it, so it
#      never does). Printing NOTHING — which is exactly what
#      contrib/claude-code/emit.sh does, and would be the natural port of
#      that pattern — is untested by the docs for either event and is not
#      the same thing as `{}`. This script therefore always prints the
#      literal JSON object `{}` on stdout: an explicit "no `decision`, no
#      `injectSteps`, allow whatever agy's default behavior is" answer that
#      is confirmed safe for both events this script is ever wired to. This
#      is the single most important difference from
#      contrib/claude-code/emit.sh, which prints nothing at all — copying
#      that silence here risks stalling or looping agy's own turn, not just
#      losing a status pill. Every exit path below still reaches the final
#      `printf '{}'`, including the early ones (missing conversation id,
#      missing busctl) — a status pill failing to update must never be the
#      reason agy's execution loop misbehaves.
#
# Usage (from an agy hook command; see hooks.json.example):
#   emit.sh working    # PreInvocation — agy is about to generate
#   emit.sh done       # Stop — turn finished; upgraded to `attention` below
#                       # if the same stdin payload carries a non-empty
#                       # `.error` field (see the STATUS reassignment below)
#   emit.sh idle       # no agy hook fires this; kept for hand-rolled wrappers
#   emit.sh ended      # no agy hook fires this (agy has no SessionEnd
#                       # equivalent); kept for hand-rolled wrappers, same as
#                       # antigravity.rs's fold keeps the arm alive
#
# Valid statuses this script accepts, matching antigravity.rs's `fold`
# vocabulary exactly: working | attention | done | idle | seen | ended.
# `subagent` and `subagent-done` are ALSO accepted (fold keeps both arms for
# forward compatibility) but nothing in this directory ever emits them — agy's
# five hook events (PreToolUse, PostToolUse, PreInvocation, PostInvocation,
# Stop) include no SubagentStart/SubagentStop equivalent. `seen` is likewise
# accepted here but is really the statusline half's status, not a hook's —
# see contrib/antigravity/statusline.sh's header (stage 24b, not yet written)
# for why the statusline needs a status distinct from `idle`.
#
# The bus schema this emits against (must match src/modules/antigravity.rs's
# ANTIGRAVITY_PATH/ANTIGRAVITY_INTERFACE/STATUS_MEMBER constants):
#   session bus, path /io/saola/Antigravity, interface io.saola.Antigravity1,
#   signal StatusChanged(conversation_id: s, status: s, transcript_path: s).
#
# `transcript_path` is the hook payload's own `transcriptPath` field — agy
# writes this unconditionally to
# ~/.gemini/antigravity-cli/brain/<conversation-uuid>/.system_generated/logs/
# on every session (stage 23's recon: this is NOT materialized specially for
# hooks, unlike what an earlier draft of this phase assumed). Empty string
# when stdin had none to read it from; the panel's listener treats that the
# same as a missing third argument — "no usage data" for that conversation.

set -uo pipefail
# Deliberately not `-e`: several paths below need to still reach the
# trailing `printf '{}'` even after a command fails (a missing conversation
# id, a missing `busctl`), and `-e` would abort the script at the first
# non-zero exit instead of falling through to that print. Individual
# commands that must not take the script down are still `|| true`d for
# clarity and to keep `set -o pipefail` from turning a swallowed failure
# inside a pipeline into a surprise.

STATUS="${1:?usage: emit.sh <working|attention|done|idle|seen|ended>}"

# --- reading the hook payload ------------------------------------------
#
# Unlike contrib/claude-code/emit.sh, there is no env-var fast path here —
# agy sets nothing in a hook subprocess's environment (see the header above).
# stdin is the only source, so the `-t 0` guard is what keeps a bare
# interactive `emit.sh working` from blocking on a terminal that will never
# send JSON; a real hook invocation's stdin is always a pipe.
HOOK_JSON=""
if [[ ! -t 0 ]]; then
    HOOK_JSON="$(cat)" || true
fi

# Pull one string field's value out of the hook JSON: jq when available,
# else a permissive regex — same helper as contrib/claude-code/emit.sh,
# reused verbatim (the payload is still a flat object, and `conversationId`/
# `transcriptPath` are still quote-and-backslash-free in practice: a UUID
# and an absolute path under $HOME). The `|| true`s keep a no-match grep or
# malformed-stdin jq from being treated as a hard failure — a hook that dies
# loudly surfaces as noise mid-turn, and every failure here must be
# swallowed (see the module doc comment's "never disrupt agy" posture).
json_field() {
    local field="$1" value=""
    if command -v jq >/dev/null 2>&1; then
        value="$(printf '%s' "$HOOK_JSON" | jq -r ".${field} // empty")" || true
    else
        value="$(printf '%s' "$HOOK_JSON" \
            | grep -o "\"${field}\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" \
            | head -n1 \
            | sed -E 's/.*:[[:space:]]*"([^"]*)"/\1/')" || true
    fi
    printf '%s' "$value"
}

# camelCase, per the Phase 3 context / stage 23's handoff — the main
# textual difference from contrib/claude-code/emit.sh's snake_case fields.
CONVERSATION_ID="$(json_field conversationId)"
TRANSCRIPT="$(json_field transcriptPath)"

# `Stop` carrying an `error` field means the turn ended in failure, which
# antigravity.rs's fold has a dedicated status for (`attention`, the same
# semaphore color Claude Code's `StopFailure`/`Notification` hooks use) —
# but agy's hook contract has no separate "Stop with error" event the way
# Claude Code splits `Stop`/`StopFailure`. hooks.json.example wires `Stop`
# to a single `emit.sh done` command, so the upgrade has to happen here,
# reading the same stdin payload the id/transcript fields came from, rather
# than being a documented follow-up: the fold already distinguishes `done`
# from `attention`, and doing this in hooks.json would mean two Stop hook
# entries racing to read the same stdin (a hook's stdin is consumed once).
if [[ "$STATUS" == "done" ]]; then
    ERROR="$(json_field error)"
    if [[ -n "$ERROR" ]]; then
        STATUS="attention"
    fi
fi

if [[ -z "$CONVERSATION_ID" ]]; then
    # No conversation id — nothing to key antigravity.rs's fold on. Fall
    # through to the closing `{}` print rather than exiting early: agy still
    # needs its valid response object regardless of whether the panel got
    # anything useful out of this run.
    :
else
    # `--user` targets the session bus, matching antigravity.rs's
    # `Connection::session()`. If the session bus is unreachable (a
    # headless/non-graphical invocation, a stripped-down container) `busctl`
    # fails — swallowed here for the same "don't disrupt agy over a missing
    # status pill" reason as the missing-id case above.
    busctl --user emit /io/saola/Antigravity io.saola.Antigravity1 StatusChanged sss \
        "$CONVERSATION_ID" "$STATUS" "$TRANSCRIPT" || true
fi

# The load-bearing line. See the header's item 3: `{}` is confirmed safe for
# both PreInvocation (no `decision` field exists in its response schema) and
# Stop (`decision` is opt-in re-entry — only the literal string "continue"
# keeps the turn going; anything else, `{}` included, allows the stop).
# Printing nothing here, which is what contrib/claude-code/emit.sh does,
# would risk interfering with agy's execution loop instead of merely
# skipping a status update — always print this, on every path above.
printf '{}'
