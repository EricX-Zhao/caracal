//! `StubPanel`: a labeled placeholder for the nyaterm side-panel categories that
//! caracal hasn't implemented yet (network, security, sync, AI assistant, active
//! sessions, command history, resource monitor). It renders a centered title +
//! hint so the activity-bar shell looks complete while the real panel is future
//! work. Purely presentational (CLAUDE.md §1) — no business logic.

use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, Window, div,
};
use gpui_component::ActiveTheme;

use crate::panels::activity_bar::PanelId;

pub struct StubPanel {
    focus_handle: FocusHandle,
    panel_id: PanelId,
}

impl StubPanel {
    /// Takes the `PanelId` rather than a pre-resolved label string —
    /// `PanelId::label()` calls `t!(...)` internally, and that must be
    /// re-evaluated fresh on every render (not baked in once here at
    /// construction), or a language switch later would never update this
    /// panel's title: this `StubPanel` entity is created once at
    /// `Workspace::new` and lives for the app's whole session, so any
    /// string resolved once here and stored would be frozen at whatever
    /// language was active at startup.
    pub fn new(panel_id: PanelId, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            panel_id,
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
                    .child(self.panel_id.label()),
            )
            .child(rust_i18n::t!("Stub.not_implemented"))
    }
}
