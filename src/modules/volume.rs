//! The bar's volume readout, fed by the PulseAudio client API — which on
//! Jordan's machine is served by PipeWire's `pipewire-pulse` shim, not by a
//! real PulseAudio daemon. (Decision recorded in PLAN.md Stage 10: talking
//! pulse to PipeWire is *far* less code than native pipewire-rs, whose
//! SPA-pod parsing is hostile territory. The pulse protocol is PipeWire's
//! best-supported compatibility surface; nothing here is PulseAudio-specific
//! beyond the client library.)
//!
//! # The thread bridge (this module's reason to exist)
//!
//! Battery/network/media are all **zbus bridges**: an async worker task owns
//! a proxy, awaits property-change streams, and pushes snapshots into the
//! panel through `iced::stream::channel` (see `battery.rs` for that pattern's
//! teaching notes). libpulse can't be written that way, and the reason is
//! worth understanding because every future non-D-Bus signal source (niri
//! IPC, say) has to pick one of these two shapes:
//!
//! - **libpulse owns a mainloop, and the mainloop wants to own a thread.**
//!   The C API's unit of progress is `pa_mainloop_iterate()`: a blocking call
//!   that waits on pulse's own file descriptors and dispatches whatever C
//!   callbacks became due. There is no `Future` anywhere in that picture and
//!   no way to hand pulse's fds to tokio's reactor — so the only correct home
//!   for it is a thread that does nothing else. That thread parks inside
//!   `iterate(true)` between events; it never polls, never wakes on a timer.
//! - **Its callbacks are `FnMut`s invoked from C, not futures.** A callback
//!   cannot `.await`, cannot borrow anything that isn't `'static`, and must
//!   not unwind into C. So the callbacks here own `Rc` clones of a small
//!   shared cell block ([`Shared`]) and of the channel sender — see
//!   "Callback ownership" below.
//!
//! The bridge itself is one `futures::channel::mpsc::unbounded` channel:
//!
//! ```text
//!   [ std::thread "saola-pulse" ]                    [ iced UI thread ]
//!   Mainloop::iterate(true)  ──dispatches──▶ C callback
//!                                                │
//!                              tx.unbounded_send(Message::Updated(..))
//!                                                │
//!                                    UnboundedReceiver  ──is a Stream──▶
//!                                        Subscription::run(volume_stream)
//!                                                          │
//!                                                    Panel::update
//! ```
//!
//! **Why `unbounded_send` is the right (and only) primitive in a C callback**:
//! it is synchronous, non-blocking, lock-free-ish (a mutex-guarded queue push
//! with no waiting on a consumer), and needs no executor — it can't return
//! `Pending`, so there is nothing to `.await` and nothing to park. A *bounded*
//! `Sender::send` is a future (it must wait when the buffer is full), which is
//! unusable from a callback that has no runtime under it. The cost of going
//! unbounded is that a stalled UI thread lets the queue grow; harmless here,
//! because messages are only produced when a human actually changes the
//! volume (a handful of events per keypress), never on a clock.
//!
//! # The command channel (Stage 17, the second bridge direction)
//!
//! Everything above is **read-only**: the worker feeds snapshots to the UI
//! and nothing goes the other way. The mute button and the quick-settings
//! slider (`popovers::quick_settings`) need the reverse direction too, and
//! it is genuinely a different shape from the snapshot side, not a mirror
//! of it:
//!
//! - **A different channel type.** The snapshot side is `futures::channel::
//!   mpsc::unbounded` because its *producer* is a C callback with no
//!   executor under it. The command side's producer is `Panel::update`
//!   (running on iced's executor) and its *consumer* is this same plain OS
//!   thread — so `std::sync::mpsc`, built for exactly that thread-to-thread
//!   handoff and drained with a non-blocking `try_recv`, fits better than a
//!   second futures channel would.
//! - **A channel send alone cannot wake the worker.** The thread spends
//!   nearly all its life parked inside the *blocking* `Mainloop::
//!   iterate(true)`, which only returns when pulse's own file descriptors
//!   have something to say — queuing a [`Command`] doesn't touch any of
//!   those, so it would sit unread until an unrelated pulse event happened
//!   to wake the loop, possibly never on an idle sink. [`CommandSender`]'s
//!   `wake` field is the fix: a **self-pipe** (`UnixStream::pair()`), whose
//!   read half [`run_pulse_session`] registers with the mainloop via
//!   `Mainloop::new_io_event`. Writing one meaningless byte is enough to
//!   make a blocked `iterate` return immediately; the loop then drains the
//!   pipe and the command queue before parking again. This is the "do not
//!   'fix' it with `iterate(false)` plus a sleep" the Stage 10 handoff
//!   flagged — that would be the poll CLAUDE.md forbids.
//! - **Why the sender can't just come back from `subscription()`.**
//!   `Subscription::run` takes a bare `fn() -> S`
//!   (`iced_futures-0.14.0/src/subscription.rs:182`), deliberately: its
//!   identity is a function pointer the runtime hashes, so it cannot
//!   capture anything and cannot return anything extra either. Emitting
//!   the [`CommandSender`] as the *first* value on the ordinary snapshot
//!   stream ([`Message::Ready`], sent once, before the worker even attempts
//!   to connect) is what gets a channel handle out of a bare-`fn`-shaped
//!   API without reaching for a process-wide global.
//!
//! # Design language
//!
//! Like every other right-region status module (see `battery.rs` for the full
//! note), this renders as a **bare Lucide glyph + label directly on the ink
//! bar** — no pill, no fill.
//!
//! Volume has **no live state at all**: it is a level readout, not an on/off
//! control, so terracotta never appears here in any form (CLAUDE.md's one rule
//! only governs things that have an "on", and the style guide allows at most
//! one terracotta element per surface — a volume level would be a poor use of
//! it). At rest, glyph and label are plain ivory (`on_ink.primary`).
//!
//! **Mute is a *quiet* state**, not a new color: glyph and label both fall to
//! the `on_ink.secondary` role — the same treatment `network.rs` gives a
//! disconnected station, and never a custom gray. If pulse isn't reachable at
//! all, the module renders nothing and the panel carries on, exactly like a
//! machine with no battery.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::Stream;
use iced::widget::{button, Space};
use iced::{Element, Subscription};
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::introspect::{Introspector, SinkInfo};
use libpulse_binding::context::subscribe::{Facility, InterestMaskSet};
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
use libpulse_binding::mainloop::api::Mainloop as MainloopApi;
use libpulse_binding::mainloop::events::io::FlagSet as IoEventFlagSet;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::volume::ChannelVolumes;
use saola_theme::convert::ColorExt;
use saola_theme::{style, Surface, Theme};

use crate::icons::{self, Icon};

/// The volume module's own message type (Stage 7's per-module refactor — see
/// `modules::clock::Message` for the full teaching note). `main.rs` nests
/// this as `Message::Volume(volume::Message)`; `Panel::update` delegates by
/// matching through both layers: `Message::Volume(volume::Message::Updated(v))`.
#[derive(Debug, Clone)]
pub enum Message {
    Updated(Volume),
    /// Emitted exactly once — the instant the worker thread's command
    /// channel and wake pipe exist, *before* it even attempts to connect to
    /// pulse. `Panel::update` stashes the [`CommandSender`] so later UI
    /// actions can reach the worker; see the module doc comment's "command
    /// channel" section for why this can't just be returned some other way.
    Ready(CommandSender),
    /// The bar's own mute button and the quick-settings mute toggle both
    /// resolve to this. It carries no target value on purpose:
    /// `Panel::update` reads the *current* `Volume::muted` off `Panel`
    /// itself and sends the negation, so there is exactly one place that
    /// decides what "toggle" means right now, rather than two call sites
    /// that both have to get the negation right independently.
    ToggleMute,
    /// The quick-settings slider's `on_change`, already rounded to a whole
    /// percent in the 0..=100 range the slider is built with (see
    /// `popovers::quick_settings::volume_row`).
    SetVolume(u32),
}

/// One instruction for the pulse worker thread — the payload of the second,
/// command-carrying channel this module owns. See the module doc comment's
/// "command channel" section for why it exists and how it's woken.
#[derive(Debug, Clone, Copy)]
pub enum Command {
    /// A target volume as a whole percent, *not* clamped to
    /// [`MAX_DISPLAY_PERCENT`] — that ceiling defends the display against a
    /// garbage read like `Volume::INVALID`; a command only ever originates
    /// from the slider's own 0..=100 range, so there's nothing to clamp
    /// against here.
    SetVolume(u32),
    /// An absolute mute state. [`Message::ToggleMute`] resolves to this on
    /// the UI side — see that variant's doc comment for why the negation is
    /// decided there rather than here.
    SetMute(bool),
}

/// A handle to the running worker's command channel, held by `Panel` from
/// the moment [`Message::Ready`] arrives.
#[derive(Debug, Clone)]
pub struct CommandSender {
    commands: std_mpsc::Sender<Command>,
    /// The write half of the wake self-pipe (see the module doc comment).
    /// `Arc` rather than a bare `UnixStream`: this handle rides inside
    /// `Message::Volume(..)`, and iced's widgets generically require
    /// `Message: Clone` (a button clones its stored `on_press` value each
    /// time it fires) — but `UnixStream` has no `Clone` impl at all (only a
    /// fallible `try_clone`), since sockets aren't duplicated implicitly.
    /// `Arc<UnixStream>` makes cloning this handle infallible and cheap, and
    /// stays `Send` (required: this value is manufactured on the worker
    /// thread and delivered to iced's runtime through the ordinary
    /// snapshot channel).
    wake: Arc<UnixStream>,
}

impl CommandSender {
    /// Queue a command and wake the worker if it is currently parked.
    ///
    /// Both steps are best-effort: an error from either means the worker
    /// thread is gone (process shutdown, or the self-pipe/thread never came
    /// up at boot — see `volume_stream`), which degrades to the same
    /// "control silently does nothing" contract as every other
    /// absent-capability path in this module.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
        // `&UnixStream` implements `Write` (a write doesn't need exclusive
        // access to a socket), so only the temporary reference through
        // which `write_all` is called needs to be a mutable binding — not
        // the `Arc`'s target itself.
        let mut wake = self.wake.as_ref();
        let _ = wake.write_all(&[0]);
    }
}

/// The worker-thread half of the command channel: the receiving end, plus
/// the read half of the same self-pipe [`CommandSender::wake`] writes to.
///
/// `Option<CommandChannel>` end to end (see `volume_stream`) when the
/// self-pipe couldn't be created at all — vanishingly unlikely (it needs no
/// more than two free file descriptors), but handled rather than
/// `.expect()`ed, since that construction runs on iced's subscription
/// runtime (the same "an OS resource failure must not take the panel down"
/// reasoning `thread::Builder::spawn`'s own comment below gives). On that
/// path no [`CommandSender`] is ever handed out, so controls simply become
/// silent no-ops — the same degrade-quietly contract as everywhere else.
struct CommandChannel {
    commands: std_mpsc::Receiver<Command>,
    wake: UnixStream,
}

/// The name this client registers under on the pulse server (`pactl list
/// clients` shows it). Not a design token — a protocol-level identity string.
const PULSE_CLIENT_NAME: &str = "saola-panel";

/// PulseAudio's unity-gain reference volume (`PA_VOLUME_NORM`, 0x10000).
/// Sink volumes are raw integers on a scale where this value means "100%",
/// and values *above* it mean software amplification — so a percent is just
/// `raw / PA_VOLUME_NORM`, and it is legitimately allowed to exceed 100.
const PA_VOLUME_NORM: f64 = libpulse_binding::volume::Volume::NORMAL.0 as f64;

/// Ceiling for the *displayed* percent. PulseAudio's own UI convention caps
/// volume sliders around here (`PA_VOLUME_UI_MAX` is +11 dB ≈ 153%), and
/// anything past it means we're reading garbage — `Volume::INVALID` is
/// `u32::MAX`, which would render as `6553500%` and shove every other module
/// off the bar. Clamping keeps a broken server from wrecking the layout.
const MAX_DISPLAY_PERCENT: f64 = 150.0;

/// Highest percent that still counts as "low" for icon purposes — i.e. the
/// `volume-1` (one sound wave) glyph covers 1–49%, `volume-2` (two waves)
/// covers 50% and up. A two-rung ladder's only sensible split point is its
/// midpoint. (0% gets the wave-less `volume` glyph and real mute gets
/// `volume-x` — since the 2026-08-01 icon-only readouts the two states look
/// different as well as being colored differently, see [`volume_icon`].)
const LOW_VOLUME_PERCENT_MAX: u32 = 49;

/// How long the worker waits before its first reconnection attempt, and the
/// ceiling that doubling walks up to. A pulse server that isn't running is a
/// perfectly normal state (a headless box, a session where PipeWire hasn't
/// started yet), so the worker has to be patient rather than hammer the
/// socket. Note this sleep is *not* polling in the sense CLAUDE.md forbids:
/// it only ever runs while **disconnected**, never as part of reading volume
/// state. Once connected the thread is purely event-driven.
const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Volume module state: the last snapshot the pulse worker thread pushed
/// through [`Message::Updated`].
///
/// `Default` is the boot state — `present: false`, i.e. "no pulse server
/// known yet" — so the module stays hidden until a real sink is read. That
/// makes "pulse absent", "connection lost", and "worker hasn't reported yet"
/// all render identically: as nothing. `Copy + PartialEq` matter beyond
/// convenience here: the worker keeps the last-sent value in a `Cell` to
/// suppress duplicate snapshots (pulse re-announces a sink on changes we
/// don't care about, like a stream attaching to it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Volume {
    /// Volume of the default sink as a whole percent, already clamped by
    /// [`volume_percent`]. Can exceed 100 (pulse allows amplification).
    percent: u32,
    /// The sink's mute flag — the module's *quiet* state.
    muted: bool,
    /// False when no sink has been read yet (or the server went away) —
    /// renders nothing.
    present: bool,
}

impl Volume {
    /// Whether this module would draw anything right now — the presence
    /// question `Panel::island_view` asks before spending an island pill on
    /// a module. Reads the same field as `view`'s early return, so the two
    /// cannot drift apart.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// The last-read volume as a whole percent (already rounded/clamped by
    /// [`volume_percent`]) — the quick-settings slider's current value.
    /// Only meaningful when [`Self::is_present`]; `0` is also the hidden
    /// default, the same "absent and zero read the same" convention
    /// [`Self::is_present`] documents.
    pub fn percent(&self) -> u32 {
        self.percent
    }

    /// The sink's mute flag — the mute button/toggle's current state.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Renders the leveled speaker glyph (a bare click-to-mute button — see
    /// the inline notes), or nothing at all when no sink is known. Ivory at
    /// rest, the quiet `secondary` role when muted; never terracotta — a
    /// level readout is not a control that is switched on.
    pub fn view(&self, theme: &Theme) -> Element<'_, Message> {
        if !self.present {
            return Space::new().into();
        }

        let muted = self.muted;

        // Ivory at rest, the quiet role when muted. Nothing here is ever
        // terracotta — a level readout is not a control that is switched on.
        let content_color = if muted {
            theme.on_ink.secondary
        } else {
            theme.on_ink.primary
        }
        .into_iced();

        // Real as of Stage 17: a click toggles mute. `button::bare` draws no
        // background at rest (`Status::Active` returns `pill(None, ..)` —
        // see that helper's doc comment), so the bare-icon-on-ink look is
        // unchanged at rest; only hover/press add a subtle fill step, the
        // first affordance any bar module has had (Stage 16's handoff
        // flagged its absence). `.padding(0)` matters: an iced `button`
        // defaults to non-zero padding, and any here would nudge this
        // module's footprint out of step with `network`/`battery`/
        // `claude_code`, which are bare rows with no button at all.
        //
        // This button lives *inside* the status cluster's quick-settings
        // trigger — itself a `bare`-styled button in both layouts
        // (`main.rs::Panel::status_cluster_trigger` on the ledger bar, the
        // layered pill in `island_view`'s right arm on islands). Nesting a
        // button in a button is deliberately safe: iced dispatches events
        // to children first, and a button skips its own `on_press` when
        // the content it wraps already captured the event
        // (`iced_widget-0.14.2/src/button.rs:297`,
        // `shell.is_event_captured()`). So clicking this glyph toggles
        // mute without also opening the quick-settings popover; clicking
        // anywhere else in the cluster still does.
        // Icon-only as of 2026-08-01 (Jordan: the glyph carries the level,
        // the number lives in the popover's slider row): the speaker ladder
        // — `volume-x` muted, wave-less `volume`, `volume-1`, `volume-2` —
        // is the whole readout.
        button(icons::icon(
            volume_icon(self.percent, muted),
            theme.sizes.icon_bar,
            content_color,
        ))
        .padding(0)
        .style(style::button::bare(theme, Surface::Ink))
        .on_press(Message::ToggleMute)
        .into()
    }

    /// The pulse feed as an iced subscription. See `battery.rs`'s
    /// `subscription` for the function-pointer-identity teaching note (why
    /// `Subscription::run`'s identity survives the `.map(crate::Message::
    /// Volume)` `main.rs` applies) — the same reasoning applies verbatim, and
    /// it is what guarantees the worker **thread** is spawned exactly once
    /// rather than on every re-subscribe.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(volume_stream)
    }
}

/// Builds the stream the subscription runs: an unbounded channel whose
/// sending half is handed to a dedicated OS thread running the pulse
/// mainloop.
///
/// Teaching note (why there's no `iced::stream::channel` here): every other
/// module wraps its worker in `iced::stream::channel`, which exists to give
/// an *async* worker a `Sender` and hand iced back the matching `Receiver` as
/// a stream. This worker isn't async at all — it's a thread — so the async
/// closure would have nothing to do but forward messages from one channel
/// into another. `futures::channel::mpsc::UnboundedReceiver` already *is* a
/// `Stream`, so it can be returned to `Subscription::run` directly and the
/// hop disappears. That also means this module pulls in no executor of its
/// own: the thread is the executor.
fn volume_stream() -> impl Stream<Item = Message> {
    let (sender, receiver) = mpsc::unbounded();

    // The command channel + its wake self-pipe (see the module doc
    // comment's "command channel" section). `UnixStream::pair()` failing is
    // near-impossible — it needs no more than two free file descriptors —
    // but is handled, not `.expect()`ed: this closure runs on iced's
    // subscription runtime, the exact "an OS resource failure must not take
    // the panel down" reasoning `thread::Builder::spawn`'s own comment below
    // gives. On failure the module simply never gets a working
    // `CommandSender`: mute/volume controls become silent no-ops, the same
    // degrade-quietly contract as every other absent-capability path here.
    let command_channel = UnixStream::pair().ok().and_then(|(writer, reader)| {
        // Non-blocking matters only for the *read* half: the loop's drain
        // (`drain_wake_pipe`) has to notice "nothing left to read" without
        // blocking, since it runs on the worker thread between pulse
        // events. The write half stays blocking — a 1-byte write to a
        // socketpair's buffer essentially never blocks.
        reader.set_nonblocking(true).ok()?;
        let (commands_tx, commands_rx) = std_mpsc::channel();
        Some((
            CommandSender {
                commands: commands_tx,
                wake: Arc::new(writer),
            },
            CommandChannel {
                commands: commands_rx,
                wake: reader,
            },
        ))
    });

    // Hand the sending half to the UI *before* the worker thread even
    // attempts to connect — see `Message::Ready`'s doc comment for why this
    // rides the ordinary snapshot stream rather than being returned some
    // other way.
    if let Some((sender_handle, _)) = &command_channel {
        let _ = sender.unbounded_send(Message::Ready(sender_handle.clone()));
    }
    let command_channel = command_channel.map(|(_, channel)| channel);

    // `Builder` rather than `thread::spawn` for two reasons: the name shows
    // up in `top`/gdb/perf (worth a lot when debugging a foreign mainloop),
    // and it returns a `Result` instead of panicking if the OS refuses a new
    // thread. On that (near-impossible) failure the closure — and with it
    // `sender` — is dropped, which closes the channel, so `receiver` below
    // ends immediately and the module simply stays hidden. Exactly the same
    // observable outcome as "no pulse server": nothing rendered, panel fine.
    // The `JoinHandle` is deliberately dropped rather than kept or asserted
    // on: nothing ever joins this thread (it lives as long as the panel), and
    // an `unwrap`/`debug_assert` here would run on the *UI* thread inside the
    // subscription runtime — turning an OS resource failure into a dead
    // panel, which is exactly what the "absent service renders nothing" rule
    // forbids.
    drop(
        std::thread::Builder::new()
            .name("saola-pulse".to_owned())
            .spawn(move || pulse_worker(sender, command_channel)),
    );

    receiver
}

/// State shared between the pulse C callbacks and the worker loop.
///
/// Teaching note (callback ownership — the crux of this whole module): every
/// pulse callback is a `Box<dyn FnMut(..) + 'static>` handed to C, which then
/// calls it at times Rust can't see. A `'static` closure can't borrow a local,
/// so anything a callback touches must be *owned* by it. `Rc<Shared>` is the
/// idiomatic answer on a single-threaded mainloop: each callback gets its own
/// `Rc::clone`, the loop keeps one, and the refcount (not a lifetime) keeps
/// the data alive. `Arc`/`Mutex` would be wrong here — nothing else ever
/// touches this data, and there is exactly one thread. The interior mutability
/// is `Cell`/`RefCell` for the same reason: a callback only ever gets `&self`
/// through the `Rc`, so it needs a cell to write through.
///
/// Teaching note (why the callbacks set flags instead of doing the work):
/// pulse dispatches callbacks from inside `Mainloop::iterate`, so any
/// `RefCell` a callback borrows must not already be borrowed by the loop that
/// called `iterate` — the classic "already mutably borrowed" panic. Keeping
/// the callbacks to "set a flag / stash a value" and doing all the *work*
/// (issuing introspect requests, which need the `Context`) in the loop body
/// makes that class of bug structurally impossible: the `Context` never has
/// to be shared into a callback at all, so it needs no `Rc<RefCell<..>>`
/// (unlike libpulse-binding's own doc example, which wraps everything).
struct Shared {
    /// "Re-read the default sink." Set by the subscribe callback on any
    /// SINK/SERVER event, and by the server-info callback once it knows the
    /// sink's name.
    dirty: Cell<bool>,
    /// "Re-resolve *which* sink is the default." Set on SERVER events (a
    /// default-device change is a server property change, not a sink one).
    resolve_default_sink: Cell<bool>,
    /// Name of the default sink, learned from `get_server_info`. `None`
    /// until the first reply lands.
    default_sink: RefCell<Option<String>>,
    /// Last snapshot actually sent, so identical re-reads don't wake the UI.
    last_sent: Cell<Option<Volume>>,
    /// The current default sink's channel count, learned the first time a
    /// sink is read. [`Command::SetVolume`] needs it to build a
    /// [`ChannelVolumes`] of the right width — there is no "set every
    /// channel to X regardless of count" API. Defaults to `2` (stereo): a
    /// command can only be sent once the module is `present` (the mute
    /// button/slider that produce one are only drawn then — see
    /// `popovers::quick_settings`), which means at least one real sink read
    /// has already landed by the time any `Command` exists to service, so
    /// this default is never actually load-bearing in practice; it exists
    /// only so the `Cell` has a starting value.
    channels: Cell<u8>,
}

/// How a pulse session ended — which decides what the worker does next.
enum SessionEnd {
    /// The iced side dropped the receiver (the panel is shutting down, or the
    /// subscription was torn down). Nothing left to feed: end the thread.
    ChannelClosed,
    /// No connection, or a connection that died. `reached_ready` records
    /// whether this attempt ever had a working context: if it did, the drop
    /// is a *new* failure and the backoff should start over from the bottom
    /// rather than inheriting the delay from some earlier outage.
    Lost { reached_ready: bool },
}

/// The worker thread's whole life: connect, feed snapshots until something
/// breaks, back off, try again.
///
/// This function must **never panic** — a panic here would abort the thread
/// and silently freeze the readout (and with `panic = "abort"` it would take the
/// panel down). So: no `unwrap`, no indexing, and every libpulse call that
/// documents "panics if the C function returns null" is guarded by a
/// `ContextState::Ready` check first (see [`run_pulse_session`]).
fn pulse_worker(sender: mpsc::UnboundedSender<Message>, commands: Option<CommandChannel>) {
    let mut backoff = RECONNECT_BACKOFF_START;

    loop {
        match run_pulse_session(&sender, commands.as_ref()) {
            SessionEnd::ChannelClosed => return,
            SessionEnd::Lost { reached_ready } => {
                if reached_ready {
                    // We *had* a server and lost it — treat this as a fresh
                    // outage so a restart of pipewire-pulse is picked up in
                    // a second, not in whatever the previous ceiling was.
                    backoff = RECONNECT_BACKOFF_START;
                }
            }
        }

        // Hide the module while there's no server to report on. `Volume::
        // default()` is `present: false`; sending it is also how we notice
        // the UI side has gone away (`unbounded_send` fails on a dropped
        // receiver) without waiting for a pulse event.
        if sender
            .unbounded_send(Message::Updated(Volume::default()))
            .is_err()
        {
            return;
        }

        std::thread::sleep(backoff);
        // Saturating double, capped. `Duration * u32` can't overflow at
        // these magnitudes, and `min` pins the ceiling.
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }
}

/// One connection attempt, from `pa_context_new` to whatever ends it.
///
/// The lifecycle, in order:
///
/// 1. Create the mainloop, then the context *from* it. **Declaration order
///    is load-bearing**: `Context` keeps only a raw pointer to the mainloop's
///    C API vtable — no Rust borrow, no refcount — so the mainloop has to
///    outlive it. Rust drops locals in reverse declaration order, so
///    declaring `mainloop` first means it is dropped *last*. (Dropping the
///    context unrefs it, which disconnects the socket; no explicit
///    `disconnect()` needed.)
/// 2. Turn the mainloop until the context reports `Ready` — the connection
///    handshake (socket, auth cookie, protocol version) is itself driven by
///    mainloop iterations.
/// 3. `subscribe(SINK | SERVER)` so the server *pushes* change events. This
///    is the "every module maps to a signal, never a poll" rule (CLAUDE.md)
///    for audio: nothing below asks pulse "what's the volume?" on a timer.
/// 4. Loop: service whatever the last event flagged, then park in a blocking
///    `iterate(true)` until pulse has something to say.
fn run_pulse_session(
    sender: &mpsc::UnboundedSender<Message>,
    commands: Option<&CommandChannel>,
) -> SessionEnd {
    let Some(mut mainloop) = Mainloop::new() else {
        return SessionEnd::Lost {
            reached_ready: false,
        };
    };
    let Some(mut context) = Context::new(&mainloop, PULSE_CLIENT_NAME) else {
        return SessionEnd::Lost {
            reached_ready: false,
        };
    };

    // `NOAUTOSPAWN` is deliberate and important: without it, libpulse will
    // try to *start a PulseAudio daemon* when it can't find a server. On a
    // PipeWire machine that is actively harmful (a real pulseaudio daemon
    // would fight pipewire-pulse over the socket), and on a machine with no
    // audio stack at all it's just noise. We'd rather see the connection
    // fail and retry. `NOFAIL` is likewise *not* set: we want the error, so
    // our own backoff owns the retry policy instead of libpulse blocking
    // inside `Connecting` forever.
    if context
        .connect(None, ContextFlagSet::NOAUTOSPAWN, None)
        .is_err()
    {
        return SessionEnd::Lost {
            reached_ready: false,
        };
    }

    // Handshake. `iterate(true)` blocks until pulse has work, so this is a
    // wait, not a spin. (If the socket exists but nothing on the other end
    // ever answers, this parks indefinitely — pathological, and preferable
    // to a retry loop that would hammer a wedged server.)
    loop {
        if !iterate(&mut mainloop) {
            return SessionEnd::Lost {
                reached_ready: false,
            };
        }
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                return SessionEnd::Lost {
                    reached_ready: false,
                }
            }
            // Unconnected / Connecting / Authorizing / SettingName: keep
            // turning the mainloop.
            _ => {}
        }
    }

    // Primed `true`: the first pass through the loop below does the initial
    // read, so boot needs no special case — it's just the same "something
    // changed, go look" path an event would take.
    let shared = Rc::new(Shared {
        dirty: Cell::new(true),
        resolve_default_sink: Cell::new(true),
        default_sink: RefCell::new(None),
        last_sent: Cell::new(None),
        channels: Cell::new(2),
    });

    // The event callback. Note it does *no* work beyond flag-setting — see
    // `Shared`'s doc comment for why that's structural, not laziness. It also
    // deliberately captures nothing but an `Rc<Shared>`: this closure is
    // owned by the `Context` for as long as the context lives, so capturing
    // anything that itself holds a context reference (an `Introspector`, say)
    // would build a reference cycle and leak the connection.
    {
        let shared = Rc::clone(&shared);
        context.set_subscribe_callback(Some(Box::new(move |facility, _operation, _index| {
            // A default-sink *change* arrives as a SERVER event; SINK events
            // are volume/mute changes on some sink (possibly not ours —
            // re-reading the default sink on any of them is cheaper than
            // tracking indices, and pulse only sends these when something
            // really changed).
            if facility == Some(Facility::Server) {
                shared.resolve_default_sink.set(true);
            }
            shared.dirty.set(true);
        })));
    }

    // Start the flow of those events. The completion callback (did the
    // server accept the subscription?) is uninteresting: if it failed we'd
    // simply never see events, and the readout would sit on its boot snapshot.
    //
    // Teaching note (dropping an `Operation` is fine): every async pulse call
    // returns an `Operation` handle. Its `Drop` only calls
    // `pa_operation_unref` — it does *not* cancel the request and does *not*
    // free the callback (verified in libpulse-binding's `operation.rs`: "we
    // deliberately do not destroy the `saved_cb` closure here"), because the
    // server-side pending operation holds its own reference and the callback
    // proxy frees the closure after it fires. Letting the handle drop at the
    // end of the statement is the canonical C idiom
    // (`pa_operation_unref(pa_context_subscribe(..))`) and is what we do for
    // every request in this module. Only `Operation::cancel()` unhooks a
    // callback — and we never want to.
    drop(context.subscribe(
        InterestMaskSet::SINK | InterestMaskSet::SERVER,
        |_success| {},
    ));

    // One `Introspector` for the session. It's a handle over the same C
    // context (it takes its own ref, which is why it must not be captured
    // into a callback the context itself owns — see above) and every getter
    // we use takes `&self`, so the loop can share it freely. `mut` as of
    // Stage 17: `set_sink_volume_by_name`/`set_sink_mute_by_name` take
    // `&mut self` (verified in libpulse-binding's source), unlike every
    // getter used above.
    let mut introspect = context.introspect();

    // The command channel's wake source (see the module doc comment).
    // Re-registered every session, deliberately: `mainloop` itself is fresh
    // each connection attempt, and an `IoEvent` is tied to the specific
    // mainloop instance that created it — it does not carry over to a new
    // one. The callback is a no-op on purpose: its only job is making the
    // blocking `iterate(true)` below return, and it cannot borrow
    // `commands.wake` itself anyway (`new_io_event` requires a `'static`
    // closure, and a borrow of a value living in this function's stack
    // frame isn't). Draining the pipe therefore happens unconditionally at
    // the top of the loop below instead of inside this callback. `_wake_event`
    // is bound here so its `Drop` doesn't fire until the whole session ends
    // (dropping it early would silently stop the wake mechanism).
    let _wake_event = commands.and_then(|channel| {
        mainloop.new_io_event(
            channel.wake.as_raw_fd(),
            IoEventFlagSet::INPUT,
            Box::new(|_event, _fd, _flags| {}),
        )
    });

    loop {
        // A dead connection is noticed here: when the server goes away the
        // socket closes, the mainloop's IO event fires, `iterate` returns,
        // and the state is no longer `Ready`. Checking *before* issuing any
        // request is also what keeps this thread panic-free — libpulse's
        // introspect calls assert on the null pointer the C API returns when
        // the context isn't ready.
        if context.get_state() != ContextState::Ready {
            return SessionEnd::Lost {
                reached_ready: true,
            };
        }
        if sender.is_closed() {
            return SessionEnd::ChannelClosed;
        }

        // Service whatever woke us. Draining the pipe is unconditional (not
        // just "if it looks like our fd fired") because there's no cheap
        // way to tell which fd `iterate` actually serviced from out here —
        // and trying to read a non-blocking, empty pipe is a single cheap
        // syscall that returns `WouldBlock` immediately, so there's no real
        // cost to checking every pass.
        if let Some(channel) = commands {
            drain_wake_pipe(&channel.wake);
            while let Ok(command) = channel.commands.try_recv() {
                service_command(&mut introspect, &shared, command);
            }
        }

        if shared.resolve_default_sink.replace(false) {
            // Which sink is "the" sink is a *server* property; ask for it,
            // and let the reply set `dirty` so the sink read below follows
            // on the next pass. (Two round-trips instead of one is the price
            // of not hardcoding a sink name — and it only happens at boot
            // and when the default device actually changes.)
            let shared = Rc::clone(&shared);
            drop(introspect.get_server_info(move |info| {
                *shared.default_sink.borrow_mut() =
                    info.default_sink_name.as_ref().map(|name| name.to_string());
                shared.dirty.set(true);
            }));
        }

        // Clone the name out of the `RefCell` before issuing the request:
        // holding a `Ref` across a call that can dispatch callbacks is how
        // "already borrowed" panics happen. (It can't here — pulse only
        // dispatches inside `iterate` — but the habit is what keeps that
        // true as this loop grows.)
        let default_sink = shared.default_sink.borrow().clone();
        // Note the `dirty` flag is only consumed when we can actually act on
        // it: at boot the first event arrives before any sink name is known,
        // and swallowing it there would lose the initial read.
        if let Some(name) = default_sink {
            if shared.dirty.replace(false) {
                let shared = Rc::clone(&shared);
                let sender = sender.clone();
                drop(introspect.get_sink_info_by_name(&name, move |result| {
                    // The list-style callback fires once per item and then
                    // once more with `End` (or `Error` if the name no longer
                    // resolves — a sink can disappear between the server
                    // reply and this request). Only `Item` carries data.
                    //
                    // `Error` is deliberately *not* self-healed by
                    // re-resolving the default sink here: server-info →
                    // sink-info → Error → server-info would be an unbounded
                    // request loop if the server kept naming a sink that
                    // doesn't resolve. The next real SINK/SERVER event fixes
                    // it instead; worst case the readout shows a stale reading
                    // until then.
                    let ListResult::Item(info) = result else {
                        return;
                    };

                    let snapshot = snapshot_from_sink(info);
                    // Refreshed on every real read, unconditionally (unlike
                    // `last_sent` below, which dedupes): a channel-count
                    // change with no accompanying volume/mute change is
                    // exotic, but cheap to just always stay current on.
                    shared.channels.set(info.volume.len());
                    // Suppress no-op updates: pulse re-announces a sink for
                    // changes that don't touch volume or mute (a stream
                    // attaching, a port change), and every message iced
                    // receives costs a re-render.
                    if shared.last_sent.get() == Some(snapshot) {
                        return;
                    }
                    shared.last_sent.set(Some(snapshot));
                    // The bridge, in one line: synchronous, non-blocking,
                    // no executor — see the module doc comment. A failure
                    // means the UI side is gone; the loop notices via
                    // `sender.is_closed()` above on its next pass.
                    let _ = sender.unbounded_send(Message::Updated(snapshot));
                }));
            }
        }

        // Park until pulse has something to dispatch. This is the whole
        // "signal, never a poll" story for audio: an idle machine leaves
        // this thread asleep in `poll(2)` indefinitely.
        //
        // Known limitation (documented, not fixed): if the iced side drops
        // the receiver while we're parked here, the thread stays asleep
        // until the *next* pulse event lets the `is_closed()` check above
        // run. Since the panel's volume subscription lives as long as the
        // process, that only ever costs one parked thread at shutdown. The
        // self-pipe above (Stage 17) *could* be reused to wake this path
        // too, but nothing currently writes to it on shutdown — it's woken
        // only by `CommandSender::send`.
        if !iterate(&mut mainloop) {
            return SessionEnd::Lost {
                reached_ready: true,
            };
        }
    }
}

/// One blocking turn of the pulse mainloop: waits for pulse's fds, then
/// dispatches whatever callbacks came due. Returns `false` when the mainloop
/// is finished (quit or errored) and the session is over.
fn iterate(mainloop: &mut Mainloop) -> bool {
    matches!(mainloop.iterate(true), IterateResult::Success(_))
}

/// Empties the wake pipe's read end so a level-triggered io-event doesn't
/// immediately refire on the next `iterate`. The read half is non-blocking
/// (`volume_stream` sets this up), so this loop terminates as soon as
/// there's nothing left rather than parking on a `read`.
fn drain_wake_pipe(reader: &UnixStream) {
    let mut buf = [0u8; 64];
    loop {
        match (&*reader).read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
}

/// Turns one queued [`Command`] into the matching pulse introspection call.
/// A no-op if the default sink isn't known yet — see [`Shared::channels`]'s
/// doc comment for why that's not a real-world path in practice. Both pulse
/// calls are fire-and-forget (`None` callback): the worker has no way to
/// report success/failure back to `Panel::update` anyway (the snapshot
/// channel only carries `Volume` readouts), and the very next real
/// `Facility::Sink` event will show the true resulting state regardless of
/// whether the request actually took effect.
fn service_command(introspect: &mut Introspector, shared: &Shared, command: Command) {
    let Some(name) = shared.default_sink.borrow().clone() else {
        return;
    };
    match command {
        Command::SetVolume(percent) => {
            let mut volumes = ChannelVolumes::default();
            volumes.set(
                shared.channels.get(),
                libpulse_binding::volume::Volume(percent_to_raw(percent)),
            );
            drop(introspect.set_sink_volume_by_name(&name, &volumes, None));
        }
        Command::SetMute(muted) => {
            drop(introspect.set_sink_mute_by_name(&name, muted, None));
        }
    }
}

/// Reads the two things the module shows out of a sink's introspection record.
///
/// `ChannelVolumes::avg()` collapses per-channel volumes into one number —
/// the right call for a single percent readout; a left/right imbalance simply
/// averages out (pavucontrol is where someone goes to see per-channel values,
/// not a status bar).
fn snapshot_from_sink(info: &SinkInfo<'_>) -> Volume {
    Volume {
        percent: volume_percent(info.volume.avg().0),
        muted: info.mute,
        present: true,
    }
}

/// Raw pulse volume → whole percent, clamped to a displayable range.
///
/// Pure function of its argument — which is what makes it unit-testable
/// without a pulse server. (The mainloop plumbing above is not testable at
/// all here; it needs a real server.)
fn volume_percent(raw: u32) -> u32 {
    // The lower clamp can't trigger for a `u32` input; it's there so the
    // range is stated in one place and stays correct if this ever takes a
    // signed or floating-point value.
    ((raw as f64 / PA_VOLUME_NORM) * 100.0)
        .round()
        .clamp(0.0, MAX_DISPLAY_PERCENT) as u32
}

/// Inverse of [`volume_percent`]: a UI percent back to pulse's raw integer
/// scale, for [`Command::SetVolume`]. Unlike `volume_percent`, this is never
/// clamped — its only caller is the quick-settings slider, whose own range
/// already stops at 100 (see `popovers::quick_settings::volume_row`), so
/// there's no `Volume::INVALID`-style garbage input to defend against here.
fn percent_to_raw(percent: u32) -> u32 {
    ((percent as f64 / 100.0) * PA_VOLUME_NORM).round() as u32
}

/// Percent + mute → which Lucide glyph the row shows. See
/// [`LOW_VOLUME_PERCENT_MAX`] for the threshold rationale.
///
/// `pub(crate)`: `popovers::quick_settings::volume_row` reuses this exact
/// mapping for the slider section's own glyph, rather than a second
/// hand-copied version that could drift from the bar's.
pub(crate) fn volume_icon(percent: u32, muted: bool) -> Icon {
    if muted {
        Icon::VolumeX
    } else if percent == 0 {
        // Not muted, just turned all the way down: the wave-less speaker
        // (added with the 2026-08-01 icon-only readouts) rather than the
        // mute cross — the states are different and now look different.
        Icon::Volume
    } else if percent <= LOW_VOLUME_PERCENT_MAX {
        Icon::Volume1
    } else {
        Icon::Volume2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PA_VOLUME_NORM` as an integer, for building test inputs.
    const NORM: u32 = libpulse_binding::volume::Volume::NORMAL.0;

    #[test]
    fn percent_maps_the_pulse_scale_to_100() {
        assert_eq!(volume_percent(0), 0);
        assert_eq!(volume_percent(NORM), 100);
        assert_eq!(volume_percent(NORM / 2), 50);
        assert_eq!(volume_percent(NORM / 4), 25);
    }

    #[test]
    fn percent_rounds_to_the_nearest_whole() {
        // 33.4% and 33.6% of the scale, which must not both land on 33.
        assert_eq!(volume_percent((NORM as f64 * 0.334) as u32), 33);
        assert_eq!(volume_percent((NORM as f64 * 0.336) as u32), 34);
    }

    #[test]
    fn percent_allows_amplification_but_clamps_garbage() {
        // Above 100% is legitimate — pulse permits software amplification.
        assert_eq!(volume_percent(NORM * 3 / 2), 150);
        assert_eq!(volume_percent((NORM as f64 * 1.25) as u32), 125);
        // `Volume::INVALID` is `u32::MAX`; unclamped that renders as
        // `6553500%` and destroys the bar's layout.
        assert_eq!(volume_percent(u32::MAX), MAX_DISPLAY_PERCENT as u32);
    }

    #[test]
    fn muted_always_shows_the_x_glyph() {
        assert_eq!(volume_icon(0, true), Icon::VolumeX);
        assert_eq!(volume_icon(30, true), Icon::VolumeX);
        assert_eq!(volume_icon(100, true), Icon::VolumeX);
    }

    #[test]
    fn unmuted_silence_shows_the_wave_less_speaker_not_the_mute_cross() {
        assert_eq!(volume_icon(0, false), Icon::Volume);
    }

    #[test]
    fn icon_steps_from_low_to_high_at_the_midpoint() {
        assert_eq!(volume_icon(1, false), Icon::Volume1);
        assert_eq!(volume_icon(LOW_VOLUME_PERCENT_MAX, false), Icon::Volume1);
        assert_eq!(
            volume_icon(LOW_VOLUME_PERCENT_MAX + 1, false),
            Icon::Volume2
        );
        assert_eq!(volume_icon(100, false), Icon::Volume2);
        assert_eq!(volume_icon(150, false), Icon::Volume2);
    }

    #[test]
    fn percent_to_raw_is_the_inverse_of_volume_percent() {
        assert_eq!(percent_to_raw(100), NORM);
        assert_eq!(percent_to_raw(50), NORM / 2);
        // Round-trip: a slider value that goes out to pulse and comes back
        // through a real sink read should land on the same whole percent.
        assert_eq!(volume_percent(percent_to_raw(42)), 42);
        assert_eq!(volume_percent(percent_to_raw(0)), 0);
    }

    #[test]
    fn getters_read_the_snapshot_fields_they_expose() {
        let volume = Volume {
            percent: 42,
            muted: true,
            present: true,
        };
        assert_eq!(volume.percent(), 42);
        assert!(volume.muted());
    }

    #[test]
    fn default_state_is_hidden() {
        // "No pulse yet", "no pulse at all", and "connection lost" must all
        // be the same, render-nothing state.
        let hidden = Volume::default();
        assert!(!hidden.present);
        assert_eq!(hidden.percent, 0);
        assert!(!hidden.muted);
    }
}
