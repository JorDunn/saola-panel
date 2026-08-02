//! Live-reload for `panel.kdl`: watch the file on disk, and hand the panel a
//! freshly parsed [`PanelConfig`] whenever it changes — so a config edit
//! reaches the running bar without a restart.
//!
//! # The signal (and why this isn't the poll CLAUDE.md forbids)
//!
//! The kernel's inotify(7) interface *pushes* file-change events: the worker
//! below is asleep in `stream.next().await` until the kernel has something to
//! say, exactly like the D-Bus modules are asleep in their signal streams.
//! Nothing here ticks, and an untouched config file costs the panel nothing
//! for the whole life of the process. (The one `sleep` below is a debounce
//! that only ever runs *after* an event has already arrived — gated, like the
//! marquee's timer, not standing.)
//!
//! # Watch the directory, not the file (teaching note)
//!
//! An inotify watch follows an **inode**, not a path. Most editors save
//! "atomically": write the new content to a temp file, then `rename(2)` it
//! over `panel.kdl` — which replaces the inode, so a watch on the file itself
//! goes quiet after the very first save. Watching the parent directory
//! (`~/.config/saola/`) instead means every way the file can change arrives
//! as a directory event carrying the file's *name* — `CLOSE_WRITE` for an
//! in-place save, `MOVED_TO` for the atomic rename, `CREATE`/`DELETE` for the
//! file appearing or going away — and the name filter below picks out the
//! ones about `panel.kdl`.
//!
//! # Debounce
//!
//! One human "save" is several kernel events (vim's atomic save is a temp
//! file plus a rename; some editors truncate, write, and close in separate
//! syscalls). Reloading on each would apply a half-written file. So the first
//! relevant event starts a short grace period, everything that arrives during
//! it is drained and discarded, and the file is read once at the end —
//! by which point the save has finished.
//!
//! # Resilience (the absent-service rule, applied to a directory)
//!
//! *Which* file to watch is decided in `main` (`PanelConfig::resolve_path`
//! — the `--config-dir` / `$SAOLA_CONFIG_DIR` / XDG chain) and handed in
//! through [`subscription`]; an environment where nothing in that chain
//! resolves gets no subscription at all. No config *directory* at boot →
//! the watch can't be established, a single stderr line says live-reload
//! is off, and the worker parks forever — the panel runs exactly as before
//! this module existed. (Creating the directory and the file later needs a
//! restart to pick up: watching the directory's *parent* for it to appear
//! is more machinery than the case is worth. A `panel.kdl` *edited* into
//! an existing directory is the case this module serves, and that one
//! works from the first save.) What the reload does with a malformed file
//! is [`PanelConfig::reload_from`]'s contract: keep the running config,
//! never flash to defaults mid-edit.

use std::path::{Path, PathBuf};
use std::time::Duration;

use iced::futures::channel::mpsc;
use iced::futures::{FutureExt, SinkExt, Stream, StreamExt};
use iced::Subscription;
use inotify::{EventMask, Inotify, WatchMask};

use crate::config::PanelConfig;

/// What the watcher produces — nested into the panel's outer enum as
/// `Message::ConfigReloaded(config_watch::Message)`, the same wiring shape as
/// every module's own message type.
#[derive(Debug, Clone)]
pub enum Message {
    /// The config file changed and parsed: here is the whole new
    /// [`PanelConfig`], resolved. Carrying the finished value (rather than a
    /// bare "something changed" ping) keeps the file I/O and parsing on the
    /// worker, off the UI thread — the same reasoning as every D-Bus module
    /// shipping a finished snapshot. **CLI overrides are *not* applied here**:
    /// the worker doesn't know them, and `main.rs`'s reload arm re-applies
    /// the flags it stored at boot so `--bottom` keeps beating the file
    /// across reloads exactly as it did at startup.
    Reloaded(PanelConfig),
}

/// The watcher as an iced subscription, for the `panel.kdl` path `main`
/// resolved at boot (`PanelConfig::resolve_path` — the `--config-dir` /
/// `$SAOLA_CONFIG_DIR` / XDG chain). Taking the *resolved* path rather than
/// re-deriving it here is what guarantees the watcher and the boot loader
/// can never disagree about which file is the config.
///
/// Identity mechanics: `Subscription::run_with` keys on the fn pointer
/// *plus* the `data` value (the same shape as `popovers::tray_menu::watch`),
/// so iced would tear the worker down and spin up a fresh one if the path
/// ever changed between `Panel::subscription` recomputations. Here it never
/// does — argv and the environment are fixed at exec time, and `Panel`
/// stores the path once — so in practice the key buys the same
/// one-worker-forever guarantee `Subscription::run` gives the other
/// modules, while letting the worker receive an argument at all (a bare
/// `run` fn cannot).
pub fn subscription(path: &Path) -> Subscription<Message> {
    Subscription::run_with(path.to_path_buf(), watch_stream)
}

/// How long after the first change event the reload waits for the save to
/// finish (see the module doc comment's debounce section). Long enough to
/// cover any editor's multi-syscall save; far too short to feel like lag on
/// a human timescale.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// The worker: establish the directory watch, then loop forever turning
/// bursts of change events into at most one reload each.
///
/// The `&PathBuf` parameter (clippy would rather see `&Path`) is not a
/// stylistic slip: [`Subscription::run_with`]'s `builder` parameter is
/// `fn(&D) -> S`, and `D` here is `PathBuf` (the type [`subscription`]
/// hands `run_with` as `data`) — a `fn(&Path) -> S` is a different,
/// non-matching type, so `&Path` would not compile. (The same note, for
/// the same reason, as `popovers::tray_menu::stream_for`.)
#[allow(
    clippy::ptr_arg,
    reason = "must match Subscription::run_with's fn(&D) -> S exactly, where D = PathBuf"
)]
fn watch_stream(path: &PathBuf) -> impl Stream<Item = Message> {
    let path = path.clone();
    iced::stream::channel(4, async move |mut sender: mpsc::Sender<Message>| {
        // The path always has a parent (`…/panel.kdl` under some resolved
        // directory, by `PanelConfig::resolve_path`'s construction), but
        // destructure rather than unwrap — a defensive posture this worker
        // can afford, since "no watch" is a legal outcome. Park (rather
        // than return) so the subscription stays formally alive without
        // iced re-running it.
        let Some(dir) = path.parent() else {
            iced::futures::future::pending::<()>().await;
            return;
        };
        let Some(file_name) = path.file_name().map(std::ffi::OsStr::to_os_string) else {
            iced::futures::future::pending::<()>().await;
            return;
        };

        // The four ways the file's content can change under its name, per
        // the module doc comment: in-place save, atomic-rename save, created
        // fresh, deleted. `MOVED_FROM` covers `mv panel.kdl elsewhere`,
        // which is a deletion from this directory's point of view. The two
        // `_SELF` marks are about the watched *directory* itself going away
        // — without them the kernel would still drop the watch (delivering
        // only an unnamed `IGNORED` this loop's name filter would swallow),
        // and live-reload would die with no trace; catching them explicitly
        // is what turns that into a stderr line (see the loop below).
        let mask = WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_TO
            | WatchMask::MOVED_FROM
            | WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF;

        let inotify = match Inotify::init() {
            Ok(inotify) => inotify,
            Err(err) => {
                eprintln!("saola-panel: inotify unavailable ({err}) — live-reload disabled");
                iced::futures::future::pending::<()>().await;
                return;
            }
        };
        if let Err(err) = inotify.watches().add(dir, mask) {
            // The common cause: ~/.config/saola doesn't exist yet. One line,
            // then quiet — the absent-service contract.
            eprintln!(
                "saola-panel: cannot watch {} ({err}) — live-reload disabled",
                dir.display()
            );
            iced::futures::future::pending::<()>().await;
            return;
        }

        // The buffer inotify parses events out of. 4 KiB fits dozens of
        // directory events per read; a config directory sees a handful per
        // save.
        let mut stream = match inotify.into_event_stream([0u8; 4096]) {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("saola-panel: inotify stream failed ({err}) — live-reload disabled");
                iced::futures::future::pending::<()>().await;
                return;
            }
        };

        while let Some(event) = stream.next().await {
            let Ok(event) = event else { continue };
            // The watched directory itself was deleted or renamed out from
            // under us. The kernel has already dropped the watch (an
            // `IGNORED` follows), so no future edit can ever wake this
            // worker again — say so once, then park, the same posture as a
            // directory that was absent at boot. (Re-establishing the watch
            // on a recreated directory would mean polling for it to
            // reappear, which is exactly what this module must not do.)
            if event
                .mask
                .intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF)
            {
                eprintln!(
                    "saola-panel: {} is gone — live-reload disabled until restart",
                    dir.display()
                );
                iced::futures::future::pending::<()>().await;
                return;
            }
            // Directory events name the child they concern; skip everything
            // that isn't about panel.kdl (the temp files of an atomic save,
            // sibling configs, …).
            if event.name.as_deref() != Some(file_name.as_os_str()) {
                continue;
            }

            // Debounce: let the save finish, then drain whatever else it
            // queued so a three-event save is one reload, not three.
            // `now_or_never` polls the next-event future exactly once —
            // `Some` means an event was already waiting (discard it and ask
            // again), `None` means the queue is empty and we can read the
            // settled file.
            tokio::time::sleep(DEBOUNCE).await;
            while let Some(Some(_)) = stream.next().now_or_never() {}

            if let Some(config) = PanelConfig::reload_from(&path) {
                // An `Err` here means the receiving side is gone — the app
                // is shutting down — so the worker's job is over either way.
                if sender.send(Message::Reloaded(config)).await.is_err() {
                    return;
                }
            }
        }
    })
}
