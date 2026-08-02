//! One tray item: where it lives on the bus, what it is called, and the
//! pure parsing that gets from a registration string to both.
//!
//! # SNI's famous registration-string quirk (the thing to read first)
//!
//! `org.kde.StatusNotifierWatcher.RegisterStatusNotifierItem` takes a single
//! string argument, and the protocol lets it be **two different things**:
//!
//! - a plain bus name (`":1.42"`, or the well-known
//!   `"org.kde.StatusNotifierItem-1234-1"`), in which case the item object
//!   is at the *default* path [`DEFAULT_ITEM_PATH`]; or
//! - an object path (`"/org/ayatana/NotificationItem/foo"`, i.e. anything
//!   starting with `/`), in which case the bus name is not in the argument
//!   at all — it is the **D-Bus message sender** of the register call.
//!
//! KDE's reference watcher resolves that ambiguity by *normalizing*: it
//! works out the (bus, path) pair, concatenates them back into one
//! `"<bus><path>"` string, and uses that as both the registry key and the
//! payload of `StatusNotifierItemRegistered`. So the strings that come back
//! out of a watcher look like `":1.42/StatusNotifierItem"` — a form that is
//! *neither* of the two input forms, and that every host then has to split
//! at the first `/`. [`registration_id`] is the normalizing half (used when
//! this panel *is* the watcher) and [`parse_registration`] is the splitting
//! half (used either way, since a foreign watcher hands us the same shape).
//!
//! [`parse_registration`] deliberately also accepts a bare bus name with no
//! path, because not every watcher in the wild normalizes: some hand hosts
//! back exactly what the item passed in. Being liberal there costs one
//! branch and covers both.

use iced::futures::stream::{self, BoxStream, StreamExt};
use zbus::proxy::CacheProperties;
use zbus::Connection;

/// Where an item's object lives when the registration string didn't say —
/// the path SNI declares as the default.
pub(super) const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";

/// A resolved item address: the two halves a registration string encodes.
///
/// Kept as owned `String`s rather than zbus's `BusName`/`ObjectPath` types
/// on purpose: this is the *model*, it crosses the worker→UI channel inside
/// [`TrayItem`], and it has to be `Clone + PartialEq + Send + 'static` with
/// no lifetime attached. The conversion into zbus's validated types happens
/// once, at proxy-build time in [`read_item`], where a malformed path fails
/// loudly and locally instead of poisoning the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAddress {
    bus: String,
    path: String,
}

impl ItemAddress {
    /// The bus name half — unique (`:1.42`) or well-known
    /// (`org.kde.StatusNotifierItem-1234-1`); both forms occur in the wild,
    /// and both are what `NameOwnerChanged` reports when the item dies, which
    /// is why the watcher can match on this string directly.
    pub(super) fn bus(&self) -> &str {
        &self.bus
    }

    /// The object path half — [`DEFAULT_ITEM_PATH`] unless the item asked
    /// for something else.
    pub(super) fn path(&self) -> &str {
        &self.path
    }

    /// The same bus name, at a different object path.
    ///
    /// Exactly one caller (Stage 20's [`read_menu_address`]): an item's
    /// `com.canonical.dbusmenu` object lives on the item's own connection but
    /// at a path only the item knows, so "same bus, different path" is a real
    /// operation on this type rather than a second address type with the same
    /// two fields.
    fn at_path(&self, path: String) -> Self {
        Self {
            bus: self.bus.clone(),
            path,
        }
    }
}

/// One tray item as the bar knows it.
///
/// `id` is the **registration string** exactly as the watcher reports it —
/// the registry key, and the string a later `StatusNotifierItemUnregistered`
/// will name. `address` is that same string parsed; `label` is the resolved
/// display text — Stage 18 rendered it directly as a temporary title pill;
/// Stage 19 renders the real icon instead and keeps the label only as the
/// fallback for an item whose icon couldn't be resolved at all (see
/// [`TrayIcon`]'s doc comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayItem {
    id: String,
    address: ItemAddress,
    label: String,
    icon: Option<TrayIcon>,
    status: ItemStatus,
    is_menu: bool,
}

impl TrayItem {
    pub(super) fn new(
        id: String,
        address: ItemAddress,
        label: String,
        icon: Option<TrayIcon>,
        status: ItemStatus,
        is_menu: bool,
    ) -> Self {
        Self {
            id,
            address,
            label,
            icon,
            status,
            is_menu,
        }
    }

    /// The registration string this item is keyed by.
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    /// Where the item's `org.kde.StatusNotifierItem` object lives — what
    /// `Activate`/`Scroll` calls target (see `send_activate`/`send_scroll`).
    pub(super) fn address(&self) -> &ItemAddress {
        &self.address
    }

    /// The fallback text — used by `Tray::view` only when [`Self::icon`] is
    /// `None`.
    pub(super) fn label(&self) -> &str {
        &self.label
    }

    /// The item's resolved icon, or `None` if nothing could be resolved at
    /// all (see [`TrayIcon`]'s doc comment for exactly when that happens).
    pub(super) fn icon(&self) -> Option<&TrayIcon> {
        self.icon.as_ref()
    }

    /// Passive / Active / NeedsAttention — see [`ItemStatus`].
    pub(super) fn status(&self) -> ItemStatus {
        self.status
    }

    /// Whether the item declared `ItemIsMenu` — "the item only supports the
    /// context menu" (spec), meaning it has no `Activate` behaviour and a
    /// left click should open its dbusmenu instead. `Tray::view` is the
    /// consumer; see its left-click wiring.
    pub(super) fn is_menu(&self) -> bool {
        self.is_menu
    }
}

/// SNI's `Status` property, reduced to the three values the spec defines.
/// Presentation only (PLAN.md Stage 19) — never a fourth color: see
/// `Tray::view`'s doc comment for exactly how each value is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ItemStatus {
    /// "Doesn't convey important information... likely that visualizations
    /// will choose to hide it" (spec). `Tray::view` takes that literally:
    /// a Passive item is filtered out of the row entirely, which is the
    /// quietest presentation there is and needs no new color to express.
    Passive,
    /// The ordinary, at-rest presentation — and also what an item that
    /// never answers `Status` at all gets (see [`Self::from_sni`]):
    /// treating "unknown" as "normal" is the least surprising default, and
    /// matches every reference host's own behavior.
    #[default]
    Active,
    /// Wants the user's attention right now. Emphasis stays within the
    /// three colors — see `Tray::view`'s doc comment for the exact
    /// treatment.
    NeedsAttention,
}

impl ItemStatus {
    /// `Status`'s three defined wire values map directly; anything else
    /// (an empty string because the item didn't implement the property, a
    /// future spec value, a nonconformant item) falls back to `Active` —
    /// see the variant's own doc comment for why that's the safe default.
    ///
    /// Pure function; unit-tested below.
    fn from_sni(status: &str) -> Self {
        match status {
            "Passive" => Self::Passive,
            "NeedsAttention" => Self::NeedsAttention,
            _ => Self::Active,
        }
    }
}

/// The item registry: every registered item, in the order they registered.
///
/// A `Vec`, not a `HashMap`: the bar renders these in a row and the order
/// has to be stable and meaningful (items appear as their apps start), which
/// a hash map cannot promise. Lookups are linear, over a set that is
/// realistically under ten entries — a tray with enough items for the
/// difference to matter is a tray nobody can read anyway.
#[derive(Debug, Default)]
pub(super) struct ItemRegistry {
    items: Vec<TrayItem>,
}

impl ItemRegistry {
    /// Insert a new item, or replace an existing one with the same
    /// registration string **in place** (keeping its position in the row).
    ///
    /// Re-registration is normal, not an error: an app that was already
    /// running when a watcher appears re-registers itself, and some
    /// toolkits re-register on every icon change.
    pub(super) fn upsert(&mut self, item: TrayItem) {
        match self.items.iter_mut().find(|held| held.id() == item.id()) {
            Some(held) => *held = item,
            None => self.items.push(item),
        }
    }

    /// Drop one item by registration string. Returns whether anything went.
    pub(super) fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id() != id);
        self.items.len() != before
    }

    /// Drop every item served by `bus`, returning their registration
    /// strings.
    ///
    /// This is the "the app just died" path: D-Bus tells us a *bus name*
    /// lost its owner, and the registry is keyed by registration strings, so
    /// the two have to be bridged by the parsed [`ItemAddress::bus`]. One
    /// bus name can legitimately own several items (an app with two
    /// indicators), hence a `Vec` rather than an `Option`.
    pub(super) fn remove_owned_by(&mut self, bus: &str) -> Vec<String> {
        let removed: Vec<String> = self
            .items
            .iter()
            .filter(|item| item.address().bus() == bus)
            .map(|item| item.id().to_string())
            .collect();
        self.items.retain(|item| item.address().bus() != bus);
        removed
    }

    /// The snapshot the worker pushes to the UI.
    pub(super) fn snapshot(&self) -> super::Tray {
        super::Tray {
            items: self.items.clone(),
        }
    }
}

/// Normalize a `RegisterStatusNotifierItem` argument into the canonical
/// `"<bus><path>"` registration string — **the watcher half of the quirk**
/// (see the module doc comment).
///
/// `sender` is the D-Bus message's own sender (its unique name), which is
/// the only place the bus name comes from when the argument is a bare object
/// path. `None` means the message had no sender — impossible on a bus
/// connection, but the header field is optional in the wire format, so it is
/// modelled honestly and rejected rather than unwrapped.
///
/// Pure function of its two arguments; unit-tested below on both input
/// forms.
pub(super) fn registration_id(service: &str, sender: Option<&str>) -> Option<String> {
    let service = service.trim();
    if service.is_empty() {
        return None;
    }

    if service.starts_with('/') {
        // Object-path form: the bus name is who called us.
        let sender = sender?;
        if sender.is_empty() {
            return None;
        }
        Some(format!("{sender}{service}"))
    } else if service.contains('/') {
        // Already in the concatenated form (an item that read another
        // watcher's output and echoed it back, or a toolkit that builds the
        // string itself). Accept it as-is rather than gluing a second path
        // onto the end.
        Some(service.to_string())
    } else {
        // Bare bus name: the item is at the default path.
        Some(format!("{service}{DEFAULT_ITEM_PATH}"))
    }
}

/// Split a registration string back into its two halves — **the host half
/// of the quirk** (see the module doc comment).
///
/// Both accepted forms:
///
/// - `":1.42/StatusNotifierItem"` / `":1.42/org/ayatana/NotificationItem/x"`
///   — everything up to the *first* `/` is the bus name, the rest (slash
///   included) is the object path;
/// - `":1.42"` / `"org.kde.StatusNotifierItem-1234-1"` — no `/` at all, so
///   the object is at [`DEFAULT_ITEM_PATH`].
///
/// `None` for anything that can't name a bus: an empty string, or a bare
/// object path (which is only meaningful *with* a message sender beside it,
/// and by the time a string reaches here that sender is long gone — see
/// [`registration_id`], which is where that form is resolved).
///
/// Pure function; unit-tested below on both forms and the rejections.
pub(super) fn parse_registration(registration: &str) -> Option<ItemAddress> {
    let registration = registration.trim();
    if registration.is_empty() {
        return None;
    }

    let (bus, path) = match registration.find('/') {
        Some(0) => return None, // a bare object path names no bus
        Some(split) => (&registration[..split], &registration[split..]),
        None => (registration, DEFAULT_ITEM_PATH),
    };

    if bus.is_empty() || path.is_empty() {
        return None;
    }

    Some(ItemAddress {
        bus: bus.to_string(),
        path: path.to_string(),
    })
}

/// The pill's text, from whatever the item actually told us.
///
/// `Title` is the human-facing name and the first choice, but the SNI spec
/// marks it optional and plenty of real items leave it empty. `Id` is
/// required by the spec ("a name that should be unique for this
/// application") and is usually something readable like `"nm-applet"`, so it
/// is the fallback. The bus name is the last resort — ugly, but it proves
/// *something* is there, which beats a blank pill that looks like a bug.
///
/// Pure function; unit-tested below.
pub(super) fn item_label(title: &str, id: &str, bus: &str) -> String {
    [title, id, bus]
        .into_iter()
        .map(str::trim)
        .find(|candidate| !candidate.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// A zbus proxy for one item's `org.kde.StatusNotifierItem` interface.
///
/// Teaching note (no `default_service`, no `default_path`): every item lives
/// at a different bus name *and* possibly a different path, both discovered
/// at runtime from the registration string, so neither default can be given
/// and the generated constructor is the builder — same situation as
/// `media.rs`'s per-player proxy, one step more so.
///
/// Teaching note (**why caching is switched off**): zbus caches
/// `#[zbus(property)]` values and keeps them fresh from the standard
/// `org.freedesktop.DBus.Properties.PropertiesChanged` signal. SNI items
/// famously **do not emit that signal** — the protocol has its own
/// `NewTitle`/`NewIcon`/`NewStatus` signals instead, which is exactly why
/// those signals exist. A cached proxy would therefore serve the first value
/// it ever read, forever. [`CacheProperties::No`] makes every getter a real
/// `Get` call, so a re-read after one of those signals (Stage 19's job)
/// actually sees the new value. It also avoids the upfront `GetAll` that
/// some items answer badly.
#[zbus::proxy(interface = "org.kde.StatusNotifierItem")]
trait StatusNotifierItem {
    /// Human-readable name. Optional in the spec; often empty in practice.
    #[zbus(property)]
    fn title(&self) -> zbus::Result<String>;

    /// Application id, e.g. `"nm-applet"`. Required by the spec.
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;

    /// A themed icon name (e.g. `"slack"`, `"network-wireless"`) — the
    /// *first* choice in [`resolve_icon`]'s precedence. Optional in the
    /// spec; plenty of items publish only [`Self::icon_pixmap`] instead.
    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;

    /// The protocol's embedded-bitmap fallback: `a(iiay)` on the wire, an
    /// array of (width, height, ARGB32-in-network-byte-order pixel data)
    /// tuples — an item may publish several sizes, largest usually last.
    /// See [`argb_network_to_rgba`]'s doc comment for the byte-order
    /// gotcha in that last field, and [`pick_pixmap`] for how one entry is
    /// chosen.
    #[zbus(property)]
    fn icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;

    /// `"Passive"` / `"Active"` / `"NeedsAttention"` — see [`ItemStatus`].
    #[zbus(property)]
    fn status(&self) -> zbus::Result<String>;

    /// The object path of this item's `com.canonical.dbusmenu` object —
    /// Stage 20's addition, and the only reason `modules::tray::menu` can
    /// find a menu at all. Lives on *this* item's bus name; see
    /// [`read_menu_address`] for the two "there is no menu" sentinels real
    /// implementations publish here.
    #[zbus(property)]
    fn menu(&self) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// "The item only supports the context menu, [and] the visualization
    /// should prefer showing the menu" (spec) — i.e. the item has no
    /// [`Self::activate`] behaviour at all. True for essentially every
    /// libdbusmenu/libappindicator-based item (tailscale, discord, ...),
    /// which don't even *export* an `Activate` method — calling it anyway
    /// gets `org.freedesktop.DBus.Error.UnknownMethod` back. Read once per
    /// [`read_item`] re-read, absent-defaults-false like the spec says.
    #[zbus(property)]
    fn item_is_menu(&self) -> zbus::Result<bool>;

    /// SNI's primary interaction: a left click. `x`/`y` are screen
    /// coordinates some items use to position a window they open in
    /// response — this panel always sends `(0, 0)`; see
    /// [`send_activate`]'s doc comment for why.
    async fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;

    /// A scroll over the item. `orientation` is `"horizontal"` or
    /// `"vertical"`; see `modules::tray::scroll_units` for how a wheel
    /// event reduces to the `(delta, orientation)` pair this wants.
    async fn scroll(&self, delta: i32, orientation: &str) -> zbus::Result<()>;

    /// Fired when `IconName`/`IconPixmap` change. Carries no payload —
    /// same shape as the standard `PropertiesChanged` signal would have
    /// been, if SNI items actually emitted that (see [`read_item`]'s doc
    /// comment on why they don't). [`watch_item`] responds by asking
    /// `watcher.rs` to re-read the whole item, the same "just re-read
    /// everything" simplicity `battery.rs`'s `watch_upower` already uses.
    #[zbus(signal)]
    fn new_icon(&self);

    /// Fired when `Status` changes. Carries the new value directly, but
    /// [`watch_item`] still triggers a full re-read rather than trusting
    /// the payload in isolation — a status change and an icon change
    /// arriving in the same moment should cost one round-trip, not two
    /// half-applied updates.
    #[zbus(signal)]
    fn new_status(&self, status: String);

    /// Fired when `Title` changes — closes the gap Stage 18's handoff
    /// flagged ("this stage reads the title once, at registration, and
    /// never refreshes it").
    #[zbus(signal)]
    fn new_title(&self);
}

/// What [`read_item`] found: the item as the bar would render it, plus
/// whether anything actually answered.
///
/// The two are separate because the right response to "it didn't answer"
/// depends on *why we asked*, and only the caller knows that (see
/// `watcher.rs`, which is lenient about an item that just registered and
/// strict about an entry inherited from another watcher's list).
pub(super) struct ItemProbe {
    pub(super) item: TrayItem,
    /// True if at least one property read came back. An item that answers
    /// nothing is either gone or so minimal that there is no name to show;
    /// either way the label will have fallen back to its bus name.
    pub(super) answered: bool,
}

/// Resolve a registration string into a renderable item, by asking the item
/// itself what it is called.
///
/// `None` means the string didn't name a bus at all (see
/// [`parse_registration`]) — the only genuinely unusable outcome. A live
/// object that refuses both property reads still comes back as an item, with
/// `answered: false` and its bus name for a label; deciding what to do with
/// that is the caller's business.
pub(super) async fn read_item(connection: &Connection, id: &str) -> Option<ItemProbe> {
    let address = parse_registration(id)?;
    let proxy = build_item_proxy(connection, &address).await?;

    let title = proxy.title().await.ok();
    let name = proxy.id().await.ok();
    let icon = resolve_icon(&proxy).await;
    let status = proxy
        .status()
        .await
        .ok()
        .map(|status| ItemStatus::from_sni(&status))
        .unwrap_or_default();

    let label = item_label(
        title.as_deref().unwrap_or_default(),
        name.as_deref().unwrap_or_default(),
        address.bus(),
    );

    // Absent (or mistyped) defaults to false, per the spec's own default —
    // an item that never heard of `ItemIsMenu` keeps the ordinary
    // left-click-activates behaviour.
    let is_menu = proxy.item_is_menu().await.unwrap_or(false);

    Some(ItemProbe {
        item: TrayItem::new(id.to_string(), address, label, icon, status, is_menu),
        answered: title.is_some() || name.is_some(),
    })
}

/// The object path KDE's own `KStatusNotifierItem` publishes in `Menu` when
/// the item has no menu at all.
///
/// It is a perfectly valid object path, so nothing about the *type* rejects
/// it — only this string comparison does. Missing it would mean building a
/// dbusmenu proxy for an object that isn't there and waiting out the bus
/// daemon's reply timeout on every right-click.
const NO_MENU_PATH: &str = "/NO_DBUSMENU";

/// Decide whether a `Menu` property value actually names a menu object.
///
/// Split out of [`read_menu_address`] as a pure function precisely because
/// this — not the D-Bus plumbing around it — is the part with rules worth
/// pinning down: `""` (an item that answered with nothing), `"/"` (the bus's
/// root path, which nothing serves an interface at) and [`NO_MENU_PATH`] all
/// mean "this item has no menu", and each of them arrives as a perfectly
/// well-formed value that a type-level check would happily accept.
///
/// Pure function; unit-tested below.
fn usable_menu_path(path: &str) -> Option<&str> {
    let path = path.trim();
    if path.is_empty() || path == "/" || path == NO_MENU_PATH {
        return None;
    }
    Some(path)
}

/// Where an item's `com.canonical.dbusmenu` object lives, or `None` if it has
/// no menu.
///
/// Stage 20's addition, and the first of the two round-trips
/// `menu::read_menu` makes. `None` covers all four ways "no menu" reaches us:
/// a registration string that names no bus, a proxy that can't be built, an
/// item that doesn't implement `Menu` at all (the property read errors), and
/// an item that implements it but publishes one of the two sentinels — `"/"`
/// (the root path, which no sane implementation serves a menu at) or
/// [`NO_MENU_PATH`].
///
/// Deliberately *not* cached on [`TrayItem`]: the path is only ever needed at
/// the moment a menu is opened, which is rare and already several D-Bus
/// round-trips deep, whereas caching it would mean re-reading it on every
/// `NewIcon`/`NewStatus`/`NewTitle` signal for every item forever.
/// `ItemIsMenu`, by contrast, *is* cached on [`TrayItem`] (2026-08-01):
/// the bar needs it at draw time to decide what a left click even means,
/// so [`read_item`] reads it with everything else.
pub(super) async fn read_menu_address(connection: &Connection, id: &str) -> Option<ItemAddress> {
    let address = parse_registration(id)?;
    let proxy = build_item_proxy(connection, &address).await?;
    let path = proxy.menu().await.ok()?;

    let path = usable_menu_path(path.as_str())?;

    Some(address.at_path(path.to_string()))
}

/// Build a fresh, uncached proxy for one item's `org.kde.StatusNotifierItem`
/// object.
///
/// The one place the four-call builder chain lives: [`read_item`],
/// [`watch_item`], and the command-out functions below (`send_activate`/
/// `send_scroll`) each need their *own* independent proxy — none of them
/// should share one, per the module-level "no persistent proxy in the
/// model" shape this file already follows — and duplicating the chain at
/// every call site is exactly the kind of copy-paste that drifts quietly.
async fn build_item_proxy<'a>(
    connection: &'a Connection,
    address: &ItemAddress,
) -> Option<StatusNotifierItemProxy<'a>> {
    StatusNotifierItemProxy::builder(connection)
        .destination(address.bus().to_string())
        .ok()?
        .path(address.path().to_string())
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()
}

/// The bar's resolved image for one item — already a ready-to-draw handle,
/// built once per [`read_item`]/[`watch_item`]-triggered re-read.
///
/// `Tray::view` never tints these: unlike this codebase's own Lucide
/// assets (`crate::icons`, always recolored to an ivory/terracotta theme
/// role), a real application's tray icon carries its own identity — Slack's
/// icon has to look like Slack's icon — so both variants are drawn with
/// whatever colors the source asset itself defines.
///
/// No `None`/"unresolved" variant here on purpose: [`TrayItem::icon`]
/// already models "nothing resolved" as `Option<TrayIcon>` one level up,
/// which is what [`Tray`](super::Tray)'s view falls back to the item's
/// label text for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TrayIcon {
    /// Resolved through the freedesktop icon-theme lookup, to a vector
    /// (SVG) asset.
    Svg(iced::widget::svg::Handle),
    /// Resolved through the same lookup, to a raster (PNG or similar)
    /// asset.
    Raster(iced::widget::image::Handle),
    /// Decoded straight from the item's own `IconPixmap` property — no
    /// theme, no disk. Already reshuffled from the wire's ARGB32/network
    /// byte order into iced's RGBA (see [`argb_network_to_rgba`]).
    Pixmap(iced::widget::image::Handle),
}

/// The size this module asks the freedesktop icon theme lookup for.
///
/// A *hint* to the OS theme lookup, not a rendering size: on-screen size is
/// always `theme.sizes.icon_bar`, set at the widget in `Tray::view`,
/// regardless of the source asset's native resolution — icons scale to fit
/// either way. 24 matches this codebase's own Lucide assets purely for
/// consistency (`assets/icons/*.svg` are all `viewBox="0 0 24 24"`), not
/// because the lookup has to agree with anything the bar does.
const ICON_LOOKUP_SIZE: u16 = 24;

/// Resolve one item's icon, in the precedence order PLAN.md Stage 19 asks
/// for: `IconName`, through the freedesktop theme lookup, first — a real
/// application usually names a theme icon, and the theme's own SVG/PNG
/// looks right at any size — falling back to `IconPixmap` (raw pixel data
/// the item embedded itself) only when that lookup has nothing to offer,
/// since decoding a bitmap by hand is the fallback, never the first choice.
///
/// `None` means the item genuinely offers nothing usable this way — no
/// name, a theme-lookup miss, no pixmap entries, or a malformed one.
/// `Tray::view` degrades to the item's label text in that case, never a
/// panic (CLAUDE.md: "a failing icon resolution must degrade gracefully").
async fn resolve_icon(proxy: &StatusNotifierItemProxy<'_>) -> Option<TrayIcon> {
    if let Ok(name) = proxy.icon_name().await {
        let name = name.trim();
        if !name.is_empty() {
            if let Some(icon) = lookup_theme_icon(name).await {
                return Some(icon);
            }
        }
    }

    let pixmaps = proxy.icon_pixmap().await.ok()?;
    pixmap_icon(&pixmaps)
}

/// The freedesktop theme lookup, off the async executor.
///
/// Teaching note (**why `spawn_blocking`**): `freedesktop_icons::lookup`
/// does real synchronous filesystem I/O — walking theme directories,
/// reading `index.theme` files — exactly the class of blocking call
/// CLAUDE.md's "never blocking calls on the UI thread" rule warns about.
/// It's cheap in the common case (a handful of `stat`s, usually a warm page
/// cache), but "usually fast" is precisely the assumption that turns into a
/// stalled worker the day it isn't (a slow network home directory, an icon
/// theme with an unusually deep `index.theme` inheritance chain).
/// `spawn_blocking` hands the call to tokio's dedicated blocking-thread
/// pool instead of running it inline on the tray worker's own async task —
/// one thread-pool hop, in exchange for never risking that stall. (`"rt"`,
/// the tokio feature `spawn_blocking` needs, isn't listed in this crate's
/// own `Cargo.toml` — it arrives via Cargo's feature unification, because
/// `iced_futures`'s own `tokio` feature already requires
/// `rt`/`rt-multi-thread`/`time` on the very same `tokio` this crate
/// depends on.)
///
/// `.ok().flatten()`: either the blocking task failing to join (extremely
/// unlikely, and not worth a panic) or the lookup finding nothing both mean
/// "no icon this way" — same as `IconName` being empty.
async fn lookup_theme_icon(name: &str) -> Option<TrayIcon> {
    let name = name.to_string();
    let path = tokio::task::spawn_blocking(move || {
        freedesktop_icons::lookup(&name)
            .with_size(ICON_LOOKUP_SIZE)
            .find()
    })
    .await
    .ok()
    .flatten()?;

    // The lookup's own priority (PNG before SVG, unless `.force_svg()` is
    // asked for — not used here) already picked the file; the extension is
    // only how this module decides *which iced widget* draws it. Checked
    // case-insensitively since some themes ship `.SVG`. Anything else is
    // treated as raster — `image::Handle`'s decoder guesses the real format
    // from the file's own bytes, not the extension, so this never has to
    // enumerate every raster format freedesktop themes use.
    let is_svg = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));

    Some(if is_svg {
        TrayIcon::Svg(iced::widget::svg::Handle::from_path(path))
    } else {
        TrayIcon::Raster(iced::widget::image::Handle::from_path(path))
    })
}

/// Build a [`TrayIcon`] from the item's own `IconPixmap` — tried only once
/// [`lookup_theme_icon`] has nothing to offer.
///
/// [`pick_pixmap`] chooses which of possibly several published sizes to
/// use; [`argb_network_to_rgba`] does the one-time byte shuffle raw wire
/// bytes need before iced will treat them as pixels. Splitting both into
/// pure, unit-tested functions is what lets the fiddly byte-level logic be
/// tested without a bus at all — this async shell is just the plumbing
/// around them.
fn pixmap_icon(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<TrayIcon> {
    let (width, height, data) = pick_pixmap(pixmaps, i32::from(ICON_LOOKUP_SIZE))?;
    let rgba = argb_network_to_rgba(*width, *height, data)?;
    Some(TrayIcon::Pixmap(iced::widget::image::Handle::from_rgba(
        *width as u32,
        *height as u32,
        rgba,
    )))
}

/// Pick the pixmap closest to `target` size among an item's `IconPixmap`
/// array — closest by absolute difference in width, largest-first on a
/// tie (an oversized icon downscales cleanly; an undersized one does not).
/// Entries with a non-positive width or height are never chosen — SNI
/// doesn't forbid a misbehaving item from publishing one, and indexing a
/// negative "size" would be nonsense regardless of the byte data behind it.
///
/// Pure function; unit-tested below.
fn pick_pixmap(pixmaps: &[(i32, i32, Vec<u8>)], target: i32) -> Option<&(i32, i32, Vec<u8>)> {
    pixmaps
        .iter()
        .filter(|(width, height, _)| *width > 0 && *height > 0)
        .min_by_key(|(width, _, _)| ((*width - target).abs(), std::cmp::Reverse(*width)))
}

/// The wire → iced byte shuffle for one `IconPixmap` entry.
///
/// # Teaching note: why this shuffle exists at all
///
/// SNI's `IconPixmap` is specified as ARGB32 **in network byte order**
/// (network byte order is always big-endian). A 32-bit big-endian
/// `0xAARRGGBB` value, read off the wire one byte at a time, arrives as the
/// four bytes `[A, R, G, B]` — alpha *first*. iced's
/// `image::Handle::from_rgba` wants the opposite convention per pixel:
/// `[R, G, B, A]`, alpha *last* (the ordinary "RGBA8" convention almost
/// every raster API, from PNG to WebGPU, shares). So every 4-byte pixel
/// needs its bytes rotated: `[A, R, G, B]` becomes `[R, G, B, A]`. That
/// rotation is the entirety of what this function does — it is a byte
/// reordering, never a color-space conversion.
///
/// `None` when `data.len()` isn't exactly `width * height * 4` — a
/// malformed pixmap from a misbehaving item, treated as "no icon this way"
/// rather than trusted to index safely.
///
/// Pure function; unit-tested below on a byte-level fixture.
fn argb_network_to_rgba(width: i32, height: i32, data: &[u8]) -> Option<Vec<u8>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if data.len() != expected {
        return None;
    }

    let mut rgba = Vec::with_capacity(data.len());
    for pixel in data.chunks_exact(4) {
        // pixel[0..4] is [A, R, G, B] (network byte order); iced wants
        // [R, G, B, A].
        rgba.extend_from_slice(&[pixel[1], pixel[2], pixel[3], pixel[0]]);
    }
    Some(rgba)
}

/// One command sent to an item over a fresh, one-shot connection — the
/// same "command-out" pattern `media.rs` established in Stage 17 (see that
/// module's doc comment): connect, make one call, let the connection drop.
/// Reusing the tray worker's own connection isn't an option for the same
/// reason it wasn't for media: that connection lives inside the
/// subscription's stream, unreachable from `Panel::update`.
enum ItemCommand {
    Activate,
    Scroll {
        delta: i32,
        orientation: &'static str,
    },
}

/// Connects to the session bus, sends one command to `id`'s item, and lets
/// the connection drop. Failures *reaching* the item — a malformed id, no
/// session bus, a proxy that can't be built — are swallowed as `None`
/// (there is nothing for anyone to do with them), while the call's own
/// error, if any, is handed back for the caller to interpret:
/// [`send_scroll`] just logs it, but [`send_activate`] reads it to
/// recognise the menu-only-item case — the same "best-effort" contract as
/// `media.rs`'s `send_player_command`, minus the swallowing where the
/// caller genuinely can do something with the answer.
async fn send_item_command(id: String, command: ItemCommand) -> Option<zbus::Error> {
    let address = parse_registration(&id)?;
    let connection = Connection::session().await.ok()?;
    let proxy = build_item_proxy(&connection, &address).await?;
    let result = match command {
        // Screen coordinates: always `(0, 0)`. A layer-shell surface has no
        // API for "where on the screen was this click" (unlike an X11/xdg
        // toplevel, wlr-layer-shell never hands a client global screen
        // coordinates), so there is nothing honest to put here — `(0, 0)`
        // is what every Wayland status bar sends, and well-behaved items
        // treat it as "wherever you like", not literally the top-left
        // corner.
        ItemCommand::Activate => proxy.activate(0, 0).await,
        ItemCommand::Scroll { delta, orientation } => proxy.scroll(delta, orientation).await,
    };
    result.err()
}

/// Left-click — SNI's primary interaction. See [`ItemCommand::Activate`]'s
/// call site above for why the coordinates are always `(0, 0)`.
///
/// Returns **true when the item turned out to be menu-only**: an
/// `UnknownMethod` reply means the item exports no `Activate` at all
/// (libdbusmenu-based items never do — see the proxy's `item_is_menu` doc
/// comment), which is an *answer*, not a failure — the caller falls back to
/// opening the item's menu instead, so it is deliberately not logged.
/// Every other error is logged and swallowed as before.
pub(super) async fn send_activate(id: String) -> bool {
    match send_item_command(id, ItemCommand::Activate).await {
        None => false,
        Some(error) if is_unknown_method(&error) => true,
        Some(error) => {
            eprintln!("saola-panel: tray: item command failed: {error}");
            false
        }
    }
}

/// Whether a call failed because the method doesn't exist on the object —
/// the bus's standard `org.freedesktop.DBus.Error.UnknownMethod` reply.
/// Matched by error *name*, not variant alone: `zbus::Error::MethodError`
/// carries every named error reply there is (polkit denials,
/// application-defined errors, ...), and only this one means "this item
/// has no such control".
fn is_unknown_method(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
    )
}

/// A scroll over the item. `delta`/`orientation` are already reduced from
/// iced's `mouse::ScrollDelta` by `modules::tray::scroll_units`.
pub(super) async fn send_scroll(id: String, delta: i32, orientation: &'static str) {
    if let Some(error) = send_item_command(id, ItemCommand::Scroll { delta, orientation }).await {
        eprintln!("saola-panel: tray: item command failed: {error}");
    }
}

/// One item's change-notification stream, reduced to just its id — the
/// signal-level counterpart of [`read_item`]'s one-shot read. Merges
/// `NewIcon`/`NewStatus`/`NewTitle`: whichever fires, the id comes out, and
/// `watcher.rs`'s worker responds by calling [`read_item`] again — a full
/// re-read rather than trying to apply each signal's payload individually,
/// the same "just re-read everything" simplicity `battery.rs`'s
/// `watch_upower` already uses for its own property-changed streams.
///
/// **This function is `async` and must be fully `.await`ed — subscribed —
/// before the caller does anything else with this item, never wrapped in a
/// lazily-polled stream.** An earlier draft of this function returned a
/// stream that deferred the three `receive_new_*()` subscribe calls (each a
/// real `AddMatch` round-trip to the bus daemon) until the stream's first
/// poll, on the theory that `stream::SelectAll` would poll a freshly-pushed
/// member promptly. It does — but "promptly" is not "before the very next
/// thing the caller does": `watcher.rs`'s live-bus test hit this exactly:
/// a registration handled, an item mutated and a `NewTitle` emitted
/// immediately after, and that signal sailing past because this function's
/// `AddMatch` for `NewTitle` hadn't landed on the daemon yet. The fix is the
/// same principle `run_session` already applies to the foreign-watcher item
/// list ("signals before the list read, so an item that registers
/// mid-read isn't lost"): subscribing is awaited to completion — meaning
/// the `AddMatch` replies have actually come back — before the caller
/// (`watcher.rs::watch_item_for_changes`) moves on to reading the item's
/// current properties. A real application changing its icon a heartbeat
/// after registering is a real scenario, not just a test artifact.
///
/// Takes an owned [`Connection`] (cheap to clone — an `Arc` internally,
/// same as every other per-item worker in this codebase, e.g.
/// `media.rs`'s `player_stream`) rather than a borrow, because the returned
/// stream must be `'static`: it lives inside `watcher.rs`'s `SelectAll` for
/// as long as the session runs, well past this function returning.
///
/// `None`/empty-stream outcomes (a malformed id, a proxy that can't be
/// built, a signal subscription that fails) all degrade to "never fires
/// again" rather than a panic or a session restart — a real (if
/// occasionally stale) tray beats losing every item over one item's D-Bus
/// hiccup.
pub(super) async fn watch_item(connection: Connection, id: String) -> BoxStream<'static, String> {
    let Some(address) = parse_registration(&id) else {
        return stream::empty().boxed();
    };
    let Some(proxy) = build_item_proxy(&connection, &address).await else {
        return stream::empty().boxed();
    };

    // The three subscriptions below are made *through* this proxy, but —
    // verified against zbus 5's source (`SignalStream::new`,
    // `proxy/mod.rs`) — each returned `SignalStream` owns its own
    // independent match rule via a freshly-built `MessageStream`, not a
    // borrow of the proxy's own D-Bus registration. So the proxy can be
    // (and here, is) dropped the moment these three calls return; nothing
    // about the streams below depends on it surviving.
    //
    // Contrast `media.rs`'s `PlayerWatch::Watching`, which *does* keep its
    // proxy alive alongside its change stream — there, it's needed
    // afterwards to re-read `PlaybackStatus`/`Metadata` once the stream
    // fires. Here, a change is handled by `watcher.rs` calling
    // [`read_item`] fresh (which builds its own proxy), so nothing in this
    // function ever reads from `proxy` again after this point.
    let (new_icon, new_status, new_title) = match (
        proxy.receive_new_icon().await,
        proxy.receive_new_status().await,
        proxy.receive_new_title().await,
    ) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        _ => return stream::empty().boxed(),
    };

    let (id_a, id_b, id_c) = (id.clone(), id.clone(), id);
    stream::select(
        new_icon.map(move |_| id_a.clone()),
        stream::select(
            new_status.map(move |_| id_b.clone()),
            new_title.map(move |_| id_c.clone()),
        ),
    )
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_bus_name_registers_at_the_default_path() {
        let address = parse_registration(":1.42").expect("a unique name is a valid registration");
        assert_eq!(address.bus(), ":1.42");
        assert_eq!(address.path(), "/StatusNotifierItem");

        let address = parse_registration("org.kde.StatusNotifierItem-1234-1")
            .expect("a well-known name is a valid registration");
        assert_eq!(address.bus(), "org.kde.StatusNotifierItem-1234-1");
        assert_eq!(address.path(), "/StatusNotifierItem");
    }

    #[test]
    fn a_bus_name_and_path_split_at_the_first_slash() {
        let address = parse_registration(":1.42/StatusNotifierItem").expect("valid");
        assert_eq!(address.bus(), ":1.42");
        assert_eq!(address.path(), "/StatusNotifierItem");

        // The path half keeps every slash after the first one.
        let address =
            parse_registration(":1.42/org/ayatana/NotificationItem/nm_applet").expect("valid");
        assert_eq!(address.bus(), ":1.42");
        assert_eq!(address.path(), "/org/ayatana/NotificationItem/nm_applet");
    }

    #[test]
    fn a_registration_that_names_no_bus_is_rejected() {
        // A bare object path is only meaningful next to a message sender —
        // `registration_id` is where that form is resolved, not here.
        assert_eq!(parse_registration("/StatusNotifierItem"), None);
        assert_eq!(parse_registration(""), None);
        assert_eq!(parse_registration("   "), None);
    }

    #[test]
    fn the_watcher_normalizes_a_bare_bus_name_to_bus_plus_default_path() {
        assert_eq!(
            registration_id(":1.42", Some(":1.42")).as_deref(),
            Some(":1.42/StatusNotifierItem")
        );
        // The sender is irrelevant in this form — apps that pass a
        // well-known name are registering that name, not themselves.
        assert_eq!(
            registration_id("org.kde.StatusNotifierItem-1234-1", Some(":1.42")).as_deref(),
            Some("org.kde.StatusNotifierItem-1234-1/StatusNotifierItem")
        );
    }

    #[test]
    fn the_watcher_resolves_a_bare_path_against_the_message_sender() {
        assert_eq!(
            registration_id("/org/ayatana/NotificationItem/nm_applet", Some(":1.42")).as_deref(),
            Some(":1.42/org/ayatana/NotificationItem/nm_applet")
        );
        // No sender: nothing to resolve it against, so it is refused rather
        // than guessed at.
        assert_eq!(
            registration_id("/org/ayatana/NotificationItem/nm_applet", None),
            None
        );
        assert_eq!(registration_id("", Some(":1.42")), None);
    }

    #[test]
    fn normalizing_is_idempotent_over_an_already_concatenated_string() {
        // Feeding a watcher its own output back must not glue a second
        // `/StatusNotifierItem` onto the end.
        let once = registration_id(":1.42", Some(":1.42")).expect("valid");
        let twice = registration_id(&once, Some(":1.42")).expect("valid");
        assert_eq!(once, twice);

        // And the two halves of the quirk round-trip: what the watcher
        // writes is exactly what a host can split back apart.
        let address = parse_registration(&once).expect("valid");
        assert_eq!(address.bus(), ":1.42");
        assert_eq!(address.path(), "/StatusNotifierItem");
    }

    #[test]
    fn the_label_prefers_title_then_id_then_the_bus_name() {
        assert_eq!(item_label("Slack", "slack", ":1.42"), "Slack");
        assert_eq!(item_label("", "nm-applet", ":1.42"), "nm-applet");
        assert_eq!(item_label("  ", "  ", ":1.42"), ":1.42");
        // Whitespace around a real value is trimmed, not treated as content.
        assert_eq!(item_label(" Slack ", "slack", ":1.42"), "Slack");
    }

    /// Fixture: an item keyed by `id`, with its address derived from that
    /// same string exactly the way the worker derives it. No icon, `Active`
    /// status, not menu-only — the registry tests below only care about
    /// label/id/address bookkeeping, not the Stage 19/22 fields.
    fn item(id: &str, label: &str) -> TrayItem {
        TrayItem::new(
            id.to_string(),
            parse_registration(id).expect("test ids parse"),
            label.to_string(),
            None,
            ItemStatus::default(),
            false,
        )
    }

    #[test]
    fn the_registry_keeps_registration_order_and_replaces_in_place() {
        let mut registry = ItemRegistry::default();
        registry.upsert(item(":1.1/StatusNotifierItem", "One"));
        registry.upsert(item(":1.2/StatusNotifierItem", "Two"));
        registry.upsert(item(":1.1/StatusNotifierItem", "One, renamed"));

        let snapshot = registry.snapshot();
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[0].label(), "One, renamed");
        assert_eq!(snapshot.items[1].label(), "Two");
        assert_eq!(snapshot.items[0].id(), ":1.1/StatusNotifierItem");
    }

    #[test]
    fn the_registry_removes_by_id_and_by_owning_bus_name() {
        let mut registry = ItemRegistry::default();
        registry.upsert(item(":1.1/StatusNotifierItem", "One"));
        registry.upsert(item(":1.1/org/ayatana/NotificationItem/two", "Two"));
        registry.upsert(item(":1.2/StatusNotifierItem", "Three"));

        assert!(registry.remove(":1.2/StatusNotifierItem"));
        assert!(!registry.remove(":1.2/StatusNotifierItem"));

        // One app owning two items loses both when its bus name dies.
        let removed = registry.remove_owned_by(":1.1");
        assert_eq!(
            removed,
            vec![
                ":1.1/StatusNotifierItem".to_string(),
                ":1.1/org/ayatana/NotificationItem/two".to_string(),
            ]
        );
        assert!(registry.snapshot().items.is_empty());

        // A bus name nobody registered under removes nothing.
        assert!(registry.remove_owned_by(":1.9").is_empty());
    }

    #[test]
    fn a_real_menu_path_is_kept_and_the_no_menu_sentinels_are_not() {
        assert_eq!(
            usable_menu_path("/StatusNotifierMenu"),
            Some("/StatusNotifierMenu")
        );
        assert_eq!(
            usable_menu_path("/org/ayatana/NotificationItem/foo/Menu"),
            Some("/org/ayatana/NotificationItem/foo/Menu")
        );

        // KDE's own "there is no menu" answer, and the two degenerate ones.
        assert_eq!(usable_menu_path("/NO_DBUSMENU"), None);
        assert_eq!(usable_menu_path("/"), None);
        assert_eq!(usable_menu_path(""), None);
        assert_eq!(usable_menu_path("   "), None);
    }

    #[test]
    fn the_menu_object_lives_on_the_items_own_bus_name() {
        // The pairing `read_menu_address` builds: the item's bus, the menu's
        // path. Getting this backwards would send every menu call to the
        // wrong destination.
        let address = parse_registration(":1.42/StatusNotifierItem").expect("valid");
        let menu = address.at_path("/StatusNotifierMenu".to_string());
        assert_eq!(menu.bus(), ":1.42");
        assert_eq!(menu.path(), "/StatusNotifierMenu");
        // ...and the item's own address is untouched.
        assert_eq!(address.path(), "/StatusNotifierItem");
    }

    #[test]
    fn sni_status_strings_map_to_the_three_states() {
        assert_eq!(ItemStatus::from_sni("Passive"), ItemStatus::Passive);
        assert_eq!(ItemStatus::from_sni("Active"), ItemStatus::Active);
        assert_eq!(
            ItemStatus::from_sni("NeedsAttention"),
            ItemStatus::NeedsAttention
        );
    }

    #[test]
    fn an_unrecognized_or_missing_status_defaults_to_active() {
        // Covers both "the item doesn't implement `Status` at all" (an
        // empty string is what `read_item` sees in that case) and a future
        // spec value / nonconformant item.
        assert_eq!(ItemStatus::from_sni(""), ItemStatus::Active);
        assert_eq!(ItemStatus::from_sni("passive"), ItemStatus::Active);
        assert_eq!(ItemStatus::default(), ItemStatus::Active);
    }

    /// The byte-level fixture PLAN.md Stage 19 asks for: one pixel, chosen
    /// so every one of the four bytes is distinct and none of them are
    /// equal to their shuffled position's value — a shuffle that
    /// accidentally left a byte in place would still pass a fixture with
    /// repeated bytes.
    #[test]
    fn argb_network_order_shuffles_into_rgba() {
        // A=0x11, R=0x22, G=0x33, B=0x44, network (big-endian) byte order.
        let data = [0x11, 0x22, 0x33, 0x44];
        let rgba = argb_network_to_rgba(1, 1, &data).expect("a well-formed 1x1 pixmap");
        assert_eq!(rgba, vec![0x22, 0x33, 0x44, 0x11]);
    }

    #[test]
    fn multiple_pixels_shuffle_independently() {
        let data = [
            0x11, 0x22, 0x33, 0x44, // pixel 1: A R G B
            0xAA, 0xBB, 0xCC, 0xDD, // pixel 2: A R G B
        ];
        let rgba = argb_network_to_rgba(2, 1, &data).expect("a well-formed 2x1 pixmap");
        assert_eq!(rgba, vec![0x22, 0x33, 0x44, 0x11, 0xBB, 0xCC, 0xDD, 0xAA]);
    }

    #[test]
    fn a_pixmap_with_the_wrong_byte_count_is_rejected() {
        // 2x2 needs 16 bytes; only 10 given.
        assert_eq!(argb_network_to_rgba(2, 2, &[0u8; 10]), None);
    }

    #[test]
    fn a_pixmap_with_a_non_positive_dimension_is_rejected() {
        assert_eq!(argb_network_to_rgba(0, 4, &[0u8; 16]), None);
        assert_eq!(argb_network_to_rgba(4, -1, &[0u8; 16]), None);
    }

    #[test]
    fn pick_pixmap_prefers_the_closest_size() {
        let pixmaps = vec![
            (16, 16, Vec::new()),
            (48, 48, Vec::new()),
            (24, 24, Vec::new()),
        ];
        let (width, _, _) = pick_pixmap(&pixmaps, 24).expect("a candidate");
        assert_eq!(*width, 24);
    }

    #[test]
    fn pick_pixmap_prefers_the_larger_size_on_a_tie() {
        // 16 and 32 are both 8 away from 24 — the larger one wins because
        // it downscales cleanly, unlike upscaling the smaller one.
        let pixmaps = vec![(16, 16, Vec::new()), (32, 32, Vec::new())];
        let (width, _, _) = pick_pixmap(&pixmaps, 24).expect("a candidate");
        assert_eq!(*width, 32);
    }

    #[test]
    fn pick_pixmap_ignores_degenerate_entries() {
        let pixmaps = vec![(0, 0, Vec::new()), (-1, 10, Vec::new())];
        assert!(pick_pixmap(&pixmaps, 24).is_none());
    }

    #[test]
    fn pick_pixmap_over_an_empty_list_yields_nothing() {
        assert!(pick_pixmap(&Vec::new(), 24).is_none());
    }

    #[test]
    fn pixmap_icon_shuffles_the_chosen_entry_into_an_rgba_image_handle() {
        let pixmaps = vec![(1_i32, 1_i32, vec![0x11u8, 0x22, 0x33, 0x44])];
        let icon = pixmap_icon(&pixmaps).expect("a well-formed single pixmap");
        let TrayIcon::Pixmap(handle) = icon else {
            panic!("pixmap_icon must build the Pixmap variant");
        };
        // `image::Handle::Rgba`'s fields are public specifically so callers
        // (and this test) can inspect a decoded handle without a renderer.
        let iced::widget::image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = handle
        else {
            panic!("from_rgba always builds the Rgba variant");
        };
        assert_eq!((width, height), (1, 1));
        assert_eq!(&pixels[..], &[0x22, 0x33, 0x44, 0x11]);
    }

    #[test]
    fn pixmap_icon_over_no_usable_entries_is_none() {
        assert!(pixmap_icon(&[]).is_none());
        // A malformed entry (wrong byte count) is the only entry offered.
        assert!(pixmap_icon(&[(2, 2, vec![0u8; 3])]).is_none());
    }
}
