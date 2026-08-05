# Cloud backup & sync: one-way WebDAV backup/restore

Date: 2026-08-05
Files under change: new `src/webdav.rs`, `src/settings.rs`, `src/panels/settings_window.rs`,
`Cargo.toml`, locale files.

One of the 6 items in the README's own "Roadmap" section (distinct from
[nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md), which tracks feature parity
with NyaTerm specifically — this item has no NyaTerm equivalent). Picked out of order (ahead
of resource monitoring rounds B/C/D, which are cheaper/more-ready) per explicit user priority.

## Background

Caracal persists all of its state as three plain TOML files under `~/.caracal`
([paths.rs](../../../src/paths.rs)): `connections.toml` ([config.rs](../../../src/config.rs) —
saved connections, groups, `ssh_keys`, and the encrypted vault), `settings.toml`
([settings.rs](../../../src/settings.rs)), and `quick_commands.toml`
([quick_commands.rs](../../../src/quick_commands.rs)). There is currently no dependency on any
HTTP client crate anywhere in the project (`Cargo.toml` has `tokio` with `net`/`io-util`/`fs`
features, used by `russh`, but nothing HTTP-specific), and no archive/compression crate either.

`connections.toml`'s secrets (passwords, private keys, key passphrases) are already encrypted
at rest under a vault master key derived from the user's master password
([encrypted-credential-storage feature](../../superpowers/specs/2026-07-15-encrypted-credential-storage-design.md),
done). `settings.toml`/`quick_commands.toml` hold no secrets. This matters for this feature:
the bundle this feature uploads is already safe to hand to a third-party server as-is, with no
new encryption layer to design.

`VaultKey` ([workspace.rs](../../../src/workspace.rs)) is a `gpui` global holding the unwrapped
`MasterKey` for the current session, readable from any panel via
`cx.try_global::<VaultKey>()` — the same mechanism [settings_window.rs](../../../src/panels/settings_window.rs)'s
Security tab already uses to gate vault-dependent actions.

## Decisions (confirmed with user)

### Scope: one-way backup/restore, not multi-device sync

Explicitly not real bidirectional sync. No automatic conflict resolution, no merge logic. The
user manually triggers an upload ("backup now") or a download+overwrite ("restore"). This is
the same simplification precedent every prior roadmap item has applied to NyaTerm's richer
originals — here applied to scope instead of visuals.

### Backup content: all 3 files, bundled as one compressed archive

No per-file selection UI. A backup is one atomic unit: `connections.toml` + `settings.toml` +
`quick_commands.toml`, packed into a single `.tar.gz` archive in memory and uploaded as one
`PUT`. New dependencies: `tar` + `flate2` (`flate2`'s pure-Rust `miniz_oxide` backend, not the
C `zlib` backend — keeps the same no-C-toolchain posture the `reqwest` choice below has).
Restore is the exact inverse: download, decompress, unpack, verify all 3 expected members are
present, then swap into place.

### Trigger: manual only, no scheduled auto-backup

A "立即备份" (backup now) button and, separately, a version list with a "恢复此版本" (restore
this version) action per entry. No background timer, no auto-backup-on-save. Matches the
project's general "off/manual by default" bias for anything that reaches out to the network
(mirrors resource monitoring's `monitor_basic_enabled: false` default).

### Versioning: timestamped filenames, keep last N

`webdav_url` itself names the target directory (the user points it at wherever they want
backups to live, e.g. `https://dav.example.com/remote.php/dav/files/me/caracal-backups/`).
Each backup uploads directly into it as `<webdav_url>/<YYYYMMDD-HHMMSS>.tar.gz` — no extra
subdirectory is created per backup. `MKCOL` against `webdav_url` is attempted once before the
first-ever upload (idempotent: a 405/409 "already exists" response is treated as success, not
an error), covering the case where the directory doesn't exist yet. After a successful upload,
list existing archives via `PROPFIND` (`Depth: 1`) against `webdav_url`, sort by the
timestamp embedded in the filename, and delete (`DELETE`) the oldest entries beyond a
configurable `keep_versions` (default 5). The same listing method backs the restore UI's
version picker — this is what makes restore work on a freshly-installed machine with no local
history, which is the actual point of a "backup for disaster recovery" feature. `PROPFIND`'s
XML `multistatus` response needs `href` extraction; the implementation plan picks between a
small XML-parsing crate (e.g. `quick-xml`) and a tolerant hand-rolled extractor — not fixed by
this spec.

### WebDAV credentials: stored like any other vault-protected secret

New `[backup]` section in `settings.toml`: `webdav_url: String`, `webdav_username: String`,
`encrypted_webdav_password: String` (same `base64(nonce || ciphertext)` shape
`config.rs`'s `SavedConnection::encrypted_password` already uses, via the session's
`MasterKey`), `keep_versions: u32` (`#[serde(default = "default_keep_versions")]`, default 5).
Configuring/using backup requires the vault to already be unlocked — always true in a normal
running session, so no new unlock flow is needed. Basic Auth only (username + password); no
OAuth/Bearer WebDAV servers in scope.

### Restore requires an app restart

The restored `connections.toml` almost certainly needs a different vault-unlock password than
whatever the current session's `VaultKey` was derived from (it's from a different point in
time, possibly a different machine's vault entirely). Rather than build in-memory re-keying
machinery, a successful restore writes the 3 files to disk, then shows a "restart required"
notice — the next launch naturally re-prompts for the vault password against the newly-
restored `connections.toml`, reusing the app's existing startup unlock flow unchanged.

### HTTP client: `reqwest` with `rustls-tls`

Rather than `native-tls`, to avoid a C OpenSSL dependency on Linux/Windows builds — consistent
with the project's existing preference (`flate2`'s Rust backend above, `keyring`'s
`crypto-rust` feature already in `Cargo.toml`). `PUT`/`GET`/`DELETE` are standard reqwest
methods; `MKCOL`/`PROPFIND` are WebDAV-specific and go through
`Client::request(Method::from_bytes(b"MKCOL"), url)`-style custom-method calls.

## Component structure

- `src/webdav.rs` (new) — plain Rust, no `gpui_component` (same CLAUDE.md §1 boundary as
  `vault.rs`/`config.rs`). Holds: a `WebDavConfig` (URL/username/password, decrypted at call
  time from `settings.toml`'s stored fields; `url` names the target directory directly, no
  separate base-path field), `test_connection`, `backup_now` (idempotent `MKCOL`, then reads
  the 3 local files, tars+gzips them, `PUT`s, then prunes old versions), `list_versions` (the
  shared `PROPFIND`-based listing used by both pruning and the restore UI), `restore(version)`
  (downloads, unpacks to temp paths, verifies, swaps into place). Network calls run on the
  existing dedicated tokio thread (same pattern `SshSession` already uses for `russh`),
  reached from panel code via `cx.spawn`.
- `src/settings.rs` — new `BackupSettings` struct (the `[backup]` section above) nested into
  the existing `AppSettings`-equivalent top-level struct, following the same
  `#[serde(default)]` backward-compat pattern every prior settings addition here has used (see
  `TerminalSettings`'s `monitor_basic_*` fields for precedent).
- `src/panels/settings_window.rs` — new "备份与同步 / Backup & Sync" tab. Credential fields
  (URL/username/password/keep_versions) follow the existing draft-state + Apply/Confirm
  pattern. 测试连接/立即备份/刷新列表/恢复此版本 are immediate-action buttons outside the draft
  state, coexisting with it the same way the Security tab's 忘记解锁/重置 vault buttons already
  sit alongside that tab's other draft fields. Restore goes through a double-confirmation
  `AlertDialog`, mirroring `reset_vault`'s exact pattern (destructive, permanent,
  two-step-confirm).
- `Cargo.toml` — new dependencies: `reqwest` (rustls-tls), `tar`, `flate2` (rust backend), and
  an XML-handling crate for `PROPFIND` parsing (exact crate decided in the implementation
  plan).
- Locale files (`locales/`) — new `Settings.Backup.*` keys for the new tab, in both `en` and
  `zh-CN`, matching every other settings tab's i18n coverage.

## Testing

- `src/webdav.rs`: pure-function logic gets unit tests with no live server — timestamp
  filename formatting/parsing, version sort + prune-selection (given a list of existing
  filenames and a `keep_versions`, which get deleted), and `PROPFIND` response parsing against
  fixture XML strings (normal multistatus response, empty directory, malformed/truncated XML
  not panicking). Archive round-trip (tar+gzip 3 files in memory, then unpack, get the same 3
  files back byte-for-byte) is also a pure in-memory test, no network or disk needed.
- No integration test against a real WebDAV server — not CI-feasible. Manual verification
  against a real server (e.g. self-hosted Nextcloud) is required before calling this done:
  test connection with correct/incorrect credentials, backup now, confirm the archive appears
  server-side with the right contents, let it exceed `keep_versions` and confirm pruning,
  restore an older version and confirm the "restart required" notice, restart and confirm the
  restored vault password (not the pre-restore session's password) is what unlocks it.
- `src/settings.rs`: a backward-compat test confirming an old `settings.toml` without a
  `[backup]` table still deserializes, with `keep_versions` defaulting to 5 and no crash on a
  missing/absent WebDAV URL — matches this file's existing convention (see
  `old_settings_file_without_monitor_fields_still_deserializes`).

## Non-goals

- No bidirectional sync, no automatic conflict resolution/merge across devices — one-way
  backup/restore only.
- No scheduled/automatic backup — manual trigger only.
- No per-file selective backup/restore — always all 3 files together, as one archive.
- No additional client-side encryption of the archive beyond what `connections.toml` already
  has from the vault — the archive is only as protected as its contents already were.
- No OAuth/Bearer-token WebDAV servers — Basic Auth only.
- No hot in-memory reload after restore — a restart is always required.
- No support for restoring a backup created by a different (incompatible) version of Caracal's
  config schema — the existing `#[serde(default)]` forward-compat fields on each struct are
  the only safety net, same as local upgrades already rely on today.
