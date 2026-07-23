//! Top-level workspace: a nyaterm-style VSCode shell built on `gpui_component`.
//! An in-app Header sits on top; below it a row of
//! `[left activity bar] [left side region] [center DockArea] [right side region] [right activity bar]`
//! sits above a slim status bar.
//!
//! The `DockArea` is used for the CENTER only (terminal tabs, plus future
//! splits — it keeps its tab drag-docking). The left/right side regions are
//! single-panel containers whose content is chosen by the activity bars and
//! whose widths are controlled by hand-rolled drag handles in `render_body`
//! (not a `gpui_component` `h_resizable` group — see `left_width`'s doc
//! comment for why).
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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyView, AnyWindowHandle, App, AppContext, Axis, Bounds, Context, Entity, EntityId, Focusable,
    Font, FontFallbacks, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, WindowBounds,
    WindowHandle, WindowOptions, div, font, prelude::FluentBuilder, px, size,
};
use gpui_component::checkbox::Checkbox;
use gpui_component::dock::{DockArea, DockItem, DockPlacement, PanelStyle};
use gpui_component::input::{Input, InputState};
use gpui_component::notification::NotificationType;
use gpui_component::{ActiveTheme, Root, WindowExt, v_flex};

use crate::config;
use crate::panels::activity_bar::{
    PanelId, Side, activity_button, quick_commands_button, settings_button, side_items,
};
use crate::panels::header::render_header;
use crate::panels::quick_commands_panel::QuickCommandsPanel;
use crate::panels::sessions::{SessionsEvent, SessionsPanel};
use crate::panels::settings_window::SettingsWindow;
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::monitor::{MonitorPanel, MonitorPlaceholder};
use crate::panels::security_auth::SecurityAuthPanel;
use crate::panels::stub::StubPanel;
use crate::panels::terminal::{TerminalPanel, TerminalPanelEvent};
use crate::settings;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::keyring_store::{self, SecretStore};
use crate::terminal::view::{TerminalView, TerminalViewEvent};
use crate::vault;

gpui::actions!(caracal_workspace, [
    NewTab,
    NextTab,
    PrevTab,
    GotoTab1,
    GotoTab2,
    GotoTab3,
    GotoTab4,
    GotoTab5,
    GotoTab6,
    GotoTab7,
    GotoTab8,
    GotoTab9,
    NewConnectionAction,
    ToggleLeftSidebar,
    ToggleRightSidebar,
    ToggleQuickCommands,
    OpenSettingsAction,
    ZoomIn,
    ZoomOut,
]);

/// The unlocked vault's master key, available anywhere via
/// `cx.global::<VaultKey>()` / `cx.try_global::<VaultKey>()` once unlock
/// succeeds — set once at startup (see `Workspace::new`), never re-locked
/// during the session. Mirrors the `Theme::global_mut(cx)` idiom
/// gpui-component itself uses for app-wide state.
pub struct VaultKey(pub crate::crypto::MasterKey);
impl gpui::Global for VaultKey {}

/// First launch after upgrade (or a fresh install): no `[vault]` section
/// yet in `connections.toml`. Blocks the app behind a "set a master
/// password" dialog — mandatory, not opt-in (see the design spec's
/// "Adoption" decision) — then migrates existing plaintext secrets and
/// unlocks. A mismatched/empty password keeps the dialog open with an
/// inline error instead of closing it.
fn show_setup_master_password_dialog(window: &mut Window, cx: &mut Context<Workspace>) {
    let pw_input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(rust_i18n::t!("Vault.password_placeholder"))
    });
    let confirm_input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(rust_i18n::t!("Vault.confirm_password_placeholder"))
    });
    let error: Entity<SharedString> = cx.new(|_cx| SharedString::default());

    window.open_alert_dialog(cx, move |alert, _window, cx| {
        let pw_input = pw_input.clone();
        let confirm_input = confirm_input.clone();
        let error = error.clone();
        let body = v_flex()
            .gap_2()
            .child(div().child(rust_i18n::t!("Vault.setup_body")))
            .child(Input::new(&pw_input))
            .child(Input::new(&confirm_input))
            .when(!error.read(cx).is_empty(), |el| {
                el.child(div().text_color(gpui::red()).child(error.read(cx).clone()))
            });
        alert
            .title(rust_i18n::t!("Vault.setup_title"))
            .description(body)
            .confirm()
            .on_ok(move |_, window, cx| {
                let password = pw_input.read(cx).value().to_string();
                let confirm = confirm_input.read(cx).value().to_string();
                if password.is_empty() {
                    error.update(cx, |e, cx| {
                        *e = rust_i18n::t!("Vault.error_empty").into();
                        cx.notify();
                    });
                    return false;
                }
                if password != confirm {
                    error.update(cx, |e, cx| {
                        *e = rust_i18n::t!("Vault.error_mismatch").into();
                        cx.notify();
                    });
                    return false;
                }
                let mut cfg = config::load();
                match vault::migrate(&mut cfg, &password) {
                    Ok(master) => {
                        vault::migrate_saved_passwords_to_direct(&mut cfg, &master);
                        if let Err(e) = config::save(&cfg) {
                            log::error!("failed to save migrated vault: {e}");
                        }
                        cx.set_global(VaultKey(master));
                        window.close_dialog(cx);
                        true
                    }
                    Err(e) => {
                        log::error!("vault migration failed: {e}");
                        error.update(cx, |msg, cx| {
                            *msg = e.to_string().into();
                            cx.notify();
                        });
                        false
                    }
                }
            })
    });
}

/// Every normal startup after the vault has been set up: blocks the app
/// behind a password prompt until it unlocks. A wrong password keeps the
/// dialog open with an inline error instead of closing it — there is no
/// lockout/rate-limiting (local-only attack surface; Argon2id already
/// makes brute force expensive).
fn show_unlock_dialog(window: &mut Window, cx: &mut Context<Workspace>) {
    let pw_input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(rust_i18n::t!("Vault.password_placeholder"))
    });
    let error: Entity<SharedString> = cx.new(|_cx| SharedString::default());
    // Unchecked by default — the opt-in convenience unlock is a deliberate
    // trade-off (see the design spec), never assumed.
    let remember: Entity<bool> = cx.new(|_cx| false);

    window.open_alert_dialog(cx, move |alert, _window, cx| {
        let pw_input = pw_input.clone();
        let error = error.clone();
        let remember = remember.clone();
        let remember_for_checkbox = remember.clone();
        let body = v_flex()
            .gap_2()
            .child(Input::new(&pw_input))
            .child(
                Checkbox::new("vault-remember-on-this-device")
                    .checked(*remember.read(cx))
                    .label(SharedString::from(rust_i18n::t!("Vault.remember_on_this_device").to_string()))
                    .on_click(move |checked, _window, cx| {
                        remember_for_checkbox.update(cx, |r, cx| {
                            *r = *checked;
                            cx.notify();
                        });
                    }),
            )
            .when(!error.read(cx).is_empty(), |el| {
                el.child(div().text_color(gpui::red()).child(error.read(cx).clone()))
            });
        alert
            .title(rust_i18n::t!("Vault.unlock_title"))
            .description(body)
            .confirm()
            .on_ok(move |_, window, cx| {
                let password = pw_input.read(cx).value().to_string();
                let mut cfg = config::load();
                match vault::unlock(&cfg, &password) {
                    Ok(master) => {
                        if vault::migrate_saved_passwords_to_direct(&mut cfg, &master) {
                            if let Err(e) = config::save(&cfg) {
                                log::error!("failed to save migrated config: {e}");
                            }
                        }
                        if *remember.read(cx) {
                            if let Some(vault_meta) = &cfg.vault {
                                keyring_store::OsSecretStore.set(&vault_meta.vault_id, &master.0);
                            }
                        }
                        cx.set_global(VaultKey(master));
                        window.close_dialog(cx);
                        true
                    }
                    Err(_) => {
                        error.update(cx, |msg, cx| {
                            *msg = rust_i18n::t!("Vault.error_wrong_password").into();
                            cx.notify();
                        });
                        false
                    }
                }
            })
    });
}

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
    /// Tab numbers currently in use per SSH connection (`SshConfig::key()`),
    /// so `open_ssh` can render tab titles as `"{name}:{n}"` — the Nth
    /// *currently open* tab for that host, reusing the lowest number freed
    /// by a closed tab rather than growing forever. Populated in `open_ssh`,
    /// released in `handle_ssh_tab_closed`.
    ssh_tab_numbers: HashMap<String, HashSet<u32>>,
    /// Every currently open `TerminalPanel`, in open order — the source
    /// of truth for the workspace-wide `"N-"` tab-title sequence number,
    /// recomputed by live position (not a sticky per-tab id) every time a
    /// tab opens or closes via `register_tab_panel`/`unregister_tab_panel`.
    /// Only goes stale after a manual drag-reorder — see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md.
    tab_panels: Vec<Entity<TerminalPanel>>,
    /// Every `TerminalView` this workspace has created, so settings changes
    /// (e.g. font) can be broadcast to already-open tabs. Dead weak refs are
    /// pruned lazily on the next broadcast rather than on tab close.
    terminal_views: Vec<WeakEntity<TerminalView>>,
    /// Number of tabs currently open in the center dock's (single) tab
    /// group. Tracked here rather than read from gpui-component's own
    /// `DockArea.center()` tree, whose cached panel list is stale after any
    /// tab close (see `center_tab_group_is_stale`'s doc comment) — and
    /// `TabPanel` has no public panel-count getter either. Incremented in
    /// `add_center` (the single entry point every `open_*` method uses),
    /// decremented wherever a `TerminalPanelEvent::Closed` is observed
    /// below.
    tab_count: usize,
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
    /// The right-dock "会话" list (real panel).
    sessions_panel: AnyView,
    /// Typed handle to the same entity as `sessions_panel`, kept alongside
    /// it so vault-related code can read its `ssh_keys()` — `AnyView` is
    /// type-erased and can't be read back.
    saved_sessions: Entity<SessionsPanel>,
    /// The left-bar 安全认证 panel (manages saved SSH keys/passwords).
    security_auth_panel: AnyView,
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
    /// default occupant (`Sessions`) stays visible unless the
    /// user manually clicks the Monitor activity-bar icon; only *which
    /// host's data* is shown follows focus automatically.
    active_monitor: Option<String>,

    // --- active slots (one per side) ----------------------------------------
    left_active: Option<PanelId>,
    right_active: Option<PanelId>,
    /// Which panel to reopen when `secondary-b` toggles the left sidebar
    /// back on after it was closed — closing it (mouse click on the active
    /// icon, or the keyboard toggle) sets `left_active` to `None`, which
    /// loses track of what was showing, so this remembers it separately.
    /// Defaults to `Sftp` (`side_items(Side::Left)`'s first entry).
    left_last_panel: PanelId,
    /// Mirrors `left_last_panel` for the right sidebar. Defaults to
    /// `Sessions`, matching today's actual startup default
    /// (`right_active: Some(PanelId::Sessions)` below).
    right_last_panel: PanelId,

    // --- horizontal body resize state ---------------------------------------
    /// Width of the left side region. A plain field, not a `gpui_component`
    /// `ResizableState` — that state type keeps stale `bounds`/`size` for
    /// whichever body-split panel is currently hidden via `.visible(false)`
    /// (see `render_body`'s doc comment), which corrupted the *other*
    /// panels' resize math (wrong min-width clamp + flicker while dragging).
    /// Hand-rolled the same way the quick-commands drawer's height already
    /// is, via raw mouse events (`start_left_resize`/`on_left_drag_move`/
    /// `stop_left_resize`, and the `_right_` equivalents below).
    left_width: Pixels,
    /// `Some((mouse_x_at_drag_start, width_at_drag_start))` while the
    /// left-region resize handle is being dragged; `None` otherwise.
    left_drag: Option<(Pixels, Pixels)>,
    /// Width of the right side region — mirrors `left_width` field-for-field.
    right_width: Pixels,
    right_drag: Option<(Pixels, Pixels)>,
    /// Current height of the quick-commands drawer, dragged via the handle
    /// at its top edge — the original hand-rolled-resize precedent that
    /// `left_width`/`right_width` above now follow too.
    quick_commands_height: Pixels,
    /// `Some((mouse_y_at_drag_start, height_at_drag_start))` while the quick-
    /// commands resize handle is being dragged; `None` otherwise.
    quick_commands_drag: Option<(Pixels, Pixels)>,

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
    /// This window's own handle, captured at construction via
    /// `window.window_handle()` — used by `open_security_auth_panel` to
    /// bring this (main) window to the foreground when triggered from a
    /// separate standalone window (`NewConnectionWindow`'s empty saved-key
    /// picker), so the panel switch is actually visible rather than
    /// happening silently behind whichever window currently has focus.
    own_window: AnyWindowHandle,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Center-only dock: terminal tabs live here. No left/right docks.
        // `PanelStyle::Auto` (the default) collapses to a plain, unstyled
        // title bar when there's exactly one tab — no background/border, so
        // it looks inconsistent with the bordered tab chips shown once a
        // second tab opens. `TabBar` always renders the same styled strip,
        // even with a single tab.
        let dock_area = cx.new(|cx| {
            DockArea::new("caracal-main", Some(1), window, cx).panel_style(PanelStyle::TabBar)
        });

        // Seed the chrome font (see `apply_appearance_font_settings`'s doc
        // comment) once at startup — resolved eagerly here, not on every
        // render.
        let startup_appearance = settings::load().appearance;
        let appearance_font_family = Self::resolve_appearance_font(&startup_appearance.font_family);
        let appearance_font_fallback =
            Self::resolve_appearance_font(&startup_appearance.font_fallback);

        // The persisted "会话" list. Clicking a row opens the SSH terminal
        // or its SFTP browser.
        let cfg = config::load();
        let needs_vault_setup = cfg.vault.is_none();
        let vault_id = cfg.vault.as_ref().map(|v| v.vault_id.clone());
        // Dialogs require the window's root view to already be a
        // `gpui_component::Root` (`Root::update` panics otherwise) — that
        // only becomes true once `main.rs`'s `cx.open_window` closure
        // returns and wraps this `Workspace` in `Root::new`, which hasn't
        // happened yet at this point inside `Workspace::new` itself.
        // `defer_in` runs after the current construction/effect cycle
        // finishes, by which point the window root is set.
        cx.defer_in(window, move |_this, window, cx| {
            if needs_vault_setup {
                show_setup_master_password_dialog(window, cx);
                return;
            }
            // Try the OS-keyring convenience-unlock cache first — a hit
            // skips the password prompt entirely. Any failure (never
            // opted in, keyring unavailable, entry cleared) is treated as
            // a cache miss, never as blocking normal password unlock.
            if let Some(vault_id) = &vault_id {
                if let Some(key_bytes) = keyring_store::OsSecretStore.get(vault_id) {
                    let master = crate::crypto::MasterKey(zeroize::Zeroizing::new(key_bytes));
                    let changed = _this
                        .saved_sessions
                        .update(cx, |panel, _cx| panel.migrate_saved_passwords_to_direct(&master));
                    if changed {
                        _this.saved_sessions.read(cx).persist_for_security_auth();
                    }
                    cx.set_global(VaultKey(master));
                    return;
                }
            }
            show_unlock_dialog(window, cx);
        });
        let workspace_handle = cx.entity().downgrade();
        let saved = cx.new(|cx| {
            SessionsPanel::new(
                cfg.connections,
                cfg.groups,
                cfg.vault,
                cfg.ssh_keys,
                cfg.saved_passwords,
                workspace_handle.clone(),
                window,
                cx,
            )
        });
        let saved_sub =
            cx.subscribe_in(&saved, window, |this, _panel, event, window, cx| match event {
                SessionsEvent::Open(conn, name) => {
                    let Some(vault) = cx.try_global::<VaultKey>() else {
                        window.push_notification(
                            (NotificationType::Error, rust_i18n::t!("Vault.locked_error")),
                            cx,
                        );
                        return;
                    };
                    let ssh_keys = this.ssh_keys_snapshot(cx);
                    let saved_passwords = this.saved_passwords_snapshot(cx);
                    match conn.to_ssh_config(&ssh_keys, &saved_passwords, &vault.0) {
                        Ok(ssh_config) => this.open_ssh(ssh_config, name.clone(), window, cx),
                        Err(e) => {
                            window.push_notification(
                                (
                                    NotificationType::Error,
                                    rust_i18n::t!("Vault.decrypt_failed_error", error = e.to_string()),
                                ),
                                cx,
                            );
                        }
                    }
                }
                SessionsEvent::OpenSftp(config) => {
                    this.show_sftp(config.clone(), window, cx)
                }
                SessionsEvent::OpenLocal(shell, cwd, name) => {
                    this.open_local_with(shell.clone(), cwd.clone(), name.clone(), window, cx)
                }
                SessionsEvent::OpenTelnet(config) => {
                    this.open_telnet(config.clone(), window, cx)
                }
                SessionsEvent::OpenSerial(config) => {
                    this.open_serial(config.clone(), window, cx)
                }
            });

        let sftp_placeholder: AnyView = cx.new(|cx| SftpPlaceholder::new(cx)).into();
        let monitor_placeholder: AnyView = cx.new(MonitorPlaceholder::new).into();

        // One stub panel per not-yet-implemented category. Security has a
        // real panel now (see `resolve`'s dedicated match arm below).
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [PanelId::Network, PanelId::History] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid, cx)).into();
            stub_panels.insert(pid, view);
        }
        let security_auth_panel: AnyView =
            cx.new(|cx| SecurityAuthPanel::new(saved.downgrade(), cx)).into();

        let quick_commands_panel = cx.new(|cx| QuickCommandsPanel::new(workspace_handle, cx));

        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            ssh_reconnect_configs: HashMap::new(),
            ssh_tab_numbers: HashMap::new(),
            tab_panels: Vec::new(),
            terminal_views: Vec::new(),
            tab_count: 0,
            settings_window: None,
            focused_terminal: None,
            _focused_terminal_observation: None,
            appearance_font_family,
            appearance_font_fallback,
            show_quick_commands: false,
            quick_commands_panel,
            saved_sessions: saved.clone(),
            security_auth_panel,
            sessions_panel: saved.into(),
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
            right_active: Some(PanelId::Sessions),
            left_last_panel: PanelId::Sftp,
            right_last_panel: PanelId::Sessions,
            left_width: px(200.0),
            left_drag: None,
            right_width: px(220.0),
            right_drag: None,
            quick_commands_height: px(220.0),
            quick_commands_drag: None,
            active_title: "Caracal".into(),
            _subscriptions: vec![saved_sub],
            own_window: window.window_handle(),
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

    /// Open a local-shell terminal with custom shell and working directory.
    /// `title` is the saved connection's display name (see
    /// `SessionsEvent::OpenLocal`'s doc comment).
    pub fn open_local_with(
        &mut self,
        shell: String,
        cwd: String,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = if shell.is_empty() && cwd.is_empty() {
            cx.new(|cx| TerminalView::new(window, cx, title))
        } else {
            cx.new(|cx| {
                TerminalView::new_local_with(
                    window,
                    cx,
                    &shell,
                    if cwd.is_empty() { None } else { Some(&cwd) },
                    title,
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
        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        let workspace_handle = cx.entity().downgrade();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0, workspace_handle));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, panel, event, _window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(tab_count_sub);
        self.register_tab_panel(panel.clone(), cx);
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }

    /// The lowest positive integer not already in `used` — the numbering
    /// scheme for concurrently-open SSH tabs sharing one connection
    /// ("name:1", "name:2", ...), reusing whatever a closed tab freed rather
    /// than growing forever. Pure/standalone so it's unit-testable without a
    /// live `Workspace` (which needs a `Window` to construct).
    fn lowest_free_number(used: &HashSet<u32>) -> u32 {
        let mut n = 1;
        while used.contains(&n) {
            n += 1;
        }
        n
    }

    /// The tab index one to the right of `current`, wrapping to `0` past the
    /// last tab. Returns `0` when `len == 0` (nothing to wrap over) — a safe
    /// no-op for the "no tabs open" case, same convention as
    /// `prev_tab_index`/`goto_tab_index` below.
    fn next_tab_index(current: usize, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        (current + 1) % len
    }

    /// The tab index one to the left of `current`, wrapping to the last tab
    /// index past the first. Mirrors `next_tab_index`.
    fn prev_tab_index(current: usize, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        if current == 0 { len - 1 } else { current - 1 }
    }

    /// Converts a 1-indexed `secondary-N` target (as pressed by the user)
    /// into a 0-indexed tab index, or `None` if `target_one_indexed` is `0`
    /// or beyond the current tab count.
    fn goto_tab_index(target_one_indexed: usize, len: usize) -> Option<usize> {
        if target_one_indexed == 0 || target_one_indexed > len {
            return None;
        }
        Some(target_one_indexed - 1)
    }

    /// Allocate the lowest unused positive tab number for the SSH connection
    /// keyed by `key`, marking it used. `release_ssh_tab_number` frees it
    /// again when that tab closes.
    fn allocate_ssh_tab_number(&mut self, key: &str) -> u32 {
        let used = self.ssh_tab_numbers.entry(key.to_string()).or_default();
        let n = Self::lowest_free_number(used);
        used.insert(n);
        n
    }

    /// Release a tab number previously returned by `allocate_ssh_tab_number`,
    /// so the next tab opened for this connection can reuse it.
    fn release_ssh_tab_number(&mut self, key: &str, number: u32) {
        if let Some(used) = self.ssh_tab_numbers.get_mut(key) {
            used.remove(&number);
            if used.is_empty() {
                self.ssh_tab_numbers.remove(key);
            }
        }
    }

    /// Append `panel` to the end of the open-tab list and recompute every
    /// open tab's displayed sequence number from its new position.
    fn register_tab_panel(&mut self, panel: Entity<TerminalPanel>, cx: &mut Context<Self>) {
        self.tab_panels.push(panel);
        self.renumber_tabs(cx);
    }

    /// Remove `panel` from the open-tab list (by entity identity) and
    /// recompute every remaining tab's displayed sequence number.
    fn unregister_tab_panel(&mut self, panel: &Entity<TerminalPanel>, cx: &mut Context<Self>) {
        self.tab_panels.retain(|p| p.entity_id() != panel.entity_id());
        self.renumber_tabs(cx);
    }

    /// Set every open tab's displayed `"N-"` prefix to its 1-indexed
    /// position in `tab_panels`.
    fn renumber_tabs(&mut self, cx: &mut Context<Self>) {
        for (i, panel) in self.tab_panels.iter().enumerate() {
            panel.update(cx, |panel, cx| panel.set_tab_number((i + 1) as u32, cx));
        }
    }

    /// Move `panel` to `new_ix` in the open-tab list (removing it from
    /// wherever it currently sits first) and recompute every open tab's
    /// displayed sequence number. Called by `TerminalPanel::on_added_to`
    /// every time gpui-component (re)attaches a panel — including a manual
    /// drag-reorder, whose new position this reads via the dropped panel's
    /// own `TabPanel::active_ix()` (always left pointing at it — see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    pub(crate) fn reposition_tab_panel(&mut self, panel: Entity<TerminalPanel>, new_ix: usize, cx: &mut Context<Self>) {
        self.tab_panels.retain(|p| p.entity_id() != panel.entity_id());
        let ix = new_ix.min(self.tab_panels.len());
        self.tab_panels.insert(ix, panel);
        self.renumber_tabs(cx);
    }

    /// Snapshot of the vault's shared SSH keys, for decrypting a
    /// connection's key-file auth at connect time (see `SessionsEvent::Open`).
    fn ssh_keys_snapshot(&self, cx: &App) -> Vec<crate::config::SshKeyEntry> {
        self.saved_sessions.read(cx).ssh_keys().to_vec()
    }

    /// Snapshot of the vault's shared saved passwords, for decrypting a
    /// connection's Saved-mode password auth at connect time.
    fn saved_passwords_snapshot(&self, cx: &App) -> Vec<crate::config::SavedPasswordEntry> {
        self.saved_sessions.read(cx).saved_passwords().to_vec()
    }

    /// Open an SSH shell terminal (reusing the host's shared connection) as a
    /// new central tab, and wire it so refocusing it later swaps its host's
    /// SFTP browser into the left region and updates the header title.
    /// `display_name` is the saved connection's own name (or `user@host` if
    /// unset, see `SavedConnection::display_name`) — the tab title becomes
    /// `"{display_name}:{n}"`, `n` deduplicating concurrently-open tabs for
    /// the same host.
    pub fn open_ssh(
        &mut self,
        config: SshConfig,
        display_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = config.key();
        let host_label = format!("{}@{}", config.user, config.host);
        let tab_number = self.allocate_ssh_tab_number(&key);
        let title = format!("{display_name}:{tab_number}");

        let terminal = if let Some(session) = self.ssh_sessions.get(&key).cloned() {
            cx.new(|cx| {
                TerminalView::new_ssh_shell(window, cx, session, host_label.clone(), title.clone())
            })
        } else {
            cx.new(|cx| {
                TerminalView::new_ssh_connecting(window, cx, host_label.clone(), title.clone())
            })
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

        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        // (Unrelated to `tab_number` below, the per-host SSH dedup number
        // used in this method's `"{display_name}:{n}"` title suffix.)
        let workspace_handle = cx.entity().downgrade();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0, workspace_handle));
        let closed_config = config.clone();
        let closed_term = term_weak.clone();
        let closed_key = key.clone();
        let closed_sub = cx.subscribe_in(&panel, window, move |this, panel, event, window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.handle_ssh_tab_closed(closed_config.clone(), &closed_term, window, cx);
            this.release_ssh_tab_number(&closed_key, tab_number);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(closed_sub);
        self.register_tab_panel(panel.clone(), cx);
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
                        view.mark_connect_failed(
                            rust_i18n::t!("Workspace.connect_failed", error = e).to_string(),
                            cx,
                        );
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
                        view.mark_connect_failed(
                            rust_i18n::t!("Workspace.connect_failed", error = e).to_string(),
                            cx,
                        );
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
        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        let workspace_handle = cx.entity().downgrade();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0, workspace_handle));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, panel, event, _window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(tab_count_sub);
        self.register_tab_panel(panel.clone(), cx);
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
        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        let workspace_handle = cx.entity().downgrade();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0, workspace_handle));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, panel, event, _window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(tab_count_sub);
        self.register_tab_panel(panel.clone(), cx);
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
                // No `.bg(cx.theme().background)` here — see `main.rs`'s Root
                // construction for why that freezes the background at the
                // theme active when this window opens.
                cx.new(|cx| Root::new(settings_window, window, cx))
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

    /// Bounds match `settings_window.rs`'s `parse_font_size` validation
    /// range (6..=96px) — zooming can't push the terminal font outside what
    /// Settings would already reject if typed by hand.
    const MIN_FONT_SIZE: f32 = 6.0;
    const MAX_FONT_SIZE: f32 = 96.0;
    const ZOOM_STEP: f32 = 1.0;

    fn clamped_font_size(current: f32, delta: f32) -> f32 {
        (current + delta).clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE)
    }

    /// Adjust the persisted terminal font size by `delta`px (clamped) and
    /// broadcast it to every open tab — the same effect as changing the
    /// font-size field in Settings and clicking Apply, just without opening
    /// the dialog. No-ops (does not touch disk) if the delta is fully
    /// absorbed by the clamp, i.e. already at the min/max.
    fn zoom_font(&mut self, delta: f32, cx: &mut Context<Self>) {
        let mut loaded = settings::load();
        let new_size = Self::clamped_font_size(loaded.terminal.font_size, delta);
        if new_size == loaded.terminal.font_size {
            return;
        }
        loaded.terminal.font_size = new_size;
        if let Err(e) = settings::save(&loaded) {
            log::error!("failed to save zoomed font size: {e}");
            return;
        }
        self.apply_font_settings(
            loaded.terminal.font_family.clone(),
            px(new_size),
            loaded.terminal.font_fallback1.clone(),
            loaded.terminal.font_fallback2.clone(),
            cx,
        );
    }

    fn on_zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_font(Self::ZOOM_STEP, cx);
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_font(-Self::ZOOM_STEP, cx);
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
        self.left_last_panel = PanelId::Sftp;
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
        self.tab_count += 1;
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

    /// Find the (currently only ever one) `Tabs` group inside the center
    /// dock's tree — a `Split` wrapping a single `Tabs` item today (see
    /// `add_center`'s doc comment); this walk also handles a future nested
    /// `Split` without changes.
    fn active_tabs_item(&self, cx: &App) -> Option<DockItem> {
        fn find(item: &DockItem) -> Option<DockItem> {
            match item {
                DockItem::Tabs { .. } => Some(item.clone()),
                DockItem::Split { items, .. } => items.iter().find_map(find),
                _ => None,
            }
        }
        find(self.dock_area.read(cx).center())
    }

    /// Switch the center dock's active tab to `ix` (0-indexed). Uses
    /// `DockItem::active_index`, gpui-component's own public method for
    /// mutating the live `TabPanel` entity's active index — but that method
    /// doesn't call `cx.notify()` itself (confirmed by reading its body), so
    /// this does that explicitly afterward, or the tab switch wouldn't
    /// repaint until something unrelated triggered a redraw.
    ///
    /// Also moves keyboard focus into the newly active panel. Mouse-click
    /// tab switching goes through `TabPanel`'s own private `set_active_ix`,
    /// which additionally calls a private `focus_active_panel` helper
    /// (confirmed by reading `tab_panel.rs`) — `DockItem::active_index`
    /// bypasses that entirely since it only touches the `active_ix` field
    /// directly, so a keyboard-driven switch left focus on whatever had it
    /// before (typically the now-hidden previous tab), stranding keystrokes
    /// there. Reproduces the same effect via `TabPanel`'s public
    /// `active_panel`/`PanelView::focus_handle` instead of needing the
    /// private method itself.
    fn set_active_tab_index(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tabs_item) = self.active_tabs_item(cx) else {
            return;
        };
        let DockItem::Tabs { ref view, .. } = tabs_item else {
            return;
        };
        let view = view.clone();
        tabs_item.active_index(ix, cx);
        view.update(cx, |_, cx| cx.notify());
        if let Some(panel) = view.read(cx).active_panel(cx) {
            panel.focus_handle(cx).focus(window, cx);
        }
    }

    /// `secondary-shift-t`: duplicate the focused tab's connection — a new
    /// shell on the same host if it's SSH, otherwise (or with nothing
    /// focused) a new local shell, same fallback `open_local_with` already
    /// uses elsewhere.
    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let fallback_title = rust_i18n::t!("NewConnectionWindow.type_local").to_string();
        let Some(terminal) = self.focused_terminal.as_ref().and_then(|w| w.upgrade()) else {
            self.open_local_with(String::new(), String::new(), fallback_title, window, cx);
            return;
        };
        let Some(config) = self.ssh_reconnect_configs.get(&terminal.entity_id()).cloned() else {
            self.open_local_with(String::new(), String::new(), fallback_title, window, cx);
            return;
        };
        // SSH tab titles are always "{display_name}:{n}" (see `open_ssh`) —
        // recover the original display name by dropping the numeric suffix.
        let title = terminal.read(cx).title().to_string();
        let display_name = title.rsplit_once(':').map_or(title.clone(), |(name, _)| name.to_string());
        self.open_ssh(config, display_name, window, cx);
    }

    /// `secondary-shift-n`: open the "New Connection" window (focuses it
    /// instead of opening a duplicate if one is already open — see
    /// `SessionsPanel::open_new_connection_window`).
    fn on_new_connection(&mut self, _: &NewConnectionAction, window: &mut Window, cx: &mut Context<Self>) {
        self.saved_sessions.update(cx, |panel, cx| {
            panel.open_new_connection_window(None, None, window, cx);
        });
    }

    fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tabs_item) = self.active_tabs_item(cx) else {
            return;
        };
        let DockItem::Tabs { ref view, .. } = tabs_item else {
            return;
        };
        let current = view.read(cx).active_ix();
        let new_ix = Self::next_tab_index(current, self.tab_count);
        self.set_active_tab_index(new_ix, window, cx);
    }

    fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tabs_item) = self.active_tabs_item(cx) else {
            return;
        };
        let DockItem::Tabs { ref view, .. } = tabs_item else {
            return;
        };
        let current = view.read(cx).active_ix();
        let new_ix = Self::prev_tab_index(current, self.tab_count);
        self.set_active_tab_index(new_ix, window, cx);
    }

    /// Shared by the nine `on_goto_tab_N` handlers below.
    fn goto_tab(&mut self, one_indexed: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = Self::goto_tab_index(one_indexed, self.tab_count) else {
            return;
        };
        self.set_active_tab_index(ix, window, cx);
    }

    fn on_goto_tab_1(&mut self, _: &GotoTab1, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(1, window, cx);
    }
    fn on_goto_tab_2(&mut self, _: &GotoTab2, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(2, window, cx);
    }
    fn on_goto_tab_3(&mut self, _: &GotoTab3, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(3, window, cx);
    }
    fn on_goto_tab_4(&mut self, _: &GotoTab4, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(4, window, cx);
    }
    fn on_goto_tab_5(&mut self, _: &GotoTab5, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(5, window, cx);
    }
    fn on_goto_tab_6(&mut self, _: &GotoTab6, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(6, window, cx);
    }
    fn on_goto_tab_7(&mut self, _: &GotoTab7, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(7, window, cx);
    }
    fn on_goto_tab_8(&mut self, _: &GotoTab8, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(8, window, cx);
    }
    fn on_goto_tab_9(&mut self, _: &GotoTab9, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(9, window, cx);
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
            PanelId::Sessions => Some(self.sessions_panel.clone()),
            PanelId::Security => Some(self.security_auth_panel.clone()),
            other => self.stub_panels.get(&other).cloned(),
        }
    }

    /// Toggle `id` in its side's single slot: open it if it isn't the active
    /// panel, otherwise close the slot. Also updates that side's
    /// remembered panel (`left_last_panel`/`right_last_panel`) whenever the
    /// slot opens, so the keyboard sidebar toggle (`toggle_side`, below)
    /// reopens the same panel a mouse click last chose.
    fn toggle_panel(&mut self, id: PanelId, _window: &mut Window, cx: &mut Context<Self>) {
        let is_active = match id.side() {
            Side::Left => self.left_active == Some(id),
            Side::Right => self.right_active == Some(id),
        };
        match id.side() {
            Side::Left => self.left_active = if is_active { None } else { Some(id) },
            Side::Right => self.right_active = if is_active { None } else { Some(id) },
        }
        if !is_active {
            match id.side() {
                Side::Left => self.left_last_panel = id,
                Side::Right => self.right_last_panel = id,
            }
        }
        cx.notify();
    }

    /// Forces the left panel to Security & Auth — unlike `toggle_panel`,
    /// never closes it if already active. Called from `SessionsPanel`
    /// (itself called from `NewConnectionWindow`, a separate standalone
    /// window with no other path back to this panel's slots). Also
    /// activates this (main) window — without it, the switch happens
    /// invisibly behind whichever window currently has focus.
    pub(crate) fn open_security_auth_panel(&mut self, cx: &mut Context<Self>) {
        self.left_active = Some(PanelId::Security);
        self.left_last_panel = PanelId::Security;
        cx.notify();
        let _ = self.own_window.update(cx, |_view, window, _cx| window.activate_window());
    }

    /// Shared by `on_toggle_left_sidebar`/`on_toggle_right_sidebar`: flips
    /// the side's slot between `None` and its remembered panel.
    fn toggle_side(&mut self, side: Side, cx: &mut Context<Self>) {
        let (currently_open, remembered) = match side {
            Side::Left => (self.left_active.is_some(), self.left_last_panel),
            Side::Right => (self.right_active.is_some(), self.right_last_panel),
        };
        let new_value = if currently_open { None } else { Some(remembered) };
        match side {
            Side::Left => self.left_active = new_value,
            Side::Right => self.right_active = new_value,
        }
        cx.notify();
    }

    fn on_toggle_left_sidebar(
        &mut self,
        _: &ToggleLeftSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_side(Side::Left, cx);
    }

    fn on_toggle_right_sidebar(
        &mut self,
        _: &ToggleRightSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_side(Side::Right, cx);
    }

    fn on_toggle_quick_commands(
        &mut self,
        _: &ToggleQuickCommands,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_quick_commands = !self.show_quick_commands;
        cx.notify();
    }

    fn on_open_settings_action(
        &mut self,
        _: &OpenSettingsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(window, cx);
    }

    /// Mouse-down on the quick-commands drawer's resize handle: record where
    /// the drag started (mouse Y + current height) so
    /// `on_quick_commands_drag_move` can compute the new height from the
    /// delta, rather than needing this element's own bounds.
    fn start_quick_commands_resize(
        &mut self,
        ev: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quick_commands_drag = Some((ev.position.y, self.quick_commands_height));
        cx.notify();
    }

    /// Dragging the handle up shrinks the gap between mouse and the drawer's
    /// top edge, which should *grow* the drawer — hence `start_y - mouse_y`.
    /// No-ops when not currently dragging (called unconditionally from the
    /// outer `Render for Workspace`'s `on_mouse_move`).
    fn on_quick_commands_drag_move(
        &mut self,
        ev: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start_y, start_height)) = self.quick_commands_drag else {
            return;
        };
        let delta = start_y - ev.position.y;
        let new_height = (start_height + delta).clamp(px(120.0), px(500.0));
        if new_height != self.quick_commands_height {
            self.quick_commands_height = new_height;
            cx.notify();
        }
    }

    fn stop_quick_commands_resize(
        &mut self,
        _ev: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quick_commands_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Mouse-down on the left region's resize handle: record where the drag
    /// started (mouse X + current width) so `on_left_drag_move` can compute
    /// the new width from the delta.
    fn start_left_resize(&mut self, ev: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.left_drag = Some((ev.position.x, self.left_width));
        cx.notify();
    }

    /// No-ops when not currently dragging (called unconditionally from the
    /// outer `Render for Workspace`'s `on_mouse_move`, same pattern as
    /// `on_quick_commands_drag_move`).
    fn on_left_drag_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.left_drag else {
            return;
        };
        let delta = ev.position.x - start_x;
        let new_width = (start_width + delta).clamp(px(180.0), px(560.0));
        if new_width != self.left_width {
            self.left_width = new_width;
            cx.notify();
        }
    }

    fn stop_left_resize(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.left_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Mirror of `start_left_resize` for the right region.
    fn start_right_resize(&mut self, ev: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.right_drag = Some((ev.position.x, self.right_width));
        cx.notify();
    }

    /// Dragging this handle left (toward the center) should *grow* the
    /// right region, hence `start_x - mouse_x` — the mirror image of
    /// `on_left_drag_move`'s `mouse_x - start_x`.
    fn on_right_drag_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.right_drag else {
            return;
        };
        let delta = start_x - ev.position.x;
        let new_width = (start_width + delta).clamp(px(200.0), px(500.0));
        if new_width != self.right_width {
            self.right_width = new_width;
            cx.notify();
        }
    }

    fn stop_right_resize(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.right_drag.take().is_some() {
            cx.notify();
        }
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
            .w_full()
            .flex_1();
        for &pid in side_items(side) {
            let active = active_id == Some(pid);
            col = col.child(
                activity_button(pid, active, side, cx)
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        this.toggle_panel(pid, window, cx)
                    })),
            );
        }

        // Quick-commands toggle: pinned to the bottom of the right bar (a
        // spacer above pushes it down), separate from the PanelId-backed
        // buttons above since it toggles a bottom drawer, not a side slot.
        if matches!(side, Side::Right) {
            col = col.child(div().flex_1()).child(
                quick_commands_button(self.show_quick_commands, cx).on_click(cx.listener(
                    |this, _ev, _window, cx| {
                        this.show_quick_commands = !this.show_quick_commands;
                        cx.notify();
                    },
                )),
            );
        }

        // Settings shortcut: pinned to the bottom of the left bar (mirrors
        // quick-commands' placement on the right) — opens the standalone
        // Settings window, not a side-panel slot.
        if matches!(side, Side::Left) {
            col = col.child(div().flex_1()).child(
                settings_button(cx)
                    .on_click(cx.listener(|this, _ev, window, cx| this.open_settings(window, cx))),
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

    /// The body row between the two activity bars: left region | center dock
    /// | right region, with a hand-rolled drag handle between each side
    /// region and the center (see `left_width`'s doc comment for why this
    /// isn't a `gpui_component` `h_resizable` group).
    ///
    /// Side widths are absolute pixels that do **not** rescale when the
    /// window itself resizes — VSCode-style: a sidebar keeps whatever width
    /// the user last dragged it to, and the center dock absorbs the change
    /// via its own `flex_1`.
    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let border = cx.theme().border;
        let left_view = self.left_active.and_then(|id| self.resolve(id));
        let right_view = self.right_active.and_then(|id| self.resolve(id));
        let show_left = left_view.is_some();
        let show_right = right_view.is_some();

        let left_region = if let Some(view) = left_view {
            div()
                .h_full()
                .flex_shrink_0()
                .w(self.left_width)
                .child(side_region_content(view, border, true))
                .into_any_element()
        } else {
            div().h_full().flex_shrink_0().w(px(0.0)).into_any_element()
        };

        let left_handle = if show_left {
            div()
                .id("body-left-resize-handle")
                .h_full()
                .w(px(4.0))
                .flex_shrink_0()
                .cursor(gpui::CursorStyle::ResizeColumn)
                .bg(border)
                .hover(|s| s.bg(cx.theme().primary))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::start_left_resize))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let right_handle = if show_right {
            div()
                .id("body-right-resize-handle")
                .h_full()
                .w(px(4.0))
                .flex_shrink_0()
                .cursor(gpui::CursorStyle::ResizeColumn)
                .bg(border)
                .hover(|s| s.bg(cx.theme().primary))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::start_right_resize))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let right_region = if let Some(view) = right_view {
            div()
                .h_full()
                .flex_shrink_0()
                .w(self.right_width)
                .child(side_region_content(view, border, false))
                .into_any_element()
        } else {
            div().h_full().flex_shrink_0().w(px(0.0)).into_any_element()
        };

        let center = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(self.dock_area.clone()),
            )
            .when(self.show_quick_commands, |d| {
                d.child(
                    div()
                        .w_full()
                        .h(self.quick_commands_height)
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .child(
                            // Drag handle: a thin strip at the drawer's top
                            // edge. Mouse-move/up are handled on the outer
                            // `Render for Workspace` div (`on_quick_commands_
                            // drag_move`/`stop_quick_commands_resize`) so the
                            // drag keeps tracking even if the cursor strays
                            // off this thin strip mid-drag.
                            div()
                                .id("quick-commands-resize-handle")
                                .w_full()
                                .h(px(4.0))
                                .flex_shrink_0()
                                .cursor(gpui::CursorStyle::ResizeRow)
                                .bg(border)
                                .hover(|s| s.bg(cx.theme().primary))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::start_quick_commands_resize),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.0))
                                .overflow_hidden()
                                .child(self.quick_commands_panel.clone()),
                        ),
                )
            });

        // Wrap in a flex_1 container so the row fills the remaining row
        // width alongside the two fixed 44px activity bars.
        div().flex_1().min_w(px(0.0)).child(
            div()
                .flex()
                .flex_row()
                .size_full()
                .child(left_region)
                .child(left_handle)
                .child(center)
                .child(right_handle)
                .child(right_region),
        )
    }

    /// Bottom status bar: a right-aligned cursor-position readout for the
    /// focused terminal (blank when nothing is focused). The quick-commands
    /// toggle lives in the right activity bar (`quick_commands_button`), not
    /// here.
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
            .justify_end()
            .px_2()
            .bg(cx.theme().muted)
            .border_t_1()
            .border_color(cx.theme().border)
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
        let header = render_header(self.active_title.clone(), window, cx);
        let left_bar = self.render_activity_bar(Side::Left, cx);
        let right_bar = self.render_activity_bar(Side::Right, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);

        let mut chrome_font: Font = font(self.appearance_font_family.clone());
        chrome_font.fallbacks = Some(FontFallbacks::from_fonts(vec![
            self.appearance_font_fallback.to_string(),
        ]));

        div()
            .flex()
            .flex_col()
            .size_full()
            .font(chrome_font)
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_goto_tab_1))
            .on_action(cx.listener(Self::on_goto_tab_2))
            .on_action(cx.listener(Self::on_goto_tab_3))
            .on_action(cx.listener(Self::on_goto_tab_4))
            .on_action(cx.listener(Self::on_goto_tab_5))
            .on_action(cx.listener(Self::on_goto_tab_6))
            .on_action(cx.listener(Self::on_goto_tab_7))
            .on_action(cx.listener(Self::on_goto_tab_8))
            .on_action(cx.listener(Self::on_goto_tab_9))
            .on_action(cx.listener(Self::on_new_connection))
            .on_action(cx.listener(Self::on_toggle_left_sidebar))
            .on_action(cx.listener(Self::on_toggle_right_sidebar))
            .on_action(cx.listener(Self::on_toggle_quick_commands))
            .on_action(cx.listener(Self::on_open_settings_action))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            // Quick-commands drawer resize: mouse-down starts on the handle
            // itself (`start_quick_commands_resize`), but move/up are
            // handled here, at the top level, so the drag keeps tracking
            // even if the cursor strays off the thin 4px handle strip.
            .on_mouse_move(cx.listener(Self::on_quick_commands_drag_move))
            .on_mouse_move(cx.listener(Self::on_left_drag_move))
            .on_mouse_move(cx.listener(Self::on_right_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_quick_commands_resize))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_left_resize))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_right_resize))
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
    fn resolve_appearance_font_passes_through_explicit_value() {
        assert_eq!(Workspace::resolve_appearance_font("Consolas"), "Consolas".to_string());
    }

    #[test]
    fn clamped_font_size_applies_the_step() {
        assert_eq!(Workspace::clamped_font_size(14.0, 1.0), 15.0);
        assert_eq!(Workspace::clamped_font_size(14.0, -1.0), 13.0);
    }

    #[test]
    fn clamped_font_size_does_not_exceed_the_max() {
        assert_eq!(Workspace::clamped_font_size(96.0, 1.0), 96.0);
    }

    #[test]
    fn clamped_font_size_does_not_go_below_the_min() {
        assert_eq!(Workspace::clamped_font_size(6.0, -1.0), 6.0);
    }

    #[test]
    fn lowest_free_number_starts_at_one_when_empty() {
        assert_eq!(Workspace::lowest_free_number(&HashSet::new()), 1);
    }

    #[test]
    fn lowest_free_number_skips_used_numbers() {
        let used: HashSet<u32> = [1, 2, 3].into_iter().collect();
        assert_eq!(Workspace::lowest_free_number(&used), 4);
    }

    #[test]
    fn lowest_free_number_reuses_a_gap() {
        // 1 and 3 are in use, 2 was freed (e.g. a closed tab) — the next
        // allocation should reuse 2, not jump to 4.
        let used: HashSet<u32> = [1, 3].into_iter().collect();
        assert_eq!(Workspace::lowest_free_number(&used), 2);
    }

    #[test]
    fn next_tab_index_advances_by_one() {
        assert_eq!(Workspace::next_tab_index(0, 3), 1);
        assert_eq!(Workspace::next_tab_index(1, 3), 2);
    }

    #[test]
    fn next_tab_index_wraps_at_the_end() {
        assert_eq!(Workspace::next_tab_index(2, 3), 0);
    }

    #[test]
    fn next_tab_index_is_a_safe_no_op_with_no_tabs() {
        assert_eq!(Workspace::next_tab_index(0, 0), 0);
    }

    #[test]
    fn next_tab_index_stays_put_with_a_single_tab() {
        assert_eq!(Workspace::next_tab_index(0, 1), 0);
    }

    #[test]
    fn prev_tab_index_retreats_by_one() {
        assert_eq!(Workspace::prev_tab_index(2, 3), 1);
        assert_eq!(Workspace::prev_tab_index(1, 3), 0);
    }

    #[test]
    fn prev_tab_index_wraps_at_the_start() {
        assert_eq!(Workspace::prev_tab_index(0, 3), 2);
    }

    #[test]
    fn prev_tab_index_is_a_safe_no_op_with_no_tabs() {
        assert_eq!(Workspace::prev_tab_index(0, 0), 0);
    }

    #[test]
    fn goto_tab_index_converts_one_indexed_to_zero_indexed() {
        assert_eq!(Workspace::goto_tab_index(1, 3), Some(0));
        assert_eq!(Workspace::goto_tab_index(3, 3), Some(2));
    }

    #[test]
    fn goto_tab_index_rejects_out_of_range() {
        assert_eq!(Workspace::goto_tab_index(4, 3), None);
        assert_eq!(Workspace::goto_tab_index(0, 3), None);
        assert_eq!(Workspace::goto_tab_index(1, 0), None);
    }
}
