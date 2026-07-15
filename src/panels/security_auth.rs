//! `SecurityAuthPanel`: the 安全认证 left-sidebar panel for managing the
//! vault's shared SSH keys and saved passwords (see
//! docs/superpowers/specs/2026-07-15-security-auth-panel-design.md).
//! Reads/writes through a `WeakEntity<SessionsPanel>` — `SessionsPanel`
//! stays the single owner/persister of this data, exactly as
//! `NewConnectionWindow` already treats it.

use std::collections::HashSet;

use gpui::{
    App, AppContext, ClickEvent, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, prelude::FluentBuilder,
    transparent_black,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, IconName, Sizable, WindowExt, v_flex};

use crate::config::{SavedPasswordEntry, SshKeyEntry};
use crate::panels::sessions::SessionsPanel;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthTab {
    Keys,
    Passwords,
}

pub struct SecurityAuthPanel {
    focus_handle: FocusHandle,
    panel: WeakEntity<SessionsPanel>,
    active_tab: AuthTab,
    ssh_keys: Vec<SshKeyEntry>,
    saved_passwords: Vec<SavedPasswordEntry>,
    /// Which saved-password rows currently show their decrypted value —
    /// toggled per-row by the eye icon.
    revealed_password_ids: HashSet<String>,
    _sync_sub: gpui::Subscription,
}

impl SecurityAuthPanel {
    pub fn new(panel: WeakEntity<SessionsPanel>, cx: &mut Context<Self>) -> Self {
        let (ssh_keys, saved_passwords) = panel
            .upgrade()
            .map(|p| (p.read(cx).ssh_keys().to_vec(), p.read(cx).saved_passwords().to_vec()))
            .unwrap_or_default();

        // Re-sync whenever `SessionsPanel` changes (e.g. a key imported
        // from the new-connection form while this panel is also open) —
        // `SessionsPanel` already calls `cx.notify()` on every mutation.
        let sync_sub = if let Some(sessions) = panel.upgrade() {
            cx.observe(&sessions, |this, sessions, cx| {
                this.ssh_keys = sessions.read(cx).ssh_keys().to_vec();
                this.saved_passwords = sessions.read(cx).saved_passwords().to_vec();
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
            saved_passwords,
            revealed_password_ids: HashSet::new(),
            _sync_sub: sync_sub,
        }
    }

    fn tab_button(&self, tab: AuthTab, label: SharedString, cx: &Context<Self>) -> impl IntoElement {
        let active = self.active_tab == tab;
        div()
            .id(SharedString::from(match tab {
                AuthTab::Keys => "security-auth-tab-keys",
                AuthTab::Passwords => "security-auth-tab-passwords",
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
                        let _ = panel.update(cx, |p, _cx| p.update_ssh_key(&id, new_name));
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
                    let _ = panel.update(cx, |p, _cx| p.remove_ssh_key(&id));
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
                            let _ = panel.update(cx, |p, _cx| p.add_ssh_key(entry));
                            let _ = panel.update(cx, |p, _cx| p.persist_for_security_auth());
                            window.close_dialog(cx);
                            true
                        })
                });
            });
        })
        .detach();
    }

    // --- Passwords tab ---------------------------------------------------

    fn render_passwords_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let vault = cx.try_global::<crate::workspace::VaultKey>();
        let mut list = div().flex().flex_col().gap_1();
        for entry in self.saved_passwords.clone() {
            let id = entry.id.clone();
            let revealed = self.revealed_password_ids.contains(&id);
            let plaintext = if revealed {
                vault.and_then(|v| v.0.decrypt_str(&entry.encrypted_password).ok())
            } else {
                None
            };
            let id_for_reveal = id.clone();
            let id_for_edit = id.clone();
            let id_for_delete = id.clone();
            let plaintext_for_copy = plaintext.clone();
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
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(entry.name.clone())
                            .when_some(plaintext.clone(), |el, p| {
                                el.child(div().text_xs().text_color(cx.theme().muted_foreground).child(p))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!("reveal-password-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .icon(if revealed { IconName::EyeOff } else { IconName::Eye })
                                    .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                                        if this.revealed_password_ids.contains(&id_for_reveal) {
                                            this.revealed_password_ids.remove(&id_for_reveal);
                                        } else {
                                            this.revealed_password_ids.insert(id_for_reveal.clone());
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("copy-password-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Copy)
                                    .on_click(cx.listener(move |_this, _ev: &ClickEvent, _window, cx| {
                                        if let Some(p) = &plaintext_for_copy {
                                            cx.write_to_clipboard(ClipboardItem::new_string(p.clone()));
                                        }
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("edit-password-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .label(rust_i18n::t!("SecurityAuth.edit_button"))
                                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                                        this.open_add_or_edit_password_dialog(
                                            Some(id_for_edit.clone()),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("delete-password-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Delete)
                                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                                        this.confirm_delete_saved_password(id_for_delete.clone(), window, cx);
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
                Button::new("add-saved-password")
                    .xsmall()
                    .label(rust_i18n::t!("SecurityAuth.add_password_button"))
                    .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                        this.open_add_or_edit_password_dialog(None, window, cx);
                    })),
            )
    }

    /// Shared by "Add password" (`existing_id: None`) and each row's
    /// "Edit" button (`existing_id: Some(id)`, pre-filled decrypted).
    fn open_add_or_edit_password_dialog(
        &mut self,
        existing_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = existing_id.as_ref().and_then(|id| self.saved_passwords.iter().find(|p| &p.id == id));
        let vault = cx.try_global::<crate::workspace::VaultKey>();
        let decrypted = existing.and_then(|e| vault.and_then(|v| v.0.decrypt_str(&e.encrypted_password).ok()));
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("SecurityAuth.password_name_placeholder"))
                .default_value(existing.map(|e| e.name.clone()).unwrap_or_default())
        });
        let password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(rust_i18n::t!("NewConnectionWindow.password_placeholder"))
                .default_value(decrypted.unwrap_or_default())
        });
        let panel = self.panel.clone();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let name_input = name_input.clone();
            let password_input = password_input.clone();
            let panel = panel.clone();
            let existing_id = existing_id.clone();
            let body = v_flex()
                .gap_2()
                .child(Input::new(&name_input))
                .child(Input::new(&password_input).mask_toggle());
            alert
                .title(if existing_id.is_some() {
                    rust_i18n::t!("SecurityAuth.edit_password_title")
                } else {
                    rust_i18n::t!("SecurityAuth.add_password_title")
                })
                .description(body)
                .confirm()
                .on_ok(move |_, window, cx| {
                    let name = name_input.read(cx).value().trim().to_string();
                    let plaintext = password_input.read(cx).value().to_string();
                    if name.is_empty() {
                        return false;
                    }
                    let Some(vault) = cx.try_global::<crate::workspace::VaultKey>() else {
                        return false;
                    };
                    let encrypted = vault.0.encrypt_str(&plaintext);
                    match &existing_id {
                        Some(id) => {
                            let _ = panel.update(cx, |p, _cx| p.update_saved_password(id, name, encrypted));
                        }
                        None => {
                            let entry = crate::config::SavedPasswordEntry {
                                id: crate::config::generate_id(),
                                name,
                                encrypted_password: encrypted,
                            };
                            let _ = panel.update(cx, |p, _cx| p.add_saved_password(entry));
                        }
                    }
                    let _ = panel.update(cx, |p, _cx| p.persist_for_security_auth());
                    window.close_dialog(cx);
                    true
                })
        });
    }

    fn confirm_delete_saved_password(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let usage = self
            .panel
            .upgrade()
            .map(|p| p.read(cx).connections_using_saved_password(&id))
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
                .title(rust_i18n::t!("SecurityAuth.delete_password_title"))
                .description(body)
                .confirm()
                .on_ok(move |_, window, cx| {
                    let _ = panel.update(cx, |p, _cx| p.remove_saved_password(&id));
                    let _ = panel.update(cx, |p, _cx| p.persist_for_security_auth());
                    window.close_dialog(cx);
                    true
                })
        });
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
                    .child(self.tab_button(AuthTab::Keys, rust_i18n::t!("SecurityAuth.tab_keys").into(), cx))
                    .child(self.tab_button(
                        AuthTab::Passwords,
                        rust_i18n::t!("SecurityAuth.tab_passwords").into(),
                        cx,
                    )),
            )
            .child(
                div().flex_1().p_2().child(match self.active_tab {
                    AuthTab::Keys => self.render_keys_tab(cx).into_any_element(),
                    AuthTab::Passwords => self.render_passwords_tab(cx).into_any_element(),
                }),
            )
    }
}
