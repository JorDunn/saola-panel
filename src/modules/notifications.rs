//! The notification indicator: a bell, a count, and two clicks — fed by
//! `saola-notifications`' bar-facing D-Bus interface.
//!
//! # Scope (read this before adding anything here)
//!
//! CLAUDE.md's "everything notifications is out of scope" rule is lifted for
//! **the bar indicator only** (Phase 4, 2026-09-05). The popups and the
//! notification centre stay `saola-notifications`' own layer-shell surfaces;
//! this module hosts no notification surface of its own, keeps no
//! notification *content*, and owns no `PopoverKind`. It is a readout plus
//! two remote calls, and that is the whole of it. Anything that wants to
//! *show* a notification belongs in the daemon, not here.
//!
//! # The frozen contract (the daemon's half)
//!
//! Canonical text: `saola-notifications`' own `contrib/notifications/README.md`
//! (its README's §"`io.saola.Notifications1` (frozen contract)" points there) —
//! verified against its `src/dbus.rs` on 2026-09-05, and pointed at from this
//! repo's `contrib/notifications/README.md`. The half this module actually
//! touches:
//!
//! - Session bus. Bus name **`io.saola.Notifications1`**, object path
//!   **`/io/saola/Notifications1`**, interface **`io.saola.Notifications1`** —
//!   the `1` suffix is on all three, which is unusual enough to be worth
//!   saying out loud (every other interface in this panel suffixes only the
//!   interface name).
//! - Properties, all real zbus properties with explicit change emitters, so
//!   each one arrives as a standard `PropertiesChanged`:
//!   `NotificationCount: u`, `DndActive: b`, `DndManual: b`, `CentreOpen: b`.
//! - Methods this module calls: `ToggleCentre()` and `SetDnd(b manual)`. The
//!   daemon serves four more (`OpenCentre`, `CloseCentre`, `Dismiss`,
//!   `DismissAll`) which the bar has no business calling — the centre owns
//!   dismissal.
//! - No custom signals at all.
//!
//! **`NotificationCount` is history length, not an unread count.**
//! `saola-notifications` v0.1 has no read/unread state whatsoever, so the
//! accent beside the bell means "history is non-empty", and it stays lit
//! until the user actually dismisses things in the centre. A `HasUrgent: b`
//! property is planned daemon-side for v0.2 and this module deliberately does
//! **not** read it — treating an unshipped property as absent is the only way
//! a panel built today keeps working against the daemon shipping today. A
//! follow-up stage moves the accent onto it once its tag is announced.
//!
//! # Why the owner watch exists (teaching note)
//!
//! Every other proxied module in this panel (`battery`, `network`,
//! `bluetooth`) talks to a service that is either there for the whole session
//! or not there at all, so "one connection attempt, and if it fails render
//! nothing forever" is an honest contract for them. `saola-notifications` is
//! different in one specific way: it is a *user* service that Jordan
//! restarts, and the panel routinely starts before it does. A
//! `#[zbus::proxy]` built against a bus name nobody owns doesn't fail at
//! construction — it fails on the first property read, and it keeps failing
//! forever after, because nothing ever retries.
//!
//! So the worker's outer loop is keyed on **name ownership** rather than on
//! one connection attempt. `org.freedesktop.DBus`' own `NameOwnerChanged`
//! signal is the event source: the bus emits it whenever a well-known name
//! gains or loses an owner, so "the daemon started" and "the daemon stopped"
//! are signals like any other and nothing here polls. Two details make it
//! race-free:
//!
//! 1. The signal stream is created **before** the first ownership check, so a
//!    daemon that appears in the window between the two lands in the stream's
//!    buffer rather than being missed.
//! 2. The loop re-asks `NameHasOwner` at the top of every pass instead of
//!    trusting the last signal it saw, which is what makes a *restart* (an
//!    owner change with a new owner, not a loss) re-attach rather than hang.
//!
//! # Why this module owns an `update` returning a `Task`
//!
//! Most modules hand `main.rs` a snapshot and nothing else. Two don't:
//! `modules::claude`/`modules::antigravity` own an `update` because folding
//! an animation tick is their business, and this one owns an `update` because
//! its two clicks are **remote calls**. [`Notifications::update`] answers
//! `ToggleCentre`/`ToggleDnd` with a fire-and-forget [`Task`] on the daemon's
//! proxy — the command-out pattern `modules::media` established — and
//! deliberately changes **no local state** while doing it. The readout moves
//! only when the daemon's own `PropertiesChanged` arrives as
//! [`Message::Updated`]. That is not laziness: an optimistic flip would show
//! do-not-disturb as on for the frame or two before a failed `SetDnd` call
//! came back, which is exactly the sort of small lie a status bar must never
//! tell.
//!
//! # Design language
//!
//! Bare on ink, element scale (CLAUDE.md's rule for status modules), one
//! accent at most:
//!
//! - The bell ([`Icon::Bell`]) is a plain ivory `primary` glyph at
//!   `sizes.icon_bar`, exactly like `battery`/`network`'s.
//! - `NotificationCount > 0` puts the number beside it in
//!   `palette.accent_light` — a small terracotta *accent*, the same "live
//!   state is an accent, never a flood" treatment `battery` gives charging.
//!   At zero there is no text at all, so a quiet bar is a bell and nothing
//!   else. The theme's `container::badge` (a solid terracotta pill) stays
//!   deliberately unused: it would spend the surface's entire accent budget,
//!   which the clock pill already holds.
//! - `DndActive` swaps the glyph to [`Icon::BellOff`] and drops both the
//!   glyph and the count to the `secondary` role. DND is a *quiet* state, not
//!   an alarming one — no fourth color, no red.
//! - `CentreOpen` draws the readout on the pressed fill
//!   `saola_theme::style::button::bare` gives a trigger under the pointer, so
//!   the bell looks held down for as long as the centre it opened is up. It
//!   reuses that helper rather than adding an "open" style to saola-theme:
//!   the treatment already exists, it is only being pinned to a state instead
//!   of a pointer.
//!
//! A missing daemon renders nothing and takes nothing down, same contract as
//! every other module here.

use iced::futures::channel::mpsc;
use iced::futures::stream::{self, StreamExt};
use iced::futures::{SinkExt, Stream};
use iced::widget::{container, row, text, Space};
use iced::{Element, Subscription, Task};
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};
use zbus::names::BusName;
use zbus::Connection;

use crate::icons::{self, Icon};

/// The daemon's well-known bus name. Also — unusually — the interface name
/// and the tail of the object path; see the module doc comment's contract
/// section.
const NOTIFICATIONS_BUS_NAME: &str = "io.saola.Notifications1";

/// The notification module's own message type (the per-module shape — see
/// `modules::clock::Message` for the full teaching note). `main.rs` nests
/// this as `Message::Notifications(notifications::Message)` and delegates the
/// whole thing to [`Notifications::update`].
#[derive(Debug, Clone)]
pub enum Message {
    /// A fresh reading of all four properties. The whole picture every time,
    /// never a delta — the worker re-reads the lot on any change, exactly as
    /// `battery`'s does, because zbus serves those reads out of its own
    /// property cache and "re-read everything" is therefore cheaper than
    /// carrying four separate messages.
    Updated(Snapshot),
    /// The daemon is not on the bus (never was, or just went away). Renders
    /// nothing; the module re-attaches by itself when the name gains an owner
    /// again, with no panel restart.
    Absent,
    /// Left click on the bell — `ToggleCentre()`.
    ToggleCentre,
    /// Right click on the bell — `SetDnd(!DndManual)`. Note **manual**, not
    /// active: `DndActive` is `manual || saola-capture is recording`, and the
    /// recording half has no bus setter by design, so toggling against it
    /// would produce a call that silently does nothing while a recording is
    /// up (see [`Notifications::update`]).
    ToggleDnd,
}

/// A zbus proxy for the daemon's bar-facing control interface.
///
/// Teaching note (the proxy macro): see `modules::battery`'s
/// `UPowerDeviceProxy` for the full walk-through — the short version is that
/// `#[zbus::proxy]` reads this trait and generates a `NotificationsProxy`
/// struct on which each `#[zbus(property)]` becomes an async getter plus a
/// `receive_*_changed()` change stream, and each plain method becomes an
/// async D-Bus method call. With both `default_service` and `default_path`
/// given, construction is just `NotificationsProxy::new(&connection)`.
///
/// Only the four properties and the two methods the bar actually uses are
/// declared. The daemon serves more (`OpenCentre`, `CloseCentre`, `Dismiss`,
/// `DismissAll`); a proxy trait is a *client's* view of an interface, not a
/// mirror of it, and declaring calls nothing here will ever make would invite
/// exactly the scope creep the module doc comment rules out.
#[zbus::proxy(
    interface = "io.saola.Notifications1",
    default_service = "io.saola.Notifications1",
    default_path = "/io/saola/Notifications1"
)]
trait Notifications {
    /// Opens the centre when it is closed, closes it when it is open.
    fn toggle_centre(&self) -> zbus::Result<()>;

    /// Sets *manual* do-not-disturb. Auto-DND (a saola-capture recording) is
    /// deliberately not settable over the bus.
    fn set_dnd(&self, manual: bool) -> zbus::Result<()>;

    /// History length — **not** an unread count (see the module doc comment).
    #[zbus(property)]
    fn notification_count(&self) -> zbus::Result<u32>;

    /// Effective do-not-disturb: manual OR a recording in progress. This is
    /// the one the glyph follows.
    #[zbus(property)]
    fn dnd_active(&self) -> zbus::Result<bool>;

    /// The manual-only DND flag. Read solely so the right click can send its
    /// negation — never rendered.
    #[zbus(property)]
    fn dnd_manual(&self) -> zbus::Result<bool>;

    /// Whether the daemon's notification-centre surface is mapped right now.
    #[zbus(property)]
    fn centre_open(&self) -> zbus::Result<bool>;
}

/// One reading of the daemon's four properties, as of a single
/// `PropertiesChanged` (or of the seed read taken when the worker attaches).
///
/// `Copy` because it is one integer and three flags — handing `Panel` a copy
/// is simpler than lending it a borrow across the channel, and it is what
/// lets [`Notifications`] absorb one with a plain field assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// `NotificationCount` — history length.
    count: u32,
    /// `DndActive` — manual OR recording.
    dnd_active: bool,
    /// `DndManual` — the manual half alone.
    dnd_manual: bool,
    /// `CentreOpen` — is the centre up?
    centre_open: bool,
}

/// Notification module state: the last snapshot the worker pushed, plus
/// whether the daemon is on the bus at all.
///
/// `Default` is the boot state — `present: false`, i.e. "no daemon known
/// yet" — so "the daemon isn't installed", "the daemon hasn't started yet"
/// and "the worker hasn't reported yet" all render identically: as nothing.
/// Same quiet-until-proven-otherwise contract as every other module.
///
/// `present` is a separate flag rather than an `Option<Snapshot>` because the
/// view asks two independent questions of this state ("is there a daemon?"
/// and "what does it say?"), and every field below has a meaningful value in
/// its own right the moment the first one is answered yes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Notifications {
    /// False when the daemon does not own its bus name — renders nothing.
    present: bool,
    /// History length; the accent shows while this is above zero.
    count: u32,
    /// Effective DND — swaps the glyph and quiets the whole readout.
    dnd_active: bool,
    /// Manual DND — the value the right click negates, never rendered.
    dnd_manual: bool,
    /// Centre open — draws the readout in the pressed treatment.
    centre_open: bool,
}

impl Notifications {
    /// Folds one of this module's messages into its state, and — for the two
    /// click messages — fires the matching remote call.
    ///
    /// The two call arms deliberately mutate **nothing**: they hand back a
    /// one-shot `Task` and let the daemon's own `PropertiesChanged` move the
    /// readout (see the module doc comment for why an optimistic flip would
    /// be a lie). A call that fails logs and leaves the bar showing what the
    /// daemon actually did.
    ///
    /// `ToggleDnd` sends `!self.dnd_manual`, **not** `!self.dnd_active`. The
    /// daemon computes `DndActive = DndManual || recording`, and `SetDnd`
    /// writes only the manual half — so negating the *effective* flag would,
    /// while saola-capture is recording, send `SetDnd(false)` for an already
    /// false `DndManual` and produce no visible change at all, leaving the
    /// user clicking a control that appears broken.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Updated(snapshot) => {
                self.present = true;
                self.count = snapshot.count;
                self.dnd_active = snapshot.dnd_active;
                self.dnd_manual = snapshot.dnd_manual;
                self.centre_open = snapshot.centre_open;
                Task::none()
            }
            // Back to the boot state wholesale rather than clearing `present`
            // alone: a stale count (or a stale "the centre is open") must not
            // outlive the daemon that reported it, and re-attaching always
            // seeds a fresh snapshot anyway.
            Message::Absent => {
                *self = Self::default();
                Task::none()
            }
            Message::ToggleCentre => toggle_centre(),
            Message::ToggleDnd => set_dnd(!self.dnd_manual),
        }
    }

    /// Whether this module would draw anything right now — the presence
    /// question `main.rs` asks before spending a bar group (ledger) or an
    /// island pill (islands) on it. Reads the same flag [`Self::view`]'s
    /// early return does, so the two cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// Renders the bell (plus the count, when there is one) — or nothing at
    /// all when the daemon is absent (`Space::new()` with no size is a
    /// zero-area widget; the region closes up around it).
    ///
    /// Every value here is a token: `sizes.icon_bar` for the glyph,
    /// `typography.size.bar` for the count, `sizes.bar_icon_gap` between them
    /// and as the pressed pill's horizontal inset, and the colors are the
    /// `Surface::Ink` roles plus `palette.accent_light`. The one judgement
    /// call is *which* role, and the module doc comment's design section
    /// records each one.
    ///
    /// The wrapping container's padding is applied **unconditionally** while
    /// its fill is conditional: a pill that only reserved its inset while the
    /// centre was open would make the whole right region jump sideways every
    /// time the centre opened.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if !self.present {
            return Space::new().into();
        }

        let on_ink = theme.on(Surface::Ink);

        // DND quiets the whole readout — glyph and count both drop to the
        // `secondary` role — and swaps the glyph for the struck-through bell.
        // Otherwise the glyph rests at ivory `primary` and the count (if any)
        // takes the terracotta *accent*.
        let (glyph, count_color) = if self.dnd_active {
            (Icon::BellOff, on_ink.secondary)
        } else {
            (Icon::Bell, theme.palette.accent_light)
        };
        let glyph_color = if self.dnd_active {
            on_ink.secondary
        } else {
            on_ink.primary
        };

        let mut readout = row![icons::icon(
            glyph,
            theme.sizes.icon_bar,
            glyph_color.into_iced(),
        )]
        .spacing(theme.sizes.bar_icon_gap)
        .align_y(iced::Center);

        // Nothing in history, nothing beside the bell. Zero is not a number
        // worth drawing: "0" beside a bell reads as a broken counter, while a
        // bare bell reads as calm.
        if self.count > 0 {
            readout = readout.push(
                text(self.count.to_string())
                    .size(theme.typography.size.bar)
                    .color(count_color.into_iced()),
            );
        }

        // The open-centre treatment. `style::button::bare` is the helper the
        // tray and agent triggers already wear on the bar; asking it for its
        // `Pressed` look and painting that behind the readout is what makes
        // "the centre is up" read as "this control is held down", with no new
        // style in saola-theme and no new color anywhere. `bare`'s pressed
        // fill is the surface's own `fill` role at `radii.pill` — a step up
        // from the `fill_subtle` hover, still ivory, still not an accent.
        //
        // Teaching note (button style → container style): a `button`'s style
        // fn takes a `Status` and returns `button::Style`; a `container`'s
        // takes neither and returns `container::Style`. The two structs carry
        // the same fields (background, border, text color, shadow), so
        // pinning the status and copying the three fields across is the whole
        // conversion. The panel does this rather than wrapping the readout in
        // a real `button` because the trigger has to answer a *right* click
        // too, and `button` has no right-press hook — `main.rs` puts a
        // `mouse_area` around this instead (the same widget `modules::tray`
        // uses per item, for the same reason).
        let bare = style::button::bare(theme, Surface::Ink);
        let centre_open = self.centre_open;

        container(readout)
            .padding([0.0, theme.sizes.bar_icon_gap])
            .align_y(iced::Center)
            .style(move |iced_theme: &iced::Theme| {
                if !centre_open {
                    return container::Style::default();
                }
                let pressed = bare(iced_theme, iced::widget::button::Status::Pressed);
                container::Style {
                    background: pressed.background,
                    text_color: Some(pressed.text_color),
                    border: pressed.border,
                    ..container::Style::default()
                }
            })
            .into()
    }

    /// The daemon's property feed as an iced subscription. See `battery.rs`'s
    /// `subscription` for the function-pointer-identity teaching note — it
    /// applies here verbatim, and it is what keeps a single owner-watching
    /// worker alive across every re-subscribe rather than spawning a second
    /// one that would register a second `NameOwnerChanged` match rule.
    ///
    /// Unconditional: unlike the agent modules there is no animation to gate,
    /// and unlike them this worker must keep running precisely *while* the
    /// module is absent — waiting for the daemon to appear is its whole job
    /// in that state. Nothing in it ticks.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(notifications_stream)
    }
}

/// Left click: `ToggleCentre()`.
///
/// The command-out pattern (`modules::media`'s "command-out pattern" section
/// has the reasoning, `modules::power::set_profile` the nearest twin): a
/// fresh one-shot session-bus connection, one method call, connection
/// dropped. `Task::future(..).discard()` runs it and throws the `()` away —
/// there is nothing useful `Panel::update` could do with the outcome beyond
/// the warning logged below.
///
/// Deliberately *not* routed through the subscription's long-lived
/// connection: that connection is owned by the worker task and unreachable
/// from `update`, and a click is rare enough that a fresh connection costs
/// nothing measurable.
fn toggle_centre() -> Task<Message> {
    Task::future(async {
        if let Err(error) = send_toggle_centre().await {
            warn(format_args!("ToggleCentre failed: {error}"));
        }
    })
    .discard()
}

async fn send_toggle_centre() -> zbus::Result<()> {
    // Session bus: the daemon is a per-user service (it draws surfaces in
    // Jordan's Wayland session), never a system one.
    let connection = Connection::session().await?;
    NotificationsProxy::new(&connection)
        .await?
        .toggle_centre()
        .await
}

/// Right click: `SetDnd(manual)`. Same command-out shape as
/// [`toggle_centre`]; the caller has already worked out which way to flip
/// (see [`Notifications::update`]).
///
/// Deliberately two near-identical helpers rather than one generic "call
/// something on the proxy": the connection has to outlive the call, so a
/// shared version would take an async closure receiving a borrowed proxy —
/// a higher-ranked lifetime bound that buys nothing here but noise. Two
/// four-line functions read better (CLAUDE.md: explicit over clever).
fn set_dnd(manual: bool) -> Task<Message> {
    Task::future(async move {
        if let Err(error) = send_set_dnd(manual).await {
            warn(format_args!("SetDnd({manual}) failed: {error}"));
        }
    })
    .discard()
}

async fn send_set_dnd(manual: bool) -> zbus::Result<()> {
    let connection = Connection::session().await?;
    NotificationsProxy::new(&connection)
        .await?
        .set_dnd(manual)
        .await
}

/// One warning line, in the panel's house format.
///
/// The crate carries no logging framework — every other module reports the
/// same way (`config.rs`, `config_watch.rs`, `modules::power`,
/// `modules::brightness`) — so this is `eprintln!` behind a name, kept as a
/// function purely so the prefix is written once.
fn warn(message: std::fmt::Arguments<'_>) {
    eprintln!("saola-panel: notifications: {message}");
}

/// Builds the async stream the subscription runs. See `battery.rs`'s
/// `battery_stream` for the full bridge teaching note (the channel, and the
/// single tokio runtime both iced and zbus share).
///
/// The failure funnel is the usual one — any bus error becomes "the daemon is
/// absent, the worker ends quietly, the panel carries on". The one difference
/// from `battery` is what counts as a failure: a *missing daemon* is not one
/// here, it is the normal waiting state the worker loops in (see
/// [`watch_notifications`]). Only the bus itself failing gets this far.
fn notifications_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, async |mut sender: mpsc::Sender<Message>| {
        if let Err(error) = watch_notifications(&mut sender).await {
            warn(format_args!("bus worker stopped: {error}"));
            let _ = sender.send(Message::Absent).await;
        }
    })
}

/// The worker proper: watch the daemon's bus name, and serve snapshots for as
/// long as somebody owns it.
///
/// The shape, in order (the module doc comment explains *why* each piece is
/// where it is):
///
/// 1. Register a `NameOwnerChanged` match rule filtered to this one name —
///    **first**, so nothing that happens after this line can be missed.
/// 2. Ask `NameHasOwner` (the daemon may already be up; a signal alone would
///    only tell us about a *future* change).
/// 3. If owned, attach and stream until the ownership changes; then report
///    absence and go straight back to 2 **without parking** — the event that
///    ended step 3 has already been consumed, and it may well have been a
///    restart handing the name to a new owner.
/// 4. If not owned (or if attaching failed), park on the signal stream until
///    the ownership changes, then go back to 2.
///
/// Step 2 is deliberately re-asked each pass rather than inferred from the
/// signal that woke us: a daemon restart produces one signal carrying a *new*
/// owner, and inferring "gone" from it would strand the module until the
/// panel restarted.
///
/// The park in step 4 is what keeps a *failing* attach from spinning. If the
/// name is owned but the proxy can't be built, the retry waits for the next
/// ownership change rather than looping straight back into the same failure —
/// so a broken daemon costs one warning, not a hot loop.
async fn watch_notifications(sender: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
    let connection = Connection::session().await?;
    let dbus = zbus::fdo::DBusProxy::new(&connection).await?;

    // `receive_*_with_args` asks the *bus* to filter: `(0, name)` means
    // "argument 0 equals this string", i.e. only ownership changes for this
    // one name reach us. Without it every name appearing or vanishing on the
    // session bus — dozens at login — would wake this task.
    let mut owner_changes = dbus
        .receive_name_owner_changed_with_args(&[(0, NOTIFICATIONS_BUS_NAME)])
        .await?;

    let name = BusName::try_from(NOTIFICATIONS_BUS_NAME)?;
    // Whether the *panel* currently thinks the module is present, so a run of
    // ownership churn doesn't send a stream of identical `Absent` messages.
    let mut reported_present = false;

    loop {
        // `unwrap_or(false)` rather than `?`: a failed ownership query is
        // indistinguishable, for our purposes, from "not owned" — and it must
        // not tear the worker down, because the next signal may well be the
        // daemon arriving.
        let owned = dbus.name_has_owner(name.clone()).await.unwrap_or(false);

        // Whether an ownership event has *already* been consumed this pass, in
        // which case the loop must re-evaluate immediately instead of parking
        // for the next one (see this function's doc comment, step 3).
        let mut recheck = false;

        if owned {
            reported_present = true;
            match serve(&connection, sender, &mut owner_changes).await {
                // The name changed hands. Whether that was a stop or a restart
                // is `NameHasOwner`'s to answer, not ours to guess.
                Ok(Detached::OwnerChanged) => recheck = true,
                // The receiving side is gone, or the bus connection ended:
                // either way there is nothing left to serve.
                Ok(Detached::Finished) => return Ok(()),
                // Attaching or reading failed while the name was owned. Report
                // absence and fall through to the park below, so the retry is
                // driven by the next ownership change rather than by a spin.
                Err(error) => warn(format_args!("lost the daemon: {error}")),
            }
        }

        // Only when the panel currently believes there *is* a daemon: at boot
        // with nothing on the bus the module is already in its default absent
        // state, and a redundant `Absent` would just wake the UI thread for a
        // no-op (the same dedupe posture the agent workers keep).
        if reported_present {
            if sender.send(Message::Absent).await.is_err() {
                // Receiving side gone (the subscription was dropped) — stop
                // quietly, the same contract every other worker keeps.
                return Ok(());
            }
            reported_present = false;
        }

        if recheck {
            continue;
        }

        // Park until the name's ownership changes again. `None` means the bus
        // connection itself ended, which is a session-ending event, not
        // something to retry through (same posture as `battery`/`media`).
        if owner_changes.next().await.is_none() {
            return Ok(());
        }
    }
}

/// Why [`serve`] stopped serving — the one thing its caller has to branch on.
///
/// A two-variant enum rather than a `bool` because the two outcomes mean
/// opposite things about what to do next ("look again right now" vs. "there is
/// nothing left to look at"), and a bare `true`/`false` at the call site would
/// need a comment to say which was which every time.
enum Detached {
    /// The daemon's bus name changed hands — a stop, or a restart. The caller
    /// re-asks `NameHasOwner` immediately.
    OwnerChanged,
    /// The subscription was dropped, or the bus connection itself ended. The
    /// worker is done.
    Finished,
}

/// What woke the serving loop. A tiny private enum rather than two booleans
/// so the `match` below is exhaustive and a third source added later cannot
/// be silently forgotten.
enum Event {
    /// One of the four watched properties changed — re-read and re-ship.
    Property,
    /// The bus name changed hands (the daemon stopped, or restarted).
    Owner,
}

/// Attach to the daemon and push a snapshot on every property change, until
/// the name changes hands.
///
/// Teaching note (why the owner stream is merged in rather than checked
/// between snapshots): a daemon that exits sends no `PropertiesChanged` on
/// its way out, so a loop parked only on the property streams would sit there
/// forever showing a stale bell. Merging the ownership signal into the same
/// `select` means the *absence* of the daemon is as much an event as any of
/// its properties — which is what lets this whole module be signal-driven
/// with no timeout and no retry timer anywhere.
///
/// `owner_changes` is borrowed rather than moved so the caller keeps its one
/// registered match rule across every attach/detach cycle. `&mut S` is itself
/// a `Stream` when `S: Stream + Unpin`, which is what lets the borrow go
/// straight into `stream::select`.
async fn serve(
    connection: &Connection,
    sender: &mut mpsc::Sender<Message>,
    owner_changes: &mut zbus::fdo::NameOwnerChangedStream,
) -> zbus::Result<Detached> {
    let proxy = NotificationsProxy::new(connection).await?;

    // One merged "something happened" stream, exactly the shape
    // `battery.rs`'s is: each typed change stream is mapped to a plain marker
    // (we re-read the whole snapshot below, so the *fact* of a change is all
    // that matters), which also gives them a common item type for
    // `stream::select` to merge. `select` is strictly binary, hence the
    // nesting; there is no priority implied by it.
    //
    // The property streams borrow nothing from `proxy` that stops the getters
    // below from being used (they clone the proxy's internals), and each one
    // also fires once immediately when zbus's cache is already warm — a
    // harmless redundant snapshot or two right after attaching.
    let mut events = {
        let count = proxy.receive_notification_count_changed().await;
        let dnd_active = proxy.receive_dnd_active_changed().await;
        let dnd_manual = proxy.receive_dnd_manual_changed().await;
        let centre_open = proxy.receive_centre_open_changed().await;
        stream::select(
            stream::select(
                stream::select(
                    count.map(|_| Event::Property),
                    dnd_active.map(|_| Event::Property),
                ),
                stream::select(
                    dnd_manual.map(|_| Event::Property),
                    centre_open.map(|_| Event::Property),
                ),
            ),
            owner_changes.map(|_| Event::Owner),
        )
    };

    loop {
        // The seed read on the first pass, a re-read on every later one.
        // Served from zbus's property cache once warm (the same
        // `PropertiesChanged` signals that drive the streams above keep it
        // fresh), so this is four cache hits, not four bus round-trips.
        let snapshot = Snapshot {
            count: proxy.notification_count().await?,
            dnd_active: proxy.dnd_active().await?,
            dnd_manual: proxy.dnd_manual().await?,
            centre_open: proxy.centre_open().await?,
        };

        if sender.send(Message::Updated(snapshot)).await.is_err() {
            return Ok(Detached::Finished);
        }

        match events.next().await {
            Some(Event::Property) => continue,
            // Ownership moved: hand back to the caller, which re-asks
            // `NameHasOwner` and either re-attaches (a restart) or reports
            // absence (a stop).
            Some(Event::Owner) => return Ok(Detached::OwnerChanged),
            // Every merged stream ended — the bus connection is gone.
            None => return Ok(Detached::Finished),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A present module in one specific state, so each test below reads as
    /// the scenario it pins rather than a wall of struct literals.
    fn present(count: u32, dnd_active: bool, dnd_manual: bool, centre_open: bool) -> Notifications {
        let mut module = Notifications::default();
        let _ = module.update(Message::Updated(Snapshot {
            count,
            dnd_active,
            dnd_manual,
            centre_open,
        }));
        module
    }

    #[test]
    fn a_module_with_no_daemon_is_absent() {
        let module = Notifications::default();
        assert!(!module.is_present());

        // Builds an `Element` without panicking — the same smoke-test shape
        // every view test in this crate uses, since iced cannot be asked what
        // it drew without a renderer.
        let theme = Theme::saola();
        let _ = module.view(&theme);
    }

    #[test]
    fn the_first_snapshot_makes_the_module_present() {
        let module = present(0, false, false, false);
        assert!(module.is_present());
    }

    /// The absence path must clear the readout, not just hide it: a stale
    /// count outliving the daemon that reported it would come back the moment
    /// something else set `present`.
    #[test]
    fn absence_clears_the_whole_readout() {
        let mut module = present(7, true, true, true);
        let _ = module.update(Message::Absent);

        assert_eq!(module, Notifications::default());
        assert!(!module.is_present());
    }

    /// The view-state mapping, as the design section states it. iced gives no
    /// way to inspect a built `Element`, so this pins the *decisions* the view
    /// reads (which glyph, whether a count is drawn) and then checks each
    /// state also builds without panicking.
    #[test]
    fn the_glyph_follows_effective_dnd_and_the_count_gates_on_being_non_zero() {
        let theme = Theme::saola();

        for (module, expected_glyph, expects_count) in [
            (present(0, false, false, false), Icon::Bell, false),
            (present(3, false, false, false), Icon::Bell, true),
            (present(0, true, true, false), Icon::BellOff, false),
            (present(3, true, true, false), Icon::BellOff, true),
            // Auto-DND (a saola-capture recording): active without manual.
            (present(1, true, false, false), Icon::BellOff, true),
        ] {
            assert_eq!(
                glyph_for(&module),
                expected_glyph,
                "dnd_active {} should pick {expected_glyph:?}",
                module.dnd_active
            );
            assert_eq!(module.count > 0, expects_count);
            let _ = module.view(&theme);
        }
    }

    /// The pressed treatment is a state of the same readout, not a different
    /// one — so it must render in both positions.
    #[test]
    fn an_open_centre_still_renders() {
        let theme = Theme::saola();
        let _ = present(2, false, false, true).view(&theme);
        let _ = present(2, false, false, false).view(&theme);
    }

    /// Which glyph `view` would pick, expressed exactly as `view` picks it.
    /// A test-only mirror rather than a `pub` accessor: the real decision is
    /// two lines inside `view`, and lifting it out into the module proper
    /// would be an abstraction nothing but this test wants.
    fn glyph_for(module: &Notifications) -> Icon {
        if module.dnd_active {
            Icon::BellOff
        } else {
            Icon::Bell
        }
    }

    /// The one genuinely error-prone bit of the click wiring: the right click
    /// negates `DndManual`, never `DndActive`. With saola-capture recording
    /// the two disagree, and negating the effective flag would send
    /// `SetDnd(false)` against an already-false manual flag — a click that
    /// visibly does nothing.
    #[test]
    fn toggle_dnd_negates_the_manual_flag_not_the_effective_one() {
        // Manual off, nothing recording: the click asks for DND on.
        assert!(dnd_request(present(0, false, false, false)));
        // Manual on: the click asks for DND off.
        assert!(!dnd_request(present(0, true, true, false)));
        // Recording, manual off — `dnd_active` is true but `dnd_manual` is
        // not, and this is the case the two would disagree on. The click must
        // still ask for manual DND *on*.
        assert!(dnd_request(present(0, true, false, false)));
    }

    /// What `Message::ToggleDnd` would put on the wire, computed the way
    /// `update` computes it. `update` itself returns an opaque `Task` that
    /// cannot be inspected (and would need a live bus to run), so the
    /// argument is what gets pinned here — the arm in `update` is a single
    /// `set_dnd(!self.dnd_manual)` call, and this is the `!self.dnd_manual`.
    fn dnd_request(module: Notifications) -> bool {
        !module.dnd_manual
    }

    /// The two click messages must not move the readout on their own — only
    /// the daemon's `PropertiesChanged` does (see `update`'s doc comment).
    #[test]
    fn a_click_changes_no_local_state() {
        let before = present(4, false, false, false);
        let mut module = before;

        let _ = module.update(Message::ToggleCentre);
        assert_eq!(module, before);

        let _ = module.update(Message::ToggleDnd);
        assert_eq!(module, before);
    }

    // ---------------------------------------------------------------
    // The live-bus test. `#[ignore]`d, so `cargo test` (and CI) skip it:
    //
    //     cargo test --bin saola-panel -- --ignored notifications
    //
    // It needs a **session** bus on which nothing owns
    // `io.saola.Notifications1` — it stands one up itself, from a
    // `#[zbus::interface]` mirroring the daemon's frozen contract, and takes
    // the name down again at the end. If the real daemon is already running it
    // bows out rather than fighting for the name (the daemon's claim is
    // DoNotQueue, so a second claimant would simply fail).
    //
    // What it covers that the unit tests above cannot — every construct the
    // compiler is blind to:
    //
    // - the proxy's `interface`/`default_service`/`default_path` really do
    //   name the same object the daemon serves, and each `#[zbus(property)]`
    //   method name really does map to the PascalCase property it must
    //   (`notification_count` → `NotificationCount`, …). A typo in any of
    //   those is a runtime failure, never a compile error;
    // - `receive_name_owner_changed_with_args` registers, and its `(0, name)`
    //   argument filter matches the right signal;
    // - the whole attach → serve → detach → **re-attach** cycle, which is the
    //   part of this module with real logic in it: the stand-in is claimed,
    //   dropped, and claimed again, and the bell must come back without the
    //   worker being restarted.
    // ---------------------------------------------------------------

    /// A stand-in for the daemon's control interface: the four properties and
    /// the two methods the bar touches, and nothing else.
    struct StandIn {
        count: u32,
    }

    #[zbus::interface(name = "io.saola.Notifications1")]
    impl StandIn {
        async fn toggle_centre(&self) {}

        async fn set_dnd(&self, _manual: bool) {}

        #[zbus(property)]
        fn notification_count(&self) -> u32 {
            self.count
        }

        #[zbus(property)]
        fn dnd_active(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn dnd_manual(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn centre_open(&self) -> bool {
            false
        }
    }

    /// Stands the stand-in up at the contract's name and path, or `None` if
    /// something already owns the name.
    async fn serve_stand_in(count: u32) -> Option<zbus::Connection> {
        zbus::connection::Builder::session()
            .ok()?
            .name(NOTIFICATIONS_BUS_NAME)
            .ok()?
            .serve_at("/io/saola/Notifications1", StandIn { count })
            .ok()?
            .build()
            .await
            .ok()
    }

    /// The next message the worker pushes, or a panic if it stays quiet — a
    /// timeout rather than an unbounded await so a broken worker fails the
    /// test instead of hanging it.
    async fn next_message(receiver: &mut mpsc::Receiver<Message>, what: &str) -> Message {
        tokio::time::timeout(std::time::Duration::from_secs(5), receiver.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap_or_else(|| panic!("the worker ended before {what}"))
    }

    #[test]
    #[ignore = "needs a session bus with nothing owning io.saola.Notifications1"]
    fn the_worker_attaches_detaches_and_re_attaches_around_a_real_bus_name() {
        let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
        runtime.block_on(async {
            let Some(daemon) = serve_stand_in(3).await else {
                eprintln!("skipping: io.saola.Notifications1 is already owned");
                return;
            };

            let (mut sender, mut receiver) = mpsc::channel(8);
            let worker = tokio::spawn(async move {
                let _ = watch_notifications(&mut sender).await;
            });

            // Attach: the name is already owned when the worker starts, which
            // is the `NameHasOwner` seed path (a signal alone would never fire
            // for a name claimed before the worker existed).
            match next_message(&mut receiver, "the seed snapshot").await {
                Message::Updated(snapshot) => assert_eq!(snapshot.count, 3),
                other => panic!("expected a seed snapshot, got {other:?}"),
            }

            // Detach: dropping the connection releases the name, which is the
            // only thing that tells the worker the daemon is gone (a service
            // that exits sends no `PropertiesChanged` on its way out).
            drop(daemon);
            loop {
                match next_message(&mut receiver, "absence").await {
                    Message::Absent => break,
                    // A late snapshot from before the release is fine.
                    Message::Updated(_) => continue,
                    other => panic!("expected absence, got {other:?}"),
                }
            }

            // Re-attach: a fresh owner, with no panel restart and no second
            // worker. This is the whole reason the module watches ownership.
            let daemon = serve_stand_in(9)
                .await
                .expect("the name is free again after the first stand-in dropped");
            loop {
                match next_message(&mut receiver, "the re-attached snapshot").await {
                    Message::Updated(snapshot) => {
                        assert_eq!(snapshot.count, 9);
                        break;
                    }
                    Message::Absent => continue,
                    other => panic!("expected a snapshot, got {other:?}"),
                }
            }

            drop(daemon);
            worker.abort();
        });
    }
}
