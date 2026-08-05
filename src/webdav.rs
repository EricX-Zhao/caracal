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
