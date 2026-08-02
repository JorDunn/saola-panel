//! Screen brightness: read from the kernel's `/sys/class/backlight`, written
//! through systemd-logind, woken by udev.
//!
//! The panel's **second popover-only module** (after [`super::power`], which
//! established the shape — read that module first). Like it, this has no
//! `view` at all, no `config::ModuleName`, and no slot in any bar region: a
//! brightness *level* is a thing you reach for occasionally in quick
//! settings, not a number worth a permanent seat on the bar. Everything else
//! about it is a normal module — its own state struct, its own `Message`, its
//! own `subscription`, the same "absent hardware renders nothing" contract,
//! and one snapshot stored on `Panel`.
//!
//! Where it differs from `power.rs` is that **no single technology does the
//! whole job**. Brightness needs three, and each was picked for a reason:
//!
//! | job | mechanism | why not something else |
//! |-----|-----------|------------------------|
//! | read | `/sys/class/backlight/<dev>/{brightness,max_brightness}` | logind exposes no brightness *property* to read — the value only lives in sysfs |
//! | write | logind's `SetBrightness(ssu)` | writing sysfs directly needs root (or a udev rule granting the `video` group); logind writes it on behalf of the *active session*, no polkit prompt |
//! | change signal | a udev `change` uevent on the device | see "Why udev" below |
//!
//! # Why udev, and why that forces a thread bridge
//!
//! Every other module in this panel watches a D-Bus signal, and the first
//! instinct here is to look for one. There isn't any: logind has a
//! `SetBrightness` *method* but no brightness property, so nothing ever emits
//! a `PropertiesChanged`. The obvious fallback — inotify on the sysfs
//! `brightness` file — doesn't work either: sysfs attributes are synthesised
//! on read, so they never generate the `IN_MODIFY` events inotify is built to
//! report.
//!
//! What the kernel *does* emit is a **udev "change" uevent** on the backlight
//! device every time its brightness moves — whoever moved it, a function key,
//! logind, or another panel. That is the signal, and it is the same one
//! waybar's backlight module uses. It arrives on a netlink socket that
//! libudev owns.
//!
//! That socket is why this is the panel's second **thread bridge** rather
//! than a zbus-shaped worker. Read `volume.rs`'s module doc comment for the
//! pattern in full (it is the precedent this copies); the short version is
//! that libudev's monitor socket is a foreign, non-blocking fd with no
//! `Future` anywhere near it and no way to hand it to tokio's reactor, so it
//! gets a dedicated OS thread that parks in `poll(2)` and pushes snapshots
//! out through a `futures::channel::mpsc::unbounded` channel whose receiver
//! *is* the subscription's stream:
//!
//! ```text
//!   [ std::thread "saola-backlight" ]                [ iced UI thread ]
//!   poll(2) on the udev monitor fd
//!            │ wakes
//!   drain the socket, re-read sysfs
//!            │
//!   tx.unbounded_send(Message::Updated(..))
//!            │
//!     UnboundedReceiver  ──is a Stream──▶ Subscription::run(brightness_stream)
//!                                                    │
//!                                              Panel::update
//! ```
//!
//! The thread is asleep in `poll` the entire time nobody is touching the
//! brightness keys — this is a signal, not a poll, in exactly the sense
//! CLAUDE.md means. (`poll(2)` the syscall is a *blocking wait* on an fd; the
//! "poll" CLAUDE.md forbids is asking a source for its state on a timer.
//! `volume.rs`'s pulse mainloop parks in the very same syscall one layer
//! down.)
//!
//! # Absent backlight
//!
//! Same "quiet until proven otherwise" contract as every other module: a
//! `Brightness::default()` has `present: false`, so a desktop with no
//! backlight at all (nothing under `/sys/class/backlight` — the normal case
//! for a tower with an external monitor) leaves the quick-settings row hidden
//! and never takes the panel down. Every failure path here collapses to that
//! same state, including "libudev wouldn't open a monitor" and "logind
//! refused the write".

use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;

use iced::futures::channel::mpsc;
use iced::futures::Stream;
use iced::{Subscription, Task};
use zbus::Connection;

/// The brightness module's own message type (Stage 7's per-module refactor —
/// see `modules::clock::Message` for the full teaching note on why every
/// module owns its `Message`). `main.rs` nests this as
/// `Message::Brightness(brightness::Message)`.
#[derive(Debug, Clone)]
pub enum Message {
    /// A fresh snapshot from the udev worker thread.
    Updated(Brightness),
    /// The quick-settings brightness slider's `on_change`, already rounded to
    /// a whole percent in the slider's own 0..=100 range. Resolves to the
    /// one-shot command-out `Task` [`set_brightness`] builds — see that
    /// function, and `modules::media`'s "command-out pattern" section for the
    /// reasoning this copies.
    ///
    /// The payload is a *percent*, not a raw kernel value, on purpose: the
    /// popover has no business knowing this panel's backlight tops out at
    /// 62451 rather than 255 or 100. `Panel::update` pairs the percent with
    /// the device name and `raw_max` it already has on `Panel` (see
    /// [`Brightness::device`] / [`Brightness::raw_max`]) and this module does
    /// the conversion in [`percent_to_raw`].
    SetBrightness(u8),
}

/// Where the kernel exposes every backlight device it knows about. One
/// directory per device (`amdgpu_bl1`, `intel_backlight`, `acpi_video0`, …),
/// each with the two attributes [`read_snapshot`] reads.
const BACKLIGHT_ROOT: &str = "/sys/class/backlight";

/// The udev subsystem name — used twice, and it must be the same string both
/// times: once to filter the monitor down to backlight events, and once as
/// the first argument of logind's `SetBrightness` (which takes a
/// subsystem/device pair because it also drives keyboard backlights, under
/// the separate `leds` subsystem).
const BACKLIGHT_SUBSYSTEM: &str = "backlight";

/// The device's current level, as a plain integer on a driver-defined scale.
const BRIGHTNESS_ATTRIBUTE: &str = "brightness";

/// The top of that scale. Wildly driver-dependent — 62451 on this machine's
/// `amdgpu_bl1`, 255 or 100 elsewhere — which is exactly why nothing outside
/// this module ever sees a raw value.
const MAX_BRIGHTNESS_ATTRIBUTE: &str = "max_brightness";

/// A zbus proxy for the calling process's own logind session.
///
/// See `battery.rs`'s `UPowerDevice` proxy for the full `#[zbus::proxy]`
/// teaching note (what the macro generates, why the trait itself is never
/// implemented, how snake_case names become PascalCase on the wire).
///
/// Teaching note (the magic `auto` path): `/org/freedesktop/login1/session/
/// auto` is not a real object — logind resolves it, per-caller, to whichever
/// session the calling process belongs to (falling back to the user's
/// display session). That is what lets this proxy be a compile-time constant
/// instead of something that first has to ask logind "which session am I?"
/// and build a path from the answer. It is also why the write needs no polkit
/// agent: logind only honours `SetBrightness` for a session that is currently
/// *active* on its seat, and it already knows this one is.
#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1/session/auto"
)]
trait Session {
    /// Set an absolute raw level on one device. `(ssu)`: the udev subsystem
    /// (`"backlight"` here), the device's udev name (`"amdgpu_bl1"`), and the
    /// value — which is on the *driver's* scale, so it must be within
    /// `0..=max_brightness` (see [`percent_to_raw`], which is what guarantees
    /// that).
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

/// Brightness module state: the last snapshot the udev worker thread pushed
/// through [`Message::Updated`]. Cached for the same reason as every other
/// module's — reading sysfs during `view` would be filesystem I/O on the UI
/// thread.
///
/// `Default` is the boot state (`present: false`), i.e. "no backlight known
/// yet", so "this machine has no backlight", "libudev wouldn't start", and
/// "the worker hasn't reported yet" all render identically: as nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Brightness {
    /// The current level as a whole percent of [`Self::raw_max`], already
    /// rounded and clamped by [`raw_to_percent`].
    percent: u8,
    /// The device's udev name, e.g. `"amdgpu_bl1"` — the second argument of
    /// `SetBrightness`. Empty until the worker reports.
    device: String,
    /// The top of this driver's scale (`max_brightness`). Kept so
    /// [`percent_to_raw`] can convert a slider percent back without
    /// re-reading sysfs on the UI thread.
    raw_max: u32,
    /// False when this machine has no usable backlight (or none is known
    /// yet) — the quick-settings row draws nothing.
    present: bool,
}

impl Brightness {
    /// Whether a backlight has been found at all — the presence question the
    /// quick-settings popover asks before spending a row on this module,
    /// exactly like `Power::is_present` guards its profile chips.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// The current level as a whole percent — the quick-settings slider's
    /// value. Only meaningful when [`Self::is_present`]; `0` is also the
    /// hidden default, the same "absent and zero read the same" convention
    /// `Volume::percent` documents.
    pub fn percent(&self) -> u8 {
        self.percent
    }

    /// The backlight's udev name, for `Panel::update` to hand to
    /// [`set_brightness`]. Empty before the worker's first report (which is
    /// also when `is_present` is false, so a caller that checks presence
    /// first never sees the empty string).
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The top of this driver's scale, likewise for `Panel::update` to hand
    /// to [`set_brightness`]. Kept out of the message payload deliberately —
    /// see [`Message::SetBrightness`].
    pub fn raw_max(&self) -> u32 {
        self.raw_max
    }

    /// The udev feed as an iced subscription. See `battery.rs`'s
    /// `subscription` for the function-pointer-identity teaching note (why
    /// `Subscription::run`'s identity survives the `.map(crate::Message::
    /// Brightness)` `main.rs` applies) — the same reasoning applies verbatim,
    /// and it is what guarantees the worker **thread** is spawned exactly
    /// once rather than on every re-subscribe.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(brightness_stream)
    }
}

/// Builds the stream the subscription runs: an unbounded channel whose
/// sending half is handed to a dedicated OS thread parked on libudev's
/// monitor socket.
///
/// Structurally identical to `volume.rs`'s `volume_stream`, including the
/// reason there is no `iced::stream::channel` wrapper here (this worker isn't
/// async at all, and an `UnboundedReceiver` already *is* a `Stream`) and the
/// reason the `JoinHandle` is dropped rather than kept (nothing ever joins
/// this thread, and an `unwrap` here would run on the UI thread and turn an
/// OS resource failure into a dead panel). Read that function's teaching
/// notes; they apply here word for word.
fn brightness_stream() -> impl Stream<Item = Message> {
    let (sender, receiver) = mpsc::unbounded();

    drop(
        std::thread::Builder::new()
            .name("saola-backlight".to_owned())
            .spawn(move || brightness_worker(sender)),
    );

    receiver
}

/// The worker thread's whole life: find the backlight, report it once, then
/// re-read and report on every udev event, forever.
///
/// Unlike `volume.rs`'s pulse worker there is **no reconnect loop**, and that
/// is deliberate rather than an omission: a backlight is a fixed piece of
/// hardware, not a daemon that can restart. If it isn't in `/sys/class/
/// backlight` at boot it isn't going to appear later, and if libudev refuses
/// to open a monitor socket the process has bigger problems than a slider.
/// Both cases end the thread quietly with the module hidden.
///
/// Like every worker thread in this crate this function must **never panic**
/// — a panic here would be invisible except as a permanently frozen readout
/// (and with `panic = "abort"` it would take the panel down). So: no
/// `unwrap`, no indexing, every fallible call handled.
fn brightness_worker(sender: mpsc::UnboundedSender<Message>) {
    // No backlight on this machine: say so once (so the module is definitively
    // hidden rather than merely un-reported) and end the thread.
    let Some(device) = first_backlight_device() else {
        let _ = sender.unbounded_send(Message::Updated(Brightness::default()));
        return;
    };

    // The initial snapshot, read *before* the event loop starts. Every other
    // module gets its first value the same way — a property read at connect
    // time — and without it the slider would sit at zero until the human
    // happened to change the brightness some other way.
    let mut last_sent = read_snapshot(&device).unwrap_or_default();
    if sender
        .unbounded_send(Message::Updated(last_sent.clone()))
        .is_err()
    {
        return;
    }

    // The signal. `match_subsystem` installs a kernel-side filter on the
    // netlink socket, so this thread is never woken by the hundreds of
    // unrelated uevents a running system produces (USB, block devices, DRM
    // hotplug) — only by the backlight ones. Each `?`-shaped step is fallible
    // and all three collapse to the same "no monitor, no updates" outcome:
    // the module keeps showing the snapshot above and the thread ends.
    let Ok(monitor) = udev::MonitorBuilder::new()
        .and_then(|builder| builder.match_subsystem(BACKLIGHT_SUBSYSTEM))
        .and_then(|builder| builder.listen())
    else {
        return;
    };

    loop {
        // Park until the kernel has something to say. The whole "signal,
        // never a poll" story for brightness: an idle machine leaves this
        // thread asleep in `poll(2)` indefinitely.
        if !wait_readable(monitor.as_raw_fd()) {
            return;
        }

        // Drain the socket completely. This matters and is easy to get
        // wrong: `poll` is *level*-triggered, so an event left unread would
        // make the next `wait_readable` return instantly and spin this loop
        // at 100% CPU. `MonitorSocketIter` reads the non-blocking socket and
        // yields `None` the moment it would block, so `for` over it drains
        // exactly what's queued and stops.
        //
        // The events themselves are deliberately ignored rather than
        // inspected. A `change` on the backlight subsystem is the only kind
        // this filter lets through in practice, and even a machine with two
        // backlights would only cost one redundant sysfs read — cheaper than
        // matching sysnames, and deduplicated below anyway.
        let mut woken = false;
        for _event in monitor.iter() {
            woken = true;
        }
        if !woken {
            continue;
        }

        // Re-read both attributes. `max_brightness` is re-read alongside
        // `brightness` even though it is effectively constant: it costs one
        // more tiny sysfs read on an event a human caused, and it means a
        // driver that ever does renegotiate its scale can't leave the slider
        // silently mis-scaled.
        let Some(snapshot) = read_snapshot(&device) else {
            continue;
        };

        // Suppress no-op updates, same reasoning as `volume.rs`: a `change`
        // uevent can fire for a write that landed on the value already in
        // effect (the quick-settings slider dragging within one percent step
        // does exactly this), and every message iced receives costs a
        // re-render.
        if snapshot == last_sent {
            continue;
        }
        last_sent = snapshot.clone();

        // The bridge, in one line — synchronous, non-blocking, no executor
        // needed (see `volume.rs`'s module doc comment for why
        // `unbounded_send` is the right primitive off an executor-less
        // thread). A failure means the UI side is gone; stop quietly.
        if sender.unbounded_send(Message::Updated(snapshot)).is_err() {
            return;
        }
    }
}

/// Blocks until `fd` is readable. Returns `false` when the fd will never be
/// readable again (an error or a hangup), which ends the worker thread.
///
/// Teaching note (why a raw `libc::poll`): libudev's monitor socket is
/// deliberately non-blocking — its own documentation says to wait on the fd
/// with `poll()` and then read — and the `udev` crate exposes the fd
/// (`AsRawFd`) without wrapping the wait. There is no safe Rust wrapper in
/// this crate's dependency graph for a bare "block on one fd", and `libc` is
/// already in the tree (udev, tokio and zbus all depend on it), so this is a
/// direct dependency for the *name* rather than for new code in the binary —
/// the same reasoning the `serde` line in `Cargo.toml` gives for its derive.
///
/// The `unsafe` block is three lines and is the only one in the crate: one
/// fully-initialised `pollfd`, a count that matches, and an infinite timeout.
fn wait_readable(fd: RawFd) -> bool {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        // SAFETY: `descriptor` is a live, fully-initialised `pollfd` and the
        // count `1` matches the single element behind the pointer. `-1` is
        // poll's documented "wait forever". `poll` writes only to
        // `revents`, which we own exclusively here.
        let ready = unsafe { libc::poll(&mut descriptor, 1, -1) };

        if ready < 0 {
            // EINTR is not a failure — a signal (a profiler attaching, a
            // SIGWINCH) interrupted the wait and the correct response is to
            // wait again. Any other error means the fd is unusable.
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }

        // `POLLERR`/`POLLHUP`/`POLLNVAL` are output-only flags: poll reports
        // them whether or not they were requested, and each means this fd is
        // finished. Checking them is what stops a closed socket from turning
        // this loop into a spin — poll would otherwise return "ready"
        // forever while the read below yields nothing.
        let broken = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
        if descriptor.revents & broken != 0 {
            return false;
        }

        return ready > 0;
    }
}

/// The backlight this module drives: the first entry under
/// [`BACKLIGHT_ROOT`], or `None` on a machine with none.
///
/// **Never a hardcoded name.** It is `amdgpu_bl1` on Jordan's machine,
/// `intel_backlight` on an Intel laptop, `acpi_video0` on older firmware —
/// the directory is the only source of truth.
///
/// "First" is defined as *first in sorted order*, not first in `read_dir`
/// order: the latter is filesystem-defined (roughly inode order) and could
/// legitimately differ between two boots of the same machine, which would
/// make a two-backlight laptop pick a different device each time. Sorting
/// costs nothing on a directory with one entry and makes the choice stable.
/// (A machine with two real backlights and a preference between them would
/// want a `panel.kdl` knob; nothing has asked for one, so this doesn't
/// speculatively build it.)
fn first_backlight_device() -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(BACKLIGHT_ROOT)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.file_name().to_str()?.to_owned()))
        .collect();
    names.sort();
    names.into_iter().next()
}

/// Reads one integer attribute out of a backlight's sysfs directory.
/// `None` for anything unexpected — missing file, no read permission, a body
/// that isn't a number — so a driver that doesn't behave leaves the module
/// hidden instead of showing a wrong value.
fn read_attribute(device: &str, attribute: &str) -> Option<u32> {
    let path = Path::new(BACKLIGHT_ROOT).join(device).join(attribute);
    // `trim` is required, not defensive: sysfs attributes are newline-
    // terminated, and `"31230\n".parse::<u32>()` fails.
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Both attributes plus the derived percent, as one snapshot.
///
/// A `max_brightness` of `0` yields `None` rather than a present-but-useless
/// module: there is no level to show and no value a slider could send, so
/// hiding is the honest rendering. (It also means [`raw_to_percent`]'s
/// divide-by-zero guard is belt-and-braces rather than the only defence.)
fn read_snapshot(device: &str) -> Option<Brightness> {
    let raw = read_attribute(device, BRIGHTNESS_ATTRIBUTE)?;
    let raw_max = read_attribute(device, MAX_BRIGHTNESS_ATTRIBUTE)?;
    if raw_max == 0 {
        return None;
    }

    Some(Brightness {
        percent: raw_to_percent(raw, raw_max),
        device: device.to_owned(),
        raw_max,
        present: true,
    })
}

/// Raw driver value → whole percent, rounded and clamped to `0..=100`.
///
/// Pure function of its two arguments, which is what makes it unit-testable
/// with no backlight present (the worker plumbing above needs real hardware
/// and is not testable here at all) — the same split `power.rs`'s
/// `profile_names` makes for the same reason.
///
/// `max == 0` returns `0` rather than dividing: [`read_snapshot`] already
/// rejects that case, but this function is the one with the division in it,
/// so it is the one that has to be safe on its own terms.
fn raw_to_percent(raw: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }

    // The clamp defends against a driver reporting `brightness > max_
    // brightness` (rare but not unheard of on buggy ACPI firmware), which
    // would otherwise produce a percent above 100 and a slider outside its
    // own range.
    ((raw as f64 / max as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u8
}

/// Inverse of [`raw_to_percent`]: a UI percent back to the driver's scale,
/// for [`set_brightness`].
///
/// Clamped at both ends, unlike `volume.rs`'s `percent_to_raw` (whose only
/// caller is a slider that already stops at 100). Here the ceiling is
/// load-bearing rather than cosmetic: the value goes to logind, which writes
/// it into the kernel, and an out-of-range write is rejected — so rounding
/// 100% up past `max` would make the slider's top end silently do nothing.
fn percent_to_raw(percent: u8, max: u32) -> u32 {
    let percent = f64::from(percent.min(100));
    let raw = ((percent / 100.0) * f64::from(max)).round();
    // `min` after the cast rather than `clamp` on the float: the arithmetic
    // above can't exceed `max` mathematically, but floating-point rounding at
    // 100% of a large `max` is exactly the kind of place a one-off would hide.
    (raw as u32).min(max)
}

/// Asks logind to set `device`'s brightness to `percent` of its scale.
///
/// The command-out pattern, copied from `modules::media` (read that module's
/// "command-out pattern" section for the reasoning): a **fresh, one-shot
/// system-bus connection**, one method call, connection dropped.
/// `Task::future(..).discard()` runs it to completion and throws away the
/// `()` result — `Panel::update` has nothing useful to do with success or
/// failure beyond the `eprintln!` below.
///
/// The UI is deliberately *not* updated optimistically (same call as
/// `power.rs`'s `set_profile`): the udev `change` event this write provokes
/// is what moves the slider, so a write logind refuses — the session isn't
/// active on its seat, say — leaves the popover showing the truth rather than
/// a lie.
pub fn set_brightness(device: String, raw_max: u32, percent: u8) -> Task<Message> {
    Task::future(send_set_brightness(device, raw_max, percent)).discard()
}

async fn send_set_brightness(device: String, raw_max: u32, percent: u8) {
    let raw = percent_to_raw(percent, raw_max);

    // System bus: logind is a machine-wide service, like UPower and iwd.
    let Ok(connection) = Connection::system().await else {
        return;
    };
    let Ok(proxy) = SessionProxy::new(&connection).await else {
        return;
    };
    if let Err(error) = proxy
        .set_brightness(BACKLIGHT_SUBSYSTEM, &device, raw)
        .await
    {
        eprintln!("saola-panel: setting {device} brightness to {percent}% ({raw}) failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This machine's real `max_brightness`, as a reminder that the scale is
    /// driver-defined and nothing may assume 100 or 255.
    const AMDGPU_MAX: u32 = 62451;

    #[test]
    fn percent_maps_the_drivers_scale_to_100() {
        assert_eq!(raw_to_percent(0, AMDGPU_MAX), 0);
        assert_eq!(raw_to_percent(AMDGPU_MAX, AMDGPU_MAX), 100);
        // The live reading at the time this module was written: 31230 of
        // 62451, i.e. a hair under half.
        assert_eq!(raw_to_percent(31230, AMDGPU_MAX), 50);
        assert_eq!(raw_to_percent(255, 255), 100);
        assert_eq!(raw_to_percent(51, 255), 20);
    }

    #[test]
    fn percent_rounds_to_the_nearest_whole() {
        // 33.4% and 33.6% of the scale, which must not both land on 33.
        assert_eq!(raw_to_percent(334, 1000), 33);
        assert_eq!(raw_to_percent(336, 1000), 34);
    }

    #[test]
    fn a_zero_max_does_not_divide_by_zero() {
        // `read_snapshot` rejects this before it can get here; the guard
        // exists so the function is safe on its own terms.
        assert_eq!(raw_to_percent(0, 0), 0);
        assert_eq!(raw_to_percent(500, 0), 0);
    }

    #[test]
    fn a_raw_above_max_clamps_instead_of_exceeding_100() {
        // Buggy ACPI firmware really does report this.
        assert_eq!(raw_to_percent(300, 255), 100);
    }

    #[test]
    fn percent_to_raw_never_exceeds_the_drivers_max() {
        assert_eq!(percent_to_raw(0, AMDGPU_MAX), 0);
        assert_eq!(percent_to_raw(100, AMDGPU_MAX), AMDGPU_MAX);
        // Out-of-range input is clamped rather than scaled past the ceiling —
        // an over-max write is rejected by the kernel outright.
        assert_eq!(percent_to_raw(200, AMDGPU_MAX), AMDGPU_MAX);
        assert_eq!(percent_to_raw(100, 255), 255);
    }

    #[test]
    fn percent_to_raw_rounds_to_the_nearest_step() {
        assert_eq!(percent_to_raw(50, 255), 128);
        assert_eq!(percent_to_raw(20, 255), 51);
    }

    #[test]
    fn a_percent_survives_the_round_trip_through_the_driver_scale() {
        // A slider value that goes out to logind and comes back through a
        // real sysfs read must land on the same whole percent.
        for percent in 0..=100u8 {
            assert_eq!(
                raw_to_percent(percent_to_raw(percent, AMDGPU_MAX), AMDGPU_MAX),
                percent
            );
        }
    }

    #[test]
    fn a_coarse_scale_still_round_trips() {
        // 255 steps is only 2.5 per percent, so rounding has real work to do.
        for percent in 0..=100u8 {
            assert_eq!(raw_to_percent(percent_to_raw(percent, 255), 255), percent);
        }
    }

    #[test]
    fn default_brightness_is_absent_and_empty() {
        // The "quiet until proven otherwise" contract: before the worker's
        // first report the quick-settings row must draw nothing.
        let brightness = Brightness::default();
        assert!(!brightness.is_present());
        assert_eq!(brightness.percent(), 0);
        assert_eq!(brightness.device(), "");
        assert_eq!(brightness.raw_max(), 0);
    }

    #[test]
    fn accessors_read_through_to_the_stored_fields() {
        let brightness = Brightness {
            percent: 50,
            device: "amdgpu_bl1".to_string(),
            raw_max: AMDGPU_MAX,
            present: true,
        };
        assert!(brightness.is_present());
        assert_eq!(brightness.percent(), 50);
        assert_eq!(brightness.device(), "amdgpu_bl1");
        assert_eq!(brightness.raw_max(), AMDGPU_MAX);
    }
}
