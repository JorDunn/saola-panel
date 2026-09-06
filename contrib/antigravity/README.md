# contrib/antigravity

Hook-side and bus-side scripts that feed the panel's Antigravity (`agy`)
session-status island (`src/modules/antigravity.rs`). Structural sibling of
`contrib/claude-code/` — read that directory first if this one is
unfamiliar; the differences from it are called out below and in each file's
own header comment.

## Status: shipped (stage 24b of `PLAN.md`'s Phase 3 complete)

| File | Status |
|---|---|
| `emit.sh` | shipped |
| `hooks.json.example` | shipped |
| `settings.json.example` | shipped |
| `statusline.sh` | shipped — verified against a real captured payload (agy 1.1.13, 2026-08-14) |

`contrib/antigravity/statusline.sh` needed a live agy statusline payload
capture before it could be written — agy's `quota` map is keyed by strings
the server supplies **at runtime**, and nothing in the `agy` binary's struct
tags revealed what those keys actually were (see
`.claude/handoffs/handoff_stage_23.md`). Jordan ran the capture on
2026-08-14; the real bucket keys are `gemini-weekly` and `3p-weekly` (see
below), and the script is now wired into `settings.json.example`'s chain.

## The bus schema

Session bus, object path `/io/saola/Antigravity`, interface
`io.saola.Antigravity1` (must match `src/modules/antigravity.rs`'s
`ANTIGRAVITY_PATH`/`ANTIGRAVITY_INTERFACE` constants exactly — a separate
interface from `io.saola.ClaudeCode1` so the two agents' sessions can never
collide in one fold):

- `StatusChanged(conversation_id: s, status: s, transcript_path: s)` — one
  per conversation, emitted both by `emit.sh` from agy's hooks and by
  `statusline.sh` from the statusline payload's `agent_state` (plus its
  `tool_confirmation_pending`/`pending_input_count` fields — see
  `statusline.sh`'s header for the precedence between them). Status
  vocabulary: `working | attention | done | idle | seen | ended` (plus
  `subagent` / `subagent-done`, kept for forward compatibility — nothing
  emits them today because agy has no subagent hook events).
- `UsageChanged(gemini_weekly_pct: d, gemini_weekly_resets_at: t,
  threep_weekly_pct: d, threep_weekly_resets_at: t)` — account-wide quota,
  emitted by `statusline.sh`. **Both windows are weekly** — the earlier
  five-hour/seven-day guess was wrong; agy has no 5-hour window at all. The
  real bucket keys, confirmed by the 2026-08-14 capture, are `gemini-weekly`
  (wire slot 1) and `3p-weekly` (wire slot 2); the `dtdt` signature is
  unchanged (Jordan's call), so `src/modules/antigravity.rs`'s listener
  needed no update — only what the four numbers *mean* changed. Per bucket,
  `statusline.sh` converts `remaining_fraction` (0–1) into `used_pct = (1 -
  remaining_fraction) * 100`, and `reset_in_seconds` into an epoch
  `resets_at = now + reset_in_seconds` (the ISO-8601 `reset_time` sibling
  field is intentionally never parsed — see that script's header).
- `TokensChanged(conversation_id: s, input: t, output: t, context_size: t)`
  — per-session (unlike `UsageChanged`, so it carries the conversation id),
  emitted by `statusline.sh` from `.context_window.
  {total_input_tokens,total_output_tokens,context_window_size}`. No Claude
  Code equivalent: this is the one place agy is better instrumented, because
  the token counts arrive on the statusline itself rather than needing a
  transcript read (agy's `transcript.jsonl` carries no `usage` object at
  all — see `src/modules/antigravity.rs`'s `UsageTarget` doc comment).

## Install steps (what's ready today)

1. Copy `hooks.json.example` to `~/.gemini/config/hooks.json` (applies to
   every agy workspace) **or** `<workspace>/.agents/hooks.json` (that
   workspace only) — **not** `~/.gemini/antigravity-cli/settings.json`; agy
   hooks are a wholly separate file from agy settings. Strip the `//`-keyed
   comment fields first (they are valid JSON but not part of agy's schema).
   Fill in the real absolute path to `emit.sh` if this repo isn't checked
   out at `~/Developer/saola-panel`.
2. `chmod +x contrib/antigravity/emit.sh contrib/antigravity/statusline.sh`
   (already done in this checkout — verify both survived if you copy the
   files elsewhere).
3. Confirm `busctl --user` reaches your session bus (`busctl --user list`
   should not error) — same prerequisite as the Claude Code half.
4. Install `settings.json.example`'s `statusLine.command` into
   `~/.gemini/antigravity-cli/settings.json` (see that file's own header for
   the full merge steps) to get the quota gauges and the continuous
   `agent_state`-driven status updates. Wiring only `hooks.json.example`
   still works on its own — that alone gets `working`/`done`/`attention`
   dots from hooks, just without `UsageChanged`/`TokensChanged` or the
   `agent_state`-driven `seen`/`idle`/`attention` updates between hook
   events.

None of this repo's tooling ever writes under `~/.gemini/` — every install
step above is manual, same posture as `contrib/claude-code/`'s.

## A real bug in Jordan's existing agy statusline (found during stage 24, worth fixing regardless of stage 24b)

`~/.gemini/antigravity-cli/statusline.sh` is Jordan's current oh-my-posh
wrapper (not part of this repo — read-only reference here). Its embedded
Python normalizer (`normalize_payload`, statusline.sh:23-123) does:

```python
rate_limits = data.get("rate_limits", {})          # line 76 — agy never sends this key
...
five_hour = rate_limits.get("five_hour", {})        # line 81 — always {} as a result
...
if not five_hour and isinstance(quota, dict):       # line 87 — the one fallback attempt
    five_hour_pct = quota.get("percentage") or quota.get("used_percentage") or 0
    five_hour_resets = quota.get("resets_at") or (now + 18000)
```

Two independent misses, not one:

- **`rate_limits` doesn't exist in agy's real payload at all.** Stage 23's
  exhaustive scan of the `agy` binary's `json:` struct tags found zero
  occurrences of `rate_limits`, `five_hour`, `seven_day`, or `resets_at` as
  field names anywhere. This lookup is guaranteed to return `{}` on every
  invocation, on every version of agy that matches the binary scanned.
- **The fallback (`quota.get(...)`) also misses**, for a different reason:
  it assumes `quota` is a single flat dict with `percentage`/`used_percentage`
  and `resets_at` keys. The real shape (confirmed from the binary's `json:`
  tags) is `quota: map[string]{used_percentage, reset_time}` — a *map of
  named buckets*, not one dict, and the reset field is spelled `reset_time`,
  not `resets_at`. Even if `quota` were a flat dict, this fallback's key
  names would still miss.

Both misses fall through to the hardcoded defaults at
`statusline.sh:111-120`:

```python
"rate_limits": {
    "five_hour": five_hour if five_hour else {
        "used_percentage": 0,
        "resets_at": now + 18000
    },
    "seven_day": seven_day if seven_day else {
        "used_percentage": 0,
        "resets_at": now + 604800
    }
},
```

**The practical effect: every rate-limit number Jordan's oh-my-posh agy
statusline has ever shown him is fabricated** — always 0% used, always a
reset timer counting down from "now" (18000s / 5h and 604800s / 7d,
respectively), never real quota. This predates this stage and is unrelated
to the panel — it's a bug in Jordan's own script, surfaced here because
stage 24 was working in this file's neighbourhood and stage 23's recon into
the real payload shape is exactly what's needed to fix it.

**Now confirmed by the 2026-08-14 capture, with real numbers.** At capture
time Jordan's `gemini-weekly` bucket had `remaining_fraction: 0.078` — i.e.
he had **92.2% of that weekly quota already used** — and his `3p-weekly`
bucket was untouched (`remaining_fraction: 1`, 0% used). His existing
oh-my-posh statusline showed **0% used on both**, because of the two misses
above: it was never going to render anything but the hardcoded fallback,
regardless of how much quota remained. The panel's own `statusline.sh` (this
directory) reads `remaining_fraction`/`reset_in_seconds` correctly and would
have shown ~92.2% used with a real reset timer for the same payload — the
gap is entirely in Jordan's script, not in what agy sends. Fixing his script
is still out of scope for this repo (it's not part of `contrib/antigravity/`
and this repo's tooling never writes under `~/.gemini/`) — flagged here so
he can fix it himself, with the real bucket keys and field names now in
hand.

## Known limitation: no session-end event

agy's five hook events (`PreToolUse`, `PostToolUse`, `PreInvocation`,
`PostInvocation`, `Stop`) include nothing equivalent to Claude Code's
`SessionEnd`. `emit.sh ended` exists and works (a hand-rolled wrapper could
call it), but nothing agy fires today ever does — so, unlike Claude Code's
island where a stale dot is an edge case, an Antigravity dot **normally**
lingers until the panel restarts, even for conversations long finished. This
is accepted, not a bug to fix here (Jordan, 2026-08-14) — see
`src/modules/antigravity.rs`'s module doc comment for the full reasoning
against adding a TTL sweep (it would require a timer, which is the poll
CLAUDE.md forbids).
