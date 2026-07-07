# New Connection Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the inline "add/edit connection" form in `saved_connections.rs` with a
standalone window (shared by create and edit), add an icon picker to it, and add SSH
private-key authentication (backend + form UI).

**Architecture:** New `src/panels/new_connection_window.rs` reuses the `cx.open_window` +
`Root::new` recipe already proven twice (`settings_window.rs`, this plan's third use). The
window holds `panel: WeakEntity<SavedConnectionsPanel>` and calls a new
`upsert_connection` method on Save, then closes itself — the same cross-window callback
shape `SettingsWindow` already uses. `SshConfig.password: String` becomes
`SshConfig.auth: SshAuth` (password or private-key), using `russh` 0.61's already-vendored
`keys` module (`load_secret_key`, `authenticate_publickey`) — no new `Cargo.toml`
dependency. The existing inline `ConnForm` and its ~700 lines of supporting code are moved
(not reimplemented) into the new file, then deleted from `saved_connections.rs`.

**Tech Stack:** Rust, gpui + gpui_platform, gpui-component (`Input`/`InputState`,
`DropdownButton`/`PopupMenuItem`, `Root`), `russh` 0.61 (`russh::keys::load_secret_key`,
`PrivateKeyWithHashAlg`, `Handle::authenticate_publickey`), `serde`/`toml`.

## Global Constraints

- The same window/form handles both create and edit (pre-filled when editing) — not two
  separate UIs.
- No group picker is added to the form — group assignment stays exactly as today (preset
  via folder-context trigger, or drag-and-drop afterward).
- The icon picker is a `DropdownButton` + `PopupMenuItem` **list** (icon + label per row),
  not a 2D grid — gpui-component's popover primitive in this codebase is list-oriented.
  Options: 自动 (auto/`None`), 终端 (terminal), 笔记本 (laptop), 服务器 (server), 网络
  (network), telnet, 串口 (serial) — matching `resolve_icon()`'s existing string keys.
- SSH private-key auth: no jump-host, 2FA, agent, or OpenSSH-certificate support — password
  and private-key (with optional passphrase) only.
- Footer is Cancel/Save only (no Apply-without-closing — unlike the settings window, a
  connection record has no "try live" concept).
- Full spec: `docs/superpowers/specs/2026-07-07-new-connection-window-design.md`.

---

### Task 1: SSH private-key auth backend (`terminal/ssh.rs` + `config.rs`)

**Files:**
- Modify: `src/terminal/ssh.rs` (`SshAuth` enum, `SshConfig.auth`, `connect_and_auth`)
- Modify: `src/config.rs` (`SavedConnection`'s 3 new fields, `to_ssh_config`, test updates)
- Modify: `src/panels/saved_connections.rs` (transitional: patch `save_form`'s 4
  `SavedConnection` literals so the crate keeps compiling — `save_form` itself is deleted
  in Task 4, this is only to keep the build green in between)

**Interfaces:**
- Produces: `SshAuth { Password(String), PrivateKey { path: String, passphrase:
  Option<String> } }`, `SshConfig { host, port, user, auth: SshAuth }`,
  `SavedConnection.auth_method: String` ("password"|"key"),
  `SavedConnection.private_key_path: Option<String>`,
  `SavedConnection.private_key_passphrase: Option<String>`,
  `SavedConnection::to_ssh_config()` updated to branch on `auth_method` — consumed by
  Task 3 (`new_connection_window.rs`'s Save button builds these fields from form state).

- [ ] **Step 1: Write the failing tests**

In `src/config.rs`, current state (the `mod tests` block's `base_connection` helper):

```rust
    fn base_connection(conn_type: ConnectionType) -> SavedConnection {
        SavedConnection {
            name: String::new(),
            host: "example.com".to_string(),
            port: 23,
            user: String::new(),
            password: String::new(),
            group_id: None,
            conn_type,
            icon: None,
            shell_path: None,
            working_dir: None,
            serial_port: None,
            baud_rate: None,
            data_bits: None,
            parity: None,
            stop_bits: None,
            flow_control: None,
            description: None,
        }
    }
```

Replace with (adds the 3 new fields to the literal — this alone will fail to compile until
Step 3 adds the fields to the struct, which is the expected failure for this step):

```rust
    fn base_connection(conn_type: ConnectionType) -> SavedConnection {
        SavedConnection {
            name: String::new(),
            host: "example.com".to_string(),
            port: 23,
            user: String::new(),
            password: String::new(),
            group_id: None,
            conn_type,
            icon: None,
            shell_path: None,
            working_dir: None,
            serial_port: None,
            baud_rate: None,
            data_bits: None,
            parity: None,
            stop_bits: None,
            flow_control: None,
            description: None,
            auth_method: "password".to_string(),
            private_key_path: None,
            private_key_passphrase: None,
        }
    }
```

Add these two new tests right after `resolve_icon_auto_resolves_new_connection_types` (or
any existing test — placement within the `mod tests` block doesn't matter):

```rust
    #[test]
    fn to_ssh_config_uses_password_auth_by_default() {
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.password = "hunter2".to_string();
        let cfg = conn.to_ssh_config();
        assert!(matches!(cfg.auth, crate::terminal::ssh::SshAuth::Password(p) if p == "hunter2"));
    }

    #[test]
    fn to_ssh_config_uses_private_key_auth_when_selected() {
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.auth_method = "key".to_string();
        conn.private_key_path = Some("/home/user/.ssh/id_ed25519".to_string());
        conn.private_key_passphrase = Some("secret".to_string());
        let cfg = conn.to_ssh_config();
        match cfg.auth {
            crate::terminal::ssh::SshAuth::PrivateKey { path, passphrase } => {
                assert_eq!(path, "/home/user/.ssh/id_ed25519");
                assert_eq!(passphrase.as_deref(), Some("secret"));
            }
            _ => panic!("expected PrivateKey auth"),
        }
    }

    #[test]
    fn old_connection_without_auth_fields_still_deserializes_as_password() {
        // Simulates a connections.toml written before this change.
        let toml_text = r#"
            [[connections]]
            host = "old.example.com"
            user = "root"
            password = "hunter2"
            conn_type = "ssh"
        "#;
        let cfg: AppConfig =
            toml::from_str(toml_text).expect("old-format connection must still parse");
        assert_eq!(cfg.connections[0].auth_method, "password");
        assert_eq!(cfg.connections[0].private_key_path, None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config:: 2>&1 | tail -40`
Expected: FAIL to compile — `SavedConnection` has no fields `auth_method`/
`private_key_path`/`private_key_passphrase`, and `SshAuth` doesn't exist yet.

- [ ] **Step 3: Add `SshAuth` and update `SshConfig`/`connect_and_auth`**

In `src/terminal/ssh.rs`, current state (top-of-file imports):

```rust
use anyhow::{Result, anyhow};
use russh::client::{self, Handle, Msg};
use russh::keys::ssh_key::PublicKey;
use russh::{Channel, ChannelMsg, Disconnect};
```

Replace with:

```rust
use anyhow::{Result, anyhow};
use russh::client::{self, Handle, Msg};
use russh::keys::ssh_key::PublicKey;
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use russh::{Channel, ChannelMsg, Disconnect};
```

Current state (`SshConfig`):

```rust
/// Connection parameters. Phase 4 supports password auth only.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}
```

Replace with:

```rust
/// Authentication method for an SSH connection.
#[derive(Clone, Debug)]
pub enum SshAuth {
    Password(String),
    /// `path` is the private key file on disk; `passphrase` decrypts it if
    /// it's passphrase-protected — `russh::keys::load_secret_key` handles
    /// both encrypted and plain keys through the same call.
    PrivateKey {
        path: String,
        passphrase: Option<String>,
    },
}

/// Connection parameters — password or private-key authentication.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
}
```

Current state (`connect_and_auth`):

```rust
async fn connect_and_auth(config: SshConfig) -> Result<Handle<ClientHandler>> {
    let SshConfig {
        host,
        port,
        user,
        password,
    } = config;

    let cfg = Arc::new(client::Config::default());
    let mut session = client::connect(cfg, (host.as_str(), port), ClientHandler)
        .await
        .map_err(|e| anyhow!("connect to {host}:{port} failed: {e}"))?;

    let auth = session.authenticate_password(user, password).await?;
    if !auth.success() {
        return Err(anyhow!("authentication failed"));
    }
    Ok(session)
}
```

Replace with:

```rust
async fn connect_and_auth(config: SshConfig) -> Result<Handle<ClientHandler>> {
    let SshConfig {
        host,
        port,
        user,
        auth,
    } = config;

    let cfg = Arc::new(client::Config::default());
    let mut session = client::connect(cfg, (host.as_str(), port), ClientHandler)
        .await
        .map_err(|e| anyhow!("connect to {host}:{port} failed: {e}"))?;

    let auth_result = match auth {
        SshAuth::Password(password) => session.authenticate_password(user, password).await?,
        SshAuth::PrivateKey { path, passphrase } => {
            let key = load_secret_key(&path, passphrase.as_deref())
                .map_err(|e| anyhow!("failed to load private key {path}: {e}"))?;
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            session.authenticate_publickey(user, key).await?
        }
    };
    if !auth_result.success() {
        return Err(anyhow!("authentication failed"));
    }
    Ok(session)
}
```

- [ ] **Step 4: Add the 3 new fields to `SavedConnection` and update `to_ssh_config`**

In `src/config.rs`, current state (top-of-file imports):

```rust
use crate::panels::icons::AppIcon;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::SshConfig;
use crate::terminal::telnet::TelnetConfig;
```

Replace with:

```rust
use crate::panels::icons::AppIcon;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshAuth, SshConfig};
use crate::terminal::telnet::TelnetConfig;
```

Current state (end of the `SavedConnection` struct):

```rust
    /// Optional description shown in tooltip.
    #[serde(default)]
    pub description: Option<String>,
}
```

Replace with:

```rust
    /// Optional description shown in tooltip.
    #[serde(default)]
    pub description: Option<String>,
    /// `"password"` | `"key"`. Defaults to `"password"` for connections
    /// saved before this field existed.
    #[serde(default = "default_auth_method")]
    pub auth_method: String,
    /// Path to a private key file. Only meaningful when `auth_method ==
    /// "key"`.
    #[serde(default)]
    pub private_key_path: Option<String>,
    /// Optional passphrase to decrypt an encrypted private key. Only
    /// meaningful when `auth_method == "key"`.
    #[serde(default)]
    pub private_key_passphrase: Option<String>,
}

fn default_auth_method() -> String {
    "password".to_string()
}
```

Current state (`to_ssh_config`):

```rust
    /// The connection parameters used to actually dial (see `workspace.rs`).
    pub fn to_ssh_config(&self) -> SshConfig {
        SshConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            password: self.password.clone(),
        }
    }
```

Replace with:

```rust
    /// The connection parameters used to actually dial (see `workspace.rs`).
    pub fn to_ssh_config(&self) -> SshConfig {
        let auth = if self.auth_method == "key" {
            SshAuth::PrivateKey {
                path: self.private_key_path.clone().unwrap_or_default(),
                passphrase: self.private_key_passphrase.clone(),
            }
        } else {
            SshAuth::Password(self.password.clone())
        };
        SshConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth,
        }
    }
```

- [ ] **Step 5: Clear the passphrase (not just the password) when duplicating a connection**

In `src/panels/saved_connections.rs`, current state (`duplicate`):

```rust
            // Clear password for security
            new_conn.password = String::new();
```

Replace with:

```rust
            // Clear credentials for security (matches the existing
            // password-clearing behavior, extended to the newer key-auth
            // fields so a duplicated connection never silently carries a
            // copy of a decryption passphrase).
            new_conn.password = String::new();
            new_conn.private_key_passphrase = None;
```

- [ ] **Step 6: Transitional patch — add the 3 new fields to `save_form`'s 4
  `SavedConnection` literals**

This keeps the crate compiling between now and Task 4 (which deletes `save_form` entirely).
In `src/panels/saved_connections.rs`'s `save_form` method, there are 4 `SavedConnection {
... }` literals (one per `ConnectionType` match arm: Ssh, Local, Telnet, Serial), each
ending with `description: None,` followed by the arm's closing `}`. Add these 3 lines
immediately after `description: None,` in **all 4** literals:

```rust
                    auth_method: "password".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
```

(Indentation matches the surrounding `description: None,` line in each arm — 20 spaces for
the Ssh/Telnet/Serial arms' top-level fields, matching whatever `description: None,` uses in
that specific arm.)

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --lib config:: -- --nocapture`
Expected: PASS — all 3 new tests plus the existing `config::tests` suite.

- [ ] **Step 8: Build and run the full test suite**

Run: `cargo build 2>&1 | tail -40` then `cargo test 2>&1 | tail -15`
Expected: builds successfully (the Step 6 transitional patch keeps `save_form` compiling);
all tests pass (56 pre-existing + 3 new = 59, 0 failed).

- [ ] **Step 9: Commit**

```bash
git add src/terminal/ssh.rs src/config.rs src/panels/saved_connections.rs
git commit -m "feat: add SSH private-key authentication (SshAuth, SavedConnection fields)"
```

---

### Task 2: `SavedConnectionsPanel::upsert_connection` + window-handle field

**Files:**
- Modify: `src/panels/saved_connections.rs`

**Interfaces:**
- Produces: `SavedConnectionsPanel.new_connection_window: Option<WindowHandle<Root>>`,
  `SavedConnectionsPanel::pub(crate) fn upsert_connection(&mut self, conn: SavedConnection,
  edit_ix: Option<usize>, cx: &mut Context<Self>)` — consumed by Task 3
  (`new_connection_window.rs`'s Save button).

This task does NOT touch the old inline form (`ConnForm` and friends keep working
unchanged) and does NOT yet add a method to open the new window — that's Task 3, since it
needs the `NewConnectionWindow` type to exist. No new tests: this is a small, self-contained
additive change with no pure logic beyond a push-or-replace, consistent with `save_form`
(the method this mirrors) having no test of its own either. Verify with `cargo build`.

- [ ] **Step 1: Add imports**

Current state (top-of-file imports):

```rust
use gpui::{
    Action, App, AppContext, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
```

Replace with:

```rust
use gpui::{
    Action, App, AppContext, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Window,
    WindowHandle, div, px,
};
use gpui_component::Root;
```

(`gpui_component::Root` is added as its own `use` line since the existing
`use gpui_component::{ActiveTheme, Sizable, StyledExt, WindowExt};` line groups
non-conflicting imports alphabetically by crate path in this file's existing style — adding
`Root` to that same brace group is also fine if the implementer prefers; either compiles
identically.)

- [ ] **Step 2: Add the field**

Current state (the `SavedConnectionsPanel` struct):

```rust
pub struct SavedConnectionsPanel {
    focus_handle: FocusHandle,
    connections: Vec<SavedConnection>,
    groups: Vec<SavedConnectionGroup>,
    // UI state
    search_query: Entity<InputState>,
    sort_mode: SortMode,
    expanded_groups: HashSet<String>,
    // Form state
    form: Option<ConnForm>,
    folder_form_target: Option<FolderFormTarget>,
    new_folder_name: Entity<InputState>,
    /// Kept alive so `new_folder_name`'s `InputEvent::PressEnter` subscription
    /// (submit-on-Enter, see `open_folder_form`) keeps firing.
    _folder_enter_sub: Option<Subscription>,
}
```

Replace with:

```rust
pub struct SavedConnectionsPanel {
    focus_handle: FocusHandle,
    connections: Vec<SavedConnection>,
    groups: Vec<SavedConnectionGroup>,
    // UI state
    search_query: Entity<InputState>,
    sort_mode: SortMode,
    expanded_groups: HashSet<String>,
    // Form state
    form: Option<ConnForm>,
    folder_form_target: Option<FolderFormTarget>,
    new_folder_name: Entity<InputState>,
    /// Kept alive so `new_folder_name`'s `InputEvent::PressEnter` subscription
    /// (submit-on-Enter, see `open_folder_form`) keeps firing.
    _folder_enter_sub: Option<Subscription>,
    /// The open new-connection/edit window, if any — re-triggering any of
    /// the "新建连接"/"编辑" entry points focuses this instead of opening a
    /// duplicate. Unused until Task 3 adds the method that opens it.
    #[allow(dead_code)]
    new_connection_window: Option<WindowHandle<Root>>,
}
```

- [ ] **Step 3: Initialize the field**

Current state (the `Self { .. }` literal in `SavedConnectionsPanel::new`):

```rust
        Self {
            focus_handle: cx.focus_handle(),
            connections,
            groups,
            search_query,
            sort_mode: SortMode::Default,
            expanded_groups,
            form: None,
            folder_form_target: None,
            new_folder_name,
            _folder_enter_sub: None,
        }
```

Replace with:

```rust
        Self {
            focus_handle: cx.focus_handle(),
            connections,
            groups,
            search_query,
            sort_mode: SortMode::Default,
            expanded_groups,
            form: None,
            folder_form_target: None,
            new_folder_name,
            _folder_enter_sub: None,
            new_connection_window: None,
        }
```

- [ ] **Step 4: Add `upsert_connection`**

Add this method anywhere in the first `impl SavedConnectionsPanel` block (e.g. right after
`save_form`, which it will eventually replace the body of — but do not modify `save_form`
itself in this task):

```rust
    /// Push a new connection or replace an existing one at `edit_ix`, persist,
    /// and repaint. Called by
    /// [`crate::panels::new_connection_window::NewConnectionWindow`] on Save.
    pub(crate) fn upsert_connection(
        &mut self,
        conn: SavedConnection,
        edit_ix: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        match edit_ix {
            Some(ix) if ix < self.connections.len() => self.connections[ix] = conn,
            _ => self.connections.push(conn),
        }
        self.persist();
        cx.notify();
    }
```

- [ ] **Step 5: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -30`
Expected: builds successfully (an unused-field warning on `new_connection_window` despite
the `#[allow(dead_code)]` on the field itself is not expected — if one appears anyway,
leave it, Task 3 resolves it by actually using the field).

- [ ] **Step 6: Run the full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all 59 tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/panels/saved_connections.rs
git commit -m "feat: add SavedConnectionsPanel::upsert_connection and window-handle field"
```

---

### Task 3: `NewConnectionWindow` (standalone window: ported form + icon picker + key-auth UI)

**Files:**
- Create: `src/panels/new_connection_window.rs`
- Modify: `src/panels/mod.rs` (register the new module)
- Modify: `src/panels/saved_connections.rs` (add `open_new_connection_window`)

**Interfaces:**
- Consumes: `SavedConnectionsPanel::upsert_connection` (Task 2), `SshAuth`/updated
  `SavedConnection` fields (Task 1), `AppIcon`/`icon()` (pre-existing,
  `src/panels/icons.rs`).
- Produces: `NewConnectionWindow::new(panel: WeakEntity<SavedConnectionsPanel>, existing:
  Option<(usize, SavedConnection)>, group_id: Option<String>, window: &mut Window, cx: &mut
  Context<Self>) -> Self`, `SavedConnectionsPanel::open_new_connection_window(&mut self,
  group_id: Option<String>, edit_ix: Option<usize>, window: &mut Window, cx: &mut
  Context<Self>)` — consumed by Task 4 (rewiring the 5 trigger points).

This task's own trigger points (the 5 places that currently open the old inline form) are
**not** rewired yet — that's Task 4. After this task, the new window exists and is fully
functional if opened programmatically, but nothing in the running app opens it yet (the old
inline form remains the only reachable path). This keeps this large task reviewable on its
own (does the new window correctly build/save a connection?) separately from the cutover
(Task 4).

No automated tests for this task's UI code, consistent with the rest of `panels/*.rs`.
Verify with `cargo build`.

- [ ] **Step 1: Move the inline form's supporting code into the new file**

Read `src/panels/saved_connections.rs` and locate these items (use the method/struct names
to find them — exact line numbers may have drifted since Tasks 1-2 touched this file):

1. The `ConnForm` struct definition (currently starts `/// The inline "add connection"
   form.\nstruct ConnForm { ... }`, ~30 lines).
2. These methods, in the first `impl SavedConnectionsPanel` block:
   `toggle_form`, `open_new_connection_form`, `start_edit`, `watch_enter_to_submit`,
   `save_form` — **do not move `save_form` yet, it stays in `saved_connections.rs` for now**
   (Task 4 deletes it; this task only reads it as reference for building the new window's
   save logic, described below — moving `toggle_form`/`open_new_connection_form`/
   `start_edit`/`watch_enter_to_submit` verbatim is fine since they only manipulate
   `self.form`, which still exists in `saved_connections.rs` at this point — actually, on
   reflection: **do not move any of these 5 methods either**. They all read/write
   `self.form: Option<ConnForm>` on `SavedConnectionsPanel`, which stays alive until Task 4.
   Moving them now would either break them (no `self.form` in the new file) or require
   deleting `ConnForm`'s field from `SavedConnectionsPanel` early, which Task 4 is scoped to
   do. **For this task, treat all of the above as read-only reference material** — you are
   writing NEW code in `new_connection_window.rs` inspired by their logic, not literally
   relocating these 5 methods. The literal relocation (cut from `saved_connections.rs`,
   confirmed no longer needed) happens in Task 4, where these methods (except `save_form`,
   whose logic you're reimplementing now as `NewConnectionWindow`'s save handler) are
   deleted outright rather than moved, because their responsibility (managing
   `self.form`'s lifecycle) no longer exists once the window-based flow replaces them.
3. `field`, `field_label`, `pill`, `serial_port_field`, `data_bits_field`, `parity_field`,
   `stop_bits_field`, `flow_control_field` — these render helper methods take `&self, form:
   &ConnForm, cx: ...` (or similar) and reference `self.field_label(...)`/`Self::pill(...)`
   internally. **These four "leaf" style helpers (`field`, `field_label`, `pill`) plus the
   four per-type field renderers should be copied (not yet deleted from
   `saved_connections.rs` — Task 4 deletes the originals) into `new_connection_window.rs`,
   adapted to `NewConnectionWindow`'s own struct** (i.e., `fn field(&self, ...)` becomes a
   method on `NewConnectionWindow` instead of `SavedConnectionsPanel`; `form: &ConnForm`
   parameters are dropped since the new window doesn't have a separate `ConnForm` — its own
   fields ARE the form state, so these render helpers read `self.<field>` directly instead
   of `form.<field>`).
4. `render_form`'s body (the connection-type pill row + per-type field list + Cancel/Save
   footer) — read as reference for `NewConnectionWindow::render`'s equivalent content;
   adapted the same way (drop the `form: &ConnForm` indirection, read `self.*` directly;
   Cancel becomes `window.remove_window()` instead of `self.form = None`; Save calls the new
   window's own save method instead of `self.save_form(cx)`).

- [ ] **Step 2: Write `NewConnectionWindow`**

Create `src/panels/new_connection_window.rs`:

```rust
//! `NewConnectionWindow`: a standalone window (File-menu-independent — opened
//! from the saved-connections panel's toolbar/context-menu/hover-edit entry
//! points, see `SavedConnectionsPanel::open_new_connection_window`) for
//! creating or editing a saved connection. Shares one form for both: `existing`
//! is `Some((ix, conn))` when editing in place, `None` when creating new.
//! Ported from the panel's former inline `ConnForm` (see git history / the
//! design spec for what changed: standalone window instead of inline,
//! added icon picker, added SSH private-key auth).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    App, ClickEvent, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Stateful, StatefulInteractiveElement, Styled, WeakEntity, Window, div, px,
};
use gpui_component::button::{Button, DropdownButton};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme, Sizable};

use crate::config::{ConnectionType, SavedConnection};
use crate::panels::icons::{AppIcon, icon};
use crate::panels::saved_connections::SavedConnectionsPanel;

/// Icon-picker options, matching `SavedConnection::resolve_icon`'s existing
/// string-key matching in `config.rs` (`"terminal"`, `"laptop"`, `"server"`,
/// `"network"`, `"telnet"`, `"serial"`). `None` means "auto" (icon inferred
/// from `conn_type`, today's behavior).
const ICON_OPTIONS: &[(Option<&str>, &str, AppIcon)] = &[
    (None, "自动", AppIcon::SavedConnections),
    (Some("terminal"), "终端", AppIcon::Terminal),
    (Some("laptop"), "笔记本", AppIcon::LocalTerminal),
    (Some("server"), "服务器", AppIcon::SavedConnections),
    (Some("network"), "网络", AppIcon::Network),
    (Some("telnet"), "Telnet", AppIcon::Telnet),
    (Some("serial"), "串口", AppIcon::SerialPort),
];

fn generate_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("id-{nanos}-{seq}")
}

pub struct NewConnectionWindow {
    panel: WeakEntity<SavedConnectionsPanel>,
    /// `Some(ix)` when editing an existing connection in place; `None` when
    /// creating a new one.
    edit_ix: Option<usize>,
    group_id: Option<String>,
    icon_key: Option<String>,
    conn_type: ConnectionType,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    auth_method: String,
    private_key_path: Entity<InputState>,
    private_key_passphrase: Entity<InputState>,
    shell_path: Entity<InputState>,
    working_dir: Entity<InputState>,
    serial_port: Entity<InputState>,
    baud_rate: Entity<InputState>,
    data_bits: u8,
    parity: String,
    stop_bits: u8,
    flow_control: String,
}

impl NewConnectionWindow {
    pub fn new(
        panel: WeakEntity<SavedConnectionsPanel>,
        existing: Option<(usize, SavedConnection)>,
        group_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let conn = existing.as_ref().map(|(_, c)| c.clone());
        let edit_ix = existing.map(|(ix, _)| ix);
        let group_id = conn.as_ref().and_then(|c| c.group_id.clone()).or(group_id);

        let text = |field: fn(&SavedConnection) -> &str| -> String {
            conn.as_ref().map(field).unwrap_or_default().to_string()
        };

        Self {
            panel,
            edit_ix,
            group_id,
            icon_key: conn.as_ref().and_then(|c| c.icon.clone()),
            conn_type: conn.as_ref().map(|c| c.conn_type.clone()).unwrap_or(ConnectionType::Ssh),
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("名称(可选)")
                    .default_value(text(|c| &c.name))
            }),
            host: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("主机 host")
                    .default_value(text(|c| &c.host))
            }),
            port: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("端口(默认 22)")
                    .default_value(conn.as_ref().map(|c| c.port.to_string()).unwrap_or_default())
            }),
            user: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("用户名 user")
                    .default_value(text(|c| &c.user))
            }),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("密码")
                    .default_value(text(|c| &c.password))
            }),
            auth_method: conn
                .as_ref()
                .map(|c| c.auth_method.clone())
                .unwrap_or_else(|| "password".to_string()),
            private_key_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("私钥文件路径")
                    .default_value(
                        conn.as_ref()
                            .and_then(|c| c.private_key_path.clone())
                            .unwrap_or_default(),
                    )
            }),
            private_key_passphrase: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("密码短语(可选)")
                    .default_value(
                        conn.as_ref()
                            .and_then(|c| c.private_key_passphrase.clone())
                            .unwrap_or_default(),
                    )
            }),
            shell_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("shell 路径(默认 $SHELL)")
                    .default_value(
                        conn.as_ref().and_then(|c| c.shell_path.clone()).unwrap_or_default(),
                    )
            }),
            working_dir: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("工作目录(默认 $HOME)")
                    .default_value(
                        conn.as_ref().and_then(|c| c.working_dir.clone()).unwrap_or_default(),
                    )
            }),
            serial_port: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("/dev/ttyUSB0")
                    .default_value(
                        conn.as_ref().and_then(|c| c.serial_port.clone()).unwrap_or_default(),
                    )
            }),
            baud_rate: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("115200")
                    .default_value(
                        conn.as_ref()
                            .and_then(|c| c.baud_rate)
                            .unwrap_or(115_200)
                            .to_string(),
                    )
            }),
            data_bits: conn.as_ref().and_then(|c| c.data_bits).unwrap_or(8),
            parity: conn
                .as_ref()
                .and_then(|c| c.parity.clone())
                .unwrap_or_else(|| "none".to_string()),
            stop_bits: conn.as_ref().and_then(|c| c.stop_bits).unwrap_or(1),
            flow_control: conn
                .as_ref()
                .and_then(|c| c.flow_control.clone())
                .unwrap_or_else(|| "none".to_string()),
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let group_id = self.group_id.clone();
        let edit_ix = self.edit_ix;
        let icon = self.icon_key.clone();

        let conn = match self.conn_type {
            ConnectionType::Ssh => {
                let host = self.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host,
                    port: self.port.read(cx).value().trim().parse().unwrap_or(22),
                    user: self.user.read(cx).value().trim().to_string(),
                    password: self.password.read(cx).value().to_string(),
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    shell_path: None,
                    working_dir: None,
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                    auth_method: self.auth_method.clone(),
                    private_key_path: if self.auth_method == "key" {
                        Some(self.private_key_path.read(cx).value().trim().to_string())
                    } else {
                        None
                    },
                    private_key_passphrase: if self.auth_method == "key" {
                        let p = self.private_key_passphrase.read(cx).value().to_string();
                        if p.is_empty() { None } else { Some(p) }
                    } else {
                        None
                    },
                }
            }
            ConnectionType::Local => {
                let shell_path = self.shell_path.read(cx).value().trim().to_string();
                let working_dir = self.working_dir.read(cx).value().trim().to_string();
                SavedConnection {
                    name,
                    host: String::new(),
                    port: 0,
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    shell_path: if shell_path.is_empty() { None } else { Some(shell_path) },
                    working_dir: if working_dir.is_empty() { None } else { Some(working_dir) },
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                    auth_method: "password".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                }
            }
            ConnectionType::Telnet => {
                let host = self.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host,
                    port: self.port.read(cx).value().trim().parse().unwrap_or(23),
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    shell_path: None,
                    working_dir: None,
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                    auth_method: "password".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                }
            }
            ConnectionType::Serial => {
                let serial_port = self.serial_port.read(cx).value().trim().to_string();
                if serial_port.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host: String::new(),
                    port: 0,
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    shell_path: None,
                    working_dir: None,
                    serial_port: Some(serial_port),
                    baud_rate: Some(
                        self.baud_rate.read(cx).value().trim().parse().unwrap_or(115_200),
                    ),
                    data_bits: Some(self.data_bits),
                    parity: Some(self.parity.clone()),
                    stop_bits: Some(self.stop_bits),
                    flow_control: Some(self.flow_control.clone()),
                    description: None,
                    auth_method: "password".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                }
            }
        };

        let _ = self.panel.update(cx, |panel, cx| {
            panel.upsert_connection(conn, edit_ix, cx);
        });
        window.remove_window();
    }

    fn field(&self, label: &str, state: &Entity<InputState>, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(label, cx))
            .child(Input::new(state))
    }

    fn field_label(&self, label: &str, cx: &App) -> impl IntoElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(label.to_string()))
    }

    fn pill(id: &'static str, label: &str, active: bool, cx: &App) -> Stateful<Div> {
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(if active { cx.theme().primary } else { cx.theme().accent })
            .text_color(if active {
                cx.theme().primary_foreground
            } else {
                cx.theme().foreground
            })
            .child(label.to_string())
    }

    fn render_icon_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = ICON_OPTIONS
            .iter()
            .find(|(key, _, _)| key.map(|k| k.to_string()) == self.icon_key)
            .unwrap_or(&ICON_OPTIONS[0]);
        // `PopupMenuItem::on_click` closures are plain closures, not
        // `cx.listener(...)` — they have no direct access to `self`. Capture
        // a `WeakEntity<Self>` and update through it instead, the same way
        // `saved_connections.rs`'s `confirm_delete_connection` mutates panel
        // state from inside its (also plain, non-listener) `on_ok` closure:
        // `weak_panel.update(cx, |this, cx| this.delete(ix, cx))`.
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("图标", cx))
            .child(
                DropdownButton::new("icon-picker")
                    .small()
                    .button(
                        Button::new("icon-picker-btn")
                            .icon(icon(current.2))
                            .label(current.1),
                    )
                    .dropdown_menu(move |menu, _window, _cx| {
                        let mut menu = menu;
                        for (key, label, app_icon) in ICON_OPTIONS {
                            let key = key.map(|k| k.to_string());
                            let weak = weak.clone();
                            menu = menu.item(
                                PopupMenuItem::new(*label).icon(icon(*app_icon)).on_click(
                                    move |_ev, _window, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.icon_key = key.clone();
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    }),
            )
    }
}
```

Continue `NewConnectionWindow`'s `impl` block with the per-type field renderers. Here is
`data_bits_field` fully adapted from `saved_connections.rs`'s version (the transformation
rule, applied identically to `parity_field`/`stop_bits_field`/`flow_control_field`: drop the
`form: &ConnForm` parameter, replace every `form.<field>` read with `self.<field>`, and
replace each pill's `on_click` body — originally `if let Some(ref mut f) = this.form {
f.<field> = <value>; } cx.notify();` — with the flattened `this.<field> = <value>; cx.notify();`,
since `NewConnectionWindow` has no `Option<ConnForm>` indirection, its own fields already
are the form state):

```rust
    fn data_bits_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("数据位", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("data-bits-5", "5", self.data_bits == 5, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 5;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-6", "6", self.data_bits == 6, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 6;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-7", "7", self.data_bits == 7, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 7;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-8", "8", self.data_bits == 8, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 8;
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }
```

Apply the exact same transformation to `parity_field` (pills `"none"`/`"odd"`/`"even"` →
`self.parity`, labels 无/奇校验/偶校验), `stop_bits_field` (pills `1`/`2` → `self.stop_bits`),
and `flow_control_field` (pills `"none"`/`"software"`/`"hardware"` → `self.flow_control`,
labels 无/软件(XON/XOFF)/硬件(RTS/CTS) — read `saved_connections.rs`'s
`flow_control_field` for the exact 3rd pill's label if it's cut off in what you read).

`serial_port_field` needs one more change beyond the mechanical rule (it also drops the
`form` param and uses `self`, but its `DropdownButton`'s `target` capture changes from
`form.serial_port.clone()` to `self.serial_port.clone()` — otherwise identical, including
the `crate::terminal::serial::list_ports()` call and the per-port `PopupMenuItem`
construction):

```rust
    fn serial_port_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.serial_port.clone();
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("串口设备", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.serial_port)))
                    .child(
                        DropdownButton::new("serial-port-picker")
                            .small()
                            .button(Button::new("serial-port-picker-btn").label("选择"))
                            .dropdown_menu(move |menu, _window, _cx| {
                                let ports = crate::terminal::serial::list_ports();
                                if ports.is_empty() {
                                    return menu.label("未检测到串口设备");
                                }
                                let mut menu = menu;
                                for path in ports {
                                    let target = target.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(path.clone()).on_click(
                                            move |_ev, window, cx| {
                                                let path = path.clone();
                                                target.update(cx, |s, cx| {
                                                    s.set_value(path, window, cx);
                                                });
                                            },
                                        ),
                                    );
                                }
                                menu
                            }),
                    ),
            )
    }
```

Add the SSH auth-method UI — a pill pair (密码/密钥) toggling `self.auth_method`, and the
password vs. private-key sub-fields shown based on it:

```rust
    fn render_ssh_auth_fields(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_key = self.auth_method == "key";
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(self.field_label("认证方式", cx))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(Self::pill("auth-password", "密码", !is_key, cx).on_click(
                                cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    this.auth_method = "password".to_string();
                                    cx.notify();
                                }),
                            ))
                            .child(Self::pill("auth-key", "密钥", is_key, cx).on_click(
                                cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    this.auth_method = "key".to_string();
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .child(if is_key {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(self.field_label("私钥文件", cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .items_center()
                                    .child(div().flex_1().child(Input::new(&self.private_key_path)))
                                    .child(
                                        div()
                                            .id("browse-private-key")
                                            .px_2()
                                            .py_0p5()
                                            .rounded_sm()
                                            .bg(cx.theme().accent)
                                            .child("浏览...")
                                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                                let path_input = this.private_key_path.clone();
                                                let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                                                    files: true,
                                                    directories: false,
                                                    multiple: false,
                                                    prompt: None,
                                                });
                                                // `set_value` needs a live `&mut Window`, which a
                                                // plain `cx.spawn` async closure doesn't have
                                                // (only `AsyncApp`) — `cx.spawn_in(window, ...)`
                                                // gives an `AsyncWindowContext` instead, whose
                                                // `.update(|window, cx| ...)` hands back a real
                                                // `&mut Window` (confirmed against
                                                // `~/.cargo/git/checkouts/zed-a70e2ad075855582/1d217ee/crates/gpui/src/app/context.rs:676`
                                                // and `.../app/async_context.rs:299`).
                                                cx.spawn_in(window, async move |_this, cx| {
                                                    let Ok(Ok(Some(paths))) = rx.await else {
                                                        return;
                                                    };
                                                    let Some(path) = paths.into_iter().next() else {
                                                        return;
                                                    };
                                                    let _ = cx.update(|window, cx| {
                                                        path_input.update(cx, |s, cx| {
                                                            s.set_value(
                                                                path.to_string_lossy().to_string(),
                                                                window,
                                                                cx,
                                                            );
                                                        });
                                                    });
                                                })
                                                .detach();
                                            })),
                                    ),
                            ),
                    )
                    .child(self.field("密码短语(可选)", &self.private_key_passphrase.clone(), cx))
                    .into_any_element()
            } else {
                self.field("密码", &self.password.clone(), cx).into_any_element()
            })
    }
```

The `cx.spawn_in`/`AsyncWindowContext::update` combination above is the one piece of this
plan with no direct existing precedent elsewhere in this codebase (native file-picker → text
-field wiring doesn't exist yet; `sftp.rs`'s own `cx.prompt_for_paths` use feeds an upload
task, never writes a value back into an `InputState`), but the exact API shapes involved
(`Context::spawn_in(&self, window: &Window, f: AsyncFn) -> Task<R>` and
`AsyncWindowContext::update(&mut self, update: impl FnOnce(&mut Window, &mut App) -> R) ->
Result<R>`) were read directly from the vendored gpui source at the line references in the
comment above — verify `cargo build` accepts it as-is; if a signature has drifted from what's
quoted, re-check those two exact file:line locations rather than guessing at a different API.

Finally, assemble `Render for NewConnectionWindow`, with the per-type field list wired into
the type-selector row (the earlier sketch of this `impl Render` block left a comment
placeholder here — replace it with):

```rust
impl Render for NewConnectionWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let conn_type = self.conn_type.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .p_2()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .child(Self::pill("type-ssh", "SSH", conn_type == ConnectionType::Ssh, cx)
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Ssh;
                            cx.notify();
                        })))
                    .child(Self::pill("type-local", "本地终端", conn_type == ConnectionType::Local, cx)
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Local;
                            cx.notify();
                        })))
                    .child(Self::pill("type-telnet", "Telnet", conn_type == ConnectionType::Telnet, cx)
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Telnet;
                            cx.notify();
                        })))
                    .child(Self::pill("type-serial", "串口", conn_type == ConnectionType::Serial, cx)
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Serial;
                            cx.notify();
                        }))),
            )
            .child(self.render_icon_picker(cx))
            .child(self.field("名称", &self.name.clone(), cx))
            .children(match conn_type {
                ConnectionType::Ssh => vec![
                    self.field("主机", &self.host.clone(), cx).into_any_element(),
                    self.field("端口", &self.port.clone(), cx).into_any_element(),
                    self.field("用户名", &self.user.clone(), cx).into_any_element(),
                    self.render_ssh_auth_fields(cx).into_any_element(),
                ],
                ConnectionType::Local => vec![
                    self.field("Shell 路径", &self.shell_path.clone(), cx).into_any_element(),
                    self.field("工作目录", &self.working_dir.clone(), cx).into_any_element(),
                ],
                ConnectionType::Telnet => vec![
                    self.field("主机", &self.host.clone(), cx).into_any_element(),
                    self.field("端口", &self.port.clone(), cx).into_any_element(),
                ],
                ConnectionType::Serial => vec![
                    self.serial_port_field(cx).into_any_element(),
                    self.field("波特率", &self.baud_rate.clone(), cx).into_any_element(),
                    self.data_bits_field(cx).into_any_element(),
                    self.parity_field(cx).into_any_element(),
                    self.stop_bits_field(cx).into_any_element(),
                    self.flow_control_field(cx).into_any_element(),
                ],
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .id("newconn-cancel")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child("取消")
                            .on_click(cx.listener(|_this, _ev: &ClickEvent, window, _cx| {
                                window.remove_window();
                            })),
                    )
                    .child(
                        div()
                            .id("newconn-save")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .child("保存")
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.save(window, cx);
                            })),
                    ),
            )
    }
}
```

(The `.child(self.field("名称", &self.name.clone(), cx))` line and the per-type fields section
are intentionally left for the implementer to fill in following the exact porting guidance
above — this is a genuine "assemble from the referenced, already-working source", not a
vague placeholder: every field and pattern it needs to use is named explicitly, either in
this plan or in the cited `saved_connections.rs` methods.)

- [ ] **Step 3: Register the module**

In `src/panels/mod.rs`, add `pub mod new_connection_window;`.

- [ ] **Step 4: Add `SavedConnectionsPanel::open_new_connection_window`**

In `src/panels/saved_connections.rs`, add this import to the top-of-file `use crate::...`
block (alongside the existing `use crate::config::...` line):

```rust
use crate::panels::new_connection_window::NewConnectionWindow;
```

Add this method to the first `impl SavedConnectionsPanel` block (e.g. right after
`upsert_connection` from Task 2):

```rust
    /// Open the new-connection/edit window, or focus it if one is already
    /// open. `edit_ix` pre-fills the form from `self.connections[edit_ix]`
    /// for editing; `None` opens a blank form, optionally preset to
    /// `group_id` (used by the folder context menu's "新建连接").
    pub(crate) fn open_new_connection_window(
        &mut self,
        group_id: Option<String>,
        edit_ix: Option<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = &self.new_connection_window {
            if handle
                .update(cx, |_root, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
        }

        let existing = edit_ix.and_then(|ix| self.connections.get(ix).map(|c| (ix, c.clone())));
        let panel = cx.entity().downgrade();
        let bounds = gpui::Bounds::centered(None, gpui::size(px(480.0), px(560.0)), cx);
        let result = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            move |window, cx| {
                let new_window = cx.new(|cx| {
                    NewConnectionWindow::new(panel.clone(), existing, group_id.clone(), window, cx)
                });
                cx.new(|cx| Root::new(new_window, window, cx).bg(cx.theme().background))
            },
        );
        match result {
            Ok(handle) => self.new_connection_window = Some(handle),
            Err(e) => log::error!("failed to open new-connection window: {e}"),
        }
    }
```

- [ ] **Step 5: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -60`
Expected: builds successfully. This task has significant new gpui-API surface (a new
window, a dropdown-based icon picker, ported pill/field patterns) — treat `cargo build` as
the real verification. Cross-check against `saved_connections.rs`'s original working code
for the exact method/field names being ported, and against `settings_window.rs`'s
`open_settings` for the window-opening shape (`Bounds`/`WindowOptions`/`WindowBounds` are
already imported as `gpui::` paths there — check whether `saved_connections.rs` needs new
`use gpui::{Bounds, WindowBounds, WindowOptions, size};` imports added, since the sketch
above uses fully-qualified `gpui::Bounds` etc. paths specifically to avoid needing to edit
that file's `use gpui::{...}` block twice in this plan — either fully-qualify as shown or add
proper `use` imports; both are fine, pick whichever keeps the diff cleaner).

- [ ] **Step 6: Run the full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all 59 tests still pass (no new tests added by this task).

- [ ] **Step 7: Commit**

```bash
git add src/panels/new_connection_window.rs src/panels/mod.rs src/panels/saved_connections.rs
git commit -m "feat: add NewConnectionWindow (standalone window, icon picker, SSH key auth UI)"
```

---

### Task 4: Cut over the 5 trigger points, delete the old inline form

**Files:**
- Modify: `src/panels/saved_connections.rs`

**Interfaces:**
- Consumes: `SavedConnectionsPanel::open_new_connection_window` (Task 3).

No new tests — this is a deletion + 5 call-site rewires, consistent with the rest of this
refactor. Verify with `cargo build` (confirming nothing still references the deleted items)
and `cargo test`.

- [ ] **Step 1: Rewire the 5 trigger points**

1. Toolbar "+" button. Current state:

```rust
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.toggle_form(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("more-btn")
```

Replace the `this.toggle_form(window, cx)` line with:

```rust
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.open_new_connection_window(None, None, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("more-btn")
```

2. Hover "✏️" edit icon. Current state:

```rust
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                        this.start_edit(ix, window, cx);
                    })),
            )
            .child(
                // Delete
```

Replace with:

```rust
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                        this.open_new_connection_window(None, Some(ix), window, cx);
                    })),
            )
            .child(
                // Delete
```

3. `on_action_edit_connection`. Current state:

```rust
    fn on_action_edit_connection(
        &mut self,
        action: &EditConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_edit(action.ix, window, cx);
    }
```

Replace with:

```rust
    fn on_action_edit_connection(
        &mut self,
        action: &EditConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_window(None, Some(action.ix), window, cx);
    }
```

4. `on_action_new_connection_in_group`. Current state:

```rust
    fn on_action_new_connection_in_group(
        &mut self,
        action: &NewConnectionInGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_form(Some(action.group_id.clone()), window, cx);
    }
```

Replace with:

```rust
    fn on_action_new_connection_in_group(
        &mut self,
        action: &NewConnectionInGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_window(Some(action.group_id.clone()), None, window, cx);
    }
```

5. `on_action_new_root_connection`. Current state:

```rust
    fn on_action_new_root_connection(
        &mut self,
        _action: &NewRootConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_form(None, window, cx);
    }
```

Replace with:

```rust
    fn on_action_new_root_connection(
        &mut self,
        _action: &NewRootConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_window(None, None, window, cx);
    }
```

- [ ] **Step 2: Remove `.children(self.render_form(cx))` from the main `render()`**

Current state:

```rust
            .children(self.render_folder_form(cx))
            .children(self.render_form(cx))
            .children(self.render_empty(cx))
```

Replace with:

```rust
            .children(self.render_folder_form(cx))
            .children(self.render_empty(cx))
```

- [ ] **Step 3: Delete the old inline-form code**

Delete these items entirely from `src/panels/saved_connections.rs` (confirm each is deleted
by re-`grep`ing for its name afterward — should return no matches outside comments/docs):

- The `ConnForm` struct definition.
- `form: Option<ConnForm>` field on `SavedConnectionsPanel` (and its `form: None,`
  initializer in `SavedConnectionsPanel::new`) — but only after confirming nothing else in
  the file still reads `self.form` (it shouldn't, once the above items are gone; `grep -n
  "self\.form" src/panels/saved_connections.rs` should return nothing).
- `toggle_form`, `open_new_connection_form`, `start_edit`, `watch_enter_to_submit`,
  `save_form`, `render_form`.
- `field`, `field_label`, `pill`, `serial_port_field`, `data_bits_field`, `parity_field`,
  `stop_bits_field`, `flow_control_field` (these were copied, not moved, into
  `new_connection_window.rs` in Task 3 — the copies stay there; delete the originals here).

After deleting, run `cargo build 2>&1 | tail -60` and fix any leftover reference (e.g. an
`on_action` registration for an action whose handler you just deleted would be a bug —
there shouldn't be one, since Step 1 rewired every handler to call
`open_new_connection_window` instead of deleting the handler itself).

- [ ] **Step 4: Build and run the full test suite**

Run: `cargo build 2>&1 | tail -60` then `cargo test 2>&1 | tail -15`
Expected: builds successfully with no warnings about unused `ConnForm`-related items (they're
gone, not just unused); all 59 tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/panels/saved_connections.rs
git commit -m "refactor: cut over to NewConnectionWindow, delete the old inline connection form"
```

---

### Task 5: Build verification and manual smoke test

**Files:** none (verification only)

- [ ] **Step 1: Release build**

Run: `cargo build --release 2>&1 | tail -30`
Expected: builds successfully.

- [ ] **Step 2: Manual smoke test**

If a display is available in this environment, run `cargo run --release` and verify by hand
(if no display is available, note that explicitly rather than claiming it was checked):

1. Toolbar "+" button opens the new standalone window (a separate OS window, not an inline
   panel section).
2. Create an SSH connection with password auth (regression check against the pre-refactor
   behavior) — fill host/port/user/password, Save, confirm it appears in the list and opens
   correctly.
3. Create an SSH connection with private-key auth: switch the auth-method pill to 密钥, use
   "浏览..." to pick a private key file via the native OS file dialog, optionally fill a
   passphrase, Save. If you have a real test SSH server + key pair available, confirm the
   connection actually authenticates; otherwise at minimum confirm the form saves and
   reopens with the key path pre-filled.
4. Set a custom icon (not "自动") on a connection via the icon-picker dropdown; confirm it
   persists (close and reopen the edit window) and the connection row's icon changes in the
   list.
5. Edit an existing connection (hover ✏️, and separately via right-click → 编辑) — confirm
   the window opens pre-filled with all existing values including auth method.
6. Folder right-click → "新建连接" — confirm the new connection lands in that folder.
7. Root-level "新建连接" (blank-area right-click) — confirm it's ungrouped.
8. Open the window, click 取消 — confirm no connection is added/changed.
9. With the window already open, trigger another "新建连接" entry point — confirm it
   focuses the existing window rather than opening a second one.
10. Create Local/Telnet/Serial connections — confirm each still works exactly as before
    (this refactor shouldn't have changed their behavior, only SSH gained key auth).
11. Quit and relaunch; confirm all connections created above (with their icons and, for
    SSH, auth method) persisted correctly in `connections.toml`.

- [ ] **Step 3: Report results**

Summarize which of the 11 manual checks passed, and paste the full text of any that didn't,
before considering this task/plan complete.
