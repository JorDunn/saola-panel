#!/usr/bin/env bash
# contrib/claude-code/statusline.sh — the statusline-side half of the panel's
# Claude Code bus schema (see src/modules/claude.rs for the listening half,
# and emit.sh in this directory for the hook-side sibling this script is
# modeled on).
#
# Wired in as Claude Code's `statusLine` command — a *top-level* settings.json
# object, not a hook (see settings.hooks.json.example in this directory):
#
#   "statusLine": {
#     "type": "command",
#     "command": "$HOME/Developer/saola-panel/contrib/claude-code/statusline.sh"
#   }
#
# Claude Code invokes the statusline command on every session update
# (throttled to roughly 300 ms), passing a JSON payload on stdin. That
# payload — and ONLY that payload; hook events never receive it, and it is
# not in the transcript JSONL — carries the account's rate-limit gauges:
#
#   "rate_limits": {
#     "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
#     "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 }
#   }
#
# `used_percentage` is a 0–100 float, `resets_at` Unix epoch seconds. The
# whole `rate_limits` block is ABSENT for API-key billing, and absent until
# the session's first API response — this script stays silent in both cases
# (never emits zeros), which the panel reads as its normal "no data yet"
# quiet state.
#
# Like emit.sh, this fires one broadcast signal on the session bus and
# exits — it holds nothing open, owns no bus name, and is not a daemon. It
# emits unconditionally on every invocation (stateless — no cache of the
# last-sent values); the panel's worker folds idempotently and dedupes
# repeats itself, so re-broadcasting an unchanged snapshot is harmless.
#
# Whatever a statusline command prints to stdout becomes Claude Code's own
# in-terminal status line; this script deliberately prints nothing (an
# empty status line), leaving that display free for a future revision.
#
# The bus schema this emits against (must match src/modules/claude.rs's
# CLAUDE_CODE_PATH/CLAUDE_CODE_INTERFACE/USAGE_MEMBER constants):
#   session bus, path /io/saola/ClaudeCode, interface io.saola.ClaudeCode1,
#   signal UsageChanged(five_hour_pct: d, five_hour_resets_at: t,
#                       seven_day_pct: d, seven_day_resets_at: t).
#
# Deliberately NO session_id argument, unlike StatusChanged: rate limits
# are account-wide, not per-session, so the panel keeps exactly one
# snapshot and the newest signal wins — there is nothing to key a
# per-session fold on.

set -euo pipefail

# The statusline payload only ever arrives on stdin; the `-t 0` guard keeps
# a bare interactive `statusline.sh` from blocking on a terminal that will
# never send JSON (same posture as emit.sh's stdin read).
PAYLOAD=""
if [[ ! -t 0 ]]; then
    PAYLOAD="$(cat)" || true
fi

# jq only, no regex fallback (unlike emit.sh's json_field): the fields here
# are nested two objects deep and numeric, which is past what a permissive
# grep can be trusted with. No jq — no signal; the panel just never shows
# rate-limit data, which is the correct degraded state.
if ! command -v jq >/dev/null 2>&1; then
    exit 0
fi

# `// empty` per field: absent, null, or unparseable all collapse to an
# empty string, and the all-four-present check below is the only gate. The
# `|| true`s keep `set -e -o pipefail` from turning malformed stdin into a
# hard stop — a statusline command that fails loudly surfaces as noise on
# every session update.
rate_field() {
    local value=""
    value="$(printf '%s' "$PAYLOAD" | jq -r "$1 // empty" 2>/dev/null)" || true
    printf '%s' "$value"
}

PCT5="$(rate_field '.rate_limits.five_hour.used_percentage')"
RESETS5="$(rate_field '.rate_limits.five_hour.resets_at')"
PCT7="$(rate_field '.rate_limits.seven_day.used_percentage')"
RESETS7="$(rate_field '.rate_limits.seven_day.resets_at')"

# All four or nothing: a partial snapshot has no honest rendering, and the
# panel's parse would reject a mistyped body anyway.
if [[ -z "$PCT5" || -z "$RESETS5" || -z "$PCT7" || -z "$RESETS7" ]]; then
    exit 0
fi

# `--user` targets the session bus, matching claude.rs's `Connection::
# session()`. Failure swallowed for the same "don't disrupt Claude Code
# over a missing panel readout" reason as emit.sh's emit.
busctl --user emit /io/saola/ClaudeCode io.saola.ClaudeCode1 UsageChanged dtdt \
    "$PCT5" "$RESETS5" "$PCT7" "$RESETS7" || true
