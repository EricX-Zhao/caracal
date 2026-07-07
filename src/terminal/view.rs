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
    ClipboardItem, Context, FocusHandle, Focusable, Font, FontFallbacks, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString, Styled, Task,
    Window, div, font, px,
};

use crate::terminal::backend::{DeadBackend, LocalPty, PtyBackend};
use crate::terminal::bridge::{run_drain, run_feeder};
use crate::terminal::keymap::{PastePayload, encode_key, encode_paste};
use crate::terminal::model::{SharedTerm, new_term};
use crate::terminal::render::terminal_canvas;
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
gpui::actions!(caracal_terminal, [Interrupt, SendTab, SendBackTab]);

/// The `key_context` set on the terminal element; the reclaiming key bindings in
/// `main` target this same context.
pub const TERMINAL_KEY_CONTEXT: &str = "Terminal";

/// The bundled symbol font (registered in `main`) used as the default fallback so
/// Nerd Font / powerline glyphs resolve even when the primary font lacks them.
const SYMBOL_FALLBACK: &str = "Symbols Nerd Font";

/// The bundled CJK font (registered in `main`) used as a fallback so Chinese
/// glyphs resolve even on a system with no East Asian fonts installed (the
/// original cause of Windows mojibake — see the design spec).
const CJK_FALLBACK: &str = "Sarasa Mono SC";

/// The bundled default primary terminal font (registered in `main`). Hardcoded
/// rather than relying on per-OS "system monospace" detection, which had no
/// working implementation on Windows/macOS (see `system_monospace_family`,
/// kept below for the explicit "reset to system font" path).
const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";

/// User-configurable terminal font. Not hardcoded to any specific family — the
/// primary defaults to the system monospace; a settings UI can later swap it via
/// [`TerminalView::set_font_family`] / [`set_font_size`] / [`set_font_config`].
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
/// string `"monospace"` is not a font and fails to resolve. Falls back to the
/// literal string on macOS/detection failure (unreported there so far).
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

pub struct TerminalView {
    term: SharedTerm,
    backend: Arc<dyn PtyBackend>,
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
    _drain_task: Task<()>,
}

impl TerminalView {
    /// A terminal backed by the local shell (`LocalPty`).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_backend(window, cx, |cols, rows, bytes_tx| {
            Arc::new(LocalPty::spawn(cols, rows, bytes_tx).expect("failed to spawn local pty"))
        })
    }

    /// A terminal backed by a local shell with custom shell path and working directory.
    pub fn new_local_with(
        window: &mut Window,
        cx: &mut Context<Self>,
        shell: &str,
        working_dir: Option<&str>,
    ) -> Self {
        let shell = shell.to_string();
        Self::with_backend(window, cx, move |cols, rows, bytes_tx| {
            Arc::new(
                LocalPty::spawn_with(cols, rows, bytes_tx, &shell, working_dir)
                    .expect("failed to spawn local pty"),
            )
        })
    }

    /// A terminal backed by a shell channel on an already-connected [`SshSession`]
    /// (shared with the SFTP panel — one connection per host, CLAUDE.md §2).
    pub fn new_ssh_shell(
        window: &mut Window,
        cx: &mut Context<Self>,
        session: Arc<SshSession>,
    ) -> Self {
        Self::with_backend(window, cx, move |cols, rows, bytes_tx| {
            session.open_shell(cols, rows, bytes_tx)
        })
    }

    /// A terminal backed by a raw Telnet connection (`TelnetBackend`). Each
    /// tab dials its own socket — unlike SSH, telnet has no SFTP-style
    /// second channel to justify a shared connection.
    pub fn new_telnet(window: &mut Window, cx: &mut Context<Self>, config: TelnetConfig) -> Self {
        Self::with_backend(window, cx, move |_cols, _rows, bytes_tx| {
            match TelnetBackend::connect(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mtelnet connect failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            }
        })
    }

    /// A terminal backed by a serial port (`SerialBackend`).
    pub fn new_serial(window: &mut Window, cx: &mut Context<Self>, config: SerialConfig) -> Self {
        Self::with_backend(window, cx, move |_cols, _rows, bytes_tx| {
            match SerialBackend::open(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mserial open failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            }
        })
    }

    /// Shared construction: wire the model, feeder, drain task, and focus; the
    /// backend is built by `make_backend` (given the initial size and the byte
    /// sink the feeder reads from). The backend is agnostic here — `TerminalView`
    /// never learns whether it's local, SSH, or serial (CLAUDE.md §2).
    fn with_backend(
        window: &mut Window,
        cx: &mut Context<Self>,
        make_backend: impl FnOnce(u16, u16, flume::Sender<Vec<u8>>) -> Arc<dyn PtyBackend>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Route keyboard to the terminal immediately.
        window.focus(&focus_handle, cx);

        // Channels (the only cross-context plumbing lives in the bridge).
        // The PTY byte channel is *bounded* so a fast producer can't outrun the
        // render-paced feeder: when it fills, the reader blocks, the PTY buffer
        // fills, and the program (e.g. `cat` of a huge file) blocks on write
        // instead of racing to completion. That keeps it alive and interruptible
        // (Ctrl-C reaches a live process) and bounds the post-interrupt backlog.
        // The feeder always drains this channel regardless of tab visibility, so
        // a backgrounded tab never deadlocks (CLAUDE.md §2).
        let (events_tx, events_rx) = flume::unbounded::<Event>();
        let (bytes_tx, bytes_rx) = flume::bounded::<Vec<u8>>(PTY_CHANNEL_CAPACITY);

        let term = new_term(DEFAULT_COLS, DEFAULT_ROWS, events_tx.clone());

        let backend = make_backend(DEFAULT_COLS as u16, DEFAULT_ROWS as u16, bytes_tx);

        // Feeder thread: raw PTY bytes -> Term (via ANSI parser) -> Wakeup.
        {
            let feeder_term = term.clone();
            std::thread::Builder::new()
                .name("caracal-feeder".into())
                .spawn(move || run_feeder(feeder_term, bytes_rx, events_tx))
                .expect("failed to spawn feeder thread");
        }

        // Drain task: terminal events -> throttled redraw + PTY write-backs.
        let drain_backend = backend.clone();
        let drain_task = cx.spawn(async move |weak, cx| {
            run_drain(weak, events_rx, drain_backend, cx).await;
        });

        Self {
            term,
            backend,
            focus_handle,
            font_config: FontConfig::default(),
            title: "terminal".to_string(),
            exited: false,
            last_cell_w: 0.0,
            last_cell_h: 0.0,
            last_cols: 0,
            last_rows: 0,
            last_origin_x: 0.0,
            last_origin_y: 0.0,
            selection_dragging: None,
            _drain_task: drain_task,
        }
    }

    #[allow(dead_code)] // consumed by the panel adapter in Phase 5
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: String) {
        if !title.is_empty() {
            self.title = title;
        }
    }

    pub fn mark_exited(&mut self) {
        self.exited = true;
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

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
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

        let mode: TermMode = *self.term.lock().mode();
        if let Some(bytes) = encode_key(&ev.keystroke, mode) {
            self.send_input(&bytes);
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

    // --- Keys reclaimed from gpui-component's Root bindings (see `main`). ---

    /// Ctrl+C → interrupt (0x03 / SIGINT), not the Root "Copy" action. Use
    /// Ctrl+Shift+C to copy a selection.
    fn on_interrupt(&mut self, _: &Interrupt, _window: &mut Window, _cx: &mut Context<Self>) {
        self.send_input(&[0x03]);
    }

    /// Tab → send a literal tab (shell completion), not Root focus navigation.
    fn on_send_tab(&mut self, _: &SendTab, _window: &mut Window, _cx: &mut Context<Self>) {
        self.send_input(b"\t");
    }

    /// Shift+Tab → CSI Z (back-tab), not Root focus navigation.
    fn on_send_back_tab(&mut self, _: &SendBackTab, _window: &mut Window, _cx: &mut Context<Self>) {
        self.send_input(b"\x1b[Z");
    }

    /// Mouse-down: start a selection. Click type (Simple / Semantic /
    /// Lines) comes from the platform's `click_count`. A right-click
    /// starts nothing — we use it only to clear any existing selection
    /// in the future (Phase 5+ context-menu work).
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

    /// Copy the current selection to the system clipboard. No-op when
    /// the selection is empty / absent.
    fn copy_selection_to_clipboard(&self, cx: &mut Context<Self>) {
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
    /// `ESC[200~…ESC[201~`.
    fn paste_from_clipboard(&self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else {
            return;
        };
        let mode: TermMode = *self.term.lock().mode();
        if let Some(bytes) = encode_paste(&text, mode, PastePayload::Clipboard) {
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

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        div()
            .track_focus(&self.focus_handle)
            .key_context(TERMINAL_KEY_CONTEXT)
            .size_full()
            .on_key_down(cx.listener(Self::on_key_down))
            // Actions reclaiming Root-context keys (tab / shift-tab / ctrl-c).
            .on_action(cx.listener(Self::on_interrupt))
            .on_action(cx.listener(Self::on_send_tab))
            .on_action(cx.listener(Self::on_send_back_tab))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(terminal_canvas(
                view,
                self.term.clone(),
                self.backend.clone(),
                self.font_config.to_font(),
                self.font_config.size,
                self.focus_handle.clone(),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_font_config_uses_bundled_fonts() {
        let config = FontConfig::default();
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
}
