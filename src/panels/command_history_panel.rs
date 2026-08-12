//! `CommandHistoryPanel`: the right-sidebar "命令历史" panel. Shows one
//! connection's persisted command history (`command_history::load_for`),
//! live-filterable by substring, with click-to-fill (not execute) on any
//! row. See docs/superpowers/specs/2026-08-06-command-history-panel-design.md.

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Sizable};

use crate::command_history;
use crate::panels::icons::{AppIcon, icon};
use crate::workspace::Workspace;

pub struct CommandHistoryPanel {
    focus_handle: FocusHandle,
    history_key: String,
    /// The connection's display label at the moment this panel was first
    /// created for `history_key` — set once, like `MonitorPanel::label`,
    /// and not refreshed on later focus events (see `Workspace::show_history`'s
    /// doc comment for why: multiple tabs can share one `history_key`).
    label: SharedString,
    entries: Vec<String>,
    search_query: Entity<InputState>,
    /// Back-reference so a row click can send its text to whichever
    /// terminal currently has focus — mirrors `QuickCommandsPanel.workspace`.
    workspace: WeakEntity<Workspace>,
}

impl CommandHistoryPanel {
    pub fn new(
        history_key: String,
        label: SharedString,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entries = command_history::load_for(&history_key);
        let search_query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("CommandHistory.search_placeholder"))
        });
        Self {
            focus_handle: cx.focus_handle(),
            history_key,
            label,
            entries,
            search_query,
            workspace,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.entries = command_history::load_for(&self.history_key);
        cx.notify();
    }

    fn send(&self, line: &str, cx: &mut App) {
        let _ = self.workspace.update(cx, |ws, cx| {
            ws.send_to_focused_terminal(line, false, cx);
        });
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
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
                    .text_sm()
                    .child(rust_i18n::t!("CommandHistory.title", label = self.label.clone())),
            )
            .child(
                Button::new("command-history-refresh")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Refresh))
                    .tooltip(rust_i18n::t!("CommandHistory.refresh_tooltip"))
                    .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| this.refresh(cx))),
            )
    }

    /// One history row. `idx` (the row's position in the already-filtered,
    /// newest-first list) is the element id suffix rather than a hash of
    /// `line`'s content, because non-consecutive duplicate entries are a
    /// real, valid case (see `command_history.rs`'s
    /// `record_into_allows_a_non_consecutive_repeat` test) and a
    /// content-hash id would collide between two identical entries; `idx`
    /// is always unique within one render pass.
    fn render_row(idx: usize, line: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let line_for_click = line.clone();
        div()
            .id(SharedString::from(format!("ch-row-{idx}")))
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .min_w(px(0.0))
            .overflow_hidden()
            .text_ellipsis()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(line)
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.send(&line_for_click, cx);
            }))
    }
}

impl Focusable for CommandHistoryPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommandHistoryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search_query.read(cx).value().to_string();
        let filtered = command_history::filter_entries(&self.entries, &query);
        let is_empty = filtered.is_empty();

        let mut list = div()
            .id("ch-list")
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
                    .child(rust_i18n::t!("CommandHistory.empty_state")),
            );
        } else {
            for (idx, line) in filtered.into_iter().enumerate() {
                list = list.child(Self::render_row(idx, line, cx));
            }
        }

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_header(cx))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .child(
                        Input::new(&self.search_query)
                            .prefix(icon(AppIcon::Search).text_color(cx.theme().muted_foreground))
                            .w_full(),
                    ),
            )
            .child(list)
    }
}

// --- placeholder for when no terminal has ever been focused -----------------

pub struct CommandHistoryPlaceholder {
    focus_handle: FocusHandle,
}

impl CommandHistoryPlaceholder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for CommandHistoryPlaceholder {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommandHistoryPlaceholder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("CommandHistory.no_terminal_focused"))
    }
}
