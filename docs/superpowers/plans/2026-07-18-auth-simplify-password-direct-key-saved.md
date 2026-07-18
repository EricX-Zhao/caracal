# Simplify SSH Auth: Password Direct-Only, Key Saved-Only Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Password auth always uses direct/typed input (no more shared "saved password" picker); key auth always picks from the shared key store (no more "import inline" tab in the connection form); Security & Auth's Passwords tab is removed entirely. Existing connections using the old saved-password mode are migrated to direct mode transparently.

**Architecture:** Reverts/simplifies the 2026-07-15 Security & Auth panel feature's password half and the key half's UI convenience, while keeping the underlying shared-key infrastructure (`ssh_keys`/`SshKeyEntry`) untouched. `password_id`/`saved_passwords` stay on the data model as migration-source-only fields (mirroring the existing `password`/`private_key_path` pattern already in `SavedConnection`), converted to direct mode by a new idempotent migration function that runs at every point the vault becomes unlocked.

**Tech Stack:** Rust, `gpui`/`gpui_component` (UI), `rust_i18n` (locale strings in `locales/app.yml`), TOML-backed `AppConfig` persistence.

## Global Constraints

- Every new user-facing string needs both `zh-CN` and `en` entries in `locales/app.yml`, referenced via `rust_i18n::t!("Namespace.key", ...)`.
- No screenshot-driven GUI verification — ask the user to manually verify GUI behavior; describe exactly what to check.
- Verification commands: `cargo build` for compile checks, `cargo test --bin caracal <module::path>` for scoped unit tests (this is a bin crate, not a lib crate — `cargo test --lib` does not work here).
- Never delete a struct field that removed UI still leaves as the only record of a user's existing data without a migration path first — TOML deserialization silently drops removed fields (see `SavedConnection`'s existing `password`/`private_key_path` doc comments for the precedent this plan follows).
- Commit messages: plain, present-tense, no "Co-Authored-By" trailer.

---

## Task 1: Vault migration — convert saved-password connections to direct mode

**Files:**
- Modify: `src/vault.rs` (new functions + tests)

**Interfaces:**
- Consumes: `crate::config::{AppConfig, SavedConnection, SavedPasswordEntry}`, `crate::crypto::MasterKey` (all existing).
- Produces: `pub(crate) fn migrate_password_ids(connections: &mut [SavedConnection], saved_passwords: &[SavedPasswordEntry], master: &MasterKey) -> bool` and `pub fn migrate_saved_passwords_to_direct(cfg: &mut AppConfig, master: &MasterKey) -> bool` — both consumed by Task 2.

- [ ] **Step 1: Write the failing tests**

In `src/vault.rs`, inside the existing `#[cfg(test)] mod tests` block (it already has a `base_connection` helper — reuse it), add:

```rust
    #[test]
    fn migrate_saved_passwords_to_direct_converts_password_id_to_encrypted_password() {
        let master = MasterKey::generate();
        let mut cfg = AppConfig::default();
        cfg.saved_passwords.push(SavedPasswordEntry {
            id: "pw-1".to_string(),
            name: "shared".to_string(),
            encrypted_password: master.encrypt_str("hunter2"),
        });
        let mut conn = base_connection("a.example.com");
        conn.password_id = Some("pw-1".to_string());
        cfg.connections.push(conn);

        let changed = migrate_saved_passwords_to_direct(&mut cfg, &master);

        assert!(changed);
        assert!(cfg.connections[0].password_id.is_none());
        assert!(cfg.saved_passwords.is_empty());
        assert_eq!(
            master.decrypt_str(&cfg.connections[0].encrypted_password).unwrap(),
            "hunter2"
        );
    }

    #[test]
    fn migrate_saved_passwords_to_direct_is_a_noop_when_nothing_to_migrate() {
        let master = MasterKey::generate();
        let mut cfg = AppConfig { connections: vec![base_connection("a.example.com")], ..Default::default() };
        let changed = migrate_saved_passwords_to_direct(&mut cfg, &master);
        assert!(!changed);
    }

    #[test]
    fn migrate_saved_passwords_to_direct_clears_dangling_reference_without_panicking() {
        let master = MasterKey::generate();
        let mut cfg = AppConfig::default();
        let mut conn = base_connection("a.example.com");
        conn.password_id = Some("does-not-exist".to_string());
        cfg.connections.push(conn);

        let changed = migrate_saved_passwords_to_direct(&mut cfg, &master);

        assert!(changed);
        assert!(cfg.connections[0].password_id.is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin caracal vault::tests::migrate_saved_passwords_to_direct`
Expected: FAIL to compile — `migrate_saved_passwords_to_direct` doesn't exist yet.

- [ ] **Step 3: Implement the migration functions**

In `src/vault.rs`, add these two functions above the existing `#[cfg(test)]` module (e.g. right after `import_merge` and before `content_hash`):

```rust
/// Core logic behind [`migrate_saved_passwords_to_direct`], factored out so
/// it can also run directly against `SessionsPanel`'s own fields — the
/// OS-keyring convenience-unlock path (`workspace.rs`) never has a full
/// `AppConfig` in scope by the time it runs (its `cfg` was already moved
/// into `SessionsPanel::new` earlier in the same function).
pub(crate) fn migrate_password_ids(
    connections: &mut [crate::config::SavedConnection],
    saved_passwords: &[SavedPasswordEntry],
    master: &MasterKey,
) -> bool {
    let mut changed = false;
    for conn in connections {
        let Some(password_id) = conn.password_id.take() else {
            continue;
        };
        changed = true;
        match saved_passwords.iter().find(|p| p.id == password_id) {
            Some(entry) => match master.decrypt_str(&entry.encrypted_password) {
                Ok(plaintext) => conn.encrypted_password = master.encrypt_str(&plaintext),
                Err(e) => log::warn!(
                    "saved-password migration: entry {password_id:?} failed to decrypt: {e:#}; leaving connection's password empty"
                ),
            },
            None => log::warn!(
                "saved-password migration: connection referenced missing saved password {password_id:?}"
            ),
        }
    }
    changed
}

/// One-time conversion of any connection still using the removed "saved
/// password" sharing feature (see the 2026-07-18 auth-simplification
/// design) back to direct mode: decrypts the referenced `saved_passwords`
/// entry and re-encrypts it into the connection's own `encrypted_password`,
/// then clears `password_id`. Safe to call on every unlock — a cheap no-op
/// once a config has already been migrated. Returns `true` if anything
/// changed, so the caller knows whether to persist.
pub fn migrate_saved_passwords_to_direct(cfg: &mut AppConfig, master: &MasterKey) -> bool {
    if cfg.saved_passwords.is_empty() && !cfg.connections.iter().any(|c| c.password_id.is_some()) {
        return false;
    }
    let saved_passwords = std::mem::take(&mut cfg.saved_passwords);
    migrate_password_ids(&mut cfg.connections, &saved_passwords, master);
    true
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --bin caracal vault::tests::migrate_saved_passwords_to_direct`
Expected: PASS (3 tests).

- [ ] **Step 5: Run the full vault test suite to confirm nothing broke**

Run: `cargo test --bin caracal vault::`
Expected: all existing vault tests still PASS alongside the 3 new ones.

- [ ] **Step 6: Commit**

```bash
git add src/vault.rs
git commit -m "feat: add one-time migration from saved-password mode to direct mode"
```

---

## Task 2: Wire the migration into all vault-unlock paths + add a Security & Auth jump target

**Files:**
- Modify: `src/panels/sessions.rs` (new field, new methods, constructor signature)
- Modify: `src/workspace.rs` (new method, constructor call site, 3 unlock-path call sites)

**Interfaces:**
- Consumes: `vault::migrate_password_ids`/`vault::migrate_saved_passwords_to_direct` (Task 1).
- Produces: `SessionsPanel::open_security_auth_panel(&self, cx: &mut Context<Self>)`, `SessionsPanel::migrate_saved_passwords_to_direct(&mut self, master: &MasterKey) -> bool`, `Workspace::open_security_auth_panel(&mut self, cx: &mut Context<Self>)` — the first two consumed by Task 4 (empty-key-picker jump button), all three exercised by this task's own wiring.

There's no practical way to unit-test the unlock-dialog wiring itself (it's driven by `open_alert_dialog`/global state) — verification here is `cargo build` plus a manual check that unlock still works, folded into this task's own steps.

- [ ] **Step 1: Add `WeakEntity` import and the `workspace` field to `SessionsPanel`**

In `src/panels/sessions.rs`, find:

```rust
use gpui::{
    Action, Anchor, App, AppContext, Bounds, ClickEvent, Context, DragMoveEvent, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, WindowHandle,
    anchored, deferred, div, point, prelude::FluentBuilder, px,
};
```

Replace with:

```rust
use gpui::{
    Action, Anchor, App, AppContext, Bounds, ClickEvent, Context, DragMoveEvent, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window,
    WindowHandle, anchored, deferred, div, point, prelude::FluentBuilder, px,
};
```

Then find the `SessionsPanel` struct's `search_placeholder_locale` field (its last field):

```rust
    search_placeholder_locale: Option<String>,
}
```

Replace with:

```rust
    search_placeholder_locale: Option<String>,
    /// Back-reference so `open_security_auth_panel` can jump the user to
    /// Security & Auth's Keys tab from `NewConnectionWindow`'s empty
    /// saved-key picker (a separate, standalone window with no other path
    /// back to the main workspace's left-panel slots).
    workspace: WeakEntity<crate::workspace::Workspace>,
}
```

- [ ] **Step 2: Add the `workspace` parameter to `SessionsPanel::new`**

In `src/panels/sessions.rs`, find:

```rust
    pub fn new(
        connections: Vec<SavedConnection>,
        groups: Vec<SavedConnectionGroup>,
        vault: Option<crate::config::VaultMeta>,
        ssh_keys: Vec<crate::config::SshKeyEntry>,
        saved_passwords: Vec<crate::config::SavedPasswordEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```

Replace with:

```rust
    pub fn new(
        connections: Vec<SavedConnection>,
        groups: Vec<SavedConnectionGroup>,
        vault: Option<crate::config::VaultMeta>,
        ssh_keys: Vec<crate::config::SshKeyEntry>,
        saved_passwords: Vec<crate::config::SavedPasswordEntry>,
        workspace: WeakEntity<crate::workspace::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```

Then find the `Self { ... }` literal's last field:

```rust
            search_placeholder_locale: None,
        }
    }
```

Replace with:

```rust
            search_placeholder_locale: None,
            workspace,
        }
    }
```

- [ ] **Step 3: Add `open_security_auth_panel` and `migrate_saved_passwords_to_direct` methods on `SessionsPanel`**

In `src/panels/sessions.rs`, find the end of `connections_using_ssh_key` (right before the doc comment for the import-merge dialog method, or any convenient spot after the existing `connections_using_ssh_key`/before `connections_using_saved_password`):

```rust
    /// Connection display names currently referencing a given saved key,
    /// for the delete-confirm dialog's "used by N connections" warning.
    pub(crate) fn connections_using_ssh_key(&self, id: &str) -> Vec<String> {
        self.connections
            .iter()
            .filter(|c| c.private_key_id.as_deref() == Some(id))
            .map(|c| c.display_name())
            .collect()
    }
```

Insert immediately after it:

```rust

    /// Jumps the main workspace's left panel to Security & Auth's Keys tab
    /// — used by `NewConnectionWindow`'s empty saved-key picker, since that
    /// standalone window has no other path back to the workspace's
    /// left-panel slots.
    pub(crate) fn open_security_auth_panel(&self, cx: &mut Context<Self>) {
        let _ = self.workspace.update(cx, |ws, cx| ws.open_security_auth_panel(cx));
    }

    /// Runs the one-time saved-password-to-direct migration (see
    /// `vault::migrate_saved_passwords_to_direct`) directly against this
    /// panel's own fields, for the one unlock path (`workspace.rs`'s
    /// OS-keyring convenience-unlock) that never has a full `AppConfig` in
    /// scope. Returns `true` if anything changed, so the caller knows
    /// whether to persist. Does not persist itself.
    pub(crate) fn migrate_saved_passwords_to_direct(&mut self, master: &crate::crypto::MasterKey) -> bool {
        if self.saved_passwords.is_empty() && !self.connections.iter().any(|c| c.password_id.is_some()) {
            return false;
        }
        let saved_passwords = std::mem::take(&mut self.saved_passwords);
        crate::vault::migrate_password_ids(&mut self.connections, &saved_passwords, master);
        true
    }
```

- [ ] **Step 4: Add `Workspace::open_security_auth_panel`**

In `src/workspace.rs`, find `toggle_panel`:

```rust
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
```

Insert immediately after it:

```rust

    /// Forces the left panel to Security & Auth — unlike `toggle_panel`,
    /// never closes it if already active. Called from `SessionsPanel`
    /// (itself called from `NewConnectionWindow`, a separate standalone
    /// window with no other path back to this panel's slots).
    pub(crate) fn open_security_auth_panel(&mut self, cx: &mut Context<Self>) {
        self.left_active = Some(PanelId::Security);
        cx.notify();
    }
```

- [ ] **Step 5: Thread a shared `workspace_handle` to `SessionsPanel::new` and wire the keyring-unlock migration**

In `src/workspace.rs`, find:

```rust
        let saved = cx.new(|cx| {
            SessionsPanel::new(
                cfg.connections,
                cfg.groups,
                cfg.vault,
                cfg.ssh_keys,
                cfg.saved_passwords,
                window,
                cx,
            )
        });
```

Replace with:

```rust
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
```

Then find the later, now-duplicate `workspace_handle` computed for `QuickCommandsPanel`:

```rust
        let workspace_handle = cx.entity().downgrade();
        let quick_commands_panel = cx.new(|cx| QuickCommandsPanel::new(workspace_handle, cx));
```

Replace with (drop the now-redundant re-computation, reuse the one from above):

```rust
        let quick_commands_panel = cx.new(|cx| QuickCommandsPanel::new(workspace_handle, cx));
```

Then find the OS-keyring convenience-unlock block:

```rust
            // Try the OS-keyring convenience-unlock cache first — a hit
            // skips the password prompt entirely. Any failure (never
            // opted in, keyring unavailable, entry cleared) is treated as
            // a cache miss, never as blocking normal password unlock.
            if let Some(vault_id) = &vault_id {
                if let Some(key_bytes) = keyring_store::OsSecretStore.get(vault_id) {
                    let master = crate::crypto::MasterKey(zeroize::Zeroizing::new(key_bytes));
                    cx.set_global(VaultKey(master));
                    return;
                }
            }
            show_unlock_dialog(window, cx);
        });
```

Replace with:

```rust
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
```

Note this reuses the `_this` parameter the deferred closure already receives (previously unused, hence the underscore — leave the underscore since it's still only used for this one new purpose, not renamed).

- [ ] **Step 6: Wire the migration into the fresh-vault-setup path**

In `src/workspace.rs`, find:

```rust
                let mut cfg = config::load();
                match vault::migrate(&mut cfg, &password) {
                    Ok(master) => {
                        if let Err(e) = config::save(&cfg) {
                            log::error!("failed to save migrated vault: {e}");
                        }
                        cx.set_global(VaultKey(master));
                        window.close_dialog(cx);
                        true
                    }
```

Replace with:

```rust
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
```

- [ ] **Step 7: Wire the migration into the manual-unlock-dialog path**

In `src/workspace.rs`, find:

```rust
            .on_ok(move |_, window, cx| {
                let password = pw_input.read(cx).value().to_string();
                let cfg = config::load();
                match vault::unlock(&cfg, &password) {
                    Ok(master) => {
                        if *remember.read(cx) {
                            if let Some(vault_meta) = &cfg.vault {
                                keyring_store::OsSecretStore.set(&vault_meta.vault_id, &master.0);
                            }
                        }
                        cx.set_global(VaultKey(master));
                        window.close_dialog(cx);
                        true
                    }
```

Replace with:

```rust
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
```

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: succeeds with no errors.

- [ ] **Step 9: Manual verification (ask the user)**

Ask the user to confirm the app still starts and unlocks normally (whichever of the three paths applies to their current setup — fresh install, password prompt, or the "remember on this device" keyring cache) with no new errors in the terminal log. This task has no user-visible behavior change yet (nothing calls `open_security_auth_panel` until Task 4), so this is purely a regression check on existing unlock flows.

- [ ] **Step 10: Commit**

```bash
git add src/panels/sessions.rs src/workspace.rs
git commit -m "feat: wire saved-password migration into all vault-unlock paths

Also adds Workspace::open_security_auth_panel / SessionsPanel::
open_security_auth_panel, a jump target for the connection form's
about-to-be-added empty saved-key-picker state (not yet called from
anywhere until the next task)."
```

---

## Task 3: Connection form — password field becomes direct-only

**Files:**
- Modify: `src/panels/new_connection_window.rs`
- Modify: `src/panels/sessions.rs` (constructor call site)

**Interfaces:**
- Consumes: nothing new.
- Produces: `NewConnectionWindow::new` drops its `saved_passwords` parameter — Task 4 doesn't touch this signature further, but be aware the parameter list changes here.

- [ ] **Step 1: Remove `PasswordTab` and the password-related fields**

In `src/panels/new_connection_window.rs`, find:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum PasswordTab {
    Direct,
    Saved,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyTab {
    ImportNew,
    Saved,
}
```

Replace with:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyTab {
    ImportNew,
    Saved,
}
```

Then find:

```rust
    /// Which sub-tab the Password field is on. `Direct` is always the
    /// default/starting tab, including when editing a connection with
    /// `password_id: Some` and that entry has since been deleted —
    /// `to_ssh_config` handles the dangling case at connect time; here it
    /// just means the picker opens with nothing pre-selected.
    password_tab: PasswordTab,
    /// `Some(id)` when an existing saved password is selected on the
    /// Saved tab.
    selected_password_id: Option<String>,
    /// Snapshot of the vault's shared saved passwords, for the picker list.
    saved_passwords: Vec<crate::config::SavedPasswordEntry>,
    /// `true` while the inline "add a new saved password" mini-form is
    /// expanded within the Saved tab.
    adding_new_saved_password: bool,
    new_saved_password_name: Entity<InputState>,
    new_saved_password_value: Entity<InputState>,
    /// Which sub-tab the Key field is on — a pure UI reshuffle of the
```

Replace with:

```rust
    /// Which sub-tab the Key field is on — a pure UI reshuffle of the
```

- [ ] **Step 2: Drop the `saved_passwords` constructor parameter and related init**

In `src/panels/new_connection_window.rs`, find:

```rust
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        panel: WeakEntity<SessionsPanel>,
        existing: Option<(usize, SavedConnection)>,
        group_id: Option<String>,
        new_sort_order: i32,
        ssh_keys: Vec<crate::config::SshKeyEntry>,
        saved_passwords: Vec<crate::config::SavedPasswordEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```

Replace with:

```rust
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        panel: WeakEntity<SessionsPanel>,
        existing: Option<(usize, SavedConnection)>,
        group_id: Option<String>,
        new_sort_order: i32,
        ssh_keys: Vec<crate::config::SshKeyEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```

Then find:

```rust
        let password_tab = if conn.as_ref().and_then(|c| c.password_id.clone()).is_some() {
            PasswordTab::Saved
        } else {
            PasswordTab::Direct
        };
        let selected_password_id = conn.as_ref().and_then(|c| c.password_id.clone());
        let key_tab = if conn.as_ref().map(|c| c.auth_method.clone()).as_deref() == Some("key") {
```

Replace with:

```rust
        let key_tab = if conn.as_ref().map(|c| c.auth_method.clone()).as_deref() == Some("key") {
```

Then find, inside the `Self { ... }` literal:

```rust
            selected_key_id: conn.as_ref().and_then(|c| c.private_key_id.clone()),
            pending_new_key: None,
            ssh_keys,
            password_tab,
            selected_password_id,
            saved_passwords,
            adding_new_saved_password: false,
            new_saved_password_name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(rust_i18n::t!("SecurityAuth.password_name_placeholder"))
            }),
            new_saved_password_value: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.password_placeholder"))
            }),
            key_tab,
```

Replace with:

```rust
            selected_key_id: conn.as_ref().and_then(|c| c.private_key_id.clone()),
            pending_new_key: None,
            ssh_keys,
            key_tab,
```

- [ ] **Step 3: Simplify `save()` — password is always direct, never sets `password_id`**

In `src/panels/new_connection_window.rs`, find:

```rust
                // Only persist whichever credential the selected auth
                // method actually uses — otherwise a password typed before
                // switching to key auth would linger even though it's
                // never used to connect (mirrors why `duplicate()` clears
                // `encrypted_key_passphrase` when copying a connection).
                // Computed as locals before the struct literal below since
                // `resolve_key_id` needs `&mut self` (it may register a
                // newly-imported key with the panel), which can't overlap
                // with the `&self.field.read(cx)` borrows used for the
                // other fields in the same literal.
                let password_id = if self.auth_method == "password" && self.password_tab == PasswordTab::Saved {
                    self.selected_password_id.clone()
                } else {
                    None
                };
                let plaintext_password = if self.auth_method == "key" || password_id.is_some() {
                    String::new()
                } else {
                    self.password.read(cx).value().to_string()
                };
```

Replace with:

```rust
                // Only persist whichever credential the selected auth
                // method actually uses — otherwise a password typed before
                // switching to key auth would linger even though it's
                // never used to connect (mirrors why `duplicate()` clears
                // `encrypted_key_passphrase` when copying a connection).
                let plaintext_password = if self.auth_method == "key" {
                    String::new()
                } else {
                    self.password.read(cx).value().to_string()
                };
```

Then find, in the SSH-branch `SavedConnection` literal:

```rust
                    private_key_id: key_id,
                    password_id,
                }
            }
            ConnectionType::Local => {
```

Replace with:

```rust
                    private_key_id: key_id,
                    password_id: None,
                }
            }
            ConnectionType::Local => {
```

(The other three branches — Local, Telnet, Serial — already write `password_id: None` unconditionally; leave those as-is.)

- [ ] **Step 4: Replace `render_ssh_auth_fields`'s password branch and remove the now-dead password-tab rendering methods**

In `src/panels/new_connection_window.rs`, find:

```rust
            } else {
                self.render_password_tabs(cx).into_any_element()
            })
    }
```

Replace with:

```rust
            } else {
                self.field(
                    rust_i18n::t!("NewConnectionWindow.password_placeholder"),
                    &self.password.clone(),
                    cx,
                )
                .into_any_element()
            })
    }
```

Then find and delete the entire `render_password_tabs`, `render_saved_password_picker`, `confirm_add_new_saved_password`, and `cancel_add_new_saved_password` methods — from:

```rust
    fn render_password_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
```

through the end of:

```rust
    fn cancel_add_new_saved_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_saved_password_name.update(cx, |s, cx| s.set_value("", window, cx));
        self.new_saved_password_value.update(cx, |s, cx| s.set_value("", window, cx));
        self.adding_new_saved_password = false;
        cx.notify();
    }
```

Delete all of it (everything from `render_password_tabs`'s opening line through `cancel_add_new_saved_password`'s closing `}`, inclusive) — the next method after it (`}` closing `impl NewConnectionWindow`, then `impl Render for NewConnectionWindow`) should follow directly.

- [ ] **Step 5: Update the `SessionsPanel::open_new_connection_window` call site**

In `src/panels/sessions.rs`, find:

```rust
        let panel = cx.entity().downgrade();
        let ssh_keys = self.ssh_keys.clone();
        let saved_passwords = self.saved_passwords.clone();
        let bounds = gpui::Bounds::centered(None, gpui::size(px(480.0), px(620.0)), cx);
        let result = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            move |window, cx| {
                let new_window = cx.new(|cx| {
                    NewConnectionWindow::new(
                        panel.clone(),
                        existing,
                        group_id.clone(),
                        new_sort_order,
                        ssh_keys.clone(),
                        saved_passwords.clone(),
                        window,
                        cx,
                    )
                });
```

Replace with:

```rust
        let panel = cx.entity().downgrade();
        let ssh_keys = self.ssh_keys.clone();
        let bounds = gpui::Bounds::centered(None, gpui::size(px(480.0), px(620.0)), cx);
        let result = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            move |window, cx| {
                let new_window = cx.new(|cx| {
                    NewConnectionWindow::new(
                        panel.clone(),
                        existing,
                        group_id.clone(),
                        new_sort_order,
                        ssh_keys.clone(),
                        window,
                        cx,
                    )
                });
```

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: FAIL — `security_auth.rs` and `sessions.rs`'s CRUD methods still reference the old signatures/fields in ways that may or may not yet break; specifically expect errors, if any, only about unused-but-still-compiling code at this point (the Passwords tab UI in `security_auth.rs` still calls `NewConnectionWindow` indirectly? No — it doesn't. The likely remaining errors, if any, are about `saved_passwords`/`PasswordTab` references that Steps 1-5 above should have fully removed from `new_connection_window.rs`). If it fails, find and fix any remaining reference to `PasswordTab`, `password_tab`, `selected_password_id`, `saved_passwords`, `adding_new_saved_password`, `new_saved_password_name`, or `new_saved_password_value` inside `new_connection_window.rs` — Steps 1-4 should have removed every one, so a leftover means a step was applied to slightly different surrounding text than expected; use `grep -n "PasswordTab\|password_tab\|selected_password_id\|adding_new_saved_password" src/panels/new_connection_window.rs` to confirm zero matches remain.
Expected once fully applied: succeeds with no errors (warnings about `security_auth.rs`'s still-intact Passwords tab are fine — that's Task 5).

- [ ] **Step 7: Manual verification (ask the user)**

Ask the user to open "新建连接" for an SSH connection type and confirm the Password field is a single masked input with no Direct/Saved toggle above it, and that saving/reconnecting with a typed password still works.

- [ ] **Step 8: Commit**

```bash
git add src/panels/new_connection_window.rs src/panels/sessions.rs
git commit -m "feat: connection form password field is direct-input only

Removes the Direct/Saved toggle, the saved-password dropdown picker,
and the inline add-new-saved-password mini-form. New/edited
connections never set password_id anymore (existing password_id-based
connections are handled by the migration added in the previous
commit)."
```

---

## Task 4: Connection form — key field becomes saved-only, with an empty-state jump to Security & Auth

**Files:**
- Modify: `src/panels/new_connection_window.rs`
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: `SessionsPanel::open_security_auth_panel` (Task 2).
- Produces: nothing new consumed elsewhere.

- [ ] **Step 1: Add `Subscription` to the gpui import and a live `ssh_keys` sync subscription**

In `src/panels/new_connection_window.rs`, find:

```rust
use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, WeakEntity,
    Window, div, prelude::FluentBuilder, px,
};
```

Replace with:

```rust
use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Subscription, WeakEntity, Window, div, prelude::FluentBuilder, px,
};
```

Then find the `ssh_keys` field:

```rust
    /// Snapshot of the vault's shared keys, for the picker list.
    ssh_keys: Vec<crate::config::SshKeyEntry>,
```

Replace with:

```rust
    /// Snapshot of the vault's shared keys, for the picker list. Kept live
    /// via `_ssh_keys_sync_sub` — needs to be, now that this field's empty
    /// state sends the user to Security & Auth to add a key and come back
    /// to this still-open form.
    ssh_keys: Vec<crate::config::SshKeyEntry>,
    _ssh_keys_sync_sub: Subscription,
```

- [ ] **Step 2: Set up the subscription in `new()`**

In `src/panels/new_connection_window.rs`, find:

```rust
        let key_tab = if conn.as_ref().map(|c| c.auth_method.clone()).as_deref() == Some("key") {
            KeyTab::Saved
        } else {
            KeyTab::ImportNew
        };

        Self {
            panel,
```

Replace with:

```rust
        let key_tab = if conn.as_ref().map(|c| c.auth_method.clone()).as_deref() == Some("key") {
            KeyTab::Saved
        } else {
            KeyTab::ImportNew
        };

        // Re-sync `ssh_keys` whenever `SessionsPanel` changes — needed so
        // adding a key via the empty-state's "open Security & Auth" jump
        // (below) shows up here once the user returns to this still-open
        // form. Mirrors `SecurityAuthPanel::new`'s identical `_sync_sub`.
        let ssh_keys_sync_sub = if let Some(sessions) = panel.upgrade() {
            cx.observe(&sessions, |this, sessions, cx| {
                this.ssh_keys = sessions.read(cx).ssh_keys().to_vec();
                cx.notify();
            })
        } else {
            cx.observe(&cx.entity(), |_, _: Entity<Self>, _| {})
        };

        Self {
            panel,
```

Then find, inside the `Self { ... }` literal:

```rust
            selected_key_id: conn.as_ref().and_then(|c| c.private_key_id.clone()),
            pending_new_key: None,
            ssh_keys,
            key_tab,
```

Replace with:

```rust
            selected_key_id: conn.as_ref().and_then(|c| c.private_key_id.clone()),
            pending_new_key: None,
            ssh_keys,
            _ssh_keys_sync_sub: ssh_keys_sync_sub,
            key_tab,
```

- [ ] **Step 3: Remove the Import-new/Saved toggle from `render_ssh_auth_fields`**

In `src/panels/new_connection_window.rs`, find:

```rust
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.private_key_file_label"), cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(
                                        Self::pill(
                                            "key-tab-import",
                                            rust_i18n::t!("SecurityAuth.tab_import_new_key"),
                                            self.key_tab == KeyTab::ImportNew,
                                            cx,
                                        )
                                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                            this.key_tab = KeyTab::ImportNew;
                                            cx.notify();
                                        })),
                                    )
                                    .child(
                                        Self::pill(
                                            "key-tab-saved",
                                            rust_i18n::t!("SecurityAuth.tab_saved_key"),
                                            self.key_tab == KeyTab::Saved,
                                            cx,
                                        )
                                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                            this.key_tab = KeyTab::Saved;
                                            cx.notify();
                                        })),
                                    ),
                            )
                            .child(if self.key_tab == KeyTab::Saved {
                                self.render_saved_key_picker(cx).into_any_element()
                            } else {
                                self.render_import_new_key(cx).into_any_element()
                            }),
                    )
```

Replace with:

```rust
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.private_key_file_label"), cx))
                            .child(self.render_saved_key_picker(cx)),
                    )
```

(`KeyTab`, `key_tab`, and `render_import_new_key` are no longer referenced by this render path — but leave them defined for now; the next step removes them explicitly so nothing is silently orphaned.)

- [ ] **Step 4: Remove the now-unused `KeyTab` enum, `key_tab` field/init, and `render_import_new_key`**

In `src/panels/new_connection_window.rs`, find:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyTab {
    ImportNew,
    Saved,
}
```

Delete it entirely (no replacement).

Then find:

```rust
    /// Which sub-tab the Key field is on — a pure UI reshuffle of the
    /// existing picker (key auth is always reference-based; this just
    /// splits "import a new one" from "pick an existing one" into two
    /// tabs instead of showing both stacked).
    key_tab: KeyTab,
```

Delete it entirely.

Then find, in `new()`:

```rust
        let key_tab = if conn.as_ref().map(|c| c.auth_method.clone()).as_deref() == Some("key") {
            KeyTab::Saved
        } else {
            KeyTab::ImportNew
        };

        // Re-sync `ssh_keys`
```

Replace with:

```rust
        // Re-sync `ssh_keys`
```

Then find, in the `Self { ... }` literal:

```rust
            _ssh_keys_sync_sub: ssh_keys_sync_sub,
            key_tab,
```

Replace with:

```rust
            _ssh_keys_sync_sub: ssh_keys_sync_sub,
```

Then find and delete the entire `render_import_new_key` method:

```rust
    /// The Key field's "Import new" tab: file-picker button + a pending
    /// (not-yet-saved) indicator. Extracted verbatim from the pre-tab
    /// layout, no logic change.
    fn render_import_new_key(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when_some(self.pending_new_key.clone(), |el, (name, _, _)| {
                el.child(
                    div()
                        .id("pending-new-ssh-key")
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(cx.theme().accent)
                        .child(name),
                )
            })
            .child(
                div()
                    .id("import-new-ssh-key")
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(cx.theme().secondary)
                    .child(rust_i18n::t!("NewConnectionWindow.import_key_button"))
                    .on_click(cx.listener(|_this, _ev: &ClickEvent, window, cx| {
                        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: None,
                        });
                        // See the (removed) "browse private key path"
                        // button this replaced for why `cx.spawn_in` +
                        // `cx.update(|window, cx| ...)` is needed here
                        // rather than a plain `cx.spawn`.
                        cx.spawn_in(window, async move |this, cx| {
                            let Ok(Ok(Some(paths))) = rx.await else {
                                return;
                            };
                            let Some(path) = paths.into_iter().next() else {
                                return;
                            };
                            let Ok(content) = std::fs::read(&path) else {
                                return;
                            };
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "key".to_string());
                            let source_path = path.to_string_lossy().to_string();
                            let _ = cx.update(|_window, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    this.selected_key_id = None;
                                    this.pending_new_key = Some((name, content, source_path));
                                    cx.notify();
                                });
                            });
                        })
                        .detach();
                    })),
            )
    }
```

Delete it entirely.

Note `pending_new_key` and `resolve_key_id` stay — `pending_new_key` becomes permanently `None` in practice now (nothing sets it), but `resolve_key_id` still correctly falls through to `self.selected_key_id.clone()` in that case, so no behavior changes there; removing that field/method pair is unnecessary churn outside this task's scope (YAGNI cuts toward leaving working, harmless code alone here, not toward maximal deletion).

- [ ] **Step 5: Add the empty-state to `render_saved_key_picker`**

In `src/panels/new_connection_window.rs`, find:

```rust
    /// The Key field's "Saved" tab: pick from the vault's shared keys.
    /// Extracted verbatim from the pre-tab layout, no logic change.
    fn render_saved_key_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let keys = self.ssh_keys.clone();
        let current_label: SharedString = self
            .selected_key_id
            .as_ref()
            .and_then(|id| keys.iter().find(|k| &k.id == id))
            .map(|k| SharedString::from(k.name.clone()))
            .unwrap_or_else(|| rust_i18n::t!("NewConnectionWindow.select_button").into());
        div().flex().flex_col().gap_1().child(
            DropdownButton::new("saved-key-picker")
                .small()
                .button(Button::new("saved-key-picker-btn").label(current_label))
                .dropdown_menu(move |menu, _window, _cx| {
                    let mut menu = menu;
                    for k in keys.clone() {
                        let key_id = k.id.clone();
                        let weak = weak.clone();
                        menu = menu.item(PopupMenuItem::new(k.name.clone()).on_click(
                            move |_ev, _window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.selected_key_id = Some(key_id.clone());
                                    this.pending_new_key = None;
                                    cx.notify();
                                });
                            },
                        ));
                    }
                    menu
                }),
        )
    }
```

Replace with:

```rust
    /// The Key field's picker: always the saved-key dropdown now (see the
    /// design's decision to drop the connection form's own "import inline"
    /// convenience — importing now only happens via Security & Auth).
    /// Empty state (no saved keys yet) offers a jump straight there instead
    /// of leaving the user stuck with nothing to pick.
    fn render_saved_key_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.ssh_keys.is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(rust_i18n::t!("NewConnectionWindow.no_saved_keys_hint")),
                )
                .child(
                    Button::new("open-security-auth-for-keys")
                        .xsmall()
                        .label(rust_i18n::t!("NewConnectionWindow.open_security_auth_button"))
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                            let _ = this.panel.update(cx, |panel, cx| panel.open_security_auth_panel(cx));
                        })),
                )
                .into_any_element();
        }

        let weak = cx.entity().downgrade();
        let keys = self.ssh_keys.clone();
        let current_label: SharedString = self
            .selected_key_id
            .as_ref()
            .and_then(|id| keys.iter().find(|k| &k.id == id))
            .map(|k| SharedString::from(k.name.clone()))
            .unwrap_or_else(|| rust_i18n::t!("NewConnectionWindow.select_button").into());
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                DropdownButton::new("saved-key-picker")
                    .small()
                    .button(Button::new("saved-key-picker-btn").label(current_label))
                    .dropdown_menu(move |menu, _window, _cx| {
                        let mut menu = menu;
                        for k in keys.clone() {
                            let key_id = k.id.clone();
                            let weak = weak.clone();
                            menu = menu.item(PopupMenuItem::new(k.name.clone()).on_click(
                                move |_ev, _window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.selected_key_id = Some(key_id.clone());
                                        this.pending_new_key = None;
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .into_any_element()
    }
```

- [ ] **Step 6: Add the two new locale keys**

In `locales/app.yml`, find:

```yaml
  private_key_file_label:
```

(Do not replace this line — search for it only to confirm you're in the `NewConnectionWindow:` namespace, then find a clean insertion point: the entry immediately after it, or any entry within that same namespace block.) Add, anywhere within the `NewConnectionWindow:` namespace block:

```yaml
  no_saved_keys_hint:
    zh-CN: "还没有保存的密钥"
    en: "No saved keys yet"
  open_security_auth_button:
    zh-CN: "前往安全认证添加"
    en: "Add one in Security & Auth"
```

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: succeeds with no errors. If it fails on a leftover `KeyTab`/`key_tab`/`render_import_new_key` reference, use `grep -n "KeyTab\|key_tab\|render_import_new_key" src/panels/new_connection_window.rs` to find and remove it — Steps 3-4 should have caught every one.

- [ ] **Step 8: Manual verification (ask the user)**

Ask the user to:
- Open "新建连接" for SSH, switch auth method to "Key". If they have no saved keys yet, confirm the empty-state message + "前往安全认证添加" button appear, and clicking it switches the main window's left panel to Security & Auth's Keys tab (the connection form window stays open).
- Add a key there, then switch back to the still-open connection form window and confirm the saved-key picker now shows the new key without needing to close/reopen the form.
- Confirm the toolbar's/Security & Auth's own "Import new" flow for adding a key still works exactly as before (untouched by this task).

- [ ] **Step 9: Commit**

```bash
git add src/panels/new_connection_window.rs locales/app.yml
git commit -m "feat: connection form key field is saved-key-picker only

Removes the Import-new/Saved toggle and the connection form's own
inline key-file import UI — adding a key now only happens via Security
& Auth. The picker's empty state jumps there directly and the picker
live-syncs so the user can come straight back to a usable form."
```

---

## Task 5: Remove Security & Auth panel's Passwords tab

**Files:**
- Modify: `src/panels/security_auth.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing new consumed elsewhere (this is the last consumer of `SessionsPanel`'s saved-password CRUD methods and several locale keys — Task 6 cleans those up once this task removes the last call site).

- [ ] **Step 1: Reduce `AuthTab` to just `Keys`**

In `src/panels/security_auth.rs`, find:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthTab {
    Keys,
    Passwords,
}
```

Replace with:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthTab {
    Keys,
}
```

- [ ] **Step 2: Remove the `saved_passwords`/`revealed_password_ids` fields and their sync**

In `src/panels/security_auth.rs`, find:

```rust
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
```

Replace with:

```rust
pub struct SecurityAuthPanel {
    focus_handle: FocusHandle,
    panel: WeakEntity<SessionsPanel>,
    active_tab: AuthTab,
    ssh_keys: Vec<SshKeyEntry>,
    _sync_sub: gpui::Subscription,
}
```

Then find:

```rust
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
```

Replace with:

```rust
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
```

- [ ] **Step 3: Remove the Passwords-tab render/dialog methods**

In `src/panels/security_auth.rs`, find the section comment starting the Passwords tab:

```rust
    // --- Passwords tab ---------------------------------------------------

    fn render_passwords_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
```

...through the end of `confirm_delete_saved_password`:

```rust
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
```

Delete everything from `// --- Passwords tab ---` through `confirm_delete_saved_password`'s closing method body, but **keep** the final `}` that closes `impl SecurityAuthPanel` (i.e. delete the methods themselves, not the impl block's closing brace).

- [ ] **Step 4: Remove the Passwords tab button and dispatch from `Render`**

In `src/panels/security_auth.rs`, find:

```rust
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
```

Replace with:

```rust
                    .child(self.tab_button(AuthTab::Keys, rust_i18n::t!("SecurityAuth.tab_keys").into(), cx)),
            )
            .child(
                div().flex_1().p_2().child(match self.active_tab {
                    AuthTab::Keys => self.render_keys_tab(cx).into_any_element(),
                }),
            )
```

- [ ] **Step 5: Update `tab_button`'s id-matching (drop the now-nonexistent `Passwords` arm) and the module import**

In `src/panels/security_auth.rs`, find:

```rust
            .id(SharedString::from(match tab {
                AuthTab::Keys => "security-auth-tab-keys",
                AuthTab::Passwords => "security-auth-tab-passwords",
            }))
```

Replace with:

```rust
            .id(SharedString::from(match tab {
                AuthTab::Keys => "security-auth-tab-keys",
            }))
```

Then find the top-of-file import:

```rust
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
```

Replace with:

```rust
use gpui::{
    App, AppContext, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, prelude::FluentBuilder, transparent_black,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, IconName, Sizable, WindowExt, v_flex};

use crate::config::SshKeyEntry;
```

(`std::collections::HashSet`, `ClipboardItem`, and `SavedPasswordEntry` were only used by the Passwords tab's reveal-toggle state and clipboard-copy button, both removed in Step 3.)

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: FAIL — `SessionsPanel::connections_using_saved_password`/`remove_saved_password`/`add_saved_password`/`update_saved_password` are now unused (dead_code warnings, not hard errors — this crate's existing convention per `docs/superpowers/plans/2026-07-18-sidebar-resize-fix-and-sftp-improvements.md`'s own precedent tolerates transient dead-code warnings between tasks). Confirm the ONLY new diagnostics are `dead_code` warnings on those four `SessionsPanel` methods, not compile errors. If there are compile errors instead, they indicate a leftover reference to something Step 1-5 should have fully removed — grep `AuthTab::Passwords\|SavedPasswordEntry\|revealed_password_ids` in `src/panels/security_auth.rs` to find it.

- [ ] **Step 7: Manual verification (ask the user)**

Ask the user to open Security & Auth (left activity bar) and confirm only a "Keys" tab shows, with no "Passwords" tab next to it, and that adding/renaming/deleting a key still works exactly as before.

- [ ] **Step 8: Commit**

```bash
git add src/panels/security_auth.rs
git commit -m "feat: remove Security & Auth panel's Passwords tab

Keys tab and its management (add/rename/delete) are untouched. The
underlying SessionsPanel saved-password CRUD methods are now dead code
— removed in the next commit alongside the locale key cleanup."
```

---

## Task 6: Cleanup — dead code and locale keys

**Files:**
- Modify: `src/panels/sessions.rs`
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing (terminal cleanup task).

- [ ] **Step 1: Remove the now-unused `SessionsPanel` saved-password CRUD methods**

In `src/panels/sessions.rs`, find:

```rust
    /// Adds a newly-created saved password. Mirrors `add_ssh_key` —
    /// persistence happens on the next `persist()`-triggering action, not
    /// immediately here.
    pub(crate) fn add_saved_password(&mut self, entry: crate::config::SavedPasswordEntry) {
        self.saved_passwords.push(entry);
    }

    /// Renames/re-encrypts an existing saved password in place. No-op if
    /// `id` doesn't match any entry (e.g. it was deleted concurrently).
    pub(crate) fn update_saved_password(&mut self, id: &str, name: String, encrypted_password: String) {
        if let Some(entry) = self.saved_passwords.iter_mut().find(|p| p.id == id) {
            entry.name = name;
            entry.encrypted_password = encrypted_password;
        }
    }

    /// Removes a saved password. Same dangling-reference contract as
    /// `remove_ssh_key`.
    pub(crate) fn remove_saved_password(&mut self, id: &str) {
        self.saved_passwords.retain(|p| p.id != id);
    }

    /// Connection display names currently referencing a given saved key,
```

Replace with:

```rust
    /// Connection display names currently referencing a given saved key,
```

(This deletes `add_saved_password`/`update_saved_password`/`remove_saved_password`, keeping the doc comment that introduces the next surviving method, `connections_using_ssh_key`, intact.)

Then find:

```rust
    /// Connection display names currently referencing a given saved key,
    /// for the delete-confirm dialog's "used by N connections" warning.
    pub(crate) fn connections_using_ssh_key(&self, id: &str) -> Vec<String> {
        self.connections
            .iter()
            .filter(|c| c.private_key_id.as_deref() == Some(id))
            .map(|c| c.display_name())
            .collect()
    }
```

Leave this method as-is (it's `connections_using_ssh_key`, still used by `security_auth.rs`'s Keys tab — do not confuse with `connections_using_saved_password` below, which IS being removed).

Then find and delete `connections_using_saved_password` itself (it comes after `open_security_auth_panel`/`migrate_saved_passwords_to_direct`, which Task 2 added right after `connections_using_ssh_key` — so it's now a few lines further down than in the original file):

```rust
    /// Connection display names currently referencing a given saved
    /// password, for the delete-confirm dialog's "used by N connections"
    /// warning.
    pub(crate) fn connections_using_saved_password(&self, id: &str) -> Vec<String> {
        self.connections
            .iter()
            .filter(|c| c.password_id.as_deref() == Some(id))
            .map(|c| c.display_name())
            .collect()
    }
```

Delete it entirely.

- [ ] **Step 2: Build to confirm the dead-code warnings are gone**

Run: `cargo build`
Expected: succeeds with no errors and no `dead_code` warnings about the four methods removed across this task and the previous one.

- [ ] **Step 3: Remove the now-dead locale keys**

In `locales/app.yml`, under the `SecurityAuth:` namespace, remove these entries entirely (each is a 3-line block: key, `zh-CN:`, `en:`):

```yaml
  tab_passwords:
    zh-CN: "密码"
    en: "Passwords"
```

```yaml
  delete_password_title:
    zh-CN: "删除密码？"
    en: "Delete password?"
```

```yaml
  add_password_button:
    zh-CN: "添加密码"
    en: "Add password"
```

```yaml
  add_password_title:
    zh-CN: "添加已保存密码"
    en: "Add saved password"
```

```yaml
  edit_button:
    zh-CN: "编辑"
    en: "Edit"
```

```yaml
  edit_password_title:
    zh-CN: "编辑已保存密码"
    en: "Edit saved password"
```

```yaml
  password_name_placeholder:
    zh-CN: "名称"
    en: "Name"
```

```yaml
  tab_direct_password:
    zh-CN: "直接输入"
    en: "Direct"
```

```yaml
  tab_saved_password:
    zh-CN: "已保存"
    en: "Saved"
```

```yaml
  tab_import_new_key:
    zh-CN: "导入新密钥"
    en: "Import new"
```

```yaml
  add_new_saved_password_row:
    zh-CN: "+ 添加密码..."
    en: "+ Add password..."
```

Keep everything else in the `SecurityAuth:` namespace unchanged, in particular `tab_keys`, `add_key_button`, `add_key_title`, `key_name_placeholder`, `no_file_picked`, `rename_button`, `rename_key_title`, `delete_key_title`, `delete_confirm_body`, `delete_confirm_body_in_use`, and `tab_saved_key` (all still used by the Keys tab and/or the connection form's saved-key picker label).

**Verify before deleting `edit_button` and `password_name_placeholder` in particular** — these two were mistakenly assumed "shared with Keys" in the original design spec, but grepping actual usage sites confirms both are exclusively used by the now-removed Passwords tab code (Keys tab uses `rename_button`, not `edit_button`; key-name input uses the separate `key_name_placeholder` key, not `password_name_placeholder`). Run:

```bash
grep -rn "SecurityAuth.edit_button\|SecurityAuth.password_name_placeholder" src/
```

Expected: no matches (confirms it's safe that Step 3 removes both).

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --bin caracal`
Expected: all tests pass (including the 3 migration tests from Task 1 and every pre-existing test — `to_ssh_config_resolves_a_saved_password_by_id`, `import_merge_remaps_password_id_to_the_dest_vaults_entry`, etc. all stay green since `to_ssh_config`/`import_merge` are intentionally untouched).

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: succeeds with no errors or warnings related to this feature area.

- [ ] **Step 6: Commit**

```bash
git add src/panels/sessions.rs locales/app.yml
git commit -m "chore: remove dead saved-password CRUD methods and locale keys

SessionsPanel::add_saved_password/update_saved_password/
remove_saved_password/connections_using_saved_password had no callers
left after removing the Passwords tab and the connection form's saved-
password picker. Also removes edit_button and password_name_placeholder,
which were incorrectly assumed shared with the Keys tab in the original
design — verified via grep that Keys uses rename_button and
key_name_placeholder instead."
```
