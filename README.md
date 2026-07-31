# saola-panel

The status bar for **Saola**, a Linux desktop environment written in Rust, targeting the
**niri** Wayland compositor via wlr-layer-shell. Themed entirely from
[saola-theme](https://github.com/JorDunn/saola-theme) — the one place the Saola look is
defined; this panel hardcodes no color or size of its own.

Two layout styles, one renderer: **ledger** (one floating ink pill, left / center /
right) and **islands** (three separate translucent pill clusters floating over the
wallpaper). Both read the same `panel.kdl` module lists and compose the same per-module
views — see [Configuring it](#configuring-it).

<!-- screenshot: ledger style — one ink pill top edge, mark+media left,
     clock+niri-column-dashes centered, volume/network/battery/claude/tray right,
     wallpaper visible only outside the pill's rounded ends -->

<!-- screenshot: islands style — three separate translucent pill clusters over
     wallpaper, same module content as ledger, no connecting bar between them -->

## Running it

```bash
cargo build
cargo test
cargo run
```

There is no `--bottom` flag anymore (Stage 14 replaced it — see
[Configuring it](#configuring-it)): the panel reads `~/.config/saola/panel.kdl` once,
at boot, and everything about its layout — style, edge, margin, height, module lists,
mark, color overrides — comes from that file. A restart is required to pick up a config
change; there is no live reload.

For quick manual testing without editing the config file, four CLI flags override
whatever `panel.kdl` says for that one run only: `--ledger` / `--islands` and
`--top` / `--bottom`. They exist purely as a development convenience (see
`main.rs`/`config::CliOverrides`) — `panel.kdl` is the real interface.

### Build dependencies

- **System `libpulse`** (headers + pkg-config) — `libpulse-sys` links against it for the
  volume module. Missing it fails the build, not just the run.
- **niri** is *not* a build dependency — `niri-ipc`'s types compile with no compositor
  present. The niri-columns module simply renders nothing if `$NIRI_SOCKET` is unset at
  runtime.
- This checkout currently carries a **temporary `[patch]`** in `Cargo.toml` pointing
  `saola-theme` at a local sibling checkout (`../saola-theme`) instead of the pinned
  `saola-theme-v0.3.0` tag, pending a theme release that ships the islands' focused-dash
  fix. Building this repo elsewhere needs that sibling checkout until the patch is
  retired (see the comment above the `[patch]` block in `Cargo.toml`).

## Configuring it

`~/.config/saola/panel.kdl` (or `$XDG_CONFIG_HOME/saola/panel.kdl`). Every knob is
optional; [`examples/panel.kdl`](examples/panel.kdl) documents all of them at their
built-in default (copying it unchanged into your config is a no-op) and is the
authoritative reference — read it before editing your own copy.

The resilience contract, because a status bar must never fail to start over a config
typo:

| Situation | Result |
|---|---|
| No `panel.kdl` at all | Built-in defaults, silent |
| File present, not valid KDL at all | One `eprintln!` naming the file + the parse error, then the **whole file** falls back to defaults |
| One knob's value is nonsense (bad `style`/`edge`/`mark` string, bad hex color) | A warning naming that knob; **only that field** defaults, the rest of the document still applies |
| An unknown module name in a list | A warning naming it; **that entry** is dropped, the rest of the list loads |
| A list block present but explicitly empty (`left { }`) | That region is really empty — distinct from the block being absent, which uses the region's default list |

Top-level knobs: `style` (`"ledger"` / `"islands"`), `edge` (`"top"` / `"bottom"`),
`margin`, `height`, the `left { }` / `center { }` / `right { }` module lists, `mark`
(`"builtin:horns"` / `"builtin:notch"` / `"file:<path>"` / `"none"`), and a `colors { }`
block overriding `ink` / `paper` / `accent` as hex strings. See `examples/panel.kdl` for
the full grammar and every default value.

**Known limitation**: `colors { }` only overrides `palette.{ink,paper,accent}` — the
alpha-stepped text/fill roles (`on_ink.primary`/`secondary`/etc.) are not recomputed from
a custom palette, so overriding `ink` moves the bar's background but not its text color.
Fixing this properly is a `saola-theme` change (recomputing `OnSurface` from the actual
palette), not a panel-side workaround.

## Modules

Every module maps to a **signal, never a poll** (CLAUDE.md's binding rule) — the table
below names each one's source, because "which of these costs me battery" (spec §10) is
the question this should let you answer at a glance.

| Module | Signal source | Notes |
|---|---|---|
| Mark | none (static glyph) | No signal source at all — its `Message` is an uninhabited enum. Which glyph draws comes from `panel.kdl`'s `mark` knob. |
| Clock | 1-minute-aligned timer | The one deliberate exception to "signal, not poll" — CLAUDE.md names it explicitly as the panel's slowest permitted tick. |
| Media (`mpris`) | MPRIS, session D-Bus (`org.mpris.MediaPlayer2.*`) | Tracks however many players come and go; picks the most-recently-active playing (else paused) one. |
| niri columns | niri IPC (`$NIRI_SOCKET`, event-stream) | Not D-Bus — newline-delimited JSON over a Unix socket. Renders nothing without niri. |
| Volume | PulseAudio client protocol over `pipewire-pulse`, via `libpulse-binding` | The one **thread bridge** in this codebase — a dedicated OS thread owns libpulse's C mainloop; every other D-Bus module is an async task. |
| Network (Wi-Fi) | iwd, system D-Bus (`net.connman.iwd`) | Jordan runs iwd, not NetworkManager — this panel never speaks NetworkManager, overriding the style guide's own NetworkManager mention. |
| Battery | UPower, system D-Bus (`org.freedesktop.UPower`) | Hidden on a machine with no battery. |
| Claude Code | custom broadcast signal, session D-Bus (`io.saola.ClaudeCode1`) | No service to poll — a hook script fires one signal and exits. See [Claude Code integration](#claude-code-integration) below. |
| Tray (SNI) | StatusNotifierItem/Watcher, session D-Bus (`org.kde.StatusNotifierWatcher`) | The panel **serves** the watcher if nothing else on the session does, rather than only consuming one — the first module to own a bus name. |

Popover content (quick settings, tray context menus) is not a bar module — it reads the
bar modules' own state (`Volume`, `Media`, the tray registry) directly; see
`src/popovers/`.

## Claude Code integration

The Claude Code pill (rightmost bar element) tracks Claude Code sessions via a small
broadcast-signal protocol, not by watching Claude Code's own process or files:

- **Bus**: session bus · **path** `/io/saola/ClaudeCode` · **interface**
  `io.saola.ClaudeCode1` · **signal** `StatusChanged(session_id: s, status: s)`, where
  `status` ∈ `working | attention | idle | ended`.
- [`contrib/claude-code/emit.sh`](contrib/claude-code/emit.sh) is the hook-side emitter:
  a `busctl --user emit` one-liner that reads the session id from
  `$CLAUDE_CODE_SESSION_ID` (falling back to the hook's stdin JSON) and exits. It never
  fails loudly — every error path exits 0 quietly, since a missing status update is far
  less disruptive than a hook that errors mid-session.

**Manual setup step (Jordan's, not automated by this repo):** merge
[`contrib/claude-code/settings.hooks.json.example`](contrib/claude-code/settings.hooks.json.example)
into the `"hooks"` key of `~/.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "matcher": "", "hooks": [
        { "type": "command", "command": "$HOME/Developer/saola-panel/contrib/claude-code/emit.sh working" }
      ] }
    ],
    "Notification": [
      { "matcher": "", "hooks": [
        { "type": "command", "command": "$HOME/Developer/saola-panel/contrib/claude-code/emit.sh attention" }
      ] }
    ],
    "Stop": [
      { "matcher": "", "hooks": [
        { "type": "command", "command": "$HOME/Developer/saola-panel/contrib/claude-code/emit.sh idle" }
      ] }
    ],
    "SessionEnd": [
      { "matcher": "", "hooks": [
        { "type": "command", "command": "$HOME/Developer/saola-panel/contrib/claude-code/emit.sh ended" }
      ] }
    ]
  }
}
```

Mapping: `UserPromptSubmit → working`, `Notification → attention`, `Stop → idle`,
`SessionEnd → ended`. The command path is hardcoded to this repo's location on Jordan's
machine (`$HOME/Developer/saola-panel/...`) — update it if the repo ever moves. After
merging, run a real Claude Code session in any other project and watch the pill: it
should show nothing while idle-only, `working` during generation, and `input?` on a
permission prompt.

## Design language (binding)

The bar is a **shell surface: always ink** — never toggled to paper, in either layout
style. Every color and size comes from [saola-theme](https://github.com/JorDunn/saola-theme)'s
tokens; the code hardcodes none. Three colors, one rule: ivory fill = at rest, terracotta
fill = on/selected/live — applied at the *element* scale on the bar (bare icon + text
directly on ink, not a whole pill flood) with the ledger clock and media pill as the two
deliberate pill exceptions. See [`docs/SAOLA-STYLE-GUIDE.md`](docs/SAOLA-STYLE-GUIDE.md)
for the full spec and CLAUDE.md for this repo's binding overrides of it (iwd instead of
NetworkManager; no notifications; no network-management UI).

## Known quirks and deliberate gaps

Carried forward from the Phase 2 stage handoffs — read as "known, not forgotten," not
bugs:

- **Three islands, not four.** The style guide's Islands layout has a fourth surface
  (mark+media, clock+strip, status, *notifications*); this build ships the first three.
  Everything notifications — daemon, popups, a bar indicator — is out of scope for this
  phase by explicit decision; a future `saola-notifications` component owns it. Adding
  the fourth is one more `IslandKind` variant plus one arm in each of the three matches
  that consume it, all compiler-enforced.
- **Popover horizontal placement is a flat approximation.** Every popover (quick
  settings, every tray menu regardless of which tray icon triggered it) anchors at the
  same fixed margin from the panel's edge, not under the icon that opened it — iced 0.14
  cannot measure a laid-out widget's screen position, so there's no way to compute a
  per-trigger anchor. Accepted for v0.2 per PLAN.md's own text.
- **The tray menu's popover height is a fixed row budget (12 rows), not a per-menu
  measurement**, because a layer-shell surface must declare its size before the
  compositor creates it — before the async fetch that would say how many rows there
  actually are has even started. A menu with more rows than the budget loses rows off
  the bottom; a real fix needs either a scrollable row list or resizing the surface after
  the fetch lands.
- **Icon-asset migration debt.** `src/icons.rs` and `assets/icons/` (the Lucide SVG
  set + tinting helper) are built panel-local for now but are meant to be shared by the
  launcher, greeter, and lock screen eventually — they belong in `saola-theme` (or a
  dedicated `saola-icons` crate). The migration is mechanical (move the files, swap the
  import) whenever a second consumer exists; not done speculatively here.
- **Quick settings' 2×2 grid (Wi-Fi, Bluetooth, Do Not Disturb, Airplane Mode) is
  entirely placeholder** — styled, disabled, no backend. Bluetooth has no module in this
  phase's plan; Do Not Disturb belongs to the excluded notifications component; a Wi-Fi
  power toggle was explicitly declined (iwd status display only, per CLAUDE.md).
- **Stale Claude Code sessions never expire.** A session that dies without emitting
  `ended` (a killed terminal, a sleep) leaves its last status in the map forever — no TTL
  sweep exists, because a sweep needs a timer ticking regardless of whether anything
  changed, which is exactly the poll CLAUDE.md forbids on every other path. Self-heals on
  a panel restart.
- **The tray's `NeedsAttention` state** renders as a 2px terracotta ring around the icon
  — precedent-following (the same idiom `card_urgent` uses) but genuinely unreviewed
  against any design mockup. Trivial to change if it doesn't read right live.
- **The tray context menu's row style (`row_style` in `popovers/tray_menu.rs`) is
  hand-built**, because none of `saola_theme::style::button`'s four variants is "quiet at
  rest, terracotta on hover/press only." Flagged as a `saola-theme` candidate
  (`style::button::selectable`, say), not promoted — this file is its only consumer today.
- **The mute toggle in quick settings is terracotta when muted** — a deliberate reading
  of "generic switch, on = terracotta" (per the theme's own toggle convention), distinct
  from the bar's own mute *readout*, which deliberately never uses terracotta (a level
  readout isn't a control that's switched on). Documented at length in
  `popovers/quick_settings.rs` in case it reads as inconsistent; if Jordan disagrees, the
  fix is a one-line widget swap.
- **The media row and quick-settings padding reuse tokens that aren't quite the right
  semantic fit** (`island_gap` for icon↔label spacing inside a pill, `panel_margin_ledger`
  for popover content padding) rather than inventing local values — flagged as
  `saola-theme` token candidates (`pill_content_gap`, `popover_padding`) in several
  handoffs, none acted on since this repo doesn't restyle locally.
- **A real SNI application's icon has never been seen on screen** — the wire protocol
  (icon-name lookup, `IconPixmap` byte-shuffle, `Activate`, dbusmenu) is proven end to
  end over a real D-Bus session in this repo's test suite, but no Wayland session was
  available while building it, so nothing tray-related has been visually confirmed
  rendering.

## Docs

[`docs/SAOLA-STYLE-GUIDE.md`](docs/SAOLA-STYLE-GUIDE.md) is a verbatim copy of the
design system's binding spec (source of truth: `saola-theme`'s own repo) — geometry,
components, surfaces, motion, and the panel config sketch all come from it. Its
NetworkManager mention is overridden by this repo's iwd rule (see CLAUDE.md).

## Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual
licensed as below, without any additional terms or conditions.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
