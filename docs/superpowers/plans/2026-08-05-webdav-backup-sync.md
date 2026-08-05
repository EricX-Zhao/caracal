# WebDAV Backup & Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user manually back up Caracal's 3 config files (`connections.toml`, `settings.toml`, `quick_commands.toml`) to a WebDAV server as a timestamped `.tar.gz` archive, and manually restore an earlier archive back onto disk (requiring an app restart to take effect).

**Architecture:** A new plain-Rust `src/webdav.rs` module owns all WebDAV/archive logic (no `gpui_component` — same boundary `vault.rs`/`config.rs` already follow) and exposes 4 functions (`test_connection`, `backup_now`, `list_versions`, `restore`), each spawning a dedicated one-shot OS thread with its own single-threaded tokio runtime (mirroring `SshSession::connect`'s thread-per-connection pattern) and returning a `flume::Receiver` the gpui side awaits via `cx.spawn`. A new Settings → "备份与同步" tab (`settings_window.rs`) holds the WebDAV credential fields (draft + Apply/Confirm, matching every other tab) plus immediate-action buttons (test/backup/refresh/restore, matching the Security tab's pattern) for the network operations, which sit outside the draft lifecycle.

**Tech Stack:** `reqwest` (rustls, no C TLS dependency) for HTTP/WebDAV verbs, `tar`+`flate2` (rust backend, no C zlib dependency) for the archive, `quick-xml` for `PROPFIND` response parsing, `chrono` for timestamped filenames. All new — nothing in this list exists in the project today.

## Global Constraints

- No `gpui_component` imports in `src/webdav.rs`, `src/settings.rs`, `src/config.rs`, `src/quick_commands.rs` (CLAUDE.md §1 boundary) — network/archive/persistence logic must stay plain Rust, callable and testable with no GPUI runtime.
- New Cargo dependencies must avoid a C toolchain requirement: `reqwest` uses `rustls` (not `default-tls`/`native-tls`), `flate2` uses `rust_backend` (not the C `zlib` backend) — matches the project's existing posture (see `keyring`'s `crypto-rust` feature, `Cargo.toml`).
- Every new `settings.toml` field is `#[serde(default = ...)]` and covered by a backward-compat deserialize test — matches every existing field in `src/settings.rs`.
- Every new user-visible string goes into `locales/app.yml` under both `zh-CN` and `en` — no hardcoded UI text.
- No bidirectional sync, no scheduled/automatic backup, no per-file selective backup, no additional client-side encryption of the archive, no OAuth WebDAV auth, no hot in-memory reload after restore, no re-entry of the current master password as a restore confirmation step, no "forgot password" fix to the startup unlock dialog — see the spec's Non-goals for the full list. Do not implement any of these even if it seems like a small addition.
- Full spec: [docs/superpowers/specs/2026-08-05-webdav-backup-sync-design.md](../specs/2026-08-05-webdav-backup-sync-design.md).

---

### Task 1: Add new dependencies

**Files:**
- Modify: `Cargo.toml`

**Interfaces:**
- Produces: `reqwest`, `tar`, `flate2`, `quick-xml`, `chrono` available as crates for every later task.

- [ ] **Step 1: Add the dependency block**

Open `Cargo.toml` and insert this new block right after the existing `env_logger = "0.11"` line (end of the "Misc" group) and before `[target.'cfg(windows)'.build-dependencies]`:

```toml

# Cloud backup & sync (WebDAV) — see
# docs/superpowers/specs/2026-08-05-webdav-backup-sync-design.md.
# rustls (not the default native-tls) to avoid a C OpenSSL build
# dependency, matching this project's existing no-C-toolchain posture.
reqwest = { version = "0.13", default-features = false, features = ["rustls"] }
tar = "0.4"
# rust_backend (miniz_oxide), not the default C zlib backend — same
# no-C-toolchain reasoning as reqwest above.
flate2 = { version = "1", default-features = false, features = ["rust_backend"] }
# Parses WebDAV PROPFIND multistatus XML responses.
quick-xml = "0.41"
# Timestamped backup filenames (`20260805-153421.tar.gz`) and parsing them
# back for sorting/pruning.
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }
```

- [ ] **Step 2: Verify the project still builds**

Run: `cargo check`
Expected: succeeds (downloads the 5 new crates, no compile errors — nothing references them yet).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add WebDAV backup dependencies (reqwest, tar, flate2, quick-xml, chrono)

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 2: `webdav.rs` — archive pack/unpack

**Files:**
- Create: `src/webdav.rs`
- Modify: `src/main.rs` (register the module)

**Interfaces:**
- Produces: `pack_archive(connections: &[u8], settings: &[u8], quick_commands: &[u8]) -> anyhow::Result<Vec<u8>>`, `unpack_archive(bytes: &[u8]) -> anyhow::Result<UnpackedArchive>`, `struct UnpackedArchive { connections: Vec<u8>, settings: Vec<u8>, quick_commands: Vec<u8> }` (crate-private for now — made `pub` in Task 5 where the public API is built on top of it).

- [ ] **Step 1: Create the file with module doc comment and the failing tests**

Create `src/webdav.rs`:

```rust
//! One-way WebDAV backup/restore of the app's 3 config files, bundled as a
//! single `.tar.gz` archive. Plain Rust — no `gpui_component` here
//! (CLAUDE.md §1 boundary), same rule `vault.rs`/`config.rs` already
//! follow.
//!
//! See docs/superpowers/specs/2026-08-05-webdav-backup-sync-design.md.

use std::io::{Read, Write};

use anyhow::{Result, anyhow};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

const MEMBER_CONNECTIONS: &str = "connections.toml";
const MEMBER_SETTINGS: &str = "settings.toml";
const MEMBER_QUICK_COMMANDS: &str = "quick_commands.toml";

/// The 3 files extracted back out of a backup archive by [`unpack_archive`].
pub(crate) struct UnpackedArchive {
    pub connections: Vec<u8>,
    pub settings: Vec<u8>,
    pub quick_commands: Vec<u8>,
}

/// Packs the 3 local config files' raw bytes into an in-memory `.tar.gz`
/// archive — the exact unit `backup_now` uploads as one `PUT`.
pub(crate) fn pack_archive(connections: &[u8], settings: &[u8], quick_commands: &[u8]) -> Result<Vec<u8>> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        append_member(&mut builder, MEMBER_CONNECTIONS, connections)?;
        append_member(&mut builder, MEMBER_SETTINGS, settings)?;
        append_member(&mut builder, MEMBER_QUICK_COMMANDS, quick_commands)?;
        builder.finish()?;
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar_bytes)?;
    Ok(encoder.finish()?)
}

fn append_member(builder: &mut tar::Builder<&mut Vec<u8>>, name: &str, content: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, name, content)?;
    Ok(())
}

/// Unpacks a `.tar.gz` archive produced by [`pack_archive`]. Errors if any
/// of the 3 expected members is missing — restore is all-or-nothing (see
/// the design spec's "Backup content" decision), never a partial result.
pub(crate) fn unpack_archive(bytes: &[u8]) -> Result<UnpackedArchive> {
    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut connections = None;
    let mut settings = None;
    let mut quick_commands = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        match path.as_str() {
            MEMBER_CONNECTIONS => connections = Some(buf),
            MEMBER_SETTINGS => settings = Some(buf),
            MEMBER_QUICK_COMMANDS => quick_commands = Some(buf),
            _ => {}
        }
    }
    Ok(UnpackedArchive {
        connections: connections.ok_or_else(|| anyhow!("archive is missing {MEMBER_CONNECTIONS}"))?,
        settings: settings.ok_or_else(|| anyhow!("archive is missing {MEMBER_SETTINGS}"))?,
        quick_commands: quick_commands
            .ok_or_else(|| anyhow!("archive is missing {MEMBER_QUICK_COMMANDS}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_then_unpack_round_trips_all_three_files() {
        let archive = pack_archive(b"conn-data", b"settings-data", b"qc-data").unwrap();
        let unpacked = unpack_archive(&archive).unwrap();
        assert_eq!(unpacked.connections, b"conn-data");
        assert_eq!(unpacked.settings, b"settings-data");
        assert_eq!(unpacked.quick_commands, b"qc-data");
    }

    #[test]
    fn unpack_archive_rejects_a_tar_gz_missing_a_member() {
        // Build a tar.gz with only 2 of the 3 expected members.
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            append_member(&mut builder, MEMBER_CONNECTIONS, b"conn-data").unwrap();
            append_member(&mut builder, MEMBER_SETTINGS, b"settings-data").unwrap();
            builder.finish().unwrap();
        }
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        let archive = encoder.finish().unwrap();

        let result = unpack_archive(&archive);
        assert!(
            result.is_err(),
            "archive missing quick_commands.toml must fail, not silently succeed with an empty file"
        );
    }

    #[test]
    fn unpack_archive_rejects_garbage_bytes() {
        let result = unpack_archive(b"not a real gzip stream");
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Register the module**

In `src/main.rs`, add `mod webdav;` after `mod vault;` (alphabetical, matching the existing order):

```rust
mod vault;
mod webdav;
mod workspace;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test webdav::tests -- --nocapture`
Expected: 3 tests pass (`pack_then_unpack_round_trips_all_three_files`, `unpack_archive_rejects_a_tar_gz_missing_a_member`, `unpack_archive_rejects_garbage_bytes`).

- [ ] **Step 4: Commit**

```bash
git add src/webdav.rs src/main.rs
git commit -m "feat: add webdav.rs archive pack/unpack

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 3: `webdav.rs` — timestamped filenames + version pruning

**Files:**
- Modify: `src/webdav.rs`

**Interfaces:**
- Consumes: nothing new from Task 2.
- Produces: `struct BackupVersion { filename: String, timestamp: chrono::NaiveDateTime }` (deriving `Clone, Debug, PartialEq, Eq`), `new_backup_filename() -> String`, `parse_backup_filename(filename: &str) -> Option<chrono::NaiveDateTime>`, `versions_to_prune(versions: Vec<BackupVersion>, keep: u32) -> Vec<BackupVersion>`. `BackupVersion` and these functions are used by Task 4 and Task 5.

- [ ] **Step 1: Add the failing tests**

Append to the `tests` module in `src/webdav.rs` (inside `mod tests { ... }`, after the existing 3 tests):

```rust
    #[test]
    fn new_backup_filename_matches_the_expected_shape() {
        let name = new_backup_filename();
        assert!(name.ends_with(".tar.gz"), "got {name:?}");
        assert_eq!(name.len(), "20260805-153421.tar.gz".len());
    }

    #[test]
    fn parse_backup_filename_round_trips_a_generated_name() {
        let name = new_backup_filename();
        assert!(parse_backup_filename(&name).is_some());
    }

    #[test]
    fn parse_backup_filename_rejects_unrelated_files() {
        assert!(parse_backup_filename("readme.txt").is_none());
        assert!(parse_backup_filename("not-a-timestamp.tar.gz").is_none());
        assert!(parse_backup_filename("20260805-153421.zip").is_none());
    }

    #[test]
    fn versions_to_prune_keeps_the_newest_n_and_prunes_the_rest() {
        let ts = |s: &str| NaiveDateTime::parse_from_str(s, FILENAME_FORMAT).unwrap();
        let versions = vec![
            BackupVersion { filename: "a".to_string(), timestamp: ts("20260801-000000") },
            BackupVersion { filename: "b".to_string(), timestamp: ts("20260803-000000") },
            BackupVersion { filename: "c".to_string(), timestamp: ts("20260802-000000") },
            BackupVersion { filename: "d".to_string(), timestamp: ts("20260805-000000") },
        ];
        let pruned = versions_to_prune(versions, 2);
        let mut pruned_filenames: Vec<&str> = pruned.iter().map(|v| v.filename.as_str()).collect();
        pruned_filenames.sort();
        assert_eq!(
            pruned_filenames,
            vec!["a", "c"],
            "must prune the 2 oldest (a, c), keeping the 2 newest (b, d)"
        );
    }

    #[test]
    fn versions_to_prune_is_empty_when_under_the_keep_limit() {
        let versions = vec![BackupVersion {
            filename: "a".to_string(),
            timestamp: NaiveDateTime::parse_from_str("20260801-000000", FILENAME_FORMAT).unwrap(),
        }];
        assert!(versions_to_prune(versions, 5).is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test webdav::tests`
Expected: compile error — `BackupVersion`, `new_backup_filename`, `parse_backup_filename`, `versions_to_prune`, `FILENAME_FORMAT`, `NaiveDateTime` don't exist yet.

- [ ] **Step 3: Implement**

Add near the top of `src/webdav.rs`, after the existing `use` block (extend it) and before `const MEMBER_CONNECTIONS`:

```rust
use chrono::NaiveDateTime;
```

Then add, after the `UnpackedArchive` struct / before `pack_archive`:

```rust
/// One backup archive that exists on the server, discovered via
/// `list_versions`. `filename` alone (joined onto the configured WebDAV
/// URL) is the full path — see the design spec's "Versioning" decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BackupVersion {
    pub filename: String,
    pub timestamp: NaiveDateTime,
}

const FILENAME_FORMAT: &str = "%Y%m%d-%H%M%S";

/// Filename for a backup taken right now, e.g. `20260805-153421.tar.gz`.
fn new_backup_filename() -> String {
    format!("{}.tar.gz", chrono::Utc::now().naive_utc().format(FILENAME_FORMAT))
}

/// Parses a filename like `20260805-153421.tar.gz` back into its
/// timestamp. `None` for anything that doesn't match this exact shape
/// (e.g. a file the user dropped into the same directory by hand) — such
/// entries are silently skipped by version listing/pruning rather than
/// erroring the whole operation.
fn parse_backup_filename(filename: &str) -> Option<NaiveDateTime> {
    let stem = filename.strip_suffix(".tar.gz")?;
    NaiveDateTime::parse_from_str(stem, FILENAME_FORMAT).ok()
}

/// Given every version currently on the server (any order) and how many
/// to keep, returns the ones to delete — the oldest, beyond `keep`. Empty
/// if there are `keep` or fewer versions.
fn versions_to_prune(mut versions: Vec<BackupVersion>, keep: u32) -> Vec<BackupVersion> {
    versions.sort_by_key(|v| v.timestamp);
    let keep = keep as usize;
    if versions.len() <= keep {
        return Vec::new();
    }
    let cut = versions.len() - keep;
    versions.drain(..cut).collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test webdav::tests`
Expected: all 7 tests pass (3 from Task 2 + 4 new).

- [ ] **Step 5: Commit**

```bash
git add src/webdav.rs
git commit -m "feat: add webdav.rs backup filename timestamps and version pruning

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 4: `webdav.rs` — PROPFIND response parsing

**Files:**
- Modify: `src/webdav.rs`

**Interfaces:**
- Consumes: `BackupVersion`, `parse_backup_filename` (Task 3).
- Produces: `parse_propfind_hrefs(xml: &str) -> Vec<String>`, `versions_from_hrefs(hrefs: &[String]) -> Vec<BackupVersion>`. Used by Task 5's `list_versions`.

- [ ] **Step 1: Add the failing tests**

Append to the `tests` module:

```rust
    #[test]
    fn parse_propfind_hrefs_extracts_namespaced_hrefs() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/caracal-backups/20260801-120000.tar.gz</d:href>
  </d:response>
  <d:response>
    <d:href>/caracal-backups/20260802-120000.tar.gz</d:href>
  </d:response>
</d:multistatus>"#;
        let hrefs = parse_propfind_hrefs(xml);
        assert_eq!(
            hrefs,
            vec![
                "/caracal-backups/20260801-120000.tar.gz".to_string(),
                "/caracal-backups/20260802-120000.tar.gz".to_string(),
            ]
        );
    }

    #[test]
    fn parse_propfind_hrefs_tolerates_bare_href_with_no_namespace_prefix() {
        let xml = r#"<multistatus><response><href>/backups/x.tar.gz</href></response></multistatus>"#;
        let hrefs = parse_propfind_hrefs(xml);
        assert_eq!(hrefs, vec!["/backups/x.tar.gz".to_string()]);
    }

    #[test]
    fn parse_propfind_hrefs_returns_empty_for_an_empty_directory_listing() {
        // Only the directory's own self-referencing entry, no backup files.
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/caracal-backups/</d:href></d:response></d:multistatus>"#;
        let hrefs = parse_propfind_hrefs(xml);
        assert_eq!(hrefs, vec!["/caracal-backups/".to_string()]);
    }

    #[test]
    fn parse_propfind_hrefs_does_not_panic_on_malformed_xml() {
        let hrefs = parse_propfind_hrefs("<not even close to valid xml");
        assert!(hrefs.is_empty());
    }

    #[test]
    fn versions_from_hrefs_filters_out_non_backup_entries() {
        // The directory's own href (no filename matching the timestamp
        // shape) must be silently dropped, not error the whole listing.
        let hrefs = vec![
            "/caracal-backups/".to_string(),
            "/caracal-backups/20260801-120000.tar.gz".to_string(),
        ];
        let versions = versions_from_hrefs(&hrefs);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].filename, "20260801-120000.tar.gz");
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test webdav::tests`
Expected: compile error — `parse_propfind_hrefs`/`versions_from_hrefs` don't exist yet.

- [ ] **Step 3: Implement**

Extend the `use` block at the top of `src/webdav.rs`:

```rust
use quick_xml::events::Event;
use quick_xml::name::LocalName;
use quick_xml::reader::Reader;
```

Add, after `versions_to_prune`:

```rust
/// Parses a WebDAV `PROPFIND` `multistatus` XML response, extracting every
/// `<href>` value's text content. Tolerant of namespace prefixes
/// (`d:href`, `D:href`, a bare `href`) since WebDAV server implementations
/// vary here — matches on local name only. Never panics: malformed XML
/// just yields whatever was successfully parsed before the error (possibly
/// empty), never an `Err` the caller has to handle.
fn parse_propfind_hrefs(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut hrefs = Vec::new();
    let mut in_href = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if local_name_is(e.local_name(), "href") => in_href = true,
            Ok(Event::End(e)) if local_name_is(e.local_name(), "href") => in_href = false,
            Ok(Event::Text(t)) if in_href => {
                if let Ok(text) = t.decode() {
                    hrefs.push(text.into_owned());
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    hrefs
}

fn local_name_is(name: LocalName<'_>, expected: &str) -> bool {
    name.as_ref() == expected.as_bytes()
}

/// Converts raw `PROPFIND` hrefs into [`BackupVersion`]s: takes the last
/// path segment of each href as a filename, keeping only the ones that
/// match the `new_backup_filename` timestamp shape — this is what silently
/// drops the directory's own self-referencing href (and anything a user
/// dropped into the same directory by hand) from the version list.
fn versions_from_hrefs(hrefs: &[String]) -> Vec<BackupVersion> {
    hrefs
        .iter()
        .filter_map(|href| {
            let filename = href.trim_end_matches('/').rsplit('/').next()?.to_string();
            let timestamp = parse_backup_filename(&filename)?;
            Some(BackupVersion { filename, timestamp })
        })
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test webdav::tests`
Expected: all 12 tests pass (7 from Tasks 2-3 + 5 new).

- [ ] **Step 5: Commit**

```bash
git add src/webdav.rs
git commit -m "feat: add webdav.rs PROPFIND response parsing

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 5: `webdav.rs` — network operations + public API

**Files:**
- Modify: `src/webdav.rs`

**Interfaces:**
- Consumes: `pack_archive`/`unpack_archive`/`UnpackedArchive` (Task 2), `BackupVersion`/`new_backup_filename`/`versions_to_prune` (Task 3), `parse_propfind_hrefs`/`versions_from_hrefs` (Task 4).
- Produces (all `pub`, this is the module's external API consumed by `settings_window.rs` in Tasks 8-11):
  - `struct WebDavConfig { pub url: String, pub username: String, pub password: String }` (`Clone, Debug`)
  - `struct RestoredFiles { pub connections: Vec<u8>, pub settings: Vec<u8>, pub quick_commands: Vec<u8> }`
  - `pub fn test_connection(config: WebDavConfig) -> flume::Receiver<Result<()>>`
  - `pub fn list_versions(config: WebDavConfig) -> flume::Receiver<Result<Vec<BackupVersion>>>`
  - `pub fn backup_now(config: WebDavConfig, keep_versions: u32, connections: Vec<u8>, settings: Vec<u8>, quick_commands: Vec<u8>) -> flume::Receiver<Result<String>>` (the `String` is the created filename)
  - `pub fn restore(config: WebDavConfig, version: BackupVersion) -> flume::Receiver<Result<RestoredFiles>>`
  - `BackupVersion` becomes `pub` (was `pub(crate)`; the version picker UI needs it).

No unit tests in this step — matches this codebase's existing convention of no unit tests for live-network/live-session code (see `terminal/ssh.rs`'s `SessionCmd` handlers, `panels/monitor.rs`'s poll loop). Verified by `cargo build` and later by manual testing against a real server (Task 12).

- [ ] **Step 1: Widen `BackupVersion`'s visibility**

In `src/webdav.rs`, change:

```rust
pub(crate) struct BackupVersion {
```

to:

```rust
pub struct BackupVersion {
```

- [ ] **Step 2: Implement the network layer**

Extend the `use` block at the top of `src/webdav.rs`:

```rust
use std::thread;

use reqwest::{Method, StatusCode};
```

Add, at the end of `src/webdav.rs` (before the `#[cfg(test)]` module):

```rust
/// WebDAV server connection details. `url` names the target directory
/// itself (not a server root) — every backup uploads directly into it, no
/// per-backup subdirectory. See the design spec's "Versioning" decision.
#[derive(Clone, Debug)]
pub struct WebDavConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

/// The 3 files downloaded and unpacked by a successful [`restore`], ready
/// for the caller to write to disk.
pub struct RestoredFiles {
    pub connections: Vec<u8>,
    pub settings: Vec<u8>,
    pub quick_commands: Vec<u8>,
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .build()
        .map_err(|e| anyhow!("failed to build HTTP client: {e}"))
}

fn join_url(base: &str, filename: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{filename}")
    } else {
        format!("{base}/{filename}")
    }
}

async fn do_test_connection(config: WebDavConfig) -> Result<()> {
    let client = build_client()?;
    let resp = client
        .request(Method::from_bytes(b"PROPFIND").expect("valid HTTP method"), &config.url)
        .basic_auth(&config.username, Some(&config.password))
        .header("Depth", "0")
        .send()
        .await
        .map_err(|e| anyhow!("request failed: {e}"))?;
    if resp.status().is_success() || resp.status() == StatusCode::MULTI_STATUS {
        Ok(())
    } else {
        Err(anyhow!("server returned {}", resp.status()))
    }
}

/// Idempotent: a 405 (Method Not Allowed) or 409 (Conflict) response —
/// both of which real WebDAV servers return for "this directory already
/// exists" — is treated as success, not an error.
async fn ensure_directory(client: &reqwest::Client, config: &WebDavConfig) -> Result<()> {
    let resp = client
        .request(Method::from_bytes(b"MKCOL").expect("valid HTTP method"), &config.url)
        .basic_auth(&config.username, Some(&config.password))
        .send()
        .await
        .map_err(|e| anyhow!("MKCOL request failed: {e}"))?;
    let status = resp.status();
    if status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::CONFLICT {
        Ok(())
    } else {
        Err(anyhow!("failed to create backup directory: server returned {status}"))
    }
}

async fn do_list_versions(config: WebDavConfig) -> Result<Vec<BackupVersion>> {
    let client = build_client()?;
    let resp = client
        .request(Method::from_bytes(b"PROPFIND").expect("valid HTTP method"), &config.url)
        .basic_auth(&config.username, Some(&config.password))
        .header("Depth", "1")
        .send()
        .await
        .map_err(|e| anyhow!("PROPFIND request failed: {e}"))?;
    if !(resp.status().is_success() || resp.status() == StatusCode::MULTI_STATUS) {
        return Err(anyhow!("server returned {}", resp.status()));
    }
    let body = resp.text().await.map_err(|e| anyhow!("failed to read response body: {e}"))?;
    Ok(versions_from_hrefs(&parse_propfind_hrefs(&body)))
}

async fn do_backup_now(
    config: WebDavConfig,
    keep_versions: u32,
    connections: Vec<u8>,
    settings: Vec<u8>,
    quick_commands: Vec<u8>,
) -> Result<String> {
    let client = build_client()?;
    ensure_directory(&client, &config).await?;

    let archive = pack_archive(&connections, &settings, &quick_commands)?;
    let filename = new_backup_filename();
    let target = join_url(&config.url, &filename);
    let resp = client
        .put(&target)
        .basic_auth(&config.username, Some(&config.password))
        .body(archive)
        .send()
        .await
        .map_err(|e| anyhow!("upload failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("upload failed: server returned {}", resp.status()));
    }

    // Prune old versions, best-effort — a pruning failure must never undo
    // an already-successful backup.
    if let Ok(versions) = do_list_versions(config.clone()).await {
        for old in versions_to_prune(versions, keep_versions) {
            let old_target = join_url(&config.url, &old.filename);
            let _ = client
                .delete(&old_target)
                .basic_auth(&config.username, Some(&config.password))
                .send()
                .await;
        }
    }

    Ok(filename)
}

async fn do_restore(config: WebDavConfig, version: BackupVersion) -> Result<RestoredFiles> {
    let client = build_client()?;
    let target = join_url(&config.url, &version.filename);
    let resp = client
        .get(&target)
        .basic_auth(&config.username, Some(&config.password))
        .send()
        .await
        .map_err(|e| anyhow!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("download failed: server returned {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| anyhow!("failed to read response body: {e}"))?;
    let unpacked = unpack_archive(&bytes)?;
    Ok(RestoredFiles {
        connections: unpacked.connections,
        settings: unpacked.settings,
        quick_commands: unpacked.quick_commands,
    })
}

/// Runs `fut` to completion on a fresh, dedicated OS thread with its own
/// single-threaded tokio runtime — mirrors `SshSession::connect`'s
/// thread-per-connection pattern (`terminal/ssh.rs`), but one-shot: the
/// thread exits once `fut` resolves, there's no persistent command loop.
/// Returns a receiver immediately (non-blocking to call); the result
/// arrives once the thread finishes.
fn spawn_worker<T, F>(fut: F) -> flume::Receiver<Result<T>>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T>> + Send + 'static,
{
    let (tx, rx) = flume::bounded(1);
    let spawned = thread::Builder::new().name("caracal-webdav".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(Err(anyhow!("failed to start webdav runtime: {e}")));
                return;
            }
        };
        let result = rt.block_on(fut);
        let _ = tx.send(result);
    });
    if let Err(e) = spawned {
        let (tx2, rx2) = flume::bounded(1);
        let _ = tx2.send(Err(anyhow!("failed to spawn webdav worker thread: {e}")));
        return rx2;
    }
    rx
}

/// Checks that `config` can reach the server and authenticate, without
/// uploading or changing anything (`PROPFIND`, `Depth: 0`).
pub fn test_connection(config: WebDavConfig) -> flume::Receiver<Result<()>> {
    spawn_worker(async move { do_test_connection(config).await })
}

/// Lists every backup archive currently on the server, in no particular
/// order (callers sort as needed).
pub fn list_versions(config: WebDavConfig) -> flume::Receiver<Result<Vec<BackupVersion>>> {
    spawn_worker(async move { do_list_versions(config).await })
}

/// Bundles the 3 given files' raw bytes into one archive, uploads it as a
/// new timestamped version, then prunes old versions beyond
/// `keep_versions`. Returns the new archive's filename on success.
pub fn backup_now(
    config: WebDavConfig,
    keep_versions: u32,
    connections: Vec<u8>,
    settings: Vec<u8>,
    quick_commands: Vec<u8>,
) -> flume::Receiver<Result<String>> {
    spawn_worker(async move { do_backup_now(config, keep_versions, connections, settings, quick_commands).await })
}

/// Downloads and unpacks the given version. Does **not** write anything to
/// disk — the caller decides where the 3 restored files go (see the
/// design spec's "Restore requires an app restart" decision).
pub fn restore(config: WebDavConfig, version: BackupVersion) -> flume::Receiver<Result<RestoredFiles>> {
    spawn_worker(async move { do_restore(config, version).await })
}
```

- [ ] **Step 3: Verify the project builds**

Run: `cargo build`
Expected: succeeds with no errors. Warnings about unused `pub` items are expected and fine at this point — Tasks 8-11 consume this API.

- [ ] **Step 4: Run the full existing test suite**

Run: `cargo test webdav::tests`
Expected: all 12 tests from Tasks 2-4 still pass (this task added no new unit tests).

- [ ] **Step 5: Commit**

```bash
git add src/webdav.rs
git commit -m "feat: add webdav.rs network operations (test/list/backup/restore)

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 6: `settings.rs` — `BackupSettings`

**Files:**
- Modify: `src/settings.rs`

**Interfaces:**
- Produces: `struct BackupSettings { pub webdav_url: String, pub webdav_username: String, pub encrypted_webdav_password: String, pub keep_versions: u32 }` (`Clone, Debug, Serialize, Deserialize`), nested as `AppSettings.backup: BackupSettings`. Consumed by `settings_window.rs` in Task 8.

- [ ] **Step 1: Add the failing tests**

Append to the `tests` module in `src/settings.rs` (after `old_settings_file_without_font_fallback_fields_still_deserializes`):

```rust
    #[test]
    fn default_backup_settings_have_expected_values() {
        let settings = AppSettings::default();
        assert_eq!(settings.backup.webdav_url, "");
        assert_eq!(settings.backup.webdav_username, "");
        assert_eq!(settings.backup.encrypted_webdav_password, "");
        assert_eq!(settings.backup.keep_versions, 5);
    }

    #[test]
    fn round_trip_preserves_backup_settings() {
        let settings = AppSettings {
            backup: BackupSettings {
                webdav_url: "https://dav.example.com/backups/".to_string(),
                webdav_username: "alice".to_string(),
                encrypted_webdav_password: "base64ciphertext".to_string(),
                keep_versions: 10,
            },
            ..AppSettings::default()
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.backup.webdav_url, "https://dav.example.com/backups/");
        assert_eq!(parsed.backup.webdav_username, "alice");
        assert_eq!(parsed.backup.encrypted_webdav_password, "base64ciphertext");
        assert_eq!(parsed.backup.keep_versions, 10);
    }

    #[test]
    fn old_settings_file_without_backup_table_still_deserializes() {
        // Simulates a settings.toml written before this feature existed:
        // no [backup] table at all.
        let toml_text = r#"
            [terminal]
            font_family = "Consolas"
            font_size = 16.0
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert_eq!(settings.backup.webdav_url, "");
        assert_eq!(settings.backup.keep_versions, 5);
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test settings::tests`
Expected: compile error — `BackupSettings` and `AppSettings.backup` don't exist yet.

- [ ] **Step 3: Implement**

In `src/settings.rs`, add `backup` to `AppSettings`:

```rust
pub struct AppSettings {
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub terminal: TerminalSettings,
    #[serde(default)]
    pub keybindings: KeybindingsSettings,
    #[serde(default)]
    pub backup: BackupSettings,
}
```

Add the new struct after `KeybindingsSettings` (before `/// \`~/.caracal/settings.toml\`.`):

```rust
/// WebDAV backup/restore settings, editable from Settings → Backup & Sync.
/// See docs/superpowers/specs/2026-08-05-webdav-backup-sync-design.md.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupSettings {
    /// Names the target directory itself, not a server root — a backup
    /// uploads directly into it. Empty = not configured yet.
    #[serde(default)]
    pub webdav_url: String,
    #[serde(default)]
    pub webdav_username: String,
    /// `base64(nonce || ciphertext)`, encrypted with the vault's
    /// `MasterKey` — same shape `config.rs`'s
    /// `SavedConnection::encrypted_password` already uses. Empty = no
    /// password saved yet.
    #[serde(default)]
    pub encrypted_webdav_password: String,
    /// How many timestamped backup archives to keep on the server; older
    /// ones are pruned after each successful backup.
    #[serde(default = "default_keep_versions")]
    pub keep_versions: u32,
}

fn default_keep_versions() -> u32 {
    5
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            webdav_url: String::new(),
            webdav_username: String::new(),
            encrypted_webdav_password: String::new(),
            keep_versions: default_keep_versions(),
        }
    }
}
```

Finally, fix the existing exhaustive struct literal in `round_trip_preserves_fields` (it lists every `AppSettings` field explicitly, with no `..Default::default()` — adding a new field breaks its compile until this is added):

```rust
    #[test]
    fn round_trip_preserves_fields() {
        let settings = AppSettings {
            general: GeneralSettings {
                language: "en".to_string(),
            },
            appearance: AppearanceSettings {
                theme_name: "Ayu Light".to_string(),
                font_family: "JetBrains Mono".to_string(),
                font_fallback: "Symbols Nerd Font".to_string(),
            },
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
                scrollback_lines: 20_000,
                font_fallback1: "JetBrains Mono".to_string(),
                font_fallback2: "Symbols Nerd Font".to_string(),
            },
            keybindings: KeybindingsSettings::default(),
            backup: BackupSettings::default(),
        };
```

(Only the `keybindings: KeybindingsSettings::default(),` line gains a new `backup: BackupSettings::default(),` line right after it — every other line in that test is unchanged.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test settings::tests`
Expected: all tests in `src/settings.rs` pass, including the 3 new ones.

- [ ] **Step 5: Commit**

```bash
git add src/settings.rs
git commit -m "feat: add BackupSettings to settings.toml

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 7: Locale strings

**Files:**
- Modify: `locales/app.yml`

**Interfaces:**
- Produces: every `Settings.Backup.*` locale key Tasks 8-11 reference via `rust_i18n::t!(...)`.

- [ ] **Step 1: Add the tab label key**

In `locales/app.yml`, under the existing `Settings:` top-level key, add a new tab label right after `tab_shortcuts` (before `General:`):

```yaml
  tab_shortcuts:
    zh-CN: "快捷键"
    en: "Shortcuts"
  tab_backup:
    zh-CN: "备份与同步"
    en: "Backup & Sync"
```

- [ ] **Step 2: Add the `Settings.Backup` section**

Still under `Settings:`, add a new `Backup:` nested key. Insert it right after the existing `Shortcuts:` block ends (i.e. right before the `theme_label:` key that currently follows `Shortcuts:`):

```yaml
  Backup:
    vault_locked_notice:
      zh-CN: "密码库已锁定，无法配置或使用备份功能"
      en: "The vault is locked — backup can't be configured or used right now"
    webdav_url_label:
      zh-CN: "WebDAV 目录地址"
      en: "WebDAV directory URL"
    webdav_username_label:
      zh-CN: "用户名"
      en: "Username"
    webdav_password_label:
      zh-CN: "密码"
      en: "Password"
    keep_versions_label:
      zh-CN: "保留版本数"
      en: "Versions to keep"
    keep_versions_invalid:
      zh-CN: "保留版本数必须是 1-100 之间的整数"
      en: "Versions to keep must be an integer between 1 and 100"
    vault_locked_error:
      zh-CN: "密码库已锁定，无法保存密码"
      en: "The vault is locked — can't save the password"
    url_required:
      zh-CN: "请先填写 WebDAV 目录地址"
      en: "Enter a WebDAV directory URL first"
    test_button:
      zh-CN: "测试连接"
      en: "Test Connection"
    test_success:
      zh-CN: "连接成功"
      en: "Connection succeeded"
    test_failed:
      zh-CN: "连接失败：%{error}"
      en: "Connection failed: %{error}"
    backup_now_button:
      zh-CN: "立即备份"
      en: "Backup Now"
    backup_success:
      zh-CN: "备份成功：%{filename}"
      en: "Backup succeeded: %{filename}"
    backup_failed:
      zh-CN: "备份失败：%{error}"
      en: "Backup failed: %{error}"
    read_local_failed:
      zh-CN: "读取本地配置文件失败：%{error}"
      en: "Failed to read local config file: %{error}"
    versions_label:
      zh-CN: "服务器上的备份版本"
      en: "Backups on the server"
    refresh_button:
      zh-CN: "刷新列表"
      en: "Refresh"
    versions_empty:
      zh-CN: "尚未获取到任何备份版本，点击"刷新列表"查看"
      en: "No versions loaded yet — click Refresh to check"
    list_failed:
      zh-CN: "获取备份列表失败：%{error}"
      en: "Failed to list backups: %{error}"
    restore_button:
      zh-CN: "恢复此版本"
      en: "Restore"
    restore_confirm_title:
      zh-CN: "恢复此版本？"
      en: "Restore this version?"
    restore_confirm_body:
      zh-CN: "这将用 %{filename} 覆盖本机的连接、设置与快捷命令配置。"
      en: "This will overwrite this device's connections, settings, and quick commands with %{filename}."
    restore_confirm_title_2:
      zh-CN: "确认恢复"
      en: "Confirm restore"
    restore_confirm_body_2:
      zh-CN: "恢复后需重启，并需输入这份备份创建时的主密码（可能与当前密码不同）。若忘记该密码，将无法进入应用。"
      en: "A restart is required after restoring, and you'll need to enter that backup's own master password (which may differ from your current one). If you don't remember it, you won't be able to get back into the app."
    restore_success_restart_required:
      zh-CN: "恢复成功，请重启应用以生效"
      en: "Restore succeeded — restart the app for it to take effect"
    restore_failed:
      zh-CN: "恢复失败：%{error}"
      en: "Restore failed: %{error}"
    restore_write_failed:
      zh-CN: "写入本地文件失败：%{error}"
      en: "Failed to write local files: %{error}"
```

- [ ] **Step 3: Verify the YAML is well-formed and the app still builds**

Run: `cargo build`
Expected: succeeds. (`rust_i18n` validates keys lazily at `t!()` call sites, which don't exist yet — this step is really just confirming the YAML itself didn't break parsing for the whole file; a quick `python3 -c "import yaml; yaml.safe_load(open('locales/app.yml'))"` or any YAML validator also works if available.)

- [ ] **Step 4: Commit**

```bash
git add locales/app.yml
git commit -m "docs: add Settings.Backup locale strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 8: `settings_window.rs` — Backup tab skeleton (draft fields + Apply/Confirm)

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `crate::webdav::WebDavConfig` (Task 5, used starting Task 9 — not yet in this task), `crate::settings::BackupSettings` (Task 6), `crate::workspace::VaultKey` (existing).
- Produces: `SettingsTab::Backup` variant; `SettingsWindow` fields `webdav_url_input`, `webdav_username_input`, `webdav_password_input`, `keep_versions_input: Entity<InputState>`; `render_backup_tab(&self, cx: &mut Context<Self>) -> impl IntoElement`. Tasks 9-11 add action buttons to this same tab and read these same 4 input fields via `current_webdav_config`.

This task makes the tab appear, be fillable, and Apply/Confirm/Cancel correctly — no network actions yet (those are Tasks 9-11). Testable end-to-end on its own: open Settings → Backup & Sync, type a URL/username/password/keep-versions, Apply, close and reopen Settings, confirm the URL/username/keep-versions are still there and the password re-populates (decrypted).

- [ ] **Step 1: Add the `Backup` tab variant**

In `SettingsTab`'s enum and both its `impl` methods:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Appearance,
    Terminal,
    Security,
    Shortcuts,
    Backup,
}
```

```rust
    fn id_key(self) -> &'static str {
        match self {
            SettingsTab::General => "general",
            SettingsTab::Appearance => "appearance",
            SettingsTab::Terminal => "terminal",
            SettingsTab::Security => "security",
            SettingsTab::Shortcuts => "shortcuts",
            SettingsTab::Backup => "backup",
        }
    }

    fn label(self) -> SharedString {
        match self {
            SettingsTab::General => rust_i18n::t!("Settings.tab_general").into(),
            SettingsTab::Appearance => rust_i18n::t!("Settings.tab_appearance").into(),
            SettingsTab::Terminal => rust_i18n::t!("Settings.tab_terminal").into(),
            SettingsTab::Security => rust_i18n::t!("Settings.tab_security").into(),
            SettingsTab::Shortcuts => rust_i18n::t!("Settings.tab_shortcuts").into(),
            SettingsTab::Backup => rust_i18n::t!("Settings.tab_backup").into(),
        }
    }
```

- [ ] **Step 2: Add a `keep_versions` parser**

Near the top of the file, after `parse_scrollback_lines`:

```rust
/// Parse the Backup tab's keep-versions field. Rejects non-positive and
/// unreasonably large values — 1 keeps just the latest backup, 100 is a
/// generous upper bound (more than that is almost certainly a typo).
fn parse_keep_versions(text: &str) -> Option<u32> {
    let value: u32 = text.trim().parse().ok()?;
    if (1..=100).contains(&value) {
        Some(value)
    } else {
        None
    }
}
```

- [ ] **Step 3: Add the 4 new input fields to `SettingsWindow`**

In the struct definition:

```rust
pub struct SettingsWindow {
    workspace: WeakEntity<Workspace>,
    committed: AppSettings,
    draft: AppSettings,
    active_tab: SettingsTab,
    font_size_input: Entity<InputState>,
    monitor_interval_input: Entity<InputState>,
    scrollback_input: Entity<InputState>,
    webdav_url_input: Entity<InputState>,
    webdav_username_input: Entity<InputState>,
    webdav_password_input: Entity<InputState>,
    keep_versions_input: Entity<InputState>,
    error: Option<SharedString>,
    recording: Option<String>,
    record_focus: FocusHandle,
}
```

In `SettingsWindow::new`, after the existing `scrollback_input` construction:

```rust
        let scrollback_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.terminal.scrollback_lines.to_string())
        });
        let webdav_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.backup.webdav_url.clone())
                .placeholder("https://dav.example.com/remote.php/dav/files/me/caracal-backups/")
        });
        let webdav_username_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.backup.webdav_username.clone())
        });
        // Decrypt the saved password (if any) to re-populate the field on
        // open, mirroring how every other draft field seeds from
        // `committed`. `None` (vault locked, or nothing saved yet, or a
        // corrupt value) just leaves the field empty rather than erroring
        // — the user can always re-type it.
        let initial_webdav_password = cx
            .try_global::<crate::workspace::VaultKey>()
            .and_then(|key| key.0.decrypt_str(&committed.backup.encrypted_webdav_password).ok())
            .unwrap_or_default();
        let webdav_password_input = cx.new(|cx| {
            InputState::new(window, cx).masked(true).default_value(initial_webdav_password)
        });
        let keep_versions_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.backup.keep_versions.to_string())
        });
```

And in the `Self { ... }` literal that follows, add the 4 new fields (alongside the existing `scrollback_input,`):

```rust
        Self {
            workspace,
            draft: committed.clone(),
            committed,
            active_tab: SettingsTab::Appearance,
            font_size_input,
            monitor_interval_input,
            scrollback_input,
            webdav_url_input,
            webdav_username_input,
            webdav_password_input,
            keep_versions_input,
            error: None,
            recording: None,
            record_focus: cx.focus_handle(),
        }
```

- [ ] **Step 4: Wire validation + persistence into `sync_inputs_to_draft`**

In `sync_inputs_to_draft`, right before the trailing `self.error = None; true`:

```rust
        self.draft.backup.webdav_url = self.webdav_url_input.read(cx).value().to_string();
        self.draft.backup.webdav_username = self.webdav_username_input.read(cx).value().to_string();

        // Leaving the password field empty keeps whatever was already
        // saved (a freshly-decrypted field is legitimately empty when
        // nothing's been configured yet, or when the vault is locked —
        // that must not silently wipe a previously-saved password).
        let webdav_password_text = self.webdav_password_input.read(cx).value().to_string();
        if !webdav_password_text.is_empty() {
            match cx.try_global::<crate::workspace::VaultKey>() {
                Some(key) => {
                    self.draft.backup.encrypted_webdav_password = key.0.encrypt_str(&webdav_password_text);
                }
                None => {
                    self.error = Some(rust_i18n::t!("Settings.Backup.vault_locked_error").into());
                    return false;
                }
            }
        }

        let keep_versions_text = self.keep_versions_input.read(cx).value();
        let Some(keep_versions) = parse_keep_versions(&keep_versions_text) else {
            self.error = Some(rust_i18n::t!("Settings.Backup.keep_versions_invalid").into());
            return false;
        };
        self.draft.backup.keep_versions = keep_versions;

        self.error = None;
        true
    }
```

(Remove the old trailing `self.error = None; true` — it's now the last two lines of this same block, unchanged in content, just after the new code above them.)

- [ ] **Step 5: Add the tab button and render dispatch**

In the `Render` impl's sidebar `.child(self.tab_button(SettingsTab::Shortcuts, cx))`, add a new line right after it:

```rust
                            .child(self.tab_button(SettingsTab::Shortcuts, cx))
                            .child(self.tab_button(SettingsTab::Backup, cx)),
```

(This replaces the trailing `,` that used to end the `Shortcuts` line — `Shortcuts` now ends with just `)` and `Backup` gets the trailing `,`.)

And in the `content` match:

```rust
        let content = match self.active_tab {
            SettingsTab::General => self.render_general_tab(cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_tab(cx).into_any_element(),
            SettingsTab::Terminal => self.render_terminal_tab(cx).into_any_element(),
            SettingsTab::Security => self.render_security_tab(cx).into_any_element(),
            SettingsTab::Shortcuts => self.render_shortcuts_tab(cx).into_any_element(),
            SettingsTab::Backup => self.render_backup_tab(cx).into_any_element(),
        };
```

- [ ] **Step 6: Add `render_backup_tab`**

Add this new method (a good spot is right after `render_security_tab`'s closing `}`, before `shortcut_row`):

```rust
    /// Draft-state credential fields only — the network action buttons
    /// (test/backup/refresh/restore) are added on top of this in later
    /// rounds, following the Security tab's pattern of immediate-action
    /// buttons coexisting with a tab's draft fields.
    fn render_backup_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let vault_unlocked = cx.try_global::<crate::workspace::VaultKey>().is_some();

        div()
            .flex()
            .flex_col()
            .gap_3()
            .when(!vault_unlocked, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(rust_i18n::t!("Settings.Backup.vault_locked_notice")),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.webdav_url_label")),
                    )
                    .child(Input::new(&self.webdav_url_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.webdav_username_label")),
                    )
                    .child(Input::new(&self.webdav_username_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.webdav_password_label")),
                    )
                    .child(Input::new(&self.webdav_password_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.keep_versions_label")),
                    )
                    .child(Input::new(&self.keep_versions_input)),
            )
    }
```

- [ ] **Step 7: Build and manually verify**

Run: `cargo build`
Expected: succeeds.

Run the app (`cargo run`), open Settings, confirm a new "备份与同步" tab appears, shows 4 labeled fields, and that typing values + clicking Apply + closing + reopening Settings preserves the URL/username/keep-versions (and the password re-populates, since the vault is unlocked in a normal running session).

- [ ] **Step 8: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add Backup & Sync settings tab (credential fields)

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 9: Test Connection + Backup Now buttons

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `crate::webdav::{WebDavConfig, test_connection, backup_now}` (Task 5), `crate::config::config_path`, `crate::settings::settings_path`, `crate::quick_commands::quick_commands_path` (existing).
- Produces: `SettingsWindow::current_webdav_config(&self, cx) -> crate::webdav::WebDavConfig`, `on_click_test_connection`, `on_click_backup_now`, `SettingsWindow.backup_busy: bool` field. Task 10/11 reuse `current_webdav_config` and `backup_busy`.

- [ ] **Step 1: Add `backup_busy` field**

Add to the `SettingsWindow` struct (after `keep_versions_input`):

```rust
    keep_versions_input: Entity<InputState>,
    /// True while any WebDAV action (test/backup/refresh/restore) is in
    /// flight — disables every backup-tab action button so a second click
    /// can't start an overlapping operation.
    backup_busy: bool,
```

And in `Self { ... }` in `new` (after `keep_versions_input,`):

```rust
            keep_versions_input,
            backup_busy: false,
```

- [ ] **Step 2: Add `current_webdav_config`**

Add near the top of the `impl SettingsWindow` block's helper methods (a good spot is right before `render_backup_tab`):

```rust
    /// Builds a `WebDavConfig` straight from whatever's currently typed in
    /// the 3 credential fields — every backup action button acts on the
    /// live draft, not on `committed`/`self.draft.backup`, so the user
    /// never has to hit Apply first just to test or use what they just
    /// typed.
    fn current_webdav_config(&self, cx: &Context<Self>) -> crate::webdav::WebDavConfig {
        crate::webdav::WebDavConfig {
            url: self.webdav_url_input.read(cx).value().to_string(),
            username: self.webdav_username_input.read(cx).value().to_string(),
            password: self.webdav_password_input.read(cx).value().to_string(),
        }
    }
```

- [ ] **Step 3: Add the two button handlers**

Add after `current_webdav_config`:

```rust
    fn on_click_test_connection(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.current_webdav_config(cx);
        if config.url.trim().is_empty() {
            window.push_notification(
                (NotificationType::Error, rust_i18n::t!("Settings.Backup.url_required")),
                cx,
            );
            return;
        }
        self.backup_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let rx = crate::webdav::test_connection(config);
            let result = rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.backup_busy = false;
                    match result {
                        Ok(Ok(())) => window.push_notification(
                            (NotificationType::Success, rust_i18n::t!("Settings.Backup.test_success")),
                            cx,
                        ),
                        Ok(Err(e)) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.test_failed", error = e.to_string()),
                            ),
                            cx,
                        ),
                        Err(_) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.test_failed", error = "worker thread failed"),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn on_click_backup_now(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.current_webdav_config(cx);
        if config.url.trim().is_empty() {
            window.push_notification(
                (NotificationType::Error, rust_i18n::t!("Settings.Backup.url_required")),
                cx,
            );
            return;
        }
        let keep_versions =
            parse_keep_versions(&self.keep_versions_input.read(cx).value()).unwrap_or(5);

        let connections = match std::fs::read(crate::config::config_path()) {
            Ok(bytes) => bytes,
            Err(e) => {
                window.push_notification(
                    (
                        NotificationType::Error,
                        rust_i18n::t!("Settings.Backup.read_local_failed", error = e.to_string()),
                    ),
                    cx,
                );
                return;
            }
        };
        let settings_bytes = match std::fs::read(crate::settings::settings_path()) {
            Ok(bytes) => bytes,
            Err(e) => {
                window.push_notification(
                    (
                        NotificationType::Error,
                        rust_i18n::t!("Settings.Backup.read_local_failed", error = e.to_string()),
                    ),
                    cx,
                );
                return;
            }
        };
        // A missing quick_commands.toml is normal (it's never created
        // until the user adds their first quick command) — an empty
        // member unpacks fine later, since `QuickCommandsFile`'s
        // `commands` field is `#[serde(default)]`.
        let quick_commands_bytes = std::fs::read(crate::quick_commands::quick_commands_path()).unwrap_or_default();

        self.backup_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let rx = crate::webdav::backup_now(config, keep_versions, connections, settings_bytes, quick_commands_bytes);
            let result = rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.backup_busy = false;
                    match result {
                        Ok(Ok(filename)) => window.push_notification(
                            (
                                NotificationType::Success,
                                rust_i18n::t!("Settings.Backup.backup_success", filename = filename),
                            ),
                            cx,
                        ),
                        Ok(Err(e)) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.backup_failed", error = e.to_string()),
                            ),
                            cx,
                        ),
                        Err(_) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.backup_failed", error = "worker thread failed"),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }
```

- [ ] **Step 4: Add the buttons to `render_backup_tab`**

At the end of `render_backup_tab`'s builder chain (after the `keep_versions` field's `.child(...)`, i.e. this becomes the new last `.child(...)` before the method's closing):

```rust
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Button::new("settings-backup-test")
                            .xsmall()
                            .label(rust_i18n::t!("Settings.Backup.test_button"))
                            .disabled(self.backup_busy)
                            .on_click(cx.listener(Self::on_click_test_connection)),
                    )
                    .child(
                        Button::new("settings-backup-now")
                            .xsmall()
                            .label(rust_i18n::t!("Settings.Backup.backup_now_button"))
                            .disabled(self.backup_busy)
                            .on_click(cx.listener(Self::on_click_backup_now)),
                    ),
            )
    }
```

(The method's final closing `}` moves down to after this new block — remove the old one that used to end the method right after the `keep_versions` field.)

- [ ] **Step 5: Build and manually verify**

Run: `cargo build`
Expected: succeeds.

Manual check against a real WebDAV server (e.g. a self-hosted Nextcloud instance) is deferred to Task 12's full checklist — for this task, just confirm the app builds and the two buttons render (disabled state toggling requires a real server to observe end-to-end, covered later).

- [ ] **Step 6: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add Test Connection and Backup Now buttons

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 10: Refresh + version list

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `crate::webdav::{BackupVersion, list_versions}` (Task 5), `current_webdav_config`/`backup_busy` (Task 9).
- Produces: `SettingsWindow.backup_versions: Vec<crate::webdav::BackupVersion>` field, `on_click_refresh_versions`, `version_row`. Task 11 reuses `backup_versions` and adds the restore button to each `version_row`.

- [ ] **Step 1: Add `backup_versions` field**

Add to the `SettingsWindow` struct (after `backup_busy`):

```rust
    backup_busy: bool,
    /// Populated by "刷新列表" — empty until the user has fetched at least
    /// once (not auto-fetched on tab open, since that would silently hit
    /// the network every time Settings opens).
    backup_versions: Vec<crate::webdav::BackupVersion>,
```

And in `Self { ... }` in `new` (after `backup_busy: false,`):

```rust
            backup_busy: false,
            backup_versions: Vec::new(),
```

- [ ] **Step 2: Add the refresh handler**

Add after `on_click_backup_now`:

```rust
    fn on_click_refresh_versions(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.current_webdav_config(cx);
        if config.url.trim().is_empty() {
            window.push_notification(
                (NotificationType::Error, rust_i18n::t!("Settings.Backup.url_required")),
                cx,
            );
            return;
        }
        self.backup_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let rx = crate::webdav::list_versions(config);
            let result = rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.backup_busy = false;
                    match result {
                        Ok(Ok(mut versions)) => {
                            versions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)); // newest first
                            this.backup_versions = versions;
                        }
                        Ok(Err(e)) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.list_failed", error = e.to_string()),
                            ),
                            cx,
                        ),
                        Err(_) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.list_failed", error = "worker thread failed"),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }
```

- [ ] **Step 3: Add `version_row` (without a restore button yet — Task 11 adds it)**

Add after `on_click_refresh_versions`:

```rust
    /// One row in the version list: the timestamp, human-formatted. The
    /// restore action is added to this row in a later round.
    fn version_row(&self, version: &crate::webdav::BackupVersion, cx: &Context<Self>) -> impl IntoElement {
        let label = version.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().text_sm().text_color(cx.theme().foreground).child(label))
    }
```

- [ ] **Step 4: Add the refresh button + version list to `render_backup_tab`**

At the end of `render_backup_tab`'s builder chain, after the test/backup buttons row added in Task 9:

```rust
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(rust_i18n::t!("Settings.Backup.versions_label")),
                            )
                            .child(
                                Button::new("settings-backup-refresh")
                                    .xsmall()
                                    .label(rust_i18n::t!("Settings.Backup.refresh_button"))
                                    .disabled(self.backup_busy)
                                    .on_click(cx.listener(Self::on_click_refresh_versions)),
                            ),
                    )
                    .child(if self.backup_versions.is_empty() {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.versions_empty"))
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(self.backup_versions.iter().map(|v| self.version_row(v, cx)))
                            .into_any_element()
                    }),
            )
    }
```

(As in Task 9's Step 4, the method's closing `}` moves down to after this new block.)

- [ ] **Step 5: Build and manually verify**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 6: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add version list refresh to Backup & Sync tab

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 11: Restore action

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `crate::webdav::{WebDavConfig, BackupVersion, RestoredFiles, restore}` (Task 5), `backup_versions`/`version_row`/`backup_busy` (Task 10).
- Produces: `on_click_restore`, `perform_restore`, `write_restored_files` (free function). Nothing later consumes these — this is the plan's last feature task.

- [ ] **Step 1: Add the free function that writes restored files to disk**

Add near the top of `src/panels/settings_window.rs`, after `find_keybinding_conflict` (before the `use gpui::{...}` block):

```rust
/// Writes the 3 restored files to their real paths. Each file is written
/// to a `.restore-tmp` sibling first and only renamed into place once
/// every temp write has succeeded — a rename is atomic within one
/// filesystem, so a failure partway through never leaves one file
/// restored and the other two untouched.
fn write_restored_files(files: &crate::webdav::RestoredFiles) -> anyhow::Result<()> {
    let targets = [
        (crate::config::config_path(), &files.connections),
        (crate::settings::settings_path(), &files.settings),
        (crate::quick_commands::quick_commands_path(), &files.quick_commands),
    ];
    let mut temp_paths = Vec::with_capacity(targets.len());
    for (path, content) in &targets {
        let temp_path = path.with_extension("restore-tmp");
        std::fs::write(&temp_path, content)?;
        temp_paths.push(temp_path);
    }
    for ((path, _), temp_path) in targets.iter().zip(temp_paths.iter()) {
        std::fs::rename(temp_path, path)?;
    }
    Ok(())
}
```

- [ ] **Step 2: Add `perform_restore` + `on_click_restore`**

Add after `on_click_refresh_versions` (before `version_row`):

```rust
    fn perform_restore(
        &mut self,
        config: crate::webdav::WebDavConfig,
        version: crate::webdav::BackupVersion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.backup_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let rx = crate::webdav::restore(config, version);
            let result = rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.backup_busy = false;
                    match result {
                        Ok(Ok(files)) => match write_restored_files(&files) {
                            Ok(()) => window.push_notification(
                                (
                                    NotificationType::Warning,
                                    rust_i18n::t!("Settings.Backup.restore_success_restart_required"),
                                ),
                                cx,
                            ),
                            Err(e) => window.push_notification(
                                (
                                    NotificationType::Error,
                                    rust_i18n::t!("Settings.Backup.restore_write_failed", error = e.to_string()),
                                ),
                                cx,
                            ),
                        },
                        Ok(Err(e)) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.restore_failed", error = e.to_string()),
                            ),
                            cx,
                        ),
                        Err(_) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.restore_failed", error = "worker thread failed"),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// Double-confirmation, mirroring `reset_vault`'s exact pattern
    /// (destructive, permanent, two-step-confirm) — see the design spec's
    /// "Restore requires an app restart" decision for why the second
    /// dialog's copy warns about the backup's own master password instead
    /// of asking the user to re-type their current one.
    fn on_click_restore(
        &mut self,
        version: crate::webdav::BackupVersion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let config = self.current_webdav_config(cx);
        let weak = cx.entity().downgrade();
        let filename = version.filename.clone();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let weak = weak.clone();
            let config = config.clone();
            let version = version.clone();
            let filename = filename.clone();
            alert
                .title(rust_i18n::t!("Settings.Backup.restore_confirm_title"))
                .description(rust_i18n::t!("Settings.Backup.restore_confirm_body", filename = filename))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    let weak = weak.clone();
                    let config = config.clone();
                    let version = version.clone();
                    window.open_alert_dialog(cx, move |alert, _window, _cx| {
                        let weak = weak.clone();
                        let config = config.clone();
                        let version = version.clone();
                        alert
                            .title(rust_i18n::t!("Settings.Backup.restore_confirm_title_2"))
                            .description(rust_i18n::t!("Settings.Backup.restore_confirm_body_2"))
                            .confirm()
                            .on_ok(move |_, window, cx| {
                                window.close_dialog(cx);
                                let _ = weak.update(cx, |this, cx| {
                                    this.perform_restore(config.clone(), version.clone(), window, cx);
                                });
                                true
                            })
                    });
                    true
                })
        });
    }
```

- [ ] **Step 3: Add the restore button to `version_row`**

Replace `version_row`'s body with:

```rust
    /// One row in the version list: the timestamp, human-formatted, plus
    /// a restore action.
    fn version_row(&self, version: &crate::webdav::BackupVersion, cx: &Context<Self>) -> impl IntoElement {
        let label = version.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
        let weak = cx.entity().downgrade();
        let version_for_click = version.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().text_sm().text_color(cx.theme().foreground).child(label))
            .child(
                Button::new(SharedString::from(format!("settings-backup-restore-{}", version.filename)))
                    .xsmall()
                    .danger()
                    .label(rust_i18n::t!("Settings.Backup.restore_button"))
                    .disabled(self.backup_busy)
                    .on_click(move |_ev, window, cx| {
                        let version = version_for_click.clone();
                        let _ = weak.update(cx, |this, cx| {
                            this.on_click_restore(version, window, cx);
                        });
                    }),
            )
    }
```

- [ ] **Step 4: Build and manually verify**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 5: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add restore action to Backup & Sync tab

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 12: Final verification

**Files:** none (verification only).

**Interfaces:** none.

- [ ] **Step 1: Run the full test suite**

Run: `cargo test --locked`
Expected: every test passes, including all `webdav::tests` (Tasks 2-4) and `settings::tests` (Task 6).

- [ ] **Step 2: Run a full release build**

Run: `cargo build --release --locked`
Expected: succeeds — matches this project's CI (`.github/workflows`).

- [ ] **Step 3: Manual smoke test against a real WebDAV server**

Not automatable — requires a real WebDAV server (e.g. a self-hosted Nextcloud instance) and the running app. Work through the spec's Testing section in full:

1. Open Settings → Backup & Sync, enter the server URL + credentials, click 测试连接 with correct credentials → success notification. Change the password to something wrong, test again → error notification, nothing else disturbed.
2. Click 立即备份 → success notification naming the new archive. Confirm server-side (any WebDAV client) that the archive exists at the configured URL and contains all 3 files when extracted.
3. Set 保留版本数 to 2, back up 3 more times (4 total) → confirm only the 2 newest archives remain on the server after the last backup.
4. Click 刷新列表 → confirm the version list shows the remaining archives, newest first, with human-readable timestamps.
5. Click 恢复此版本 on an older version → confirm both confirmation dialogs appear in sequence, and the second one's body text warns about needing that backup's own master password. Confirm through both → "restore succeeded, restart required" notification.
6. Restart the app → confirm the unlock prompt now requires the master password that was active when *that specific backup* was taken (not necessarily the password from just before the restore) — and that a wrong password just shows the inline error without crashing or locking anything out further.
7. Confirm `~/.caracal/connections.toml`, `settings.toml`, `quick_commands.toml` match the restored backup's contents (e.g. a saved connection that existed only in the older backup is back; one added after that backup was taken is gone).

- [ ] **Step 4: Confirm no regressions in adjacent features**

Manually open every other Settings tab (General/Appearance/Terminal/Security/Shortcuts) and confirm Apply/Cancel/Confirm still work as before — this plan touched `sync_inputs_to_draft` and the tab-dispatch `match`, both shared across every tab.

- [ ] **Step 5: Final commit (if Step 3/4 surfaced any fixes)**

If manual testing found and required fixing any issue, commit it separately with a `fix:` message describing exactly what was wrong — do not fold silent fixes into this task's non-existent diff. If nothing needed fixing, this task has no commit of its own.
