# Resource Monitoring Round A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a real 基础 (basic system stats) resource-monitoring panel for SSH hosts, replacing the current `StubPanel` placeholder, backed by a new SSH command-exec primitive.

**Architecture:** A new `SshSession::exec_command` runs one-off non-interactive shell commands over a fresh channel on the existing SSH connection (same pattern as the shell/SFTP channels — one Session, one connection). `MonitorPanel` (new file, mirrors `SftpPanel`'s per-host-instance architecture) polls the host every N seconds with one combined shell script covering hostname/uptime/load/CPU/memory/network/disk, parses the reply with pure functions, computes CPU%/network-rate deltas across two samples, and renders simplified bars/text (not nyaterm's SVG donuts). Settings → Terminal gains an enable toggle + poll-interval field, off by default.

**Tech Stack:** Rust, GPUI (git `gpui`/`gpui_platform`), `gpui-component` (git), `russh` 0.61.2 (already a dependency, `Channel::exec`).

## Global Constraints

- No new crate dependencies.
- Linux remote hosts only — reads `/proc/*`, uses `df`. A non-Linux remote shows an error in this panel; no macOS/BSD parsing path this round.
- No destructive actions of any kind — this round is read-only stats display.
- No GPU/进程管理/Docker panels — each deferred to its own later round.
- No per-core CPU breakdown, no historical graphing/sparklines, no cross-panel "metrics bus" abstraction.
- `monitor_basic_enabled` defaults to `false`; `monitor_basic_interval_secs` defaults to `5`.
- Threshold coloring: red (`cx.theme().danger`) at ≥90%, amber (`cx.theme().warning`) at ≥70%, default (`cx.theme().primary`) otherwise.
- 3-consecutive-failure cutoff clears stale data and shows an error state.
- CPU%/network-rate rows show a "预热中…" placeholder on the first poll (no prior sample), real numbers from the second poll onward.
- Build with `cargo build` and run `cargo test` after every task; both must be clean before moving to the next task.

---

### Task 1: SSH exec primitive

**Files:**
- Modify: `src/terminal/ssh.rs` (`SessionCmd` enum ~line 187-195, `SshSession` impl ~line 224+, `command_loop` ~line 466-525)

**Interfaces:**
- Produces: `SshSession::exec_command(&self, command: String) -> flume::Receiver<Result<String>>` — consumed by Task 5 (`MonitorPanel`'s poll loop).
- Consumes: nothing from other tasks (foundation task).

- [ ] **Step 1: Add the `Exec` variant to `SessionCmd`**

In `src/terminal/ssh.rs`, change the enum (~line 187-195):

```rust
enum SessionCmd {
    OpenShell {
        cols: u16,
        rows: u16,
        bytes_tx: flume::Sender<Vec<u8>>,
        ctrl_rx: flume::Receiver<Ctrl>,
    },
    Sftp(SftpRequest),
    /// Run a one-off, non-interactive command over a fresh channel on the
    /// same connection (used for resource-monitoring polls). Not a shell —
    /// no PTY, no persistent state between calls.
    Exec {
        command: String,
        reply: flume::Sender<Result<String>>,
    },
}
```

- [ ] **Step 2: Add `SshSession::exec_command`**

In `src/terminal/ssh.rs`, add this method to `impl SshSession` (e.g. right after `sftp_rename`, near the end of the other `sftp_*` public methods):

```rust
    /// Run `command` on the remote host over a fresh, non-interactive
    /// channel (no PTY) and collect its stdout as a `String`. Used for
    /// resource-monitoring polls — a separate channel from the shell/SFTP,
    /// same connection (CLAUDE.md §2: one Session, one connection).
    pub fn exec_command(&self, command: String) -> flume::Receiver<Result<String>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Exec { command, reply });
        rx
    }
```

- [ ] **Step 3: Handle `SessionCmd::Exec` in `command_loop`**

In `src/terminal/ssh.rs`'s `command_loop` (~line 466-525), add a new match arm after `SessionCmd::Sftp(request) => { ... }` and before the `while let` loop's closing `}`:

```rust
            SessionCmd::Exec { command, reply } => {
                // Long-running-ish (network round-trip); spawn so the
                // command loop keeps servicing shell/SFTP concurrently,
                // same rationale as `OpenShell`'s `tokio::spawn`.
                let handle = handle.clone();
                tokio::spawn(async move {
                    let result = run_exec(&handle, &command).await;
                    let _ = reply.send(result);
                });
            }
```

- [ ] **Step 4: Add the `run_exec` helper**

In `src/terminal/ssh.rs`, add this function near `open_shell_channel` (e.g. right after it, ~line 543):

```rust
/// Run `command` over a fresh non-interactive channel and collect its
/// stdout. Mirrors `shell_pump`'s read loop (`ChannelMsg::Data`/
/// `ExtendedData` until `Eof`/`Close`), minus the write side — exec sends
/// no input, just runs one command and streams the reply.
async fn run_exec(handle: &Handle<ClientHandler>, command: &str) -> Result<String> {
    let mut channel = handle.channel_open_session().await?;
    channel.exec(true, command).await?;
    let mut output = Vec::new();
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { ref data }) => output.extend_from_slice(data),
            Some(ChannelMsg::ExtendedData { .. }) => {}
            Some(ChannelMsg::ExitStatus { .. }) => {}
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}
```

Note: `command_loop`'s new `Exec` arm clones `handle` (a `Handle<ClientHandler>`, already `Clone` — confirmed by `open_shell_channel`/`open_sftp` both taking `&Handle<ClientHandler>` and the existing code already cloning/reusing `handle` freely) into the spawned task, so `run_exec` doesn't need to borrow the command loop's own `handle`.

- [ ] **Step 5: Build and test**

Run: `cargo build && cargo test --lib`
Expected: clean build (the `Exec` variant's addition to `SessionCmd` doesn't force any other exhaustive `match` to be updated — unlike `SftpRequest`, `SessionCmd` is only matched once, in `command_loop` itself, which Step 3 already updated), all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/terminal/ssh.rs
git commit -m "feat: add SshSession::exec_command for one-off remote commands"
```

---

### Task 2: Settings fields

**Files:**
- Modify: `src/settings.rs` (`TerminalSettings` struct ~line 33-42, its `impl Default` ~line 60-67, test module ~line 108+)

**Interfaces:**
- Produces: `TerminalSettings.monitor_basic_enabled: bool`, `TerminalSettings.monitor_basic_interval_secs: u32` — consumed by Task 3 (Settings UI) and Task 5 (`MonitorPanel` reads them at construction).
- Consumes: nothing from other tasks.

- [ ] **Step 1: Add the two fields to `TerminalSettings`**

In `src/settings.rs`, change the struct (~line 33-42):

```rust
/// Terminal-content settings, editable from Settings → Terminal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalSettings {
    /// Empty string = bundled default (`terminal::view`'s `DEFAULT_FONT_FAMILY`).
    #[serde(default)]
    pub font_family: String,
    /// Raw point size; `TerminalView::set_font_size` takes `gpui::Pixels`, so
    /// callers convert via `px(settings.terminal.font_size)` — `Pixels`
    /// itself isn't (de)serializable, hence the raw `f32` here.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Whether the 资源监控 (basic system stats) panel polls the remote
    /// host at all. Off by default — matches nyaterm's own "all off by
    /// default" convention for remote-monitoring panels.
    #[serde(default)]
    pub monitor_basic_enabled: bool,
    /// Poll interval in seconds. Read once when a `MonitorPanel` is
    /// created; changing this in Settings takes effect for panels created
    /// afterward (not a live-reload of already-open panels).
    #[serde(default = "default_monitor_interval_secs")]
    pub monitor_basic_interval_secs: u32,
}
```

- [ ] **Step 2: Add the default function**

In `src/settings.rs`, add near `default_font_size` (~line 44-46):

```rust
fn default_monitor_interval_secs() -> u32 {
    5
}
```

- [ ] **Step 3: Update `impl Default for TerminalSettings`**

In `src/settings.rs` (~line 60-67):

```rust
impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_font_size(),
            monitor_basic_enabled: false,
            monitor_basic_interval_secs: default_monitor_interval_secs(),
        }
    }
}
```

- [ ] **Step 4: Update the existing `round_trip_preserves_fields` test**

In `src/settings.rs`'s test module, the existing test constructs a `TerminalSettings { ... }` literal that will fail to compile once the struct gains required fields — find it (~line 120-136) and update:

```rust
    #[test]
    fn round_trip_preserves_fields() {
        let settings = AppSettings {
            appearance: AppearanceSettings {
                theme_mode: "light".to_string(),
            },
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
            },
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.terminal.font_family, "Consolas");
        assert_eq!(parsed.terminal.font_size, 16.0);
        assert_eq!(parsed.terminal.monitor_basic_enabled, true);
        assert_eq!(parsed.terminal.monitor_basic_interval_secs, 10);
        assert_eq!(parsed.appearance.theme_mode, "light");
    }
```

- [ ] **Step 5: Write the backward-compat test**

In `src/settings.rs`'s test module, add a new test right after `old_settings_file_without_terminal_table_still_deserializes`:

```rust
    #[test]
    fn old_settings_file_without_monitor_fields_still_deserializes() {
        // Simulates a settings.toml written before this round: a [terminal]
        // table with only the font keys, no monitor_basic_* keys at all.
        let toml_text = r#"
            [terminal]
            font_family = "Consolas"
            font_size = 16.0
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert_eq!(settings.terminal.monitor_basic_enabled, false);
        assert_eq!(settings.terminal.monitor_basic_interval_secs, 5);
    }
```

- [ ] **Step 6: Run tests to verify**

Run: `cargo test --lib settings::tests`
Expected: all settings tests pass, including the updated `round_trip_preserves_fields` and the new backward-compat test.

- [ ] **Step 7: Commit**

```bash
git add src/settings.rs
git commit -m "feat: add monitor_basic_enabled/monitor_basic_interval_secs to TerminalSettings"
```

---

### Task 3: Settings UI

**Files:**
- Modify: `src/panels/settings_window.rs` (`SettingsWindow` struct ~line 47-55, `::new` ~line 58-77, `sync_inputs_to_draft` ~line 82-96, `apply` ~line 102-130, `render_terminal_tab` ~line 253-284)

**Interfaces:**
- Consumes: `TerminalSettings.monitor_basic_enabled: bool`, `TerminalSettings.monitor_basic_interval_secs: u32` (Task 2).
- Produces: nothing consumed by later tasks — leaf feature (the settings UI; `MonitorPanel` in Task 5 reads `settings::load()` directly, not through this window).

- [ ] **Step 1: Add draft fields to `SettingsWindow`**

In `src/panels/settings_window.rs`'s struct (~line 47-55), add:

```rust
pub struct SettingsWindow {
    workspace: WeakEntity<Workspace>,
    committed: AppSettings,
    draft: AppSettings,
    active_tab: SettingsTab,
    font_family_input: Entity<InputState>,
    font_size_input: Entity<InputState>,
    monitor_interval_input: Entity<InputState>,
    error: Option<SharedString>,
}
```

- [ ] **Step 2: Initialize the new field in `::new`**

In `src/panels/settings_window.rs`'s `SettingsWindow::new` (~line 58-77):

```rust
    pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let committed = settings::load();
        let font_family_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空 = 内置默认字体")
                .default_value(committed.terminal.font_family.clone())
        });
        let font_size_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.terminal.font_size.to_string())
        });
        let monitor_interval_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.terminal.monitor_basic_interval_secs.to_string())
        });
        Self {
            workspace,
            draft: committed.clone(),
            committed,
            active_tab: SettingsTab::Appearance,
            font_family_input,
            font_size_input,
            monitor_interval_input,
            error: None,
        }
    }
```

- [ ] **Step 3: Add a poll-interval parser and read it in `sync_inputs_to_draft`**

In `src/panels/settings_window.rs`, add near `parse_font_size` (~line 7-17):

```rust
/// Parse the Terminal tab's monitor-poll-interval field. Rejects
/// non-positive and unreasonably small/large values (a 0-second interval
/// would spin the poll loop; a typo like "99999" would poll once every 27
/// hours, effectively never).
fn parse_monitor_interval(text: &str) -> Option<u32> {
    let value: u32 = text.trim().parse().ok()?;
    if (1..=3600).contains(&value) {
        Some(value)
    } else {
        None
    }
}
```

Change `sync_inputs_to_draft` (~line 82-96):

```rust
    fn sync_inputs_to_draft(&mut self, cx: &App) -> bool {
        self.draft.terminal.font_family = self.font_family_input.read(cx).value().to_string();
        let size_text = self.font_size_input.read(cx).value();
        let Some(size) = parse_font_size(&size_text) else {
            self.error = Some("字号必须是 6-96 之间的数字".into());
            return false;
        };
        self.draft.terminal.font_size = size;

        let interval_text = self.monitor_interval_input.read(cx).value();
        let Some(interval) = parse_monitor_interval(&interval_text) else {
            self.error = Some("轮询间隔必须是 1-3600 之间的整数(秒)".into());
            return false;
        };
        self.draft.terminal.monitor_basic_interval_secs = interval;

        self.error = None;
        true
    }
```

- [ ] **Step 4: Add a toggle method and wire it into the draft**

In `src/panels/settings_window.rs`, add this method near `set_theme_mode`:

```rust
    fn toggle_monitor_enabled(&mut self, cx: &mut Context<Self>) {
        self.draft.terminal.monitor_basic_enabled = !self.draft.terminal.monitor_basic_enabled;
        cx.notify();
    }
```

- [ ] **Step 5: Add the enable-toggle pill and interval input to `render_terminal_tab`**

In `src/panels/settings_window.rs`'s `render_terminal_tab` (~line 253-284), add a new section after the font-size field, mirroring `theme_pill`'s exact pattern for the toggle:

```rust
    fn render_terminal_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("字体族"),
                    )
                    .child(Input::new(&self.font_family_input)),
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
                            .child("字号 (px)"),
                    )
                    .child(Input::new(&self.font_size_input)),
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
                            .child("资源监控 (基础)"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(self.monitor_enabled_pill(cx)),
                    ),
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
                            .child("轮询间隔 (秒)"),
                    )
                    .child(Input::new(&self.monitor_interval_input)),
            )
    }

    fn monitor_enabled_pill(&self, cx: &Context<Self>) -> impl IntoElement {
        let active = self.draft.terminal.monitor_basic_enabled;
        div()
            .id("settings-monitor-enabled")
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(if active { cx.theme().primary } else { cx.theme().accent })
            .text_color(if active {
                cx.theme().primary_foreground
            } else {
                cx.theme().foreground
            })
            .hover(|s| s.bg(cx.theme().accent))
            .child(if active { "已启用" } else { "已禁用" })
            .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.toggle_monitor_enabled(cx);
            }))
    }
```

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: clean build, no warnings.

- [ ] **Step 7: Manual smoke test**

Run: `cargo run`, open Settings → Terminal, confirm the new "资源监控 (基础)" toggle pill and "轮询间隔 (秒)" input appear below the existing font fields, toggling the pill flips its label/color, entering an out-of-range interval (e.g. "0" or "99999") and clicking Apply shows the validation error message instead of silently accepting it, a valid value (e.g. "10") applies and persists (re-open Settings, confirm it shows "10").

- [ ] **Step 8: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add resource-monitoring enable toggle + poll interval to Settings > Terminal"
```

---

### Task 4: Parsing pure functions

**Files:**
- Create: `src/panels/monitor.rs` (data-parsing section only — no `MonitorPanel`/UI yet, added in Task 5)
- Modify: `src/panels/mod.rs` (or wherever panel modules are declared — add `pub mod monitor;`)

**Interfaces:**
- Produces: `RawSample`, `SystemStats`, `NicRate`, `DiskUsage` structs; `parse_combined_output(raw: &str) -> HashMap<String, String>`; `parse_uname(section: &str) -> (String, String)` (hostname, os_line); `parse_uptime_secs(section: &str) -> u64`; `parse_loadavg(section: &str) -> (f32, f32, f32)`; `parse_meminfo(section: &str) -> (u64, u64, u64)` (total, available, cached, all in bytes); `parse_stat_cpu(section: &str) -> (u64, u64, usize)` (total_jiffies, idle_jiffies, core_count); `parse_netdev(section: &str) -> Vec<(String, u64, u64)>` (name, rx_bytes, tx_bytes); `parse_df(section: &str) -> Vec<DiskUsage>`; `compute_stats(sections: &HashMap<String, String>, prev: Option<&RawSample>) -> (SystemStats, RawSample)`. Consumed by Task 5 (`MonitorPanel`'s poll loop calls `parse_combined_output` + `compute_stats` on each reply).
- Consumes: nothing from other tasks.

- [ ] **Step 1: Create the file with imports and structs**

Create `src/panels/monitor.rs`:

```rust
//! `MonitorPanel`: per-host 资源监控 (basic system stats) panel. Polls the
//! remote host over `SshSession::exec_command` (a fresh, non-interactive
//! channel — CLAUDE.md §2: one Session, one connection) and renders
//! CPU/memory/network/disk usage. Linux remote hosts only (reads `/proc/*`,
//! uses `df`) — see
//! `docs/superpowers/specs/2026-07-08-resource-monitoring-round-a-design.md`.
//!
//! This file's parsing/computation functions are pure (no GPUI, no
//! network) — the poll loop and rendering live in a later section of this
//! same file, added once these are proven correct.

use std::collections::HashMap;
use std::time::Instant;

/// One poll's raw, cumulative counters — needed to compute CPU%/network
/// rates as deltas against the *next* poll (both `/proc/stat`'s jiffie
/// counters and `/proc/net/dev`'s byte counters are cumulative since boot,
/// not instantaneous).
#[derive(Clone, Debug)]
pub struct RawSample {
    pub cpu_total: u64,
    pub cpu_idle: u64,
    /// (interface name, rx_bytes, tx_bytes).
    pub net: Vec<(String, u64, u64)>,
    pub at: Instant,
}

/// Parsed + computed stats, ready for rendering.
#[derive(Clone, Debug)]
pub struct SystemStats {
    pub hostname: String,
    pub os: String,
    pub uptime_secs: u64,
    /// `None` on the first poll (no prior sample to diff against).
    pub cpu_percent: Option<f32>,
    pub core_count: usize,
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_available: u64,
    pub mem_cached: u64,
    pub net: Vec<NicRate>,
    pub disks: Vec<DiskUsage>,
}

#[derive(Clone, Debug)]
pub struct NicRate {
    pub name: String,
    /// Bytes/sec. `None` on the first poll.
    pub rx_rate: Option<f64>,
    pub tx_rate: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct DiskUsage {
    pub mount: String,
    pub used: u64,
    pub total: u64,
    pub available: u64,
}
```

- [ ] **Step 2: Write `parse_combined_output`**

Add to `src/panels/monitor.rs`:

```rust
/// Split the combined poll script's reply on `===SECTION===` markers into
/// named blocks. The script (see `MonitorPanel::poll_script` in the next
/// task) looks like:
/// ```text
/// ===UNAME===
/// Linux yoga-arch 7.1.2-arch3-1 x86_64
/// ===UPTIME===
/// 245610.78 1989338.46
/// ...
/// ```
pub fn parse_combined_output(raw: &str) -> HashMap<String, String> {
    let mut sections = HashMap::new();
    let mut current_key: Option<String> = None;
    let mut current_body = String::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("===").and_then(|s| s.strip_suffix("===")) {
            if let Some(key) = current_key.take() {
                sections.insert(key, std::mem::take(&mut current_body));
            }
            current_key = Some(rest.to_string());
        } else if current_key.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    if let Some(key) = current_key {
        sections.insert(key, current_body);
    }
    sections
}
```

- [ ] **Step 3: Write the failing tests for `parse_combined_output`**

Add a test module at the end of `src/panels/monitor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_combined_output_splits_on_markers() {
        let raw = "===UNAME===\nLinux host 1.0 x86_64\n===UPTIME===\n123.4 567.8\n";
        let sections = parse_combined_output(raw);
        assert_eq!(sections.get("UNAME").map(|s| s.trim()), Some("Linux host 1.0 x86_64"));
        assert_eq!(sections.get("UPTIME").map(|s| s.trim()), Some("123.4 567.8"));
    }

    #[test]
    fn parse_combined_output_empty_input_yields_no_sections() {
        let sections = parse_combined_output("");
        assert!(sections.is_empty());
    }
}
```

- [ ] **Step 4: Run to verify it passes (this parser is simple enough to write correct the first time, but confirm)**

Run: `cargo test --lib panels::monitor::tests -- --nocapture`
Expected: both tests pass.

- [ ] **Step 5: Write `parse_uname`, `parse_uptime_secs`, `parse_loadavg`**

Add to `src/panels/monitor.rs`, above the test module:

```rust
/// `uname -srmn` output: "Linux yoga-arch 7.1.2-arch3-1 x86_64" — fixed
/// field order (kernel-name, nodename, release, machine) regardless of
/// flag order given. Returns (hostname, "kernel-name release machine").
pub fn parse_uname(section: &str) -> (String, String) {
    let line = section.trim();
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 4 {
        return (String::new(), line.to_string());
    }
    let hostname = fields[1].to_string();
    let os = format!("{} {} {}", fields[0], fields[2], fields[3]);
    (hostname, os)
}

/// `/proc/uptime`: "245610.78 1989338.46" (uptime_seconds idle_seconds).
pub fn parse_uptime_secs(section: &str) -> u64 {
    section
        .trim()
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0)
}

/// `/proc/loadavg`: "1.04 0.92 0.56 2/2388 4129263" — first three fields.
pub fn parse_loadavg(section: &str) -> (f32, f32, f32) {
    let fields: Vec<&str> = section.trim().split_whitespace().collect();
    let get = |i: usize| fields.get(i).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
    (get(0), get(1), get(2))
}
```

- [ ] **Step 6: Write tests for Step 5's functions**

Add to `src/panels/monitor.rs`'s test module:

```rust
    #[test]
    fn parse_uname_extracts_hostname_and_os() {
        let (hostname, os) = parse_uname("Linux yoga-arch 7.1.2-arch3-1 x86_64\n");
        assert_eq!(hostname, "yoga-arch");
        assert_eq!(os, "Linux 7.1.2-arch3-1 x86_64");
    }

    #[test]
    fn parse_uptime_secs_truncates_fractional() {
        assert_eq!(parse_uptime_secs("245610.78 1989338.46\n"), 245610);
    }

    #[test]
    fn parse_loadavg_extracts_three_values() {
        let (l1, l5, l15) = parse_loadavg("1.04 0.92 0.56 2/2388 4129263\n");
        assert_eq!(l1, 1.04);
        assert_eq!(l5, 0.92);
        assert_eq!(l15, 0.56);
    }
```

- [ ] **Step 7: Run to verify**

Run: `cargo test --lib panels::monitor::tests`
Expected: all tests pass.

- [ ] **Step 8: Write `parse_meminfo`**

Add to `src/panels/monitor.rs`:

```rust
/// `/proc/meminfo`: lines like "MemTotal:       32451764 kB". Returns
/// (total_bytes, available_bytes, cached_bytes). Values in the file are
/// KiB; multiplied by 1024 for a byte count.
pub fn parse_meminfo(section: &str) -> (u64, u64, u64) {
    let mut total = 0u64;
    let mut available = 0u64;
    let mut cached = 0u64;
    for line in section.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kb: u64 = rest
            .trim()
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        match key.trim() {
            "MemTotal" => total = kb * 1024,
            "MemAvailable" => available = kb * 1024,
            "Cached" => cached = kb * 1024,
            _ => {}
        }
    }
    (total, available, cached)
}
```

- [ ] **Step 9: Write `parse_stat_cpu`**

Add to `src/panels/monitor.rs`:

```rust
/// `/proc/stat`: first line "cpu  15255309 1458805 3142683 198933847
/// 172252364 1038658 208180 0 0 0" (user nice system idle iowait irq
/// softirq steal guest guest_nice). total = sum of all fields; idle =
/// idle + iowait (fields 3 and 4, 0-indexed after "cpu"). Core count =
/// number of "cpuN" lines (cpu0, cpu1, ...).
pub fn parse_stat_cpu(section: &str) -> (u64, u64, usize) {
    let mut total = 0u64;
    let mut idle = 0u64;
    let mut core_count = 0usize;
    for line in section.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("cpu ") {
            let fields: Vec<u64> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
            total = fields.iter().sum();
            idle = fields.get(3).copied().unwrap_or(0) + fields.get(4).copied().unwrap_or(0);
        } else if line.starts_with("cpu") && line.chars().nth(3).is_some_and(|c| c.is_ascii_digit()) {
            core_count += 1;
        }
    }
    (total, idle, core_count)
}
```

- [ ] **Step 10: Write `parse_netdev`**

Add to `src/panels/monitor.rs`:

```rust
/// `/proc/net/dev`: two header lines, then per-interface lines like
/// "    lo: 8170727377 7531429    0 ... 8170727377 7531429 ...". After
/// splitting on ':', the value side has 16 whitespace-separated fields:
/// rx_bytes is field 0, tx_bytes is field 8 (8 receive fields precede it:
/// bytes/packets/errs/drop/fifo/frame/compressed/multicast).
pub fn parse_netdev(section: &str) -> Vec<(String, u64, u64)> {
    let mut result = Vec::new();
    for line in section.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<u64> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        let rx = fields.first().copied().unwrap_or(0);
        let tx = fields.get(8).copied().unwrap_or(0);
        result.push((name.trim().to_string(), rx, tx));
    }
    result
}
```

- [ ] **Step 11: Write `parse_df`**

Add to `src/panels/monitor.rs`:

```rust
/// `df -B1 ...` output: header line "Filesystem  1B-blocks  Used
/// Available Use% Mounted on", then rows like "/dev/nvme0n1p3
/// 997467398144 517232562176 429490835456  55% /". Fields: filesystem,
/// total, used, available, use%, mount (mount may contain spaces in rare
/// cases — joined from field 5 onward).
pub fn parse_df(section: &str) -> Vec<DiskUsage> {
    let mut result = Vec::new();
    for line in section.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with("Filesystem") {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let total: u64 = fields[1].parse().unwrap_or(0);
        let used: u64 = fields[2].parse().unwrap_or(0);
        let available: u64 = fields[3].parse().unwrap_or(0);
        let mount = fields[5..].join(" ");
        result.push(DiskUsage { mount, used, total, available });
    }
    result
}
```

- [ ] **Step 12: Write tests for Steps 8-11**

Add to `src/panels/monitor.rs`'s test module:

```rust
    #[test]
    fn parse_meminfo_extracts_total_available_cached_in_bytes() {
        let section = "MemTotal:       32451764 kB\nMemFree:         1220564 kB\nMemAvailable:   15806552 kB\nCached:         14819784 kB\n";
        let (total, available, cached) = parse_meminfo(section);
        assert_eq!(total, 32451764 * 1024);
        assert_eq!(available, 15806552 * 1024);
        assert_eq!(cached, 14819784 * 1024);
    }

    #[test]
    fn parse_stat_cpu_sums_fields_and_counts_cores() {
        let section = "cpu  15255309 1458805 3142683 198933847 172252364 1038658 208180 0 0 0\ncpu0 1163087 29626 157337 13427739 9648658 60680 22212 0 0 0\ncpu1 1416461 544227 278372 18917054 3266066 74132 10993 0 0 0\n";
        let (total, idle, cores) = parse_stat_cpu(section);
        assert_eq!(total, 15255309 + 1458805 + 3142683 + 198933847 + 172252364 + 1038658 + 208180);
        assert_eq!(idle, 198933847 + 172252364);
        assert_eq!(cores, 2);
    }

    #[test]
    fn parse_netdev_extracts_rx_tx_per_interface_skipping_headers() {
        let section = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n  eth0: 300 3 0 0 0 0 0 0 400 4 0 0 0 0 0 0\n";
        let nics = parse_netdev(section);
        assert_eq!(nics, vec![
            ("lo".to_string(), 100, 200),
            ("eth0".to_string(), 300, 400),
        ]);
    }

    #[test]
    fn parse_df_extracts_mount_rows_skipping_header() {
        let section = "Filesystem        1B-blocks         Used    Available Use% Mounted on\n/dev/nvme0n1p3 997467398144 517232562176 429490835456  55% /\n/dev/nvme0n1p1   1071628288    480604160    591024128  45% /boot\n";
        let disks = parse_df(section);
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].mount, "/");
        assert_eq!(disks[0].total, 997467398144);
        assert_eq!(disks[0].used, 517232562176);
        assert_eq!(disks[0].available, 429490835456);
        assert_eq!(disks[1].mount, "/boot");
    }
```

All four tests above assert on individual fields (`disks[0].mount`, `nics` as a `Vec<(String, u64, u64)>` tuple, etc.), never a whole `NicRate`/`DiskUsage`/`SystemStats` value — no `PartialEq` derive needed on any struct for these tests to compile.

- [ ] **Step 13: Run to verify**

Run: `cargo test --lib panels::monitor::tests`
Expected: all tests pass.

- [ ] **Step 14: Write `compute_stats` (the delta/threshold logic tying everything together)**

Add to `src/panels/monitor.rs`:

```rust
/// Combine one poll's parsed sections with the previous poll's `RawSample`
/// (if any) into `SystemStats` (CPU%/network rates computed as deltas)
/// plus this poll's own `RawSample` (for the *next* poll to diff against).
pub fn compute_stats(
    sections: &HashMap<String, String>,
    prev: Option<&RawSample>,
) -> (SystemStats, RawSample) {
    let (hostname, os) = sections.get("UNAME").map(|s| parse_uname(s)).unwrap_or_default();
    let uptime_secs = sections.get("UPTIME").map(|s| parse_uptime_secs(s)).unwrap_or(0);
    let (load1, load5, load15) = sections.get("LOADAVG").map(|s| parse_loadavg(s)).unwrap_or((0.0, 0.0, 0.0));
    let (mem_total, mem_available, mem_cached) =
        sections.get("MEMINFO").map(|s| parse_meminfo(s)).unwrap_or((0, 0, 0));
    let (cpu_total, cpu_idle, core_count) =
        sections.get("STAT").map(|s| parse_stat_cpu(s)).unwrap_or((0, 0, 0));
    let net_raw = sections.get("NETDEV").map(|s| parse_netdev(s)).unwrap_or_default();
    let disks = sections.get("DF").map(|s| parse_df(s)).unwrap_or_default();

    let now = Instant::now();
    let cur_sample = RawSample {
        cpu_total,
        cpu_idle,
        net: net_raw.clone(),
        at: now,
    };

    let cpu_percent = prev.and_then(|p| {
        let total_delta = cpu_total.checked_sub(p.cpu_total)?;
        let idle_delta = cpu_idle.checked_sub(p.cpu_idle)?;
        if total_delta == 0 {
            return None;
        }
        Some((1.0 - (idle_delta as f32 / total_delta as f32)) * 100.0)
    });

    let net: Vec<NicRate> = net_raw
        .iter()
        .map(|(name, rx, tx)| {
            let rates = prev.and_then(|p| {
                let (_, prev_rx, prev_tx) = p.net.iter().find(|(n, _, _)| n == name)?;
                let elapsed = now.duration_since(p.at).as_secs_f64();
                if elapsed <= 0.0 {
                    return None;
                }
                let rx_rate = (rx.checked_sub(*prev_rx)? as f64) / elapsed;
                let tx_rate = (tx.checked_sub(*prev_tx)? as f64) / elapsed;
                Some((rx_rate, tx_rate))
            });
            NicRate {
                name: name.clone(),
                rx_rate: rates.map(|(rx, _)| rx),
                tx_rate: rates.map(|(_, tx)| tx),
            }
        })
        .collect();

    let stats = SystemStats {
        hostname,
        os,
        uptime_secs,
        cpu_percent,
        core_count,
        load1,
        load5,
        load15,
        mem_total,
        mem_used: mem_total.saturating_sub(mem_available),
        mem_available,
        mem_cached,
        net,
        disks,
    };

    (stats, cur_sample)
}
```

- [ ] **Step 15: Write tests for `compute_stats`**

Add to `src/panels/monitor.rs`'s test module:

```rust
    fn sample_sections() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("UNAME".to_string(), "Linux host 1.0 x86_64\n".to_string());
        m.insert("UPTIME".to_string(), "100.0 50.0\n".to_string());
        m.insert("LOADAVG".to_string(), "1.0 2.0 3.0 1/1 999\n".to_string());
        m.insert("MEMINFO".to_string(), "MemTotal: 1000 kB\nMemAvailable: 400 kB\nCached: 200 kB\n".to_string());
        m.insert("STAT".to_string(), "cpu  100 0 0 800 0 0 0 0 0 0\ncpu0 100 0 0 800 0 0 0 0 0 0\n".to_string());
        m.insert("NETDEV".to_string(), "h1\nh2\n  eth0: 1000 1 0 0 0 0 0 0 2000 1 0 0 0 0 0 0\n".to_string());
        m.insert("DF".to_string(), "Filesystem 1B-blocks Used Available Use% Mounted on\n/dev/x 1000 500 500 50% /\n".to_string());
        m
    }

    #[test]
    fn compute_stats_first_poll_has_no_cpu_percent_or_net_rate() {
        let sections = sample_sections();
        let (stats, sample) = compute_stats(&sections, None);
        assert_eq!(stats.hostname, "host");
        assert_eq!(stats.cpu_percent, None);
        assert_eq!(stats.net[0].rx_rate, None);
        assert_eq!(stats.mem_used, (1000 - 400) * 1024);
        assert_eq!(sample.cpu_total, 900);
        assert_eq!(sample.cpu_idle, 800);
    }

    #[test]
    fn compute_stats_second_poll_computes_cpu_percent_and_net_rate() {
        let sections = sample_sections();
        let (_, prev_sample) = compute_stats(&sections, None);
        // Simulate a second poll 1 second later with more CPU busy time and
        // more network bytes transferred.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut sections2 = sample_sections();
        sections2.insert(
            "STAT".to_string(),
            "cpu  200 0 0 1600 0 0 0 0 0 0\ncpu0 200 0 0 1600 0 0 0 0 0 0\n".to_string(),
        );
        sections2.insert(
            "NETDEV".to_string(),
            "h1\nh2\n  eth0: 3000 1 0 0 0 0 0 0 6000 1 0 0 0 0 0 0\n".to_string(),
        );
        let (stats2, _) = compute_stats(&sections2, Some(&prev_sample));
        // total_delta = 900, idle_delta = 800 -> cpu_percent = (1 - 800/900) * 100 ≈ 11.1%
        let cpu_pct = stats2.cpu_percent.expect("second poll must have a cpu_percent");
        assert!((cpu_pct - 11.11).abs() < 0.5);
        assert!(stats2.net[0].rx_rate.is_some());
        assert!(stats2.net[0].tx_rate.is_some());
    }
```

- [ ] **Step 16: Run to verify**

Run: `cargo test --lib panels::monitor::tests`
Expected: all tests pass.

- [ ] **Step 17: Write the threshold-coloring pure function**

Add to `src/panels/monitor.rs`:

```rust
/// Threshold band for a usage percentage (0-100). Red at ≥90%, amber at
/// ≥70%, default otherwise — nyaterm's own convention, not a reuse of
/// anything already in caracal (no percentage-driven threshold coloring
/// exists elsewhere in this codebase).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageBand {
    Danger,
    Warning,
    Default,
}

pub fn usage_band(percent: f32) -> UsageBand {
    if percent >= 90.0 {
        UsageBand::Danger
    } else if percent >= 70.0 {
        UsageBand::Warning
    } else {
        UsageBand::Default
    }
}
```

- [ ] **Step 18: Write tests for `usage_band`**

Add to `src/panels/monitor.rs`'s test module:

```rust
    #[test]
    fn usage_band_boundaries() {
        assert_eq!(usage_band(0.0), UsageBand::Default);
        assert_eq!(usage_band(69.9), UsageBand::Default);
        assert_eq!(usage_band(70.0), UsageBand::Warning);
        assert_eq!(usage_band(89.9), UsageBand::Warning);
        assert_eq!(usage_band(90.0), UsageBand::Danger);
        assert_eq!(usage_band(100.0), UsageBand::Danger);
    }
}
```

(This closing `}` ends the `mod tests` block — the file at this point has no `MonitorPanel`/UI code yet; Task 5 adds it above the `#[cfg(test)] mod tests` block.)

- [ ] **Step 19: Register the module**

Find where panel modules are declared (likely `src/panels/mod.rs` — check with `grep -n "pub mod" src/panels/mod.rs`) and add:

```rust
pub mod monitor;
```

in alphabetical position among the existing `pub mod` declarations.

- [ ] **Step 20: Run the full test suite**

Run: `cargo test --lib`
Expected: all tests pass, including every new `panels::monitor::tests::*` test. `cargo build` should also show a `dead_code` warning-free build even though `MonitorPanel` doesn't exist yet, since every function/struct added this task is `pub` (public items don't trigger the `dead_code` lint the way private unused functions do) — confirm with `cargo build` that there are 0 warnings.

- [ ] **Step 21: Commit**

```bash
git add src/panels/monitor.rs src/panels/mod.rs
git commit -m "feat: add resource-monitoring parsing/computation pure functions"
```

---

### Task 5: `MonitorPanel` (poll loop + rendering)

**Files:**
- Modify: `src/panels/monitor.rs` (add above the `#[cfg(test)] mod tests` block from Task 4)

**Interfaces:**
- Consumes: `SshSession::exec_command` (Task 1), `TerminalSettings.monitor_basic_enabled`/`.monitor_basic_interval_secs` (Task 2), `parse_combined_output`/`compute_stats`/`usage_band`/`RawSample`/`SystemStats` (Task 4).
- Produces: `MonitorPanel::new(session: Arc<SshSession>, label: impl Into<SharedString>, cx: &mut Context<Self>) -> Self`, `MonitorPlaceholder::new(cx: &mut Context<Self>) -> Self` — both consumed by Task 6 (`Workspace` wiring).

- [ ] **Step 1: Add imports and the poll script constant**

In `src/panels/monitor.rs`, add at the top of the file, right after the existing `use std::collections::HashMap; use std::time::Instant;` lines:

```rust
use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{ActiveTheme, Sizable};
use gpui_component::button::{Button, ButtonVariants};

use crate::panels::icons::{AppIcon, icon};
use crate::terminal::ssh::SshSession;

/// One combined shell script per poll — one round-trip instead of 7+,
/// each section preceded by an `echo ===SECTION===` marker so the reply
/// parses with `parse_combined_output`.
const POLL_SCRIPT: &str = "echo ===UNAME===; uname -srmn\n\
echo ===UPTIME===; cat /proc/uptime\n\
echo ===LOADAVG===; cat /proc/loadavg\n\
echo ===MEMINFO===; cat /proc/meminfo\n\
echo ===STAT===; cat /proc/stat\n\
echo ===NETDEV===; cat /proc/net/dev\n\
echo ===DF===; df -B1 -x tmpfs -x devtmpfs -x overlay\n";
```

- [ ] **Step 2: Add the `MonitorPanel` struct and constructor**

In `src/panels/monitor.rs`, add right after the imports (before the existing struct definitions from Task 4, or after them — position doesn't matter as long as it's above `#[cfg(test)] mod tests`):

```rust
pub struct MonitorPanel {
    focus_handle: FocusHandle,
    session: Arc<SshSession>,
    label: SharedString,
    enabled: bool,
    interval_secs: u32,
    prev_sample: Option<RawSample>,
    stats: Option<SystemStats>,
    /// Set on poll failure; cleared on the next successful poll. Used
    /// together with `consecutive_failures` to decide when to clear
    /// `stats` (3-failure cutoff) vs. keep showing the last-known reading
    /// through a single transient failure.
    last_error: Option<String>,
    consecutive_failures: u32,
    /// True while a poll (scheduled or manual-refresh) is in flight — the
    /// header refresh button spins while this is true.
    polling: bool,
}

impl MonitorPanel {
    pub fn new(
        session: Arc<SshSession>,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = crate::settings::load();
        let this = Self {
            focus_handle: cx.focus_handle(),
            session,
            label: label.into(),
            enabled: settings.terminal.monitor_basic_enabled,
            interval_secs: settings.terminal.monitor_basic_interval_secs,
            prev_sample: None,
            stats: None,
            last_error: None,
            consecutive_failures: 0,
            polling: false,
        };
        if this.enabled {
            this.start_poll_loop(cx);
        }
        this
    }
```

`window` is otherwise unused in `new` after this point — `cx.spawn` (used by `start_poll_loop` in the next step) doesn't need `Window`, only `Context<Self>`, since polling never touches the UI focus/input system, just entity state.

- [ ] **Step 3: Add the poll loop**

In `src/panels/monitor.rs`, add to `impl MonitorPanel` (right after `new`):

```rust
    fn start_poll_loop(&self, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let interval_secs = self.interval_secs;
        cx.spawn(async move |this, cx| {
            loop {
                let rx = session.exec_command(POLL_SCRIPT.to_string());
                let result = rx.recv_async().await;
                let still_alive = this
                    .update(cx, |panel, cx| {
                        panel.apply_poll_result(result);
                        cx.notify();
                    })
                    .is_ok();
                if !still_alive {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(interval_secs as u64))
                    .await;
            }
        })
        .detach();
    }

    /// Apply one poll's outcome: on success, parse + compute stats and
    /// reset the failure counter; on failure, increment the counter and
    /// clear `stats` once it reaches 3 (the cutoff), matching nyaterm's
    /// own rule.
    fn apply_poll_result(&mut self, result: Result<Result<String, anyhow::Error>, flume::RecvError>) {
        self.polling = false;
        match result {
            Ok(Ok(raw)) => {
                let sections = parse_combined_output(&raw);
                let (stats, sample) = compute_stats(&sections, self.prev_sample.as_ref());
                self.prev_sample = Some(sample);
                self.stats = Some(stats);
                self.last_error = None;
                self.consecutive_failures = 0;
            }
            Ok(Err(e)) => self.record_failure(format!("{e}")),
            Err(_) => self.record_failure("session closed".to_string()),
        }
    }

    fn record_failure(&mut self, message: String) {
        self.consecutive_failures += 1;
        self.last_error = Some(message);
        if self.consecutive_failures >= 3 {
            self.stats = None;
            self.prev_sample = None;
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.polling = true;
        cx.notify();
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
            let rx = session.exec_command(POLL_SCRIPT.to_string());
            let result = rx.recv_async().await;
            let _ = this.update(cx, |panel, cx| {
                panel.apply_poll_result(result);
                cx.notify();
            });
        })
        .detach();
    }
```

- [ ] **Step 4: Add the render methods**

In `src/panels/monitor.rs`, add to `impl MonitorPanel`:

```rust
    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .child(SharedString::from(format!("资源监控: {}", self.label))),
            )
            .child(
                Button::new("monitor-refresh")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Refresh))
                    .tooltip("刷新")
                    .loading(self.polling)
                    .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
            )
    }

    fn render_body(&self, cx: &Context<Self>) -> impl IntoElement {
        if !self.enabled {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("资源监控已在设置中禁用")
                .into_any_element();
        }
        if self.consecutive_failures >= 3 {
            let msg = self.last_error.clone().unwrap_or_default();
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(format!("连续 3 次轮询失败: {msg}"))),
                )
                .into_any_element();
        }
        let Some(stats) = &self.stats else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("加载中…")
                .into_any_element();
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_2()
            .child(self.render_system_section(stats, cx))
            .child(self.render_cpu_section(stats, cx))
            .child(self.render_memory_section(stats, cx))
            .child(self.render_network_section(stats, cx))
            .child(self.render_disk_section(stats, cx))
            .into_any_element()
    }

    fn render_system_section(&self, stats: &SystemStats, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .text_xs()
            .child(SharedString::from(format!("主机: {}", stats.hostname)))
            .child(SharedString::from(format!("系统: {}", stats.os)))
            .child(SharedString::from(format!("运行时间: {}", format_uptime(stats.uptime_secs))))
            .text_color(cx.theme().muted_foreground)
    }

    fn render_cpu_section(&self, stats: &SystemStats, cx: &Context<Self>) -> impl IntoElement {
        let (bar, label) = match stats.cpu_percent {
            Some(pct) => (usage_bar(pct, cx), format!("{pct:.1}%")),
            None => (usage_bar(0.0, cx), "预热中…".to_string()),
        };
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .text_xs()
                    .child("CPU")
                    .child(SharedString::from(label)),
            )
            .child(bar)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!(
                        "{} 核心 · 负载 {:.2} {:.2} {:.2}",
                        stats.core_count, stats.load1, stats.load5, stats.load15
                    ))),
            )
    }

    fn render_memory_section(&self, stats: &SystemStats, cx: &Context<Self>) -> impl IntoElement {
        let pct = if stats.mem_total == 0 {
            0.0
        } else {
            (stats.mem_used as f32 / stats.mem_total as f32) * 100.0
        };
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .text_xs()
                    .child("内存")
                    .child(SharedString::from(format!(
                        "{} / {}",
                        human_bytes(stats.mem_used),
                        human_bytes(stats.mem_total)
                    ))),
            )
            .child(usage_bar(pct, cx))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!(
                        "可用 {} · 缓存 {}",
                        human_bytes(stats.mem_available),
                        human_bytes(stats.mem_cached)
                    ))),
            )
    }

    fn render_network_section(&self, stats: &SystemStats, cx: &Context<Self>) -> impl IntoElement {
        let mut section = div().flex().flex_col().gap_0p5().text_xs();
        section = section.child(div().text_color(cx.theme().muted_foreground).child("网络"));
        for nic in &stats.net {
            let rates = match (nic.rx_rate, nic.tx_rate) {
                (Some(rx), Some(tx)) => {
                    format!("↓{}/s ↑{}/s", human_bytes(rx as u64), human_bytes(tx as u64))
                }
                _ => "预热中…".to_string(),
            };
            section = section.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(SharedString::from(nic.name.clone()))
                    .child(SharedString::from(rates)),
            );
        }
        section
    }

    fn render_disk_section(&self, stats: &SystemStats, cx: &Context<Self>) -> impl IntoElement {
        let mut section = div().flex().flex_col().gap_1().text_xs();
        section = section.child(div().text_color(cx.theme().muted_foreground).child("磁盘"));
        for disk in &stats.disks {
            let pct = if disk.total == 0 {
                0.0
            } else {
                (disk.used as f32 / disk.total as f32) * 100.0
            };
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .child(SharedString::from(disk.mount.clone()))
                            .child(SharedString::from(format!("{pct:.0}%"))),
                    )
                    .child(usage_bar(pct, cx))
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(format!(
                                "已用 {} · 可用 {}",
                                human_bytes(disk.used),
                                human_bytes(disk.available)
                            ))),
                    ),
            );
        }
        section
    }
}

/// A thin colored bar whose fill width is `percent` (0-100), colored by
/// `usage_band`.
fn usage_bar(percent: f32, cx: &App) -> impl IntoElement {
    let color = match usage_band(percent) {
        UsageBand::Danger => cx.theme().danger,
        UsageBand::Warning => cx.theme().warning,
        UsageBand::Default => cx.theme().primary,
    };
    let clamped = percent.clamp(0.0, 100.0);
    div()
        .w_full()
        .h(px(6.0))
        .rounded_sm()
        .bg(cx.theme().accent)
        .child(
            div()
                .h_full()
                .rounded_sm()
                .bg(color)
                .w(gpui::relative(clamped / 100.0)),
        )
}

/// `123456789` -> `"117.7M"` (binary/1024-based units, matching
/// `sftp.rs`'s existing `human_size` convention but kept local to this
/// file rather than importing across panel modules).
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[0])
    } else {
        format!("{size:.1}{}", UNITS[unit])
    }
}

/// `3665` -> `"1h 1m"` (drops seconds; drops hours if 0; always shows at
/// least minutes).
fn format_uptime(total_secs: u64) -> String {
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}
```

- [ ] **Step 5: Add `Focusable`/`EventEmitter`/`Panel`/`Render` impls**

In `src/panels/monitor.rs`, add after the `impl MonitorPanel` block:

```rust
impl Focusable for MonitorPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<PanelEvent> for MonitorPanel {}

impl Panel for MonitorPanel {
    fn panel_name(&self) -> &'static str {
        "MonitorPanel"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(format!("资源监控: {}", self.label))
    }
}

impl Render for MonitorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_header(cx))
            .child(self.render_body(cx))
    }
}

// --- placeholder for non-SSH focused terminals -----------------------------

pub struct MonitorPlaceholder {
    focus_handle: FocusHandle,
}

impl MonitorPlaceholder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for MonitorPlaceholder {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<PanelEvent> for MonitorPlaceholder {}

impl Panel for MonitorPlaceholder {
    fn panel_name(&self) -> &'static str {
        "MonitorPlaceholder"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("资源监控")
    }
}

impl Render for MonitorPlaceholder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("未连接 SSH 主机")
    }
}
```

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: clean build, no warnings. `Button::loading(bool)` is confirmed present on the pinned `gpui-component` (`crates/ui/src/button/button.rs:320`), so `.loading(self.polling)` in Step 4's `render_header` should compile without any fallback needed.

- [ ] **Step 7: Run tests**

Run: `cargo test --lib`
Expected: all tests pass (Task 4's parsing tests plus any pre-existing tests), no regressions.

- [ ] **Step 8: Commit**

```bash
git add src/panels/monitor.rs
git commit -m "feat: add MonitorPanel poll loop and rendering"
```

---

### Task 6: `Workspace` wiring

**Files:**
- Modify: `src/workspace.rs` (struct ~line 55-107, `::new` ~line 110-179, `open_local`/`open_local_with`/`open_ssh`/`open_telnet`/`open_serial` ~line 200-310, `show_sftp`/`show_sftp_placeholder` region ~line 446-470, `resolve` ~line 487-498)

**Interfaces:**
- Consumes: `MonitorPanel::new(session, label, cx)`, `MonitorPlaceholder::new(cx)` (Task 5).
- Produces: nothing consumed by later tasks — final integration task.

- [ ] **Step 1: Import the new types**

In `src/workspace.rs`'s imports (~line 46, right after the `crate::panels::sftp::{SftpPanel, SftpPlaceholder}` line), add:

```rust
use crate::panels::monitor::{MonitorPanel, MonitorPlaceholder};
```

- [ ] **Step 2: Add the three new `Workspace` fields**

In `src/workspace.rs`'s struct (~line 79-90), add after `active_sftp: Option<String>,`:

```rust
    /// Host key whose SFTP browser the `PanelId::Sftp` slot resolves to.
    active_sftp: Option<String>,
    /// One 资源监控 panel per host key (created on first use, reused
    /// after) — mirrors `sftp_panels` field-for-field.
    monitor_panels: HashMap<String, AnyView>,
    /// Shown in the Monitor slot when no SSH host is active.
    monitor_placeholder: AnyView,
    /// Host key whose monitor panel the `PanelId::Monitor` slot resolves
    /// to. Unlike `active_sftp`, updating this does NOT force
    /// `right_active` to switch to `PanelId::Monitor` — the right dock's
    /// default occupant (`SavedConnections`) stays visible unless the
    /// user manually clicks the Monitor activity-bar icon; only *which
    /// host's data* is shown follows focus automatically.
    active_monitor: Option<String>,
```

- [ ] **Step 3: Remove `PanelId::Monitor` from `stub_panels`**

In `src/workspace.rs`'s `::new` (~line 140-150), remove `PanelId::Monitor` from the stub-panel loop:

```rust
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [
            PanelId::Network,
            PanelId::Security,
            PanelId::Sessions,
            PanelId::History,
        ] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid.label(), cx)).into();
            stub_panels.insert(pid, view);
        }
```

- [ ] **Step 4: Initialize the new fields**

In `src/workspace.rs`'s `::new`, right after `let sftp_placeholder: AnyView = cx.new(|cx| SftpPlaceholder::new(cx)).into();` (~line 137), add:

```rust
        let sftp_placeholder: AnyView = cx.new(|cx| SftpPlaceholder::new(cx)).into();
        let monitor_placeholder: AnyView = cx.new(|cx| MonitorPlaceholder::new(cx)).into();
```

In the `Self { ... }` literal (~line 157-178), add after `active_sftp: None,`:

```rust
            active_sftp: None,
            monitor_panels: HashMap::new(),
            monitor_placeholder,
            active_monitor: None,
```

- [ ] **Step 5: Add `show_monitor`/`show_monitor_placeholder`**

In `src/workspace.rs`, add right after `show_sftp_placeholder` (~line 465-470):

```rust
    /// Bind the Monitor slot to `config`'s host (reusing the shared
    /// connection, creating the panel once). Unlike `show_sftp`, does NOT
    /// force `right_active` — see the `active_monitor` field's doc comment.
    /// Takes `_window` (unused) only so its signature matches `show_sftp`'s
    /// at every call site — both are invoked from the same `cx.on_focus`
    /// closures, which hand both functions the same `window` binding.
    fn show_monitor(&mut self, config: SshConfig, _window: &mut Window, cx: &mut Context<Self>) {
        let key = config.key();
        if !self.monitor_panels.contains_key(&key) {
            let Some(session) = self.ssh_session(&config) else {
                return;
            };
            let label = format!("{}@{}", config.user, config.host);
            let panel: AnyView = cx.new(|cx| MonitorPanel::new(session, label, cx)).into();
            self.monitor_panels.insert(key.clone(), panel);
        }
        self.active_monitor = Some(key);
        cx.notify();
    }

    /// Detach the Monitor slot from any host so it resolves to the "no
    /// host" placeholder. Mirrors `show_sftp_placeholder`.
    fn show_monitor_placeholder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.active_monitor = None;
        cx.notify();
    }
```

- [ ] **Step 6: Add the `PanelId::Monitor` arm to `resolve`**

In `src/workspace.rs`'s `resolve` (~line 487-498):

```rust
    fn resolve(&self, id: PanelId) -> Option<AnyView> {
        match id {
            PanelId::Sftp => Some(
                self.active_sftp
                    .as_ref()
                    .and_then(|k| self.sftp_panels.get(k).cloned())
                    .unwrap_or_else(|| self.sftp_placeholder.clone()),
            ),
            PanelId::Monitor => Some(
                self.active_monitor
                    .as_ref()
                    .and_then(|k| self.monitor_panels.get(k).cloned())
                    .unwrap_or_else(|| self.monitor_placeholder.clone()),
            ),
            PanelId::SavedConnections => Some(self.saved_panel.clone()),
            other => self.stub_panels.get(&other).cloned(),
        }
    }
```

- [ ] **Step 7: Add `show_monitor`/`show_monitor_placeholder` calls at every `show_sftp`/`show_sftp_placeholder` call site**

In `src/workspace.rs`'s `open_local` (~line 203-217), add `show_monitor_placeholder` calls alongside both existing `show_sftp_placeholder` calls:

```rust
    pub fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new(window, cx));
        Self::seed_font_from_settings(&terminal, cx);
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }
```

In `src/workspace.rs`'s `open_local_with` (~line 220-251), same two additions:

```rust
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }
```

In `src/workspace.rs`'s `open_ssh` (~line 256-273), add `show_monitor` calls alongside both existing `show_sftp` calls:

```rust
    pub fn open_ssh(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.ssh_session(&config) {
            let terminal = cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session));
            Self::seed_font_from_settings(&terminal, cx);
            let follow = config.clone();
            let handle = terminal.read(cx).focus_handle(cx);
            let term_weak = terminal.downgrade();
            self.terminal_views.push(term_weak.clone());
            let sub = cx.on_focus(&handle, window, move |this, window, cx| {
                this.set_active_title_from(&term_weak, cx);
                this.show_sftp(follow.clone(), window, cx);
                this.show_monitor(follow.clone(), window, cx);
            });
            self._subscriptions.push(sub);
            let panel = cx.new(|_cx| TerminalPanel::new(terminal));
            self.add_center(Arc::new(panel), window, cx);
            self.show_sftp(config.clone(), window, cx);
            self.show_monitor(config, window, cx);
        }
    }
```

(Note: the original final line was `self.show_sftp(config, window, cx);`, moving `config` — since `show_monitor` also needs a `config: SshConfig`, the `show_sftp` call must now take `config.clone()` instead of moving it, so `show_monitor(config, ...)` on the next line still has a value to move.)

In `src/workspace.rs`'s `open_telnet` (~line 278-292), same two additions as `open_local`:

```rust
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }
```

In `src/workspace.rs`'s `open_serial` (~line 296-310), same two additions:

```rust
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
        self.show_monitor_placeholder(window, cx);
    }
```

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: clean build, no warnings. This is the step that proves every one of Step 7's 9 call sites compiles and the `open_ssh` `config`/`config.clone()` adjustment is correct.

- [ ] **Step 9: Run tests**

Run: `cargo test`
Expected: all tests pass, no regressions.

- [ ] **Step 10: Manual smoke test**

Run: `cargo run`, connect to a real Linux SSH host, then:
- Click the Monitor activity-bar icon (right side) — confirm it shows "未连接 SSH 主机" before any SSH terminal is focused.
- Focus the SSH terminal tab — confirm the Monitor panel (if you have it open) updates to that host's data, without forcibly popping open over whatever was on the right dock (e.g. if SavedConnections was showing, it stays showing until you manually click Monitor).
- If monitoring is enabled in Settings (Task 3), confirm the panel populates within one poll interval; if disabled, confirm it shows the "资源监控已在设置中禁用" message.
- Open a local-shell tab and focus it — confirm the Monitor panel (if visible) falls back to the "未连接 SSH 主机" placeholder.
- Confirm the manual refresh button in the Monitor panel's header works.
- Confirm the previously-existing SFTP panel and its own focus-following behavior are unaffected by this task's changes (this task touches the same 5 `open_*` methods SFTP's wiring lives in).

- [ ] **Step 11: Commit**

```bash
git add src/workspace.rs
git commit -m "feat: wire MonitorPanel into Workspace (per-host, focus-following, manual-toggle-visible)"
```

---

### Task 7: Final verification

**Files:** None (verification only).

**Interfaces:** None.

- [ ] **Step 1: Full build and test suite**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --all-targets`
Expected: no new warnings in `src/terminal/ssh.rs`, `src/settings.rs`, `src/panels/settings_window.rs`, `src/panels/monitor.rs`, `src/panels/mod.rs`, or `src/workspace.rs` (the only files this plan touches) compared to `main`. Pre-existing warnings elsewhere are out of scope.

- [ ] **Step 3: End-to-end manual smoke test**

Run: `cargo run` and walk through Task 3 Step 7, Task 5's build-time checks, and Task 6 Step 10's checklists in one pass, plus: confirm every other panel (SavedConnections, SFTP, quick commands, existing settings tabs) still works unchanged — this task's `Workspace` changes touch every terminal-opening method (`open_local`/`open_local_with`/`open_ssh`/`open_telnet`/`open_serial`), so a regression there would silently break terminal opening broadly, not just resource monitoring.

- [ ] **Step 4: No commit needed for this task** — it's verification only. If Step 2/3 surface a bug, fix it in the relevant task's file, re-run that task's own build/test steps, then re-run this task's steps.
