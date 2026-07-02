//! `StubPanel`: a labeled placeholder for the nyaterm side-panel categories that
//! caracal hasn't implemented yet (network, security, sync, AI assistant, active
//! sessions, command history, resource monitor). It renders a centered title +
//! hint so the activity-bar shell looks complete while the real panel is future
//! work. Purely presentational (CLAUDE.md §1) — no business logic.

use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div,
};
use gpui_component::ActiveTheme;

pub struct StubPanel {
    focus_handle: FocusHandle,
    title: SharedString,
}

impl StubPanel {
    pub fn new(title: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            title: title.into(),
        }
    }
}

impl Focusable for StubPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StubPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .text_color(cx.theme().foreground)
                    .child(self.title.clone()),
            )
            .child("此面板尚未实现")
    }
}
