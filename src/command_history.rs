//! Persisted per-connection command history, used to power terminal input
//! suggestions as the user types. Plain Rust — no `gpui_component` here,
//! same CLAUDE.md §1 boundary `terminal/view.rs` itself enforces.
//!
//! Stored at `~/.caracal/command_history.toml` (see `paths::app_dir`).
//!
//! See docs/superpowers/specs/2026-08-06-command-history-suggestions-design.md.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How many entries a single connection's history keeps — oldest dropped
/// once exceeded.
const MAX_ENTRIES_PER_HOST: usize = 500;

/// How many matching entries `matching_suggestions` returns at most.
const MAX_SUGGESTIONS: usize = 8;

/// The whole persisted file: connection key -> that connection's history,
/// oldest-first (newest at the end).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CommandHistoryFile {
    #[serde(default)]
    hosts: HashMap<String, Vec<String>>,
}

/// `~/.caracal/command_history.toml`.
pub fn command_history_path() -> PathBuf {
    crate::paths::app_dir().join("command_history.toml")
}

/// Load the whole file. Missing file → empty map. A parse error is logged
/// and also yields empty, so a corrupt file never crashes startup — same
/// convention as `quick_commands::load`/`config::load`.
pub fn load() -> HashMap<String, Vec<String>> {
    let path = command_history_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };
    match toml::from_str::<CommandHistoryFile>(&text) {
        Ok(file) => file.hosts,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            HashMap::new()
        }
    }
}

/// Persist the whole file, creating the parent directory if needed.
pub fn save(hosts: &HashMap<String, Vec<String>>) -> anyhow::Result<()> {
    let path = command_history_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = CommandHistoryFile { hosts: hosts.clone() };
    let text = toml::to_string_pretty(&file)?;
    std::fs::write(&path, text)?;
    Ok(())
}

/// Pure: records `line` into one connection's entry list. No-ops for an
/// empty line or a line identical to the most recent entry (no back-to-
/// back duplicate spam), otherwise appends and truncates to the oldest
/// `MAX_ENTRIES_PER_HOST` dropped.
///
/// `pub(crate)` so `TerminalView` can apply exactly these rules to its own
/// in-memory `history_cache` synchronously while the matching disk write
/// (`record`, which applies the same rules to the on-disk copy) runs on a
/// background thread — see that field's doc comment.
pub(crate) fn record_into(entries: &mut Vec<String>, line: &str) {
    if line.is_empty() {
        return;
    }
    if entries.last().map(String::as_str) == Some(line) {
        return;
    }
    entries.push(line.to_string());
    if entries.len() > MAX_ENTRIES_PER_HOST {
        let excess = entries.len() - MAX_ENTRIES_PER_HOST;
        entries.drain(..excess);
    }
}

/// I/O convenience: load the whole file, record `line` into `key`'s list,
/// save, and return that key's updated list. A no-op (empty/duplicate)
/// still returns the unchanged list, but skips the `save()` write entirely.
///
/// This does blocking disk I/O (read + parse + serialize + write), so it
/// must **not** be called on the foreground/render thread —
/// `TerminalView` dispatches it to `cx.background_spawn` and keeps its own
/// in-memory cache current with `record_into` instead of using the
/// returned list.
pub fn record(key: &str, line: &str) -> anyhow::Result<Vec<String>> {
    let mut hosts = load();
    let entries = hosts.entry(key.to_string()).or_default();
    let before = entries.len();
    let before_last = entries.last().cloned();
    record_into(entries, line);
    let changed = entries.len() != before || entries.last().cloned() != before_last;
    let updated = entries.clone();
    if changed {
        save(&hosts)?;
    }
    Ok(updated)
}

/// I/O convenience: load just one connection's history — used once when a
/// `TerminalView` is constructed, to seed its in-memory cache.
pub fn load_for(key: &str) -> Vec<String> {
    load().remove(key).unwrap_or_default()
}

/// Pure: prefix-matches `prefix` against `entries`, most-recent-first,
/// deduped (each distinct string appears once, at its most recent
/// position), capped at `MAX_SUGGESTIONS`. Empty `prefix` matches nothing
/// (an empty input line showing every historical command would be noise,
/// not a suggestion) — this also means the caller must check for at least
/// one typed character before calling.
pub fn matching_suggestions(entries: &[String], prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in entries.iter().rev() {
        if !entry.starts_with(prefix) {
            continue;
        }
        if !seen.insert(entry.clone()) {
            continue;
        }
        out.push(entry.clone());
        if out.len() >= MAX_SUGGESTIONS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_into_skips_empty_lines() {
        let mut entries = vec!["ls".to_string()];
        record_into(&mut entries, "");
        assert_eq!(entries, vec!["ls".to_string()]);
    }

    #[test]
    fn record_into_skips_duplicate_of_most_recent_entry() {
        let mut entries = vec!["ls".to_string(), "git status".to_string()];
        record_into(&mut entries, "git status");
        assert_eq!(entries, vec!["ls".to_string(), "git status".to_string()]);
    }

    #[test]
    fn record_into_appends_a_new_distinct_line() {
        let mut entries = vec!["ls".to_string()];
        record_into(&mut entries, "git status");
        assert_eq!(entries, vec!["ls".to_string(), "git status".to_string()]);
    }

    #[test]
    fn record_into_allows_a_non_consecutive_repeat() {
        // "ls" appears again after "git status" ran in between — this is
        // NOT a back-to-back duplicate, so it's recorded (both copies kept
        // — dedup for display purposes happens in `matching_suggestions`,
        // not here).
        let mut entries = vec!["ls".to_string(), "git status".to_string()];
        record_into(&mut entries, "ls");
        assert_eq!(
            entries,
            vec!["ls".to_string(), "git status".to_string(), "ls".to_string()]
        );
    }

    #[test]
    fn record_into_caps_at_max_entries_dropping_oldest() {
        let mut entries: Vec<String> = (0..MAX_ENTRIES_PER_HOST).map(|i| format!("cmd{i}")).collect();
        record_into(&mut entries, "newest");
        assert_eq!(entries.len(), MAX_ENTRIES_PER_HOST);
        assert_eq!(entries.first().unwrap(), "cmd1", "oldest entry (cmd0) must be dropped");
        assert_eq!(entries.last().unwrap(), "newest");
    }

    #[test]
    fn matching_suggestions_returns_empty_for_an_empty_prefix() {
        let entries = vec!["ls".to_string(), "git status".to_string()];
        assert!(matching_suggestions(&entries, "").is_empty());
    }

    #[test]
    fn matching_suggestions_prefix_matches_and_orders_most_recent_first() {
        let entries = vec!["git status".to_string(), "git commit".to_string(), "git push".to_string()];
        let result = matching_suggestions(&entries, "git");
        assert_eq!(
            result,
            vec!["git push".to_string(), "git commit".to_string(), "git status".to_string()]
        );
    }

    #[test]
    fn matching_suggestions_excludes_non_matching_entries() {
        let entries = vec!["ls -la".to_string(), "git status".to_string()];
        assert_eq!(matching_suggestions(&entries, "git"), vec!["git status".to_string()]);
    }

    #[test]
    fn matching_suggestions_dedups_keeping_the_most_recent_position() {
        let entries = vec!["git status".to_string(), "ls".to_string(), "git status".to_string()];
        assert_eq!(matching_suggestions(&entries, "git"), vec!["git status".to_string()]);
    }

    #[test]
    fn matching_suggestions_caps_at_max_suggestions() {
        let entries: Vec<String> = (0..20).map(|i| format!("git cmd{i}")).collect();
        let result = matching_suggestions(&entries, "git");
        assert_eq!(result.len(), MAX_SUGGESTIONS);
        assert_eq!(result[0], "git cmd19", "most recent match must come first");
    }

    #[test]
    fn load_missing_file_yields_empty_map() {
        // No filesystem setup — this only proves a parse of empty/garbage
        // text doesn't panic, matching `load`'s own missing-file branch
        // (the real "file truly doesn't exist" path isn't independently
        // testable without touching `~/.caracal`, same limitation
        // `quick_commands`/`config`'s own tests already accept).
        let file: Result<CommandHistoryFile, _> = toml::from_str("");
        assert!(file.is_ok());
        assert!(file.unwrap().hosts.is_empty());
    }

    #[test]
    fn round_trip_preserves_per_host_entries() {
        let mut hosts = HashMap::new();
        hosts.insert("root@example.com:22".to_string(), vec!["ls".to_string(), "git status".to_string()]);
        let file = CommandHistoryFile { hosts: hosts.clone() };
        let text = toml::to_string_pretty(&file).expect("serialize");
        let parsed: CommandHistoryFile = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.hosts, hosts);
    }
}
