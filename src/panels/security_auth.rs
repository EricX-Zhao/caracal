//! `SecurityAuthPanel`: the 安全认证 left-sidebar panel for managing the
//! vault's shared SSH keys (originally also saved passwords — see
//! docs/superpowers/specs/2026-07-15-security-auth-panel-design.md for that
//! history; the 2026-07-18 auth-simplification design removed the
//! Passwords tab, reverting password auth to always-direct input).
//! Reads/writes through a `WeakEntity<SessionsPanel>` — `SessionsPanel`
//! stays the single owner/persister of this data, exactly as
//! `NewConnectionWindow` already treats it.

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, transparent_black,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, IconName, Sizable, WindowExt, v_flex};

use crate::config::SshKeyEntry;
use crate::panels::sessions::SessionsPanel;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthTab {
    Keys,
}

pub struct SecurityAuthPanel {
    focus_handle: FocusHandle,
    panel: WeakEntity<SessionsPanel>,
    active_tab: AuthTab,
    ssh_keys: Vec<SshKeyEntry>,
    _sync_sub: gpui::Subscription,
}

impl SecurityAuthPanel {
    pub fn new(panel: WeakEntity<SessionsPanel>, cx: &mut Context<Self>) -> Self {
        let ssh_keys = panel.upgrade().map(|p| p.read(cx).ssh_keys().to_vec()).unwrap_or_default();

        // Re-sync whenever `SessionsPanel` changes (e.g. a key imported
        // from the new-connection form while this panel is also open) —
        // `SessionsPanel` already calls `cx.notify()` on every mutation.
        let sync_sub = if let Some(sessions) = panel.upgrade() {
            cx.observe(&sessions, |this, sessions, cx| {
                this.ssh_keys = sessions.read(cx).ssh_keys().to_vec();
                cx.notify();
            })
        } else {
            // No live `SessionsPanel` to observe — degrade to a no-op
            // subscription rather than panicking; shouldn't happen in
            // practice (this panel is always constructed with a live one).
            cx.observe(&cx.entity(), |_, _: Entity<Self>, _| {})
        };

        Self {
            focus_handle: cx.focus_handle(),
            panel,
            active_tab: AuthTab::Keys,
            ssh_keys,
            _sync_sub: sync_sub,
        }
    }

    fn tab_button(&self, tab: AuthTab, label: SharedString, cx: &Context<Self>) -> impl IntoElement {
        let active = self.active_tab == tab;
        div()
            .id(SharedString::from(match tab {
                AuthTab::Keys => "security-auth-tab-keys",
            }))
            .px_3()
            .py_1()
            .rounded_sm()
            .bg(if active { cx.theme().list_active } else { transparent_black() })
            .text_color(if active { cx.theme().foreground } else { cx.theme().muted_foreground })
            .hover(|s| s.bg(cx.theme().accent))
            .child(label)
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.active_tab = tab;
                cx.notify();
            }))
    }

    // --- Keys tab ------------------------------------------------------

    fn render_keys_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1();
        for key in self.ssh_keys.clone() {
            let id = key.id.clone();
            let id_for_rename = id.clone();
            let id_for_delete = id.clone();
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(cx.theme().secondary)
                    .child(div().flex_1().child(key.name.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!("rename-key-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .label(rust_i18n::t!("SecurityAuth.rename_button"))
                                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                                        this.rename_ssh_key(id_for_rename.clone(), window, cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("delete-key-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Delete)
                                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                                        this.confirm_delete_ssh_key(id_for_delete.clone(), window, cx);
                                    })),
                            ),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(list)
            .child(
                Button::new("add-ssh-key")
                    .xsmall()
                    .label(rust_i18n::t!("SecurityAuth.add_key_button"))
                    .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                        this.pick_and_add_ssh_key(window, cx);
                    })),
            )
    }

    fn rename_ssh_key(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let current_name = self
            .ssh_keys
            .iter()
            .find(|k| k.id == id)
            .map(|k| k.name.clone())
            .unwrap_or_default();
        let name_input = cx.new(|cx| InputState::new(window, cx).default_value(current_name));
        let panel = self.panel.clone();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let name_input = name_input.clone();
            let id = id.clone();
            let panel = panel.clone();
            alert
                .title(rust_i18n::t!("SecurityAuth.rename_key_title"))
                .description(Input::new(&name_input))
                .confirm()
                .on_ok(move |_, window, cx| {
                    let new_name = name_input.read(cx).value().trim().to_string();
                    if !new_name.is_empty() {
                        let _ = panel.update(cx, |p, cx| p.update_ssh_key(&id, new_name, cx));
                        let _ = panel.update(cx, |p, _cx| p.persist_for_security_auth());
                    }
                    window.close_dialog(cx);
                    true
                })
        });
    }

    fn confirm_delete_ssh_key(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let usage = self
            .panel
            .upgrade()
            .map(|p| p.read(cx).connections_using_ssh_key(&id))
            .unwrap_or_default();
        let panel = self.panel.clone();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let id = id.clone();
            let panel = panel.clone();
            let body = if usage.is_empty() {
                rust_i18n::t!("SecurityAuth.delete_confirm_body").to_string()
            } else {
                rust_i18n::t!("SecurityAuth.delete_confirm_body_in_use", count = usage.len()).to_string()
            };
            alert
                .title(rust_i18n::t!("SecurityAuth.delete_key_title"))
                .description(body)
                .confirm()
                .on_ok(move |_, window, cx| {
                    let _ = panel.update(cx, |p, cx| p.remove_ssh_key(&id, cx));
                    let _ = panel.update(cx, |p, _cx| p.persist_for_security_auth());
                    window.close_dialog(cx);
                    true
                })
        });
    }

    /// Picks a key file first (exact pattern already proven in
    /// `new_connection_window.rs`'s "Import key file..." button and
    /// `sessions.rs`'s `import_connections`), *then* opens a small dialog
    /// (name + optional passphrase) once the content is in hand — avoids
    /// needing to spawn async file-picker work from within an
    /// already-open dialog, which no code in this codebase does yet.
    fn pick_and_add_ssh_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let panel = self.panel.clone();
        cx.spawn_in(window, async move |_this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let Ok(content) = std::fs::read(&path) else {
                return;
            };
            let default_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "key".to_string());
            let source_path = path.to_string_lossy().to_string();
            let _ = cx.update(|window, cx| {
                let name_input =
                    cx.new(|cx| InputState::new(window, cx).default_value(default_name.clone()));
                let passphrase_input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(true)
                        .placeholder(rust_i18n::t!("NewConnectionWindow.private_key_passphrase_placeholder"))
                });
                window.open_alert_dialog(cx, move |alert, _window, _cx| {
                    let name_input = name_input.clone();
                    let content = content.clone();
                    let source_path = source_path.clone();
                    let panel = panel.clone();
                    let body = v_flex().gap_2().child(Input::new(&name_input)).child(
                        Input::new(&passphrase_input).mask_toggle(),
                    );
                    alert
                        .title(rust_i18n::t!("SecurityAuth.add_key_title"))
                        .description(body)
                        .confirm()
                        .on_ok(move |_, window, cx| {
                            let mut name = name_input.read(cx).value().trim().to_string();
                            if name.is_empty() {
                                name = std::path::Path::new(&source_path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| source_path.clone());
                            }
                            let Some(vault) = cx.try_global::<crate::workspace::VaultKey>() else {
                                return false;
                            };
                            let entry = crate::config::SshKeyEntry {
                                id: crate::config::generate_id(),
                                name,
                                source_path: Some(source_path.clone()),
                                encrypted_content: vault.0.encrypt_bytes(&content),
                            };
                            let _ = panel.update(cx, |p, cx| p.add_ssh_key(entry, cx));
                            let _ = panel.update(cx, |p, _cx| p.persist_for_security_auth());
                            window.close_dialog(cx);
                            true
                        })
                });
            });
        })
        .detach();
    }

}

impl Focusable for SecurityAuthPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SecurityAuthPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(self.tab_button(AuthTab::Keys, rust_i18n::t!("SecurityAuth.tab_keys").into(), cx)),
            )
            .child(
                div().flex_1().p_2().child(match self.active_tab {
                    AuthTab::Keys => self.render_keys_tab(cx).into_any_element(),
                }),
            )
    }
}
