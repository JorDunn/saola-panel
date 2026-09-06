# contrib/notifications

This file is a pointer, not a copy. The full, frozen wire contract for
`io.saola.Notifications1` lives in a separate repository. Read it here:
[`saola-notifications`'s `contrib/notifications/README.md`](https://github.com/JorDunn/saola-notifications/blob/main/contrib/notifications/README.md).
That file gives the bus name, the object path, and the full method and
property table. This file does not copy that table. A second copy
would drift from the real one over time.

This file states only the panel's own half: how
`src/modules/notifications.rs` reads those properties, how it draws
the bell, and the smoke test that proves the link works end to end.

## What the panel shows

The bell sits in the right region, in its own group. It is not inside
the status cluster, and not inside a pill. It shows one glyph, plus,
some of the time, one number:

- **Glyph**: [`Icon::Bell`], plain ivory, at rest.
- **Count**: while `NotificationCount` is above zero, the count shows
  next to the bell, in terracotta (`accent_light`). At zero, no number
  shows at all. A quiet bar shows a bell, and nothing else.
- **Do Not Disturb**: while `DndActive` is true, the glyph swaps to
  [`Icon::BellOff`]. Both the glyph and the count (if any) drop to the
  quiet `secondary` role. This is a quiet state, not an alarm state.
  It uses no red, and no new color.
- **Pressed**: while `CentreOpen` is true, the bell draws on the same
  pressed fill a tray trigger uses while its own menu is open. The
  bell then looks held down for as long as the centre stays open.

`NotificationCount` counts history entries, not unread entries — the
daemon has no read/unread state yet. So the count and its accent color
stay lit until the user dismisses items in the centre.

**`HasUrgent: b` has approval on the daemon side, but the daemon does
not yet serve it.** This module does not read `HasUrgent` in this
phase. Treat `HasUrgent` as absent, until the daemon's own README
names the release that adds it. A later stage will move the accent
onto `HasUrgent` once that release ships.

## The two clicks

- **A left click** sends `ToggleCentre()`. The panel builds no centre
  of its own. The daemon's own layer-shell surface opens and closes.
- **A right click** sends `SetDnd(!DndManual)` — the boolean opposite
  of the current `DndManual` value. The module reads `DndManual` fresh
  each time; it does not flip a value it already holds. The glyph and
  the count move only once the daemon's own `PropertiesChanged` signal
  comes back. A failed call changes nothing on screen, and logs one
  warning.

## Show nothing, then come back

When no process owns the `io.saola.Notifications1` bus name, the bell
shows nothing at all. There is no gap, and no placeholder glyph. The
module watches `org.freedesktop.DBus`'s `NameOwnerChanged` signal for
that name. The module re-attaches on its own, with no panel restart,
the moment a daemon claims the name again — the same daemon, or a
fresh one after a restart.

## Exclusive zone

The panel reserves its own strip at the top of the screen. In Wayland
layer-shell terms, this is an exclusive zone: other windows must not
draw under it. The daemon's centre and toast surfaces respect that
strip on their own, with no message passed between the two programs.
They anchor 72 px below it. The strip itself is 84 px tall in ledger
style, and 76 px tall in islands style, per this repo's own unit tests
as of 2026-09-05. If a future change moves either number, check both
repos again together.

## Smoke test (Stage 28)

With the real daemon, or a stand-in for it:

```bash
systemctl --user start saola-notifications   # or: cargo run, in that repo
cargo run
notify-send "hello"                          # count text appears beside the bell
busctl --user call io.saola.Notifications1 /io/saola/Notifications1 \
  io.saola.Notifications1 SetDnd b true      # glyph becomes bell-off, quiet
systemctl --user stop saola-notifications    # bell vanishes; no gap, no crash
systemctl --user start saola-notifications   # bell returns, with no panel restart
```

## Related

- [`src/modules/notifications.rs`](../../src/modules/notifications.rs) —
  the module's own doc comment repeats the frozen contract's exact
  fields, and gives the full design-language reasoning behind each
  rule above.
- [`saola-notifications`'s `contrib/notifications/README.md`](https://github.com/JorDunn/saola-notifications/blob/main/contrib/notifications/README.md) —
  the one canonical contract file. Ask before you treat anything here
  as an override of that file. This file only restates the panel's own
  behavior.
