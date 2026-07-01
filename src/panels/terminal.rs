//! `TerminalPanel`: the adapter that lets a `terminal::TerminalView` live inside
//! a `gpui_component` dock. It does three things only (CLAUDE.md §1): embed the
//! inner entity, show a title (with a close button), and **delegate focus to the
//! inner view**. No other business logic.

use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, WeakEntity, Window, div,
};
use gpui_component::dock::{Panel, PanelEvent, PanelView, TabPanel};
use gpui_component::{ActiveTheme, Icon, IconName};
use std::sync::Arc;

use crate::terminal::view::TerminalView;

pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    /// The `TabPanel` this panel currently lives in, handed to us via
    /// `on_added_to`. Needed so the close button (embedded in `title()`, since
    /// this gpui-component revision's tab strip has no built-in per-tab close
    /// icon) can remove *this specific* panel regardless of which tab is active.
    tab_panel: Option<WeakEntity<TabPanel>>,
}

impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>) -> Self {
        Self {
            terminal,
            tab_panel: None,
        }
    }

    fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_panel) = self.tab_panel.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        let this: Arc<dyn PanelView> = Arc::new(cx.entity());
        // Deferred: this runs inside a `cx.listener` on *this* panel, which
        // GPUI already wraps in an `Entity<TerminalPanel>::update(..)` call.
        // Calling `remove_panel` synchronously would trigger our own
        // `on_removed` hook, re-entering that same update — GPUI panics on
        // nested self-updates ("cannot update X while it is already being
        // updated"). Deferring runs it after the current update finishes,
        // the same way gpui-component's own `TabPanel::on_action_close_panel`
        // defers its cleanup.
        window.defer(cx, move |window, cx| {
            tab_panel.update(cx, |tab_panel, cx| {
                tab_panel.remove_panel(this, window, cx);
            });
        });
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
        let title = self.terminal.read(cx).title().to_string();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(title)
            .child(
                div()
                    .id(("close-terminal", cx.entity_id()))
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
                    .child(Icon::new(IconName::Close))
                    .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                        // Don't also let the tab strip's own click handler fire
                        // (it would select this tab right before we remove it).
                        cx.stop_propagation();
                        this.close(window, cx);
                    })),
            )
    }

    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
    }
}
