//! One-way WebDAV backup/restore of the app's 3 config files, bundled as a
//! single `.tar.gz` archive. Plain Rust — no `gpui_component` here
//! (CLAUDE.md §1 boundary), same rule `vault.rs`/`config.rs` already
//! follow.
//!
//! See docs/superpowers/specs/2026-08-05-webdav-backup-sync-design.md.

use std::io::{Read, Write};

use anyhow::{Result, anyhow};
use chrono::NaiveDateTime;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use quick_xml::events::Event;
use quick_xml::name::LocalName;
use quick_xml::reader::Reader;

const MEMBER_CONNECTIONS: &str = "connections.toml";
const MEMBER_SETTINGS: &str = "settings.toml";
const MEMBER_QUICK_COMMANDS: &str = "quick_commands.toml";

/// The 3 files extracted back out of a backup archive by [`unpack_archive`].
pub(crate) struct UnpackedArchive {
    pub connections: Vec<u8>,
    pub settings: Vec<u8>,
    pub quick_commands: Vec<u8>,
}

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
}
