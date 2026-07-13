//! Top-level workspace: a nyaterm-style VSCode shell built on `gpui_component`.
//! An in-app Header sits on top; below it a row of
//! `[left activity bar] [left side region] [center DockArea] [right side region] [right activity bar]`
//! sits above a slim status bar.
//!
//! The `DockArea` is used for the CENTER only (terminal tabs, plus future
//! splits — it keeps its tab drag-docking). The left/right side regions are
//! single-panel containers whose content is chosen by the activity bars and
//! whose widths are controlled by an `h_resizable` group surrounding the body.
//!
//! One connection per host (CLAUDE.md §2): SSH sessions are cached by host key
//! in `ssh_sessions`, so a host's shell tab and SFTP browser share a single
//! russh connection instead of dialing twice.
//!
//! SFTP follows terminal focus: focusing an SSH terminal opens/updates the
//! left SFTP panel bound to that host; focusing a non-SSH terminal swaps the
//! SFTP slot back to the "no SFTP available" placeholder (see `show_sftp` /
//! `show_sftp_placeholder`). The saved-connections folder icon still works as a
//! manual way to browse a host too.
//!
//! Background drain: every `TerminalView` owns its feeder thread + drain task,
//! kept alive while the dock holds the panel entity; unbounded event channels
//! mean a backgrounded tab never back-pressures. Switching back shows the latest.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyView, App, AppContext, Axis, Bounds, Context, Entity, EntityId, Focusable, Font,
    FontFallbacks, InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, WindowBounds,
    WindowHandle, WindowOptions, div, font, prelude::FluentBuilder, px, size,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement};
use gpui_component::resizable::{ResizableState, resizable_panel, h_resizable};
use gpui_component::{ActiveTheme, Root};

use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::icons::{AppIcon, icon};
use crate::panels::quick_commands_panel::QuickCommandsPanel;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::settings_window::SettingsWindow;
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::monitor::{MonitorPanel, MonitorPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::{TerminalPanel, TerminalPanelEvent};
use crate::settings;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::terminal::view::{TerminalView, TerminalViewEvent};

pub struct Workspace {
    /// Hosts the CENTER terminal tabs only (no side docks anymore).
    dock_area: Entity<DockArea>,
    /// Shared SSH connections, keyed by `user@host:port`.
    ssh_sessions: HashMap<String, Arc<SshSession>>,
    /// The `SshConfig` behind each SSH-backed `TerminalView`, so a
    /// `TerminalViewEvent::ReconnectRequested` (user pressed Enter on a
    /// disconnected tab) knows which host to redial. Populated in
    /// `open_ssh`, pruned on tab close (`handle_ssh_tab_closed`) — also
    /// doubles as the "how many tabs does this host still have open"
    /// count that decides whether closing a tab should tear down the
    /// shared session.
    ssh_reconnect_configs: HashMap<EntityId, SshConfig>,
    /// Every `TerminalView` this workspace has created, so settings changes
    /// (e.g. font) can be broadcast to already-open tabs. Dead weak refs are
    /// pruned lazily on the next broadcast rather than on tab close.
    terminal_views: Vec<WeakEntity<TerminalView>>,
    /// The open settings window, if any — re-triggering the menu item
    /// focuses this instead of opening a duplicate.
    settings_window: Option<WindowHandle<Root>>,
    /// The most recently focused terminal, if any — used to send quick
    /// commands and to show the status bar's cursor-position readout. Set in
    /// `set_active_title_from`, which already runs on focus for every
    /// terminal. A dead weak ref (tab closed) reads as "nothing focused"
    /// until another tab is focused.
    focused_terminal: Option<WeakEntity<TerminalView>>,
    /// Whether the bottom quick-commands drawer is open. Closed by default.
    show_quick_commands: bool,
    /// The quick-commands panel shown in the drawer. Owned here (not part of
    /// the `PanelId` side-dock system — this is a new bottom region).
    quick_commands_panel: Entity<QuickCommandsPanel>,

    // --- panel registry -----------------------------------------------------
    /// The right-dock "已保存的连接" list (real panel).
    saved_panel: AnyView,
    /// Placeholder panels for the not-yet-implemented nyaterm categories.
    stub_panels: HashMap<PanelId, AnyView>,
    /// One SFTP browser per host key (created on first use, reused after).
    sftp_panels: HashMap<String, AnyView>,
    /// Shown in the SFTP slot when no SSH host is active.
    sftp_placeholder: AnyView,
    /// Host key whose SFTP browser the `PanelId::Sftp` slot resolves to.
    active_sftp: Option<String>,
    /// One 资源监控 panel per host key (created on first use, reused
    /// after) — mirrors `sftp_panels` field-for-field.
    monitor_panels: HashMap<String, AnyView>,
    /// Shown in the Monitor slot when no SSH host is active.
    monitor_placeholder: AnyView,
    /// Host key whose monitor panel the `PanelId::Monitor` slot resolves
    /// to. Unlike `active_sftp`, updating this does NOT force
    /// `right_active` to switch to `PanelId::Monitor` — the right dock's
    /// default occupant (`SavedConnections`) stays visible unless the
    /// user manually clicks the Monitor activity-bar icon; only *which
    /// host's data* is shown follows focus automatically.
    active_monitor: Option<String>,

    // --- active slots (one per side) ----------------------------------------
    left_active: Option<PanelId>,
    right_active: Option<PanelId>,

    // --- horizontal body resize state ---------------------------------------
    body_resize: Entity<ResizableState>,

    /// Focused terminal's title, shown centered in the header.
    active_title: SharedString,
    /// Forwards the currently-focused terminal's `cx.notify()` into a
    /// `Workspace`-level `cx.notify()`, so the status bar's cursor-position
    /// readout repaints on new terminal output/cursor movement, not just on
    /// focus changes. Replaced (dropping the old subscription) every time
    /// `set_active_title_from` runs, so only the current focus is observed.
    _focused_terminal_observation: Option<Subscription>,
    /// The application chrome's primary/fallback font, already resolved
    /// (never the raw `""` "系统默认" sentinel) — seeded from
    /// `AppSettings.appearance` in `Workspace::new`, re-resolved by
    /// `apply_appearance_font_settings` on Settings → Apply. Applied by
    /// `Render for Workspace` via an explicit `.font(...)` override on its
    /// own top-level element (see that impl's doc comment).
    appearance_font_family: SharedString,
    appearance_font_fallback: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Center-only dock: terminal tabs live here. No left/right docks.
        let dock_area = cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));

        // Seed the chrome font (see `apply_appearance_font_settings`'s doc
        // comment) once at startup — resolved eagerly here, not on every
        // render.
        let startup_appearance = settings::load().appearance;
        let appearance_font_family = Self::resolve_appearance_font(&startup_appearance.font_family);
        let appearance_font_fallback =
            Self::resolve_appearance_font(&startup_appearance.font_fallback);

        // The persisted "已保存的连接" list. Clicking a row opens the SSH terminal
        // or its SFTP browser.
        let cfg = config::load();
        let saved = cx.new(|cx| {
            SavedConnectionsPanel::new(cfg.connections, cfg.groups, window, cx)
        });
        let saved_sub =
            cx.subscribe_in(&saved, window, |this, _panel, event, window, cx| match event {
                SavedConnectionsEvent::Open(config) => this.open_ssh(config.clone(), window, cx),
                SavedConnectionsEvent::OpenSftp(config) => {
                    this.show_sftp(config.clone(), window, cx)
                }
                SavedConnectionsEvent::OpenLocal(shell, cwd) => {
                    this.open_local_with(shell.clone(), cwd.clone(), window, cx)
                }
                SavedConnectionsEvent::OpenTelnet(config) => {
                    this.open_telnet(config.clone(), window, cx)
                }
                SavedConnectionsEvent::OpenSerial(config) => {
                    this.open_serial(config.clone(), window, cx)
                }
            });

        let sftp_placeholder: AnyView = cx.new(|cx| SftpPlaceholder::new(cx)).into();
        let monitor_placeholder: AnyView = cx.new(MonitorPlaceholder::new).into();

        // One stub panel per not-yet-implemented category.
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [
            PanelId::Network,
            PanelId::Security,
            PanelId::Sessions,
            PanelId::History,
        ] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid.label(), cx)).into();
            stub_panels.insert(pid, view);
        }

        let body_resize = cx.new(|_| ResizableState::default());

        let workspace_handle = cx.entity().downgrade();
        let quick_commands_panel = cx.new(|cx| QuickCommandsPanel::new(workspace_handle, cx));

        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            ssh_reconnect_configs: HashMap::new(),
            terminal_views: Vec::new(),
            settings_window: None,
            focused_terminal: None,
            _focused_terminal_observation: None,
            appearance_font_family,
            appearance_font_fallback,
            show_quick_commands: false,
            quick_commands_panel,
            saved_panel: saved.into(),
            stub_panels,
            sftp_panels: HashMap::new(),
            sftp_placeholder,
            active_sftp: None,
            monitor_panels: HashMap::new(),
            monitor_placeholder,
            active_monitor: None,
            // Defaults: left panel closed until an SSH terminal is focused (see
            // `show_sftp`) or the user opens it manually; saved connections on the right.
            left_active: None,
            right_active: Some(PanelId::SavedConnections),
            body_resize,
            active_title: "Caracal".into(),
            _subscriptions: vec![saved_sub],
        }
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
    /// focusing it swaps the SFTP slot back to the placeholder (SFTP only makes
    /// sense over SSH) and updates the header title.
    pub fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new(window, cx));
        Self::seed_font_from_settings(&terminal, cx);
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }

    /// Open a local-shell terminal with custom shell and working directory.
    pub fn open_local_with(
        &mut self,
        shell: String,
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = if shell.is_empty() && cwd.is_empty() {
            cx.new(|cx| TerminalView::new(window, cx))
        } else {
            cx.new(|cx| {
                TerminalView::new_local_with(
                    window,
                    cx,
                    &shell,
                    if cwd.is_empty() { None } else { Some(&cwd) },
                )
            })
        };
        Self::seed_font_from_settings(&terminal, cx);
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }

    /// Open an SSH shell terminal (reusing the host's shared connection) as a
    /// new central tab, and wire it so refocusing it later swaps its host's
    /// SFTP browser into the left region and updates the header title.
    pub fn open_ssh(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        let key = config.key();
        let host_label = format!("{}@{}", config.user, config.host);

        let terminal = if let Some(session) = self.ssh_sessions.get(&key).cloned() {
            cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session, host_label.clone()))
        } else {
            cx.new(|cx| TerminalView::new_ssh_connecting(window, cx, host_label.clone()))
        };
        Self::seed_font_from_settings(&terminal, cx);
        let follow = config.clone();
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        // `term_weak` is needed again below (the background-connect
        // closure and, from Task 6, the close-cleanup subscription), so
        // the on-focus closure gets its own clone rather than moving the
        // outer binding.
        let sub = cx.on_focus(&handle, window, {
            let term_weak = term_weak.clone();
            move |this, window, cx| {
                this.set_active_title_from(&term_weak, cx);
                // Only show SFTP/monitor once the session is actually
                // cached — while a tab is still connecting, this closure
                // fires immediately (the tab is focused on creation) and
                // must not trigger `show_sftp`'s own on-demand
                // synchronous connect, which would race the background
                // dial below.
                if this.ssh_sessions.contains_key(&follow.key()) {
                    this.show_sftp(follow.clone(), window, cx);
                    this.show_monitor(follow.clone(), window, cx);
                } else {
                    this.show_sftp_placeholder(window, cx);
                    this.show_monitor_placeholder(window, cx);
                }
            }
        });
        self._subscriptions.push(sub);
        // Remember which host this tab is for, so a
        // `ReconnectRequested` (Enter pressed on the disconnected
        // banner — see `terminal/view.rs`) knows what to redial.
        self.ssh_reconnect_configs.insert(terminal.entity_id(), config.clone());
        let reconnect_sub =
            cx.subscribe_in(&terminal, window, |this, terminal, event, window, cx| {
                let TerminalViewEvent::ReconnectRequested = event;
                this.reconnect_ssh_terminal(terminal.clone(), window, cx);
            });
        self._subscriptions.push(reconnect_sub);

        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let closed_config = config.clone();
        let closed_term = term_weak.clone();
        let closed_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.handle_ssh_tab_closed(closed_config.clone(), &closed_term, window, cx);
        });
        self._subscriptions.push(closed_sub);
        self.add_center(Arc::new(panel), window, cx);

        if self.ssh_sessions.contains_key(&key) {
            self.show_sftp(config.clone(), window, cx);
            self.show_monitor(config, window, cx);
            return;
        }

        // Not cached: dial in the background so the tab above opens
        // instantly and a slow/unreachable host can't freeze the UI (same
        // rationale as `reconnect_ssh_terminal`, below).
        let dial_config = config.clone();
        let connect_task = cx.background_spawn(async move { SshSession::connect(dial_config) });
        let term_for_connect = term_weak.clone();
        cx.spawn_in(window, async move |this, cx| {
            match connect_task.await {
                Ok(session) => {
                    let _ = this.update_in(cx, move |this, window, cx| {
                        // The tab may have closed while this dial was in
                        // flight (`handle_ssh_tab_closed` removes its
                        // `ssh_reconnect_configs` entry on close) — don't
                        // resurrect a session (or touch side panels) for a
                        // host with no open tabs; that would leak a live
                        // `SshSession` that nothing will ever evict.
                        if !this.ssh_reconnect_configs.contains_key(&term_for_connect.entity_id()) {
                            return;
                        }
                        this.ssh_sessions.insert(key, session.clone());
                        if let Some(t) = term_for_connect.upgrade() {
                            t.update(cx, |view, cx| {
                                let session = session.clone();
                                view.reconnect_with(
                                    move |cols, rows, bytes_tx| session.open_shell(cols, rows, bytes_tx),
                                    cx,
                                );
                            });
                        }
                        // Don't yank the side panels to this host if the
                        // user has since focused a different tab — the
                        // on-focus handler above will show them correctly
                        // if the user comes back to this tab later.
                        let is_focused = this
                            .focused_terminal
                            .as_ref()
                            .map(|w| w.entity_id())
                            == Some(term_for_connect.entity_id());
                        if is_focused {
                            this.show_sftp(config.clone(), window, cx);
                            this.show_monitor(config, window, cx);
                        }
                    });
                }
                Err(e) => {
                    log::error!("SSH connect to {key} failed: {e}");
                    let _ = term_for_connect.update(cx, |view, cx| {
                        view.mark_connect_failed(format!("连接失败: {e}"), cx);
                    });
                }
            }
        })
        .detach();
    }

    /// Redial a disconnected SSH terminal's host and swap in a fresh shell
    /// channel. Triggered by `TerminalViewEvent::ReconnectRequested` (the
    /// user pressed Enter on the "connection lost" banner).
    ///
    /// Evicts the cached `SshSession` for this host first: the disconnect
    /// that got us here may have only been noticed on this one shell
    /// channel, but if the underlying TCP connection actually died, the
    /// cached session is a zombie whose `command_loop` thread is still
    /// alive but will hang opening any new channel on it (the root cause of
    /// "can't open new SSH connections after a disconnect" — see the
    /// investigation this feature came out of). Redialing fresh here also
    /// means the connect itself runs on the background executor, not the
    /// main thread, so a slow/unreachable host can't freeze the UI.
    ///
    /// Known limitation: other tabs or the SFTP/monitor panels sharing the
    /// now-evicted session are *not* migrated to the new one — they may
    /// still hang on their next use and need to be closed/reopened by hand.
    fn reconnect_ssh_terminal(
        &mut self,
        terminal: Entity<TerminalView>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.ssh_reconnect_configs.get(&terminal.entity_id()).cloned() else {
            return;
        };
        self.ssh_sessions.remove(&config.key());
        terminal.update(cx, |view, cx| view.mark_connecting(cx));

        let dial_config = config.clone();
        let connect_task = cx.background_spawn(async move { SshSession::connect(dial_config) });
        cx.spawn(async move |this, cx| {
            match connect_task.await {
                Ok(session) => {
                    let key = config.key();
                    let entity_id = terminal.entity_id();
                    let still_open = this
                        .update(cx, |this, _cx| {
                            // The tab may have closed while this redial was
                            // in flight (`handle_ssh_tab_closed` removes its
                            // `ssh_reconnect_configs` entry on close) — don't
                            // resurrect a session for a host with no open
                            // tabs; that would leak a live `SshSession` that
                            // nothing will ever evict.
                            let still_open = this.ssh_reconnect_configs.contains_key(&entity_id);
                            if still_open {
                                this.ssh_sessions.insert(key, session.clone());
                            }
                            still_open
                        })
                        .unwrap_or(false);
                    if still_open {
                        let _ = terminal.update(cx, |view, cx| {
                            let session = session.clone();
                            view.reconnect_with(
                                move |cols, rows, bytes_tx| session.open_shell(cols, rows, bytes_tx),
                                cx,
                            );
                        });
                    }
                }
                Err(e) => {
                    log::error!("SSH reconnect to {} failed: {e}", config.key());
                    let _ = terminal.update(cx, |view, cx| {
                        view.mark_connect_failed(format!("连接失败: {e}"), cx);
                    });
                }
            }
        })
        .detach();
    }

    /// Cleanup when an SSH-backed terminal tab is removed from the dock
    /// (`TerminalPanelEvent::Closed`, emitted by `TerminalPanel::on_removed`).
    /// If any other tab for the same host is still open, does nothing —
    /// the shared session is still needed. Otherwise evicts the cached
    /// `SshSession` and closes that host's SFTP/monitor panels (falling
    /// back to their placeholders if either was the one currently shown).
    fn handle_ssh_tab_closed(
        &mut self,
        config: SshConfig,
        terminal: &WeakEntity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ssh_reconnect_configs.remove(&terminal.entity_id());
        let key = config.key();
        let any_tabs_left = self.ssh_reconnect_configs.values().any(|c| c.key() == key);
        if any_tabs_left {
            return;
        }
        self.ssh_sessions.remove(&key);
        self.sftp_panels.remove(&key);
        self.monitor_panels.remove(&key);
        if self.active_sftp.as_deref() == Some(key.as_str()) {
            self.show_sftp_placeholder(window, cx);
        }
        if self.active_monitor.as_deref() == Some(key.as_str()) {
            self.show_monitor_placeholder(window, cx);
        }
        cx.notify();
    }

    /// Open a raw Telnet terminal as a new central tab. No shared-connection
    /// cache (unlike SSH): telnet has no SFTP-style second channel to
    /// justify one, so each tab dials its own socket, same as a local shell.
    pub fn open_telnet(&mut self, config: TelnetConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_telnet(window, cx, config));
        Self::seed_font_from_settings(&terminal, cx);
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }

    /// Open a serial-port terminal as a new central tab. Same
    /// no-shared-cache rationale as `open_telnet`.
    pub fn open_serial(&mut self, config: SerialConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_serial(window, cx, config));
        Self::seed_font_from_settings(&terminal, cx);
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }

    /// Open the settings window, or focus it if one is already open.
    pub fn open_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = &self.settings_window {
            if handle
                .update(cx, |_root, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
            // Handle is stale (window was closed) — fall through and open a
            // fresh one, replacing it below.
        }

        let workspace = cx.entity().downgrade();
        let bounds = Bounds::centered(None, size(px(640.0), px(480.0)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            move |window, cx| {
                let settings_window =
                    cx.new(|cx| SettingsWindow::new(workspace.clone(), window, cx));
                cx.new(|cx| Root::new(settings_window, window, cx).bg(cx.theme().background))
            },
        );
        match result {
            Ok(handle) => self.settings_window = Some(handle),
            Err(e) => log::error!("failed to open settings window: {e}"),
        }
    }

    /// The system's default UI-chrome font family, used when Appearance's
    /// primary/fallback font is left at "系统默认" (empty string). Mirrors
    /// `terminal::view`'s `system_monospace_family()` but resolves a UI font,
    /// not monospace — chrome text (menus, buttons, panels) reading in a
    /// monospace font looks unusually rigid.
    fn system_ui_font_family() -> SharedString {
        #[cfg(target_os = "linux")]
        {
            if let Ok(out) = std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "sans-serif"])
                .output()
                && out.status.success()
            {
                let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !name.is_empty() {
                    return name.into();
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            return "Segoe UI".into();
        }
        // macOS (and any detection failure above): gpui-component's own
        // default sentinel, which already resolves correctly there — only
        // Windows/Linux needed an override (see `main.rs`'s existing
        // `Theme::global_mut(cx).font_family` comment).
        #[allow(unreachable_code)]
        ".SystemUIFont".into()
    }

    /// Resolve an Appearance font-setting value: `""` (系统默认) becomes a
    /// detected system UI font; anything else passes through unchanged.
    /// Called only at startup (`Workspace::new`) and on Settings →
    /// Apply/Confirm (`apply_appearance_font_settings`) — never from
    /// `Render::render`, since this can spawn a subprocess on Linux.
    fn resolve_appearance_font(raw: &str) -> SharedString {
        if raw.is_empty() {
            Self::system_ui_font_family()
        } else {
            raw.into()
        }
    }

    /// Resolve and store a new Appearance primary/fallback font, then
    /// `cx.notify()` so `Render for Workspace`'s own `.font(...)` override
    /// (see that impl's doc comment) picks it up immediately — no restart
    /// required. Called by `SettingsWindow` on Apply/Confirm, alongside
    /// `apply_font_settings` (terminal) and `Theme::change` (theme mode).
    pub fn apply_appearance_font_settings(
        &mut self,
        font_family: String,
        font_fallback: String,
        cx: &mut Context<Self>,
    ) {
        self.appearance_font_family = Self::resolve_appearance_font(&font_family);
        self.appearance_font_fallback = Self::resolve_appearance_font(&font_fallback);
        cx.notify();
    }

    /// Seed a newly-created terminal's font from persisted settings, so a new
    /// tab picks up whatever was last applied via Settings → Terminal instead
    /// of always starting at the compiled-in default.
    fn seed_font_from_settings(terminal: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let loaded = settings::load();
        terminal.update(cx, |view, cx| {
            view.set_font_family(loaded.terminal.font_family.clone(), cx);
            view.set_font_size(px(loaded.terminal.font_size), cx);
            view.set_font_fallbacks(
                vec![
                    loaded.terminal.font_fallback1.clone().into(),
                    loaded.terminal.font_fallback2.clone().into(),
                ],
                cx,
            );
        });
    }

    /// Broadcast a new font family/size/fallback chain to every currently-open
    /// terminal tab, pruning any that have since closed. Called by
    /// [`SettingsWindow`] on Apply/Confirm.
    pub fn apply_font_settings(
        &mut self,
        font_family: String,
        font_size: Pixels,
        font_fallback1: String,
        font_fallback2: String,
        cx: &mut Context<Self>,
    ) {
        self.terminal_views.retain(|weak| {
            weak.update(cx, |view, cx| {
                view.set_font_family(font_family.clone(), cx);
                view.set_font_size(font_size, cx);
                view.set_font_fallbacks(
                    vec![font_fallback1.clone().into(), font_fallback2.clone().into()],
                    cx,
                );
            })
            .is_ok()
        });
    }

    /// Update the header's active title and the focused-terminal pointer
    /// (used by quick commands and the status bar's cursor-position display)
    /// from a (possibly-dropped) terminal. Also (re)subscribes to the
    /// terminal's own repaint notifications so the status bar's
    /// cursor-position readout stays live instead of freezing at whatever it
    /// was when focus last changed — see `_focused_terminal_observation`'s
    /// doc comment.
    fn set_active_title_from(&mut self, term: &WeakEntity<TerminalView>, cx: &mut Context<Self>) {
        self.focused_terminal = Some(term.clone());
        if let Some(t) = term.upgrade() {
            self.active_title = t.read(cx).title().to_string().into();
            self._focused_terminal_observation =
                Some(cx.observe(&t, |_this, _terminal, cx| {
                    cx.notify();
                }));
        }
    }

    /// Send `text` to the currently-focused terminal tab, if any, per
    /// `execute`. No-op if no terminal is focused or its weak ref has died.
    /// Called by [`crate::panels::quick_commands_panel::QuickCommandsPanel`].
    pub fn send_to_focused_terminal(&self, text: &str, execute: bool, cx: &App) {
        let Some(terminal) = self.focused_terminal.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        terminal.read(cx).send_text(text, execute);
    }

    /// Best-effort: ask the currently-focused terminal what directory it's
    /// in by injecting `pwd` and reading back the line the shell echoes.
    /// No OSC7/shell-integration exists in this terminal emulator, so this
    /// is a fixed-delay guess, not a reliable signal — see
    /// `docs/superpowers/specs/2026-07-08-file-explorer-gaps-round-b-design.md`.
    /// Resolves to `None` if there's no focused terminal, or if the line
    /// read back doesn't look like an absolute path.
    pub fn guess_focused_terminal_cwd(&self, cx: &mut Context<Self>) -> Task<Option<String>> {
        let Some(terminal) = self.focused_terminal.as_ref().and_then(|w| w.upgrade()) else {
            return Task::ready(None);
        };
        let start_row = terminal.read(cx).cursor_position().0;
        terminal.read(cx).send_text("pwd", true);
        cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            let line = terminal.read_with(cx, |term, _cx| term.line_text(start_row + 1));
            let trimmed = line.trim().to_string();
            if trimmed.starts_with('/') {
                Some(trimmed)
            } else {
                None
            }
        })
    }

    /// Bind the SFTP slot to `config`'s host (reusing the shared connection,
    /// creating the browser once) and force the left slot to show it.
    fn show_sftp(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        let key = config.key();
        if !self.sftp_panels.contains_key(&key) {
            let Some(session) = self.ssh_session(&config) else {
                return;
            };
            let label = format!("{}@{}", config.user, config.host);
            let workspace = cx.entity().downgrade();
            let panel: AnyView =
                cx.new(|cx| SftpPanel::new(session, label, workspace, window, cx)).into();
            self.sftp_panels.insert(key.clone(), panel);
        }
        self.active_sftp = Some(key);
        self.left_active = Some(PanelId::Sftp);
        cx.notify();
    }

    /// Detach the SFTP slot from any host so it resolves to the "no SFTP
    /// available" placeholder. Does NOT force the left slot open.
    fn show_sftp_placeholder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.active_sftp = None;
        cx.notify();
    }

    /// Bind the Monitor slot to `config`'s host (reusing the shared
    /// connection, creating the panel once). Unlike `show_sftp`, does NOT
    /// force `right_active` — see the `active_monitor` field's doc comment.
    /// Takes `_window` (unused) only so its signature matches `show_sftp`'s
    /// at every call site — both are invoked from the same `cx.on_focus`
    /// closures, which hand both functions the same `window` binding.
    fn show_monitor(&mut self, config: SshConfig, _window: &mut Window, cx: &mut Context<Self>) {
        let key = config.key();
        if !self.monitor_panels.contains_key(&key) {
            let Some(session) = self.ssh_session(&config) else {
                return;
            };
            let label = format!("{}@{}", config.user, config.host);
            let workspace = cx.entity().downgrade();
            let panel: AnyView =
                cx.new(|cx| MonitorPanel::new(session, label, workspace, cx)).into();
            self.monitor_panels.insert(key.clone(), panel);
        }
        self.active_monitor = Some(key);
        cx.notify();
    }

    /// Detach the Monitor slot from any host so it resolves to the "no
    /// host" placeholder. Mirrors `show_sftp_placeholder`.
    fn show_monitor_placeholder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.active_monitor = None;
        cx.notify();
    }

    fn add_center(
        &mut self,
        panel: Arc<dyn gpui_component::dock::PanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_area.update(cx, |dock_area, cx| {
            // When the Center dock's tab group empties out (every terminal
            // tab closed), its `TabPanel` detaches itself from the live
            // `StackPanel` (`TabPanel::remove_self_if_empty` in
            // gpui-component), but nothing prunes the matching entry from
            // `DockArea.center`'s separate `DockItem::Split.items` tree —
            // that tree is only ever appended to, never pruned on removal.
            // Left alone, `add_panel` below would find that stale, now
            // invisible `Tabs` entry and silently repopulate it instead of
            // creating a new, visible tab group: the "double-click a saved
            // connection does nothing after closing every tab" bug. Rebuild
            // a fresh, empty center whenever we detect this.
            if Self::center_tab_group_is_stale(dock_area, cx) {
                let weak = cx.entity().downgrade();
                let fresh_center = DockItem::split(Axis::Horizontal, vec![], &weak, window, cx);
                dock_area.set_center(fresh_center, window, cx);
            }
            dock_area.add_panel(panel, DockPlacement::Center, None, window, cx);
        });
    }

    /// True if the Center dock's tab group exists but is empty — see
    /// `add_center`'s doc comment for why this needs special handling.
    fn center_tab_group_is_stale(dock_area: &DockArea, cx: &App) -> bool {
        match dock_area.center() {
            DockItem::Split { items, .. } => items.iter().any(|item| {
                matches!(item, DockItem::Tabs { view, .. } if view.read(cx).active_panel(cx).is_none())
            }),
            DockItem::Tabs { view, .. } => view.read(cx).active_panel(cx).is_none(),
            _ => false,
        }
    }

    // --- panel registry / slots ---------------------------------------------

    /// Resolve a [`PanelId`] to its live view (SFTP falls back to the
    /// placeholder when no host is active).
    fn resolve(&self, id: PanelId) -> Option<AnyView> {
        match id {
            PanelId::Sftp => Some(
                self.active_sftp
                    .as_ref()
                    .and_then(|k| self.sftp_panels.get(k).cloned())
                    .unwrap_or_else(|| self.sftp_placeholder.clone()),
            ),
            PanelId::Monitor => Some(
                self.active_monitor
                    .as_ref()
                    .and_then(|k| self.monitor_panels.get(k).cloned())
                    .unwrap_or_else(|| self.monitor_placeholder.clone()),
            ),
            PanelId::SavedConnections => Some(self.saved_panel.clone()),
            other => self.stub_panels.get(&other).cloned(),
        }
    }

    /// Toggle `id` in its side's single slot: open it if it isn't the active
    /// panel, otherwise close the slot.
    fn toggle_panel(&mut self, id: PanelId, _window: &mut Window, cx: &mut Context<Self>) {
        let slot = match id.side() {
            Side::Left => &mut self.left_active,
            Side::Right => &mut self.right_active,
        };
        if *slot == Some(id) {
            *slot = None;
        } else {
            *slot = Some(id);
        }
        cx.notify();
    }
}

impl Workspace {
    /// One edge-strip activity bar (single column, full-width).
    fn render_activity_bar(&self, side: Side, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let border = cx.theme().border;
        let bg = cx.theme().muted;
        let active_id = match side {
            Side::Left => self.left_active,
            Side::Right => self.right_active,
        };

        let mut col = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .pt_1()
            .w_full();
        for &pid in side_items(side) {
            let active = active_id == Some(pid);
            col = col.child(
                activity_button(pid, active, side, cx)
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        this.toggle_panel(pid, window, cx)
                    })),
            );
        }

        div()
            .flex()
            .flex_col()
            .items_center()
            .w(px(44.0))
            .h_full()
            .bg(bg)
            .when(matches!(side, Side::Left), |d| {
                d.border_r_1().border_color(border)
            })
            .when(matches!(side, Side::Right), |d| {
                d.border_l_1().border_color(border)
            })
            .child(col)
    }

    /// The body row between the two activity bars: an `h_resizable` group
    /// containing three panels (left region | center | right region).
    ///
    /// The left/right panels are ALWAYS present as group children — toggling
    /// `left_active`/`right_active` only flips `.visible(..)` — instead of
    /// conditionally adding/removing them. `ResizableState` indexes its
    /// per-panel sizes positionally, and `sync_panels_count` only ever
    /// extends/truncates the *end* of that list; if the left panel's
    /// `resizable_panel()` came and went, a later re-add would land at index
    /// 0 but inherit whatever size used to live at that index (the center
    /// dock's, typically most of the window width) — hence a freshly-opened
    /// SFTP panel rendering full-width. Keeping a stable 3-panel identity
    /// (the pattern gpui-component's own resizable story uses for a
    /// collapsible sidebar) avoids the index shift entirely.
    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let border = cx.theme().border;
        let left_view = self.left_active.and_then(|id| self.resolve(id));
        let right_view = self.right_active.and_then(|id| self.resolve(id));

        let group = h_resizable("body-split")
            .with_state(&self.body_resize)
            .child(
                resizable_panel()
                    .visible(left_view.is_some())
                    // Wide enough that the SFTP file list's 名称/修改时间/大小
                    // columns are visible without resizing on first open.
                    .size(px(200.0))
                    .size_range(px(180.0)..px(560.0))
                    .child(
                        left_view
                            .map(|view| side_region_content(view, border, true))
                            .unwrap_or_else(|| div().into_any_element()),
                    ),
            )
            .child(
                resizable_panel().child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .child(self.dock_area.clone()),
                ),
            )
            .child(
                resizable_panel()
                    .visible(right_view.is_some())
                    .size(px(240.0))
                    .size_range(px(180.0)..px(560.0))
                    .child(
                        right_view
                            .map(|view| side_region_content(view, border, false))
                            .unwrap_or_else(|| div().into_any_element()),
                    ),
            );

        // Wrap in a flex_1 container so the group fills the remaining row width
        // alongside the two fixed 44px activity bars.
        div().flex_1().min_w(px(0.0)).child(group)
    }

    /// Bottom status bar: a left icon cluster (currently just the
    /// quick-commands toggle; structured so a second icon can be added later
    /// without redesign) and a right-aligned cursor-position readout for the
    /// focused terminal (blank when nothing is focused).
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cursor_text = self
            .focused_terminal
            .as_ref()
            .and_then(|w| w.upgrade())
            .map(|t| {
                let (row, col) = t.read(cx).cursor_position();
                format!("{}:{}", row + 1, col + 1)
            })
            .unwrap_or_default();

        div()
            .w_full()
            .h(px(22.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .bg(cx.theme().muted)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("status-quick-commands")
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_color(if self.show_quick_commands {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground
                    })
                    .hover(|s| s.text_color(cx.theme().foreground))
                    .child(icon(AppIcon::QuickCmd))
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.show_quick_commands = !this.show_quick_commands;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(cursor_text),
            )
    }
}

impl Render for Workspace {
    /// `gpui-component`'s `Root` (which wraps this `Workspace`, see
    /// `main.rs`) renders its own top-level `.font_family(cx.theme()
    /// .font_family.clone())` — a single family, no fallback field exists on
    /// `Theme` at all (`gpui-component/crates/ui/src/theme/mod.rs`). Rather
    /// than patch that vendored crate, this `div`'s own more specific
    /// `.font(...)` below overrides it for all of `Workspace`'s content
    /// (effectively the whole app UI) via GPUI's normal style cascade — a
    /// descendant's explicit font setting wins over an ancestor's.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), cx);
        let left_bar = self.render_activity_bar(Side::Left, cx);
        let right_bar = self.render_activity_bar(Side::Right, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
        let border = cx.theme().border;
        let show_quick_commands = self.show_quick_commands;
        let quick_commands_panel = self.quick_commands_panel.clone();

        let mut chrome_font: Font = font(self.appearance_font_family.clone());
        chrome_font.fallbacks = Some(FontFallbacks::from_fonts(vec![
            self.appearance_font_fallback.to_string(),
        ]));

        div()
            .flex()
            .flex_col()
            .size_full()
            .font(chrome_font)
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(left_bar)
                    .child(body)
                    .child(right_bar),
            )
            .when(show_quick_commands, |d| {
                d.child(
                    div()
                        .w_full()
                        .h(px(220.0))
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(border)
                        .child(quick_commands_panel),
                )
            })
            .child(status_bar)
            .children(gpui_component::Root::render_notification_layer(window, cx))
            .children(gpui_component::Root::render_dialog_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_appearance_font_passes_through_explicit_value() {
        assert_eq!(Workspace::resolve_appearance_font("Consolas"), "Consolas".to_string());
    }
}
