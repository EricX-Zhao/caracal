//! `TerminalPanel`: the adapter that lets a `terminal::TerminalView` live inside
//! a `gpui_component` dock. It does three things only (CLAUDE.md §1): embed the
//! inner entity, show a title (with a close button), and **delegate focus to the
//! inner view**. No other business logic.

use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, WeakEntity, Window, div,
};
use gpui_component::dock::{Panel, PanelControl, PanelEvent, PanelView, TabPanel};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};
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

/// Emitted when the dock actually removes this panel
/// (`Panel::on_removed`, below) — a generic, backend-agnostic "this tab
/// is gone" signal. `TerminalPanel` doesn't know or care why (matches
/// its "adapter only" mandate, file header above); `Workspace` is the
/// one that knows what, if anything, needs cleaning up for a given
/// backend kind (see `open_ssh` in `workspace.rs`, the only current
/// subscriber — local/Telnet/Serial tabs emit this too, just unobserved,
/// since they share no session to clean up).
#[derive(Clone, Debug)]
pub enum TerminalPanelEvent {
    Closed,
}

impl gpui::EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl Panel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "TerminalPanel"
    }

    /// Hide the "Expand / Zoom" icon in the tab strip. gpui-component's
    /// `TabPanel::render_toolbar` only renders that icon when `zoomable`
    /// returns `Some(PanelControl::Both | Toolbar)`; returning `None`
    /// makes the entire zoom branch fall through to `None` and no icon is
    /// emitted. The tab strip's "..." menu still offers Zoom In/Out as
    /// menu actions, so the feature isn't lost — just demoted to a less
    /// visible spot. We don't use TabPanel's tab-strip close button either
    /// (this gpui-component revision doesn't ship one), hence our own `X`
    /// in `title()`.
    fn zoomable(&self, _cx: &App) -> Option<PanelControl> {
        None
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
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
                    .child(icon(AppIcon::Delete))
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

    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TerminalPanelEvent::Closed);
    }
}
