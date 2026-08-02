#!/usr/bin/env bash
# contrib/claude-code/emit.sh — the hook-side half of the panel's Claude Code
# bus schema (see src/modules/claude.rs for the listening half).
#
# Wired in as a Claude Code hook `command` (see settings.hooks.json.example
# in this directory), this script fires one `StatusChanged` broadcast signal
# on the *session* bus and exits — it holds nothing open, owns no bus name,
# and is not a daemon. Nothing runs between hook events; there is no process
# for the panel to lose track of, only a signal that either arrives or
# doesn't (see claude.rs's module doc comment for why the panel side treats
# "no signal yet" as its normal quiet state rather than an error).
#
# Usage (from a Claude Code hook command):
#   emit.sh idle        # SessionStart   — session open, nothing happening yet
#   emit.sh working     # UserPromptSubmit
#   emit.sh subagent    # PreToolUse, matcher "Task"  — a subagent is spawning
#   emit.sh working     # SubagentStop   — a subagent finished, main loop
#                       #                  resumes generating (see note below)
#   emit.sh done        # Stop            — turn finished, output awaiting review
#   emit.sh attention   # Notification
#   emit.sh ended       # SessionEnd
#
# Six statuses on the wire now, one per session-status semaphore dot (see
# saola-theme's SAOLA-STYLE-GUIDE.md, "Session status semaphore"):
#   working | subagent | attention | done | idle | ended
#
# `idle` and `done` used to be the same status (the old 4-status schema had
# no "session just started, nothing happened yet" state distinct from
# "Claude just finished a turn") — they're now split: SessionStart emits
# `idle`, Stop emits `done`. If you're updating an old hooks config from
# before this split, its Stop -> `idle` wiring needs to become Stop -> `done`.
#
# SubagentStop -> working, note: this is imperfect with parallel subagents.
# `SubagentStop` fires per subagent, so if Claude has three `Task` calls in
# flight and the first one finishes, its hook flips the dot back to
# `working` even though two subagents are still running underneath — there
# is no "N of M subagents done" signal to emit instead. The dot only goes
# back to `subagent` if another `PreToolUse`/`Task` call starts before the
# main loop's own `Stop` fires. Accepted as close enough: the dot is still
# accurate about "is the session doing something right now," just not
# about exactly what.
#
# The bus schema this emits against (must match src/modules/claude.rs's
# CLAUDE_CODE_PATH/CLAUDE_CODE_INTERFACE/STATUS_MEMBER constants):
#   session bus, path /io/saola/ClaudeCode, interface io.saola.ClaudeCode1,
#   signal StatusChanged(session_id: s, status: s, transcript_path: s).
#
# The same interface carries a second signal, UsageChanged (the account's
# rate-limit gauges, 2026-08-01) — emitted by statusline.sh in this
# directory, not by this script: only Claude Code's statusLine command ever
# receives the rate-limit numbers, so that's where they enter the bus.
#
# `transcript_path` (added 2026-08-01, for the panel's usage popover) is the
# hook payload's own `transcript_path` field — the session's JSONL file,
# which the panel reads *only* when the usage popover is opened (never on a
# timer; the "signal, never a poll" rule stands). Empty string when stdin
# had no payload to read it from; the panel treats that as "no usage data".
# The panel's listener accepts the old two-argument `ss` body too, so an
# un-updated hook keeps its status dots and only lacks usage.

set -euo pipefail

STATUS="${1:?usage: emit.sh <working|subagent|attention|done|idle|ended>}"

# --- reading the hook payload ------------------------------------------
#
# Verified locally (this machine's installed Claude Code build, 2.1.220 —
# there is no bundled hooks.md to cite chapter and verse, so this was
# confirmed by inspecting the CLI binary's own hook-subprocess-spawning
# code, not guessed):
#
#   - `CLAUDE_CODE_SESSION_ID` (NOT `CLAUDE_SESSION_ID`) is set in the
#     environment of every hook subprocess Claude Code spawns. The similar-
#     looking `${CLAUDE_SESSION_ID}` token exists too, but it's a *command-
#     string* template substitution (like `${CLAUDE_PROJECT_DIR}`) applied
#     before some other kinds of commands run — it is not guaranteed to be
#     set as an env var, so this script does not rely on it.
#   - Every hook also receives a JSON object on stdin with (at minimum)
#     `session_id`, `transcript_path`, `cwd`, and `hook_event_name` fields
#     — the same session id, belt-and-suspenders, plus the transcript path
#     that has no env-var counterpart at all.
#
# stdin is now read whenever it is a pipe (which a hook invocation's always
# is — `transcript_path` only exists there); the `-t 0` guard keeps a bare
# interactive `emit.sh working` from blocking on a terminal that will never
# send JSON. The env var still wins for the session id since it needs no
# parsing to trust.
HOOK_JSON=""
if [[ ! -t 0 ]]; then
    HOOK_JSON="$(cat)" || true
fi

# Pull one string field's value out of the hook JSON: jq when available,
# else a permissive regex. Not a real parser, but Claude Code's hook
# payload is a flat object and both fields this script reads are
# quote-and-backslash-free in practice (a UUID, an absolute path under
# $HOME). The `|| true`s keep `set -e -o pipefail` from turning a no-match
# grep or malformed-stdin jq into a hard stop — a hook that fails loudly
# surfaces as noise mid-session.
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

SESSION_ID="${CLAUDE_CODE_SESSION_ID:-}"
if [[ -z "$SESSION_ID" ]]; then
    SESSION_ID="$(json_field session_id)"
fi
TRANSCRIPT="$(json_field transcript_path)"

if [[ -z "$SESSION_ID" ]]; then
    # No session id from either source — nothing to key the panel's fold on.
    # Exit quietly rather than erroring: a missing status update is far
    # less disruptive than hook noise mid-session.
    exit 0
fi

# `--user` targets the session bus, matching claude.rs's `Connection::
# session()`. If the session bus is unreachable (a headless/non-graphical
# invocation, a stripped-down container) `busctl` fails — swallowed here
# for the same "don't disrupt Claude Code over a missing status pill"
# reason as the missing-session-id case above.
busctl --user emit /io/saola/ClaudeCode io.saola.ClaudeCode1 StatusChanged sss \
    "$SESSION_ID" "$STATUS" "$TRANSCRIPT" || true
