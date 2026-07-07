# New Connection window (standalone) + icon picker + SSH private-key auth

Date: 2026-07-07
Files under change: `src/panels/saved_connections.rs` (remove inline form), new
`src/panels/new_connection_window.rs`, `src/config.rs`, `src/terminal/ssh.rs`,
`Cargo.toml` (no new dependency — `russh`'s bundled `keys` module already provides
private-key loading).

Item 3 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md). Originally scoped
as "convert the inline connection form to a standalone window + add an icon picker"; SSH
private-key authentication was added to this pass after investigation showed it's tractable
with `russh` 0.61's existing bundled `keys` module (`load_secret_key` +
`authenticate_publickey`, no new crate dependency) — unlike jump-host chaining, 2FA, and
per-connection algorithm ordering, which remain deferred (see Non-goals) because they need
much deeper `russh::client::Config` plumbing.

## Background

`src/panels/saved_connections.rs` currently opens a `ConnForm` inline (embedded in the
side-dock panel itself, toggle-visibility) for both creating and editing connections,
covering all 4 protocol types (SSH/Local/Telnet/Serial) in one struct with per-type field
subsets shown/hidden by a type-selector pill row. There is no icon picker (connections
always auto-resolve their icon from `conn_type`, even though `SavedConnection.icon:
Option<String>` and `resolve_icon()`'s string-key matching already support a manual
override — nothing in the UI ever sets it). SSH auth is password-only
(`SshConfig.password: String`, `terminal/ssh.rs`'s `connect_and_auth` calls
`session.authenticate_password(...)` unconditionally).

## Decisions (confirmed with user)

### Standalone window, shared by create and edit

New `src/panels/new_connection_window.rs`, using the exact `cx.open_window` + `Root::new`
recipe `settings_window.rs` already established (confirmed working, reused a second time).
The **same window/form handles both creating and editing** — matching nyaterm and avoiding
two parallel form UIs — pre-filled from an existing `SavedConnection` when editing, blank
otherwise. `SavedConnectionsPanel` gains a `new_connection_window: Option<WindowHandle<Root>>`
field with the same open-or-focus-existing behavior as `Workspace.settings_window`.

The window holds `panel: WeakEntity<SavedConnectionsPanel>` and calls a new
`SavedConnectionsPanel::upsert_connection(conn: SavedConnection, edit_ix: Option<usize>, cx)`
method on Save (push new or replace at `edit_ix`, persist, `cx.notify()`), then
`window.remove_window()` — the same cross-window callback shape `SettingsWindow` already
uses for `Workspace::apply_font_settings`. Footer is just Cancel/Save (no Apply-without-
closing — unlike settings, a connection record has no "try live" concept).

The entire inline `ConnForm` struct, `toggle_form`, `open_new_connection_form`, `start_edit`,
`save_form`, `render_form`, and the per-type field-rendering helpers that only that form used
(`serial_port_field`, `data_bits_field`, `parity_field`, `stop_bits_field`,
`flow_control_field`, `watch_enter_to_submit`) move to the new file — ported, not
reimplemented from scratch, then deleted from `saved_connections.rs`. All 5 existing trigger
points are rewired to open-or-focus the new window instead:
- Toolbar "+" button (`toggle_form`'s call site)
- Hover "✏️" edit icon on a connection row (`start_edit`'s call site)
- `EditConnection` context-menu action
- `NewConnectionInGroup` context-menu action (folder "新建连接", presets `group_id`)
- `NewRootConnection` action

Group assignment stays exactly as today (preset via the folder context-menu trigger, or
changed afterward by dragging a connection into a folder) — no group picker is added to the
form itself; that was correctly excluded from nyaterm's "missing" list for caracal.

### Icon picker: dropdown list, not a 2D grid

nyaterm's icon picker is a popover with icons arranged in a grid. gpui-component's available
popover primitive in this codebase (`DropdownButton` + `PopupMenuItem`, already used for
`saved_connections.rs`'s serial-port picker) is list-oriented, not grid-oriented, and
`PopupMenuItem::icon(impl Into<Icon>)` supports an icon alongside each item's label. Rather
than build a custom grid-layout popover against unproven gpui-component APIs, the picker is
a `DropdownButton` showing the currently-selected icon (or a default) that opens a vertical
list of `PopupMenuItem`s, each with an icon + label, one per `resolve_icon()`-supported key:
"终端" (terminal), "笔记本" (laptop/local), "服务器" (server), "网络" (network), "telnet",
"串口" (serial) — plus "自动" at the top, which sets `icon: None` (falls back to
`conn_type`-based auto-resolution, today's behavior). This is a deliberate simplification of
nyaterm's grid visual, not a missing feature — same information, list layout.

### SSH private-key authentication

**Backend** (`src/terminal/ssh.rs`): `SshConfig.password: String` becomes
`SshConfig.auth: SshAuth`:

```rust
pub enum SshAuth {
    Password(String),
    PrivateKey { path: String, passphrase: Option<String> },
}
```

`connect_and_auth` branches on `auth`: the existing `authenticate_password` call for
`SshAuth::Password`; for `SshAuth::PrivateKey`, `russh::keys::load_secret_key(&path,
passphrase.as_deref())` (a `russh`-bundled function — no new `Cargo.toml` dependency;
decrypts a passphrase-protected key in the same call) then
`session.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), None))`.
`None` for the hash algorithm is the correct default for non-RSA keys (ed25519/ecdsa, the
common case today) and falls back to legacy SHA-1 for RSA — matching `russh`'s own
documented default; per-key hash-algorithm negotiation is not in scope.

**Data model** (`src/config.rs`): `SavedConnection` gains `#[serde(default)]` fields
`auth_method: String` ("password" | "key", default "password" for backward compatibility
with existing `connections.toml` files that predate this change), `private_key_path:
Option<String>`, `private_key_passphrase: Option<String>` (⚠️ same plaintext-on-disk caveat
as the existing `password` field — no new disclosure needed, the module doc's existing
`SECURITY` note already covers "fields on this struct persist in plaintext" generally, but
the implementer should confirm that note reads correctly for the new fields too).
`to_ssh_config()` builds `SshAuth::Password`/`SshAuth::PrivateKey` from these.

**UI** (in the new window's SSH tab): an auth-method pill row (密码 / 密钥, same pill visual
already used elsewhere in this codebase), toggling between the existing password `Input` and
a private-key section: a path `Input` + a "浏览..." button using `cx.prompt_for_paths`
(already proven in `sftp.rs`'s upload flow — `files: true, directories: false, multiple:
false`) to fill the path via the native OS file picker, plus an optional passphrase `Input`
(masked, like the existing password field).

## Component structure

- `src/panels/new_connection_window.rs` — `NewConnectionWindow` (the standalone window's
  root view): the ported `ConnForm`-equivalent state (renamed to fit its new home, e.g.
  `NewConnectionForm` or kept as an internal struct — implementer's call), the icon-picker
  dropdown, the auth-method pill for SSH, group_id/edit_ix carried through unchanged from
  today's `ConnForm`/`open_new_connection_form` semantics.
- `src/panels/saved_connections.rs` — loses the inline-form code listed above; gains
  `new_connection_window: Option<WindowHandle<Root>>`, `open_new_connection_window(&mut
  self, group_id: Option<String>, edit_ix: Option<usize>, window: &mut Window, cx: &mut
  Context<Self>)` (open-or-focus, mirrors `Workspace::open_settings`), and
  `pub(crate) fn upsert_connection(&mut self, conn: SavedConnection, edit_ix:
  Option<usize>, cx: &mut Context<Self>)`.
- `src/config.rs` — `SavedConnection`'s new auth fields; `to_ssh_config()` updated.
- `src/terminal/ssh.rs` — `SshConfig.auth: SshAuth` (replacing `password: String`);
  `connect_and_auth` branches on it.

## Testing

- `src/config.rs`: extend the existing test module — a round-trip test for the new
  `auth_method`/`private_key_path`/`private_key_passphrase` fields, and a backward-compat
  test (an old-format TOML connection entry with no auth fields at all still deserializes,
  defaulting to `auth_method = "password"`), matching the file's existing
  `old_config_without_new_fields_still_deserializes` test shape.
- `src/terminal/ssh.rs`: `SshAuth`'s construction/branching logic isn't independently
  unit-testable without a live SSH server (matches the existing `connect_and_auth`/
  `SshSession` code having no unit tests today — it's exercised by the manual smoke test,
  not `cargo test`).
- The new window's UI (`new_connection_window.rs`) isn't unit-tested, consistent with the
  rest of the codebase's `panels/*.rs` render code.
- Manual smoke test must cover: create an SSH connection with password auth (regression
  check), create one with a private key (unencrypted and, if the tester has one, passphrase-
  protected), edit an existing connection and confirm fields are pre-filled correctly
  including auth method, set a custom icon and confirm it persists and displays, confirm the
  toolbar/hover-edit/context-menu/root-new triggers all still work identically to before.

## Non-goals

- No jump-host chaining, 2FA/OTP, or per-connection SSH algorithm preferences — these need
  substantially more `russh::client::Config` work than private-key auth did and remain a
  separate future item if ever prioritized.
- No group picker in the form (unchanged from today — group assignment via folder-context
  trigger or drag-and-drop).
- No icon-picker grid layout — list-with-icons instead (see Decisions).
- No SSH agent / OpenSSH certificate authentication (russh supports both, but neither was
  requested).
- No changes to Local/Telnet/Serial forms beyond being relocated into the new window
  unchanged.
