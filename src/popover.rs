//! Popover infrastructure — **one** floating ink panel, at most, ever.
//!
//! Spec §6: a popover is opaque ink at `radii.popover` (30) with the
//! `shadows.popover` drop shadow, sitting `sizes.popover_top` (72) below the
//! screen edge the panel is on — realized as a 6 px margin under the panel's
//! 66 px reservation, see `SurfaceGeometry::of` in `main.rs` — and
//! `sizes.popover_width` (440) wide. It grows
//! *away* from the panel, never overlaps the control that opened it, and
//! **only one is open at a time — opening one closes the others.**
//!
//! Stage 16 built the lifecycle and nothing else: the quick-settings popover
//! it opened was an empty ink shell, rendered by a `view` function this file
//! used to own. Stage 17 gave `QuickSettings` real content in
//! `popovers::quick_settings`, and Stage 21 adds `TrayMenu` the same way in
//! `popovers::tray_menu` — both rendered directly by `main.rs::Panel::
//! popover_view` rather than by this file, since real content needs module
//! state (`Volume`/`Media`, or Stage 20's `Menu` tree) this file deliberately
//! stays ignorant of (see `popovers::quick_settings`'s doc comment). With a
//! second real kind now shipping, this file has genuinely nothing left to
//! render — its old `view`/`shell` functions are gone, not just unreachable.
//! Both kinds get the open/close machinery below for free, which is the
//! point of having built it on its own.
//!
//! # How the pieces fit
//!
//! A popover is a **layer-shell surface of its own** (not an xdg popup — see
//! "Why not an xdg popup" below), so opening one means asking the compositor
//! for a surface and closing one means destroying it. That is `main.rs`'s
//! job — it owns the [`SurfaceRole`](crate::SurfaceRole) registry and the
//! spawn/remove helpers. This module owns only the *decision*:
//!
//! ```text
//!   trigger click / Escape / focus loss
//!            │
//!            ▼
//!   popover::Message  ──►  PopoverManager::update  ──►  popover::Action
//!                                                            │
//!                              main.rs turns it into  ◄──────┘
//!                              spawn_surface / remove_surface tasks
//!                              and calls PopoverManager::opened(id, kind)
//! ```
//!
//! Keeping the manager free of `Task`s and `NewLayerShellSettings` is what
//! makes the whole state machine unit-testable without a compositor (see
//! `mod tests` at the bottom) — the same trick [`SurfaceGeometry`] plays with
//! the geometry arithmetic.
//!
//! [`SurfaceGeometry`]: crate::SurfaceGeometry
//!
//! # Why not an xdg popup
//!
//! `#[to_layer_message(multi)]` also injects `NewPopUp` (a real
//! `xdg_popup` parented to one of our surfaces). It is the wrong tool here:
//! an xdg popup is positioned *relative to its parent surface* by an
//! `xdg_positioner`, and our parent — the bar, or the status island — is a
//! full-output-width surface whose interesting content (the status cluster)
//! sits at an offset we cannot measure (iced 0.14 has no laid-out-widget
//! measurement; see Stage 15's handoff). The spec's anchoring is stated in
//! *screen* terms anyway ("72px from the screen top, 26px from the relevant
//! edge"), which is exactly what a layer surface's anchor + margin express
//! directly. Layer surfaces also let the popover sit on `Layer::Overlay`,
//! above the panel's own `Layer::Top`.

use iced::{keyboard, window, Subscription};
use saola_theme::Theme;

/// Which popover a surface is — the payload of
/// [`SurfaceRole::Popover`](crate::SurfaceRole::Popover).
///
/// One variant today. Stage 17 keeps using [`Self::QuickSettings`] (it fills
/// the shell rather than adding a kind); Stage 21 adds a tray-menu kind, and
/// *that* is when the "opening one closes the sibling" branch of
/// [`PopoverManager::update`] starts firing for real.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PopoverKind {
    /// The quick-settings panel: spec §7's "Ink, right — 2×2 toggles,
    /// sliders, media", opened from the bar's status cluster (ledger) or the
    /// status island (islands). Real content since Stage 17
    /// (`popovers::quick_settings`).
    QuickSettings,
    /// A tray item's `com.canonical.dbusmenu` context menu, opened by a
    /// right-click on the item's icon (`modules::tray::Message::
    /// ContextMenu`). Real content in `popovers::tray_menu`, rendering
    /// Stage 20's `modules::tray::menu::Menu` model. This is the second real
    /// kind Stage 16's handoff predicted would replace [`Self`]'s old
    /// `#[cfg(test)] Probe` variant — the "opening one closes the others"
    /// rule (below) now has two real kinds to prove it with, so `Probe` is
    /// gone.
    TrayMenu,
}

impl PopoverKind {
    /// The height, in logical pixels, of the layer-shell surface this kind of
    /// popover needs.
    ///
    /// # Why a popover has to declare a height at all (teaching note)
    ///
    /// A wlr-layer-shell surface may pass `0` for a dimension **only** when
    /// it is anchored to both edges on that axis — the compositor then
    /// stretches it between them. A popover is anchored to the panel's edge
    /// and one side, so it is stretched on neither axis and both numbers have
    /// to be real. There is no "hug your content" mode: the surface's size is
    /// negotiated with the compositor before iced has laid anything out, and
    /// iced 0.14 offers no way to measure a laid-out widget afterwards
    /// (`container::visible_bounds` is gone — see Stage 15's handoff for the
    /// full argument). So the size is a *declaration*, not a measurement.
    ///
    /// # The number itself
    ///
    /// `QuickSettings` delegates to `popovers::quick_settings::height`
    /// (Stage 17) — the same function its own `view` is laid out by, so the
    /// declared surface size and the content can't drift apart independently
    /// (a mismatch still only costs some blank ink at the bottom, never a
    /// panic or clipped content; see that function's doc comment). Stage
    /// 16's placeholder (`sizes.list_row * 6.0`, a guess at a plausible
    /// block for the 2×2 grid plus sliders and media row) is gone now that
    /// there's real content to measure against.
    pub fn height(self, theme: &Theme) -> f32 {
        match self {
            PopoverKind::QuickSettings => crate::popovers::quick_settings::height(theme),
            PopoverKind::TrayMenu => crate::popovers::tray_menu::height(theme),
        }
    }
}

/// Everything that can change which popover is open.
///
/// Nested in the panel's own enum as `Message::Popover(..)`, exactly like a
/// bar module's messages (see `modules::clock::Message` for the pattern's
/// full teaching note). Unlike a module's, these do not all come from a
/// subscription: [`Self::Triggered`] is a widget message from the trigger's
/// `mouse_area`, while the other two come from [`subscription`] below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// A trigger was clicked (the ledger bar's status region, or the status
    /// island). Toggles: opens `kind`, or closes it if it is already the open
    /// one, or swaps if a *different* kind is open.
    Triggered(PopoverKind),
    /// A surface lost keyboard focus — `iced::window::Event::Unfocused`,
    /// which `iced_layershell` produces from the Wayland `wl_keyboard.leave`
    /// (`iced_layershell-0.19.1/src/conversion.rs:148`). See [`subscription`]
    /// for why the `window::Id` is carried but deliberately not compared.
    Unfocused(window::Id),
    /// Escape was pressed on a surface that had keyboard focus. The only
    /// surface of ours that can ever have it is a popover: every panel
    /// surface is created with `KeyboardInteractivity::None`.
    Escaped(window::Id),
}

/// What [`PopoverManager::update`] wants `main.rs` to do about the surfaces.
///
/// Deliberately *not* a `Task<Message>`: minting Ids, registering roles and
/// deriving layer-shell geometry all belong to the surface registry in
/// `main.rs`, and keeping them out of here is what lets every rule in this
/// module be tested without a Wayland connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do (e.g. a focus-loss report while nothing is open).
    None,
    /// Destroy this surface — `Panel::remove_surface`.
    Close(window::Id),
    /// Create a popover surface of this kind — `Panel::spawn_surface` with
    /// `SurfaceRole::Popover(kind)` — and hand the minted Id straight back
    /// via [`PopoverManager::opened`].
    ///
    /// `close` is the sibling that has to go first, if there was one. That is
    /// the spec's "opening one closes the others" rule, expressed as a single
    /// atomic action rather than as two messages, so there is no in-between
    /// state in which two popovers exist.
    Open {
        kind: PopoverKind,
        close: Option<window::Id>,
    },
}

/// The whole "at most one popover" invariant, in one field.
///
/// `Option<(window::Id, PopoverKind)>` rather than a map: the invariant *is*
/// the type. There is no code path that can produce two popovers, because
/// there is nowhere to put a second one.
#[derive(Debug, Default)]
pub struct PopoverManager {
    /// The open popover's surface Id and kind, or `None`.
    ///
    /// Written in exactly two places: [`Self::opened`] (after `main.rs` has
    /// minted the Id) and the arms of [`Self::update`] that close something.
    open: Option<(window::Id, PopoverKind)>,
}

impl PopoverManager {
    /// Fold one message into the open/closed state and say what the surfaces
    /// need to do about it.
    ///
    /// # The dismissal rule, and why it ignores the `window::Id`
    ///
    /// [`Message::Unfocused`] and [`Message::Escaped`] both close whatever is
    /// open, **whichever surface the event was attributed to**. That looks
    /// sloppy and is actually the careful choice; the reasoning is worth
    /// recording because it depends on three separate facts:
    ///
    /// 1. **A panel surface can never hold keyboard focus.** The bar and the
    ///    islands are created with `KeyboardInteractivity::None`, so the
    ///    compositor never gives them a `wl_keyboard.enter` and they can
    ///    never produce a genuine `leave`. A popover (`OnDemand`) is the only
    ///    surface of ours that can, so any `Unfocused`/key event we see is a
    ///    report about *the* popover, whatever Id it arrives with.
    /// 2. **The Id it arrives with is genuinely unreliable.** layershellev
    ///    attributes a `wl_keyboard.leave` to `current_surface_id()` — the
    ///    surface most recently entered *or clicked*
    ///    (`layershellev-0.19.1/src/seat.rs:216`, `:275`, `:683`) — not to the
    ///    surface named in the event. So clicking the bar while a popover is
    ///    open makes the popover's own unfocus arrive stamped with the *bar's*
    ///    Id. Comparing Ids would silently drop exactly the dismissal the user
    ///    just asked for.
    /// 3. **It cannot cause a toggle flicker.** The obvious worry is clicking
    ///    the trigger to close: if the unfocus were processed *first* the
    ///    popover would close and the trigger would immediately re-open it.
    ///    It can't happen, structurally: a widget message
    ///    ([`Message::Triggered`]) is drained synchronously at the end of the
    ///    same dispatch that produced it
    ///    (`iced_layershell-0.19.1/src/multi_window.rs:952–962`), while a
    ///    subscription message round-trips through the runtime's broadcast
    ///    channel and can only surface on a later iteration. `Triggered`
    ///    therefore always wins, and the `Unfocused` that follows finds
    ///    nothing open and does nothing.
    pub fn update(&mut self, message: Message) -> Action {
        match message {
            // Same kind again ⇒ toggle shut. A different kind ⇒ the spec's
            // "opening one closes the others". Nothing open ⇒ just open.
            Message::Triggered(kind) => match self.open.take() {
                Some((id, open_kind)) if open_kind == kind => Action::Close(id),
                Some((id, _)) => Action::Open {
                    kind,
                    close: Some(id),
                },
                None => Action::Open { kind, close: None },
            },
            // Escape and focus loss are the same rule; see the note above for
            // why neither compares `id`. `take()` leaves the manager closed
            // whether or not there was anything to close.
            Message::Unfocused(_) | Message::Escaped(_) => match self.open.take() {
                Some((id, _)) => Action::Close(id),
                None => Action::None,
            },
        }
    }

    /// Record the Id `main.rs` minted for the popover [`Action::Open`] just
    /// asked for.
    ///
    /// Split out from [`Self::update`] because the Id does not exist yet when
    /// the decision is made: it comes back from
    /// `Message::layershell_open(..)`, the constructor
    /// `#[to_layer_message(multi)]` generates, which mints a `window::Id` and
    /// returns it *alongside* the `Task` that requests the surface (Stage 13's
    /// handoff has the full teaching note). So the sequence is always
    /// decide → spawn → record, in one `update` call.
    pub fn opened(&mut self, id: window::Id, kind: PopoverKind) {
        self.open = Some((id, kind));
    }

    /// A surface reported that it is gone (`iced::window::close_events()`).
    ///
    /// Belt-and-braces: every close path already clears `open` before asking
    /// for the removal. This exists so a popover that dies some *other* way —
    /// the compositor closing it, an output disappearing — cannot leave the
    /// manager believing a destroyed surface is still open, which would make
    /// the next trigger click a no-op (it would "close" a surface that is
    /// already gone).
    ///
    /// The Id check is load-bearing during a sibling swap: the outgoing
    /// popover's `Closed` event arrives *after* the incoming one has been
    /// recorded, and must not clear it.
    pub fn closed(&mut self, id: window::Id) {
        if self.open.is_some_and(|(open_id, _)| open_id == id) {
            self.open = None;
        }
    }

    /// Whether `kind` is the one currently open. The accessor Stage 16's
    /// handoff flagged as "one accessor away": content that lives *inside* a
    /// popover (a tray-menu row click, say) sometimes needs to ask "is my
    /// surface still the open one" without driving the state machine through
    /// [`Self::update`] — see [`Self::close`] for the companion that acts on
    /// the answer, and `main.rs::Panel::open_tray_menu` for why "different
    /// item, same kind, already open" needs this rather than just re-sending
    /// [`Message::Triggered`] (which would toggle it *shut*, since same-kind
    /// re-trigger means "close" — see [`Self::update`]'s doc comment).
    pub fn is_open(&self, kind: PopoverKind) -> bool {
        self.open.is_some_and(|(_, open_kind)| open_kind == kind)
    }

    /// Close the popover **if** it is currently showing `kind` — a
    /// close-from-the-inside path distinct from [`Message::Escaped`]/
    /// [`Message::Unfocused`] (which close *whatever* is open, no kind
    /// check — see [`Self::update`]) and from [`Message::Triggered`] (which
    /// toggles). A tray-menu row activating needs exactly this: dismiss the
    /// popover as a side effect of a click *inside* it, but only if the
    /// popover the click came from is still the one that's open (it always
    /// will be in practice — nothing else can close a popover out from under
    /// content that is actively being clicked — but the check costs nothing
    /// and means this can never close the *wrong* popover if that invariant
    /// is ever violated by a future change).
    pub fn close(&mut self, kind: PopoverKind) -> Action {
        match self.open.filter(|(_, open_kind)| *open_kind == kind) {
            Some((id, _)) => {
                self.open = None;
                Action::Close(id)
            }
            None => Action::None,
        }
    }
}

/// The two dismissal signals, lifted out of iced's event broadcast.
///
/// `iced::event::listen_with` is a **filter over events the runtime is
/// already broadcasting**, not a poll — the same reasoning that lets
/// `window::open_events()`/`close_events()` live in the panel's subscription
/// batch (CLAUDE.md: "every module maps to a signal, never a poll"). It emits
/// only when a surface actually loses focus or Escape is actually pressed.
///
/// # The focus-loss event, pinned down
///
/// It is `iced::window::Event::Unfocused`, and the chain that produces it is:
///
/// | layer | what happens |
/// |---|---|
/// | Wayland | the compositor sends `wl_keyboard.leave` |
/// | `layershellev-0.19.1/src/seat.rs:275–282` | pushes `DispatchMessageInner::Unfocus`, stamped with `current_surface_id()` |
/// | `iced_layershell-0.19.1/src/event.rs:176` | `DispatchMessage::Unfocus` → `WindowEvent::Unfocus` |
/// | `iced_layershell-0.19.1/src/conversion.rs:148` | `WindowEvent::Unfocus` → `iced::window::Event::Unfocused` |
/// | `multi_window.rs:945–951` | broadcast as `subscription::Event::Interaction { window, event, status }` |
/// | here | `listen_with` turns it into [`Message::Unfocused`] |
///
/// There is no separate "popup dismissed" or "grab lost" event in this stack;
/// `Unfocused` is the whole story. (Its counterpart `Focused` exists too, but
/// it fires on surface *creation* as well as on real keyboard focus —
/// `layershellev`'s `push_window` calls `update_current_surface` — so it is
/// not a reliable "I now have the keyboard" signal and nothing here uses it.)
///
/// # Does the popover ever actually get keyboard focus?
///
/// On niri, yes, the moment it maps — verified in niri's own source rather
/// than assumed, because the answer decides whether Escape can work at all.
/// `src/handlers/layer_shell.rs` (niri 26.04):
///
/// ```text
/// let on_demand = layer.cached_state().keyboard_interactivity
///     == wlr_layer::KeyboardInteractivity::OnDemand;
/// if was_unmapped && on_demand {
///     self.niri.layer_shell_on_demand_focus = Some(layer.clone());
/// }
/// ```
///
/// and `Niri::focus_layer_surface_if_on_demand` (`src/niri.rs`) clears that
/// focus when anything which is *not* an on-demand layer surface is clicked —
/// which is what turns "click elsewhere" into the `wl_keyboard.leave` above.
/// A compositor that only granted on-demand focus on click would still work
/// (the trigger and Escape-after-clicking-in still function), it would just
/// not dismiss on click-away; that is a compositor-portability note, not a
/// niri one.
///
/// # Why the filter takes a `fn`, not a closure
///
/// `listen_with`'s parameter is a bare `fn(Event, Status, window::Id) ->
/// Option<Message>` (`iced_futures-0.14.0/src/event.rs:26`) — it is part of
/// the subscription's *identity hash*, so it cannot capture. That is exactly
/// why [`Message::Unfocused`] carries the raw `window::Id` instead of the
/// filter deciding whether the event is interesting: the filter has no access
/// to `PopoverManager`, so the decision has to happen in `update`.
pub fn subscription() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, id| match event {
        iced::Event::Window(window::Event::Unfocused) => Some(Message::Unfocused(id)),
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::Escaped(id)),
        _ => None,
    })
}

/// The open/close state machine, exercised without a compositor — which is
/// the whole reason [`PopoverManager::update`] returns an [`Action`] instead
/// of a `Task`. The surface-side half (spawning, removing, geometry) is
/// tested in `main.rs`'s own `mod tests`.
#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in Id for a surface the runtime would have minted.
    fn id() -> window::Id {
        window::Id::unique()
    }

    #[test]
    fn a_fresh_manager_has_nothing_open() {
        let manager = PopoverManager::default();
        assert_eq!(manager.open, None);
    }

    #[test]
    fn a_trigger_opens_its_popover() {
        let mut manager = PopoverManager::default();

        let action = manager.update(Message::Triggered(PopoverKind::QuickSettings));

        assert_eq!(
            action,
            Action::Open {
                kind: PopoverKind::QuickSettings,
                close: None,
            }
        );
        // Still nothing recorded: the Id does not exist until `main.rs` has
        // asked the runtime for the surface.
        assert_eq!(manager.open, None);
    }

    #[test]
    fn opened_records_the_surface_the_runtime_minted() {
        let mut manager = PopoverManager::default();
        let surface = id();

        let _ = manager.update(Message::Triggered(PopoverKind::QuickSettings));
        manager.opened(surface, PopoverKind::QuickSettings);

        assert_eq!(manager.open, Some((surface, PopoverKind::QuickSettings)));
    }

    #[test]
    fn triggering_the_open_popover_again_closes_it() {
        let mut manager = PopoverManager::default();
        let surface = id();
        manager.opened(surface, PopoverKind::QuickSettings);

        let action = manager.update(Message::Triggered(PopoverKind::QuickSettings));

        assert_eq!(action, Action::Close(surface));
        assert_eq!(manager.open, None);
    }

    /// The spec's "only one popover open at a time": a trigger for a
    /// *different* kind closes the incumbent in the same action that opens
    /// the newcomer, so there is never a moment with two. Exercised with the
    /// two real kinds this crate ships — `TrayMenu` replaced the
    /// `#[cfg(test)] Probe` kind Stage 16 used to reach this branch before a
    /// second real kind existed (see that stage's handoff).
    #[test]
    fn opening_a_different_popover_closes_the_incumbent_first() {
        let mut manager = PopoverManager::default();
        let incumbent = id();
        manager.opened(incumbent, PopoverKind::QuickSettings);

        let action = manager.update(Message::Triggered(PopoverKind::TrayMenu));

        assert_eq!(
            action,
            Action::Open {
                kind: PopoverKind::TrayMenu,
                close: Some(incumbent),
            },
            "the incumbent must be closed by the same action that opens its \
             replacement — there is never a moment with two popovers"
        );
        // The incumbent's entry is gone; the newcomer's Id does not exist yet
        // (`main.rs` mints it and calls `opened`).
        assert_eq!(manager.open, None);
    }

    #[test]
    fn losing_keyboard_focus_closes_the_open_popover() {
        let mut manager = PopoverManager::default();
        let surface = id();
        manager.opened(surface, PopoverKind::QuickSettings);

        assert_eq!(
            manager.update(Message::Unfocused(surface)),
            Action::Close(surface)
        );
        assert_eq!(manager.open, None);
    }

    /// The Id on an unfocus report is not trustworthy — layershellev stamps
    /// it with the most recently *clicked* surface, so clicking the bar makes
    /// the popover's own unfocus arrive with the bar's Id. Dismissal must not
    /// depend on the Ids matching (see `PopoverManager::update`'s note).
    #[test]
    fn an_unfocus_attributed_to_another_surface_still_closes_the_popover() {
        let mut manager = PopoverManager::default();
        let popover = id();
        let bar = id();
        manager.opened(popover, PopoverKind::QuickSettings);

        assert_eq!(
            manager.update(Message::Unfocused(bar)),
            Action::Close(popover),
            "the surface Id reported is the last-clicked one, not the one \
             that lost focus"
        );
    }

    #[test]
    fn escape_closes_the_open_popover() {
        let mut manager = PopoverManager::default();
        let surface = id();
        manager.opened(surface, PopoverKind::QuickSettings);

        assert_eq!(
            manager.update(Message::Escaped(surface)),
            Action::Close(surface)
        );
        assert_eq!(manager.open, None);
    }

    /// Escape and stray unfocus events arrive constantly in a real session
    /// (every time any surface of ours is clicked); with nothing open they
    /// must be free.
    #[test]
    fn dismissing_nothing_is_a_no_op() {
        let mut manager = PopoverManager::default();

        assert_eq!(manager.update(Message::Unfocused(id())), Action::None);
        assert_eq!(manager.update(Message::Escaped(id())), Action::None);
        assert_eq!(manager.open, None);
    }

    #[test]
    fn a_closed_surface_is_forgotten() {
        let mut manager = PopoverManager::default();
        let surface = id();
        manager.opened(surface, PopoverKind::QuickSettings);

        manager.closed(surface);

        assert_eq!(manager.open, None);
    }

    /// The outgoing popover's `Closed` event arrives *after* the incoming one
    /// was recorded, so `closed` must check the Id or a swap would leave the
    /// manager thinking nothing is open while a surface is on screen.
    #[test]
    fn a_stale_close_does_not_forget_the_current_popover() {
        let mut manager = PopoverManager::default();
        let outgoing = id();
        let incoming = id();
        manager.opened(incoming, PopoverKind::QuickSettings);

        manager.closed(outgoing);

        assert_eq!(manager.open, Some((incoming, PopoverKind::QuickSettings)));
    }

    /// The surface height comes from each kind's own content module, not a
    /// magic number — Stage 17 replaced the provisional `list_row * 6.0`
    /// guess with `quick_settings::height`; Stage 21 adds `tray_menu::
    /// height` for the second kind the same way. This pins the delegation
    /// rather than either function's formula.
    #[test]
    fn every_kinds_height_comes_from_its_content_module() {
        let theme = Theme::saola();
        assert_eq!(
            PopoverKind::QuickSettings.height(&theme),
            crate::popovers::quick_settings::height(&theme)
        );
        assert_eq!(
            PopoverKind::TrayMenu.height(&theme),
            crate::popovers::tray_menu::height(&theme)
        );
        for kind in [PopoverKind::QuickSettings, PopoverKind::TrayMenu] {
            assert!(kind.height(&theme) > 0.0);
        }
    }

    #[test]
    fn is_open_reports_the_open_kind_and_nothing_else() {
        let mut manager = PopoverManager::default();
        assert!(!manager.is_open(PopoverKind::QuickSettings));
        assert!(!manager.is_open(PopoverKind::TrayMenu));

        manager.opened(id(), PopoverKind::TrayMenu);

        assert!(manager.is_open(PopoverKind::TrayMenu));
        assert!(!manager.is_open(PopoverKind::QuickSettings));
    }

    #[test]
    fn close_closes_only_a_matching_open_kind() {
        let mut manager = PopoverManager::default();
        let surface = id();
        manager.opened(surface, PopoverKind::TrayMenu);

        assert_eq!(
            manager.close(PopoverKind::QuickSettings),
            Action::None,
            "closing a kind that isn't the open one must not touch it"
        );
        assert!(manager.is_open(PopoverKind::TrayMenu));

        assert_eq!(manager.close(PopoverKind::TrayMenu), Action::Close(surface));
        assert!(!manager.is_open(PopoverKind::TrayMenu));
    }

    #[test]
    fn close_with_nothing_open_is_a_no_op() {
        let mut manager = PopoverManager::default();
        assert_eq!(manager.close(PopoverKind::TrayMenu), Action::None);
    }
}
