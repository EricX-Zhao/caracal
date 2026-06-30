//! `SessionList`: the left-dock panel listing configured sessions. Clicking a
//! row emits [`SessionListEvent::Open`]; `workspace.rs` subscribes and opens a
//! new terminal tab. No connection logic here — it only describes intent.

use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    div,
};
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};

use crate::terminal::ssh::SshConfig;

/// What a session row opens.
#[derive(Clone)]
pub enum SessionSpec {
    Local,
    Ssh(SshConfig),
}

/// One entry in the list.
pub struct SessionItem {
    pub label: SharedString,
    pub spec: SessionSpec,
}

impl SessionItem {
    pub fn local() -> Self {
        Self {
            label: "Local shell".into(),
            spec: SessionSpec::Local,
        }
    }

    pub fn ssh(label: impl Into<SharedString>, config: SshConfig) -> Self {
        Self {
            label: label.into(),
            spec: SessionSpec::Ssh(config),
        }
    }
}

/// Emitted when the user picks a session to open.
pub enum SessionListEvent {
    Open(SessionSpec),
}

pub struct SessionList {
    focus_handle: FocusHandle,
    items: Vec<SessionItem>,
}

impl SessionList {
    pub fn new(items: Vec<SessionItem>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            items,
        }
    }
}

impl EventEmitter<SessionListEvent> for SessionList {}
impl EventEmitter<PanelEvent> for SessionList {}

impl Focusable for SessionList {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for SessionList {
    fn panel_name(&self) -> &'static str {
        "SessionList"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Sessions")
    }
}

impl Render for SessionList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.items.iter().enumerate().map(|(ix, item)| {
            let spec = item.spec.clone();
            div()
                .id(("session", ix))
                .w_full()
                .px_2()
                .py_1()
                .rounded_md()
                .text_color(cx.theme().foreground)
                .hover(|s| s.bg(cx.theme().accent))
                .child(item.label.clone())
                .on_click(cx.listener(move |_this, _ev: &ClickEvent, _window, cx| {
                    cx.emit(SessionListEvent::Open(spec.clone()));
                }))
        });

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .size_full()
            .children(rows)
    }
}
