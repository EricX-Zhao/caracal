# Simplify SSH auth: password direct-only, key saved-only

## Background

The [[2026-07-15 Security & Auth panel]](2026-07-15-security-auth-panel-design.md) feature added
a shared, named "saved password" store (mirroring the existing shared `ssh_keys` store) plus a
Direct/Saved toggle on the connection form's Password field, and restyled the Key field's existing
Saved/Import-new toggle to match. This request reverses the password half of that work and
simplifies the key half: password auth goes back to always-direct input, key auth drops its
"import inline while creating a connection" convenience and only offers picking from the shared
key store. The Security & Auth panel's Keys tab and the underlying `ssh_keys` store are unaffected.

## Decisions (confirmed with user)

- **Password field**: connection form always shows a plain masked `Input` — no Direct/Saved
  toggle, no saved-password picker, no inline "add saved password" mini-form.
- **Key field**: connection form always shows the saved-key picker (dropdown over `ssh_keys`) — no
  Import-new/Saved toggle, no inline key-file import UI in the connection form itself. If
  `ssh_keys` is empty, the picker's empty state includes a button that opens the Security & Auth
  panel's Keys tab directly, so the user can add a key there and come back to finish the
  connection form.
- **Security & Auth panel**: keeps its tab-bar structure (per user preference, rather than
  collapsing to a plain content view) but reduces to a single "Keys" tab — the "Passwords" tab,
  its list/add/edit/delete UI, and its confirm-delete-in-use dialog are removed entirely.
- **Existing `password_id`-based connections get migrated, not orphaned**: any connection
  currently using the "saved password" mode has its shared password decrypted and re-encrypted
  into its own `encrypted_password` field, then `password_id` is cleared — converting it to Direct
  mode transparently. This mirrors the existing `password`/`private_key_path`
  "migration-source-only field" pattern already in `SavedConnection` (see `src/config.rs`'s doc
  comments on those fields): TOML deserialization silently drops struct fields that no longer
  exist, so removing `password_id`/`saved_passwords` outright — without migrating first — would
  silently empty the password of every connection currently in Saved mode. `password_id` and
  `AppConfig.saved_passwords` therefore stay on the structs as migration-source-only (no UI ever
  writes to them again after this change), exactly like `password`/`private_key_path` already do.
- **Migration timing**: runs once, wherever the vault becomes unlocked. There are three such
  places today ([workspace.rs](../../../src/workspace.rs)): fresh vault setup
  (`vault::migrate`, ~line 126), manual password unlock (`vault::unlock` in
  `show_unlock_dialog`, ~line 193), and the OS-keyring convenience-unlock (~line 375, inside
  `Workspace::new`'s deferred block — this path already holds the master key but not a mutable
  `AppConfig`, since `cfg` was moved into `SessionsPanel::new` earlier in the same function, so
  this path's migration operates on `SessionsPanel`'s own in-memory connections/saved-passwords
  state and persists through whatever save helper `SessionsPanel` already uses for its other
  mutations, e.g. `persist_for_security_auth`). The migration is idempotent — safe to run on every
  unlock even if a previous attempt was interrupted before saving, since it only acts on
  connections that still have `password_id: Some(_)`.
- **Orphaned saved passwords** (a `saved_passwords` entry no connection ever referenced) are
  simply discarded during migration — there's nothing to migrate them onto, and the feature that
  created them no longer exists.

## Scope

### Connection form (`src/panels/new_connection_window.rs`)

- Remove `PasswordTab` enum and `render_password_tabs`/`render_saved_password_picker`; the
  Password field's auth-method branch renders a single masked `Input`, unconditionally.
- Remove `adding_new_saved_password`, `new_saved_password_name`, `new_saved_password_value`,
  `selected_password_id`, `saved_passwords` fields and `confirm_add_new_saved_password`/
  `cancel_add_new_saved_password` methods.
- `save()`'s auth-method branch always writes the typed password into `encrypted_password` and
  leaves `password_id: None` (never set) for new/edited connections.
- Remove `KeyTab` enum's `ImportNew` variant and `render_import_new_key`; the Key field's
  auth-method branch renders the saved-key picker unconditionally. Empty-state: an inline message
  plus a button that opens Security & Auth (Keys tab) — reuses whatever mechanism the app already
  uses elsewhere to open that panel (activity-bar toggle), then this window can stay open so the
  user returns to it. The picker already needs to refresh when `ssh_keys` changes elsewhere, since
  today's Saved tab does — confirm during implementation whether that refresh is automatic
  (e.g. already observes the shared key list) or needs a manual refresh call after returning.
- Constructor drops its `saved_passwords: Vec<SavedPasswordEntry>` parameter.

### Security & Auth panel (`src/panels/security_auth.rs`)

- `AuthTab` enum keeps only `Keys` (drop `Passwords`).
- Remove `render_passwords_tab`, `open_add_or_edit_password_dialog`,
  `confirm_delete_saved_password`, `saved_passwords`/`revealed_password_ids` fields, and the
  `cx.observe` subscription that re-syncs `saved_passwords`.
- Tab bar keeps its current visual structure with the single remaining Keys button.

### Shared data/session layer

- `src/panels/sessions.rs`: remove `add_saved_password`/`update_saved_password`/
  `remove_saved_password`/`connections_using_saved_password` (no longer reachable from any UI).
  `saved_passwords` storage itself stays (read-only from here on) since the migration needs to
  read it once per connection; `SessionsPanel::new`'s `saved_passwords` parameter and field stay
  for that same reason, but no code writes into it anymore other than `import_merge`'s existing
  dedup logic (which continues to work unchanged — importing an old vault that still has
  `saved_passwords` entries is exactly the migration-source case this design accounts for; the
  migration step runs after import too, same as any other unlock).
- `src/config.rs`: `SavedConnection::to_ssh_config` keeps its `saved_passwords` parameter and
  `password_id` branch (still needed to resolve a connection *before* its own migration pass has
  run in a given session — e.g. right after `import_merge` but before the next unlock cycle
  re-triggers migration; harmless to leave functionally intact since nothing can create a *new*
  `password_id` reference anymore, only consume pre-existing ones down to zero over time).
- New migration function (name/location decided in the implementation plan, likely
  `vault::migrate_saved_passwords_to_direct` or folded into a small helper called from all three
  unlock sites): for each connection with `password_id: Some(id)`, look up `id` in
  `saved_passwords`, decrypt with the master key, `master.encrypt_str(...)` into that connection's
  own `encrypted_password`, clear `password_id`. Missing/dangling references (entry not found) are
  treated the same as today's existing dangling-reference handling in `to_ssh_config` — log and
  leave that connection's `encrypted_password` empty (user re-enters it manually), not a hard
  error that blocks unlock.

### Locales (`locales/app.yml`, `SecurityAuth:` namespace)

Remove: `tab_passwords`, `delete_password_title`, `add_password_button`, `add_password_title`,
`edit_password_title`, `tab_direct_password`, `tab_saved_password`, `add_new_saved_password_row`,
and `tab_import_new_key` (the connection-form Key field's Import-new pill label — the pill toggle
itself is gone, so this string has no caller left). Keep `tab_saved_key`: reused as the picker's
static label now that there's no toggle to switch it with. Keep (shared with the Keys tab, still
in use there after this change): `delete_confirm_body`, `delete_confirm_body_in_use`,
`edit_button`, `password_name_placeholder` — verify each one's Keys-tab usage survives before
deleting the Passwords-only keys around it.

### Tests

`config.rs`'s `to_ssh_config_resolves_a_saved_password_by_id` and
`to_ssh_config_errors_when_referenced_password_is_missing` stay (they test the resolution branch
that migration-source connections may still hit before their own migration runs). `vault.rs`'s
`reset_clears_saved_passwords_and_password_id`, `import_merge_dedups_saved_passwords_by_content...`,
and `import_merge_remaps_password_id_to_the_dest_vaults_entry` all stay unchanged — they test
`saved_passwords`/`password_id` plumbing that remains, just no longer UI-reachable for *new*
entries. Add new tests for the migration function itself: converts a `password_id`-referencing
connection to direct mode and clears the reference; is a no-op for connections already in direct
mode; handles a dangling `password_id` without erroring.

## Out of scope

- Any change to the `ssh_keys` shared-key store, its Security & Auth CRUD, or the connection
  form's saved-key picker's own behavior beyond removing the Import-new toggle around it.
- Renaming the Security & Auth panel or its activity-bar icon/label.
- Retroactively warning users about the behavior change (e.g. a one-time "your saved passwords
  were converted to direct mode" notification) — not requested; the migration is silent, matching
  how the original plaintext→vault migration is also silent.
