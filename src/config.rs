//! `panel.kdl` — the user-facing knobs for layout, module placement, the
//! mark (and its launcher), and color overrides (style guide §10; PLAN.md
//! Stage 14).
//!
//! # Why `kdl`, and why by hand (teaching note)
//!
//! [`kdl`] is the reference Rust implementation of the
//! [KDL](https://kdl.dev) document language — the format the style guide's
//! own `panel { }` sketch is written in. This module walks the parsed
//! [`KdlDocument`] explicitly (`document.get("panel")`, then
//! `.get_arg("style")`, `.children()`, …) instead of reaching for a
//! derive-macro-based deserializer (the way `serde` does for
//! `saola-theme`'s TOML files). Two reasons, both from PLAN.md Stage 14:
//! explicit walks are what a newer-to-Rust reader can trace line by line
//! (CLAUDE.md's teaching-note rule), and a hand-written extractor can give
//! *precise* per-knob warnings ("unknown module `notifications` in `right`
//! — skipped") that a derive macro's one-shot "deserialize failed" error
//! cannot.
//!
//! # Resilience rules (binding — see PLAN.md Stage 14 and CLAUDE.md)
//!
//! A status bar must never fail to start because of a config typo:
//!
//! - **No file at all** → [`PanelConfig::default`], silently. This is the
//!   expected case for anyone who hasn't written a `panel.kdl` yet.
//! - **File present but not valid KDL** ("garbage") → one `eprintln!`
//!   warning naming the file and the parse error, then the **whole**
//!   config falls back to [`PanelConfig::default`] — not a partial merge.
//!   Unlike `saola_tokens::Theme`'s TOML loader (which lets a partial file
//!   override just the fields it mentions, because `#[serde(default)]`
//!   makes every *field* independently optional), a `panel.kdl` that
//!   doesn't even parse gives this module nothing safe to partially trust,
//!   so it discards the whole attempt rather than guessing which half of a
//!   syntactically broken document was "the good part".
//! - **File parses, but a single knob's *value* is nonsense** (an
//!   unrecognized `style`, an `mark` string that matches no known prefix,
//!   an `ink` that isn't `"#RRGGBB"`, …) → warn on that one knob, keep
//!   parsing the rest of the document, and default just that knob. A typo
//!   in `colors { }` must not blank out `left { }`.
//! - **Unknown module name** in a `left`/`center`/`right` list (the style
//!   guide's own sketch includes `notifications`, which this phase
//!   excludes per PLAN.md — see [`ModuleName::parse`]) → warn + skip that
//!   one entry; the rest of the list still loads.
//!
//! Every one of these paths is unit-tested below (`default_config_parses`,
//! `full_config_parses`, `partial_config_parses`, `garbage_falls_back_to_defaults`,
//! `unknown_module_is_skipped_with_the_rest_of_the_list_intact`).
//!
//! # How module lists become bar regions (read this before touching `main.rs`)
//!
//! [`PanelConfig::left`] / `.center` / `.right` are `Vec<ModuleName>` — a
//! *name*, not a view. `main.rs`'s `Panel::module_view` is the other half of
//! this: one `match` arm per [`ModuleName`] variant, each producing that
//! module's real `Element` (`self.mark.view(t).map(Message::Mark)`, etc).
//! Keeping "which modules, in which order" (this file) separate from
//! "how each one renders" (`main.rs`) is what lets a config change reorder
//! or drop modules without either file needing to know the other's
//! internals — `main.rs`'s `match` is exhaustive over [`ModuleName`], so
//! the compiler catches a forgotten arm the moment a variant is added here.
//!
//! # How `colors { }` reaches the theme
//!
//! [`ColorOverrides::apply`] mutates a `saola_theme::tokens::Palette` in
//! place — `main.rs` calls it once, on `Theme::saola()`'s own `palette`
//! field, before `Panel::new` ever runs. This works because
//! `saola_tokens::Palette` (and the rest of `Theme`) is **plain data**: no
//! trait objects, no interior mutability, nothing that would make "swap out
//! three fields after construction" awkward. Only `palette.{ink,paper,
//! accent}` are exposed as override knobs — matching the style guide's
//! `colors { ink; paper; accent }` sketch — never `on_ink`/`on_paper` (the
//! alpha-stepped role tables): those are derived from the *built-in* ivory
//! and ink at fixed alpha steps (`OnSurface::on_ink`/`on_paper` in
//! `saola-tokens`), not recomputed from a custom palette, so a `colors { }`
//! override changes the three identity colors but not (yet) the text/fill
//! roles layered on top of them — a known limitation of the theme crate,
//! not something this module works around.

use std::fmt;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlValue};
use saola_theme::tokens::{Color, Palette};

/// The two panel layouts the style guide defines (§7/§10). Only [`Ledger`]
/// actually renders today — `Panel::bar_view` is Ledger-shaped and Stage 15
/// is what teaches `Panel::view` to draw Islands. This field exists *now*,
/// ahead of that stage, precisely so Stage 15 reads `style "islands"` out
/// of a config knob that already exists rather than inventing a throwaway
/// CLI flag first and migrating it later (PLAN.md Stage 14's own stated
/// reason for this stage's placement in the plan).
///
/// [`Ledger`]: PanelStyle::Ledger
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelStyle {
    #[default]
    Ledger,
    Islands,
}

/// Which screen edge the bar's layer-shell surface anchors to. Replaces the
/// `--bottom` CLI flag (PLAN.md Stage 14, bullet 3) — `edge "bottom"` in
/// `panel.kdl` is now the only way to flip it; `main.rs` no longer reads
/// `std::env::args()` at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Edge {
    #[default]
    Top,
    Bottom,
}

/// Every module name this phase recognizes in a `left`/`center`/`right`
/// list, per PLAN.md Stage 14's list: `mark`, `window-title`, `mpris`,
/// `clock`, `niri-columns`, `volume`, `network`, `bluetooth`, `battery`,
/// `claude`, `tray`.
///
/// `notifications` (present in the style guide's own `panel { }` sketch) is
/// **deliberately absent** — Phase 2 excludes everything notifications
/// (CLAUDE.md, PLAN.md) — so it falls through [`ModuleName::parse`]'s `None`
/// arm and is warned-and-skipped exactly like a typo would be.
///
/// [`Tray`] was recognized here before `src/modules/tray/` existed, so that a
/// `panel.kdl` written against the style guide's own sketch kept parsing
/// cleanly in the meantime rather than needing an edit when Stage 18 landed.
/// It now maps to the real module (`modules::tray`), and every name in this
/// enum has a live `Panel::module_view` arm.
///
/// [`Tray`]: ModuleName::Tray
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleName {
    Mark,
    /// The focused window's title (style guide §7, 2026-08-01) — ambient text
    /// to the right of the mark. Its own knobs live in the top-level
    /// `window-title { }` block, not here: this enum only says *where* a
    /// module sits (see [`WindowTitleConfig`]).
    WindowTitle,
    Mpris,
    Clock,
    NiriColumns,
    Volume,
    Network,
    Bluetooth,
    Battery,
    Claude,
    Tray,
}

impl ModuleName {
    /// The wire name → variant mapping. `None` means "not a module this
    /// phase knows about" — the caller's job is to warn and skip, not to
    /// treat that as a fatal parse error (see the module doc comment's
    /// resilience rules).
    fn parse(name: &str) -> Option<Self> {
        match name {
            "mark" => Some(Self::Mark),
            "window-title" => Some(Self::WindowTitle),
            "mpris" => Some(Self::Mpris),
            "clock" => Some(Self::Clock),
            "niri-columns" => Some(Self::NiriColumns),
            "volume" => Some(Self::Volume),
            "network" => Some(Self::Network),
            "bluetooth" => Some(Self::Bluetooth),
            "battery" => Some(Self::Battery),
            "claude" => Some(Self::Claude),
            "tray" => Some(Self::Tray),
            _ => None,
        }
    }
}

/// Where the bar's mark glyph (style guide §8) comes from, chosen by the
/// top-level `mark "…"` directive. Consumed by `modules::mark::Mark`, which
/// stores one of these and switches on it in `view` — see that module for
/// how each variant actually reaches the screen (the two `Builtin*`
/// variants route through `crate::icons`' embedded-asset machinery from
/// Stage 8; [`File`] loads an SVG from disk at boot via
/// `iced::widget::svg::Handle::from_path`, **not** `icons::icon` — the
/// embedded-asset helper only knows about `include_bytes!`-compiled-in
/// icons, and a user-supplied path is the opposite of that by definition).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MarkSource {
    /// `mark "builtin:horns"` — the default Saola mark. Also the fallback
    /// for an absent `mark` directive, matching today's hardcoded behavior.
    #[default]
    BuiltinHorns,
    /// `mark "builtin:notch"` — the alternative mark from style guide §8.
    BuiltinNotch,
    /// `mark "file:~/.icons/arch.svg"` — an arbitrary user SVG. `~` is
    /// expanded against `$HOME` at parse time (see [`expand_tilde`]); the
    /// file is not read until `Mark::view` builds an `svg::Handle` from it,
    /// so a path that doesn't exist yet is a render-time SVG error (handled
    /// by iced/resvg), not a config-parse error.
    File(PathBuf),
    /// `mark "none"` — no mark at all. Distinct from simply leaving `mark`
    /// out of a module list: this is "the mark module has nothing to draw",
    /// not "the mark module isn't in the bar".
    None,
}

impl MarkSource {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "builtin:horns" => Some(Self::BuiltinHorns),
            "builtin:notch" => Some(Self::BuiltinNotch),
            "none" => Some(Self::None),
            other => other
                .strip_prefix("file:")
                .map(|path| Self::File(expand_tilde(path))),
        }
    }
}

/// Which glyph heads the Claude Code module's session-dot row, chosen by
/// the top-level `claude-icon "…"` directive. A closed two-value set (like
/// `style`/`edge`, unlike `mark`'s open `file:` form): both glyphs are
/// embedded brand assets (`crate::icons::Icon::{Anthropic, ClaudeCode}`),
/// and a user SVG here would be a third brand where the design language
/// wants a known mark — anyone who feels strongly can still ask for the
/// knob to grow a `file:` form later. Consumed by
/// `modules::claude::ClaudeCode`, which stores one of these at boot and
/// switches on it in `view` — the same "config picks, module renders" split
/// as [`MarkSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeIcon {
    /// `claude-icon "anthropic"` — the Anthropic "A" mark. Also the
    /// fallback for an absent directive, matching the default glyph the
    /// module drew before this knob existed.
    #[default]
    Anthropic,
    /// `claude-icon "claude-code"` — Claude Code's own terminal-window mark.
    ClaudeCode,
}

impl ClaudeIcon {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" => Some(Self::Anthropic),
            "claude-code" => Some(Self::ClaudeCode),
            _ => None,
        }
    }
}

/// What the focused-window-title module does with a title longer than its
/// character limit (style guide §7's two modes; the motion of the second is
/// specced exactly in §5).
///
/// A closed two-value set, like `style`/`edge`/`claude-icon` — so an
/// unrecognized value warns and falls back to the default, per the module
/// doc comment's per-knob resilience rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TitleOverflow {
    /// `overflow "truncate"` — cut at the limit with a single `…`, no motion.
    /// The default, and the whole of a stock Saola desktop's behavior.
    #[default]
    Truncate,
    /// `overflow "marquee"` — the opt-in ping-pong sweep (style guide §5:
    /// 2s dwell, 24px/s linear, 2s dwell, back).
    ///
    /// The one knob in `panel.kdl` that starts an animation, which is why it
    /// is opt-in and why the module gates it so tightly: a title that fits
    /// (or a window losing focus) renders exactly like
    /// [`Truncate`](Self::Truncate) and runs no timer at all. See
    /// `modules::window_title` for the state machine and the gate.
    Marquee,
}

impl TitleOverflow {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "truncate" => Some(Self::Truncate),
            "marquee" => Some(Self::Marquee),
            _ => None,
        }
    }
}

/// The default `max-chars`, straight from the style guide (§7: "Titles cap at
/// a configurable **character limit** (default `50`)").
pub const DEFAULT_TITLE_MAX_CHARS: usize = 50;

/// `window-title { max-chars 50; overflow "truncate" }` — the focused-window
/// title's two knobs, resolved to concrete values (unlike `margin`/`height`,
/// neither of these has a theme token to defer to, so there is no `Option`
/// to resolve later).
///
/// `max_chars` is a count of **characters**, not pixels: approximate under a
/// proportional face, but the knob a human can reason about (style guide §7).
/// `usize` because that is what the truncation walks with; the parser is what
/// guarantees it is positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowTitleConfig {
    pub max_chars: usize,
    pub overflow: TitleOverflow,
}

impl Default for WindowTitleConfig {
    fn default() -> Self {
        Self {
            max_chars: DEFAULT_TITLE_MAX_CHARS,
            overflow: TitleOverflow::Truncate,
        }
    }
}

/// The built-in launcher command for an absent `launcher` directive — see
/// [`read_launcher`]. `fuzzel` is the default app launcher this panel ships
/// alongside; anyone running a different one overrides it with one line.
pub const DEFAULT_LAUNCHER: &str = "fuzzel";

/// A leading `~/` (or a bare `~`) expands against `$HOME`; anything else
/// passes through unchanged. Deliberately minimal — no `~user/` form, no
/// crate dependency — because the style guide's own example
/// (`file:~/.icons/arch.svg`) only needs the common case.
fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_with_home(path, std::env::var_os("HOME"))
}

/// The testable core of [`PanelConfig::resolve_path`]'s directory chain:
/// takes every environment variable as a plain argument instead of reading
/// the environment itself, for exactly the reason [`expand_tilde_with_home`]
/// below does — precedence can be unit-tested without mutating (and thereby
/// racing every other test in this binary against) the process's real
/// environment.
///
/// An env var set to the **empty string** is treated as unset and falls
/// through to the next rung — the XDG spec's own rule for
/// `$XDG_CONFIG_HOME` ("if $XDG_CONFIG_HOME is either not set or empty"),
/// applied uniformly to `$SAOLA_CONFIG_DIR` too, since `VAR= cmd` is how a
/// shell one-liner *clears* a variable, not how it names a directory.
fn config_dir_from(
    cli: Option<&Path>,
    saola: Option<std::ffi::OsString>,
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(dir) = cli {
        return Some(dir.to_path_buf());
    }
    if let Some(saola) = saola {
        if !saola.is_empty() {
            return Some(PathBuf::from(saola));
        }
    }
    if let Some(xdg) = xdg {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("saola"));
        }
    }
    // The same empty-means-unset rule as the two vars above — a `HOME=""`
    // would otherwise produce the *relative* path `.config/saola`, i.e. a
    // config resolved against whatever directory the panel happened to be
    // started from.
    home.filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config/saola"))
}

/// The testable core of [`expand_tilde`]: takes `$HOME` as a plain
/// argument instead of reading the environment itself, so the tilde-
/// expansion behavior can be unit-tested without mutating (and thereby
/// racing every other test in this binary against) the process's real
/// `$HOME`.
fn expand_tilde_with_home(path: &str, home: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home {
            return PathBuf::from(home).join(rest);
        }
    } else if path == "~" {
        if let Some(home) = home {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}

/// `colors { ink "#…"; paper "#…"; accent "#…" }` — the style guide's own
/// three identity colors, each independently optional. `None` means "the
/// built-in Saola value" (see [`ColorOverrides::apply`]), so a `colors { }`
/// block that only mentions `accent` leaves `ink`/`paper` untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorOverrides {
    pub ink: Option<Color>,
    pub paper: Option<Color>,
    pub accent: Option<Color>,
}

impl ColorOverrides {
    /// Overwrites only the fields that were actually set — see the module
    /// doc comment's "How `colors { }` reaches the theme" section for why
    /// this mutates `palette` in place rather than producing a new `Theme`.
    pub fn apply(&self, palette: &mut Palette) {
        if let Some(ink) = self.ink {
            palette.ink = ink;
        }
        if let Some(paper) = self.paper {
            palette.paper = paper;
        }
        if let Some(accent) = self.accent {
            palette.accent = accent;
        }
    }
}

/// The whole of `panel.kdl`, resolved to typed values — loaded at boot
/// (`PanelConfig::load`) and re-loaded live whenever the file changes on
/// disk ([`Self::reload_from`], driven by the `config_watch` subscription;
/// `main.rs`'s reload arm is what re-applies the result to the running
/// panel).
///
/// `margin`/`height` are `Option<f32>` rather than plain `f32`: "absent
/// knob → today's behavior" (the module doc comment's resilience rules)
/// needs a theme in hand to know what "today's behavior" *is* (the
/// `sizes.panel_bar`/`sizes.panel_margin_ledger` tokens), and `PanelConfig`
/// itself is built before `main.rs` necessarily has a `&Theme` reference
/// threaded all the way through parsing. Resolving the `Option` against a
/// theme is [`PanelConfig::height`]/[`PanelConfig::margin`]'s job, called at
/// the two sites in `main.rs`'s layer-shell setup that actually need a
/// concrete number — this is the same "tokens are the only source of a real
/// default number" discipline CLAUDE.md asks of the rest of the codebase.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelConfig {
    pub style: PanelStyle,
    pub edge: Edge,
    pub margin: Option<f32>,
    pub height: Option<f32>,
    pub left: Vec<ModuleName>,
    pub center: Vec<ModuleName>,
    pub right: Vec<ModuleName>,
    pub mark: MarkSource,
    /// Which command clicking the mark glyph runs (`modules::mark::Mark`'s
    /// click handler — see that module's doc comment). `Some(cmd)` is the
    /// command line to spawn (default [`DEFAULT_LAUNCHER`], `"fuzzel"`);
    /// `None` is the `launcher "none"` directive, which disables the mark's
    /// click behavior entirely (it renders as the same bare, unclickable
    /// glyph the panel always drew before this knob existed). Resolved at
    /// parse time, same as `mark` above.
    pub launcher: Option<String>,
    /// Which glyph the Claude Code module leads its dot row with — see
    /// [`ClaudeIcon`].
    pub claude_icon: ClaudeIcon,
    /// The focused-window-title module's own knobs — see
    /// [`WindowTitleConfig`]. A *block* rather than two top-level directives
    /// because that is the shape the style guide's own §10 sketch writes, and
    /// because both knobs belong to one module.
    pub window_title: WindowTitleConfig,
    pub colors: ColorOverrides,
}

impl Default for PanelConfig {
    /// The style guide's own module order: mark+window-title left,
    /// clock+niri-columns center,
    /// mpris/volume/network/bluetooth/battery/claude/tray right (media
    /// moved here from the left region on 2026-08-01 — style guide §7,
    /// "Media is a status glyph" — and heads the cluster, ahead of
    /// volume; Bluetooth sits with the other radios, immediately after
    /// Wi-Fi; `claude` and `tray` close the region — and `main.rs` renders
    /// those two as standalone groups *beside* the status cluster rather
    /// than inside it, see `Panel::bar_view`'s right-region split, decided
    /// 2026-08-01: the Claude Code dots are their own island immediately
    /// left of the tray). Top edge, ledger style, the built-in horns mark
    /// clicking through to `fuzzel` ([`DEFAULT_LAUNCHER`]), no color
    /// overrides. This is also what an absent `panel.kdl` produces, and
    /// what a garbage one falls back to in full.
    fn default() -> Self {
        PanelConfig {
            style: PanelStyle::Ledger,
            edge: Edge::Top,
            margin: None,
            height: None,
            left: vec![ModuleName::Mark, ModuleName::WindowTitle],
            center: vec![ModuleName::Clock, ModuleName::NiriColumns],
            right: vec![
                ModuleName::Mpris,
                ModuleName::Volume,
                ModuleName::Network,
                ModuleName::Bluetooth,
                ModuleName::Battery,
                ModuleName::Claude,
                ModuleName::Tray,
            ],
            mark: MarkSource::BuiltinHorns,
            launcher: Some(DEFAULT_LAUNCHER.to_string()),
            claude_icon: ClaudeIcon::Anthropic,
            window_title: WindowTitleConfig::default(),
            colors: ColorOverrides::default(),
        }
    }
}

/// A KDL document that failed to parse at all — the "garbage file" case.
/// Deliberately the *only* error this module has: once the document parses,
/// every remaining problem (a bad knob value, an unknown module) is handled
/// knob-by-knob with a warning, never by returning `Err` — see the module
/// doc comment.
#[derive(Debug)]
pub struct ConfigError(kdl::KdlError);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl PanelConfig {
    /// The height (logical pixels) the bar's layer-shell surface and
    /// exclusive zone should use: the `height` knob if set, otherwise
    /// `theme.sizes.panel_bar` — today's hardcoded value.
    pub fn height(&self, theme: &saola_theme::Theme) -> f32 {
        self.height.unwrap_or(theme.sizes.panel_bar)
    }

    /// The panel's **inset from the screen edge** (logical pixels,
    /// horizontal): the `margin` knob if set, otherwise
    /// `panel_margin_ledger` — for **both** styles. Islands originally used
    /// the wider `panel_margin_islands` (26); Jordan matched the two on
    /// 2026-08-01 (islands now share the ledger bar's inset, recorded in the
    /// style guide's Sizes table), so the style no longer changes the
    /// resolved margin. The match stays written out so a future style with a
    /// genuinely different inset has an obvious slot.
    ///
    /// This is the style guide's own meaning of "panel margin", and as of
    /// the floating-pill change it is literally that: `main.rs` feeds it to
    /// the bar's layer-shell side margins, so the surface stops this many
    /// pixels short of each screen edge. It used to be applied as padding
    /// *inside* an edge-to-edge bar instead; the bar's content padding is
    /// now derived from its own pill radius and is not configurable (see
    /// `Panel::bar_view`). The vertical counterpart — the gap between the
    /// anchored edge and the bar — is the `sizes.panel_margin_ledger_top`
    /// token, with no config knob of its own.
    pub fn margin(&self, theme: &saola_theme::Theme) -> f32 {
        self.margin.unwrap_or(match self.style {
            PanelStyle::Ledger | PanelStyle::Islands => theme.sizes.panel_margin_ledger,
        })
    }

    /// Where `panel.kdl` lives: the resolved config **directory** joined
    /// with the fixed file name. `main` calls this exactly once, at boot,
    /// and threads the result to both consumers — [`Self::load`] and the
    /// `config_watch` worker — so the loader and the watcher cannot end up
    /// pointed at different files.
    ///
    /// The directory itself resolves most-specific-first, the same
    /// precedence the style/edge flags already have over their file knobs:
    ///
    /// 1. **`--config-dir <dir>`** — [`CliOverrides::config_dir`], a
    ///    per-run override (iterate on a scratch config while the real
    ///    session's panel keeps running against the default).
    /// 2. **`$SAOLA_CONFIG_DIR`** — the Saola desktop's own env var, for a
    ///    session manager to export once and every Saola component to
    ///    honor. Scoped to Saola alone, unlike `$XDG_CONFIG_HOME` below,
    ///    which relocates *every* application's config.
    /// 3. **`$XDG_CONFIG_HOME/saola`** — the XDG base-directory spec.
    /// 4. **`~/.config/saola`** — the spec's own fallback for an unset
    ///    `$XDG_CONFIG_HOME` (and PLAN.md Stage 14's stated default).
    ///
    /// `None` only when nothing in the chain resolves — no flag, no Saola
    /// or XDG var, and no `$HOME` — an environment this resilient-by-design
    /// loader treats the same as "no file": defaults, and no watch.
    pub(crate) fn resolve_path(cli_dir: Option<&Path>) -> Option<PathBuf> {
        config_dir_from(
            cli_dir,
            std::env::var_os("SAOLA_CONFIG_DIR"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
        .map(|dir| dir.join("panel.kdl"))
    }

    /// Load the config at boot, from the path [`Self::resolve_path`] gave
    /// `main` (`None` — nothing in the resolution chain was set — loads
    /// pure defaults; not a warning-worthy situation: a container/CI
    /// environment with no `$HOME` is not "broken config", it's "no config
    /// is possible here"). Never fails — see the module doc comment's
    /// resilience rules; every error path prints a warning (via
    /// `eprintln!`, since this runs before iced's event loop exists — there
    /// is no bar surface yet to show an error on) and returns a value, not
    /// a `Result`.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        Self::load_from(path)
    }

    fn load_from(path: &Path) -> Self {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            // Covers both "the file doesn't exist" (the common case: nobody
            // has written a panel.kdl yet) and any other I/O error (e.g.
            // permissions) — both degrade to defaults silently. An I/O
            // error here is not "malformed KDL", so it does not get the
            // stderr warning that a parse failure does.
            Err(_) => return Self::default(),
        };
        match Self::parse(&contents) {
            Ok(config) => config,
            Err(err) => {
                eprintln!(
                    "saola-panel: {} is not valid KDL ({err}) — using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Re-read the config for a **live reload** (`config_watch` calls this
    /// after inotify reports the file changed). `None` means "keep whatever
    /// config the panel is already running".
    ///
    /// The resilience rules here deliberately differ from [`Self::load_from`]
    /// in exactly one case, because the two run at different moments:
    ///
    /// - **File missing** → `Some(default)`. At boot that meant "nobody wrote
    ///   a config"; mid-session it means the user *deleted* it, and reverting
    ///   the running panel to defaults is what honoring that edit looks like.
    /// - **File present but not valid KDL** → warn, then `None`. Boot has no
    ///   previous config to keep, so it falls back to defaults — but a live
    ///   panel does, and a half-saved edit (or a typo mid-editing-session)
    ///   flashing the whole bar back to the stock layout would punish the
    ///   user for a keystroke. Keeping the current config until the file
    ///   parses again is the kinder failure, and the warning on stderr still
    ///   says why nothing visibly changed.
    /// - **File parses** → `Some(config)`, per-knob resilience exactly as at
    ///   boot (a typo'd knob warns and defaults; the rest of the file lands).
    pub fn reload_from(path: &Path) -> Option<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(_) => return Some(Self::default()),
        };
        match Self::parse(&contents) {
            Ok(config) => Some(config),
            Err(err) => {
                eprintln!(
                    "saola-panel: {} is not valid KDL ({err}) — keeping the current config",
                    path.display()
                );
                None
            }
        }
    }

    /// Parse a `panel.kdl` document's contents into a [`PanelConfig`].
    ///
    /// Returns `Err` **only** if `contents` isn't valid KDL at all — every
    /// other problem (a missing `panel { }` node, an absent knob, a bad
    /// knob value, an unknown module name) is resolved to a default and
    /// reported with `eprintln!` rather than failing the whole parse. This
    /// is the function the unit tests below exercise directly, without
    /// touching the filesystem.
    pub fn parse(contents: &str) -> Result<Self, ConfigError> {
        let document = KdlDocument::parse(contents).map_err(ConfigError)?;

        // No top-level `panel { }` node at all is not "garbage" — it's a
        // config file that doesn't configure the panel (an empty file is
        // the trivial case of this, and `Theme::from_toml_str("")` treats
        // an empty *theme* file the same permissive way). Every knob below
        // is read through this `Option`, so "no panel node" and "panel node
        // present but every knob absent" produce the identical result:
        // `PanelConfig::default()`.
        let body = document.get("panel").and_then(|node| node.children());

        let style = read_arg_str(body, "style")
            .and_then(|value| match_or_warn(value, "style", parse_style))
            .unwrap_or_default();

        let edge = read_arg_str(body, "edge")
            .and_then(|value| match_or_warn(value, "edge", parse_edge))
            .unwrap_or_default();

        let margin = read_arg_number(body, "margin");
        let height = read_arg_number(body, "height");

        let left = read_module_list(body, "left").unwrap_or_else(|| PanelConfig::default().left);
        let center =
            read_module_list(body, "center").unwrap_or_else(|| PanelConfig::default().center);
        let right = read_module_list(body, "right").unwrap_or_else(|| PanelConfig::default().right);

        let mark = read_arg_str(body, "mark")
            .and_then(|value| match_or_warn(value, "mark", MarkSource::parse))
            .unwrap_or_default();

        let launcher = read_launcher(body);

        let claude_icon = read_arg_str(body, "claude-icon")
            .and_then(|value| match_or_warn(value, "claude-icon", ClaudeIcon::parse))
            .unwrap_or_default();

        let window_title = read_window_title(body);

        let colors = read_colors(body);

        Ok(PanelConfig {
            style,
            edge,
            margin,
            height,
            left,
            center,
            right,
            mark,
            launcher,
            claude_icon,
            window_title,
            colors,
        })
    }
}

/// The `--help` text, kept next to [`CliOverrides::parse`] so the two lists
/// of flags can't drift apart without the mismatch staring the editor in the
/// face (and a test cross-checks them). `main` prints this and exits before
/// any config is read or any Wayland connection is attempted.
pub const HELP: &str = "\
saola-panel — status bar for the Saola desktop environment

Usage: saola-panel [FLAGS]

Flags override the matching knobs in panel.kdl; anything not flagged
comes from the file (or the built-in default when there is no file).

  --ledger             ledger layout: one full-width bar
  --islands            islands layout: floating ink pill clusters
  --top                anchor the panel to the top edge of the screen
  --bottom             anchor the panel to the bottom edge
  --config-dir <dir>   read panel.kdl from <dir> instead of the
                       $SAOLA_CONFIG_DIR / XDG search path
                       (also spelled --config-dir=<dir>)
  -h, --help           print this help and exit
";

/// Command-line overrides for quick testing: `--ledger`, `--islands`,
/// `--top`, `--bottom`, `--config-dir <dir>`, plus `--help`. A flag beats the
/// corresponding `panel.kdl` knob, which beats the built-in default — so
/// `cargo run -- --islands` tries the Islands layout without editing (or
/// even having) a config file, and the next plain `cargo run` is back to
/// whatever the file says.
///
/// Same `Option`-per-field shape as [`ColorOverrides`]: `None` means "this
/// flag wasn't given, leave the config value alone". Only the two
/// mode-switch knobs and the config location are exposed — flags are a
/// testing convenience, not a second config surface; anything richer
/// belongs in `panel.kdl`.
///
/// (Historical note: Stage 14 removed the original v0.1 `--bottom` flag in
/// favor of `edge "bottom"` in the config file. This brings it back with
/// different semantics — an *override on top of* the file rather than the
/// only knob there is.)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliOverrides {
    pub style: Option<PanelStyle>,
    pub edge: Option<Edge>,
    /// `--config-dir <dir>` (or `--config-dir=<dir>`): read `panel.kdl`
    /// from this directory instead of the `$SAOLA_CONFIG_DIR`/XDG chain —
    /// the head of [`PanelConfig::resolve_path`]'s precedence list. Unlike
    /// `style`/`edge` above, this one is **not** applied by [`Self::apply`]
    /// (it isn't a `PanelConfig` field): it decides where the config *is*,
    /// so `main` consumes it before the file is ever read, and the
    /// live-reload watcher follows the same resolved path. It is also the
    /// field that cost this struct its `Copy` (a `PathBuf` owns heap) —
    /// callers clone instead, which is just as cheap for a value this
    /// small and this rarely passed around.
    pub config_dir: Option<PathBuf>,
    /// `--help` (or `-h`): the user asked for the flag reference. Only
    /// *recorded* here — `parse` never prints [`HELP`] or exits itself,
    /// because it also runs in tests; `main` checks this field first thing
    /// and prints-and-returns before any config load or Wayland setup.
    pub help: bool,
}

impl CliOverrides {
    /// Parse override flags from an iterator of arguments (in `main`:
    /// `std::env::args().skip(1)` — `skip(1)` drops argv\[0\], the binary's
    /// own path, which is not a flag).
    ///
    /// Generic over `S: AsRef<str>` so tests can pass `["--islands"]`
    /// (`&str` literals) while `main` passes the `String`s that
    /// `std::env::args()` yields — both types view as `&str` through
    /// `AsRef`, and the function never needs ownership of the text.
    ///
    /// Resilience matches the config file's rules: an unrecognized argument
    /// warns on stderr and is ignored — a typo on the command line must
    /// never kill the bar, same as a typo'd knob in `panel.kdl` doesn't.
    /// If contradictory flags are given (`--ledger --islands`), the last
    /// one wins, like shadowing a shell variable.
    ///
    /// `--config-dir` is the one flag that takes a value, in either
    /// conventional spelling: `--config-dir /some/dir` (the value is the
    /// next argument) or `--config-dir=/some/dir`. A `--config-dir` with no
    /// usable value — last on the line, `--config-dir=` with nothing after
    /// the `=`, an empty string, or a following argument that is itself a
    /// flag (`--config-dir --islands` is almost certainly a forgotten
    /// value, so `--islands` is *not* swallowed as a directory named
    /// "--islands": the missing value warns and the flag then applies
    /// normally; a genuine directory whose name starts with `--` can still
    /// be given via the `=` form) — warns and is ignored, the same fate as
    /// any other unusable argument. The value gets the same `~/` expansion
    /// as `mark`'s `file:` form ([`expand_tilde`]) — a shell expands `~`
    /// itself, but a value arriving through an exec line or a `.desktop`
    /// file doesn't get that pass, and "expand the common case" costs
    /// nothing.
    pub fn parse<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut overrides = Self::default();
        // `--config-dir`'s two-token form is parsed with a one-step state
        // flag rather than an extra `.next()` inside the loop body: the
        // "value" that follows may turn out to be a flag itself (see the
        // doc comment), and this shape lets such an argument fall through
        // to the ordinary `match` below instead of being consumed as a
        // directory and lost.
        let mut awaiting_dir = false;
        for arg in args {
            let arg = arg.as_ref();
            if awaiting_dir {
                awaiting_dir = false;
                if !arg.is_empty() && !arg.starts_with("--") {
                    overrides.config_dir = Some(expand_tilde(arg));
                    continue;
                }
                eprintln!("saola-panel: --config-dir needs a directory argument — ignored");
                // Deliberately no `continue`: whatever this argument was,
                // it wasn't a directory, so it gets its normal treatment
                // below (a real flag applies; anything else warns as
                // unrecognized).
            }
            match arg {
                "--ledger" => overrides.style = Some(PanelStyle::Ledger),
                "--islands" => overrides.style = Some(PanelStyle::Islands),
                "--top" => overrides.edge = Some(Edge::Top),
                "--bottom" => overrides.edge = Some(Edge::Bottom),
                "--config-dir" => awaiting_dir = true,
                "--help" | "-h" => overrides.help = true,
                other => {
                    if let Some(dir) = other.strip_prefix("--config-dir=") {
                        if dir.is_empty() {
                            eprintln!(
                                "saola-panel: --config-dir= needs a directory argument — ignored"
                            );
                        } else {
                            overrides.config_dir = Some(expand_tilde(dir));
                        }
                    } else {
                        eprintln!("saola-panel: unrecognized argument \"{other}\" — ignored");
                    }
                }
            }
        }
        if awaiting_dir {
            eprintln!("saola-panel: --config-dir needs a directory argument — ignored");
        }
        overrides
    }

    /// Overwrite `config`'s fields with whichever overrides were actually
    /// given — mirrors [`ColorOverrides::apply`]'s "only the `Some` fields
    /// touch anything" contract.
    pub fn apply(&self, config: &mut PanelConfig) {
        if let Some(style) = self.style {
            config.style = style;
        }
        if let Some(edge) = self.edge {
            config.edge = edge;
        }
    }
}

fn parse_style(value: &str) -> Option<PanelStyle> {
    match value {
        "ledger" => Some(PanelStyle::Ledger),
        "islands" => Some(PanelStyle::Islands),
        _ => None,
    }
}

fn parse_edge(value: &str) -> Option<Edge> {
    match value {
        "top" => Some(Edge::Top),
        "bottom" => Some(Edge::Bottom),
        _ => None,
    }
}

/// Applies `parser` to `value`; on `None`, warns (naming the offending knob
/// and the value that didn't match) and returns `None` so the caller falls
/// back to that knob's default — the per-knob half of the resilience rules.
fn match_or_warn<T>(value: &str, knob: &str, parser: fn(&str) -> Option<T>) -> Option<T> {
    let parsed = parser(value);
    if parsed.is_none() {
        eprintln!("saola-panel: panel.kdl: unrecognized {knob} \"{value}\" — using default");
    }
    parsed
}

/// `body.get_arg(name)` as a string, if the node exists and its first
/// positional argument is a KDL string. A node present but holding a
/// non-string value (`margin "twenty"` for a numeric knob's cousin, say)
/// falls through to `None` — same "absent knob" fallback path, no separate
/// error needed for "wrong value type" versus "missing entirely".
fn read_arg_str<'a>(body: Option<&'a KdlDocument>, name: &str) -> Option<&'a str> {
    body?.get_arg(name)?.as_string()
}

/// `body.get_arg(name)` as an `f32`, accepting either KDL integers
/// (`margin 26`) or floats (`margin 26.0`) — the style guide's own example
/// uses bare integers, but nothing about the knob's meaning requires one.
fn read_arg_number(body: Option<&KdlDocument>, name: &str) -> Option<f32> {
    let value = body?.get_arg(name)?;
    number_as_f32(value)
}

fn number_as_f32(value: &KdlValue) -> Option<f32> {
    if let Some(i) = value.as_integer() {
        return Some(i as f32);
    }
    value.as_float().map(|f| f as f32)
}

/// Reads a `left { mark; mpris }`-shaped block: a child node named `list_name`
/// whose own children are bare module-name nodes. `None` means the block
/// itself is absent (caller substitutes the default list for that region);
/// `Some(vec)` is returned even if every name inside turned out to be
/// unknown (an empty list is a legitimate "nothing in this region" choice,
/// not a signal to fall back to defaults — the two are different requests).
fn read_module_list(body: Option<&KdlDocument>, list_name: &str) -> Option<Vec<ModuleName>> {
    let list_node = body?.get(list_name)?;
    let list_body = list_node.children()?;
    let modules = list_body
        .nodes()
        .iter()
        .filter_map(|node| {
            let name = node.name().value();
            match ModuleName::parse(name) {
                Some(module) => Some(module),
                None => {
                    eprintln!(
                        "saola-panel: panel.kdl: unknown module \"{name}\" in {list_name} — skipped"
                    );
                    None
                }
            }
        })
        .collect();
    Some(modules)
}

/// Reads the top-level `launcher "…"` directive. Unlike `mark` (a closed set
/// of prefixes matched against `MarkSource::parse`) or `style`/`edge` (a
/// closed set of named values), a launcher is a free-form shell command
/// line — almost any string is "valid" here, there's no typo to warn about,
/// so this has no `match_or_warn` step. Only one string is special-cased:
/// the literal `"none"`, which disables the mark's click behavior rather
/// than being passed to `std::process::Command` as a (nonsensical) program
/// name. An absent directive resolves to [`DEFAULT_LAUNCHER`] — the mark is
/// clickable out of the box, matching the style guide's "bare-icon menu"
/// being a real menu trigger, not a decoration.
fn read_launcher(body: Option<&KdlDocument>) -> Option<String> {
    match read_arg_str(body, "launcher") {
        None => Some(DEFAULT_LAUNCHER.to_string()),
        Some("none") => None,
        Some(command) => Some(command.to_string()),
    }
}

/// Reads `window-title { max-chars 50; overflow "truncate" }` — the style
/// guide §10 sketch's own block, verbatim.
///
/// Same knob-by-knob resilience as `colors { }` just below: an absent block,
/// a block that only sets one of the two, a nonsense `overflow`, or a
/// `max-chars` that isn't a positive integer each fall back to that one
/// knob's default (warning where there is a typo to warn about) rather than
/// discarding the block.
fn read_window_title(body: Option<&KdlDocument>) -> WindowTitleConfig {
    let Some(block) = body
        .and_then(|d| d.get("window-title"))
        .and_then(|n| n.children())
    else {
        return WindowTitleConfig::default();
    };

    let defaults = WindowTitleConfig::default();
    WindowTitleConfig {
        max_chars: read_max_chars(block).unwrap_or(defaults.max_chars),
        overflow: read_arg_str(Some(block), "overflow")
            .and_then(|value| match_or_warn(value, "window-title overflow", TitleOverflow::parse))
            .unwrap_or(defaults.overflow),
    }
}

/// `max-chars` as a positive count. Unlike `margin`/`height` (which accept
/// floats, since a logical pixel can sensibly be fractional) this is a count
/// of characters, so only KDL integers qualify — and only positive ones: a
/// `max-chars 0` would render every title as a bare `…`, and a negative one
/// means nothing at all. Both warn and default, the same per-knob rule every
/// other bad value gets.
fn read_max_chars(block: &KdlDocument) -> Option<usize> {
    let value = block.get_arg("max-chars")?;
    match value.as_integer() {
        // `i128` → `usize`: the guard is what makes the conversion sound
        // (positive, and no larger than a title could ever be). Anything
        // absurd is clamped rather than rejected — a `max-chars` of a billion
        // is a strange config, not a broken one, and it simply never cuts.
        Some(count) if count > 0 => Some(count.min(usize::MAX as i128) as usize),
        _ => {
            eprintln!(
                "saola-panel: panel.kdl: window-title max-chars {value} is not a positive integer — using default"
            );
            None
        }
    }
}

/// Reads `colors { ink "#…"; paper "#…"; accent "#…" }`. Each of the three
/// is independently optional — both "the whole `colors { }` block is
/// absent" and "the block exists but only sets `accent`" leave the other
/// fields `None`, which `ColorOverrides::apply` treats as "keep the
/// built-in value".
fn read_colors(body: Option<&KdlDocument>) -> ColorOverrides {
    let Some(colors_body) = body
        .and_then(|d| d.get("colors"))
        .and_then(|n| n.children())
    else {
        return ColorOverrides::default();
    };
    ColorOverrides {
        ink: read_arg_str(Some(colors_body), "ink").and_then(|v| parse_color("ink", v)),
        paper: read_arg_str(Some(colors_body), "paper").and_then(|v| parse_color("paper", v)),
        accent: read_arg_str(Some(colors_body), "accent").and_then(|v| parse_color("accent", v)),
    }
}

fn parse_color(field: &str, value: &str) -> Option<Color> {
    match Color::parse_hex(value) {
        Ok(color) => Some(color),
        Err(err) => {
            eprintln!(
                "saola-panel: panel.kdl: colors.{field} \"{value}\" is not a valid color ({err}) — using default"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absent file (represented here as an empty document, which is what
    /// `PanelConfig::load_from` effectively sees when a real file is
    /// missing and falls back before ever calling `parse`) yields exactly
    /// today's hardcoded layout.
    #[test]
    fn default_config_parses() {
        let config = PanelConfig::parse("").expect("an empty document is valid KDL");
        assert_eq!(config, PanelConfig::default());
    }

    /// Every knob the style guide's `panel { }` sketch shows, set to
    /// non-default values, all land correctly.
    #[test]
    fn full_config_parses() {
        let kdl = r##"
            panel {
                style "islands"
                edge "bottom"
                margin 26
                height 40

                left   { mark; window-title; mpris }
                center { clock; niri-columns }
                right  { volume; network; bluetooth; battery; claude; tray }

                mark "builtin:notch"
                launcher "wofi --show drun"
                claude-icon "claude-code"

                window-title { max-chars 24; overflow "marquee" }

                colors { ink "#111111"; paper "#EEEEEE"; accent "#FF8800" }
            }
        "##;
        let config = PanelConfig::parse(kdl).expect("well-formed KDL");

        assert_eq!(config.style, PanelStyle::Islands);
        assert_eq!(config.edge, Edge::Bottom);
        assert_eq!(config.margin, Some(26.0));
        assert_eq!(config.height, Some(40.0));
        assert_eq!(
            config.left,
            vec![ModuleName::Mark, ModuleName::WindowTitle, ModuleName::Mpris]
        );
        assert_eq!(
            config.center,
            vec![ModuleName::Clock, ModuleName::NiriColumns]
        );
        assert_eq!(
            config.right,
            vec![
                ModuleName::Volume,
                ModuleName::Network,
                ModuleName::Bluetooth,
                ModuleName::Battery,
                ModuleName::Claude,
                ModuleName::Tray,
            ]
        );
        assert_eq!(config.mark, MarkSource::BuiltinNotch);
        assert_eq!(config.launcher, Some("wofi --show drun".to_string()));
        assert_eq!(config.claude_icon, ClaudeIcon::ClaudeCode);
        assert_eq!(
            config.window_title,
            WindowTitleConfig {
                max_chars: 24,
                overflow: TitleOverflow::Marquee,
            }
        );
        assert_eq!(
            config.colors,
            ColorOverrides {
                ink: Some(Color::parse_hex("#111111").unwrap()),
                paper: Some(Color::parse_hex("#EEEEEE").unwrap()),
                accent: Some(Color::parse_hex("#FF8800").unwrap()),
            }
        );
    }

    /// A config that only overrides a couple of knobs leaves the rest at
    /// their defaults — proves knob-by-knob fallback, not "any knob present
    /// disables all defaults".
    #[test]
    fn partial_config_parses() {
        let kdl = r##"
            panel {
                edge "bottom"
                colors { accent "#00FF00" }
            }
        "##;
        let config = PanelConfig::parse(kdl).expect("well-formed KDL");

        assert_eq!(config.edge, Edge::Bottom);
        assert_eq!(
            config.colors.accent,
            Some(Color::parse_hex("#00FF00").unwrap())
        );
        // Everything untouched by this document matches the default.
        assert_eq!(config.style, PanelConfig::default().style);
        assert_eq!(config.left, PanelConfig::default().left);
        assert_eq!(config.center, PanelConfig::default().center);
        assert_eq!(config.right, PanelConfig::default().right);
        assert_eq!(config.mark, PanelConfig::default().mark);
        assert_eq!(config.launcher, PanelConfig::default().launcher);
        assert_eq!(config.claude_icon, PanelConfig::default().claude_icon);
        assert_eq!(config.window_title, WindowTitleConfig::default());
        assert_eq!(config.colors.ink, None);
        assert_eq!(config.colors.paper, None);
    }

    /// Syntactically invalid KDL is the one case `parse` itself rejects —
    /// `load_from` (not exercised here, since it touches the filesystem) is
    /// what turns this `Err` into a full-default fallback plus a warning.
    #[test]
    fn garbage_is_rejected_by_parse() {
        let result = PanelConfig::parse("panel { this is not } valid kdl {{{");
        assert!(result.is_err());
    }

    /// `load_from`'s fallback path, exercised directly against a temp file
    /// so the "malformed file → full defaults" resilience rule is proven
    /// end to end, not just at the `parse` layer.
    #[test]
    fn garbage_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "saola-panel-test-garbage-{}.kdl",
            std::process::id()
        ));
        std::fs::write(&path, "panel { this is not } valid kdl {{{").unwrap();

        let config = PanelConfig::load_from(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(config, PanelConfig::default());
    }

    /// The live-reload loader's three outcomes (see [`PanelConfig::
    /// reload_from`]'s doc comment for why the malformed case differs from
    /// boot): a valid file loads, a *deleted* file reverts to defaults, and
    /// a malformed file answers `None` — "keep the running config" — rather
    /// than flashing the panel back to the stock layout mid-edit.
    #[test]
    fn reload_keeps_the_running_config_on_a_malformed_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "saola-panel-test-reload-{}.kdl",
            std::process::id()
        ));

        std::fs::write(&path, r#"panel { edge "bottom" }"#).unwrap();
        let reloaded = PanelConfig::reload_from(&path).expect("a valid file must load");
        assert_eq!(reloaded.edge, Edge::Bottom);

        std::fs::write(&path, "panel { this is not } valid kdl {{{").unwrap();
        assert_eq!(
            PanelConfig::reload_from(&path),
            None,
            "a malformed file must keep the current config, not reset it"
        );

        std::fs::remove_file(&path).ok();
        assert_eq!(
            PanelConfig::reload_from(&path),
            Some(PanelConfig::default()),
            "a deleted file is a real edit: revert to defaults"
        );
    }

    /// A missing file is not an error at all — same defaults, no warning
    /// path exercised (nothing to assert on stderr here, but the important
    /// thing is that this does not panic and does not fail the parse).
    #[test]
    fn missing_file_falls_back_to_defaults() {
        let path = std::env::temp_dir().join("saola-panel-test-definitely-missing.kdl");
        std::fs::remove_file(&path).ok();

        let config = PanelConfig::load_from(&path);

        assert_eq!(config, PanelConfig::default());
    }

    /// An unknown module name in a list is skipped, but its known
    /// neighbors still load — the list-level resilience rule.
    #[test]
    fn unknown_module_is_skipped_with_the_rest_of_the_list_intact() {
        let kdl = r##"
            panel {
                right { volume; notifications; battery }
            }
        "##;
        let config = PanelConfig::parse(kdl).expect("well-formed KDL");

        assert_eq!(
            config.right,
            vec![ModuleName::Volume, ModuleName::Battery],
            "the unknown \"notifications\" entry should be dropped, not the whole list"
        );
    }

    /// An empty list is a real answer ("nothing in this region"), distinct
    /// from an absent list ("use the default region").
    #[test]
    fn an_explicitly_empty_list_stays_empty() {
        let kdl = r##"
            panel {
                left { }
            }
        "##;
        let config = PanelConfig::parse(kdl).expect("well-formed KDL");
        assert_eq!(config.left, Vec::new());
        // The un-mentioned regions still default.
        assert_eq!(config.right, PanelConfig::default().right);
    }

    /// The name → variant mapping that module lists compose regions from —
    /// this is the pure core `main.rs`'s per-region `row(...)` calls rely
    /// on to turn a `panel.kdl` list into the actual bar layout.
    #[test]
    fn module_list_maps_names_to_module_name_variants() {
        assert_eq!(ModuleName::parse("mark"), Some(ModuleName::Mark));
        assert_eq!(
            ModuleName::parse("window-title"),
            Some(ModuleName::WindowTitle)
        );
        assert_eq!(ModuleName::parse("mpris"), Some(ModuleName::Mpris));
        assert_eq!(ModuleName::parse("clock"), Some(ModuleName::Clock));
        assert_eq!(
            ModuleName::parse("niri-columns"),
            Some(ModuleName::NiriColumns)
        );
        assert_eq!(ModuleName::parse("volume"), Some(ModuleName::Volume));
        assert_eq!(ModuleName::parse("network"), Some(ModuleName::Network));
        assert_eq!(ModuleName::parse("bluetooth"), Some(ModuleName::Bluetooth));
        assert_eq!(ModuleName::parse("battery"), Some(ModuleName::Battery));
        assert_eq!(ModuleName::parse("claude"), Some(ModuleName::Claude));
        assert_eq!(ModuleName::parse("tray"), Some(ModuleName::Tray));
        // Excluded this phase (CLAUDE.md, PLAN.md Stage 14) despite
        // appearing in the style guide's own sketch.
        assert_eq!(ModuleName::parse("notifications"), None);
        assert_eq!(ModuleName::parse("not-a-real-module"), None);
    }

    /// `mark`'s four wire forms.
    #[test]
    fn mark_source_parses_all_four_forms() {
        assert_eq!(
            MarkSource::parse("builtin:horns"),
            Some(MarkSource::BuiltinHorns)
        );
        assert_eq!(
            MarkSource::parse("builtin:notch"),
            Some(MarkSource::BuiltinNotch)
        );
        assert_eq!(MarkSource::parse("none"), Some(MarkSource::None));
        assert_eq!(
            MarkSource::parse("file:/etc/saola/mark.svg"),
            Some(MarkSource::File(PathBuf::from("/etc/saola/mark.svg")))
        );
        assert_eq!(MarkSource::parse("bogus"), None);
    }

    /// `claude-icon`'s two wire forms, plus the resilience paths: absent →
    /// the Anthropic default, and an unrecognized value → warned and
    /// defaulted, same per-knob rule as every other closed-set directive.
    #[test]
    fn claude_icon_parses_both_forms_and_defaults_on_nonsense() {
        assert_eq!(ClaudeIcon::parse("anthropic"), Some(ClaudeIcon::Anthropic));
        assert_eq!(
            ClaudeIcon::parse("claude-code"),
            Some(ClaudeIcon::ClaudeCode)
        );
        assert_eq!(ClaudeIcon::parse("sparkle"), None);

        let absent = PanelConfig::parse("panel {}").expect("well-formed KDL");
        assert_eq!(absent.claude_icon, ClaudeIcon::Anthropic);

        let typo = PanelConfig::parse(r#"panel { claude-icon "clod" }"#).expect("well-formed KDL");
        assert_eq!(typo.claude_icon, ClaudeIcon::Anthropic);
    }

    /// `launcher`'s three shapes: absent → `DEFAULT_LAUNCHER`, `"none"` →
    /// disabled, anything else → passed through verbatim as the command
    /// line. Unlike `mark_source_parses_all_four_forms`, there is no
    /// "unrecognized value" case to test — a launcher directive is a
    /// free-form command line, not a closed set of named values.
    #[test]
    fn launcher_directive_parses_all_three_forms() {
        let absent = PanelConfig::parse("panel {}").expect("well-formed KDL");
        assert_eq!(absent.launcher, Some(DEFAULT_LAUNCHER.to_string()));

        let disabled = PanelConfig::parse(r#"panel { launcher "none" }"#).expect("well-formed KDL");
        assert_eq!(disabled.launcher, None);

        let custom = PanelConfig::parse(r#"panel { launcher "wofi --show drun" }"#)
            .expect("well-formed KDL");
        assert_eq!(custom.launcher, Some("wofi --show drun".to_string()));
    }

    /// `window-title { }`'s two knobs, each independently optional — and an
    /// absent block is exactly the same as an empty one, per the style
    /// guide's defaults (50 characters, truncate).
    #[test]
    fn window_title_block_parses_both_knobs_independently() {
        let absent = PanelConfig::parse("panel {}").expect("well-formed KDL");
        assert_eq!(absent.window_title, WindowTitleConfig::default());
        assert_eq!(absent.window_title.max_chars, DEFAULT_TITLE_MAX_CHARS);
        assert_eq!(absent.window_title.overflow, TitleOverflow::Truncate);

        let empty = PanelConfig::parse("panel { window-title { } }").expect("well-formed KDL");
        assert_eq!(empty.window_title, WindowTitleConfig::default());

        let only_chars =
            PanelConfig::parse("panel { window-title { max-chars 12 } }").expect("well-formed KDL");
        assert_eq!(only_chars.window_title.max_chars, 12);
        assert_eq!(only_chars.window_title.overflow, TitleOverflow::Truncate);

        let only_overflow = PanelConfig::parse(r#"panel { window-title { overflow "marquee" } }"#)
            .expect("well-formed KDL");
        assert_eq!(
            only_overflow.window_title.max_chars,
            DEFAULT_TITLE_MAX_CHARS
        );
        assert_eq!(only_overflow.window_title.overflow, TitleOverflow::Marquee);
    }

    /// Both overflow modes are recognized now (the marquee's *animation* is a
    /// later step, but its config value is not), and anything else warns and
    /// defaults.
    #[test]
    fn title_overflow_parses_both_modes_and_defaults_on_nonsense() {
        assert_eq!(
            TitleOverflow::parse("truncate"),
            Some(TitleOverflow::Truncate)
        );
        assert_eq!(
            TitleOverflow::parse("marquee"),
            Some(TitleOverflow::Marquee)
        );
        assert_eq!(TitleOverflow::parse("scroll"), None);

        let typo = PanelConfig::parse(r#"panel { window-title { overflow "scroll" } }"#)
            .expect("well-formed KDL");
        assert_eq!(typo.window_title.overflow, TitleOverflow::Truncate);
    }

    /// A `max-chars` that isn't a positive integer resets *that* knob only —
    /// the sibling `overflow` in the same block still loads.
    #[test]
    fn a_nonsense_max_chars_falls_back_without_taking_the_block_down() {
        for bad in ["0", "-5", "\"fifty\"", "12.5"] {
            let kdl =
                format!(r#"panel {{ window-title {{ max-chars {bad}; overflow "marquee" }} }}"#);
            let config = PanelConfig::parse(&kdl).expect("well-formed KDL");
            assert_eq!(
                config.window_title.max_chars, DEFAULT_TITLE_MAX_CHARS,
                "max-chars {bad} should have defaulted"
            );
            assert_eq!(
                config.window_title.overflow,
                TitleOverflow::Marquee,
                "the sibling knob should survive max-chars {bad}"
            );
        }
    }

    /// `~/` expands against a given `$HOME`; an absolute path passes
    /// through unchanged. Exercises `expand_tilde_with_home` directly
    /// (rather than mutating the process's real `$HOME` through
    /// `MarkSource::parse`) so this test can't race any other test in the
    /// binary over shared process environment state.
    #[test]
    fn tilde_expands_against_a_given_home() {
        assert_eq!(
            expand_tilde_with_home("~/.icons/arch.svg", Some("/home/test-user".into())),
            PathBuf::from("/home/test-user/.icons/arch.svg")
        );
        assert_eq!(
            expand_tilde_with_home("~", Some("/home/test-user".into())),
            PathBuf::from("/home/test-user")
        );
        assert_eq!(
            expand_tilde_with_home("/etc/saola/mark.svg", Some("/home/test-user".into())),
            PathBuf::from("/etc/saola/mark.svg")
        );
    }

    /// `height`/`margin` resolve to the theme's tokens when the knob is
    /// absent — the "absent knob → today's behavior" rule, proven against
    /// the real `saola_theme::Theme::saola()` rather than a hand-copied
    /// number (which could drift from the token silently).
    #[test]
    fn absent_height_and_margin_resolve_to_theme_defaults() {
        let theme = saola_theme::Theme::saola();
        let config = PanelConfig::default();
        assert_eq!(config.height(&theme), theme.sizes.panel_bar);
        assert_eq!(config.margin(&theme), theme.sizes.panel_margin_ledger);
    }

    /// An explicit knob wins over the theme default.
    #[test]
    fn explicit_height_and_margin_override_the_theme() {
        let theme = saola_theme::Theme::saola();
        let config = PanelConfig {
            height: Some(64.0),
            margin: Some(12.0),
            ..PanelConfig::default()
        };
        assert_eq!(config.height(&theme), 64.0);
        assert_eq!(config.margin(&theme), 12.0);
    }

    /// `colors { }` overrides exactly the fields it sets, and only after
    /// `apply` is called — proving the mutation shape `main.rs` relies on.
    #[test]
    fn color_overrides_apply_only_the_set_fields() {
        let mut palette = Palette::default();
        let original_paper = palette.paper;
        let overrides = ColorOverrides {
            ink: Some(Color::parse_hex("#123456").unwrap()),
            paper: None,
            accent: Some(Color::parse_hex("#abcdef").unwrap()),
        };

        overrides.apply(&mut palette);

        assert_eq!(palette.ink, Color::parse_hex("#123456").unwrap());
        assert_eq!(
            palette.paper, original_paper,
            "unset fields must be left alone"
        );
        assert_eq!(palette.accent, Color::parse_hex("#abcdef").unwrap());
    }

    /// A bad color string in `colors { }` warns and leaves that one field
    /// `None` (default), without touching the rest of the parse.
    #[test]
    fn invalid_color_falls_back_to_none_for_that_field_only() {
        let kdl = r##"
            panel {
                colors { ink "not-a-color"; accent "#C67139" }
            }
        "##;
        let config = PanelConfig::parse(kdl).expect("well-formed KDL");
        assert_eq!(config.colors.ink, None);
        assert_eq!(
            config.colors.accent,
            Some(Color::parse_hex("#C67139").unwrap())
        );
    }

    /// Each of the four flags lands in the matching override field, and the
    /// fields not named by any flag stay `None`.
    #[test]
    fn cli_flags_parse_to_the_matching_overrides() {
        assert_eq!(
            CliOverrides::parse(["--islands"]),
            CliOverrides {
                style: Some(PanelStyle::Islands),
                ..CliOverrides::default()
            }
        );
        assert_eq!(
            CliOverrides::parse(["--ledger", "--bottom"]),
            CliOverrides {
                style: Some(PanelStyle::Ledger),
                edge: Some(Edge::Bottom),
                ..CliOverrides::default()
            }
        );
        assert_eq!(
            CliOverrides::parse(["--top"]),
            CliOverrides {
                edge: Some(Edge::Top),
                ..CliOverrides::default()
            }
        );
        assert_eq!(
            CliOverrides::parse(std::iter::empty::<&str>()),
            CliOverrides::default()
        );
    }

    /// Both spellings of the help flag set `help`, alongside (not instead
    /// of) whatever other flags were given — `main` decides that help
    /// short-circuits everything else, not the parser.
    #[test]
    fn cli_help_flag_parses_in_both_spellings() {
        assert!(CliOverrides::parse(["--help"]).help);
        assert!(CliOverrides::parse(["-h"]).help);
        assert!(!CliOverrides::parse(["--islands"]).help);
        let mixed = CliOverrides::parse(["--islands", "--help"]);
        assert!(mixed.help);
        assert_eq!(mixed.style, Some(PanelStyle::Islands));
    }

    /// Every flag `parse` recognizes appears in the `--help` text — the
    /// guard that keeps [`HELP`] honest when a flag is added or renamed.
    #[test]
    fn help_text_names_every_recognized_flag() {
        for flag in [
            "--ledger",
            "--islands",
            "--top",
            "--bottom",
            "--config-dir",
            "--help",
            "-h",
        ] {
            assert!(HELP.contains(flag), "HELP is missing {flag}");
        }
    }

    /// Contradictory flags don't error — the last one wins, like shadowing
    /// a shell variable.
    #[test]
    fn cli_last_flag_wins_on_conflict() {
        let overrides = CliOverrides::parse(["--ledger", "--islands", "--bottom", "--top"]);
        assert_eq!(overrides.style, Some(PanelStyle::Islands));
        assert_eq!(overrides.edge, Some(Edge::Top));
    }

    /// A typo'd argument is warned about and skipped; the recognizable
    /// flags around it still apply — the command-line mirror of the
    /// config file's unknown-module rule.
    #[test]
    fn cli_unknown_arguments_are_ignored() {
        let overrides = CliOverrides::parse(["--islands", "--sideways", "extra"]);
        assert_eq!(
            overrides,
            CliOverrides {
                style: Some(PanelStyle::Islands),
                ..CliOverrides::default()
            }
        );
    }

    /// `--config-dir`'s two spellings both land, alongside the other flags;
    /// the value-less forms warn and are ignored rather than eating a flag
    /// or crashing — the same resilience as every other argument.
    #[test]
    fn cli_config_dir_parses_both_forms_and_ignores_a_missing_value() {
        assert_eq!(
            CliOverrides::parse(["--config-dir", "/tmp/panel-test"]).config_dir,
            Some(PathBuf::from("/tmp/panel-test"))
        );
        assert_eq!(
            CliOverrides::parse(["--config-dir=/tmp/panel-test"]).config_dir,
            Some(PathBuf::from("/tmp/panel-test"))
        );
        // The value is consumed by the flag, not mistaken for a stray
        // argument — the sibling flags around it still apply.
        let mixed = CliOverrides::parse(["--islands", "--config-dir", "/tmp/x", "--bottom"]);
        assert_eq!(mixed.style, Some(PanelStyle::Islands));
        assert_eq!(mixed.edge, Some(Edge::Bottom));
        assert_eq!(mixed.config_dir, Some(PathBuf::from("/tmp/x")));

        // Trailing `--config-dir` with nothing after it, and the empty
        // `=` form: warned, ignored, nothing else disturbed.
        assert_eq!(CliOverrides::parse(["--config-dir"]).config_dir, None);
        let empty = CliOverrides::parse(["--config-dir=", "--top"]);
        assert_eq!(empty.config_dir, None);
        assert_eq!(empty.edge, Some(Edge::Top));

        // A flag where the value should be is a forgotten value, not a
        // directory named "--islands" — and the flag must not be swallowed
        // with it.
        let flag_eaten = CliOverrides::parse(["--config-dir", "--islands"]);
        assert_eq!(flag_eaten.config_dir, None);
        assert_eq!(
            flag_eaten.style,
            Some(PanelStyle::Islands),
            "the flag following a value-less --config-dir must still apply"
        );

        // An empty string as the two-token value is as unusable as
        // `--config-dir=`.
        assert_eq!(
            CliOverrides::parse(["--config-dir", ""]).config_dir,
            None,
            "an empty value must not resolve to a cwd-relative panel.kdl"
        );
    }

    /// The directory resolution chain, rung by rung: the flag beats
    /// `$SAOLA_CONFIG_DIR` beats `$XDG_CONFIG_HOME/saola` beats
    /// `~/.config/saola` beats nothing. Exercises `config_dir_from`
    /// directly (env values as arguments) for the same no-races reason as
    /// `tilde_expands_against_a_given_home`.
    #[test]
    fn config_dir_resolves_most_specific_first() {
        let cli = Some(Path::new("/cli/dir"));
        let saola = || Some(std::ffi::OsString::from("/saola/dir"));
        let xdg = || Some(std::ffi::OsString::from("/xdg"));
        let home = || Some(std::ffi::OsString::from("/home/test-user"));

        assert_eq!(
            config_dir_from(cli, saola(), xdg(), home()),
            Some(PathBuf::from("/cli/dir")),
            "the flag beats everything"
        );
        assert_eq!(
            config_dir_from(None, saola(), xdg(), home()),
            Some(PathBuf::from("/saola/dir")),
            "$SAOLA_CONFIG_DIR beats the XDG chain"
        );
        assert_eq!(
            config_dir_from(None, None, xdg(), home()),
            Some(PathBuf::from("/xdg/saola")),
            "$XDG_CONFIG_HOME gets the saola/ namespace joined on"
        );
        assert_eq!(
            config_dir_from(None, None, None, home()),
            Some(PathBuf::from("/home/test-user/.config/saola")),
            "the spec's ~/.config fallback"
        );
        assert_eq!(config_dir_from(None, None, None, None), None);
    }

    /// An env var set to the empty string is unset, per the XDG spec's own
    /// rule — it falls through to the next rung, it doesn't name "". That
    /// includes `$HOME` itself: an empty `HOME` resolves to *no* config
    /// path, never to the cwd-relative `.config/saola`.
    #[test]
    fn empty_env_vars_fall_through_the_chain() {
        let empty = || Some(std::ffi::OsString::new());
        let home = Some(std::ffi::OsString::from("/home/test-user"));

        assert_eq!(
            config_dir_from(None, empty(), empty(), home),
            Some(PathBuf::from("/home/test-user/.config/saola"))
        );
        assert_eq!(config_dir_from(None, empty(), empty(), empty()), None);
    }

    /// `apply` overwrites exactly the fields a flag set — a config whose
    /// file said Islands keeps Islands when only `--bottom` is given, and a
    /// flag beats the file's value when both name the same knob.
    #[test]
    fn cli_apply_only_touches_set_fields() {
        let kdl = r#"panel { style "islands"; edge "top" }"#;
        let mut config = PanelConfig::parse(kdl).expect("well-formed KDL");

        CliOverrides::parse(["--bottom"]).apply(&mut config);
        assert_eq!(config.style, PanelStyle::Islands, "file value kept");
        assert_eq!(config.edge, Edge::Bottom, "flag beat the file");

        CliOverrides::parse(["--ledger"]).apply(&mut config);
        assert_eq!(config.style, PanelStyle::Ledger);
        assert_eq!(config.edge, Edge::Bottom, "earlier override undisturbed");
    }
}
