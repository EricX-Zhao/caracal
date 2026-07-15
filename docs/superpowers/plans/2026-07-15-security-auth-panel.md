# 安全认证 (Security & Auth) Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement caracal's existing-but-unimplemented `PanelId::Security` stub as a real panel for managing shared SSH keys and a new shared "saved passwords" store, and let the new-connection form pick a saved password (not just type one directly) the same way it already picks a saved key.

**Architecture:** A new `SavedPasswordEntry` type (parallel to the existing `SshKeyEntry`) is added to `AppConfig`; `SavedConnection` gains a `password_id: Option<String>` reference field (parallel to `private_key_id`) so a connection can either type a password directly (`encrypted_password`, unchanged) or reference a shared entry. `SessionsPanel` stays the single owner/persister of all of this (connections, groups, ssh_keys, saved_passwords), exactly as it already is for ssh_keys. A new `SecurityAuthPanel` and the existing `NewConnectionWindow` both read/write through it via a `WeakEntity<SessionsPanel>`, the same pattern `NewConnectionWindow` already uses.

**Tech Stack:** Rust, existing `gpui`/`gpui-component` (`Input::mask_toggle()`, `IconName::{Eye,EyeOff,Copy,Delete}`, `window.open_alert_dialog`), no new dependencies.

**Reference:** `docs/superpowers/specs/2026-07-15-security-auth-panel-design.md` (design spec — read this first for the *why* behind every decision below). Builds on [[encrypted-credential-storage-feature]]'s existing `ssh_keys`/vault infrastructure.

## Global Constraints

- **caracal is a single binary crate** — every task must leave `cargo build`/`cargo test` fully green. Adding a field to `SavedConnection` requires fixing every struct-literal construction site *in the same task*, not a later one (learned the hard way in the previous encrypted-credential-storage plan).
- **`SessionsPanel` is the single source of truth** for `connections`/`groups`/`ssh_keys`/`saved_passwords` and the only thing that calls `persist()`. Both `NewConnectionWindow` and the new `SecurityAuthPanel` hold a `WeakEntity<SessionsPanel>` and route every mutation through it — never hold a separately-persisted copy.
- **IDs** reuse `config::generate_id()` (the existing `id-<nanos>` scheme) — no new dependency.
- A referenced-but-deleted `password_id`/`private_key_id` must fail *cleanly* (a normal `Err` at connect time, a "nothing selected" state when reopening the edit form) — never a panic.
- Every new user-facing string goes through the existing `rust_i18n::t!("Key")` convention, with entries added to `locales/app.yml` (`zh-CN` + `en`).
- Reuse `gpui-component`'s built-in `Input::new(&state).mask_toggle()` for every password-type input this feature adds or touches — do not hand-build a custom reveal-toggle.

---

### Task 1: Data model — `SavedPasswordEntry`, `password_id`, and `SessionsPanel` storage

**Why this is one task, not several:** adding `SavedConnection.password_id` breaks every existing `SavedConnection { ... }` struct literal in the crate at once (Rust struct literals require every field). Splitting "add the field" from "fix the call sites" would leave the tree non-compiling in between, which the single-binary-crate constraint above rules out.

**Files:**
- Modify: `src/config.rs` (new type, new fields, `to_ssh_config` signature)
- Modify: `src/vault.rs` (`reset`, `import_merge` gain parallel `saved_passwords` handling)
- Modify: `src/panels/sessions.rs` (`SessionsPanel` storage + mutation methods, `duplicate`, `persist`, `import_connections`)
- Modify: `src/panels/new_connection_window.rs` (struct-literal fix only — 4 sites, all `password_id: None` for now; the real Direct/Saved UI is Task 3)
- Modify: `src/workspace.rs` (`to_ssh_config` call site, new `saved_passwords_snapshot` accessor, `SessionsPanel::new` call site)

**Interfaces:**
- Produces: `config::SavedPasswordEntry { id: String, name: String, encrypted_password: String }`; `AppConfig.saved_passwords: Vec<SavedPasswordEntry>`; `SavedConnection.password_id: Option<String>`; `SavedConnection::to_ssh_config(&self, ssh_keys: &[SshKeyEntry], saved_passwords: &[SavedPasswordEntry], master_key: &crate::crypto::MasterKey) -> anyhow::Result<SshConfig>` (signature change: new 2nd parameter); `SessionsPanel::saved_passwords(&self) -> &[SavedPasswordEntry]`, `add_saved_password(&mut self, entry: SavedPasswordEntry)`, `update_saved_password(&mut self, id: &str, name: String, encrypted_password: String)`, `remove_saved_password(&mut self, id: &str)`, `update_ssh_key(&mut self, id: &str, name: String)`, `remove_ssh_key(&mut self, id: &str)`.
- Consumed by: Task 2 (`SecurityAuthPanel`), Task 3 (connection form).

- [ ] **Step 1: Add `SavedPasswordEntry` and the new fields to `config.rs`**

In `src/config.rs`, add this new struct right after the existing `SshKeyEntry` definition (currently ends around line 361, right before `pub struct AppConfig`):

```rust
/// A named, shared password. Connections reference one by `id` (like
/// `SshKeyEntry` already works) instead of only ever embedding their own —
/// so rotating a password shared across many servers updates every
/// connection that uses it, in one place.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedPasswordEntry {
    pub id: String,
    pub name: String,
    /// `base64(nonce || ciphertext)`, via `crypto::MasterKey::encrypt_str`.
    pub encrypted_password: String,
}
```

Add `saved_passwords` to `AppConfig` (currently `connections`, `groups`, `vault`, `ssh_keys`):

```rust
/// The whole persisted config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
    #[serde(default)]
    pub groups: Vec<SavedConnectionGroup>,
    /// `None` until the vault is set up (see `vault::migrate`).
    #[serde(default)]
    pub vault: Option<VaultMeta>,
    #[serde(default)]
    pub ssh_keys: Vec<SshKeyEntry>,
    #[serde(default)]
    pub saved_passwords: Vec<SavedPasswordEntry>,
}
```

Add `password_id` to `SavedConnection`, right after the existing `private_key_id` field:

```rust
    /// References an `AppConfig.ssh_keys` entry by id. Only meaningful when
    /// `auth_method == "key"`.
    #[serde(default)]
    pub private_key_id: Option<String>,
    /// References an `AppConfig.saved_passwords` entry by id — the
    /// connection form's "Saved" password tab. `None` means "Direct" mode:
    /// use `encrypted_password` instead. Only meaningful when
    /// `auth_method == "password"`.
    #[serde(default)]
    pub password_id: Option<String>,
```

- [ ] **Step 2: Rewrite `to_ssh_config` to resolve a saved password**

Replace the password branch of `to_ssh_config` (currently `src/config.rs`, the `else` arm around line 178-180):

```rust
    pub fn to_ssh_config(
        &self,
        ssh_keys: &[SshKeyEntry],
        master_key: &crate::crypto::MasterKey,
    ) -> anyhow::Result<SshConfig> {
        let auth = if self.auth_method == "key" {
            ...
        } else {
            let password = master_key.decrypt_str(&self.encrypted_password)?;
            SshAuth::Password(password)
        };
```

with:

```rust
    pub fn to_ssh_config(
        &self,
        ssh_keys: &[SshKeyEntry],
        saved_passwords: &[SavedPasswordEntry],
        master_key: &crate::crypto::MasterKey,
    ) -> anyhow::Result<SshConfig> {
        let auth = if self.auth_method == "key" {
            ...
        } else if let Some(password_id) = &self.password_id {
            let entry = saved_passwords
                .iter()
                .find(|p| &p.id == password_id)
                .ok_or_else(|| anyhow::anyhow!("the saved password this connection uses was not found"))?;
            let password = master_key.decrypt_str(&entry.encrypted_password)?;
            SshAuth::Password(password)
        } else {
            let password = master_key.decrypt_str(&self.encrypted_password)?;
            SshAuth::Password(password)
        };
```

(the `if self.auth_method == "key" { ... }` arm is unchanged — leave its body exactly as it is, only the `else` tail changes.)

- [ ] **Step 3: Fix the two `SavedConnection` test fixtures**

In `src/config.rs`'s `#[cfg(test)] mod tests`, `base_connection` (around line 425) has a `private_key_id: None,` line — add right after it:

```rust
            private_key_id: None,
            password_id: None,
```

In `src/vault.rs`'s `#[cfg(test)] mod tests`, `base_connection` (around line 183) has the same `private_key_id: None,` line — add the identical `password_id: None,` line right after it.

- [ ] **Step 4: Fix `to_ssh_config`'s 3 existing call sites in `config.rs`'s own tests**

`to_ssh_config_uses_password_auth_by_default` (around line 533-539): change `conn.to_ssh_config(&[], &master)` to `conn.to_ssh_config(&[], &[], &master)`.

`to_ssh_config_uses_private_key_auth_when_selected` (around line 542-562): change `conn.to_ssh_config(&ssh_keys, &master)` to `conn.to_ssh_config(&ssh_keys, &[], &master)`.

`to_ssh_config_errors_when_referenced_key_is_missing` (around line 565-571): change `conn.to_ssh_config(&[], &master)` to `conn.to_ssh_config(&[], &[], &master)`.

- [ ] **Step 5: Add a new test for saved-password resolution**

Add to `config.rs`'s test module, after `to_ssh_config_errors_when_referenced_key_is_missing`:

```rust
    #[test]
    fn to_ssh_config_resolves_a_saved_password_by_id() {
        let master = crate::crypto::MasterKey::generate();
        let saved = vec![SavedPasswordEntry {
            id: "pw-1".to_string(),
            name: "shared root password".to_string(),
            encrypted_password: master.encrypt_str("hunter2"),
        }];
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.password_id = Some("pw-1".to_string());
        let cfg = conn.to_ssh_config(&[], &saved, &master).unwrap();
        assert!(matches!(cfg.auth, crate::terminal::ssh::SshAuth::Password(p) if p == "hunter2"));
    }

    #[test]
    fn to_ssh_config_errors_when_referenced_password_is_missing() {
        let master = crate::crypto::MasterKey::generate();
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.password_id = Some("does-not-exist".to_string());
        assert!(conn.to_ssh_config(&[], &[], &master).is_err());
    }
```

- [ ] **Step 6: `SessionsPanel` gains `saved_passwords` storage**

In `src/panels/sessions.rs`, add the field right after `ssh_keys: Vec<crate::config::SshKeyEntry>,` in the `SessionsPanel` struct (currently around line 223):

```rust
    ssh_keys: Vec<crate::config::SshKeyEntry>,
    saved_passwords: Vec<crate::config::SavedPasswordEntry>,
```

Add the parameter to `SessionsPanel::new` (currently `connections, groups, vault, ssh_keys, window, cx`):

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

and add `saved_passwords,` to the `Self { ... }` literal alongside the existing `ssh_keys,` line.

- [ ] **Step 7: `SessionsPanel` mutation + accessor methods**

Add these right after the existing `add_ssh_key` method (currently ends around line 871):

```rust
    pub(crate) fn saved_passwords(&self) -> &[crate::config::SavedPasswordEntry] {
        &self.saved_passwords
    }

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

    /// Removes a saved password. Any connection whose `password_id`
    /// referenced it is left with a dangling reference — `to_ssh_config`
    /// already fails cleanly for that case, and the connection form
    /// already handles reopening a dangling reference gracefully (Task 3).
    pub(crate) fn remove_saved_password(&mut self, id: &str) {
        self.saved_passwords.retain(|p| p.id != id);
    }

    /// Renames an existing SSH key in place (content is never edited in
    /// place — replacing content means importing a new key via
    /// `add_ssh_key` again). No-op if `id` doesn't match any entry.
    pub(crate) fn update_ssh_key(&mut self, id: &str, name: String) {
        if let Some(entry) = self.ssh_keys.iter_mut().find(|k| k.id == id) {
            entry.name = name;
        }
    }

    /// Removes an SSH key. Same dangling-reference contract as
    /// `remove_saved_password`.
    pub(crate) fn remove_ssh_key(&mut self, id: &str) {
        self.ssh_keys.retain(|k| k.id != id);
    }

    /// Connection ids/names currently referencing a given saved key or
    /// password, for the delete-confirm dialog's "used by N connections"
    /// warning (Task 2).
    pub(crate) fn connections_using_ssh_key(&self, id: &str) -> Vec<String> {
        self.connections
            .iter()
            .filter(|c| c.private_key_id.as_deref() == Some(id))
            .map(|c| c.display_name())
            .collect()
    }

    pub(crate) fn connections_using_saved_password(&self, id: &str) -> Vec<String> {
        self.connections
            .iter()
            .filter(|c| c.password_id.as_deref() == Some(id))
            .map(|c| c.display_name())
            .collect()
    }
```

- [ ] **Step 8: Update `persist()`, `import_connections`, and `duplicate()`**

In `persist()` (currently around line 849-859), add `saved_passwords: self.saved_passwords.clone(),` to the `AppConfig` literal, alongside the existing `ssh_keys: self.ssh_keys.clone(),`. Also fix its doc comment, which is stale (claims data is "read back from the on-disk config", which stopped being true once `ssh_keys` became real `SessionsPanel` state) — replace the comment block directly above `fn persist(&self)` with:

```rust
    /// Persist the current state to disk.
    fn persist(&self) {
```

In `import_connections`'s merge closure (currently around line 993-1014), add `saved_passwords: this.saved_passwords.clone(),` to the `dest` literal (alongside `ssh_keys: this.ssh_keys.clone(),`), and `this.saved_passwords = dest.saved_passwords;` to the assignment-back block (alongside `this.ssh_keys = dest.ssh_keys;`).

In `duplicate()` (currently around line 535-556), add `new_conn.password_id = None;` right after the existing `new_conn.private_key_id = None;` line, so a duplicated connection never silently inherits a saved-password reference either.

- [ ] **Step 9: `vault::reset` and `vault::import_merge` handle `saved_passwords`**

In `src/vault.rs`, update `reset` (currently clears `vault`, `ssh_keys`, and per-connection secret fields):

```rust
pub fn reset(cfg: &mut AppConfig) {
    cfg.vault = None;
    cfg.ssh_keys.clear();
    cfg.saved_passwords.clear();
    for conn in &mut cfg.connections {
        conn.encrypted_password.clear();
        conn.encrypted_key_passphrase = None;
        conn.private_key_id = None;
        conn.password_id = None;
    }
}
```

Update `import_merge` to dedup+remap `saved_passwords` the same way it already does for `ssh_keys`. Insert this block right after the existing `ssh_keys` dedup block (after `source_id_to_dest_id` is fully populated for keys, before `dest.groups.extend(...)`):

```rust
    let mut dest_password_hash_to_id: HashMap<String, String> = HashMap::new();
    for pw in &dest.saved_passwords {
        let plaintext = dest_key.decrypt_str(&pw.encrypted_password)?;
        dest_password_hash_to_id.insert(content_hash(plaintext.as_bytes()), pw.id.clone());
    }

    let mut source_password_id_to_dest_id: HashMap<String, String> = HashMap::new();
    for pw in &source.saved_passwords {
        let plaintext = source_key.decrypt_str(&pw.encrypted_password)?;
        let hash = content_hash(plaintext.as_bytes());
        let dest_id = match dest_password_hash_to_id.get(&hash) {
            Some(existing) => existing.clone(),
            None => {
                let new_id = generate_id();
                dest.saved_passwords.push(SavedPasswordEntry {
                    id: new_id.clone(),
                    name: pw.name.clone(),
                    encrypted_password: dest_key.encrypt_str(&plaintext),
                });
                dest_password_hash_to_id.insert(hash, new_id.clone());
                new_id
            }
        };
        source_password_id_to_dest_id.insert(pw.id.clone(), dest_id);
    }
```

and add `SavedPasswordEntry` to the `use crate::config::{...}` import at the top of `vault.rs`.

Then, in the `for conn in &source.connections` loop, add the remap right after the existing `private_key_id` remap:

```rust
        if let Some(source_key_id) = &conn.private_key_id {
            conn.private_key_id = source_id_to_dest_id.get(source_key_id).cloned();
        }
        if let Some(source_password_id) = &conn.password_id {
            conn.password_id = source_password_id_to_dest_id.get(source_password_id).cloned();
        }
```

- [ ] **Step 10: Add tests for the new `vault.rs` behavior**

Add to `vault.rs`'s test module:

```rust
    #[test]
    fn reset_clears_saved_passwords_and_password_id() {
        let mut cfg = AppConfig { connections: vec![base_connection("a.example.com")], ..Default::default() };
        let master = migrate(&mut cfg, "pw").unwrap();
        cfg.saved_passwords.push(SavedPasswordEntry {
            id: "pw-1".to_string(),
            name: "shared".to_string(),
            encrypted_password: master.encrypt_str("hunter2"),
        });
        cfg.connections[0].password_id = Some("pw-1".to_string());
        reset(&mut cfg);
        assert!(cfg.saved_passwords.is_empty());
        assert!(cfg.connections[0].password_id.is_none());
    }

    #[test]
    fn import_merge_dedups_saved_passwords_by_content_on_repeated_import() {
        let mut dest = AppConfig::default();
        let dest_key = migrate(&mut dest, "dest-pw").unwrap();

        let mut source = AppConfig::default();
        let source_key = MasterKey::generate();
        source.saved_passwords.push(SavedPasswordEntry {
            id: "src-pw-1".to_string(),
            name: "shared root password".to_string(),
            encrypted_password: source_key.encrypt_str("same-password"),
        });

        import_merge(&mut dest, &dest_key, &source, &source_key).unwrap();
        import_merge(&mut dest, &dest_key, &source, &source_key).unwrap();

        assert_eq!(dest.saved_passwords.len(), 1, "importing the same password twice must not duplicate it");
    }

    #[test]
    fn import_merge_remaps_password_id_to_the_dest_vaults_entry() {
        let mut dest = AppConfig::default();
        let dest_key = migrate(&mut dest, "dest-pw").unwrap();

        let mut source = AppConfig::default();
        let source_key = MasterKey::generate();
        source.saved_passwords.push(SavedPasswordEntry {
            id: "src-pw-1".to_string(),
            name: "shared".to_string(),
            encrypted_password: source_key.encrypt_str("hunter2"),
        });
        let mut conn = base_connection("imported.example.com");
        conn.auth_method = "password".to_string();
        conn.password_id = Some("src-pw-1".to_string());
        conn.encrypted_password = source_key.encrypt_str(""); // unused in Saved mode
        source.connections.push(conn);

        import_merge(&mut dest, &dest_key, &source, &source_key).unwrap();

        let imported_conn = &dest.connections[0];
        let new_id = imported_conn.password_id.as_ref().expect("password_id should remap, not clear");
        let entry = dest.saved_passwords.iter().find(|p| &p.id == new_id).unwrap();
        assert_eq!(dest_key.decrypt_str(&entry.encrypted_password).unwrap(), "hunter2");
    }
```

- [ ] **Step 11: Fix `new_connection_window.rs`'s 4 `SavedConnection` literals**

In `src/panels/new_connection_window.rs`'s `save()`, all 4 branches of `match self.conn_type` already end their `SavedConnection { ... }` literal with a `private_key_id:` line — confirmed at lines 280 (SSH: `private_key_id: key_id,`), 310, 342, and 376 (Local/Telnet/Serial: `private_key_id: None,` each). Add `password_id: None,` immediately after each of the 3 identical `private_key_id: None,` occurrences:

```rust
                    private_key_id: None,
```
→
```rust
                    private_key_id: None,
                    password_id: None,
```

(this text is identical at lines 310, 342, and 376 — use `replace_all` if your editor supports it, or fix each of the 3 individually; either way, do **not** touch the SSH branch's `private_key_id: key_id,` at line 280 the same way.)

For the SSH branch specifically (line 280), add `password_id: None,` after it too — this task always writes `None` here since the real Direct/Saved logic doesn't exist until Task 3:

```rust
                    private_key_id: key_id,
```
→
```rust
                    private_key_id: key_id,
                    password_id: None,
```

- [ ] **Step 12: Fix `workspace.rs`'s `to_ssh_config` call site and add `saved_passwords_snapshot`**

Add a new accessor right after the existing `ssh_keys_snapshot` (currently around line 553-555):

```rust
    fn ssh_keys_snapshot(&self, cx: &App) -> Vec<crate::config::SshKeyEntry> {
        self.saved_sessions.read(cx).ssh_keys().to_vec()
    }

    fn saved_passwords_snapshot(&self, cx: &App) -> Vec<crate::config::SavedPasswordEntry> {
        self.saved_sessions.read(cx).saved_passwords().to_vec()
    }
```

Update the `SessionsEvent::Open` handler (currently around line 381-383):

```rust
                    let ssh_keys = this.ssh_keys_snapshot(cx);
                    match conn.to_ssh_config(&ssh_keys, &vault.0) {
```
→
```rust
                    let ssh_keys = this.ssh_keys_snapshot(cx);
                    let saved_passwords = this.saved_passwords_snapshot(cx);
                    match conn.to_ssh_config(&ssh_keys, &saved_passwords, &vault.0) {
```

Update `SessionsPanel::new`'s one call site (currently around line 356-358 inside `Workspace::new`):

```rust
        let saved = cx.new(|cx| {
            SessionsPanel::new(cfg.connections, cfg.groups, cfg.vault, cfg.ssh_keys, window, cx)
        });
```
→
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

Also fix `open_new_connection_window`'s call to `NewConnectionWindow::new` in `sessions.rs` (currently passes `ssh_keys.clone()` as the 5th arg, around line 431-449) — Task 3 adds the `saved_passwords` parameter to `NewConnectionWindow::new` itself, so this step only needs the snapshot captured here for Task 3 to consume: add `let saved_passwords = self.saved_passwords.clone();` right after the existing `let ssh_keys = self.ssh_keys.clone();` line — the actual `NewConnectionWindow::new(...)` call gets the new argument added in Task 3, once that constructor accepts it. Leave the call itself unchanged in this task (adding an unused captured variable produces a warning, not an error — acceptable and expected until Task 3, consistent with this plan's established pattern for foundation work).

- [ ] **Step 13: Build and test**

Run: `cargo build`
Expected: succeeds. Fix any remaining `SavedConnection { ... }`/`to_ssh_config(...)` call sites the compiler flags that this plan's steps didn't explicitly enumerate.

Run: `cargo test`
Expected: full suite passes, including the new tests from Steps 5 and 10.

- [ ] **Step 14: Commit**

```bash
git add src/config.rs src/vault.rs src/panels/sessions.rs src/panels/new_connection_window.rs src/workspace.rs
git commit -m "feat: add shared saved-password store (SavedPasswordEntry, password_id)"
```

---

### Task 2: `SecurityAuthPanel` — the 安全认证 panel itself

**Files:**
- Create: `src/panels/security_auth.rs`
- Modify: `src/panels/activity_bar.rs` (nothing structural — `PanelId::Security` already exists; this task's i18n label change happens in `locales/app.yml`)
- Modify: `src/workspace.rs` (replace the `StubPanel` wiring for `PanelId::Security` with the real panel)
- Modify: `src/main.rs` (register the new module)
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: `SessionsPanel::{ssh_keys, saved_passwords, add_ssh_key, update_ssh_key, remove_ssh_key, add_saved_password, update_saved_password, remove_saved_password, connections_using_ssh_key, connections_using_saved_password}` (Task 1).
- Produces: `SecurityAuthPanel` (an `Entity`-wrapped `Focusable + Render`, same minimal shape as `StubPanel` — no `Panel`/`PanelEvent` trait needed, matching `StubPanel`'s own precedent for a side-region content view).

- [ ] **Step 1: Scaffold `SecurityAuthPanel` with a Keys/Passwords tab bar**

Create `src/panels/security_auth.rs`:

```rust
//! `SecurityAuthPanel`: the 安全认证 left-sidebar panel for managing the
//! vault's shared SSH keys and saved passwords (see
//! docs/superpowers/specs/2026-07-15-security-auth-panel-design.md).
//! Reads/writes through a `WeakEntity<SessionsPanel>` — `SessionsPanel`
//! stays the single owner/persister of this data, exactly as
//! `NewConnectionWindow` already treats it.

use std::collections::HashSet;

use gpui::{
    App, ClickEvent, ClipboardItem, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, WeakEntity, Window, div,
    prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, IconName, Sizable, WindowExt};

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
    /// toggled per-row by the eye icon (see the design spec's "reveal
    /// toggle" decision).
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
            // No live `SessionsPanel` to observe — degrade to a static
            // empty view rather than panicking; this shouldn't happen in
            // practice (the panel is always constructed with a live one).
            cx.observe(&cx.entity(), |_, _, _| {})
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
            .bg(if active { cx.theme().list_active } else { gpui::transparent_black() })
            .text_color(if active { cx.theme().foreground } else { cx.theme().muted_foreground })
            .hover(|s| s.bg(cx.theme().accent))
            .child(label)
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.active_tab = tab;
                cx.notify();
            }))
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
```

- [ ] **Step 2: Run a build to confirm the scaffold compiles (with the two `render_*_tab` methods stubbed)**

Add temporary stub methods so Step 1 compiles standalone:

```rust
impl SecurityAuthPanel {
    fn render_keys_tab(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }

    fn render_passwords_tab(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
```

Add `mod security_auth;` to `src/main.rs`'s `mod` block (alphabetical — actually this lives under `src/panels/`, so add it to `src/panels/mod.rs`'s own `pub mod` list instead; check that file for the exact existing pattern other panel modules use, e.g. `pub mod sftp;`, and add `pub mod security_auth;` alongside them, keeping alphabetical order).

Run: `cargo build`
Expected: succeeds (unused-field warnings on `ssh_keys`/`saved_passwords`/`revealed_password_ids` are expected until Steps 3-4 replace the stub bodies).

- [ ] **Step 3: Implement the Keys tab**

Replace the `render_keys_tab` stub:

```rust
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
                        this.open_add_key_dialog(window, cx);
                    })),
            )
    }
```

Add the three new methods (rename dialog, delete-confirm-with-usage-count, add dialog), right after `render_keys_tab`:

```rust
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

    fn open_add_key_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("SecurityAuth.key_name_placeholder"))
        });
        let passphrase_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(rust_i18n::t!("NewConnectionWindow.private_key_passphrase_placeholder"))
        });
        let picked_file: Entity<Option<(Vec<u8>, String)>> = cx.new(|_cx| None); // (content, source_path)
        let panel = self.panel.clone();
        window.open_alert_dialog(cx, move |alert, _window, cx| {
            let name_input = name_input.clone();
            let passphrase_input = passphrase_input.clone();
            let picked_file = picked_file.clone();
            let panel = panel.clone();
            let picked_label = picked_file
                .read(cx)
                .as_ref()
                .map(|(_, path)| path.clone())
                .unwrap_or_else(|| rust_i18n::t!("SecurityAuth.no_file_picked").to_string());
            let body = gpui_component::v_flex()
                .gap_2()
                .child(Input::new(&name_input))
                .child(
                    Button::new("add-key-pick-file")
                        .xsmall()
                        .label(rust_i18n::t!("NewConnectionWindow.import_key_button"))
                        .on_click({
                            let picked_file = picked_file.clone();
                            cx.listener_for(&picked_file, move |_this, _ev: &ClickEvent, window, cx| {
                                // placeholder — see note below
                                let _ = window;
                                let _ = cx;
                            })
                        }),
                )
                .child(div().text_xs().child(picked_label))
                .child(Input::new(&passphrase_input).mask_toggle());
            alert
                .title(rust_i18n::t!("SecurityAuth.add_key_title"))
                .description(body)
                .confirm()
                .on_ok(move |_, window, cx| {
                    let Some((content, source_path)) = picked_file.read(cx).clone() else {
                        return false; // nothing picked yet — keep the dialog open
                    };
                    let name = name_input.read(cx).value().trim().to_string();
                    let name = if name.is_empty() {
                        std::path::Path::new(&source_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or(source_path.clone())
                    } else {
                        name
                    };
                    let Some(vault) = cx.try_global::<crate::workspace::VaultKey>() else {
                        return false;
                    };
                    let entry = crate::config::SshKeyEntry {
                        id: crate::config::generate_id(),
                        name,
                        source_path: Some(source_path),
                        encrypted_content: vault.0.encrypt_bytes(&content),
                    };
                    let _ = panel.update(cx, |p, _cx| p.add_ssh_key(entry));
                    let _ = panel.update(cx, |p, _cx| p.persist_for_security_auth());
                    window.close_dialog(cx);
                    true
                })
        });
    }
```

**Note on the file-picker button inside `open_add_key_dialog`**: the placeholder
`cx.listener_for(&picked_file, ...)` above does not compile as written —
`cx.listener_for` isn't a real gpui API; this is flagged deliberately rather
than guessed. Replace it with the same `cx.prompt_for_paths` +
`cx.spawn_in` + `cx.update(|window, cx| { picked_file.update(cx, |slot, cx| { *slot = Some((content, source_path)); cx.notify(); }) })`
pattern already used (and proven) in `new_connection_window.rs`'s
"Import key file..." button (`src/panels/new_connection_window.rs`,
the `on_click` inside the Key field's Saved/Import-new picker) — copy
that exact `cx.spawn_in(window, async move |_this, cx| { ... })` shape,
substituting a write to the `picked_file: Entity<Option<(Vec<u8>, String)>>`
entity (via `cx.update(|window, cx| picked_file.update(cx, |slot, cx| ...))`)
for what that other call site does to `this.pending_new_key`. This is the
one spot in this plan where copying an existing, already-verified pattern
from this same codebase — rather than writing fresh gpui async-closure
code from research alone — is the right move; do not re-derive it.

- [ ] **Step 4: Implement the Passwords tab**

Replace the `render_passwords_tab` stub:

```rust
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
            let id_for_copy = id.clone();
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
                                        this.open_edit_password_dialog(id_for_edit.clone(), window, cx);
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

    fn open_edit_password_dialog(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_add_or_edit_password_dialog(Some(id), window, cx);
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
            let body = gpui_component::v_flex()
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
```

- [ ] **Step 5: Add `SessionsPanel::persist_for_security_auth`**

The dialogs above call `panel.update(cx, |p, _cx| p.persist_for_security_auth())` after every mutation — `SessionsPanel::persist` is already `fn persist(&self)` (private). Add a tiny `pub(crate)` wrapper in `src/panels/sessions.rs`, right after the existing `persist` method:

```rust
    /// Exposes `persist()` to callers outside this module (the
    /// `SecurityAuthPanel`'s add/edit/delete dialogs) — those mutate
    /// `ssh_keys`/`saved_passwords` via the `update_*`/`remove_*`/`add_*`
    /// methods above, which intentionally don't persist themselves (same
    /// rationale as `add_ssh_key`'s existing doc comment), so the caller
    /// must persist explicitly once done.
    pub(crate) fn persist_for_security_auth(&self) {
        self.persist();
    }
```

- [ ] **Step 6: Wire `SecurityAuthPanel` into `workspace.rs`, replacing the stub**

Replace the stub-panel loop (currently `src/workspace.rs` around line 413-416):

```rust
        // One stub panel per not-yet-implemented category.
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [PanelId::Network, PanelId::Security, PanelId::History] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid, cx)).into();
            stub_panels.insert(pid, view);
        }
```

with:

```rust
        // One stub panel per not-yet-implemented category. Security has a
        // real panel now (see `resolve`'s dedicated match arm below).
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [PanelId::Network, PanelId::History] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid, cx)).into();
            stub_panels.insert(pid, view);
        }
        let security_auth_panel: AnyView =
            cx.new(|cx| SecurityAuthPanel::new(saved.downgrade(), cx)).into();
```

(`saved` is the `Entity<SessionsPanel>` local variable already in scope at this point in `Workspace::new` — same one `saved_sessions: saved.clone()` uses a few lines later.)

Add a new field to the `Workspace` struct, right after `sessions_panel`/`saved_sessions` (currently around line 257-258):

```rust
    security_auth_panel: AnyView,
```

Add `security_auth_panel,` to the `Self { ... }` literal (alongside `sessions_panel: saved.into(),`).

Update `resolve` (currently around line 1139-1156):

```rust
    fn resolve(&self, id: PanelId) -> Option<AnyView> {
        match id {
            PanelId::Sftp => Some(...),
            PanelId::Monitor => Some(...),
            PanelId::Sessions => Some(self.sessions_panel.clone()),
            other => self.stub_panels.get(&other).cloned(),
        }
    }
```
→ add a dedicated arm before the `other => ...` catch-all:
```rust
            PanelId::Security => Some(self.security_auth_panel.clone()),
            other => self.stub_panels.get(&other).cloned(),
```

Add the import at the top of `workspace.rs`, alongside the existing `use crate::panels::stub::StubPanel;`:

```rust
use crate::panels::security_auth::SecurityAuthPanel;
```

- [ ] **Step 7: i18n strings and the label rename**

Change the existing `ActivityBar.security` entry in `locales/app.yml` from "安全 / 认证" / "Security / Auth" to:

```yaml
  security:
    zh-CN: "安全认证"
    en: "Security & Auth"
```

Add a new top-level `SecurityAuth:` section:

```yaml
SecurityAuth:
  tab_keys:
    zh-CN: "密钥"
    en: "Keys"
  tab_passwords:
    zh-CN: "密码"
    en: "Passwords"
  add_key_button:
    zh-CN: "添加密钥"
    en: "Add key"
  add_key_title:
    zh-CN: "添加 SSH 密钥"
    en: "Add SSH key"
  key_name_placeholder:
    zh-CN: "名称"
    en: "Name"
  no_file_picked:
    zh-CN: "尚未选择文件"
    en: "No file picked yet"
  rename_button:
    zh-CN: "重命名"
    en: "Rename"
  rename_key_title:
    zh-CN: "重命名密钥"
    en: "Rename key"
  delete_key_title:
    zh-CN: "删除密钥？"
    en: "Delete key?"
  delete_password_title:
    zh-CN: "删除密码？"
    en: "Delete password?"
  delete_confirm_body:
    zh-CN: "此操作不可撤销。"
    en: "This cannot be undone."
  delete_confirm_body_in_use:
    zh-CN: "有 %{count} 个连接正在使用它——删除后这些连接将无法使用，直到重新选择。是否继续删除？"
    en: "Used by %{count} connection(s) — deleting leaves them unable to connect until you pick a different one. Delete anyway?"
  add_password_button:
    zh-CN: "添加密码"
    en: "Add password"
  add_password_title:
    zh-CN: "添加已保存密码"
    en: "Add saved password"
  edit_button:
    zh-CN: "编辑"
    en: "Edit"
  edit_password_title:
    zh-CN: "编辑已保存密码"
    en: "Edit saved password"
  password_name_placeholder:
    zh-CN: "名称"
    en: "Name"
```

- [ ] **Step 8: Build and manually verify (no automated UI tests exist in this codebase, matching the established project convention)**

Run: `cargo build && cargo test`
Expected: green.

Manual: open caracal, click the 安全认证 icon in the left activity bar — confirm it shows the real panel (Keys/Passwords tabs), not the "not implemented" stub placeholder. Add a saved password, confirm it appears in the list; reveal/copy it; edit it; delete it (confirm dialog appears). Add an SSH key by importing a file; rename it; delete a key that's referenced by a connection and confirm the "used by N connections" wording appears.

- [ ] **Step 9: Commit**

```bash
git add src/panels/security_auth.rs src/panels/mod.rs src/workspace.rs locales/app.yml
git commit -m "feat: implement the 安全认证 panel (manage saved SSH keys and passwords)"
```

---

### Task 3: New-connection form — Direct/Saved password tabs, Import-new/Saved key tabs, reveal toggles

**Files:**
- Modify: `src/panels/new_connection_window.rs`
- Modify: `src/panels/sessions.rs` (thread `saved_passwords` into the one `NewConnectionWindow::new` call site)
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: `SessionsPanel::{saved_passwords, add_saved_password}` (Task 1).

- [ ] **Step 1: Add the new struct fields and constructor parameter**

In `src/panels/new_connection_window.rs`, add to the `NewConnectionWindow` struct, right after the existing `pending_new_key: Option<(String, Vec<u8>, String)>,` field:

```rust
    /// Which sub-tab the Password field is on. `Direct` is always the
    /// default/starting tab, including when editing a connection with
    /// `password_id: Some` and that entry has since been deleted (Task 1's
    /// `to_ssh_config` handles the dangling case at connect time; here it
    /// just means the picker opens with nothing pre-selected).
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
    /// existing picker (Task 1/earlier work already made key auth always
    /// reference-based; this just splits "import a new one" from "pick an
    /// existing one" into two tabs instead of showing both stacked).
    key_tab: KeyTab,
```

Add the two small enums right above `pub struct NewConnectionWindow`:

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

Add `saved_passwords: Vec<crate::config::SavedPasswordEntry>` as a new parameter to `NewConnectionWindow::new`, right after the existing `ssh_keys: Vec<crate::config::SshKeyEntry>,` parameter.

- [ ] **Step 2: Initialize the new fields in `new()`**

Right after the existing `decrypted_password`/`decrypted_key_passphrase` computation (Task 1 left this section unchanged — it's still in `new_connection_window.rs` from the earlier encrypted-storage work), add:

```rust
        let password_tab = if conn.as_ref().and_then(|c| c.password_id.clone()).is_some() {
            PasswordTab::Saved
        } else {
            PasswordTab::Direct
        };
        let selected_password_id = conn.as_ref().and_then(|c| c.password_id.clone());
        let key_tab = if conn.as_ref().map(|c| c.auth_method.clone()).as_deref() == Some("key") {
            KeyTab::Saved
        } else {
            KeyTab::ImportNew
        };
```

In the `Self { ... }` literal, add (near the existing `selected_key_id`/`pending_new_key`/`ssh_keys` fields):

```rust
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

- [ ] **Step 3: Update `save()`'s password resolution**

Replace the existing `plaintext_password`/`encrypted_password` computation in `save()`'s SSH branch (currently the block starting `let plaintext_password = if self.auth_method == "key" { ... }` through `let encrypted_password = vault.map(...)`) with:

```rust
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
                let key_passphrase = if self.auth_method == "key" {
                    let p = self.private_key_passphrase.read(cx).value().to_string();
                    if p.is_empty() { None } else { Some(p) }
                } else {
                    None
                };
                let key_id = if self.auth_method == "key" { self.resolve_key_id(cx) } else { None };
                let vault = cx.try_global::<crate::workspace::VaultKey>();
                // `vault` is always `Some` in practice — the vault is
                // unlocked before any window that can reach `save()` opens
                // (see `workspace.rs`'s startup dialogs). Falling back to
                // an empty ciphertext rather than panicking keeps this
                // defensive instead of crashing on an unreachable state.
                let encrypted_password = vault
                    .map(|v| v.0.encrypt_str(&plaintext_password))
                    .unwrap_or_default();
                let encrypted_key_passphrase =
                    key_passphrase.and_then(|p| vault.map(|v| v.0.encrypt_str(&p)));
```

Then, in the `SavedConnection { ... }` literal, replace the `password_id: None,` line Task 1 Step 11 added after `private_key_id: key_id,` — it must become the real computed value, not stay hardcoded:

```rust
                    encrypted_password,
                    encrypted_key_passphrase,
                    private_key_id: key_id,
                    password_id: None,
```
→
```rust
                    encrypted_password,
                    encrypted_key_passphrase,
                    private_key_id: key_id,
                    password_id,
```

- [ ] **Step 4: Restructure `render_ssh_auth_fields`'s password section into Direct/Saved tabs**

Replace the current `else { self.field(...password...) }` tail of `render_ssh_auth_fields` (the branch that renders when `!is_key`) with a 2-way tab, mirroring the existing `auth_method` pill pattern exactly (`Self::pill(...)`, already used for Password/Key):

```rust
            .child(if is_key {
                // ... existing is_key branch, restructured in Step 5 ...
            } else {
                self.render_password_tabs(cx).into_any_element()
            })
```

Add the new method right after `render_ssh_auth_fields`:

```rust
    fn render_password_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_saved = self.password_tab == PasswordTab::Saved;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("password-tab-direct", rust_i18n::t!("SecurityAuth.tab_direct_password"), !is_saved, cx)
                            .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.password_tab = PasswordTab::Direct;
                                cx.notify();
                            })),
                    )
                    .child(
                        Self::pill("password-tab-saved", rust_i18n::t!("SecurityAuth.tab_saved_password"), is_saved, cx)
                            .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.password_tab = PasswordTab::Saved;
                                cx.notify();
                            })),
                    ),
            )
            .child(if is_saved {
                self.render_saved_password_picker(cx).into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(self.field_label(rust_i18n::t!("NewConnectionWindow.password_placeholder"), cx))
                    .child(Input::new(&self.password).mask_toggle())
                    .into_any_element()
            })
    }

    fn render_saved_password_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1();
        for entry in self.saved_passwords.clone() {
            let selected = self.selected_password_id.as_deref() == Some(entry.id.as_str());
            let row_id = SharedString::from(format!("saved-password-{}", entry.id));
            let entry_id = entry.id.clone();
            list = list.child(
                div()
                    .id(row_id)
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .when(selected, |el| el.bg(cx.theme().accent))
                    .when(!selected, |el| el.bg(cx.theme().secondary))
                    .child(entry.name.clone())
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.selected_password_id = Some(entry_id.clone());
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(list)
            .child(if self.adding_new_saved_password {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Input::new(&self.new_saved_password_name))
                    .child(Input::new(&self.new_saved_password_value).mask_toggle())
                    .child(
                        Button::new("confirm-add-saved-password")
                            .xsmall()
                            .label(rust_i18n::t!("SecurityAuth.add_password_button"))
                            .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                this.confirm_add_new_saved_password(cx);
                            })),
                    )
                    .into_any_element()
            } else {
                div()
                    .id("add-new-saved-password-row")
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(cx.theme().secondary)
                    .child(rust_i18n::t!("SecurityAuth.add_new_saved_password_row"))
                    .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                        this.adding_new_saved_password = true;
                        cx.notify();
                    }))
                    .into_any_element()
            })
    }

    fn confirm_add_new_saved_password(&mut self, cx: &mut Context<Self>) {
        let name = self.new_saved_password_name.read(cx).value().trim().to_string();
        let value = self.new_saved_password_value.read(cx).value().to_string();
        if name.is_empty() {
            return;
        }
        let Some(vault) = cx.try_global::<crate::workspace::VaultKey>() else {
            return;
        };
        let entry = crate::config::SavedPasswordEntry {
            id: crate::config::generate_id(),
            name,
            encrypted_password: vault.0.encrypt_str(&value),
        };
        let id = entry.id.clone();
        let _ = self.panel.update(cx, |panel, _cx| panel.add_saved_password(entry.clone()));
        let _ = self.panel.update(cx, |panel, _cx| panel.persist_for_security_auth());
        self.saved_passwords.push(entry);
        self.selected_password_id = Some(id);
        self.adding_new_saved_password = false;
        cx.notify();
    }
```

- [ ] **Step 5: Restructure the Key field into Import-new/Saved tabs**

Replace the current `if is_key { ... }` branch body of `render_ssh_auth_fields` — currently it renders the key-file label, the flat list-of-buttons + pending-new-key row + import button, then the passphrase field, all stacked together. Split the "list of buttons + pending-new-key row" part and the "import button" part into two tabs the same way Step 4 split the password field:

```rust
            .child(if is_key {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                Self::pill("key-tab-import", rust_i18n::t!("SecurityAuth.tab_import_new_key"), self.key_tab == KeyTab::ImportNew, cx)
                                    .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                        this.key_tab = KeyTab::ImportNew;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Self::pill("key-tab-saved", rust_i18n::t!("SecurityAuth.tab_saved_key"), self.key_tab == KeyTab::Saved, cx)
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
                    })
                    .child(self.field(
                        rust_i18n::t!("NewConnectionWindow.private_key_passphrase_placeholder"),
                        &self.private_key_passphrase.clone(),
                        cx,
                    ))
                    .into_any_element()
            } else {
                self.render_password_tabs(cx).into_any_element()
            })
```

Split the *existing* body (Step 5's predecessor, from the current `render_ssh_auth_fields`, lines roughly 758-843 as read during planning) into two new methods: `render_saved_key_picker` gets the existing `.children(self.ssh_keys.clone().into_iter().map(...))` list-of-buttons block (unchanged logic, just extracted into its own function, dropping the field label div wrapper since the tab pills now serve that role); `render_import_new_key` gets the existing `pending_new_key` indicator + `"import-new-ssh-key"` button block (also unchanged logic, extracted). Both are straight extractions — copy the existing code verbatim into the new function bodies, no logic changes, since this step is a pure reshuffle per the design spec.

- [ ] **Step 6: Update the one `NewConnectionWindow::new` call site**

In `src/panels/sessions.rs`'s `open_new_connection_window` (Task 1 Step 12 already added `let saved_passwords = self.saved_passwords.clone();` right after `let ssh_keys = self.ssh_keys.clone();` — if that line isn't present yet, add it now), add the argument to the `NewConnectionWindow::new(...)` call:

```rust
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

- [ ] **Step 7: Add the remaining i18n strings**

Add to `locales/app.yml`'s `SecurityAuth:` section (from Task 2 Step 7):

```yaml
  tab_direct_password:
    zh-CN: "直接输入"
    en: "Direct"
  tab_saved_password:
    zh-CN: "已保存"
    en: "Saved"
  tab_import_new_key:
    zh-CN: "导入新密钥"
    en: "Import new"
  tab_saved_key:
    zh-CN: "已保存"
    en: "Saved"
  add_new_saved_password_row:
    zh-CN: "＋ 添加新的已保存密码..."
    en: "+ Add new saved password..."
```

- [ ] **Step 8: Build, test, manually verify**

Run: `cargo build && cargo test`
Expected: green.

Manual: open "新建连接" for an SSH connection. Confirm the Password field shows Direct/Saved tabs (Direct is the default, has the eye-icon reveal toggle); switch to Saved, confirm the saved-passwords list appears, pick one, confirm the row highlights; use "+ Add new saved password...", fill it in, confirm it's added and auto-selected. Confirm the Key field now shows Import-new/Saved tabs matching the same visual style. Save the connection, reopen it for editing, confirm the correct tab (Direct/Saved, Import-new/Saved) is pre-selected based on what was saved. Connect using a saved password to confirm the connect flow still works end-to-end.

- [ ] **Step 9: Commit**

```bash
git add src/panels/new_connection_window.rs src/panels/sessions.rs locales/app.yml
git commit -m "feat: new-connection form picks a saved password or SSH key, not just types/imports one"
```

---

## Self-Review Notes

- **Spec coverage**: data model (`SavedPasswordEntry`, `password_id`, reference-not-copy linking) → Task 1; panel placement/layout/add-edit-delete/delete-in-use/reveal-copy → Task 2; connection-form Direct/Saved and Import-new/Saved tabs, inline add-new-saved-password, `mask_toggle()` everywhere, dangling-reference-on-open → Task 3.
- **Known soft spot flagged inline, not hidden**: Task 2 Step 3's file-picker button placeholder is explicitly marked as non-compiling as written, with an exact pointer to the already-proven pattern in `new_connection_window.rs` to copy instead — same honesty standard as the previous plan's flagged soft spots, which the compiler caught fast when they came up.
- **Type/name consistency**: `SavedPasswordEntry{id, name, encrypted_password}`, `SessionsPanel`'s six new method names, `PasswordTab`/`KeyTab` enum variants, and `password_id`'s meaning (`None` = Direct, `Some` = Saved) are used identically across all three tasks — cross-checked while writing.
- **Task 3 Step 5 is described as an extraction rather than fully inlined new code** (unlike every other step in this plan) — deliberate: the source code being extracted already exists verbatim in the current `new_connection_window.rs` and was read in full during planning; reproducing all ~85 lines of it a second time here would violate DRY within the plan document itself without adding information the implementer needs beyond "move this, don't change it."
