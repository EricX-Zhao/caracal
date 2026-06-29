//! `TerminalView`: the terminal entity. Holds the shared `Term`, the backend,
//! and focus; wires keyboard input; renders via `render::terminal_canvas`.
//!
//! It is agnostic to the backend kind (CLAUDE.md §2). This file contains no
//! `gpui_component` imports — the boundary is enforced here.

use std::sync::Arc;

use alacritty_terminal::event::Event;
use alacritty_terminal::term::TermMode;
use gpui::{
    Context, FocusHandle, Focusable, Font, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Pixels, Render, Styled, Task, Window, div, font, px,
};

use crate::terminal::backend::{LocalPty, PtyBackend};
use crate::terminal::bridge::{run_drain, run_feeder};
use crate::terminal::keymap::encode_key;
use crate::terminal::model::{SharedTerm, new_term};
use crate::terminal::render::terminal_canvas;

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;

pub struct TerminalView {
    term: SharedTerm,
    backend: Arc<dyn PtyBackend>,
    focus_handle: FocusHandle,
    font: Font,
    font_size: Pixels,
    title: String,
    exited: bool,
    _drain_task: Task<()>,
}

impl TerminalView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        // Route keyboard to the terminal immediately.
        window.focus(&focus_handle);

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
            font: font("JetBrains Mono"),
            font_size: px(14.0),
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
                self.font.clone(),
                self.font_size,
                self.focus_handle.clone(),
            ))
    }
}
