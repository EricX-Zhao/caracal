# Encrypted credential storage for saved connections

## Background

`~/.caracal/connections.toml` ([config.rs](../../../src/config.rs)) currently
stores every SSH connection's `password` and `private_key_passphrase` as
plaintext strings, alongside `private_key_path` (a path to a key file that
itself sits unencrypted on disk, but at least isn't duplicated into
caracal's own storage). Anyone with read access to the file — another local
user, a backup, a synced dotfiles repo, malware — gets every saved
credential in the clear. This is flagged as a known TODO in the file's own
header comment.

As reference, [nyaterm](https://github.com/nyakang/nyaterm) (a much larger
Tauri/Rust terminal app) was cloned and analyzed for how it solves the same
problem. Its approach: AES-256-GCM encryption with a two-tier key hierarchy
(password-wrapped master key, master key encrypts each secret with its own
nonce), connection metadata left plaintext while secrets are segregated,
and export/import built on top of the same encryption primitives. Its one
notable weakness — password-to-key derivation via plain SHA-256 with a
fixed prefix, no KDF stretching — is called out below as something this
design deliberately improves on. Its "forgotten password" story (secrets
are unrecoverable; a rescue tool only unlocks the *UI*, never the data) is
adopted as-is, since that's the honest trade-off real encryption makes.

## Decisions (confirmed with user)

- **Scope**: encrypt secrets at rest, plus export/import of caracal's own
  config. Importing from *other* terminal apps' formats (PuTTY, Xshell,
  etc.) is explicitly out of scope — worth its own future spec.
- **Key source**: a user-set **master password**, not OS-keyring-only. OS
  keyring entries don't travel with an exported file, and export must work
  on a different machine.
- **Key derivation**: Argon2id (OWASP-floor params), fixing nyaterm's
  un-stretched SHA-256 approach.
- **SSH key-file auth**: the vault stores the **encrypted key content**,
  not just a path — a path is meaningless after moving to another machine.
  Because one physical key is commonly reused across many servers, keys are
  a **shared, named entity** (`ssh_keys`) that connections reference by id,
  not embedded per-connection. This avoids N duplicated copies of the same
  key and means rotating a key updates every connection that uses it.
- **Adoption**: mandatory, not opt-in. Migration runs once on first launch
  after upgrade — an opt-in toggle would leave most users unprotected.
- **Unlock timing**: once at startup (unlocks the whole session), not
  lazily per-connection. Simpler mental model, fewer prompts scattered
  through a session.
- **Convenience unlock**: opt-in only. A "Remember on this device" checkbox
  stashes the raw master key in the OS keyring (Keychain / Credential
  Manager / Secret Service) so subsequent launches skip the password
  prompt. This is a deliberate, user-visible trade-off — anyone with access
  to the unlocked OS account gets instant access, same threat model as a
  browser's "remember this password." The password-derived encryption
  remains canonical regardless; the keyring entry is a local-only shortcut,
  never used for export.

## Data model

`connections.toml` stays the single file, gains a `[vault]` section and an
`[[ssh_keys]]` list; three fields on `SavedConnection` change:

```toml
[vault]
vault_id = "<uuid>"              # namespaces the OS keyring entry
kdf = "argon2id"
salt = "<base64, 16 bytes>"
kdf_mem_kib = 19456
kdf_time = 2
kdf_parallelism = 1
wrapped_master_key = "<base64: nonce || ciphertext>"

[[ssh_keys]]
id = "<uuid>"
name = "id_ed25519 (imported 2026-07-15)"
source_path = "/home/user/.ssh/id_ed25519"   # informational only; supports a "reload from disk" action
encrypted_content = "<base64: nonce || ciphertext>"

[[connections]]
host = "example.com"
user = "root"
auth_method = "key"
private_key_id = "<uuid>"               # replaces private_key_path
encrypted_password = "<base64>"         # replaces password
encrypted_key_passphrase = "<base64>"   # replaces private_key_passphrase, optional
# name, port, group_id, conn_type, sort_order, etc. — unchanged, plaintext
```

Metadata (host/user/port/name/groups) stays plaintext, matching nyaterm's
split and keeping the file diffable/greppable/hand-editable for anything
non-sensitive. `SavedConnection.private_key_path` is removed (moved into
`SshKeyEntry.source_path`). New types: `SshKeyEntry { id, name, source_path,
encrypted_content }`, `AppConfig.ssh_keys: Vec<SshKeyEntry>`.
`to_ssh_config()` becomes fallible (`Result`, requires the unlocked master
key) instead of infallible, since it now needs to decrypt.

**Rejected alternatives** (considered against this):
- *Separate `secrets.enc` file*, mirroring nyaterm's table split literally
  — rejected because two files must stay in sync by connection id, and the
  "keep metadata separately inspectable" benefit is marginal for a
  single-user desktop file. Also breaks the "one well-known folder, easy to
  find/back up" philosophy already documented in
  [paths.rs](../../../src/paths.rs).
- *Whole-file envelope encryption* (encrypt the entire serialized config as
  one opaque blob) — rejected because it loses the ability to eyeball or
  diff the connection list at all, and turns any single-bit corruption into
  total data loss instead of one field's worth.

## Crypto

AES-256-GCM (`aes-gcm` crate) throughout, 12-byte random nonce generated
fresh on every encryption operation (never reused, not even across re-saves
of the same field). Argon2id (`argon2` crate) derives a 32-byte wrapping
key from the master password + `[vault].salt`. The wrapping key unwraps a
random 32-byte master key generated once at setup; the master key encrypts
every secret (passwords, passphrases, key contents) independently with its
own nonce. A wrong password causes the AEAD tag check on
`wrapped_master_key` to fail — that failure *is* the "incorrect password"
signal; no separate verifier/canary field is needed.

New dependencies: `aes-gcm`, `argon2`, `zeroize`, `base64` (`rand`/`OsRng`
already available transitively via `aes-gcm`'s `aead` re-export). No
`keyring` dependency is required for the core encryption — only for the
opt-in convenience-unlock feature.

## Unlock flow

1. `connections.toml` has no `[vault]` section → run migration (below).
2. Otherwise, look up the OS keyring by `vault_id` first.
   - Hit → master key loaded directly, no prompt, app opens straight to the
     connection list.
   - Miss (never opted in, keyring unavailable, entry cleared) → show an
     unlock prompt.
3. Correct password → Argon2id derives the wrapping key, unwraps the master
   key, holds it in memory (`Zeroizing<[u8; 32]>`) for the process
   lifetime. An unchecked-by-default "Remember on this device" checkbox
   additionally stashes the raw key in the OS keyring.
4. Wrong password → AEAD unwrap fails, show an error, allow retry. No
   lockout/rate-limiting for v1 (local-only attack surface, Argon2id
   already makes brute force expensive).
5. No idle-lock / re-lock for v1 — once unlocked, stays unlocked for the
   rest of the running process.
6. Settings gains a "Forget saved unlock on this device" action that
   deletes only the keyring entry (falls back to password-required on next
   launch; doesn't touch encrypted data).

**Forgotten password**: unrecoverable by design, matching nyaterm. Settings
gains a clearly-labeled "Reset vault" action (double confirmation) that
discards `[vault]` and `ssh_keys` entirely, and on every connection clears
`encrypted_password`, `encrypted_key_passphrase`, and `private_key_id`
(rather than leaving them as dangling references) — this avoids every
connection immediately surfacing a "secret unreadable"/"key not found"
error right after reset. Connection *metadata* (host/user/port/names/
groups) is preserved so the list itself isn't lost — only the secrets,
which the user re-enters. Immediately after reset, the app re-enters the
first-launch migration flow (new `vault_id`/salt/master key, prompts for a
new master password) rather than leaving the file in a no-`[vault]`,
about-to-re-migrate-later state.

## Migration (first launch after upgrade)

Existing plaintext `connections.toml` loads fine (no `[vault]` section) →
app shows a one-time "Set a master password to protect your saved
connections" dialog → generates `vault_id`, salt, and a random master key
→ encrypts every existing `password`/`private_key_passphrase` in place →
for each connection using `auth_method = "key"`, reads its
`private_key_path` once, creates a shared `SshKeyEntry` (deduped by content
hash if multiple connections point at the same file), and rewrites the
connection to reference it by `private_key_id` → writes the file atomically
(temp file + rename). No plaintext backup is retained — that would defeat
the point of the migration.

## Export / Import

**Export**: "Export connections..." saves a copy of the current
(already-encrypted) `connections.toml` to a user-chosen path. No extra
crypto step — the file is already a self-contained, password-protected
unit; the same master password unlocks it on another machine.

**Import**: "Import connections..." picks a file, prompts for *that file's*
master password (independent from the currently-unlocked vault), decrypts
it standalone, then merges its `groups`/`connections`/`ssh_keys` into the
current vault — re-encrypting every secret under the *current* master key
— and saves. Connections and groups are append-only on merge (matching
nyaterm: preserve/recreate group paths by name, never overwrite existing
connections). `ssh_keys` are deduped by content hash so re-importing the
same file twice doesn't create duplicate copies of the same physical key.

## Error handling & edge cases

- **Tampered/corrupted ciphertext** on one field → AEAD decrypt fails for
  just that field. Because encryption is per-field, the rest of the vault
  stays usable; that connection shows a "secret unreadable, please
  re-enter" state instead of crashing the app or blocking everything else.
- **OS keyring unavailable at runtime** (headless Linux without Secret
  Service, denied macOS Keychain prompt, etc.) → treated as a cache miss,
  falls back to the password prompt. Never crashes, never nags a user who
  didn't opt in.
- **Dangling `private_key_id`** (references a deleted key, or a hand-edited
  file) → `to_ssh_config()` returns `Err("referenced SSH key not found")`,
  surfaced as a normal connect failure, not a panic.
- **Zeroing**: master key, wrapping key, and decrypted plaintext secrets
  are wrapped in `zeroize::Zeroizing<..>` — plain `String`/`Vec<u8>` don't
  clear their heap buffer on drop.

## Testing

- Crypto round-trip unit tests: encrypt → decrypt equality; wrong password
  → unwrap fails cleanly, doesn't panic.
- Migration test, extending the existing fixtures in `config.rs`'s test
  module: old-format plaintext `connections.toml` → migrates → every
  secret field is ciphertext, and an explicit assertion that no plaintext
  password/passphrase substring survives anywhere in the serialized output.
- Merge/import tests: two vaults with overlapping and distinct
  groups/connections/keys merge as expected; re-importing the same file
  doesn't duplicate `ssh_keys`.
- The OS-keyring path is abstracted behind a small trait so unlock-flow
  tests can inject a fake store instead of touching the real OS keyring in
  CI (real keyring backends are inconsistently available in CI
  environments, especially headless Linux).

## Non-goals / future work

- Importing from other terminal apps' proprietary formats (PuTTY, Xshell,
  MobaXterm, etc.) — separate future spec if wanted.
- Idle-lock / re-lock-after-inactivity — not needed for v1's "unlock once
  per session" model.
- Cloud sync of the vault — out of scope; export/import to a local file is
  sufficient for now.
