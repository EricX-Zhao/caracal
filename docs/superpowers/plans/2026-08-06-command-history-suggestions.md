# Terminal Command-History Suggestions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** As the user types in any terminal tab, show a dropdown of matching previously-typed commands for that same connection, navigable with ↑/↓ and acceptable with Tab/Enter (fills the line only, never auto-executes), backed by a new per-connection, disk-persisted history Caracal records itself.

**Architecture:** A new plain-Rust `src/command_history.rs` module owns persistence (`~/.caracal/command_history.toml`, one `Vec<String>` per connection key) and the pure prefix-matching logic. `TerminalView` (`src/terminal/view.rs`) gains a small local-keystroke tracker inside its existing `on_key_down` — since Caracal has no way to read back the shell's actual input line, it approximates "what's typed" by watching plain character/Backspace keys and treating anything else (arrows, Ctrl+A/E/U, etc.) as desyncing that approximation until the next Enter — plus a small popup rendered near the cursor using cell metrics `TerminalView` already caches for mouse handling.

**Tech Stack:** No new dependencies — `serde`/`toml` (already used by every other `~/.caracal/*.toml` file), plain gpui styling (no `gpui_component` — `terminal/view.rs`'s own CLAUDE.md §1 boundary).

## Global Constraints

- No `gpui_component` imports in `src/command_history.rs` or `src/terminal/view.rs` (CLAUDE.md §1 boundary) — persistence and terminal-input code stay plain Rust/plain gpui, callable and testable without the UI component library.
- Every new `settings.toml` field is `#[serde(default = ...)]` and covered by a backward-compat deserialize test — matches every existing field in `src/settings.rs`.
- Every new user-visible string goes into `locales/app.yml` under both `zh-CN` and `en`.
- Accepting a suggestion only fills the input line — it must never also send Enter/execute.
- Any key besides plain characters/Backspace (arrows, Ctrl+A/E/U, Home/End, Delete, PageUp/Down, F-keys, any Ctrl/Alt/Cmd combo) must hide the suggestion dropdown and keep it hidden until the next Enter, even if the user resumes plain typing before then.
- No fuzzy/substring matching, no shell-history-file reading, no cross-connection history sharing, no accept-and-execute — see the spec's Non-goals for the full list. Do not implement any of these even if it looks like a small addition.
- Full spec: [docs/superpowers/specs/2026-08-06-command-history-suggestions-design.md](../specs/2026-08-06-command-history-suggestions-design.md).

---

### Task 1: `command_history.rs` — persistence + matching logic

**Files:**
- Create: `src/command_history.rs`
- Modify: `src/main.rs` (register the module)

**Interfaces:**
- Produces (all `pub`): `command_history_path() -> PathBuf`, `load() -> HashMap<String, Vec<String>>`, `save(&HashMap<String, Vec<String>>) -> anyhow::Result<()>`, `record(key: &str, line: &str) -> anyhow::Result<Vec<String>>` (returns the updated list for that key), `load_for(key: &str) -> Vec<String>`, `matching_suggestions(entries: &[String], prefix: &str) -> Vec<String>`. Consumed by `TerminalView` starting Task 5.

- [ ] **Step 1: Create the file with the failing tests**

Create `src/command_history.rs`:

```rust
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
fn record_into(entries: &mut Vec<String>, line: &str) {
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
/// save, and return that key's updated list — so the caller (`TerminalView`)
/// can refresh its own in-memory cache without a second read. A no-op
/// (empty/duplicate) still returns the unchanged list, but skips the
/// `save()` write entirely.
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
```

- [ ] **Step 2: Register the module**

In `src/main.rs`, add `mod command_history;` alphabetically, right after `mod assets;` and before `mod config;`:

```rust
mod assets;
mod command_history;
mod config;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test command_history::tests -- --nocapture`
Expected: all 12 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/command_history.rs src/main.rs
git commit -m "feat: add command_history.rs persistence and matching logic

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 2: `TerminalSettings.command_suggestions_enabled`

**Files:**
- Modify: `src/settings.rs`

**Interfaces:**
- Produces: `TerminalSettings.command_suggestions_enabled: bool` (default `true`). Consumed by `settings_window.rs` in Task 3 and `terminal/view.rs` in Task 5.

- [ ] **Step 1: Add the failing tests**

In `src/settings.rs`, add to `default_settings_have_expected_values` (append inside the existing test, don't create a new one, matching how this test already covers every other `TerminalSettings` field in one place):

```rust
        assert_eq!(settings.terminal.font_fallback2, "Sarasa Mono SC");
        assert!(settings.terminal.command_suggestions_enabled);
```

(The new line goes right after the existing `font_fallback2` assertion, before the `appearance.theme_name` one.)

Add a new backward-compat test, right after `old_settings_file_without_font_fallback_fields_still_deserializes` (the file's last test):

```rust
    #[test]
    fn old_settings_file_without_command_suggestions_field_still_deserializes() {
        // Simulates a settings.toml written before this field existed.
        let toml_text = r#"
            [terminal]
            font_family = "Consolas"
            font_size = 16.0
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert!(settings.terminal.command_suggestions_enabled);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test settings::tests`
Expected: compile error — `command_suggestions_enabled` field doesn't exist yet.

- [ ] **Step 3: Implement**

Add the field to `TerminalSettings` (after `font_fallback2`):

```rust
    /// Second fallback font, consulted after `font_fallback1` — the CJK
    /// glyph slot.
    #[serde(default = "default_font_fallback2")]
    pub font_fallback2: String,
    /// Whether the terminal shows a dropdown of matching historical
    /// commands as the user types. On by default — see
    /// docs/superpowers/specs/2026-08-06-command-history-suggestions-design.md.
    #[serde(default = "default_true")]
    pub command_suggestions_enabled: bool,
}
```

Add the default-value helper, next to the other `default_*` functions:

```rust
fn default_true() -> bool {
    true
}
```

Add the field to `TerminalSettings`'s `impl Default` (after `font_fallback2`):

```rust
            font_fallback2: default_font_fallback2(),
            command_suggestions_enabled: default_true(),
        }
    }
}
```

Finally, fix the exhaustive `TerminalSettings` literal in `round_trip_preserves_fields` (it lists every field explicitly, so adding a struct field breaks its compile until this is added) — add one line after `font_fallback2`:

```rust
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
                scrollback_lines: 20_000,
                font_fallback1: "JetBrains Mono".to_string(),
                font_fallback2: "Symbols Nerd Font".to_string(),
                command_suggestions_enabled: false,
            },
```

And add a matching assertion right after the existing `font_fallback2` one in that same test:

```rust
        assert_eq!(parsed.terminal.font_fallback2, "Symbols Nerd Font");
        assert!(!parsed.terminal.command_suggestions_enabled);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test settings::tests`
Expected: all tests pass, including the 2 new/modified ones.

- [ ] **Step 5: Commit**

```bash
git add src/settings.rs
git commit -m "feat: add command_suggestions_enabled to settings.toml

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 3: Settings → Terminal toggle

**Files:**
- Modify: `src/panels/settings_window.rs`, `locales/app.yml`

**Interfaces:**
- Consumes: `TerminalSettings.command_suggestions_enabled` (Task 2).
- Produces: nothing later tasks depend on — this is a leaf UI addition.

- [ ] **Step 1: Add the locale key**

In `locales/app.yml`, right after the existing `scrollback_lines:` key (under `Settings:`):

```yaml
  scrollback_lines:
    zh-CN: "回滚行数"
    en: "Scrollback Lines"
  command_suggestions:
    zh-CN: "命令历史建议"
    en: "Command History Suggestions"
```

- [ ] **Step 2: Add the toggle handler**

In `src/panels/settings_window.rs`, right after `toggle_monitor_enabled` (find it via `grep -n "fn toggle_monitor_enabled"`):

```rust
    fn toggle_command_suggestions_enabled(&mut self, cx: &mut Context<Self>) {
        self.draft.terminal.command_suggestions_enabled = !self.draft.terminal.command_suggestions_enabled;
        cx.notify();
    }
```

- [ ] **Step 3: Add the switch widget**

Right after `monitor_enabled_switch` (same pattern, new method):

```rust
    fn command_suggestions_enabled_switch(&self, cx: &Context<Self>) -> impl IntoElement {
        Switch::new("settings-command-suggestions-enabled")
            .checked(self.draft.terminal.command_suggestions_enabled)
            .on_click(cx.listener(|this, _checked: &bool, _window, cx| {
                this.toggle_command_suggestions_enabled(cx);
            }))
    }
```

- [ ] **Step 4: Add it to the Terminal tab**

In `render_terminal_tab`, add a new field block at the end (after the existing `scrollback_lines` block):

```rust
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.scrollback_lines")),
                    )
                    .child(Input::new(&self.scrollback_input)),
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
                            .child(rust_i18n::t!("Settings.command_suggestions")),
                    )
                    .child(self.command_suggestions_enabled_switch(cx)),
            )
    }
```

(Only the new trailing block is added — the `scrollback_lines` block above it and everything before it is unchanged.)

- [ ] **Step 5: Build and verify**

Run: `cargo build`
Expected: succeeds. Note: `sync_inputs_to_draft`/`apply` need no changes — this field has no separate text-input validation, it's a direct boolean toggle written straight into `self.draft`, same as `monitor_basic_enabled`'s toggle (the draft's whole `terminal` struct is persisted as one unit on Apply regardless of which fields changed).

- [ ] **Step 6: Commit**

```bash
git add src/panels/settings_window.rs locales/app.yml
git commit -m "feat: add Command History Suggestions toggle to Settings → Terminal

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 4: `TerminalView` gains a stable per-connection history key

**Files:**
- Modify: `src/terminal/view.rs`, `src/workspace.rs`

**Interfaces:**
- Produces: `TerminalView.history_key: String` (private field, read in Task 5). No public API change — purely internal plumbing.

This task only threads an identity string through every constructor; it changes no behavior. Verified by `cargo build` + `cargo test` (existing tests must still pass unchanged) — no new tests, since this task adds no new logic, only a new always-set field.

- [ ] **Step 1: Add the field**

In `src/terminal/view.rs`, add to the `TerminalView` struct (right after `host_label: String,`):

```rust
    /// "user@host" label used by the connecting/failed banner text
    /// (`conn_banner_text`). Empty for non-SSH backends, which never show
    /// a banner (`remote_reconnect` gates that).
    host_label: String,
    /// Stable key identifying which connection this tab's command history
    /// belongs to — SSH reuses `SshConfig::key()` (`user@host:port`);
    /// Local is the fixed string `"local"` (one shared bucket); Telnet is
    /// `"telnet://{host}:{port}"`; Serial is `"serial://{port_name}"`. Set
    /// once at construction, never changes for this tab's lifetime
    /// (including across a `reconnect_with` reconnect — it's still the
    /// same connection). See the design spec's "Component structure".
    history_key: String,
```

- [ ] **Step 2: Thread it through `assemble` and `with_backend`**

Change `assemble`'s signature (add the new last parameter) and body:

```rust
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        focus_handle: FocusHandle,
        term: SharedTerm,
        events_tx: flume::Sender<Event>,
        drain_task: Task<()>,
        backend: Arc<dyn PtyBackend>,
        disconnect_watch: Task<()>,
        remote_reconnect: bool,
        host_label: String,
        banner: Option<ConnBanner>,
        title: String,
        history_key: String,
    ) -> Self {
        Self {
            term,
            backend,
            events_tx,
            focus_handle,
            font_config: FontConfig::default(),
            title,
            exited: false,
            last_cell_w: 0.0,
            last_cell_h: 0.0,
            last_cols: 0,
            last_rows: 0,
            last_origin_x: 0.0,
            last_origin_y: 0.0,
            selection_dragging: None,
            remote_reconnect,
            host_label,
            history_key,
            banner,
            _drain_task: drain_task,
            _disconnect_watch: disconnect_watch,
        }
    }
```

Change `with_backend`'s signature (add `history_key: String` right after `title: String`, before `make_backend`) and its tail call to `assemble`:

```rust
    fn with_backend(
        window: &mut Window,
        cx: &mut Context<Self>,
        remote_reconnect: bool,
        host_label: String,
        title: String,
        history_key: String,
        make_backend: impl FnOnce(u16, u16, flume::Sender<Vec<u8>>) -> Arc<dyn PtyBackend>,
    ) -> Self {
        let (focus_handle, term, events_tx, drain_task) = Self::base_setup(window, cx);

        let (backend, disconnect_watch) = Self::spawn_generation(
            term.clone(),
            events_tx.clone(),
            DEFAULT_COLS as u16,
            DEFAULT_ROWS as u16,
            make_backend,
            cx,
        );

        Self::assemble(
            focus_handle,
            term,
            events_tx,
            drain_task,
            backend,
            disconnect_watch,
            remote_reconnect,
            host_label,
            None,
            title,
            history_key,
        )
    }
```

- [ ] **Step 3: Update every public constructor**

`new` (add `"local".to_string(),` after `title,`):

```rust
    pub fn new(window: &mut Window, cx: &mut Context<Self>, title: String) -> Self {
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            "local".to_string(),
            |cols, rows, bytes_tx| {
                Arc::new(LocalPty::spawn(cols, rows, bytes_tx).expect("failed to spawn local pty"))
            },
        )
    }
```

`new_local_with` (same, add `"local".to_string(),` after `title,`):

```rust
    pub fn new_local_with(
        window: &mut Window,
        cx: &mut Context<Self>,
        shell: &str,
        working_dir: Option<&str>,
        title: String,
    ) -> Self {
        let shell = shell.to_string();
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            "local".to_string(),
            move |cols, rows, bytes_tx| {
                Arc::new(
                    LocalPty::spawn_with(cols, rows, bytes_tx, &shell, working_dir)
                        .expect("failed to spawn local pty"),
                )
            },
        )
    }
```

`new_ssh_shell` (gains a new `history_key: String` parameter, placed right after `host_label`):

```rust
    pub fn new_ssh_shell(
        window: &mut Window,
        cx: &mut Context<Self>,
        session: Arc<SshSession>,
        host_label: String,
        history_key: String,
        title: String,
    ) -> Self {
        Self::with_backend(
            window,
            cx,
            true,
            host_label,
            title,
            history_key,
            move |cols, rows, bytes_tx| session.open_shell(cols, rows, bytes_tx),
        )
    }
```

`new_ssh_connecting` (gains the same new parameter, placed the same way; calls `assemble` directly):

```rust
    pub fn new_ssh_connecting(
        window: &mut Window,
        cx: &mut Context<Self>,
        host_label: String,
        history_key: String,
        title: String,
    ) -> Self {
        let (focus_handle, term, events_tx, drain_task) = Self::base_setup(window, cx);
        Self::assemble(
            focus_handle,
            term,
            events_tx,
            drain_task,
            Arc::new(DeadBackend),
            Task::ready(()),
            true,
            host_label,
            Some(ConnBanner::Connecting),
            title,
            history_key,
        )
    }
```

`new_telnet` (derives its own key internally, no new external parameter — `config` already has everything):

```rust
    pub fn new_telnet(window: &mut Window, cx: &mut Context<Self>, config: TelnetConfig) -> Self {
        let title = format!("{}:{}", config.host, config.port);
        let history_key = format!("telnet://{}:{}", config.host, config.port);
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            history_key,
            move |_cols, _rows, bytes_tx| match TelnetBackend::connect(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mtelnet connect failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            },
        )
    }
```

`new_serial` (same — derives internally from `config.port`, which is already the port name like `/dev/ttyUSB0` or `COM3`):

```rust
    pub fn new_serial(window: &mut Window, cx: &mut Context<Self>, config: SerialConfig) -> Self {
        let title = config.port.clone();
        let history_key = format!("serial://{}", config.port);
        Self::with_backend(
            window,
            cx,
            false,
            String::new(),
            title,
            history_key,
            move |_cols, _rows, bytes_tx| match SerialBackend::open(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mserial open failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            },
        )
    }
```

- [ ] **Step 4: Update the two `workspace.rs` call sites that need a new argument**

`open_ssh` (`src/workspace.rs`) already computes `let key = config.key();` at the top of the function — pass it to both SSH constructors:

```rust
        let terminal = if let Some(session) = self.ssh_sessions.get(&key).cloned() {
            cx.new(|cx| {
                TerminalView::new_ssh_shell(window, cx, session, host_label.clone(), key.clone(), title.clone())
            })
        } else {
            cx.new(|cx| {
                TerminalView::new_ssh_connecting(window, cx, host_label.clone(), key.clone(), title.clone())
            })
        };
```

`open_local_with`, `open_telnet`, `open_serial` need **no changes** — `open_local_with`'s two `TerminalView::new(...)`/`TerminalView::new_local_with(...)` calls are unchanged (the `"local"` key is now hardcoded inside those constructors themselves), and `open_telnet`/`open_serial` pass their whole `config` through unchanged (the key is derived inside `new_telnet`/`new_serial` from that same `config`).

- [ ] **Step 5: Build and run the existing test suite**

Run: `cargo build`
Expected: succeeds (a `history_key` dead-code warning is expected and fine — nothing reads the field until Task 5).

Run: `cargo test --locked`
Expected: every existing test still passes unchanged — this task adds no new tests of its own.

- [ ] **Step 6: Commit**

```bash
git add src/terminal/view.rs src/workspace.rs
git commit -m "feat: give every TerminalView a stable per-connection history key

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 5: Track typed input and record it to history on Enter

**Files:**
- Modify: `src/terminal/view.rs`

**Interfaces:**
- Consumes: `history_key` (Task 4), `command_history::{load_for, record, matching_suggestions}` (Task 1), `TerminalSettings.command_suggestions_enabled` (Task 2).
- Produces: `TerminalView` fields `suggestions_enabled`, `input_buffer`, `tracking_desynced`, `suggestions: Vec<String>`, `selected_index: Option<usize>`, `history_cache: Vec<String>`. Tasks 6 and 7 both read/write `suggestions`/`selected_index`/`input_buffer`; Task 6 additionally reads `suggestions_enabled` implicitly (via `suggestions` staying empty when disabled) and `history_cache` is only ever touched by this task and Task 7's accept flow (which doesn't mutate it).

No unit tests — `TerminalView` needs a live gpui window to construct, matching this file's existing zero-test convention for anything past `FontConfig`/pure functions (see the `#[cfg(test)]` module's own comments). Verified by `cargo build` + manual smoke test in Task 8. The pure logic this task calls into (`record`, `matching_suggestions`) is already covered by Task 1's tests.

- [ ] **Step 1: Add the new state fields**

Add to the `TerminalView` struct, right after `history_key: String,` (from Task 4):

```rust
    /// Whether command-history tracking/suggestions are active for this
    /// tab at all — read once from `TerminalSettings.command_suggestions_enabled`
    /// at construction, same "read once, changes affect tabs opened
    /// afterward" convention `scrollback_lines` already uses (not live-
    /// reloaded).
    suggestions_enabled: bool,
    /// This connection's command history, loaded once at construction and
    /// kept in sync with what's on disk as this tab itself records new
    /// entries (via `record`'s returned updated list) — avoids a disk
    /// read on every keystroke. Doesn't pick up entries recorded by other
    /// tabs to the same connection in real time; an accepted limitation.
    history_cache: Vec<String>,
    /// Characters typed (plain printable keys + Backspace) since the last
    /// Enter — Caracal's own best-effort approximation of "what's on the
    /// current input line", since it has no way to read that back from
    /// the shell. See the design spec's "Input tracking" decision.
    input_buffer: String,
    /// True once a key we can't safely track (arrows, Ctrl+A/E/U, Home/
    /// End, etc.) has been pressed since the last Enter — `input_buffer`
    /// may no longer match the real line, so suggestions stay hidden
    /// until the next Enter resets everything, even if the user resumes
    /// plain typing before then.
    tracking_desynced: bool,
    /// Currently-matching history entries for `input_buffer`, most-
    /// recent-first — empty means no dropdown is shown.
    suggestions: Vec<String>,
    /// Index into `suggestions` the user has navigated to via ↑/↓, or
    /// `None` if nothing's been explicitly selected yet.
    selected_index: Option<usize>,
```

- [ ] **Step 2: Initialize them in `assemble`**

In `assemble`'s body, right before the `Self { ... }` literal:

```rust
        let settings = settings::load();
        let suggestions_enabled = settings.terminal.command_suggestions_enabled;
        let history_cache = crate::command_history::load_for(&history_key);
        Self {
```

And add the 5 new fields to the `Self { ... }` literal (right after `history_key,`):

```rust
            history_key,
            suggestions_enabled,
            history_cache,
            input_buffer: String::new(),
            tracking_desynced: false,
            suggestions: Vec::new(),
            selected_index: None,
```

- [ ] **Step 3: Add the tracking/recording logic to `on_key_down`**

In `on_key_down`, insert this new block right after the existing scrollback-navigation `if` block and right before `let mode: TermMode = *self.term.lock().mode();`:

```rust
        if self.suggestions_enabled {
            let key = ev.keystroke.key.as_str();
            if key == "enter" {
                if !self.tracking_desynced && !self.input_buffer.is_empty() {
                    match crate::command_history::record(&self.history_key, &self.input_buffer) {
                        Ok(updated) => self.history_cache = updated,
                        Err(e) => log::warn!(
                            "failed to save command history for {}: {e}",
                            self.history_key
                        ),
                    }
                }
                self.input_buffer.clear();
                self.tracking_desynced = false;
                self.suggestions.clear();
                self.selected_index = None;
                cx.notify();
                // Deliberately no `return` here — Enter must still reach
                // the PTY via the normal encode_key/send_input path below.
            } else if !self.tracking_desynced {
                let is_plain_char = !m.control
                    && !m.alt
                    && !m.platform
                    && ev.keystroke.key_char.as_deref().is_some_and(|s| !s.is_empty());
                if is_plain_char {
                    self.input_buffer.push_str(ev.keystroke.key_char.as_deref().unwrap());
                    self.suggestions = crate::command_history::matching_suggestions(
                        &self.history_cache,
                        &self.input_buffer,
                    );
                    self.selected_index = None;
                    cx.notify();
                } else if key == "backspace" && !m.control && !m.alt {
                    self.input_buffer.pop();
                    self.suggestions = crate::command_history::matching_suggestions(
                        &self.history_cache,
                        &self.input_buffer,
                    );
                    self.selected_index = None;
                    cx.notify();
                } else {
                    self.tracking_desynced = true;
                    self.suggestions.clear();
                    self.selected_index = None;
                    cx.notify();
                }
            }
        }
```

This reuses the existing `let m = &ev.keystroke.modifiers;` binding already in scope from the scrollback-navigation code right above it — don't redeclare it.

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 5: Manual smoke test**

Not automatable. Run the app (`cargo run`), open any terminal tab, type a short command and press Enter, then inspect `~/.caracal/command_history.toml` — confirm a `[hosts]` table exists with an entry for this tab's connection key containing the typed command. Type the exact same command again and press Enter — confirm the file's entry list did **not** grow (duplicate-of-last skip). Press Enter on an empty line — confirm nothing was added.

- [ ] **Step 6: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: track typed input and record command history on Enter

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 6: Render the suggestion dropdown

**Files:**
- Modify: `src/terminal/view.rs`

**Interfaces:**
- Consumes: `suggestions: Vec<String>`, `selected_index: Option<usize>` (Task 5), `cursor_position()`, `last_origin_x/y`, `last_cell_w/h` (all pre-existing).
- Produces: nothing later tasks depend on programmatically — this is purely visual. Task 7 does not need anything from this task's code, only from Task 5's state.

No unit tests (rendering, not testable without a live window). Verified by `cargo build` and a manual visual check.

- [ ] **Step 1: Add the popup to `render`**

In `impl Render for TerminalView`'s `render` method, add a new `.when(...)` block right after the existing `.when_some(banner_text, |this, text| { ... })` block (i.e. as the new last thing chained onto the root `div()`):

```rust
            .when(!self.suggestions.is_empty(), |el| {
                let (row, col) = self.cursor_position();
                let x = self.last_origin_x + col as f32 * self.last_cell_w;
                let y = self.last_origin_y + (row as f32 + 1.0) * self.last_cell_h;
                let selected = self.selected_index;
                el.child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .flex()
                        .flex_col()
                        .bg(hsla(0.0, 0.0, 0.12, 0.97))
                        .border_1()
                        .border_color(hsla(0.0, 0.0, 0.35, 1.0))
                        .rounded_sm()
                        .py_0p5()
                        .children(self.suggestions.iter().enumerate().map(|(i, s)| {
                            let is_selected = selected == Some(i);
                            div()
                                .px_2()
                                .py_0p5()
                                .text_color(hsla(0.0, 0.0, 0.9, 1.0))
                                .when(is_selected, |row_el| {
                                    row_el.bg(hsla(210.0, 0.7, 0.45, 0.35))
                                })
                                .child(s.clone())
                        })),
                )
            })
```

This uses only plain gpui styling (`hsla`, `.absolute()`, `.left()`/`.top()`, `.bg()`, `.border_1()`, `.rounded_sm()`) — no `gpui_component` theme tokens, matching this file's own boundary rule (see the banner overlay right above it, which already follows the same plain-color convention).

- [ ] **Step 2: Build and manually verify**

Run: `cargo build`
Expected: succeeds.

Run the app, open a terminal tab that already has some recorded history for its connection (from Task 5's smoke test), and start typing a prefix that matches — confirm a small popup appears just below the cursor showing the matching command(s), and that it narrows/disappears as you keep typing. Nothing should be selectable yet (that's Task 7) — ↑/↓/Tab still behave exactly as before.

- [ ] **Step 3: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: render the command-suggestion dropdown near the cursor

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 7: Dropdown keyboard interaction (navigate, accept, dismiss)

**Files:**
- Modify: `src/terminal/view.rs`

**Interfaces:**
- Consumes: `suggestions`, `selected_index`, `input_buffer` (Task 5); the render output (Task 6) is what makes this visible, but this task's logic doesn't depend on Task 6's code.
- Produces: `move_suggestion_selection(&mut self, down: bool)`, `accept_selected_suggestion(&mut self)` — private helpers, nothing later tasks consume (this is the plan's last feature task).

No unit tests (same reason as Tasks 5/6). Verified by `cargo build` and Task 8's manual checklist.

- [ ] **Step 1: Add the two helper methods**

Add these as new `TerminalView` methods — a reasonable spot is right after `send_input` (near the other small input-related helpers):

```rust
    /// Moves `selected_index` through `suggestions` — `None` -> index 0
    /// regardless of direction (first press always lands on the top
    /// suggestion), then wraps in the given direction. No-op if there are
    /// no suggestions to select.
    fn move_suggestion_selection(&mut self, down: bool) {
        if self.suggestions.is_empty() {
            return;
        }
        let len = self.suggestions.len();
        self.selected_index = Some(match self.selected_index {
            None => 0,
            Some(i) if down => (i + 1) % len,
            Some(i) => (i + len - 1) % len,
        });
    }

    /// Fills `input_buffer`/the input line with the currently-selected
    /// suggestion by sending only the unmatched suffix — `input_buffer`
    /// is always a prefix of every entry in `suggestions` by construction
    /// (`matching_suggestions` only returns prefix matches), so no
    /// backspacing is ever needed to correct what's already typed. Does
    /// **not** send Enter — accepting only fills the line (see the design
    /// spec's "Accepting a suggestion" decision).
    fn accept_selected_suggestion(&mut self) {
        let Some(idx) = self.selected_index else { return };
        let Some(full) = self.suggestions.get(idx).cloned() else { return };
        if let Some(suffix) = full.strip_prefix(&self.input_buffer) {
            if !suffix.is_empty() {
                self.send_input(suffix.as_bytes());
            }
        }
        self.input_buffer = full;
        self.suggestions.clear();
        self.selected_index = None;
    }
```

- [ ] **Step 2: Intercept the dropdown-consumed keys in `on_key_down`**

Insert this new block immediately **before** Task 5's block (the one starting `if self.suggestions_enabled { let key = ev.keystroke.key.as_str(); if key == "enter" { ...`), so these keys are consumed first and never reach that tracking logic or the final `encode_key` call:

```rust
        if self.suggestions_enabled && !self.suggestions.is_empty() {
            let key = ev.keystroke.key.as_str();
            match key {
                "up" => {
                    self.move_suggestion_selection(false);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.move_suggestion_selection(true);
                    cx.notify();
                    return;
                }
                "escape" => {
                    self.suggestions.clear();
                    self.selected_index = None;
                    cx.notify();
                    return;
                }
                "tab" | "enter" if self.selected_index.is_some() => {
                    self.accept_selected_suggestion();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
```

Note this block computes its own local `let key = ...` — a second, separate binding from the one inside Task 5's block right below it. That's intentional (each block is independently readable); don't try to share one `key` binding across both blocks.

- [ ] **Step 3: Build and manually verify**

Run: `cargo build`
Expected: succeeds.

Run the app, get the dropdown showing (as in Task 6's check), then: press ↓ a few times and confirm the highlighted row cycles through the list (wrapping at the end); press ↑ and confirm it cycles backward; press Tab or Enter while a row is highlighted and confirm the input line fills with the full suggestion (visible via the shell's own echo) **without** the command executing; press Escape at any point and confirm the dropdown closes without altering what's typed; type more characters with nothing selected and confirm Enter still submits the line normally (runs the command) rather than substituting anything.

- [ ] **Step 4: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: add keyboard navigation and accept/dismiss for the suggestion dropdown

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 8: Final verification

**Files:** none (verification only).

**Interfaces:** none.

- [ ] **Step 1: Run the full test suite**

Run: `cargo test --locked`
Expected: every test passes, including all `command_history::tests` (Task 1) and `settings::tests` (Task 2).

- [ ] **Step 2: Run a full release build**

Run: `cargo build --release --locked`
Expected: succeeds — matches this project's CI (`.github/workflows`).

- [ ] **Step 3: Regression check — basic typing is unaffected**

This plan's riskiest change is inserting new logic into `on_key_down`, which every single keystroke in every terminal tab passes through. Before testing the new feature itself, confirm nothing broke:
1. Open a local terminal, an SSH terminal, a Telnet terminal, and a Serial terminal (whichever of these you can reach) — type normal commands in each and confirm they execute correctly.
2. Confirm Ctrl+Shift+C (copy) and Ctrl+Shift+V (paste) still work.
3. Confirm Shift+PageUp/PageDown/Home/End still scroll the viewport over scrollback instead of being sent to the program.
4. Confirm Tab still sends a literal tab for shell completion when no suggestion is selected, and Ctrl+C still sends SIGINT.

- [ ] **Step 4: Full feature smoke test**

Work through the spec's Testing section in full, across at least one SSH connection and the local terminal (to confirm the per-connection-key scoping — a command typed on one should not suggest on the other):
1. Type a few distinct commands and press Enter after each, building up some history.
2. Start typing a prefix that matches 2+ of them — confirm the dropdown shows all matches, most-recently-used first, and narrows as you type more.
3. Type a prefix that matches nothing — confirm no dropdown appears.
4. With the dropdown open, press an arrow key, Ctrl+A, Ctrl+U, or Home/End — confirm the dropdown closes, and confirm it stays closed even if you then resume typing plain characters, until you press Enter.
5. Confirm Tab/Enter-with-a-selection fills the line with only the missing suffix (not retyping/backspacing anything already there) and does not execute.
6. Confirm a plain Enter with nothing arrowed-into still submits the typed line normally.
7. Open Settings → Terminal, turn the new toggle off, apply, and open a **new** tab — confirm no dropdown ever appears in that tab even when typing a matching prefix (existing already-open tabs keep behaving as before, matching the read-once-at-construction design).
8. Confirm `~/.caracal/command_history.toml` has separate entries per connection key (e.g. a local-only command doesn't show up as a suggestion in an SSH tab, and vice versa).

- [ ] **Step 5: Final commit (if Step 3/4 surfaced any fixes)**

If manual testing found and required fixing any issue, commit it separately with a `fix:` message describing exactly what was wrong — do not fold silent fixes into this task's non-existent diff. If nothing needed fixing, this task has no commit of its own.
