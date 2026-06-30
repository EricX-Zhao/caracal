//! Top-level workspace: hosts the `gpui_component` `DockArea`, a left-dock
//! session list, and the central terminal tabs. Subscribes to the session list
//! and opens a new terminal tab per request (Phase 6).
//!
//! Background drain: every `TerminalView` owns its feeder thread + drain task,
//! which keep running while the entity is alive — and the dock keeps every
//! panel's entity alive even when its tab isn't visible. flume channels are
//! unbounded, so a backgrounded tab never back-pressures its IO thread
//! (CLAUDE.md §2). So switching back to a background tab shows the latest output
//! with no extra wiring here.

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, px,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement};

use crate::panels::session_list::{SessionItem, SessionList, SessionListEvent, SessionSpec};
use crate::panels::terminal::TerminalPanel;
use crate::terminal::ssh::SshConfig;
use crate::terminal::view::TerminalView;

pub struct Workspace {
    dock_area: Entity<DockArea>,
    _subscription: Subscription,
}

impl Workspace {
    pub fn new(initial_ssh: Option<SshConfig>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));
        let weak = dock_area.downgrade();

        // Left dock: the session list. Always offers a local shell, plus the
        // SSH host from the environment if one was configured.
        let mut items = vec![SessionItem::local()];
        if let Some(config) = initial_ssh.clone() {
            let label = format!("{}@{}", config.user, config.host);
            items.push(SessionItem::ssh(label, config));
        }
        let session_list = cx.new(|cx| SessionList::new(items, cx));

        let subscription =
            cx.subscribe_in(&session_list, window, |this, _list, event, window, cx| {
                let SessionListEvent::Open(spec) = event;
                this.open_session(spec.clone(), window, cx);
            });

        dock_area.update(cx, |dock_area, cx| {
            dock_area.set_left_dock(
                DockItem::tab(session_list, &weak, window, cx),
                Some(px(200.0)),
                true,
                window,
                cx,
            );
        });

        let mut this = Self {
            dock_area,
            _subscription: subscription,
        };

        // Open the initial central tab (the configured SSH host, else local).
        let initial = match initial_ssh {
            Some(config) => SessionSpec::Ssh(config),
            None => SessionSpec::Local,
        };
        this.open_session(initial, window, cx);

        this
    }

    /// Build a terminal for `spec` and add it as a new central tab. The new
    /// terminal grabs focus in its own constructor.
    fn open_session(&mut self, spec: SessionSpec, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| match spec {
            SessionSpec::Local => TerminalView::new(window, cx),
            SessionSpec::Ssh(config) => TerminalView::new_ssh(window, cx, config),
        });
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));

        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.add_panel(Arc::new(panel), DockPlacement::Center, None, window, cx);
        });
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}
