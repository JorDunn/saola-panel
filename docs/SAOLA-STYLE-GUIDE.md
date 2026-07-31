# Saola Design System

A style guide for the Saola desktop environment: a niri-based Wayland desktop with a
custom panel, launcher, greeter and shell surfaces written in Rust/Iced.

This document is the source of truth. If an implementation disagrees with it, the
implementation is wrong. Everything here was derived from the Saola UI concept work and
is meant to be handed to an implementer (human or agent) without further explanation.

---

## 1. The one rule

Saola has exactly **three colours** and one rule that governs all of them:

| Colour | Token | Hex | Means |
|---|---|---|---|
| Ink | `ink` | `#0C0A00` | Surface. Every shell surface is ink. |
| Ivory | `paper` | `#FFFFF0` | A control at rest — off, unselected, available. |
| Terracotta | `accent` | `#C67139` | On, selected, focused, live, active. |

**Ivory fill takes ink text. Terracotta fill takes ivory text.** Both states are
equally bright, so switching a control changes only its hue — the interface never
flickers lighter or darker as you move through it.

Corollaries that follow from the rule and must not be broken:

- Terracotta is never decorative. If something is terracotta, it is the thing you are
  on, the thing that is running, or the thing that will happen when you press Enter.
- There is no third colour. No red for danger, no green for success, no blue for links.
  Severity is carried by **wording**, not hue. A destructive confirm button is
  terracotta because it is the live action, not because it is dangerous.
- Only one element in a given surface is terracotta at a time, with one exception:
  a list may have one selected row while a separate toggle is on.
- Sage `#7A8A5E` exists **only** in the terminal palette (ANSI green) and in the
  Organic-derived brand material. It is not a shell colour.

### Neutral ramps

On ink surfaces, ivory is stepped with alpha. Never introduce a grey.

| Role | Value |
|---|---|
| Primary text | `#FFFFF0` |
| Secondary text | `rgba(255,255,240,0.72)` |
| Tertiary text / metadata | `rgba(255,255,240,0.55)` |
| Quaternary / disabled label | `rgba(255,255,240,0.40)` |
| Disabled control label | `rgba(255,255,240,0.35)` |
| Hairline / divider | `rgba(255,255,240,0.12)` |
| Subtle fill (rows, inset panels) | `rgba(255,255,240,0.07)` |
| Fill (secondary buttons, tracks) | `rgba(255,255,240,0.12)` |
| Strong fill (track filled, chips) | `rgba(255,255,240,0.16)` |

On ivory surfaces (windows), ink is stepped the same way:

| Role | Value |
|---|---|
| Primary text | `#0C0A00` |
| Secondary text | `rgba(12,10,0,0.70)` |
| Tertiary text | `rgba(12,10,0,0.55)` |
| Metadata | `rgba(12,10,0,0.45)` |
| Hairline / divider | `rgba(12,10,0,0.10)` |
| Subtle fill (zebra rows) | `rgba(12,10,0,0.04)` |
| Fill (secondary buttons) | `rgba(12,10,0,0.08)` |
| Track | `rgba(12,10,0,0.14)` |

### Accent ramp

Terracotta needs exactly two variants:

| Token | Hex | Use |
|---|---|---|
| `accent` | `#C67139` | Every fill. |
| `accent-light` | `#F6A06B` | Accent-coloured **text on ink** only (hints, error copy, prompt highlights). `#C67139` text on ink fails contrast. |
| `accent-dark` | `#8C491A` | Accent-coloured **text on ivory** only. |

---

## 2. Surfaces

Two grounds, and the choice is not aesthetic:

- **Ink** — everything the shell draws: panel, popovers, launcher, overview, notification
  centre, greeter, lock, power menu, boot. Ink surfaces float over the wallpaper.
- **Ivory** — windows. Saola Settings, the file picker, anything with a title bar. An
  ivory window sits next to GTK/Qt apps and should not look like shell chrome.

Both variants are specified for the file picker and the control kit. Ship ivory as the
window default; ink is a user preference.

### Scrims over the wallpaper

The wallpaper is never changed between states, only revealed at different strengths.
Use these exact values so boot → greeter → lock → desktop has no visible image change:

| State | Scrim |
|---|---|
| Desktop | none |
| Boot menu / boot splash | `rgba(12,10,0,0.78)` |
| Shutdown | `rgba(12,10,0,0.88)` |
| Lock / greeter at rest | `linear-gradient(180deg, rgba(12,10,0,.18), rgba(12,10,0,.02) 34%, rgba(12,10,0,.34))` |
| Lock / greeter awake (prompt shown) | `rgba(12,10,0,0.62)` |
| Launcher open | `rgba(12,10,0,0.52)` + 4px blur |
| Overview | `rgba(12,10,0,0.55)` + 6px blur |
| Capture overlay (outside selection) | `rgba(12,10,0,0.62)` |
| Modal dialog backdrop | `rgba(12,10,0,0.62)` + 7px blur |

Translucent panel surfaces over the wallpaper use `rgba(12,10,0,0.60)`–`0.78` with
`backdrop-filter: blur(12–14px)`. Opaque ink (`#0C0A00`) is correct for popovers,
launcher and dialogs.

---

## 3. Type

**IBM Plex.** Three faces, three jobs, no overlap.

| Face | Weights | Used for |
|---|---|---|
| IBM Plex Sans | 400, 500 | All interface text. Everything you scan. |
| IBM Plex Serif | 400 | Display only — the wordmark, the lock/greeter clock, panel headings, dialog titles, workspace numerals. Never inside the panel bar. |
| IBM Plex Mono | 400, 500 | Keycaps, file paths, hex values, terminal, kernel versions, D-Bus names. |

### Scale

| Role | Size / weight | Notes |
|---|---|---|
| Lock clock | Serif 400 · 168px · `-0.025em` | Tabular numerals |
| Screen title (greeter "Starting niri…") | Serif 400 · 44px | |
| Panel heading (Session, Notifications) | Serif 400 · 22–30px | |
| Dialog title | Serif 400 · 22–26px | |
| Section heading in a window | Serif 400 · 20px | |
| Launcher input | Sans 400 · 22px | |
| Body / row title | Sans 500 · 13–13.5px | |
| **Panel and bar text** | **Sans 500 · 13px** | Hard minimum. Never below 13px in the bar. |
| Secondary row text | Sans 400 · 12–12.5px | |
| Metadata | Sans 400 · 11.5–12px | |
| Section label (uppercase) | Mono 500 · 10–11px · `0.12em` | |
| Keycap | Mono 500 · 11–12px | |

**Tabular numerals** (`font-variant-numeric: tabular-nums`) on every clock, percentage,
timer and countdown so nothing reflows tick to tick.

Never mix serif and sans within one bar, pill or row. The serif appears at most twice
per screen.

---

## 4. Geometry

Everything is a pill or an over-rounded rectangle. There are no sharp corners.

| Element | Radius |
|---|---|
| Buttons, chips, pills, inputs, toggles, list rows | `999px` |
| Popovers, quick settings, launcher, power menu | `30px` (34px for the widest) |
| Windows | `24px` |
| Notification cards | `26px` |
| Inset panels, media rows | `18–22px` |
| Icon tiles | `12–14px` |
| Checkboxes | `7px` |
| Capture selection | `6px` |

### Sizes

| Element | Value |
|---|---|
| Panel pill height (islands) | `40px` |
| Panel bar height (ledger) | `48px` |
| Panel margin from screen edge | `26px` (islands), `20px` (ledger) |
| Gap between islands | `10px` |
| Popover top offset from panel | `72px` from screen top |
| Popover width | `400–460px` |
| Launcher width | `620–640px` |
| Notification card width | `440px` |
| Hit target, minimum | `40px` (bar) / `44px` (touch and lock/greeter) |
| Icon in bar | `15–16px` |
| Icon in menus and rows | `16–19px` |
| Icon in a bare-icon menu (power) | `30–34px` in an `84px` target |
| Icon stroke width | **`2.75`** everywhere, at every size |

Icons are [Lucide](https://lucide.dev) at stroke-width 2.75. No filled icons except
play/next/record/stop, which are solid.

---

## 5. Motion

Short, and only where it carries meaning.

| Transition | Duration / easing |
|---|---|
| Hover colour change | `140ms ease` |
| Popover open | `160ms ease-out`, from `translateY(-6px) scale(0.985)` + fade |
| Lock/greeter wake (rest → prompt) | `450ms ease` on opacity and `scale(0.97 → 1)` |
| Countdown / progress rule | `1s linear` |
| Notification popup | see below |
| Boot rule (indeterminate) | `1.8s ease-in-out` loop |

### Notification popup timing — exact

Total **6.35s**:

1. **0 → 0.35s** slide in from the right edge (`translateX(120% → 0)`), ease-out, fading in.
2. **0.35 → 5.35s** at rest. 5 seconds.
3. **5.35 → 6.35s** fade to 0, linear, **with no movement**. It leaves in place.

A terracotta life rule under the card scales from 1 to 0 over the same span. Hover
pauses both. Urgent notifications have no life rule and never auto-dismiss.

Nothing else in the shell animates. No spinners, no pulsing, no parallax — the whole
system is built to poll as little as possible and the visuals should reflect that.

---

## 6. Components

### Pill button

Height 38–40px (46–48px in overlays), `border-radius: 999px`, horizontal padding
16–22px, `font: 500 13–13.5px "IBM Plex Sans"`.

| Variant | On ink | On ivory |
|---|---|---|
| Primary (the live action) | `#C67139` bg, `#FFFFF0` text | `#C67139` bg, `#FFFFF0` text |
| Secondary | `#FFFFF0` bg, `#0C0A00` text | `rgba(12,10,0,.08)` bg, `#0C0A00` text |
| Ghost | transparent, `#F6A06B` text | transparent, `#8C491A` text |
| Disabled | `rgba(255,255,240,.10)` bg, `rgba(255,255,240,.35)` text | `rgba(12,10,0,.08)` bg, `rgba(12,10,0,.35)` text |

### Bare-icon menu

Used for the power menu and the boot menu. Icons sit **directly on the surface** with no
pill behind them: ivory at 55% opacity at rest, full terracotta when hovered or selected.
A single shared label sits below the row, in a fixed-height block so the panel does not
resize as the pointer moves.

### Toggle / switch

Track `48×28px`, knob `22px` ivory. Off track = `rgba(x,x,x,.16–.20)`. On track =
terracotta. The knob is ivory in both states.

### Checkbox / radio

`20px`. Off = transparent with a 2px `rgba(…,.28–.32)` ring. On = terracotta fill;
checkbox shows an ivory tick, radio shows a 4px inset ring in the surface colour.

### Text field

Height 38px (60–64px on lock/greeter), `999px`, subtle fill. Focused = 2px terracotta
outline at `outline-offset: -2px`, terracotta caret. **Never a browser-default focus ring.**

### List row

Height 36–42px, `999px`, transparent at rest, terracotta when selected with ivory text
and lightened metadata (`rgba(255,255,240,.75)`).

### Popover

Opaque ink, `30px` radius, `box-shadow: 0 18px 48px rgba(12,10,0,.5)`, 20–22px padding.
Anchored 72px from the screen top, 26px from the relevant edge. **Only one popover open
at a time** — opening one closes the others.

Popovers grow **downward** from their trigger when the trigger is near the top of the
screen, and must never overlap the control that opened them.

### Notification card

440px, ink, `26px`, `0 18px 48px rgba(12,10,0,.5)`. 36px icon tile (ivory, ink glyph),
title (Sans 500 13px) with app name right-aligned in tertiary, body at 13px/1.45,
optional ivory action pills, and a 3px life rule across the bottom.

Popups stack at most **three**; the fourth replaces the oldest. A second notification
from an app already on screen replaces its card and resets the clock.

### Notification centre

460px, anchored 72px from the top and 26px from the right, `max-height: calc(100% - 98px)`
with the list scrolling. **It hugs its content and only reaches full height when there is
enough to show.** Grouped by application, each group collapsible, with a count chip.

---

## 7. Surface inventory

| Surface | Ground | Notes |
|---|---|---|
| Panel — Islands (default) | Ink pills over wallpaper | Four separate layer-shell surfaces: mark + media, clock + column strip, status, notifications. Each redraws independently. |
| Panel — Ledger | One ink bar | Same modules, fixed slots, edge to edge. |
| Launcher | Ink, 620–640px | One field, ranked rows, group header in serif. Selection terracotta. |
| Overview | Ink over blurred wallpaper | Workspaces as horizontal strips stacked vertically, window widths proportional to real column widths. |
| Notification centre | Ink, right | Grouped, collapsible, DND toggle, media footer. |
| Quick settings | Ink, right | 2×2 toggles, sliders, media. |
| Lock | Wallpaper | Clock, date, temperature centred, nothing else. Click reveals avatar → name → password. |
| Greeter | Wallpaper | Identical to lock plus a user list and a session list as ivory pills below the field. |
| Power menu | Ink, centred | Bare icons, one shared label. |
| Boot menu | Ink (optionally wallpaper at 78%) | Bare icons and text, terracotta on the active entry, progress rule as the countdown. |
| Boot / shutdown splash | Ink | Mark and one indeterminate rule. Shutdown drops terracotta entirely. |
| Settings | Ivory window (ink variant available) | Sidebar + pages. Every module row names its D-Bus source. |
| File picker (portal) | Ivory window (ink variant available) | Places sidebar, breadcrumb pills, list/grid, selection terracotta. |
| Capture overlay | Wallpaper + scrim | Dashed terracotta selection edge with round terracotta handles, size readout, floating toolbar. |
| Terminal | Ink | See §9. |

### niri specifics

- niri is a **scrollable strip**, not a grid. The panel's centre module is a live minimap
  of the current strip: one dash per column, the focused one widened, off-screen columns
  shown as stubs at each end.
- Window decoration is a **2px border and nothing else**. Focus is the only state and
  terracotta is the only thing that says it. Ordinary apps get no title bar at all;
  Saola's own windows draw a 46px header.
- **There is no minimise and no taskbar.** Do not add a minimise button. The closest
  equivalent is "Send to workspace", which belongs in the window menu.

---

## 8. The mark

Saola ships two built-in marks and reads an SVG from disk otherwise.

- **Horns** (default) — two strokes rising and splaying: `M9 21c0-7-1.2-12-3-18` and
  `M15 21c0 -7 1.2-12 3-18` on a 24×24 viewBox at stroke 2.75. Reads as an antelope at
  512px and as a scroll cue at 15px.
- **Notch** (alternative) — a ring broken by the focused column, with a dot at the break.
- **Custom** — any single-path 24×24 SVG. It inherits the surface's colour and stroke
  weight, so a distro mark drops in without looking pasted on.

The mark is 15–16px in the bar, 46px on boot, 76px on splash, 112px on lock.

---

## 9. Terminal (Alacritty + oh-my-posh + oh-my-zsh)

**Terracotta is deliberately absent from the sixteen ANSI slots.** If it appeared as
"red", every `ls` would shout the accent and the prompt would stop meaning anything.
Slot 1 is a warm brick; slot 2 is the shell's sage.

| Slot | Normal | Bright |
|---|---|---|
| black | `#0C0A00` | `#3A342A` |
| red | `#C05B3C` | `#E07A57` |
| green | `#7A8A5E` | `#9CB077` |
| yellow | `#C69139` | `#E0B25C` |
| blue | `#5E7A8A` | `#7C9CAE` |
| magenta | `#9E6B7A` | `#C08A99` |
| cyan | `#6E9188` | `#8FB3A8` |
| white | `#E8E0CE` | `#FFFFF0` |

Background `#0C0A00`, foreground `#E8E0CE`, cursor `#FFFFF0` on ink text, selection
`#C67139`.

### Prompt

One line: `[user] / [path] / [git]  ❯ command`, with the right prompt (`toolchain`,
`duration`, `time`) flushed right.

- Segments are joined by **angled `/` separators** (Nerd Font `` `` powerline
  slants). The **outer** ends of the run are **round caps** (`` ``); only the joins
  between segments are angled.
- Padding inside each segment is tight — roughly one character each side.
- Everyday states move **only** the git segment and the caret, so the prompt never
  reflows between runs: clean = sage git segment, dirty = amber with `±n`, failed =
  brick caret plus `exit n` on the right.
- Root and SSH deliberately break that: root turns the user segment brick, SSH replaces
  it with `user@host` in bright blue. Those are the two states you must never miss.
- Provide a `plain` fallback with no glyphs and no colour beyond the sixteen, for a TTY
  or an SSH session into a box without the font.

---

## 10. Configuration

The two panel styles are one renderer with two layout passes, sharing one module list:

```kdl
panel {
  style "islands"          // or "ledger"
  edge "top"
  margin 26
  height 40

  left   { mark; mpris }
  center { clock; niri-columns }
  right  { volume; network; battery; tray; notifications }

  mark "builtin:horns"     // or "builtin:notch", "file:~/.icons/arch.svg", "none"

  colors { ink "#0C0A00"; paper "#FFFFF0"; accent "#C67139" }
}
```

Every module maps to a **signal, not a poll** — niri IPC, `org.freedesktop.UPower`,
`…NetworkManager`, `org.bluez`, `org.mpris.MediaPlayer2`, StatusNotifierItem. If a
module needs a timer faster than 1 Hz, it does not belong in the panel. Settings shows
each module's source next to it, because "which of these costs me battery" is the
question that window should answer.

---

## 11. Checklist for any new surface

1. Is it shell chrome (ink) or a window (ivory)?
2. Is there exactly one terracotta element, and is it the live one?
3. Is every control at rest ivory, and is its text the opposite of its fill?
4. Is all bar text ≥13px Plex Sans 500, with tabular numerals on anything counting?
5. Are the corners pills or ≥18px radii?
6. Does the serif appear at most twice, and never inside the bar?
7. Are icons Lucide at stroke 2.75?
8. Does anything animate that isn't a notification, a popover, or a hover?
9. Does every popover close its siblings, open away from its trigger, and hug its content?
10. Did you add a colour? Remove it.
