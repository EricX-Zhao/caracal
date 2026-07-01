//! Top-level workspace: hosts the `gpui_component` `DockArea`, a left-dock
//! session list, and the central tabs (terminals + SFTP browsers). Subscribes to
//! the session list and opens the requested panel.
//!
//! One connection per host (CLAUDE.md §2): SSH sessions are cached by host key in
//! `ssh_sessions`, so a host's shell tab and SFTP tab share a single russh
//! connection instead of dialing twice.
//!
//! Background drain: every `TerminalView` owns its feeder thread + drain task,
//! kept alive while the dock holds the panel entity; unbounded event channels
//! mean a backgrounded tab never back-pressures. Switching back shows the latest.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, px,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement};

use crate::panels::session_list::{SessionItem, SessionList, SessionListEvent, SessionSpec};
use crate::panels::sftp::SftpPanel;
use crate::panels::terminal::TerminalPanel;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::view::TerminalView;

pub struct Workspace {
    dock_area: Entity<DockArea>,
    /// Shared SSH connections, keyed by `user@host:port`.
    ssh_sessions: HashMap<String, Arc<SshSession>>,
    _subscription: Subscription,
}

impl Workspace {
    pub fn new(initial_ssh: Option<SshConfig>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));
        let weak = dock_area.downgrade();

        // Left dock: the session list. Always offers a local shell, plus the
        // configured SSH host as both a shell and an SFTP entry.
        let mut items = vec![SessionItem::local()];
        if let Some(config) = initial_ssh.clone() {
            let label = format!("{}@{}", config.user, config.host);
            items.push(SessionItem::ssh(label.clone(), config.clone()));
            items.push(SessionItem::sftp(format!("{label} (sftp)"), config));
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
            ssh_sessions: HashMap::new(),
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

    /// Get the shared connection for `config`, connecting on first use. Returns
    /// `None` (and logs) on connection failure.
    fn ssh_session(&mut self, config: &SshConfig) -> Option<Arc<SshSession>> {
        let key = config.key();
        if let Some(session) = self.ssh_sessions.get(&key) {
            return Some(session.clone());
        }
        match SshSession::connect(config.clone()) {
            Ok(session) => {
                self.ssh_sessions.insert(key, session.clone());
                Some(session)
            }
            Err(e) => {
                log::error!("SSH connect to {} failed: {e}", config.key());
                None
            }
        }
    }

    /// Build the requested panel and add it as a new central tab. New terminals
    /// grab focus in their own constructor.
    fn open_session(&mut self, spec: SessionSpec, window: &mut Window, cx: &mut Context<Self>) {
        match spec {
            SessionSpec::Local => {
                let terminal = cx.new(|cx| TerminalView::new(window, cx));
                let panel = cx.new(|_cx| TerminalPanel::new(terminal));
                self.add_center(Arc::new(panel), window, cx);
            }
            SessionSpec::Ssh(config) => {
                if let Some(session) = self.ssh_session(&config) {
                    let terminal = cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session));
                    let panel = cx.new(|_cx| TerminalPanel::new(terminal));
                    self.add_center(Arc::new(panel), window, cx);
                }
            }
            SessionSpec::Sftp(config) => {
                if let Some(session) = self.ssh_session(&config) {
                    let label = format!("{}@{}", config.user, config.host);
                    let panel = cx.new(|cx| SftpPanel::new(session, label, cx));
                    self.add_center(Arc::new(panel), window, cx);
                }
            }
        }
    }

    fn add_center(
        &mut self,
        panel: Arc<dyn gpui_component::dock::PanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.add_panel(panel, DockPlacement::Center, None, window, cx);
        });
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}
