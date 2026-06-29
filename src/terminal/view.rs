//! `TerminalView`: the terminal entity. Holds the shared `Term`, the backend,
//! and focus; wires keyboard input; renders via `render::terminal_canvas`.
//!
//! It is agnostic to the backend kind (CLAUDE.md §2). This file contains no
//! `gpui_component` imports — the boundary is enforced here.

use std::sync::Arc;

use alacritty_terminal::event::Event;
use alacritty_terminal::term::TermMode;
use gpui::{
    Context, FocusHandle, Focusable, Font, FontFallbacks, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Pixels, Render, SharedString, Styled, Task, Window, div, font, px,
};

use crate::terminal::backend::{LocalPty, PtyBackend};
use crate::terminal::bridge::{run_drain, run_feeder};
use crate::terminal::keymap::encode_key;
use crate::terminal::model::{SharedTerm, new_term};
use crate::terminal::render::terminal_canvas;

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;

/// The bundled symbol font (registered in `main`) used as the default fallback so
/// Nerd Font / powerline glyphs resolve even when the primary font lacks them.
const SYMBOL_FALLBACK: &str = "Symbols Nerd Font Mono";

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
            family: system_monospace_family(),
            size: px(14.0),
            fallbacks: vec![SYMBOL_FALLBACK.into()],
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

/// The system's default monospace family. We resolve it ourselves (via
/// fontconfig on Linux) because gpui doesn't map the generic `"monospace"`
/// alias. Falls back to `"monospace"` if detection fails.
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
    "monospace".into()
}

pub struct TerminalView {
    term: SharedTerm,
    backend: Arc<dyn PtyBackend>,
    focus_handle: FocusHandle,
    font_config: FontConfig,
    title: String,
    exited: bool,
    _drain_task: Task<()>,
}

impl TerminalView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        // Route keyboard to the terminal immediately.
        window.focus(&focus_handle, cx);

        // Channels (the only cross-context plumbing lives in the bridge).
        let (events_tx, events_rx) = flume::unbounded::<Event>();
        let (bytes_tx, bytes_rx) = flume::unbounded::<Vec<u8>>();

        let term = new_term(DEFAULT_COLS, DEFAULT_ROWS, events_tx.clone());

        let backend: Arc<dyn PtyBackend> = Arc::new(
            LocalPty::spawn(DEFAULT_COLS as u16, DEFAULT_ROWS as u16, bytes_tx)
                .expect("failed to spawn local pty"),
        );

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

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let mode: TermMode = *self.term.lock().mode();
        if let Some(bytes) = encode_key(&ev.keystroke, mode) {
            self.backend.write(&bytes);
            // Typing while scrolled back should snap to the bottom (Phase 3 will
            // refine this); harmless no-op at offset 0.
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
        div()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .size_full()
            .on_key_down(cx.listener(Self::on_key_down))
            .child(terminal_canvas(
                self.term.clone(),
                self.backend.clone(),
                self.font_config.to_font(),
                self.font_config.size,
                self.focus_handle.clone(),
            ))
    }
}
