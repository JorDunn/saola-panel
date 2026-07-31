//! `com.canonical.dbusmenu` — a tray item's context menu, decoded into a
//! plain [`MenuNode`] tree.
//!
//! **There is no UI in this file** (PLAN.md Stage 20: "model only"). Stage 21
//! renders exactly the model below inside a popover and wires the clicks; the
//! whole point of splitting the two is that everything here is provable
//! headlessly, with fixtures, which is what the test module at the bottom
//! does.
//!
//! # The protocol in one paragraph
//!
//! An SNI item that has a context menu publishes a *second* object — on the
//! same bus name, at the object path its `Menu` property names — implementing
//! `com.canonical.dbusmenu`. That interface is not SNI's; it is Canonical's
//! generic "expose a menu over D-Bus" protocol (the one behind Unity's global
//! menu), and it is what every tray implementation in the wild actually uses
//! for right-click menus. A menu is a tree of items, each with a numeric `id`
//! and a **dictionary of properties**. `GetLayout` returns the tree,
//! `AboutToShow` warns the application it is about to be displayed, and
//! `Event` reports a click back.
//!
//! # The recursive variant shape (the part that is genuinely awkward)
//!
//! `GetLayout(parentId: i, recursionDepth: i, propertyNames: as)` replies with
//! `(u, (ia{sv}av))` — a revision number and one *layout node*. The node is
//! the struct `(ia{sv}av)`:
//!
//! ```text
//! (
//!     i        the item id (0 is the root, which is the menu itself and is
//!              never drawn — its children are the menu's rows)
//!     a{sv}    that item's properties
//!     av       its children, each variant holding another (ia{sv}av)
//! )
//! ```
//!
//! The recursion is the third field: an *array of variants*, where every
//! variant's payload is the same struct again. D-Bus has no recursive type,
//! so the protocol smuggles the recursion through `v`. In zvariant that means
//! [`RawMenuNode`] cannot simply have `children: Vec<RawMenuNode>` — the
//! declared type has to be `Vec<OwnedValue>`, and each element is turned back
//! into a `RawMenuNode` by hand (see [`menu_node`]). `#[derive(Value,
//! OwnedValue)]` is what makes that per-element conversion a `try_from` rather
//! than a hand-written `Structure` field walk.
//!
//! `recursionDepth` controls how deep the reply goes: `-1` is "everything",
//! `0` is "no children at all", `n` is "n levels". This module always asks for
//! `-1` — a tray menu is a handful of rows, one round-trip beats lazily
//! fetching submenus on hover, and the alternative (depth 1 plus a fetch per
//! submenu) is the thing that makes other bars' tray menus feel laggy.
//!
//! # Properties are **omitted at their defaults**
//!
//! The spec is explicit: "To reduce the amount of DBus traffic, a property
//! should only be returned if its value is not the default value." So an
//! absent key is *not* an error and *not* "unknown" — it is a value, and the
//! parser has to know the table:
//!
//! | property           | type   | default      | this module's field                    |
//! |--------------------|--------|--------------|----------------------------------------|
//! | `type`             | string | `"standard"` | [`MenuNode::kind`]                     |
//! | `label`            | string | `""`         | [`MenuNode::label`] (mnemonics stripped) |
//! | `enabled`          | bool   | `true`       | [`MenuNode::enabled`]                  |
//! | `visible`          | bool   | `true`       | [`MenuNode::visible`]                  |
//! | `icon-name`        | string | `""`         | [`MenuNode::icon_name`]                |
//! | `icon-data`        | binary | empty        | *not decoded* — see below              |
//! | `shortcut`         | `aas`  | empty        | *not decoded* — see below              |
//! | `toggle-type`      | string | `""`         | [`MenuNode::toggle_type`]              |
//! | `toggle-state`     | int    | `-1`         | [`MenuNode::toggle_state`]             |
//! | `children-display` | string | `""`         | [`MenuNode::has_submenu`]              |
//!
//! [`MenuNode::default()`] **is** that table — every field's default is the
//! spec's default value — and [`menu_node`] parses by starting from it and
//! overriding only the keys the application actually sent. That is not a
//! stylistic choice: it is the only shape in which "the defaults" can't drift
//! away from "what the parser fills in", and there is a test asserting the
//! default node matches the table row-for-row.
//!
//! `icon-data` (a PNG blob) and `shortcut` are deliberately not decoded:
//! nothing in the panel's menu design shows either, and CLAUDE.md's "don't
//! build speculatively" applies. Adding them later is a field plus a lookup;
//! the shape above is what makes that cheap.
//!
//! # Mnemonics
//!
//! `label` is not display text. The spec's rules, verbatim: two consecutive
//! underscores display as a single underscore; any remaining underscore is not
//! displayed at all; the first such remaining underscore marks the following
//! character as the access key. [`strip_mnemonics`] implements the first two
//! (which is all that "what does this row say" needs). The access key itself
//! is dropped rather than recorded — the panel has no keyboard menu
//! navigation, and inventing a field for it now would be exactly the
//! speculative build CLAUDE.md warns off.
//!
//! # `AboutToShow` semantics
//!
//! `AboutToShow(id)` tells the application "I am about to display the menu
//! under `id`" and replies `needUpdate`. Its real purpose is applications that
//! populate lazily — an item whose submenu is empty until somebody asks. So
//! it must be called **before** `GetLayout`, not after, and [`read_menu`] does
//! exactly that, in that order, which is also why the reply's `needUpdate`
//! flag is ignored here: we are going to read the layout in the very next
//! call regardless, so "you should re-read" is already true. A failure is
//! swallowed rather than aborting the read — plenty of implementations don't
//! bother with the method, and a menu that exists is worth showing even if the
//! application declined to be warned about it.

use std::collections::HashMap;

use iced::futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedValue, Type, Value};
use zbus::Connection;

use super::item::{self, ItemAddress};

/// Property keys, spelled once. All lowercase-with-hyphens, exactly as the
/// spec writes them — an application that sends `"toggleType"` is simply
/// sending an unknown key, and unknown keys are ignored (they may be a
/// `x-<vendor>-` extension, which the spec explicitly allows).
const PROP_TYPE: &str = "type";
const PROP_LABEL: &str = "label";
const PROP_ENABLED: &str = "enabled";
const PROP_VISIBLE: &str = "visible";
const PROP_ICON_NAME: &str = "icon-name";
const PROP_TOGGLE_TYPE: &str = "toggle-type";
const PROP_TOGGLE_STATE: &str = "toggle-state";
const PROP_CHILDREN_DISPLAY: &str = "children-display";

/// The `type` value that means "a divider line, not a row".
const TYPE_SEPARATOR: &str = "separator";
/// The `children-display` value that means "this row opens a submenu".
const CHILDREN_DISPLAY_SUBMENU: &str = "submenu";

/// How deep the parser will follow `children` before giving up.
///
/// Not a spec limit — a stack-overflow guard. The wire format nests through
/// `av`, so a single (large but legal) reply can describe a menu thousands of
/// levels deep, and [`menu_node`] is recursive; without a cap, a buggy or
/// hostile application could take the whole panel down with one D-Bus reply.
/// Real menus are two or three levels; sixteen is far past anything a human
/// would build and far short of anything that could exhaust the stack.
const MAX_MENU_DEPTH: usize = 16;

/// The event name `Event` wants for "the user clicked this row". The spec
/// defines exactly two (`"clicked"` and `"hovered"`); this panel sends only
/// the first — a hover event on a menu row conveys nothing the application
/// can act on that a click doesn't, and sending them would mean a D-Bus
/// message per pointer move.
const EVENT_CLICKED: &str = "clicked";

/// The `timestamp` argument of `Event`, always zero.
///
/// The spec asks for "the time that the event occurred if available" — on
/// X11 that is the server's event timestamp, which toolkits use for
/// focus-stealing prevention. Wayland hands a layer-shell client no
/// equivalent global clock, so there is nothing honest to put here, and
/// zero (the conventional "no timestamp available") is what every Wayland bar
/// sends. Same reasoning, and the same conclusion, as the `(0, 0)` screen
/// coordinates `item::send_activate` passes to `Activate`.
const EVENT_TIMESTAMP: u32 = 0;

/// One layout node exactly as it arrives on the wire: `(ia{sv}av)`.
///
/// Teaching note (**why `children` is `Vec<OwnedValue>` and not
/// `Vec<RawMenuNode>`**): D-Bus's type system has no recursion, so dbusmenu
/// expresses "a node contains nodes" by declaring the children as an array of
/// *variants* — `av` — and putting another `(ia{sv}av)` inside each one. A
/// `Vec<RawMenuNode>` field would generate the signature `a(ia{sv}av)`, which
/// is a different (and wrong) type, and deserialization would fail outright.
/// The variant has to stay a variant in the declared type, and the recursion
/// happens in [`menu_node`] instead.
///
/// The four derives each earn their place: `Type` + `Deserialize` are what
/// the proxy's `GetLayout` return type needs; `Serialize` is what lets the
/// live-bus test in `watcher.rs` *serve* a menu; `Value`/`OwnedValue`
/// generate the `TryFrom` impls that turn one child variant back into a
/// `RawMenuNode` (and, in that same test, a `RawMenuNode` back into a child
/// variant).
#[derive(Debug, Serialize, Deserialize, Type, Value, OwnedValue)]
pub(super) struct RawMenuNode {
    pub(super) id: i32,
    pub(super) properties: HashMap<String, OwnedValue>,
    pub(super) children: Vec<OwnedValue>,
}

/// A zbus proxy for one item's `com.canonical.dbusmenu` object.
///
/// Like `item.rs`'s `StatusNotifierItemProxy` this has no `default_service`
/// or `default_path`: the bus name comes from the item's registration string
/// and the object path from the item's own `Menu` property, so both are
/// runtime values and the generated constructor is the builder.
///
/// Caching is off for the same reason it is off on every other proxy in this
/// module (see that proxy's doc comment): dbusmenu has its own
/// `LayoutUpdated`/`ItemsPropertiesUpdated` signals and implementations do not
/// reliably emit `org.freedesktop.DBus.Properties.PropertiesChanged`, so a
/// cached proxy would serve the first `Status` it ever read forever. It also
/// avoids the upfront `GetAll`, which matters more here than usual: this proxy
/// is built, used once, and dropped.
#[zbus::proxy(interface = "com.canonical.dbusmenu")]
trait DBusMenu {
    /// The whole tree. See the module doc comment for the reply's shape and
    /// for why `recursion_depth` is always `-1` here.
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, RawMenuNode)>;

    /// "I am about to show the menu under `id`." Replies whether the layout
    /// should be re-read; see the module doc comment for why this module
    /// ignores that answer.
    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;

    /// Report an interaction. `event_id` is [`EVENT_CLICKED`] for every call
    /// this panel makes; `data` is event-specific and unused for a click
    /// (an empty string is the conventional filler); `timestamp` is
    /// [`EVENT_TIMESTAMP`].
    ///
    /// Teaching note (**a reply is expected, deliberately**): some copies of
    /// the interface XML in the wild annotate `Event` with
    /// `org.freedesktop.DBus.Method.NoReply`. That annotation only ever
    /// affects generated *proxies* — it tells them to set the message's
    /// `NO_REPLY_EXPECTED` flag — and setting it would mean this panel could
    /// never tell a delivered click from one that vanished. Every real
    /// dbusmenu client (Qt's importer, GNOME Shell's appindicator extension)
    /// waits for the reply too, and every real server sends one. So: plain
    /// method, error logged by the caller.
    fn event(&self, id: i32, event_id: &str, data: &Value<'_>, timestamp: u32) -> zbus::Result<()>;

    /// The layout changed structurally. `parent` is the subtree that changed,
    /// or 0 for "the whole thing". [`watch_menu`] ignores both arguments and
    /// re-reads everything — see its doc comment.
    #[zbus(signal)]
    fn layout_updated(&self, revision: u32, parent: i32);

    /// Properties changed on some set of items without the structure moving —
    /// the cheap update path an application uses to, say, tick a checkbox.
    /// The arguments carry the deltas; [`watch_menu`] ignores them and
    /// re-reads, for the same reason.
    #[zbus(signal)]
    fn items_properties_updated(
        &self,
        updated: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    );
}

/// `type`, reduced to the two values that mean anything to a renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MenuItemKind {
    /// A row: clickable, or the parent of a submenu. The spec's default.
    #[default]
    Standard,
    /// A divider. Has no label, is never clickable, and (per the spec) is the
    /// only other `type` value defined.
    Separator,
}

impl MenuItemKind {
    /// Anything that isn't exactly `"separator"` is a row.
    ///
    /// That includes the `x-<vendor>-` extension types the spec allows: a
    /// renderer that doesn't know a vendor type still has a label and an id
    /// to work with, so drawing it as an ordinary row degrades far better
    /// than dropping it. Pure function; unit-tested below.
    fn from_wire(value: &str) -> Self {
        if value == TYPE_SEPARATOR {
            Self::Separator
        } else {
            Self::Standard
        }
    }
}

/// `toggle-type`: whether the row carries a check/radio mark at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ToggleType {
    /// `""` — an ordinary row. The spec's default.
    #[default]
    None,
    /// `"checkmark"` — an independent on/off row.
    Checkmark,
    /// `"radio"` — one of a group where only one is on. Note the spec's own
    /// warning: nothing in the protocol enforces that, so a renderer must not
    /// assume exactly one radio row in a group is on.
    Radio,
}

impl ToggleType {
    /// Pure function; unit-tested below. An unrecognized value is treated as
    /// "no toggle" rather than guessed at — a mark the panel invented is a
    /// worse lie than no mark.
    fn from_wire(value: &str) -> Self {
        match value {
            "checkmark" => Self::Checkmark,
            "radio" => Self::Radio,
            _ => Self::None,
        }
    }
}

/// `toggle-state`: whether the mark is filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ToggleState {
    /// `0`.
    Off,
    /// `1`.
    On,
    /// The spec's `-1` default, **and** every other integer: "anything else =
    /// indeterminate", verbatim. A row that never mentions `toggle-state`
    /// lands here, which is why this — not `Off` — is the `Default`.
    #[default]
    Indeterminate,
}

impl ToggleState {
    /// Pure function; unit-tested below, including the "anything else" arm.
    fn from_wire(value: i32) -> Self {
        match value {
            0 => Self::Off,
            1 => Self::On,
            _ => Self::Indeterminate,
        }
    }
}

/// One decoded menu row (or the root, or a separator) — **the model Stage 21
/// renders**.
///
/// Plain `pub(super)` fields rather than accessors, unlike `item::TrayItem`:
/// this is a data tree whose whole job is to be matched on and read by a view
/// function, it has no invariant between fields to protect, and every field is
/// exactly one protocol property. `TrayItem` has accessors because its fields
/// are derived (a label picked from three candidates, an icon resolved through
/// a four-step precedence) and the derivation is the thing worth hiding; here
/// there is nothing to hide.
///
/// [`Self::default()`] is the spec's property-default table — see the module
/// doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuNode {
    /// The dbusmenu item id — what [`send_clicked`] sends back, and what
    /// [`read_menu`]'s `parent` argument takes. Ids are per-menu and are not
    /// stable across a `LayoutUpdated`.
    pub(crate) id: i32,
    /// Row or separator.
    pub(crate) kind: MenuItemKind,
    /// Display text, **already stripped of mnemonic underscores** (see
    /// [`strip_mnemonics`]). A separator's is normally empty.
    pub(crate) label: String,
    /// False for a row that is shown greyed out and must not be clickable.
    pub(crate) enabled: bool,
    /// False for a row that must not be drawn at all. Kept in the model
    /// rather than filtered out during parsing on purpose: the model is the
    /// application's answer, and dropping rows is a rendering decision —
    /// `popovers::tray_menu::rows_to_draw` is where Stage 21 makes it (see
    /// that function for the leading/trailing/doubled-separator rules that
    /// come along with it).
    pub(crate) visible: bool,
    /// A freedesktop icon name, undecoded — the same kind of string
    /// `item::resolve_icon` feeds to the theme lookup. Empty for the
    /// overwhelming majority of rows. **Not yet rendered** — see the Stage 21
    /// handoff's flagged gaps.
    pub(crate) icon_name: String,
    /// Whether the row carries a check/radio mark.
    pub(crate) toggle_type: ToggleType,
    /// Whether that mark is filled in. Meaningful only when
    /// [`Self::toggle_type`] isn't [`ToggleType::None`], but decoded
    /// unconditionally because the wire property is independent of it — and
    /// real applications lean on that: every row of the captured tailscale
    /// menu in this module's tests sends `toggle-state: 0` despite none of
    /// them being togglable.
    pub(crate) toggle_state: ToggleState,
    /// Whether the row opens a submenu (`children-display == "submenu"`).
    /// Independent of [`Self::children`] being non-empty — see that field.
    pub(crate) has_submenu: bool,
    /// Decoded children, in wire order. Non-empty only when the application
    /// sent children *and* the depth cap wasn't hit; note that a row can
    /// legitimately declare [`Self::has_submenu`] with an empty `children`
    /// list, which is precisely the lazily-populated case `AboutToShow`
    /// exists for — `popovers::tray_menu` re-calls [`read_menu`] for exactly
    /// that row when Stage 21's inline expansion hits it.
    pub(crate) children: Vec<MenuNode>,
}

impl Default for MenuNode {
    /// **The spec's property-default table, as code.** Every value here is
    /// the "Default Value" column of the table in the module doc comment;
    /// [`menu_node`] starts from this and overrides only what the application
    /// actually sent, which is what makes "properties are omitted at their
    /// defaults" a one-line consequence rather than ten scattered
    /// `unwrap_or`s. There is a test pinning each field to its table row.
    fn default() -> Self {
        Self {
            id: 0,
            kind: MenuItemKind::Standard,
            label: String::new(),
            enabled: true,
            visible: true,
            icon_name: String::new(),
            toggle_type: ToggleType::None,
            toggle_state: ToggleState::Indeterminate,
            has_submenu: false,
            // Not a spec property: "no children" is simply the shape of a
            // node whose `av` was empty.
            children: Vec::new(),
        }
    }
}

/// A whole menu: one `GetLayout` reply, decoded.
///
/// `root` is the node whose id was passed as `parentId` — id 0 for a whole
/// menu. **The root itself is never drawn**: it is the menu container, its
/// properties are conventionally empty, and its `children` are the rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Menu {
    /// The layout revision this tree came from — the same counter
    /// `LayoutUpdated` reports. Kept so a future refresh can tell a stale
    /// tree from a current one; nothing compares it yet.
    pub(crate) revision: u32,
    /// The container node; its [`MenuNode::children`] are the rows.
    pub(crate) root: MenuNode,
}

/// Turn a dbusmenu `label` into display text.
///
/// The spec's rules, in order: `"__"` renders as one `_`; any *other* `_` is
/// not displayed; the first of those marks the next character as the access
/// key. This implements the first two and drops the access-key information —
/// see the module doc comment for why.
///
/// Note the third rule's "unless it is the last character" only affects
/// which character is the *access key*, never what is displayed, so a
/// trailing lone underscore still disappears. Pure function; unit-tested
/// below on every rule and on the trailing-underscore edge.
fn strip_mnemonics(label: &str) -> String {
    let mut display = String::with_capacity(label.len());
    let mut characters = label.chars().peekable();

    while let Some(character) = characters.next() {
        if character != '_' {
            display.push(character);
            continue;
        }
        // A doubled underscore is an escaped literal one: consume the second
        // half so it can't be re-examined as the start of another pair
        // (without that, `"a___b"` would eat all three).
        if characters.peek() == Some(&'_') {
            characters.next();
            display.push('_');
        }
        // A lone underscore is the mnemonic marker: not displayed.
    }

    display
}

/// Read one string property, or `None` if it's absent or of another type.
///
/// Teaching note (`downcast_ref`): `OwnedValue` derefs to
/// [`zbus::zvariant::Value`], whose `downcast_ref` both checks the variant's
/// actual type *and* transparently unwraps a `Value::Value` — a variant
/// nested inside a variant, which some toolkits genuinely send. Doing this by
/// hand with a `match` on `Value::Str(..)` would miss that case.
///
/// A property of the wrong type is treated exactly like an absent one:
/// [`menu_node`] then keeps the spec default, which is a strictly better
/// outcome than a whole menu failing to decode because one application put an
/// integer in `label`.
fn property_str<'a>(properties: &'a HashMap<String, OwnedValue>, name: &str) -> Option<&'a str> {
    properties.get(name)?.downcast_ref::<&str>().ok()
}

/// Read one boolean property. See [`property_str`] for the type-mismatch
/// contract.
fn property_bool(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    properties.get(name)?.downcast_ref::<bool>().ok()
}

/// Read one signed-integer property. See [`property_str`] for the
/// type-mismatch contract.
fn property_i32(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<i32> {
    properties.get(name)?.downcast_ref::<i32>().ok()
}

/// Decode one wire node (and, recursively, its children) into the model.
///
/// `depth` is the current nesting level, starting at 0 for the node
/// `GetLayout` was asked about; children past [`MAX_MENU_DEPTH`] are dropped
/// rather than followed (see that constant).
///
/// Every property is optional on the wire, so this starts from
/// [`MenuNode::default()`] — the spec's default table — and overrides only
/// the keys that are present. Pure function; unit-tested below against both a
/// captured real reply and synthetic edge cases.
fn menu_node(raw: &RawMenuNode, depth: usize) -> MenuNode {
    let properties = &raw.properties;

    let mut node = MenuNode {
        id: raw.id,
        ..MenuNode::default()
    };

    if let Some(kind) = property_str(properties, PROP_TYPE) {
        node.kind = MenuItemKind::from_wire(kind);
    }
    if let Some(label) = property_str(properties, PROP_LABEL) {
        node.label = strip_mnemonics(label);
    }
    if let Some(enabled) = property_bool(properties, PROP_ENABLED) {
        node.enabled = enabled;
    }
    if let Some(visible) = property_bool(properties, PROP_VISIBLE) {
        node.visible = visible;
    }
    if let Some(icon_name) = property_str(properties, PROP_ICON_NAME) {
        node.icon_name = icon_name.to_string();
    }
    if let Some(toggle_type) = property_str(properties, PROP_TOGGLE_TYPE) {
        node.toggle_type = ToggleType::from_wire(toggle_type);
    }
    if let Some(toggle_state) = property_i32(properties, PROP_TOGGLE_STATE) {
        node.toggle_state = ToggleState::from_wire(toggle_state);
    }
    if let Some(children_display) = property_str(properties, PROP_CHILDREN_DISPLAY) {
        node.has_submenu = children_display == CHILDREN_DISPLAY_SUBMENU;
    }

    if depth < MAX_MENU_DEPTH {
        node.children = raw
            .children
            .iter()
            .filter_map(|child| {
                // `try_clone` rather than a borrow because the generated
                // `TryFrom<OwnedValue>` consumes its input, and `OwnedValue`
                // is deliberately not `Clone` (it may own file descriptors,
                // which cannot be duplicated infallibly). A child that isn't
                // actually a `(ia{sv}av)` — a nonconformant application, or a
                // truncated reply — is skipped, not fatal: one bad row should
                // cost that row, never the whole menu.
                let child = RawMenuNode::try_from(child.try_clone().ok()?).ok()?;
                Some(menu_node(&child, depth + 1))
            })
            .collect();
    }

    node
}

/// Build a fresh, uncached proxy for a dbusmenu object.
///
/// The address it needs never comes from the caller: a menu lives on the
/// item's own bus name but at a path only the item knows, so every entry
/// point below spends its *first* round-trip on `item::read_menu_address`
/// (which is where the `Menu` property read and the two "no menu" sentinels
/// live) before it can even build this.
async fn build_menu_proxy<'a>(
    connection: &'a Connection,
    address: &ItemAddress,
) -> Option<DBusMenuProxy<'a>> {
    DBusMenuProxy::builder(connection)
        .destination(address.bus().to_string())
        .ok()?
        .path(address.path().to_string())
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()
}

/// Read one tray item's menu, from its registration string.
///
/// **This is the function Stage 21 calls** when a right-click opens a tray
/// popover. `parent` is the node to read from: 0 for the whole menu, or a
/// submenu row's [`MenuNode::id`] to re-read just that subtree after the
/// application populated it lazily.
///
/// The sequence, and why it is that sequence:
///
/// 1. resolve the item's `Menu` property → the dbusmenu object's path;
/// 2. `AboutToShow(parent)` — **before** the layout read, because the whole
///    point of the call is to give a lazily-populating application the chance
///    to fill the menu in first (see the module doc comment);
/// 3. `GetLayout(parent, -1, [])` — the entire subtree, all properties, one
///    round-trip;
/// 4. decode.
///
/// Uses a fresh one-shot connection, the same command-out shape `media.rs`
/// established and `item::send_item_command` copied: connect, ask, drop.
/// `Panel::update` cannot reach the tray worker's own connection — it lives
/// inside the subscription's stream.
///
/// `None` for every failure (no session bus, no `Menu` property, a
/// `GetLayout` that errors or times out): a tray item whose menu can't be
/// read should show no menu, never take the panel down.
///
/// `pub(crate)`, not `pub(super)`: Stage 21's caller (`popovers::tray_menu`
/// and `main.rs`) lives outside `modules::tray`, unlike everything else in
/// this file.
pub(crate) async fn read_menu(item_id: &str, parent: i32) -> Option<Menu> {
    let connection = Connection::session().await.ok()?;
    let address = item::read_menu_address(&connection, item_id).await?;
    let proxy = build_menu_proxy(&connection, &address).await?;

    // Failure ignored on purpose — see the module doc comment. The reply's
    // `needUpdate` is ignored for the same reason: the layout read below is
    // unconditional.
    let _ = proxy.about_to_show(parent).await;

    let (revision, raw) = proxy.get_layout(parent, -1, &[]).await.ok()?;
    Some(Menu {
        revision,
        root: menu_node(&raw, 0),
    })
}

/// Tell the application a menu row was clicked.
///
/// **The other function Stage 21 calls.** `node` is the clicked
/// [`MenuNode::id`]; `item_id` is the tray item's registration string, the
/// same key everything else in this module is addressed by.
///
/// Best-effort and fire-and-forget, exactly like `item::send_item_command`:
/// there is nothing `Panel::update` could usefully do with a failure beyond
/// the log line below, and a menu row whose application vanished between the
/// click and the call is an ordinary race, not an error worth surfacing.
pub(crate) async fn send_clicked(item_id: String, node: i32) {
    let Ok(connection) = Connection::session().await else {
        return;
    };
    let Some(address) = item::read_menu_address(&connection, &item_id).await else {
        return;
    };
    let Some(proxy) = build_menu_proxy(&connection, &address).await else {
        return;
    };

    // `data` is documented as "event-specific data" and is unused for a
    // click; an empty string is what every implementation sends.
    let data = Value::from("");
    if let Err(error) = proxy
        .event(node, EVENT_CLICKED, &data, EVENT_TIMESTAMP)
        .await
    {
        eprintln!("saola-panel: tray: menu click on {item_id} failed: {error}");
    }
}

/// One menu's change-notification stream, reduced to "something changed".
///
/// The signal-level counterpart of [`read_menu`], and the direct analogue of
/// `item::watch_item` one level up — including its load-bearing rule:
/// **this is `async` and must be fully awaited before the caller does
/// anything else with this menu.** The two `receive_*` calls each cost a real
/// `AddMatch` round-trip to the bus daemon, and a signal emitted before those
/// land is gone for good (D-Bus never replays to a late subscriber). Stage
/// 19's live-bus test hit exactly that race with `NewTitle`; the same shape
/// avoids it here.
///
/// Both signals collapse to `()`: `LayoutUpdated` carries a revision and a
/// subtree, `ItemsPropertiesUpdated` carries per-item property deltas, and
/// applying either incrementally would mean a second, subtly different
/// decoder living beside [`menu_node`]. A menu is a handful of rows and one
/// `GetLayout` is one round-trip, so "re-read everything" is both simpler and
/// fast enough — the same call `battery.rs`'s `watch_upower` and
/// `item::watch_item` both make.
///
/// Takes an owned [`Connection`] because the returned stream must be
/// `'static`, same as `item::watch_item` — and unlike the two one-shot
/// functions above, which each open (and drop) their own. A watch outlives
/// the call that created it, so it has to borrow a connection that will
/// outlive it too: whichever one Stage 21 keeps for as long as the menu is
/// on screen — `Panel::subscription` (via `popovers::tray_menu::watch`),
/// re-opened with a fresh connection whenever the open item changes (see
/// that module's doc comment).
pub(crate) async fn watch_menu(connection: Connection, item_id: &str) -> BoxStream<'static, ()> {
    let Some(address) = item::read_menu_address(&connection, item_id).await else {
        return stream::empty().boxed();
    };
    let Some(proxy) = build_menu_proxy(&connection, &address).await else {
        return stream::empty().boxed();
    };

    let (layout, properties) = match (
        proxy.receive_layout_updated().await,
        proxy.receive_items_properties_updated().await,
    ) {
        (Ok(layout), Ok(properties)) => (layout, properties),
        _ => return stream::empty().boxed(),
    };

    stream::select(layout.map(|_| ()), properties.map(|_| ())).boxed()
}

/// Build one wire node from a property list and already-built children —
/// **test-only**, and deliberately outside `mod tests` so that `watcher.rs`'s
/// live-bus test can use it too (it *serves* a menu built this way; the tests
/// below only decode). A private helper inside `mod tests` would have had to
/// be copy-pasted into that sibling module, which is what
/// `mod.rs`/`item.rs`'s duplicated `item()` fixtures already cost.
#[cfg(test)]
pub(super) fn fixture_node(
    id: i32,
    properties: &[(&str, Value<'static>)],
    children: Vec<RawMenuNode>,
) -> RawMenuNode {
    RawMenuNode {
        id,
        properties: properties
            .iter()
            .map(|(name, value)| {
                (
                    (*name).to_string(),
                    OwnedValue::try_from(value.try_clone().expect("cloneable fixture value"))
                        .expect("a fixture value converts"),
                )
            })
            .collect(),
        children: children
            .into_iter()
            .map(|child| OwnedValue::try_from(child).expect("a fixture child converts"))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::fixture_node as raw;

    // ---------------------------------------------------------------
    // Mnemonics.
    // ---------------------------------------------------------------

    #[test]
    fn a_lone_underscore_marks_an_access_key_and_is_not_displayed() {
        assert_eq!(strip_mnemonics("_File"), "File");
        assert_eq!(strip_mnemonics("Save _As..."), "Save As...");
        assert_eq!(strip_mnemonics("no mnemonic here"), "no mnemonic here");
    }

    #[test]
    fn a_doubled_underscore_is_one_literal_underscore() {
        assert_eq!(strip_mnemonics("__Really"), "_Really");
        assert_eq!(strip_mnemonics("snake__case"), "snake_case");
        // Three in a row: the first pair escapes, the leftover is a marker.
        assert_eq!(strip_mnemonics("a___b"), "a_b");
        assert_eq!(strip_mnemonics("____"), "__");
    }

    #[test]
    fn a_trailing_underscore_still_disappears() {
        // The spec's "unless it is the last character" clause only decides
        // which character is the *access key*; it never makes an underscore
        // display.
        assert_eq!(strip_mnemonics("Quit_"), "Quit");
        assert_eq!(strip_mnemonics("_"), "");
        assert_eq!(strip_mnemonics(""), "");
    }

    // ---------------------------------------------------------------
    // The property-default table.
    // ---------------------------------------------------------------

    #[test]
    fn the_default_node_is_the_specs_default_table() {
        // Pinned field by field against the "Default Value" column of the
        // table in this module's doc comment. If a future edit "tidies" one
        // of these (`toggle_state` to `Off` is the tempting one), this test
        // is the thing that says no.
        let node = MenuNode::default();
        assert_eq!(node.kind, MenuItemKind::Standard, "type = \"standard\"");
        assert_eq!(node.label, "", "label = \"\"");
        assert!(node.enabled, "enabled = true");
        assert!(node.visible, "visible = true");
        assert_eq!(node.icon_name, "", "icon-name = \"\"");
        assert_eq!(node.toggle_type, ToggleType::None, "toggle-type = \"\"");
        assert_eq!(
            node.toggle_state,
            ToggleState::Indeterminate,
            "toggle-state = -1, i.e. indeterminate"
        );
        assert!(!node.has_submenu, "children-display = \"\"");
        assert!(node.children.is_empty());
    }

    #[test]
    fn a_node_that_sends_no_properties_at_all_gets_every_default() {
        // The commonest real shape after a separator: an application that
        // trusts the defaults sends an empty dictionary.
        let node = menu_node(&raw(7, &[], Vec::new()), 0);
        assert_eq!(node.id, 7);
        assert_eq!(
            node,
            MenuNode {
                id: 7,
                ..MenuNode::default()
            }
        );
    }

    #[test]
    fn present_properties_override_their_defaults() {
        let node = menu_node(
            &raw(
                3,
                &[
                    (PROP_LABEL, Value::from("_Quit")),
                    (PROP_ENABLED, Value::from(false)),
                    (PROP_VISIBLE, Value::from(false)),
                    (PROP_ICON_NAME, Value::from("application-exit")),
                    (PROP_TOGGLE_TYPE, Value::from("checkmark")),
                    (PROP_TOGGLE_STATE, Value::from(1_i32)),
                    (PROP_CHILDREN_DISPLAY, Value::from("submenu")),
                ],
                Vec::new(),
            ),
            0,
        );
        assert_eq!(node.label, "Quit", "the mnemonic underscore is stripped");
        assert!(!node.enabled);
        assert!(!node.visible);
        assert_eq!(node.icon_name, "application-exit");
        assert_eq!(node.toggle_type, ToggleType::Checkmark);
        assert_eq!(node.toggle_state, ToggleState::On);
        assert!(node.has_submenu);
    }

    #[test]
    fn a_separator_is_decoded_as_one() {
        let node = menu_node(
            &raw(4, &[(PROP_TYPE, Value::from("separator"))], Vec::new()),
            0,
        );
        assert_eq!(node.kind, MenuItemKind::Separator);
        // Everything else still defaults — a separator sends nothing else.
        assert_eq!(node.label, "");
        assert!(node.enabled);
        assert!(node.visible);
    }

    #[test]
    fn an_unknown_or_vendor_type_is_drawn_as_an_ordinary_row() {
        assert_eq!(MenuItemKind::from_wire("standard"), MenuItemKind::Standard);
        assert_eq!(
            MenuItemKind::from_wire("separator"),
            MenuItemKind::Separator
        );
        assert_eq!(
            MenuItemKind::from_wire("x-canonical-fancy"),
            MenuItemKind::Standard
        );
        // Case matters: the wire values are lowercase.
        assert_eq!(MenuItemKind::from_wire("Separator"), MenuItemKind::Standard);
    }

    #[test]
    fn toggle_type_and_state_follow_the_spec_including_its_catch_all() {
        assert_eq!(ToggleType::from_wire(""), ToggleType::None);
        assert_eq!(ToggleType::from_wire("checkmark"), ToggleType::Checkmark);
        assert_eq!(ToggleType::from_wire("radio"), ToggleType::Radio);
        assert_eq!(ToggleType::from_wire("something-else"), ToggleType::None);

        assert_eq!(ToggleState::from_wire(0), ToggleState::Off);
        assert_eq!(ToggleState::from_wire(1), ToggleState::On);
        // "anything else = indeterminate", verbatim from the spec — which
        // includes the -1 default and any other integer.
        assert_eq!(ToggleState::from_wire(-1), ToggleState::Indeterminate);
        assert_eq!(ToggleState::from_wire(42), ToggleState::Indeterminate);
    }

    #[test]
    fn a_property_of_the_wrong_type_falls_back_to_the_default() {
        // A nonconformant application putting an integer in `label` must cost
        // that one property, never the whole menu.
        let node = menu_node(
            &raw(
                1,
                &[
                    (PROP_LABEL, Value::from(12_i32)),
                    (PROP_ENABLED, Value::from("yes")),
                ],
                Vec::new(),
            ),
            0,
        );
        assert_eq!(node.label, "");
        assert!(node.enabled);
    }

    // ---------------------------------------------------------------
    // Recursion.
    // ---------------------------------------------------------------

    #[test]
    fn a_nested_submenu_decodes_into_a_tree() {
        let tree = raw(
            0,
            &[],
            vec![
                raw(1, &[(PROP_LABEL, Value::from("Top"))], Vec::new()),
                raw(
                    2,
                    &[
                        (PROP_LABEL, Value::from("More")),
                        (PROP_CHILDREN_DISPLAY, Value::from("submenu")),
                    ],
                    vec![
                        raw(3, &[(PROP_LABEL, Value::from("Inner"))], Vec::new()),
                        raw(
                            4,
                            &[
                                (PROP_LABEL, Value::from("Deeper")),
                                (PROP_CHILDREN_DISPLAY, Value::from("submenu")),
                            ],
                            vec![raw(5, &[(PROP_LABEL, Value::from("Leaf"))], Vec::new())],
                        ),
                    ],
                ),
            ],
        );

        let root = menu_node(&tree, 0);
        assert_eq!(root.id, 0);
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].label, "Top");
        assert!(!root.children[0].has_submenu);

        let more = &root.children[1];
        assert_eq!(more.label, "More");
        assert!(more.has_submenu);
        assert_eq!(more.children.len(), 2);
        assert_eq!(more.children[0].label, "Inner");

        let deeper = &more.children[1];
        assert!(deeper.has_submenu);
        assert_eq!(deeper.children.len(), 1);
        assert_eq!(deeper.children[0].label, "Leaf");
        assert!(deeper.children[0].children.is_empty());
    }

    #[test]
    fn a_submenu_row_may_legitimately_have_no_children_yet() {
        // The lazily-populated case `AboutToShow` exists for: the row says
        // "I open a submenu" and the children arrive only after the
        // application is told the menu is about to be shown.
        let node = menu_node(
            &raw(
                2,
                &[(PROP_CHILDREN_DISPLAY, Value::from("submenu"))],
                Vec::new(),
            ),
            0,
        );
        assert!(node.has_submenu);
        assert!(node.children.is_empty());
    }

    #[test]
    fn recursion_stops_at_the_depth_cap_instead_of_overflowing_the_stack() {
        // Build MAX_MENU_DEPTH + 5 levels, then check the tree stops.
        let mut deepest = raw(1000, &[(PROP_LABEL, Value::from("bottom"))], Vec::new());
        let levels = MAX_MENU_DEPTH + 5;
        for level in (0..levels).rev() {
            deepest = raw(
                level as i32,
                &[(PROP_CHILDREN_DISPLAY, Value::from("submenu"))],
                vec![deepest],
            );
        }

        let mut node = &menu_node(&deepest, 0);
        let mut depth = 0;
        while let Some(child) = node.children.first() {
            node = child;
            depth += 1;
        }
        assert_eq!(
            depth, MAX_MENU_DEPTH,
            "the parser must stop following children at the cap"
        );
    }

    // ---------------------------------------------------------------
    // The captured real-world reply.
    // ---------------------------------------------------------------

    /// A **real** `GetLayout` reply, captured from a running application.
    ///
    /// Provenance: `tailscaled`'s tray icon (`org.kde.StatusNotifierItem-…`,
    /// `Id` = `"tailscale"`), on this machine's live niri session on
    /// 2026-07-31, via `GetLayout(0, -1, [])` on its `/StatusNotifierMenu`
    /// object. These are the **raw D-Bus body bytes of the reply message**,
    /// not a hand-written reconstruction — signature `(u(ia{sv}av))`,
    /// little-endian, 1140 bytes. Deserializing them exercises the real
    /// marshalling (dictionary ordering, `av` alignment, the nested variant
    /// signatures) that a Rust-side fixture cannot reach.
    ///
    /// The same reply printed by `gdbus`, for anyone reading this without a
    /// debugger:
    ///
    /// ```text
    /// (uint32 7, (0, @a{sv} {}, [
    ///   <(1, {'toggle-state': <0>, 'enabled': <true>, 'label': <'Connect'>,
    ///         'toggle-type': <''>}, @av [])>,
    ///   <(2, {'toggle-type': <''>, 'toggle-state': <0>, 'visible': <false>,
    ///         'enabled': <true>, 'label': <'Disconnect'>}, @av [])>,
    ///   <(3, {'type': <'separator'>}, @av [])>,
    ///   <(4, {'toggle-type': <''>, 'toggle-state': <0>, 'enabled': <true>,
    ///         'label': <'Account'>}, @av [])>,
    ///   <(5, {'toggle-state': <0>, 'enabled': <false>,
    ///         'label': <'This Device: not connected'>, 'toggle-type': <''>}, @av [])>,
    ///   <(6, {'type': <'separator'>}, @av [])>,
    ///   <(7, {'enabled': <false>, 'label': <'More settings'>,
    ///         'toggle-type': <''>, 'toggle-state': <0>}, @av [])>,
    ///   <(8, {'enabled': <true>, 'label': <'Rebuild menu'>,
    ///         'toggle-type': <''>, 'toggle-state': <0>}, @av [])>,
    ///   <(9, {'toggle-state': <0>, 'enabled': <true>, 'label': <'Quit'>,
    ///         'toggle-type': <''>}, @av [])>
    /// ]))
    /// ```
    ///
    /// Three things about this capture are worth more than the bytes:
    /// the root's property dictionary is **empty**; `type` appears **only**
    /// on the two separators and `visible` **only** on the one hidden row
    /// (everything else relies on the defaults); and every ordinary row sends
    /// `toggle-type: ""` + `toggle-state: 0` even though none of them is
    /// togglable — a real application shipping redundant, at-default-ish
    /// properties, which is exactly the sort of thing a synthetic fixture
    /// would never think to include.
    const CAPTURED_TAILSCALE_GETLAYOUT: &[u8] = &[
        0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x60, 0x04, 0x00, 0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73, 0x76, 0x7d, 0x61, 0x76,
        0x29, 0x00, 0x01, 0x00, 0x00, 0x00, 0x70, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x65,
        0x6e, 0x61, 0x62, 0x6c, 0x65, 0x64, 0x00, 0x01, 0x62, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x6c, 0x61, 0x62, 0x65, 0x6c, 0x00, 0x01,
        0x73, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x43, 0x6f, 0x6e, 0x6e, 0x65, 0x63,
        0x74, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c,
        0x65, 0x2d, 0x74, 0x79, 0x70, 0x65, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67,
        0x67, 0x6c, 0x65, 0x2d, 0x73, 0x74, 0x61, 0x74, 0x65, 0x00, 0x01, 0x69, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73, 0x76, 0x7d, 0x61,
        0x76, 0x29, 0x00, 0x02, 0x00, 0x00, 0x00, 0x88, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,
        0x76, 0x69, 0x73, 0x69, 0x62, 0x6c, 0x65, 0x00, 0x01, 0x62, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65,
        0x64, 0x00, 0x01, 0x62, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05,
        0x00, 0x00, 0x00, 0x6c, 0x61, 0x62, 0x65, 0x6c, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00,
        0x0a, 0x00, 0x00, 0x00, 0x44, 0x69, 0x73, 0x63, 0x6f, 0x6e, 0x6e, 0x65, 0x63, 0x74, 0x00,
        0x00, 0x0b, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65, 0x2d, 0x74, 0x79, 0x70,
        0x65, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65, 0x2d, 0x73,
        0x74, 0x61, 0x74, 0x65, 0x00, 0x01, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73, 0x76, 0x7d, 0x61, 0x76, 0x29, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x1a, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x74, 0x79, 0x70, 0x65, 0x00,
        0x01, 0x73, 0x00, 0x09, 0x00, 0x00, 0x00, 0x73, 0x65, 0x70, 0x61, 0x72, 0x61, 0x74, 0x6f,
        0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73, 0x76,
        0x7d, 0x61, 0x76, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x69, 0x00,
        0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65, 0x2d, 0x73, 0x74,
        0x61, 0x74, 0x65, 0x00, 0x01, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,
        0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x64, 0x00, 0x01, 0x62, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x6c, 0x61, 0x62, 0x65, 0x6c, 0x00,
        0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x41, 0x63, 0x63, 0x6f, 0x75,
        0x6e, 0x74, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67,
        0x6c, 0x65, 0x2d, 0x74, 0x79, 0x70, 0x65, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73,
        0x76, 0x7d, 0x61, 0x76, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x7f,
        0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65, 0x2d, 0x74,
        0x79, 0x70, 0x65, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65,
        0x2d, 0x73, 0x74, 0x61, 0x74, 0x65, 0x00, 0x01, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
        0x00, 0x00, 0x00, 0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x64, 0x00, 0x01, 0x62, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x6c, 0x61, 0x62,
        0x65, 0x6c, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x1a, 0x00, 0x00, 0x00, 0x54, 0x68,
        0x69, 0x73, 0x20, 0x44, 0x65, 0x76, 0x69, 0x63, 0x65, 0x3a, 0x20, 0x6e, 0x6f, 0x74, 0x20,
        0x63, 0x6f, 0x6e, 0x6e, 0x65, 0x63, 0x74, 0x65, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73, 0x76, 0x7d, 0x61, 0x76, 0x29, 0x00, 0x06, 0x00, 0x00,
        0x00, 0x1a, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x74, 0x79, 0x70, 0x65, 0x00, 0x01,
        0x73, 0x00, 0x09, 0x00, 0x00, 0x00, 0x73, 0x65, 0x70, 0x61, 0x72, 0x61, 0x74, 0x6f, 0x72,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73, 0x76, 0x7d,
        0x61, 0x76, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x78, 0x00, 0x00,
        0x00, 0x07, 0x00, 0x00, 0x00, 0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x64, 0x00, 0x01, 0x62,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x6c,
        0x61, 0x62, 0x65, 0x6c, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00,
        0x4d, 0x6f, 0x72, 0x65, 0x20, 0x73, 0x65, 0x74, 0x74, 0x69, 0x6e, 0x67, 0x73, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65,
        0x2d, 0x74, 0x79, 0x70, 0x65, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67,
        0x6c, 0x65, 0x2d, 0x73, 0x74, 0x61, 0x74, 0x65, 0x00, 0x01, 0x69, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b, 0x73, 0x76, 0x7d, 0x61, 0x76,
        0x29, 0x00, 0x08, 0x00, 0x00, 0x00, 0x71, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x74,
        0x6f, 0x67, 0x67, 0x6c, 0x65, 0x2d, 0x73, 0x74, 0x61, 0x74, 0x65, 0x00, 0x01, 0x69, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x64,
        0x00, 0x01, 0x62, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00,
        0x00, 0x00, 0x6c, 0x61, 0x62, 0x65, 0x6c, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x0c,
        0x00, 0x00, 0x00, 0x52, 0x65, 0x62, 0x75, 0x69, 0x6c, 0x64, 0x20, 0x6d, 0x65, 0x6e, 0x75,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67,
        0x67, 0x6c, 0x65, 0x2d, 0x74, 0x79, 0x70, 0x65, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x28, 0x69, 0x61, 0x7b,
        0x73, 0x76, 0x7d, 0x61, 0x76, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00,
        0x70, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x64,
        0x00, 0x01, 0x62, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00,
        0x00, 0x00, 0x6c, 0x61, 0x62, 0x65, 0x6c, 0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x04,
        0x00, 0x00, 0x00, 0x51, 0x75, 0x69, 0x74, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0b, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65, 0x2d, 0x74, 0x79, 0x70, 0x65,
        0x00, 0x01, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x74, 0x6f, 0x67, 0x67, 0x6c, 0x65, 0x2d, 0x73, 0x74,
        0x61, 0x74, 0x65, 0x00, 0x01, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    /// Deserialize the captured reply the way the proxy does, straight from
    /// the wire bytes.
    ///
    /// Teaching note (**why the context is `new_dbus(Little, 0)`**): a D-Bus
    /// message body's alignment is computed from the start of the *message*,
    /// not the body, so zbus hands the body a context whose `position` is the
    /// header's length (80, for this capture). The spec requires that header
    /// length to be a multiple of 8 — which is the widest alignment D-Bus
    /// has — so re-deserializing from position 0 lays every field out
    /// identically. Little-endian because the capture was taken on x86; the
    /// byte order is part of the fixture, not of the protocol (D-Bus messages
    /// carry their own endianness flag).
    fn decode_captured() -> (u32, RawMenuNode) {
        let context =
            zbus::zvariant::serialized::Context::new_dbus(zbus::zvariant::Endian::Little, 0);
        let data = zbus::zvariant::serialized::Data::new(CAPTURED_TAILSCALE_GETLAYOUT, context);
        data.deserialize::<(u32, RawMenuNode)>()
            .expect("the captured reply deserializes against our declared wire type")
            .0
    }

    #[test]
    fn the_captured_reply_deserializes_against_the_declared_wire_type() {
        // This assertion is really about `RawMenuNode`'s *signature*: if the
        // derived `Type` impl produced anything other than `(ia{sv}av)` —
        // which is exactly what a `children: Vec<RawMenuNode>` field would do
        // — deserialization fails here rather than subtly later.
        let (revision, root) = decode_captured();
        assert_eq!(revision, 7);
        assert_eq!(root.id, 0);
        assert!(
            root.properties.is_empty(),
            "the root is the menu container and carries no properties"
        );
        assert_eq!(root.children.len(), 9);
    }

    #[test]
    fn the_captured_reply_decodes_into_the_menu_a_user_would_see() {
        let (revision, raw) = decode_captured();
        let menu = Menu {
            revision,
            root: menu_node(&raw, 0),
        };

        assert_eq!(menu.revision, 7);
        let rows = &menu.root.children;
        assert_eq!(rows.len(), 9);

        // Row 1: an ordinary enabled row. Note what it does *not* send —
        // `type`, `visible` and `children-display` are all absent, so all
        // three have to come from the default table.
        assert_eq!(rows[0].id, 1);
        assert_eq!(rows[0].label, "Connect");
        assert_eq!(rows[0].kind, MenuItemKind::Standard);
        assert!(rows[0].enabled);
        assert!(rows[0].visible);
        assert!(!rows[0].has_submenu);
        // The application sends `toggle-type: ""` + `toggle-state: 0` on
        // every row including this one, which is *not* the same as sending
        // nothing: `toggle-state: 0` decodes to `Off`, whereas the default
        // (had it been omitted) would have been `Indeterminate`. Pinning
        // both is what proves the parser reads the wire rather than assuming.
        assert_eq!(rows[0].toggle_type, ToggleType::None);
        assert_eq!(rows[0].toggle_state, ToggleState::Off);

        // Row 2: the one row that is explicitly invisible. The model keeps
        // it — hiding is Stage 21's decision, not the decoder's.
        assert_eq!(rows[1].label, "Disconnect");
        assert!(!rows[1].visible);
        assert!(rows[1].enabled);

        // Rows 3 and 6: separators, the only two rows that send `type`.
        assert_eq!(rows[2].kind, MenuItemKind::Separator);
        assert_eq!(rows[2].label, "");
        assert_eq!(rows[5].kind, MenuItemKind::Separator);

        // Row 5: disabled, and the longest label in the menu.
        assert_eq!(rows[4].label, "This Device: not connected");
        assert!(!rows[4].enabled);

        // Row 7: also disabled.
        assert_eq!(rows[6].label, "More settings");
        assert!(!rows[6].enabled);

        assert_eq!(rows[7].label, "Rebuild menu");
        assert!(rows[7].enabled);

        // Row 9: the last one, and the id `send_clicked` would carry.
        assert_eq!(rows[8].id, 9);
        assert_eq!(rows[8].label, "Quit");
        assert!(rows[8].enabled);

        // A real tray menu is flat: nothing in this capture has children.
        assert!(rows.iter().all(|row| row.children.is_empty()));
        assert!(rows.iter().all(|row| !row.has_submenu));
        // ...and none of it is togglable, despite every row saying so
        // half-heartedly.
        assert!(rows.iter().all(|row| row.toggle_type == ToggleType::None));
    }

    #[test]
    fn the_captured_reply_uses_ids_that_are_stable_and_ordered() {
        // Row order is the menu's order and the ids happen to be 1..=9 here;
        // the model must preserve wire order exactly, since a menu whose rows
        // shuffle between reads is unusable.
        let (_, raw) = decode_captured();
        let ids: Vec<i32> = menu_node(&raw, 0)
            .children
            .iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}
