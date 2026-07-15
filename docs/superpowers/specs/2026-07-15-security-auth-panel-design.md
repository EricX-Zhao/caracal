# 安全认证 (Security & Auth) panel: shared saved keys and passwords

## Background

Caracal's left activity bar already has an unimplemented `PanelId::Security`
stub (icon `AppIcon::SecurityAuth`, label "安全 / 认证") — a placeholder
left over from before the [[encrypted-credential-storage-feature]] work.
That work added a shared, named `ssh_keys` store (so one physical key
reused across many servers isn't duplicated per-connection) but no
equivalent shared store for *passwords*, and no dedicated UI for managing
either — only an inline picker inside the new-connection form.

The request: implement that stub for real as a place to browse/add/edit/
delete both saved SSH keys and saved passwords, rename its label to
"安全认证", and extend the new-connection form so a password can be either
typed directly or picked from the saved list — the same way key auth
already works, but password auth never had a "saved" mode at all.

As reference, this session's earlier research into
[nyaterm](https://github.com/nyakang/nyaterm)'s actual frontend (not just
its backend, which the earlier encrypted-storage work already covered) was
revisited: its Security/Auth panel has Keys/Passwords/OTP/Credentials
tabs (only Keys and Passwords are in scope here); its new-connection form
uses a nested Direct/Saved tab for password auth, with a "Manage
passwords"/"Manage keys" shortcut pinned inside the saved-picker dropdown
that opens the full management screen inline.

## Decisions (confirmed with user)

- **Placement**: the new panel is purely for per-item key/password CRUD.
  The existing Settings → Security tab (vault-wide "forget saved unlock" /
  "reset vault" actions) stays exactly as-is and is not touched — matches
  nyaterm's own separation between app-level security settings and a
  dedicated credential-management screen.
- **Password linking**: a connection using a saved password stores a
  *reference* (`password_id`), not a copy — mirrors the existing
  `private_key_id` → `ssh_keys` design exactly, so rotating a password
  shared across many servers updates every connection using it in one
  place.
- **New-connection form**: Password becomes a 2-way tab — **Direct**
  (today's plain input, unchanged) vs **Saved** (pick from the list, with
  an "Add new saved password..." row that expands an inline name+password
  mini-form so a user never has to leave the form). The existing Key
  picker is restyled to the same 2-way tab shape (**Import new** /
  **Saved**) for visual consistency between the two fields — a pure UI
  reshuffle of the key picker, no data-model change (keys have no "direct"
  mode; they never embedded content per-connection to begin with).
- **Delete-in-use**: deleting a key or password that's still referenced by
  one or more connections shows a confirm dialog naming the count ("Used
  by 3 connections — deleting leaves them unable to connect until you pick
  a different one. Delete anyway?"), then allows it — matches the existing
  connection-delete confirm pattern and the fact that a dangling reference
  already fails cleanly (no panic) at connect time.
- **Panel layout**: two tabs, **Keys** and **Passwords**, in the narrow
  left-sidebar panel — matches the Settings window's own tab-bar pattern
  already in the codebase, and nyaterm's own tab structure (minus the
  OTP/autofill tabs, which are out of scope).
- **Add/edit UI**: a small dialog (reusing the exact `open_alert_dialog`
  pattern already built for the vault unlock/setup dialogs), not an inline
  form — consistent with caracal's existing precedent, and the narrow
  sidebar panel doesn't have to cram a multi-field form into limited
  width.
- **Password reveal toggle**: every password-type input this feature
  touches (the connection form's Direct password field, the key
  passphrase field, and the inline add-saved-password mini-form) gets an
  eye-icon show/hide toggle. `gpui-component`'s `Input` already has this
  built in as a single builder call — `Input::new(&state).mask_toggle()`
  — no custom icon/state code needed.

## Data model

New type in `config.rs`, additive to the existing schema:

```rust
/// A named, shared password. Connections reference one by `id` (like
/// SshKeyEntry already works) instead of only ever embedding their own —
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

`AppConfig` gains `saved_passwords: Vec<SavedPasswordEntry>`, parallel to
the existing `ssh_keys: Vec<SshKeyEntry>`.

`SavedConnection` gains `password_id: Option<String>`. Semantics for
`auth_method == "password"`:
- `password_id: Some(id)` → **Saved mode**: resolve `id` against
  `AppConfig.saved_passwords`, decrypt that entry's value at connect time.
- `password_id: None` → **Direct mode** (today's behavior, unchanged):
  decrypt the connection's own `encrypted_password`.

`to_ssh_config()` gains this same branch for password resolution that key
auth already has for `private_key_id` — a dangling `password_id`
(referenced entry deleted) fails cleanly with a clear error, exactly like
a dangling key reference already does, never a panic.

## The 安全认证 panel

**Ownership**: `SecurityAuthPanel` (new, replacing `StubPanel` for
`PanelId::Security`) holds a `WeakEntity<SessionsPanel>`, exactly like
`NewConnectionWindow` already does — `SessionsPanel` stays the single
source of truth for `connections`/`ssh_keys`/`saved_passwords` (and the
sole thing that calls `persist()`). The new panel reads a snapshot at
construction and re-syncs via `cx.observe(&sessions_panel, ...)`, so it
stays correct if `ssh_keys`/`saved_passwords` changes elsewhere (e.g.
someone imports a key from the new-connection form while this panel is
also open). Mutations (add/edit/delete) go through new methods on
`SessionsPanel`:
- `add_ssh_key` (already exists), plus new `update_ssh_key(id, name)` and
  `remove_ssh_key(id)`.
- New `add_saved_password(entry)`, `update_saved_password(id, name,
  encrypted_password)`, `remove_saved_password(id)`.

Each ends in `persist()`, matching the existing pattern.

**Layout**: two tabs, **Keys** and **Passwords**. Each tab is a bordered
list + an "Add" button.

- **Keys tab**: rows show the key's `name`; row actions are rename (small
  dialog, name field only — re-importing content means picking a new
  file via "Add key" again, not editing raw key bytes in place) and
  delete. "Add key" opens a small dialog: name + "Import key file..."
  (reusing the exact file-picker flow already built in
  `new_connection_window.rs`) + optional passphrase.
- **Passwords tab**: rows show `name` + a reveal/hide eye toggle (decrypts
  on demand, matches the existing precedent of decrypting for pre-fill
  when editing a connection) + copy button. Row actions: edit (small
  dialog: name + password with `.mask_toggle()`, pre-filled decrypted) and
  delete. "Add password" opens the same dialog, blank.

**Delete-in-use check**: before showing the delete-confirm dialog, scan
`connections` for `private_key_id == Some(this_id)` (Keys tab) or
`password_id == Some(this_id)` (Passwords tab); if any match, the dialog
body includes the count, otherwise a plain confirm. Matches the existing
`sftp.rs` delete-confirm dialog pattern (title + description + `.confirm()`
+ `on_ok`).

## New-connection form changes

**Password field** becomes a 2-way tab, mirroring `auth_method`'s existing
pill-button pattern:

- **Direct** tab (default, matches today's behavior exactly): a masked
  password input with `.mask_toggle()`. Saving writes to
  `encrypted_password`, `password_id = None`.
- **Saved** tab: a list of saved passwords (button-per-row, same visual
  style as the Key picker) to select one, plus an **"Add new saved
  password..."** row that expands into a small inline name + password
  (with `.mask_toggle()`) mini-form. Confirming it calls
  `SessionsPanel::add_saved_password` immediately (so it's usable right
  away) and selects it for this connection. Saving the connection writes
  `password_id = Some(id)`; `encrypted_password` is left empty/unused.

**Key field** (currently a flat list-of-buttons + "Import key file...")
gets restyled to the same 2-way tab shape for consistency:

- **Import new** tab: the existing "pick a file" flow (unchanged logic,
  just moved under its own tab instead of always being visible below the
  list).
- **Saved** tab: the list of existing keys to pick from (today's
  list-of-buttons, unchanged).

Picking a key or a freshly-imported key still sets `private_key_id`
exactly as it does today — this section is a pure UI reshuffle, no
data-model change.

**Editing an existing connection**: pre-fill logic extends naturally — if
`password_id` is `Some`, the form opens on the Saved tab with that entry
pre-selected; if `None`, it opens on Direct with the decrypted password
filled in (today's behavior, now also with `.mask_toggle()`). The Key
field's tab choice when editing is always effectively "Saved" (connections
have no "direct" key mode) — a minor, intentional asymmetry between the
two fields, not a bug.

**Dangling reference on open**: since delete-in-use is allowed (previous
section), a connection's `password_id`/`private_key_id` can point at an
entry that no longer exists by the time it's next edited. In that case the
form opens on the Saved tab with *nothing* pre-selected (not an error, not
a crash) — the user picks a different saved entry or a fresh Direct
value/import before saving, same as if they'd never picked one.

## Persistence & error handling

`saved_passwords` round-trips through `persist()` / `export_connections` /
`vault::migrate` / `vault::reset` / `vault::import_merge` exactly like
`ssh_keys` already does — every place that currently touches `ssh_keys`
gets a parallel `saved_passwords` line:
- `vault::reset` additionally clears every connection's `password_id`
  (alongside the existing `private_key_id`/`encrypted_password` clearing).
- `vault::import_merge` dedups saved passwords by decrypted-content hash,
  same as it already does for keys, and remaps `password_id` references
  the same way it already remaps `private_key_id`.

A dangling `password_id` (referenced entry deleted, or a hand-edited file)
surfaces as a clean connect-time error — "the saved password this
connection uses was not found" — mirroring the existing dangling-key-
reference error, never a panic.

## Non-goals

- OTP/TOTP management and terminal-prompt autofill "credentials" (both
  present in nyaterm's panel, neither requested here).
- A "used by N connections" indicator shown at rest in the list (only
  surfaced reactively at delete time, per the decision above) — YAGNI
  unless requested later.
- Any change to how the master password / vault unlock itself works —
  this feature only adds shared, referenceable secrets *within* an
  already-unlocked vault.
