//! `SavedConnectionsPanel`: the right-dock "已保存的连接" list. Renders the
//! persisted SSH connections ([`crate::config`]) as nyaterm-style rows; clicking
//! one emits [`SavedConnectionsEvent::Open`] (workspace opens the SSH terminal).
//! A `+` button reveals an inline add-connection form. No connection logic here —
//! it only describes intent and persists the list (CLAUDE.md §1 boundary).

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::input::{Input, InputState};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};

use crate::config::{self, AppConfig, SavedConnection};
use crate::terminal::ssh::SshConfig;

/// Emitted when the user picks a saved connection to open.
pub enum SavedConnectionsEvent {
    /// Open an SSH shell terminal.
    Open(SshConfig),
    /// Open an SFTP browser (routed to the bottom "SFTP" dock).
    OpenSftp(SshConfig),
}

/// The inline "add connection" form: one text input per field. Created lazily
/// (needs a `Window`) when the `+` button is pressed.
struct ConnForm {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
}

pub struct SavedConnectionsPanel {
    focus_handle: FocusHandle,
    connections: Vec<SavedConnection>,
    form: Option<ConnForm>,
}

impl SavedConnectionsPanel {
    pub fn new(connections: Vec<SavedConnection>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            connections,
            form: None,
        }
    }

    /// Toggle the add-connection form. Input widgets need a `Window`, so they are
    /// built here rather than in the constructor.
    fn toggle_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.is_some() {
            self.form = None;
        } else {
            self.form = Some(ConnForm {
                name: cx.new(|cx| InputState::new(window, cx).placeholder("名称(可选)")),
                host: cx.new(|cx| InputState::new(window, cx).placeholder("主机 host")),
                port: cx.new(|cx| InputState::new(window, cx).placeholder("端口(默认 22)")),
                user: cx.new(|cx| InputState::new(window, cx).placeholder("用户名 user")),
                password: cx
                    .new(|cx| InputState::new(window, cx).masked(true).placeholder("密码")),
            });
        }
        cx.notify();
    }

    /// Read the form, append a connection, persist, and close the form. Host is
    /// required; an empty host is a no-op.
    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let host = form.host.read(cx).value().trim().to_string();
        if host.is_empty() {
            return;
        }
        let conn = SavedConnection {
            name: form.name.read(cx).value().trim().to_string(),
            host,
            port: form.port.read(cx).value().trim().parse().unwrap_or(22),
            user: form.user.read(cx).value().trim().to_string(),
            password: form.password.read(cx).value().to_string(),
        };
        self.connections.push(conn);
        self.form = None;
        self.persist();
        cx.notify();
    }

    fn delete(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.connections.len() {
            self.connections.remove(ix);
            self.persist();
            cx.notify();
        }
    }

    fn persist(&self) {
        let cfg = AppConfig {
            connections: self.connections.clone(),
        };
        if let Err(e) = config::save(&cfg) {
            log::error!("failed to save connections: {e}");
        }
    }

    /// One labelled input line inside the form.
    fn field(&self, label: &str, state: &Entity<InputState>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .child(SharedString::from(label.to_string())),
            )
            .child(Input::new(state))
    }
}

impl EventEmitter<SavedConnectionsEvent> for SavedConnectionsPanel {}
impl EventEmitter<PanelEvent> for SavedConnectionsPanel {}

impl Focusable for SavedConnectionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for SavedConnectionsPanel {
    fn panel_name(&self) -> &'static str {
        "SavedConnections"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("已保存的连接")
    }
}

impl SavedConnectionsPanel {
    fn render_form(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let form = self.form.as_ref()?;
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .m_2()
                .p_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .child(self.field("名称", &form.name))
                .child(self.field("主机", &form.host))
                .child(self.field("端口", &form.port))
                .child(self.field("用户名", &form.user))
                .child(self.field("密码", &form.password))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .justify_end()
                        .child(
                            div()
                                .id("conn-cancel")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .hover(|s| s.bg(cx.theme().accent))
                                .child("取消")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    this.form = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("conn-save")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .child("保存")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    this.save_form(cx)
                                })),
                        ),
                ),
        )
    }
}

impl Render for SavedConnectionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The dock tab already shows the panel title ("已保存的连接"), so this
        // toolbar only needs the add-connection action.
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .px_3()
            .py_1()
            .child(
                div()
                    .id("conn-add")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(icon(AppIcon::Plus))
                    .child("新增")
                    .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                        this.toggle_form(window, cx)
                    })),
            );

        let rows: Vec<_> = self.connections.iter().enumerate().map(|(ix, conn)| {
            let spec = conn.to_ssh_config();
            let sftp_spec = spec.clone();
            let name = conn.display_name();
            let subtitle = conn.subtitle();
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .py_1()
                .rounded_md()
                .hover(|s| s.bg(cx.theme().list_hover))
                .child(
                    // Clickable part (icon + labels) → open the connection.
                    div()
                        .id(("conn", ix))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .child(
                            icon(AppIcon::Terminal)
                                .text_color(cx.theme().foreground),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_color(cx.theme().foreground).child(name))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(subtitle),
                                ),
                        )
                        .on_click(cx.listener(move |_this, _ev: &ClickEvent, _w, cx| {
                            cx.emit(SavedConnectionsEvent::Open(spec.clone()));
                        })),
                )
                .child(
                    // SFTP (sibling of the clickable part, so it doesn't open a shell).
                    div()
                        .id(("conn-sftp", ix))
                        .p_1()
                        .rounded_sm()
                        .text_color(cx.theme().muted_foreground)
                        .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                        .child(icon(AppIcon::Folder))
                        .on_click(cx.listener(move |_this, _ev: &ClickEvent, _w, cx| {
                            cx.emit(SavedConnectionsEvent::OpenSftp(sftp_spec.clone()));
                        })),
                )
                .child(
                    // Delete (sibling of the clickable part, so it doesn't open).
                    div()
                        .id(("conn-del", ix))
                        .p_1()
                        .rounded_sm()
                        .text_color(cx.theme().muted_foreground)
                        .hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
                        .child(icon(AppIcon::Delete))
                        .on_click(cx.listener(move |this, _ev: &ClickEvent, _w, cx| {
                            this.delete(ix, cx)
                        })),
                )
        }).collect();

        let empty_hint = self.connections.is_empty().then(|| {
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("暂无保存的连接,点 + 新增")
        });

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap_1()
            .py_1()
            .size_full()
            .text_sm()
            .child(header)
            .children(self.render_form(cx))
            .children(empty_hint)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_1()
                    .child(div().w_full().children(rows)),
            )
    }
}
