//! `TerminalView`: the terminal entity. Holds the shared `Term`, the backend,
//! and focus; wires keyboard input; renders via `render::terminal_canvas`.
//!
//! It is agnostic to the backend kind (CLAUDE.md §2). This file contains no
//! `gpui_component` imports — the boundary is enforced here.

use std::sync::Arc;

use alacritty_terminal::event::Event;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use gpui::{
    AppContext, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, Font, FontFallbacks,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString,
    Styled, Task, Window, div, font, hsla, prelude::FluentBuilder, px,
};

use crate::settings;
use crate::terminal::backend::{DeadBackend, LocalPty, PtyBackend};
use crate::terminal::bridge::{run_drain, run_feeder};
use crate::terminal::keymap::{PastePayload, encode_key, encode_paste};
use crate::terminal::model::{SharedTerm, new_term};
use crate::terminal::render::{cell_metrics, terminal_canvas};
use crate::terminal::scrollback;
use crate::terminal::selection;
use crate::terminal::serial::{SerialBackend, SerialConfig};
use crate::terminal::ssh::SshSession;
use crate::terminal::telnet::{TelnetBackend, TelnetConfig};

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;

/// Capacity (in read-chunks, ~8 KiB each) of the PTY→feeder byte channel. Small
/// enough that a runaway producer is paced to render speed and the backlog left
/// after Ctrl-C drains in a few frames; large enough not to stutter normal
/// bursts. ~256 KiB.
const PTY_CHANNEL_CAPACITY: usize = 32;

// Keys that gpui-component's `Root` context binds for itself (tab → focus nav,
// ctrl-c → Copy) must be reclaimed for the terminal. We bind these in the deeper
// "Terminal" context (see `main`), which wins over Root, and forward the right
// bytes to the PTY. Plain `on_key_down` never sees them — Root's bindings run
// before key listeners (gpui window dispatch order).
gpui::actions!(caracal_terminal, [Interrupt, SendTab, SendBackTab, ClearScreen]);

/// The `key_context` set on the terminal element; the reclaiming key bindings in
/// `main` target this same context.
pub const TERMINAL_KEY_CONTEXT: &str = "Terminal";

/// The bundled symbol font (registered in `main`) used as the default fallback so
/// Nerd Font / powerline glyphs resolve even when the primary font lacks them.
pub(crate) const SYMBOL_FALLBACK: &str = "Symbols Nerd Font";

/// The bundled CJK font (registered in `main`) used as a fallback so Chinese
/// glyphs resolve even on a system with no East Asian fonts installed (the
/// original cause of Windows mojibake — see the design spec).
pub(crate) const CJK_FALLBACK: &str = "Sarasa Mono SC";

/// The bundled default primary terminal font (registered in `main`). Hardcoded
/// rather than relying on per-OS "system monospace" detection, which had no
/// working implementation on Windows/macOS (see `system_monospace_family`,
/// kept below for the explicit "reset to system font" path).
pub(crate) const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";

/// User-configurable terminal font. The primary defaults to a bundled font
/// (`DEFAULT_FONT_FAMILY`) for consistent cross-platform rendering; a settings
/// UI can later swap it via [`TerminalView::set_font_family`] /
/// [`set_font_size`] / [`set_font_config`], and `set_font_family("")` resets
/// to the system monospace (`system_monospace_family`).
#[derive(Clone, Debug)]
pub struct FontConfig {
    /// Primary font family. Empty string means "system monospace".
    pub family: SharedString,
    pub size: Pixels,
    /// Extra families consulted (in order) for glyphs the primary lacks.
    pub fallbacks: Vec<SharedString>,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: DEFAULT_FONT_FAMILY.into(),
            size: px(14.0),
            fallbacks: vec![SYMBOL_FALLBACK.into(), CJK_FALLBACK.into()],
        }
    }
}

impl FontConfig {
    /// Build the gpui `Font` (primary family + fallback chain). gpui_wgpu's
    /// cosmic-text layer coverage-checks each glyph against the primary then the
    /// fallbacks, so icons resolve regardless of the chosen primary.
    fn to_font(&self) -> Font {
        let mut f = font(self.family.clone());
        if !self.fallbacks.is_empty() {
            f.fallbacks = Some(FontFallbacks::from_fonts(
                self.fallbacks.iter().map(|s| s.to_string()).collect(),
            ));
        }
        f
    }
}

/// The system's default monospace family, used by
/// `TerminalView::set_font_family("")` to reset away from the bundled default
/// (see `DEFAULT_FONT_FAMILY`). We resolve it ourselves (via fontconfig on
/// Linux, hardcoded on Windows) because gpui doesn't map the generic
/// `"monospace"` alias to a real family name on either platform — the literal
/// string `"monospace"` is not a font and fails to resolve. On macOS (and on
/// any detection failure) it returns the same non-resolving `"monospace"`
/// literal; macOS was left as-is because no mojibake has been reported there
/// and the default font no longer routes through this function.
fn system_monospace_family() -> SharedString {
    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = std::process::Command::new("fc-match")
            .args(["-f", "%{family[0]}", "monospace"])
            .output()
            && out.status.success()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name.into();
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        return "Consolas".into();
    }
    #[allow(unreachable_code)]
    "monospace".into()
}

/// IME pre-edit overlay while the OS is composing (pinyin, hangul, etc.).
/// Empty marked text is represented as `None` on the view, not an empty
/// string — that matches how Zed's `TerminalView::ime_state` is cleared.
#[derive(Clone, Debug, PartialEq)]
struct ImeState {
    marked_text: String,
}

impl ImeState {
    fn from_marked_text(text: String) -> Option<Self> {
        if text.is_empty() {
            None
        } else {
            Some(Self { marked_text: text })
        }
    }

    fn marked_text_range(&self) -> std::ops::Range<usize> {
        0..self.marked_text.encode_utf16().count()
    }
}

/// While IME is composing, printable keys (and Enter/Backspace/Escape) must
/// not also go to the PTY — the OS will commit via `InputHandler`. Modifier
/// chords (Ctrl-C, etc.) still pass through, matching Zed's terminal which
/// keeps `prefers_ime_for_printable_keys` at the default `false` so raw
/// control keys reach the process.
fn swallow_key_during_ime(composing: bool, _key: &str, modifiers: &gpui::Modifiers) -> bool {
    composing && !modifiers.control && !modifiers.alt && !modifiers.platform
}

/// Non-live state shown as a full-terminal banner overlay
/// (`conn_banner_text`, `TerminalView::render`). Only ever set on a
/// `remote_reconnect` (SSH) backend.
#[derive(Clone, Debug, PartialEq)]
enum ConnBanner {
    /// Dialing out (initial connect or a manual reconnect). Enter does
    /// nothing yet — there's nothing to retry.
    Connecting,
    /// Dead, with a human-readable reason. Enter re-dials (emits
    /// `TerminalViewEvent::ReconnectRequested`).
    Failed(String),
}

/// Pure text for the connecting/failed banner overlay
/// (`TerminalView::render`). Extracted standalone so it's unit-testable
/// without a `Window`/`Context`.
fn conn_banner_text(host_label: &str, banner: &ConnBanner) -> String {
    match banner {
        ConnBanner::Connecting => {
            rust_i18n::t!("Terminal.connecting", host = host_label).to_string()
        }
        ConnBanner::Failed(reason) => {
            rust_i18n::t!("Terminal.disconnected_press_enter", host = host_label, reason = reason)
                .to_string()
        }
    }
}

pub struct TerminalView {
    term: SharedTerm,
    backend: Arc<dyn PtyBackend>,
    /// The sender `Term`'s `EventProxy` was built with (see `model::new_term`)
    /// — kept so `reconnect_with` can spin up a new feeder generation that
    /// still signals the same, long-lived drain task (`_drain_task`) after a
    /// reconnect, rather than needing to reach inside `Term` for it.
    events_tx: flume::Sender<Event>,
    focus_handle: FocusHandle,
    font_config: FontConfig,
    title: String,
    exited: bool,
    /// Cell metrics cached from the last paint. Mouse handlers need to
    /// convert pixel coordinates into grid coordinates, and the cell
    /// metrics are computed inside `terminal_canvas`'s prepaint. We
    /// refresh them on every paint via [`Self::remember_cell_metrics`].
    last_cell_w: f32,
    last_cell_h: f32,
    last_cols: usize,
    last_rows: usize,
    /// The canvas's on-screen origin from the last paint, in *window*
    /// coordinates — the same space `gpui::MouseDownEvent::position` uses.
    /// Mouse handlers must subtract this before converting to grid
    /// coordinates, since the canvas is rarely flush with the window's
    /// top-left corner (docks, status bar). Missing this subtraction was a
    /// real bug: selections landed on the cell down-and-right of the actual
    /// click, offset by exactly however far the canvas sits from (0, 0).
    last_origin_x: f32,
    last_origin_y: f32,
    /// `Some(ty)` while a left-button drag is in progress; the type was
    /// chosen from the originating `MouseDownEvent::click_count`. We keep
    /// it as an `Option` here (not on the alacritty `Term`) because the
    /// `Term::selection` field is already mutated to be the live anchor
    /// — we just need a tiny bit of "is a drag currently active" memory
    /// to drive the cursor shape and copy-on-up behavior.
    selection_dragging: Option<SelectionType>,
    /// True for a backend where a broken connection is meaningfully
    /// reconnectable (currently: SSH shells). Gates whether a dead backend
    /// shows the "connection lost, press Enter to reconnect" banner — a
    /// local shell exiting, or a serial port closing, isn't a "connection"
    /// in that sense, so those stay silent (unchanged behavior).
    remote_reconnect: bool,
    /// "user@host" label used by the connecting/failed banner text
    /// (`conn_banner_text`). Empty for non-SSH backends, which never show
    /// a banner (`remote_reconnect` gates that).
    host_label: String,
    /// Stable key identifying which connection this tab's command history
    /// belongs to — SSH reuses `SshConfig::key()` (`user@host:port`);
    /// Local is the fixed string `"local"` (one shared bucket); Telnet is
    /// `"telnet://{host}:{port}"`; Serial is `"serial://{port_name}"`. Set
    /// once at construction, never changes for this tab's lifetime
    /// (including across a `reconnect_with` reconnect — it's still the
    /// same connection). See the design spec's "Component structure".
    history_key: String,
    /// Whether command-history tracking/suggestions are active for this
    /// tab at all — read once from `TerminalSettings.command_suggestions_enabled`
    /// at construction, same "read once, changes affect tabs opened
    /// afterward" convention `scrollback_lines` already uses (not live-
    /// reloaded).
    suggestions_enabled: bool,
    /// This connection's command history, loaded once at construction and
    /// kept in sync with what's on disk as this tab itself records new
    /// entries (via `record`'s returned updated list) — avoids a disk
    /// read on every keystroke. Doesn't pick up entries recorded by other
    /// tabs to the same connection in real time; an accepted limitation.
    history_cache: Vec<String>,
    /// Characters typed (plain printable keys + Backspace) since the last
    /// Enter — Caracal's own best-effort approximation of "what's on the
    /// current input line", since it has no way to read that back from
    /// the shell. See the design spec's "Input tracking" decision.
    input_buffer: String,
    /// True once a key we can't safely track (arrows, Ctrl+A/E/U, Home/
    /// End, etc.) has been pressed since the last Enter — `input_buffer`
    /// may no longer match the real line, so suggestions stay hidden
    /// until the next Enter resets everything, even if the user resumes
    /// plain typing before then.
    tracking_desynced: bool,
    /// Currently-matching history entries for `input_buffer`, most-
    /// recent-first — empty means no dropdown is shown.
    suggestions: Vec<String>,
    /// Index into `suggestions` the user has navigated to via ↑/↓, or
    /// `None` if nothing's been explicitly selected yet.
    selected_index: Option<usize>,
    /// `Some` while a non-live state (connecting or dead) should be shown
    /// as a full-terminal overlay; `None` means the backend is live. Only
    /// ever `Some` when `remote_reconnect` is true. Set by
    /// `mark_disconnected` / `mark_connect_failed` / `mark_connecting`
    /// (the latter two added in Task 3), cleared by `reconnect_with`.
    banner: Option<ConnBanner>,
    /// Pre-edit string from the platform IME, painted as an underlined
    /// overlay at the cursor. `None` when not composing.
    ime_state: Option<ImeState>,
    _drain_task: Task<()>,
    /// Watches the current backend generation's byte stream for closure.
    /// Replaced (dropping — and so cancelling — the previous one) each time
    /// `reconnect_with` swaps in a fresh backend generation.
    _disconnect_watch: Task<()>,
}

/// Emitted by `TerminalView` for events its owner (`Workspace`) needs to act
/// on, since the view itself doesn't know how to redial a backend (CLAUDE.md
/// §2 — it stays agnostic to backend kind). Follows the same
/// event-emitter + `cx.subscribe_in` pattern as `SessionsEvent`
/// (`src/panels/sessions.rs`).
#[derive(Clone, Debug)]
pub enum TerminalViewEvent {
    /// The user pressed Enter on a disconnected, reconnectable terminal.
    ReconnectRequested,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl TerminalView {
    /// A terminal backed by the local shell (`LocalPty`). `title` is the tab
    /// name — pass `"本地终端"` for a bare/menu-triggered local shell, or a
    /// saved connection's `display_name()` when opened from the sessions
    /// list (see `Workspace::open_local`/`open_local_with`).
    pub fn new(window: &mut Window, cx: &mut Context<Self>, title: String) -> Self {
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            "local".to_string(),
            |cols, rows, bytes_tx| {
                Arc::new(LocalPty::spawn(cols, rows, bytes_tx).expect("failed to spawn local pty"))
            },
        )
    }

    /// A terminal backed by a local shell with custom shell path and working
    /// directory. `title` is the tab name, same convention as `new`'s.
    pub fn new_local_with(
        window: &mut Window,
        cx: &mut Context<Self>,
        shell: &str,
        working_dir: Option<&str>,
        title: String,
    ) -> Self {
        let shell = shell.to_string();
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            "local".to_string(),
            move |cols, rows, bytes_tx| {
                Arc::new(
                    LocalPty::spawn_with(cols, rows, bytes_tx, &shell, working_dir)
                        .expect("failed to spawn local pty"),
                )
            },
        )
    }

    /// A terminal backed by a shell channel on an already-connected [`SshSession`]
    /// (shared with the SFTP panel — one connection per host, CLAUDE.md §2).
    /// `remote_reconnect: true` — a dead SSH shell shows the reconnect banner
    /// (see `mark_disconnected`, `reconnect_with`). `host_label` (e.g.
    /// "user@host") is only for that banner text; `title` (e.g. "my-server:1",
    /// computed by `Workspace::open_ssh` from the saved connection's display
    /// name plus a dedup counter) is the tab name — the two deliberately
    /// don't have to match.
    pub fn new_ssh_shell(
        window: &mut Window,
        cx: &mut Context<Self>,
        session: Arc<SshSession>,
        host_label: String,
        history_key: String,
        title: String,
    ) -> Self {
        Self::with_backend(
            window,
            cx,
            true,
            host_label,
            title,
            history_key,
            move |cols, rows, bytes_tx| session.open_shell(cols, rows, bytes_tx),
        )
    }

    /// A placeholder SSH tab shown the instant the user double-clicks a
    /// saved connection, before the dial resolves. No feeder/
    /// disconnect-watch is spawned — a `DeadBackend` generation's
    /// `bytes_tx` would be dropped immediately (same pattern telnet/
    /// serial's connect-failure fallback uses in `new_telnet`/
    /// `new_serial`), which would fire `mark_disconnected` within the
    /// same tick and stomp the `Connecting` banner set here. `Workspace`
    /// swaps in the real generation via `reconnect_with` once
    /// `SshSession::connect` resolves (success) or calls
    /// `mark_connect_failed` (failure).
    pub fn new_ssh_connecting(
        window: &mut Window,
        cx: &mut Context<Self>,
        host_label: String,
        history_key: String,
        title: String,
    ) -> Self {
        let (focus_handle, term, events_tx, drain_task) = Self::base_setup(window, cx);
        Self::assemble(
            focus_handle,
            term,
            events_tx,
            drain_task,
            Arc::new(DeadBackend),
            Task::ready(()),
            true,
            host_label,
            Some(ConnBanner::Connecting),
            title,
            history_key,
        )
    }

    /// A terminal backed by a raw Telnet connection (`TelnetBackend`). Each
    /// tab dials its own socket — unlike SSH, telnet has no SFTP-style
    /// second channel to justify a shared connection.
    pub fn new_telnet(window: &mut Window, cx: &mut Context<Self>, config: TelnetConfig) -> Self {
        let title = format!("{}:{}", config.host, config.port);
        let history_key = format!("telnet://{}:{}", config.host, config.port);
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            history_key,
            move |_cols, _rows, bytes_tx| match TelnetBackend::connect(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mtelnet connect failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            },
        )
    }

    /// A terminal backed by a serial port (`SerialBackend`).
    pub fn new_serial(window: &mut Window, cx: &mut Context<Self>, config: SerialConfig) -> Self {
        let title = config.port.clone();
        let history_key = format!("serial://{}", config.port);
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            history_key,
            move |_cols, _rows, bytes_tx| match SerialBackend::open(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mserial open failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            },
        )
    }

    /// Wire one backend "generation": the bytes channel, the feeder thread,
    /// and a small watcher task that notices when the feeder's loop ends
    /// (backend read side closed — see `bridge::run_feeder`'s `closed`
    /// param) and calls `mark_disconnected`. Used both by `with_backend`
    /// (first generation) and `reconnect_with` (every generation after a
    /// reconnect) — factored out so a reconnect only rebuilds this part,
    /// not the `Term`/scrollback/events plumbing that `with_backend` also
    /// sets up once.
    fn spawn_generation(
        term: SharedTerm,
        events_tx: flume::Sender<Event>,
        cols: u16,
        rows: u16,
        make_backend: impl FnOnce(u16, u16, flume::Sender<Vec<u8>>) -> Arc<dyn PtyBackend>,
        cx: &mut Context<Self>,
    ) -> (Arc<dyn PtyBackend>, Task<()>) {
        let (bytes_tx, bytes_rx) = flume::bounded::<Vec<u8>>(PTY_CHANNEL_CAPACITY);
        let (closed_tx, closed_rx) = flume::bounded::<()>(1);

        let backend = make_backend(cols, rows, bytes_tx);

        std::thread::Builder::new()
            .name("caracal-feeder".into())
            .spawn(move || run_feeder(term, bytes_rx, events_tx, closed_tx))
            .expect("failed to spawn feeder thread");

        let disconnect_watch = cx.spawn(async move |view, cx| {
            if closed_rx.recv_async().await.is_ok() {
                let _ = view.update(cx, |view, cx| view.mark_disconnected(cx));
            }
        });

        (backend, disconnect_watch)
    }

    /// Shared prelude for both a full backend generation (`with_backend`)
    /// and a placeholder-only view (`new_ssh_connecting`): the focus
    /// handle, `Term`, event channel, and long-lived drain task. Doesn't
    /// touch the backend itself — callers wire that up separately (a real
    /// generation via `spawn_generation`, or nothing at all for a
    /// placeholder).
    fn base_setup(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (FocusHandle, SharedTerm, flume::Sender<Event>, Task<()>) {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let (events_tx, events_rx) = flume::unbounded::<Event>();
        let scrollback_lines = settings::load().terminal.scrollback_lines as usize;
        let term = new_term(DEFAULT_COLS, DEFAULT_ROWS, scrollback_lines, events_tx.clone());

        let drain_task = cx.spawn(async move |weak, cx| {
            run_drain(weak, events_rx, cx).await;
        });

        (focus_handle, term, events_tx, drain_task)
    }

    /// Shared tail for both a full backend generation (`with_backend`)
    /// and a placeholder-only view (`new_ssh_connecting`): fills in the
    /// remaining fields that never vary by caller (default font, initial
    /// title, zeroed cell metrics, etc.) around the handful that do.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        focus_handle: FocusHandle,
        term: SharedTerm,
        events_tx: flume::Sender<Event>,
        drain_task: Task<()>,
        backend: Arc<dyn PtyBackend>,
        disconnect_watch: Task<()>,
        remote_reconnect: bool,
        host_label: String,
        banner: Option<ConnBanner>,
        title: String,
        history_key: String,
    ) -> Self {
        let settings = settings::load();
        let suggestions_enabled = settings.terminal.command_suggestions_enabled;
        // Only read the history file when the feature is actually on — with
        // the toggle off nothing ever reads `history_cache`, and every new
        // tab was otherwise paying for a full read+parse of
        // `command_history.toml` (final review finding 11). The settings
        // read above is unavoidable: it's what tells us the flag's value.
        let history_cache = if suggestions_enabled {
            crate::command_history::load_for(&history_key)
        } else {
            Vec::new()
        };
        Self {
            term,
            backend,
            events_tx,
            focus_handle,
            font_config: FontConfig::default(),
            title,
            exited: false,
            last_cell_w: 0.0,
            last_cell_h: 0.0,
            last_cols: 0,
            last_rows: 0,
            last_origin_x: 0.0,
            last_origin_y: 0.0,
            selection_dragging: None,
            remote_reconnect,
            host_label,
            history_key,
            suggestions_enabled,
            history_cache,
            input_buffer: String::new(),
            tracking_desynced: false,
            suggestions: Vec::new(),
            selected_index: None,
            banner,
            ime_state: None,
            _drain_task: drain_task,
            _disconnect_watch: disconnect_watch,
        }
    }

    /// Shared construction: wire the model, feeder, drain task, and focus; the
    /// backend is built by `make_backend` (given the initial size and the byte
    /// sink the feeder reads from). The backend is agnostic here — `TerminalView`
    /// never learns whether it's local, SSH, or serial (CLAUDE.md §2).
    fn with_backend(
        window: &mut Window,
        cx: &mut Context<Self>,
        remote_reconnect: bool,
        host_label: String,
        title: String,
        history_key: String,
        make_backend: impl FnOnce(u16, u16, flume::Sender<Vec<u8>>) -> Arc<dyn PtyBackend>,
    ) -> Self {
        let (focus_handle, term, events_tx, drain_task) = Self::base_setup(window, cx);

        let (backend, disconnect_watch) = Self::spawn_generation(
            term.clone(),
            events_tx.clone(),
            DEFAULT_COLS as u16,
            DEFAULT_ROWS as u16,
            make_backend,
            cx,
        );

        Self::assemble(
            focus_handle,
            term,
            events_tx,
            drain_task,
            backend,
            disconnect_watch,
            remote_reconnect,
            host_label,
            None,
            title,
            history_key,
        )
    }

    #[allow(dead_code)] // consumed by the panel adapter in Phase 5
    pub fn title(&self) -> &str {
        &self.title
    }

    #[allow(dead_code)] // consumed by CommandHistoryPanel wiring in workspace.rs
    pub fn history_key(&self) -> &str {
        &self.history_key
    }

    pub fn mark_exited(&mut self) {
        self.exited = true;
    }

    /// Write bytes straight to the current backend. Called by `run_drain`
    /// for PTY write-backs (query responses, clipboard load) — going through
    /// the view instead of a captured `Arc<dyn PtyBackend>` means these
    /// always reach whatever backend is current, even right after a
    /// `reconnect_with` swap.
    pub(crate) fn write_to_backend(&self, bytes: &[u8]) {
        self.backend.write(bytes);
    }

    /// Called (via the per-generation disconnect-watch task) when the
    /// current backend's byte stream ends. Only reconnectable backends
    /// (`remote_reconnect`, currently just SSH) surface this as the
    /// "connection lost, press Enter to reconnect" banner — a local shell
    /// exiting or a serial port closing isn't a "connection" in that sense
    /// and stays as-is.
    fn mark_disconnected(&mut self, cx: &mut Context<Self>) {
        if !self.remote_reconnect || matches!(self.banner, Some(ConnBanner::Failed(_))) {
            return;
        }
        self.banner = Some(ConnBanner::Failed(rust_i18n::t!("Terminal.disconnected").to_string()));
        cx.notify();
    }

    /// Called by `Workspace` when an SSH dial (initial connect or a
    /// manual reconnect) fails. Flips the banner to `Failed(reason)` —
    /// Enter now re-emits `TerminalViewEvent::ReconnectRequested` (see
    /// `on_key_down`).
    pub fn mark_connect_failed(&mut self, reason: String, cx: &mut Context<Self>) {
        if !self.remote_reconnect {
            return;
        }
        self.banner = Some(ConnBanner::Failed(reason));
        cx.notify();
    }

    /// Called by `Workspace` right before redialing a dead SSH tab, so
    /// the banner reads "connecting" instead of stale "failed" text
    /// during the redial.
    pub fn mark_connecting(&mut self, cx: &mut Context<Self>) {
        if !self.remote_reconnect {
            return;
        }
        self.banner = Some(ConnBanner::Connecting);
        cx.notify();
    }

    /// Swap in a freshly-dialed backend after a reconnect, replacing the
    /// (bytes channel / feeder thread / disconnect-watch) trio the dead
    /// generation left behind — the `Term`/scrollback and the long-lived
    /// drain task are untouched, so history survives the reconnect.
    ///
    /// Takes a `make_backend` factory, same shape as `with_backend`'s —
    /// *not* a ready-built `Arc<dyn PtyBackend>` — because the backend must
    /// be opened with *this* generation's own `bytes_tx` (created inside
    /// `spawn_generation`); building it earlier would wire it to nothing.
    /// Called by `Workspace` once it has redialed (see
    /// `TerminalViewEvent::ReconnectRequested`).
    pub fn reconnect_with(
        &mut self,
        make_backend: impl FnOnce(u16, u16, flume::Sender<Vec<u8>>) -> Arc<dyn PtyBackend>,
        cx: &mut Context<Self>,
    ) {
        let (cols, rows) = if self.last_cols > 0 && self.last_rows > 0 {
            (self.last_cols as u16, self.last_rows as u16)
        } else {
            (DEFAULT_COLS as u16, DEFAULT_ROWS as u16)
        };
        let term = self.term.clone();
        let events_tx = self.events_tx.clone();
        let (backend, disconnect_watch) =
            Self::spawn_generation(term, events_tx, cols, rows, make_backend, cx);
        self.backend = backend;
        self._disconnect_watch = disconnect_watch;
        self.banner = None;
        // The reconnected shell starts with an empty input line, so anything
        // we believed about the pre-disconnect line (a half-typed buffer, a
        // desync flag, an open dropdown) is stale and would mis-record the
        // next Enter (final review finding 6).
        self.reset_tracking(cx);
        cx.notify();
    }

    /// Send `text` into this terminal as if pasted (honours the term's
    /// `BRACKETED_PASTE` mode, same encoder `paste_from_clipboard` uses). If
    /// `execute` is `true`, a trailing Enter (`\r`) is sent after the encoded
    /// payload. Used by the quick-commands panel to inject saved command
    /// snippets. No-op for empty `text` (matches `encode_paste`'s own
    /// empty-string `None` behavior).
    ///
    /// Takes `&mut self` because injecting text mutates the real input line
    /// without going through `on_key_down`'s per-character tracking, so
    /// local tracking has to be invalidated (final review finding 4).
    pub fn send_text(&mut self, text: &str, execute: bool, cx: &mut Context<Self>) {
        let mode: TermMode = *self.term.lock().mode();
        let Some(mut bytes) = encode_paste(text, mode, PastePayload::Clipboard) else {
            return;
        };
        if execute {
            bytes.push(b'\r');
            // A trailing Enter submits the line, so the shell's input line
            // is empty afterwards — tracking can resume clean, same as the
            // Ctrl+C / Enter reset paths.
            self.reset_tracking(cx);
        } else {
            self.desync_tracking(cx);
        }
        self.backend.write(&bytes);
        self.term
            .lock()
            .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
    }

    /// The cursor's current (row, column), 0-indexed, adjusted for
    /// scrollback (`display_offset`) exactly like
    /// `grid_snapshot::snapshot_content`'s own cursor computation. Used by
    /// the status bar to show `row+1:col+1`.
    pub fn cursor_position(&self) -> (usize, usize) {
        let term = self.term.lock();
        let content = term.renderable_content();
        let display_offset = content.display_offset as i32;
        let row = (content.cursor.point.line.0 + display_offset).max(0) as usize;
        let col = content.cursor.point.column.0;
        (row, col)
    }

    /// Best-effort plain-text read of one grid row, identified in the same
    /// "visual row from viewport top" coordinate space `cursor_position`
    /// returns (i.e. `visual_row` is what `cursor_position().0` gives you,
    /// not a raw `alacritty_terminal::index::Line`). Built on a temporary
    /// programmatic `Lines`-type selection — the same mechanism mouse
    /// triple-click already uses (`terminal/selection.rs`) — rather than
    /// direct grid-cell iteration. Saves and restores whatever selection
    /// was already active first, so this never clobbers a real
    /// in-progress user selection. Used by
    /// `Workspace::guess_focused_terminal_cwd` to screen-scrape `pwd`'s
    /// output for the SFTP panel's "sync from terminal" button — see
    /// `docs/superpowers/specs/2026-07-08-file-explorer-gaps-round-b-design.md`
    /// for why this is best-effort, not a reliable signal.
    pub fn line_text(&self, visual_row: usize) -> String {
        use alacritty_terminal::index::{Column, Line, Point, Side};
        use alacritty_terminal::selection::Selection;

        let mut term = self.term.lock();
        let display_offset = term.renderable_content().display_offset as i32;
        let raw_line = visual_row as i32 - display_offset;
        let saved = term.selection.take();
        term.selection = Some(Selection::new(
            SelectionType::Lines,
            Point::new(Line(raw_line), Column(0)),
            Side::Left,
        ));
        let text = term.selection_to_string().unwrap_or_default();
        term.selection = saved;
        text.trim().to_string()
    }

    // --- Font configuration interface (for a future settings UI) ---

    #[allow(dead_code)]
    pub fn font_config(&self) -> &FontConfig {
        &self.font_config
    }

    /// Replace the whole font config and repaint (re-derives cell metrics / size).
    #[allow(dead_code)]
    pub fn set_font_config(&mut self, config: FontConfig, cx: &mut Context<Self>) {
        self.font_config = config;
        cx.notify();
    }

    /// Set the primary font family (empty string = system monospace).
    #[allow(dead_code)]
    pub fn set_font_family(&mut self, family: impl Into<SharedString>, cx: &mut Context<Self>) {
        let family = family.into();
        self.font_config.family = if family.is_empty() {
            system_monospace_family()
        } else {
            family
        };
        cx.notify();
    }

    /// Replace the fallback chain, in order. Empty entries resolve to the
    /// system monospace font — same "" convention `set_font_family` already
    /// uses — so a settings value of `""` for either fallback slot means
    /// "detect a system font here too", not "no fallback".
    #[allow(dead_code)]
    pub fn set_font_fallbacks(&mut self, fallbacks: Vec<SharedString>, cx: &mut Context<Self>) {
        self.font_config.fallbacks = fallbacks
            .into_iter()
            .map(|f| if f.is_empty() { system_monospace_family() } else { f })
            .collect();
        cx.notify();
    }

    #[allow(dead_code)]
    pub fn set_font_size(&mut self, size: Pixels, cx: &mut Context<Self>) {
        self.font_config.size = size;
        cx.notify();
    }

    /// Stash the most recent paint's cell metrics on the view. Called
    /// by the canvas prepaint callback (see `render::terminal_canvas`)
    /// so the mouse handlers can map pixel coordinates into grid
    /// coordinates. `pub(crate)` because the only legitimate caller
    /// is `render::terminal_canvas` in this same crate.
    pub(crate) fn remember_cell_metrics(
        &mut self,
        cell_w: f32,
        cell_h: f32,
        cols: usize,
        rows: usize,
        origin_x: f32,
        origin_y: f32,
    ) {
        self.last_cell_w = cell_w;
        self.last_cell_h = cell_h;
        self.last_cols = cols;
        self.last_rows = rows;
        self.last_origin_x = origin_x;
        self.last_origin_y = origin_y;
    }

    /// The shared `Term` handle, for building read-only viewers outside the
    /// `Entity`/`cx` system — currently only `panels::terminal`'s scrollbar
    /// adapter, which needs it inside `ScrollbarHandle` methods that take no
    /// `cx`.
    pub fn shared_term(&self) -> SharedTerm {
        self.term.clone()
    }

    /// The cell height measured at the last paint (`0.0` before the first
    /// paint). Used by `panels::terminal`'s scrollbar adapter, which has no
    /// `cx` inside its `ScrollbarHandle` methods to read this live.
    pub fn last_cell_height(&self) -> f32 {
        self.last_cell_h
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Non-live state (connecting or dead SSH connection): swallow all
        // input except Enter, and only forward that as
        // `ReconnectRequested` once there's actually something to retry
        // (`Failed`, not `Connecting`) — `Workspace` is the only thing
        // that knows how to redial (CLAUDE.md §2).
        if let Some(banner) = &self.banner {
            if matches!(banner, ConnBanner::Failed(_)) && ev.keystroke.key == "enter" {
                cx.emit(TerminalViewEvent::ReconnectRequested);
            }
            return;
        }
        // IME is composing: the platform will commit via `InputHandler`.
        // Don't also encode the underlying latin keys (or Enter/Backspace)
        // into the PTY. Ctrl/Alt/Super chords still go through.
        if swallow_key_during_ime(
            self.ime_state.is_some(),
            ev.keystroke.key.as_str(),
            &ev.keystroke.modifiers,
        ) {
            return;
        }
        // Copy shortcut: Ctrl+Shift+C → copy current selection to clipboard.
        // This is the XTerm / gnome-terminal convention; Ctrl+C alone
        // continues to mean SIGINT (Phase 2 behavior).
        if ev.keystroke.modifiers.control
            && ev.keystroke.modifiers.shift
            && matches!(ev.keystroke.key.as_ref(), "c" | "C")
        {
            self.copy_selection_to_clipboard(cx);
            return;
        }
        // Paste shortcut: Ctrl+Shift+V → read clipboard and feed the
        // bracketed-paste-aware encoder. We don't take ownership of
        // focus on the keystroke; the encoder will correctly wrap in
        // ESC[200~ / ESC[201~ if the term is in BRACKETED_PASTE mode.
        if ev.keystroke.modifiers.control
            && ev.keystroke.modifiers.shift
            && matches!(ev.keystroke.key.as_ref(), "v" | "V")
        {
            self.paste_from_clipboard(cx);
            return;
        }

        // Local scrollback navigation: Shift+PageUp/PageDown/Home/End move the
        // viewport over history instead of being sent to the program. Plain
        // PageUp/etc. still go to the app (encode_key below -> ESC[5~).
        let m = &ev.keystroke.modifiers;
        if m.shift && !m.control && !m.alt && !m.platform {
            use alacritty_terminal::grid::Scroll;
            let scroll = match ev.keystroke.key.as_ref() {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                "home" => Some(Scroll::Top),
                "end" => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(s) = scroll {
                self.term.lock().scroll_display(s);
                cx.notify();
                return;
            }
        }

        // Read the terminal mode once, up front: it gates command-history
        // tracking (alt screen, below) and is also what `encode_key` needs
        // at the bottom of this method.
        let mode: TermMode = *self.term.lock().mode();
        // Full-screen programs (vim, less, htop, …) run in the alternate
        // screen buffer. What's typed there is editor/pager input, not a
        // shell command line, and is often not echoed at all — so never
        // track, record, or suggest while it's active (final review
        // finding 1b). Anything tracked before the switch is stale.
        let alt_screen = mode.contains(TermMode::ALT_SCREEN);
        if alt_screen
            && (!self.input_buffer.is_empty()
                || !self.suggestions.is_empty()
                || self.tracking_desynced)
        {
            self.reset_tracking(cx);
        }
        let tracking_active = self.suggestions_enabled && !alt_screen;

        if tracking_active && !self.suggestions.is_empty() {
            let key = ev.keystroke.key.as_str();
            match key {
                "up" => {
                    self.move_suggestion_selection(false);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.move_suggestion_selection(true);
                    cx.notify();
                    return;
                }
                "escape" => {
                    // Escape closes the dropdown either way, but only
                    // *swallows* the byte when it's cancelling an actual
                    // ↑/↓ selection. With nothing selected the dropdown is
                    // merely on screen (a typed prefix happened to match
                    // history), and Escape still has its normal terminal
                    // meaning — e.g. leaving vim insert mode inside a REPL
                    // — so it falls through to encode_key below (final
                    // review finding 7).
                    let had_selection = self.selected_index.is_some();
                    self.suggestions.clear();
                    self.selected_index = None;
                    cx.notify();
                    if had_selection {
                        return;
                    }
                }
                // Tab is deliberately absent: it never reaches a key
                // listener (Root's binding is reclaimed as the `SendTab`
                // action, dispatched first), so accepting-with-Tab lives in
                // `on_send_tab` (final review finding 3).
                "enter" if self.selected_index.is_some() => {
                    self.accept_selected_suggestion(cx);
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }

        if tracking_active {
            let key = ev.keystroke.key.as_str();
            if key == "enter" {
                if !self.tracking_desynced
                    && !self.input_buffer.is_empty()
                    && self.input_buffer_is_echoed()
                {
                    let line = self.input_buffer.clone();
                    // In-memory cache first, synchronously, so the very next
                    // keystroke can already match this command…
                    crate::command_history::record_into(&mut self.history_cache, &line);
                    // …while the load+parse+serialize+write round-trip goes
                    // to a background thread instead of blocking the render
                    // thread on every submitted command (final review
                    // finding 8).
                    let history_key = self.history_key.clone();
                    cx.background_spawn(async move {
                        if let Err(e) = crate::command_history::record(&history_key, &line) {
                            log::warn!("failed to save command history for {history_key}: {e}");
                        }
                    })
                    .detach();
                }
                self.reset_tracking(cx);
                // Deliberately no `return` here — Enter must still reach
                // the PTY via the normal encode_key/send_input path below.
            } else if !self.tracking_desynced {
                // Control characters are excluded explicitly: some platforms
                // report a `key_char` for keys like Escape (`\x1b`), which is
                // not typed text and must never land in `input_buffer` — it
                // matters more now that Escape can fall through to here with
                // the dropdown open (final review finding 7).
                let is_plain_char = !m.control
                    && !m.alt
                    && !m.platform
                    && ev.keystroke.key_char.as_deref().is_some_and(|s| {
                        !s.is_empty() && !s.chars().any(char::is_control)
                    });
                if is_plain_char {
                    self.input_buffer.push_str(ev.keystroke.key_char.as_deref().unwrap());
                    self.suggestions = crate::command_history::matching_suggestions(
                        &self.history_cache,
                        &self.input_buffer,
                    );
                    self.selected_index = None;
                    cx.notify();
                } else if key == "backspace" && !m.control && !m.alt {
                    // No `!m.platform` check on purpose: `encode_key`
                    // (terminal/keymap.rs) encodes Backspace as 0x7f for
                    // everything except Alt (0x1b 0x7f), ignoring
                    // Ctrl/Shift/platform entirely — so e.g. Cmd+Backspace
                    // really does delete exactly one character on the wire
                    // and must stay tracked, not desynced (final review
                    // finding 9: this guard is already correct as written).
                    self.input_buffer.pop();
                    self.suggestions = crate::command_history::matching_suggestions(
                        &self.history_cache,
                        &self.input_buffer,
                    );
                    self.selected_index = None;
                    cx.notify();
                } else {
                    self.desync_tracking(cx);
                }
            }
        }

        if let Some(bytes) = encode_key(&ev.keystroke, mode) {
            self.send_input(&bytes);
            // Stop propagation once the key is actually sent to the PTY.
            // Since the CJK IME support above registered a `TerminalInputHandler`
            // (render::terminal_canvas -> window.handle_input), the platform
            // layer (e.g. gpui_linux's X11 Window::handle_input) *also*
            // forwards every plain keystroke's `key_char` straight to
            // `InputHandler::replace_text_in_range` — but only when this
            // listener left the event propagating. Without this call, a
            // plain letter is written to the PTY here AND a second time via
            // `commit_text`, echoing every typed character twice. Zed's own
            // `TerminalView::key_down` stops propagation the same way once
            // `process_keystroke` reports the key as handled.
            cx.stop_propagation();
        }
    }

    /// Write bytes to the backend and snap the viewport to the live area (typing
    /// while scrolled back jumps to the bottom).
    fn send_input(&self, bytes: &[u8]) {
        self.backend.write(bytes);
        self.term
            .lock()
            .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
    }

    /// Whether the platform IME should be able to commit into this terminal.
    /// Connecting/failed banners swallow keyboard input; IME must too.
    pub(crate) fn accepts_ime(&self) -> bool {
        self.banner.is_none()
    }

    pub(crate) fn ime_marked_text(&self) -> Option<&str> {
        self.ime_state.as_ref().map(|s| s.marked_text.as_str())
    }

    pub(crate) fn marked_text_range(&self) -> Option<std::ops::Range<usize>> {
        self.ime_state.as_ref().map(ImeState::marked_text_range)
    }

    /// Sets the marked (pre-edit) text from the IME. Empty text clears.
    pub(crate) fn set_marked_text(&mut self, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            return self.clear_marked_text(cx);
        }
        self.ime_state = ImeState::from_marked_text(text);
        cx.notify();
    }

    pub(crate) fn clear_marked_text(&mut self, cx: &mut Context<Self>) {
        if self.ime_state.is_some() {
            self.ime_state = None;
            cx.notify();
        }
    }

    /// Commits composed text to the PTY. Called by `InputHandler::replace_text_in_range`.
    pub(crate) fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.clear_marked_text(cx);
        if text.is_empty() || self.banner.is_some() {
            return;
        }
        let mode: TermMode = *self.term.lock().mode();
        let alt_screen = mode.contains(TermMode::ALT_SCREEN);
        let tracking_active = self.suggestions_enabled && !alt_screen;
        if tracking_active && !self.tracking_desynced {
            self.input_buffer.push_str(text);
            self.suggestions = crate::command_history::matching_suggestions(
                &self.history_cache,
                &self.input_buffer,
            );
            self.selected_index = None;
        }
        self.send_input(text.as_bytes());
        cx.notify();
    }

    /// Local input tracking can no longer be trusted to match the shell's
    /// real input line (an untracked key, a paste, shell tab-completion, a
    /// back-tab, …): keep whatever `input_buffer` holds but stop believing
    /// it — no suggestions, no recording — until the next Enter resets it.
    /// The single place that "invalidate tracking" is expressed, shared by
    /// `on_key_down`'s untracked-key branch and by every action handler /
    /// text-injection path that mutates the line behind `on_key_down`'s back
    /// (final review findings 3 and 4).
    fn desync_tracking(&mut self, cx: &mut Context<Self>) {
        if !self.suggestions_enabled {
            return;
        }
        self.tracking_desynced = true;
        self.suggestions.clear();
        self.selected_index = None;
        cx.notify();
    }

    /// Back to a clean slate: the shell's input line is known to be empty
    /// (Enter submitted it, Ctrl+C discarded it, or a reconnect replaced the
    /// shell), so tracking resumes trusted rather than desynced.
    fn reset_tracking(&mut self, cx: &mut Context<Self>) {
        self.input_buffer.clear();
        self.tracking_desynced = false;
        self.suggestions.clear();
        self.selected_index = None;
        cx.notify();
    }

    /// Whether the text we think has been typed is actually visible on the
    /// cursor's row. Caracal tracks keystrokes blind — it has no idea
    /// whether the PTY is echoing them — so a `sudo`/`ssh`/`mysql -p`
    /// password prompt (echo off) would otherwise get the typed secret
    /// recorded verbatim into `command_history.toml` and later offered as
    /// an on-screen suggestion. Reading the row back (`line_text`, the same
    /// helper the SFTP cwd-sync screen-scrape uses) and requiring
    /// `input_buffer` to appear in it makes a non-echoed prompt fail the
    /// check, so nothing is recorded (final review finding 1a).
    ///
    /// Best-effort by construction: an input line that wrapped across rows,
    /// or a prompt that rewrites the line (some fancy zsh setups), also
    /// fails the check — a missed recording, which is the safe direction.
    fn input_buffer_is_echoed(&self) -> bool {
        let (row, _col) = self.cursor_position();
        self.line_text(row).contains(&self.input_buffer)
    }

    /// Moves `selected_index` through `suggestions` — `None` -> index 0
    /// regardless of direction (first press always lands on the top
    /// suggestion), then wraps in the given direction. No-op if there are
    /// no suggestions to select.
    fn move_suggestion_selection(&mut self, down: bool) {
        if self.suggestions.is_empty() {
            return;
        }
        let len = self.suggestions.len();
        self.selected_index = Some(match self.selected_index {
            None => 0,
            Some(i) if down => (i + 1) % len,
            Some(i) => (i + len - 1) % len,
        });
    }

    /// Fills `input_buffer`/the input line with the currently-selected
    /// suggestion by sending only the unmatched suffix — `input_buffer`
    /// is always a prefix of every entry in `suggestions` by construction
    /// (`matching_suggestions` only returns prefix matches), so no
    /// backspacing is ever needed to correct what's already typed. Does
    /// **not** send Enter — accepting only fills the line (see the design
    /// spec's "Accepting a suggestion" decision).
    fn accept_selected_suggestion(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.selected_index else { return };
        let Some(full) = self.suggestions.get(idx).cloned() else { return };
        // A miss shouldn't be reachable (suggestions are prefix-filtered),
        // but the paths that can make `input_buffer` diverge from the real
        // line are real — so treat it as exactly what it is, a desync,
        // rather than silently claiming a line we never sent (final review
        // finding 10).
        let Some(suffix) = full.strip_prefix(&self.input_buffer) else {
            self.desync_tracking(cx);
            return;
        };
        if !suffix.is_empty() {
            self.send_input(suffix.as_bytes());
        }
        self.input_buffer = full;
        self.suggestions.clear();
        self.selected_index = None;
    }

    // --- Keys reclaimed from gpui-component's Root bindings (see `main`). ---

    /// Ctrl+C → interrupt (0x03 / SIGINT), not the Root "Copy" action. Use
    /// Ctrl+Shift+C to copy a selection.
    ///
    /// Ctrl+C genuinely discards the shell's pending input line, so this is
    /// a full tracking *reset* (empty buffer, trusted again), not a desync —
    /// same end state a plain Enter leaves behind (final review finding 3).
    fn on_interrupt(&mut self, _: &Interrupt, _window: &mut Window, cx: &mut Context<Self>) {
        self.reset_tracking(cx);
        self.send_input(&[0x03]);
    }

    /// Tab → accept the highlighted suggestion if there is one, otherwise
    /// send a literal tab (shell completion), not Root focus navigation.
    ///
    /// The accept lives here rather than in `on_key_down` because Tab is
    /// dispatched as this bound action *before* any key listener runs (see
    /// this file's top-of-file comment), so a `"tab"` arm in `on_key_down`
    /// would be dead code. A tab that does reach the shell runs its
    /// completion, arbitrarily rewriting the line, so it desyncs tracking
    /// (final review finding 3).
    fn on_send_tab(&mut self, _: &SendTab, _window: &mut Window, cx: &mut Context<Self>) {
        if self.suggestions_enabled
            && !self.suggestions.is_empty()
            && self.selected_index.is_some()
        {
            self.accept_selected_suggestion(cx);
            cx.notify();
            return;
        }
        self.desync_tracking(cx);
        self.send_input(b"\t");
    }

    /// Shift+Tab → CSI Z (back-tab), not Root focus navigation. Whatever the
    /// program does with it, it isn't something `on_key_down`'s character
    /// tracking observed, so tracking desyncs (final review finding 3).
    fn on_send_back_tab(&mut self, _: &SendBackTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.desync_tracking(cx);
        self.send_input(b"\x1b[Z");
    }

    /// `secondary-shift-l`: erase the visible viewport only (not
    /// scrollback history) — a client-side force-clear, distinct from
    /// plain `Ctrl+L` (still passed through as a raw byte to the
    /// shell/remote program, which already binds it to clear-screen via
    /// readline in almost every shell).
    ///
    /// Erasing the viewport takes the echoed input line off screen with it,
    /// so the `input_buffer_is_echoed` check could no longer confirm what's
    /// typed — tracking desyncs (final review finding 3).
    fn on_clear_screen(&mut self, _: &ClearScreen, _window: &mut Window, cx: &mut Context<Self>) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        self.term.lock().clear_screen(ClearMode::All);
        self.desync_tracking(cx);
        cx.notify();
    }

    /// Mouse-down: start a selection. Click type (Simple / Semantic /
    /// Lines) comes from the platform's `click_count`. A right-click
    /// starts nothing here — `TerminalPanel` wraps this view in a
    /// `gpui_component` context menu instead (CLAUDE.md §1 boundary).
    fn on_mouse_down(
        &mut self,
        ev: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ev.button != MouseButton::Left {
            return;
        }
        // Snap the screen point to a grid point using the last paint's
        // cell metrics. If the canvas hasn't measured yet (very first
        // frame) we still know enough: the defaults are 80×24 cols/rows
        // and we approximate cell size as 0 so the snap produces a
        // well-defined (0, 0) — alacritty then expands the selection
        // from the cursor on the next mouse move.
        let local_x = f32::from(ev.position.x) - self.last_origin_x;
        let local_y = f32::from(ev.position.y) - self.last_origin_y;
        let pt = selection::screen_to_grid(
            local_x,
            local_y,
            self.last_cell_w,
            self.last_cell_h,
            self.last_cols,
            self.last_rows,
            {
                let t = self.term.lock();
                t.grid().display_offset()
            },
        );
        let side = selection::side_for(local_x, self.last_cell_w, pt.column.0, self.last_cols);
        let ty = selection::selection_type_for_click(ev.click_count)
            .expect("selection_type_for_click is total");
        {
            let mut t = self.term.lock();
            selection::start(&mut t, pt, side, ty);
        }
        self.selection_dragging = Some(ty);
        // While the user is mid-drag we want fresh paints on every move,
        // so propagate to GPUI. The drag flag also keeps render from
        // re-asserting a stale cursor.
        cx.notify();
    }

    /// Mouse-move during a left-button drag: extend the selection. We
    /// ignore moves while no drag is active (gpui fires moves whenever
    /// the cursor is over the view).
    fn on_mouse_move(
        &mut self,
        ev: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selection_dragging.is_none() {
            return;
        }
        let local_x = f32::from(ev.position.x) - self.last_origin_x;
        let local_y = f32::from(ev.position.y) - self.last_origin_y;
        let pt = selection::screen_to_grid(
            local_x,
            local_y,
            self.last_cell_w,
            self.last_cell_h,
            self.last_cols,
            self.last_rows,
            {
                let t = self.term.lock();
                t.grid().display_offset()
            },
        );
        let side = selection::side_for(local_x, self.last_cell_w, pt.column.0, self.last_cols);
        {
            let mut t = self.term.lock();
            selection::update(&mut t, pt, side);
        }
        cx.notify();
    }

    /// Mouse-up: end the drag. For Semantic / Lines selections, keep
    /// the selection (so the user can copy it with a follow-up shortcut
    /// or by re-clicking inside it on some terminals — we keep it
    /// unconditionally, simpler and matches alacritty). For Simple
    /// selections, we also keep; the explicit Ctrl+Shift+C path
    /// handles copy intent.
    fn on_mouse_up(
        &mut self,
        _ev: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selection_dragging.is_some() {
            self.selection_dragging = None;
            // Sync the cached selection text into the system clipboard.
            // This is the "select to copy" behaviour most terminal
            // emulators (including gnome-terminal, iTerm2, and alacritty
            // itself) ship by default; users who want explicit copy
            // only still have Ctrl+Shift+C.
            self.copy_selection_to_clipboard(cx);
            cx.notify();
        }
    }

    /// Scroll wheel: drive the alacritty grid's `display_offset`.
    fn on_scroll_wheel(
        &mut self,
        ev: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scroll = match ev.delta {
            ScrollDelta::Pixels(p) => {
                scrollback::delta_from_pixels(f32::from(p.y), self.last_cell_h)
            }
            ScrollDelta::Lines(p) => scrollback::delta_from_lines(p.y),
        };
        {
            let mut t = self.term.lock();
            scrollback::apply(&mut t, scroll);
        }
        cx.notify();
    }

    /// Middle-click: paste from the primary selection (X11) — on
    /// non-Linux platforms this falls back to the regular clipboard
    /// since gpui doesn't expose a primary-selection reader there.
    fn on_middle_click(
        &mut self,
        _ev: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.paste_from_clipboard(cx);
    }

    /// Whether there's a non-empty selection to copy — lets `TerminalPanel`
    /// (the `gpui_component` context-menu owner, CLAUDE.md §1 boundary)
    /// decide whether to grey out its "Copy" menu item without duplicating
    /// selection/clipboard logic there.
    pub fn has_selection(&self) -> bool {
        !selection::is_empty(&self.term.lock())
    }

    /// Copy the current selection to the system clipboard. No-op when
    /// the selection is empty / absent. `pub(crate)`: called directly by
    /// `TerminalPanel`'s right-click context menu, not just the in-view
    /// Ctrl+Shift+C shortcut.
    pub(crate) fn copy_selection_to_clipboard(&self, cx: &mut Context<Self>) {
        let text = {
            let t = self.term.lock();
            selection::selected_text(&t)
        };
        if let Some(s) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(s));
        }
    }

    /// Paste from the system clipboard. Honours the term's
    /// `BRACKETED_PASTE` mode by wrapping the payload in
    /// `ESC[200~…ESC[201~`. `pub(crate)`: called directly by
    /// `TerminalPanel`'s right-click context menu, not just the in-view
    /// Ctrl+Shift+V shortcut.
    ///
    /// `&mut self`: a paste drops arbitrary text onto the real input line
    /// without `on_key_down` seeing a single character of it, so local
    /// tracking is invalidated (final review finding 4).
    pub(crate) fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else {
            return;
        };
        let mode: TermMode = *self.term.lock().mode();
        if let Some(bytes) = encode_paste(&text, mode, PastePayload::Clipboard) {
            self.desync_tracking(cx);
            self.backend.write(&bytes);
            // Snapping the viewport on paste is consistent with
            // on_key_down's behaviour; the user is now interacting
            // with the live area.
            self.term
                .lock()
                .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Breathing room around the grid, in cell units: left/right get 2 columns,
/// top/bottom get 1 row (a full cell height reads as more whitespace than a
/// cell width does, so the vertical side uses a smaller count to match).
const EDGE_PADDING_COLS: f32 = 2.0;
const EDGE_PADDING_ROWS: f32 = 1.0;

/// Widest the suggestion dropdown is allowed to get, in pixels; longer
/// entries are truncated with an ellipsis. Also clamped to the grid's own
/// width at render time, so a narrow terminal never gets a popup wider than
/// itself (see the popup block in `render`).
const SUGGESTION_POPUP_MAX_WIDTH: f32 = 520.0;

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let font = self.font_config.to_font();
        let metrics = cell_metrics(window, &font, self.font_config.size);
        let banner_text = self.banner.as_ref().map(|b| conn_banner_text(&self.host_label, b));
        div()
            .track_focus(&self.focus_handle)
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .px(metrics.width * EDGE_PADDING_COLS)
            .py(metrics.height * EDGE_PADDING_ROWS)
            .on_key_down(cx.listener(Self::on_key_down))
            // Actions reclaiming Root-context keys (tab / shift-tab / ctrl-c).
            .on_action(cx.listener(Self::on_interrupt))
            .on_action(cx.listener(Self::on_send_tab))
            .on_action(cx.listener(Self::on_send_back_tab))
            .on_action(cx.listener(Self::on_clear_screen))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(terminal_canvas(
                view,
                self.term.clone(),
                self.backend.clone(),
                font,
                self.font_config.size,
                self.focus_handle.clone(),
            ))
            // Connecting/disconnected-SSH banner: overlays the last-painted
            // frame (left in place, not cleared) with a dimmed backdrop +
            // centered message, matching nyaterm's "connection lost"
            // treatment. Text comes from `conn_banner_text`.
            .when_some(banner_text, |this, text| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(hsla(0.0, 0.0, 0.0, 0.72))
                        .text_color(hsla(0.0, 0.0, 1.0, 1.0))
                        .child(text),
                )
            })
            // Suggestion dropdown, anchored to the cursor cell. Positions are
            // *parent-relative*, exactly like the banner overlay above (whose
            // `.inset_0()` is parent-relative by construction): an
            // `.absolute()` child of this `.relative()` div resolves its
            // insets against this div's own box, not the window. The cached
            // `last_origin_x/y` are the canvas's *window*-absolute origin
            // (what the mouse handlers subtract from window-space event
            // positions), so using them here double-counted however far this
            // terminal element sits from the window's top-left — header,
            // sidebar and all (final review finding 2). The correct
            // parent-relative offset of the canvas's (0,0) cell is simply
            // this div's own padding, computed right here from the same
            // `metrics` the `.px()`/`.py()` above use.
            .when(!self.suggestions.is_empty(), |el| {
                let pad_x = metrics.width * EDGE_PADDING_COLS;
                let pad_y = metrics.height * EDGE_PADDING_ROWS;
                let (row, col) = self.cursor_position();
                let count = self.suggestions.len();
                // Cap the width so a long historical command can't spill past
                // the terminal's right edge, and pull the popup left when the
                // cursor sits too close to that edge (final review finding 5).
                let grid_w = self.last_cols as f32 * self.last_cell_w;
                let max_w = SUGGESTION_POPUP_MAX_WIDTH.min(grid_w.max(1.0));
                let x = (col as f32 * self.last_cell_w).min((grid_w - max_w).max(0.0));
                // One row below the cursor when the whole list fits inside the
                // visible rows; otherwise flip above it — at the bottom of the
                // screen (where prompts usually are) a downward list would
                // render off the end of the terminal.
                let fits_below = self.last_rows == 0 || row + 1 + count <= self.last_rows;
                let selected = self.selected_index;
                el.child(
                    div()
                        .absolute()
                        .left(pad_x + px(x))
                        .map(|d| {
                            if fits_below {
                                d.top(pad_y + px((row + 1) as f32 * self.last_cell_h))
                            } else {
                                // `bottom` is measured from this div's bottom
                                // edge: (rows - row) rows of grid sit below the
                                // cursor row's top, plus one row of slack so
                                // the popup never covers the cursor row itself
                                // (the canvas height isn't an exact multiple of
                                // the cell height), plus the bottom padding.
                                let rows_below = self.last_rows.saturating_sub(row) + 1;
                                d.bottom(pad_y + px(rows_below as f32 * self.last_cell_h))
                            }
                        })
                        .flex()
                        .flex_col()
                        .max_w(px(max_w))
                        .overflow_hidden()
                        .bg(hsla(0.0, 0.0, 0.12, 0.97))
                        .border_1()
                        .border_color(hsla(0.0, 0.0, 0.35, 1.0))
                        .rounded_sm()
                        .py_0p5()
                        .children(self.suggestions.iter().enumerate().map(|(i, s)| {
                            let is_selected = selected == Some(i);
                            div()
                                .px_2()
                                .py_0p5()
                                .truncate()
                                .text_color(hsla(0.0, 0.0, 0.9, 1.0))
                                .when(is_selected, |row_el| {
                                    row_el.bg(hsla(210.0, 0.7, 0.45, 0.35))
                                })
                                .child(s.clone())
                        })),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_font_config_uses_bundled_fonts() {
        let config = FontConfig::default();
        // Bare literal (not `DEFAULT_FONT_FAMILY`) is intentional: this pins the
        // constant against drift rather than restating it.
        assert_eq!(config.family.as_ref(), "JetBrains Mono");
        assert_eq!(
            config.fallbacks,
            vec![
                SharedString::from(SYMBOL_FALLBACK),
                SharedString::from(CJK_FALLBACK),
            ]
        );
    }

    #[test]
    fn to_font_carries_fallback_chain() {
        let config = FontConfig::default();
        let font = config.to_font();
        assert_eq!(font.family.as_ref(), "JetBrains Mono");
        assert!(font.fallbacks.is_some());
    }

    #[test]
    fn set_font_fallbacks_replaces_chain_in_order() {
        let mut config = FontConfig::default();
        // Directly exercise the struct + to_font(), matching how the other
        // FontConfig tests in this module already work (TerminalView itself
        // needs a live gpui window to construct).
        config.fallbacks = vec!["Fallback One".into(), "Fallback Two".into()];
        let font = config.to_font();
        let fallbacks = font.fallbacks.expect("fallbacks should be set");
        assert_eq!(
            fallbacks.fallback_list(),
            &["Fallback One".to_string(), "Fallback Two".to_string()]
        );
    }

    #[test]
    fn conn_banner_text_connecting() {
        // Locale is process-global state (shared across all tests running in
        // this binary) — pin it explicitly rather than assuming whatever an
        // earlier test left active.
        rust_i18n::set_locale("zh-CN");
        assert_eq!(
            conn_banner_text("root@example.com", &ConnBanner::Connecting),
            rust_i18n::t!("Terminal.connecting", host = "root@example.com").to_string()
        );
    }

    #[test]
    fn conn_banner_text_failed_includes_reason() {
        rust_i18n::set_locale("zh-CN");
        assert_eq!(
            conn_banner_text(
                "root@example.com",
                &ConnBanner::Failed("连接失败: Connection refused".to_string())
            ),
            rust_i18n::t!(
                "Terminal.disconnected_press_enter",
                host = "root@example.com",
                reason = "连接失败: Connection refused"
            )
            .to_string()
        );
    }

    #[test]
    fn ime_from_marked_text_none_when_empty() {
        assert!(ImeState::from_marked_text(String::new()).is_none());
        assert!(ImeState::from_marked_text("zhong".into()).is_some());
    }

    #[test]
    fn ime_marked_text_range_is_utf16_units() {
        let cjk = ImeState::from_marked_text("中".into()).unwrap();
        assert_eq!(cjk.marked_text_range(), 0..1);
        let emoji = ImeState::from_marked_text("👍".into()).unwrap();
        assert_eq!(emoji.marked_text_range(), 0..2);
        let pinyin = ImeState::from_marked_text("zhong".into()).unwrap();
        assert_eq!(pinyin.marked_text_range(), 0..5);
    }

    #[test]
    fn composing_swallows_printable_and_enter_not_ctrl() {
        let none = gpui::Modifiers::default();
        let ctrl = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        assert!(!swallow_key_during_ime(false, "a", &none));
        assert!(swallow_key_during_ime(true, "a", &none));
        assert!(swallow_key_during_ime(true, "enter", &none));
        assert!(swallow_key_during_ime(true, "backspace", &none));
        assert!(!swallow_key_during_ime(true, "c", &ctrl));
    }
}
