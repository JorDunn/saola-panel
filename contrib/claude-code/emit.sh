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
#   emit.sh working     # UserPromptSubmit
#   emit.sh attention    # Notification
#   emit.sh idle          # Stop
#   emit.sh ended          # SessionEnd
#
# The bus schema this emits against (must match src/modules/claude.rs's
# CLAUDE_CODE_PATH/CLAUDE_CODE_INTERFACE/CLAUDE_CODE_MEMBER constants):
#   session bus, path /io/saola/ClaudeCode, interface io.saola.ClaudeCode1,
#   signal StatusChanged(session_id: s, status: s).

set -euo pipefail

STATUS="${1:?usage: emit.sh <working|attention|idle|ended>}"

# --- finding the session id --------------------------------------------
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
#     — the same session id, belt-and-suspenders. This is the fallback for
#     any hook invocation path that doesn't set the env var (or a future
#     Claude Code version that stops setting it).
#
# The env var is checked first since it needs no JSON parsing; stdin is
# only read if it's absent, so a hook command wired to run this with the
# env var present never has to consume (or block on) stdin at all.
SESSION_ID="${CLAUDE_CODE_SESSION_ID:-}"

if [[ -z "$SESSION_ID" ]]; then
    HOOK_JSON="$(cat)"
    if command -v jq >/dev/null 2>&1; then
        # `|| true`: with `set -o pipefail`, a `jq` failure (malformed
        # stdin) would otherwise trip `set -e` and kill the script before
        # the empty-`SESSION_ID` check below gets a chance to exit cleanly.
        SESSION_ID="$(printf '%s' "$HOOK_JSON" | jq -r '.session_id // empty')" || true
    else
        # No jq (not a given on every box this might run on — not present
        # on the machine this was developed on, in fact): a permissive
        # regex pull of the "session_id" field's value out of the raw
        # JSON. Not a real parser, but Claude Code's hook payload is a
        # flat object and session ids are UUID-shaped (no embedded quotes
        # or backslashes to confuse it), so this is safe in practice.
        # Same `|| true` reasoning as the jq branch: a no-match `grep`
        # exits 1, which `pipefail` would otherwise turn into a hard stop.
        SESSION_ID="$(printf '%s' "$HOOK_JSON" \
            | grep -o '"session_id"[[:space:]]*:[[:space:]]*"[^"]*"' \
            | head -n1 \
            | sed -E 's/.*:[[:space:]]*"([^"]*)"/\1/')" || true
    fi
fi

if [[ -z "$SESSION_ID" ]]; then
    # No session id from either source — nothing to key the panel's fold on.
    # Exit quietly rather than erroring: a hook that fails loudly surfaces
    # as noise mid-session, and a missing status update is far less
    # disruptive than that.
    exit 0
fi

# `--user` targets the session bus, matching claude.rs's `Connection::
# session()`. If the session bus is unreachable (a headless/non-graphical
# invocation, a stripped-down container) `busctl` fails — swallowed here
# for the same "don't disrupt Claude Code over a missing status pill"
# reason as the missing-session-id case above.
busctl --user emit /io/saola/ClaudeCode io.saola.ClaudeCode1 StatusChanged ss \
    "$SESSION_ID" "$STATUS" || true
