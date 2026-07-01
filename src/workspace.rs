//! Top-level workspace: hosts the `gpui_component` `DockArea` — a left-dock
//! SFTP browser, a right-dock saved-connections list, central terminal tabs,
//! and two edge-mounted vertical icon strips that toggle the left/right docks.
//! A slim bottom status bar is reserved for future info (connection state,
//! cwd, …) — currently empty.
//!
//! One connection per host (CLAUDE.md §2): SSH sessions are cached by host key in
//! `ssh_sessions`, so a host's shell tab and SFTP tab share a single russh
//! connection instead of dialing twice.
//!
//! SFTP follows terminal focus and the left dock holds exactly one tab: when
//! focus moves to an SSH terminal, its host's SFTP browser replaces whatever
//! was showing; when focus moves to a non-SSH terminal (local shell — serial,
//! once it exists), the browser is swapped back out for a placeholder, since
//! SFTP only makes sense over an SSH connection. See `show_sftp` /
//! `show_sftp_placeholder`. The folder icon in "已保存的连接" still works too,
//! as a manual way to browse a host without opening its terminal.
//!
//! Background drain: every `TerminalView` owns its feeder thread + drain task,
//! kept alive while the dock holds the panel entity; unbounded event channels
//! mean a backgrounded tab never back-pressures. Switching back shows the latest.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder,
    px,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use crate::config;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::terminal::TerminalPanel;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::view::TerminalView;

/// What's currently occupying the left dock's single tab.
enum LeftDockContent {
    Placeholder(Entity<SftpPlaceholder>),
    Sftp { key: String, panel: Entity<SftpPanel> },
}

pub struct Workspace {
    dock_area: Entity<DockArea>,
    /// Shared SSH connections, keyed by `user@host:port`.
    ssh_sessions: HashMap<String, Arc<SshSession>>,
    /// The left dock's one live tab (never more, never fewer) — see the
    /// module doc comment on SFTP focus-follow.
    left_dock_content: LeftDockContent,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(initial_ssh: Option<SshConfig>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));
        let weak = dock_area.downgrade();

        // Right dock: the persisted "已保存的连接" list. Loaded from disk; clicking
        // a row opens the SSH terminal or its SFTP browser.
        let saved = cx.new(|cx| SavedConnectionsPanel::new(config::load().connections, cx));
        let saved_sub =
            cx.subscribe_in(&saved, window, |this, _panel, event, window, cx| match event {
                SavedConnectionsEvent::Open(config) => this.open_ssh(config.clone(), window, cx),
                SavedConnectionsEvent::OpenSftp(config) => {
                    this.show_sftp(config.clone(), window, cx)
                }
            });

        // Left dock: exactly one tab, an SFTP browser or (initially, and
        // whenever a non-SSH terminal is focused) this placeholder.
        let sftp_placeholder = cx.new(|cx| SftpPlaceholder::new(cx));

        dock_area.update(cx, |dock_area, cx| {
            dock_area.set_left_dock(
                DockItem::tab(sftp_placeholder.clone(), &weak, window, cx),
                Some(px(260.0)),
                false,
                window,
                cx,
            );
            dock_area.set_right_dock(
                DockItem::tab(saved, &weak, window, cx),
                Some(px(240.0)),
                true,
                window,
                cx,
            );
        });

        let mut this = Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            left_dock_content: LeftDockContent::Placeholder(sftp_placeholder),
            _subscriptions: vec![saved_sub],
        };

        // Open the initial central tab (the configured SSH host, else local).
        match initial_ssh {
            Some(config) => this.open_ssh(config, window, cx),
            None => this.open_local(window, cx),
        }

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

    /// Open a local-shell terminal as a new central tab, and wire it so
    /// focusing it swaps the left dock back to the SFTP placeholder (SFTP
    /// only makes sense over SSH).
    fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new(window, cx));
        let handle = terminal.read(cx).focus_handle(cx);
        let sub = cx.on_focus(&handle, window, |this, window, cx| {
            this.show_sftp_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        // The terminal already grabbed focus inside its own constructor —
        // before the `on_focus` listener above existed to observe it — so the
        // very first open needs an explicit follow-up call (mirrors `open_ssh`).
        self.show_sftp_placeholder(window, cx);
    }

    /// Open an SSH shell terminal (reusing the host's shared connection) as a
    /// new central tab, and wire it so refocusing it later swaps its host's
    /// SFTP browser into the left dock.
    fn open_ssh(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.ssh_session(&config) {
            let terminal = cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session));
            let follow = config.clone();
            let handle = terminal.read(cx).focus_handle(cx);
            let sub = cx.on_focus(&handle, window, move |this, window, cx| {
                this.show_sftp(follow.clone(), window, cx);
            });
            self._subscriptions.push(sub);
            let panel = cx.new(|_cx| TerminalPanel::new(terminal));
            self.add_center(Arc::new(panel), window, cx);
            // The terminal already grabbed focus inside its own constructor —
            // before the `on_focus` listener above existed to observe it — so
            // the very first open needs an explicit follow-up call.
            self.show_sftp(config, window, cx);
        }
    }

    /// Swap the left dock's one tab to `config`'s SFTP browser (reusing the
    /// host's shared connection) and force the dock open. No-op if that
    /// host's browser is already showing.
    fn show_sftp(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        let key = config.key();
        if let LeftDockContent::Sftp { key: current, .. } = &self.left_dock_content {
            if *current == key {
                self.reveal_left(window, cx);
                return;
            }
        }
        let Some(session) = self.ssh_session(&config) else {
            return;
        };
        let label = format!("{}@{}", config.user, config.host);
        let panel = cx.new(|cx| SftpPanel::new(session, label, cx));
        self.replace_left(Arc::new(panel.clone()), window, cx);
        self.left_dock_content = LeftDockContent::Sftp { key, panel };
        self.reveal_left(window, cx);
    }

    /// Swap the left dock's one tab back to the "no SFTP available" hint
    /// (the focused terminal isn't SSH). No-op if already showing it. Unlike
    /// `show_sftp`, this does not force the dock open — switching to a local
    /// tab shouldn't pop a dock the user closed on purpose.
    fn show_sftp_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.left_dock_content, LeftDockContent::Placeholder(_)) {
            return;
        }
        let placeholder = cx.new(|cx| SftpPlaceholder::new(cx));
        self.replace_left(Arc::new(placeholder.clone()), window, cx);
        self.left_dock_content = LeftDockContent::Placeholder(placeholder);
    }

    /// Remove whatever the left dock is currently showing and add `panel` in
    /// its place, so the dock always holds exactly one tab.
    fn replace_left(
        &mut self,
        panel: Arc<dyn gpui_component::dock::PanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old: Arc<dyn gpui_component::dock::PanelView> = match &self.left_dock_content {
            LeftDockContent::Placeholder(p) => Arc::new(p.clone()),
            LeftDockContent::Sftp { panel, .. } => Arc::new(panel.clone()),
        };
        self.dock_area.update(cx, |dock_area, cx| {
            dock_area.remove_panel(old, DockPlacement::Left, window, cx);
            dock_area.add_panel(panel, DockPlacement::Left, None, window, cx);
        });
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

    /// Force the left "SFTP" dock open, regardless of prior `toggle_dock` state.
    fn reveal_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dock) = self.dock_area.read(cx).left_dock().cloned() {
            dock.update(cx, |dock, cx| dock.set_open(true, window, cx));
        }
    }
}

impl Workspace {
    /// One edge-strip toggle button: a fixed icon (VSCode-activity-bar style —
    /// the glyph doesn't change, only its color/background do) that flips
    /// `placement` open/closed via `DockArea::toggle_dock`. Selected (dock
    /// open) shows a persistent highlight; hover shows a lighter one.
    fn dock_toggle_button(
        &self,
        id: &'static str,
        icon: IconName,
        placement: DockPlacement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.dock_area.read(cx).is_dock_open(placement, cx);
        let text_color = if selected {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        };

        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(40.0))
            .rounded_md()
            .text_color(text_color)
            .when(selected, |this| this.bg(cx.theme().accent))
            .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
            .child(Icon::new(icon).large())
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.dock_area.update(cx, |dock_area, cx| {
                    dock_area.toggle_dock(placement, window, cx);
                });
            }))
    }

    /// Far-left vertical icon strip: toggles the left "SFTP" dock (VSCode
    /// Explorer-style icon).
    fn render_left_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .w(px(44.0))
            .h_full()
            .py_2()
            .bg(cx.theme().secondary)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.dock_toggle_button("edge-sftp", IconName::File, DockPlacement::Left, cx))
    }

    /// Far-right vertical icon strip: toggles the right "已保存的连接" dock
    /// (VSCode Remote-Explorer-style icon).
    fn render_right_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .w(px(44.0))
            .h_full()
            .py_2()
            .bg(cx.theme().secondary)
            .border_l_1()
            .border_color(cx.theme().border)
            .child(self.dock_toggle_button(
                "edge-saved",
                IconName::Network,
                DockPlacement::Right,
                cx,
            ))
    }

    /// Bottom status bar — reserved for future info (connection state, cwd,
    /// …). Empty for now; the dock-toggle icons live in the edge strips.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .h(px(22.0))
            .bg(cx.theme().secondary)
            .border_t_1()
            .border_color(cx.theme().border)
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .size_full()
            .child(self.render_left_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_hidden()
                    .child(div().flex_1().overflow_hidden().child(self.dock_area.clone()))
                    .child(self.render_status_bar(cx)),
            )
            .child(self.render_right_bar(cx))
    }
}
