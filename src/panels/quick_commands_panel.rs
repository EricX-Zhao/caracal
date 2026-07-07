//! `QuickCommandsPanel`: the bottom-drawer quick-commands list, toggled from
//! the status bar's quick-commands icon (see `Workspace::render_status_bar`).
//! Minimal v1 (per the design spec): flat list, inline add/edit form, two
//! send modes (execute / append) — no categories, search, sort, pin, tags,
//! import, or variable templating.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};
use crate::quick_commands::{self, ExecutionMode, QuickCommand};
use crate::workspace::Workspace;

/// The inline "add/edit quick command" form.
struct QuickCommandForm {
    label: Entity<InputState>,
    command: Entity<InputState>,
    execution_mode: ExecutionMode,
    /// `Some(id)` when editing an existing command in place; `None` when
    /// adding a new one.
    edit_id: Option<String>,
}

pub struct QuickCommandsPanel {
    workspace: WeakEntity<Workspace>,
    commands: Vec<QuickCommand>,
    form: Option<QuickCommandForm>,
}

impl QuickCommandsPanel {
    /// No `window` param — unlike most panel constructors in this codebase,
    /// nothing here needs it: the form's `InputState`s are created lazily in
    /// `toggle_form`/`open_edit_form`, which each receive their own `window`
    /// from their click-handler call site (matches `StubPanel::new`'s
    /// window-less signature, used for the same reason).
    pub fn new(workspace: WeakEntity<Workspace>, _cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            commands: quick_commands::load(),
            form: None,
        }
    }

    fn generate_id() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("qc-{}", nanos)
    }

    fn persist(&self) {
        if let Err(e) = quick_commands::save(&self.commands) {
            log::error!("failed to save quick commands: {e}");
        }
    }

    fn toggle_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.is_some() {
            self.form = None;
        } else {
            self.form = Some(QuickCommandForm {
                label: cx.new(|cx| InputState::new(window, cx).placeholder("名称")),
                command: cx.new(|cx| InputState::new(window, cx).placeholder("命令")),
                execution_mode: ExecutionMode::Execute,
                edit_id: None,
            });
        }
        cx.notify();
    }

    fn open_edit_form(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cmd) = self.commands.iter().find(|c| c.id == id) else {
            return;
        };
        self.form = Some(QuickCommandForm {
            label: cx.new(|cx| InputState::new(window, cx).default_value(cmd.label.clone())),
            command: cx.new(|cx| InputState::new(window, cx).default_value(cmd.command.clone())),
            execution_mode: cmd.execution_mode,
            edit_id: Some(id),
        });
        cx.notify();
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let label = form.label.read(cx).value().to_string();
        let command = form.command.read(cx).value().to_string();
        if label.trim().is_empty() || command.trim().is_empty() {
            return;
        }
        if let Some(id) = form.edit_id.clone() {
            if let Some(existing) = self.commands.iter_mut().find(|c| c.id == id) {
                existing.label = label;
                existing.command = command;
                existing.execution_mode = form.execution_mode;
            }
        } else {
            self.commands.push(QuickCommand {
                id: Self::generate_id(),
                label,
                command,
                execution_mode: form.execution_mode,
            });
        }
        self.form = None;
        self.persist();
        cx.notify();
    }

    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        self.commands.retain(|c| c.id != id);
        self.persist();
        cx.notify();
    }

    fn send(&self, cmd: &QuickCommand, cx: &App) {
        let execute = matches!(cmd.execution_mode, ExecutionMode::Execute);
        let _ = self.workspace.read_with(cx, |ws, cx| {
            ws.send_to_focused_terminal(&cmd.command, execute, cx);
        });
    }

    fn set_form_mode(&mut self, mode: ExecutionMode, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.execution_mode = mode;
            cx.notify();
        }
    }

    /// One toggle pill's styling — shared visual idiom with
    /// `saved_connections.rs`'s connection-type pills.
    fn pill(id: &'static str, label: &str, active: bool, cx: &App) -> Stateful<Div> {
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(if active {
                cx.theme().primary
            } else {
                cx.theme().accent
            })
            .text_color(if active {
                cx.theme().primary_foreground
            } else {
                cx.theme().foreground
            })
            .child(label.to_string())
    }

    fn render_form(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let form = self.form.as_ref()?;
        let is_execute = matches!(form.execution_mode, ExecutionMode::Execute);
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .m_2()
                .p_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .child(Input::new(&form.label))
                .child(Input::new(&form.command))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            Self::pill("qc-mode-execute", "执行", is_execute, cx).on_click(
                                cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                    this.set_form_mode(ExecutionMode::Execute, cx);
                                }),
                            ),
                        )
                        .child(
                            Self::pill("qc-mode-append", "追加", !is_execute, cx).on_click(
                                cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                    this.set_form_mode(ExecutionMode::Append, cx);
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .justify_end()
                        .child(
                            div()
                                .id("qc-form-cancel")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .hover(|s| s.bg(cx.theme().accent))
                                .child("取消")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                    this.toggle_form(window, cx);
                                })),
                        )
                        .child(
                            div()
                                .id("qc-form-save")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .child("保存")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                    this.save_form(cx);
                                })),
                        ),
                ),
        )
    }

    /// One command row. Mirrors `saved_connections.rs`'s `render_connection`
    /// structure: a `clickable` sub-div (send) and a separate `action_bar`
    /// sub-div (edit/delete) as SIBLINGS under a non-clickable outer row —
    /// nesting the action icons' `.on_click` inside the row's own `.on_click`
    /// would make both fire on one click, so they must not be nested.
    fn render_row(cmd: &QuickCommand, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = cmd.id.clone();
        let cmd_for_send = cmd.clone();
        let id_for_edit = id.clone();
        let id_for_delete = id.clone();
        let mode_label = match cmd.execution_mode {
            ExecutionMode::Execute => "执行",
            ExecutionMode::Append => "追加",
        };

        let action_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .opacity(0.3)
            .child(
                div()
                    .id(SharedString::from(format!("qc-edit-{id_for_edit}")))
                    .p(px(4.0))
                    .rounded_sm()
                    .hover(|s| {
                        s.bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                    })
                    .child(icon(AppIcon::Pencil).text_color(cx.theme().muted_foreground))
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                        this.open_edit_form(id_for_edit.clone(), window, cx);
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("qc-delete-{id_for_delete}")))
                    .p(px(4.0))
                    .rounded_sm()
                    .hover(|s| {
                        s.bg(cx.theme().danger)
                            .text_color(cx.theme().danger_foreground)
                    })
                    .child(icon(AppIcon::Delete).text_color(cx.theme().muted_foreground))
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.delete(&id_for_delete, cx);
                    })),
            );

        let clickable = div()
            .id(SharedString::from(format!("qc-send-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_1()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_sm()
                    .child(cmd.label.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(mode_label),
            )
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.send(&cmd_for_send, cx);
            }));

        div()
            .id(SharedString::from(format!("qc-row-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .py_1()
            .rounded_md()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(clickable)
            .child(action_bar)
    }
}

impl Render for QuickCommandsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = self.render_form(cx);
        let commands = self.commands.clone();
        let is_empty = commands.is_empty();

        // Built with a `for` loop (not `.children(iter().map(...))`) reusing
        // `cx` sequentially each iteration — matches the established pattern
        // in `saved_connections.rs`'s `render_tree`/`render_ungrouped_section`
        // (`for group in ... { tree = tree.child(self.render_group(group, 0, cx)); }`),
        // which avoids passing `&mut Context<Self>` into a `.map()` closure.
        let mut list = div()
            .id("qc-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll();
        if is_empty {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_full()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("暂无快捷命令，点 + 添加"),
            );
        } else {
            for cmd in &commands {
                list = list.child(Self::render_row(cmd, cx));
            }
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("快捷命令"),
                    )
                    .child(
                        div()
                            .id("qc-toggle-form")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(if self.form.is_some() { "取消" } else { "+ 添加" })
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.toggle_form(window, cx);
                            })),
                    ),
            )
            .children(form)
            .child(list)
    }
}
