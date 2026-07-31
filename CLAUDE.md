# saola-panel — agent instructions

Status bar for Saola, a Linux desktop environment built in Rust. First real consumer of
the design-system crate [saola-theme](https://github.com/JorDunn/saola-theme). Target
compositor: **niri** (wlr-layer-shell). Stack: stable iced 0.14 + iced_layershell 0.19.x
+ zbus 5.

The design source of truth is `~/Developer/saola-theme/design/SAOLA-STYLE-GUIDE.md`
(to be copied into this repo as `docs/SAOLA-STYLE-GUIDE.md` in Phase 2's final stage) — geometry, components,
surfaces, motion, and the panel config sketch all come from it. Its NetworkManager
mention is overridden by this repo's iwd rule.

## Commands

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings   # CI gate — keep it green
cargo fmt --check                            # CI gate
cargo run                                    # bar anchored top (needs a Wayland session)
cargo run -- --bottom                        # bar anchored bottom
```

## Architecture

Single binary crate. `src/main.rs` owns the `Panel` state, the single flat `Message`
enum, update/view, and the layer-shell setup (anchor top|left|right, height
`sizes.panel_bar`, exclusive zone; `--bottom` flips the anchor). Each bar module lives in
`src/modules/<name>.rs` and exposes: a state struct, `view(&Theme) -> Element`, and
`subscription() -> Subscription<Message>`. Layout is ledger-style: one padded row,
left / center (clock) / right (status pills) — keep this outer layout swappable so the
Islands style (floating pill clusters) can be added later as a mode.

D-Bus modules (battery = UPower, Wi-Fi = iwd, media = MPRIS, tray = SNI, Claude Code =
`io.saola.ClaudeCode1` signals) use async zbus proxies/streams feeding iced
subscriptions via stream channels — never blocking calls on the UI thread. Non-D-Bus
signal sources have their own bridges: libpulse (volume) runs on a dedicated thread
pushing snapshots through an unbounded channel ("thread bridge", handoff 10); niri IPC
(columns minimap) reads JSON lines from `$NIRI_SOCKET`. **Every module maps to a
signal, never a poll** — nothing in the panel ticks faster than the clock. A module
whose service is absent (no battery, no iwd, no pulse, no `$NIRI_SOCKET`) renders
nothing and must not take the panel down.
Jordan runs **iwd, not NetworkManager** — never write NetworkManager code.

Phase 2 (stages 7–22 in PLAN.md) grows this into the full-spec panel: per-module
message enums nested in the panel enum, SVG icon infra (Lucide, stroke 2.75), mark +
MPRIS + volume + niri-columns + Claude Code + tray (full dbusmenu) modules,
multi-window daemon architecture (`SurfaceRole` registry), KDL config
(`~/.config/saola/panel.kdl`), Islands layout (three translucent islands; the
notifications island is a flagged future slot), and popover infrastructure (one open
at a time) with quick settings.

## Design language (binding — the theme crate is the authority)

- The bar is a **shell surface: always ink** (`Surface::Ink`). Never toggle it to paper.
- **Zero hardcoded colors or sizes.** Every value comes from `saola_theme::tokens`
  (bar height `panel_bar` 48, pill height `panel_pill` 40, ledger margin 20, bar font 13,
  bar icons 15) and every widget style from `saola_theme::style` helpers. If a needed
  style doesn't exist, add it to saola-theme (its CLAUDE.md governs that repo) — don't
  restyle locally.
- **Three colors, never a fourth**; the one rule: ivory fill = at rest, terracotta fill =
  on/selected/live. A module's "active" state (charging, connected-and-transferring…) is
  terracotta per this rule — never a new color, no green/red status colors.
- On the bar, that rule applies at the *element* scale, not the pill scale (concept 4b,
  PLAN.md Stage 14.5): status modules are bare ivory icon + text directly on ink;
  terracotta marks live states as a small accent (glyph, dot, `accent_light` text),
  never a whole-pill flood; **in ledger style** the clock is the bar's only solid ivory
  pill and media its only (subtle-fill) pill besides it. At most one solid terracotta
  element per surface.
- **Islands style (listing 2a, "Ink & ivory") differs in surface treatment, not in the
  rule**: the island scrim is itself the surface, so nothing nests a second one inside
  it — the clock is plain ivory text on the scrim, never a pill. In the centre island the
  column strip's focused dash is the one solid ivory element; that dash is a documented
  exception to "terracotta = selected" (terracotta never appears on the strip). Layout
  stays out of module code, but a module's surface treatment may legitimately differ per
  style (decided 2026-07-31) — `modules::clock` is the only one that does.
- Text/dividers on the bar use the `theme.on(Surface::Ink)` roles; quiet states
  (disconnected, idle) use `secondary`/`disabled`, not custom grays.
- Everything is a pill or over-rounded rectangle; focus is the 2 px terracotta ring.

## Conventions

- The theme dependency is pinned to a release tag (currently
  `tag = "saola-theme-v0.3.0"`, with a matching `version`).
  Bumping it is a deliberate, reviewed change — never switch to `branch = "main"`.
- Copy the established module pattern for new modules (read an existing one first).
  From stage 7 on, each module owns its `pub enum Message`, nested as a variant of the
  panel's outer enum (`Message::Battery(battery::Message)`).
- Jordan is newer to Rust: comment the non-obvious (async ownership, proxy macros,
  stream bridging) as teaching notes; prefer explicit code over clever abstraction.
- Out of scope (don't build speculatively): **everything notifications** (daemon,
  popups, centre, bar indicator — a future saola-notifications component owns it) and
  **network management UI** (iwd status display only; the QS Wi-Fi toggle is opt-in
  per stage 17).

## Releases

- **Commit messages must follow Conventional Commits** (`feat:`, `fix:`, `feat!:` for
  breaking) — release-plz derives the semver bump and changelog from them. Pre-1.0, a
  breaking change bumps the minor version, and so does a plain `feat:`
  (`features_always_increment_minor` in `release-plz.toml`).
- This crate is not on crates.io, so the release-pr CI job feeds release-plz a checkout
  of the latest `saola-panel-v*` tag as its released baseline
  (`--registry-manifest-path`); without it, version detection asks crates.io and
  concludes "already up-to-date" forever. Tags are pinned to the workspace-style
  `saola-panel-v{version}` via `git_tag_name`.
- The `saola-theme` git dependency carries both `tag` and `version`, and the two must
  move together (cargo errors if they disagree).
- Never bump the version in Cargo.toml or edit CHANGELOG.md by hand — the release-plz
  release PR does both. Config: `release-plz.toml`; workflow:
  `.github/workflows/release-plz.yml` (needs the `RELEASE_PLZ_TOKEN` repo secret for CI
  to run on release PRs).
- Changelog-invisible commits use `chore:`/`ci:`/`docs:`/`test:` types.
