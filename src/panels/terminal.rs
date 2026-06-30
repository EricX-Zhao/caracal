//! `TerminalPanel`: the adapter that lets a `terminal::TerminalView` live inside
//! a `gpui_component` dock. It does three things only (CLAUDE.md §1): embed the
//! inner entity, show a title, and **delegate focus to the inner view**. No
//! business logic.

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, Styled,
    Window, div,
};
use gpui_component::dock::{Panel, PanelEvent};

use crate::terminal::view::TerminalView;

pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
}

impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>) -> Self {
        Self { terminal }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Just embed the inner terminal entity; it renders/handles input itself.
        div().size_full().child(self.terminal.clone())
    }
}

impl Focusable for TerminalPanel {
    /// Delegate to the inner view's handle — returning our own would swallow all
    /// keyboard input (CLAUDE.md §2: focus 委托).
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.read(cx).focus_handle(cx)
    }
}

impl gpui::EventEmitter<PanelEvent> for TerminalPanel {}

impl Panel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "TerminalPanel"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.terminal.read(cx).title().to_string()
    }
}
