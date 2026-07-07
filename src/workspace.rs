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
    AnyView, App, AppContext, Bounds, Context, Entity, Focusable, IntoElement, ParentElement,
    Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, WeakEntity,
    Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::FluentBuilder, px, size,
};
use gpui_component::dock::{DockArea, DockPlacement};
use gpui_component::resizable::{ResizableState, resizable_panel, h_resizable};
use gpui_component::{ActiveTheme, Root};

use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::settings_window::SettingsWindow;
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::TerminalPanel;
use crate::settings;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::terminal::view::{FontConfig, TerminalView};

pub struct Workspace {
    /// Hosts the CENTER terminal tabs only (no side docks anymore).
    dock_area: Entity<DockArea>,
    /// Shared SSH connections, keyed by `user@host:port`.
    ssh_sessions: HashMap<String, Arc<SshSession>>,
    /// Every `TerminalView` this workspace has created, so settings changes
    /// (e.g. font) can be broadcast to already-open tabs. Dead weak refs are
    /// pruned lazily on the next broadcast rather than on tab close.
    terminal_views: Vec<WeakEntity<TerminalView>>,
    /// The open settings window, if any — re-triggering the menu item
    /// focuses this instead of opening a duplicate.
    settings_window: Option<WindowHandle<Root>>,

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

    // --- active slots (one per side) ----------------------------------------
    left_active: Option<PanelId>,
    right_active: Option<PanelId>,

    // --- horizontal body resize state ---------------------------------------
    body_resize: Entity<ResizableState>,

    /// Focused terminal's title, shown centered in the header.
    active_title: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Center-only dock: terminal tabs live here. No left/right docks.
        let dock_area = cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));

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

        // One stub panel per not-yet-implemented category.
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [
            PanelId::Network,
            PanelId::Security,
            PanelId::Sessions,
            PanelId::History,
            PanelId::Monitor,
        ] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid.label(), cx)).into();
            stub_panels.insert(pid, view);
        }

        let body_resize = cx.new(|_| ResizableState::default());

        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            settings_window: None,
            saved_panel: saved.into(),
            stub_panels,
            sftp_panels: HashMap::new(),
            sftp_placeholder,
            active_sftp: None,
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
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
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
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
    }

    /// Open an SSH shell terminal (reusing the host's shared connection) as a
    /// new central tab, and wire it so refocusing it later swaps its host's
    /// SFTP browser into the left region and updates the header title.
    pub fn open_ssh(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.ssh_session(&config) {
            let terminal = cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session));
            Self::seed_font_from_settings(&terminal, cx);
            let follow = config.clone();
            let handle = terminal.read(cx).focus_handle(cx);
            let term_weak = terminal.downgrade();
            self.terminal_views.push(term_weak.clone());
            let sub = cx.on_focus(&handle, window, move |this, window, cx| {
                this.set_active_title_from(&term_weak, cx);
                this.show_sftp(follow.clone(), window, cx);
            });
            self._subscriptions.push(sub);
            let panel = cx.new(|_cx| TerminalPanel::new(terminal));
            self.add_center(Arc::new(panel), window, cx);
            self.show_sftp(config, window, cx);
        }
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
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
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
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
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

    /// Resolve a font-family setting for applying to a `TerminalView`. Empty
    /// means "use the bundled default" (`FontConfig::default().family`), per
    /// what `settings_window.rs`'s "留空 = 内置默认字体" placeholder promises —
    /// this is deliberately NOT the same as `TerminalView::set_font_family`'s
    /// own empty-string meaning ("reset to system monospace"), which remains
    /// available as a separate, still-unused path for a possible future
    /// "reset to system font" control.
    fn resolved_font_family(raw: &str) -> String {
        if raw.is_empty() {
            FontConfig::default().family.to_string()
        } else {
            raw.to_string()
        }
    }

    /// Seed a newly-created terminal's font from persisted settings, so a new
    /// tab picks up whatever was last applied via Settings → Appearance
    /// instead of always starting at the compiled-in default.
    fn seed_font_from_settings(terminal: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let loaded = settings::load();
        let family = Self::resolved_font_family(&loaded.appearance.font_family);
        terminal.update(cx, |view, cx| {
            view.set_font_family(family, cx);
            view.set_font_size(px(loaded.appearance.font_size), cx);
        });
    }

    /// Broadcast a new font family/size to every currently-open terminal tab,
    /// pruning any that have since closed. Called by [`SettingsWindow`] on
    /// Apply/Confirm.
    pub fn apply_font_settings(
        &mut self,
        font_family: String,
        font_size: Pixels,
        cx: &mut Context<Self>,
    ) {
        let font_family = Self::resolved_font_family(&font_family);
        self.terminal_views.retain(|weak| {
            weak.update(cx, |view, cx| {
                view.set_font_family(font_family.clone(), cx);
                view.set_font_size(font_size, cx);
            })
            .is_ok()
        });
    }

    /// Update the header's active title from a (possibly-dropped) terminal.
    fn set_active_title_from(&mut self, term: &WeakEntity<TerminalView>, cx: &App) {
        if let Some(t) = term.upgrade() {
            self.active_title = t.read(cx).title().to_string().into();
        }
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
            let panel: AnyView = cx.new(|cx| SftpPanel::new(session, label, window, cx)).into();
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

    /// Bottom status bar — reserved for future info (connection state, cwd, …).
    /// Empty for now.
    fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        div()
            .w_full()
            .h(px(22.0))
            .bg(cx.theme().muted)
            .border_t_1()
            .border_color(cx.theme().border)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), cx);
        let left_bar = self.render_activity_bar(Side::Left, cx);
        let right_bar = self.render_activity_bar(Side::Right, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);

        div()
            .flex()
            .flex_col()
            .size_full()
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
            .child(status_bar)
            .children(gpui_component::Root::render_notification_layer(window, cx))
            .children(gpui_component::Root::render_dialog_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_font_family_empty_uses_bundled_default() {
        assert_eq!(Workspace::resolved_font_family(""), "JetBrains Mono");
    }

    #[test]
    fn resolved_font_family_passes_through_explicit_value() {
        assert_eq!(Workspace::resolved_font_family("Consolas"), "Consolas");
    }
}
