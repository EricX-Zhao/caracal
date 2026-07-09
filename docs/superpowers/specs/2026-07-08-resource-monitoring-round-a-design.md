# Resource Monitoring, round A: remote exec primitive + 基础 (basic system stats)

Date: 2026-07-08
Files under change: `src/terminal/ssh.rs`, `src/workspace.rs`, `src/panels/activity_bar.rs`,
`src/settings.rs`, `src/panels/settings_window.rs`, new `src/panels/monitor.rs`.

Item 6 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md), the last and
largest remaining item. nyaterm's own version is 4 fully independent panels (基础/basic,
GPU, 进程管理/process manager, Docker 管理), each comparable in scope to an entire prior
roadmap item on its own. Per user decision, this is split into one round per panel, starting
with 基础 (basic system stats) — this spec covers round A only. GPU/进程管理/Docker each get
their own later brainstorm → spec → plan → implementation cycle.

## Background

`PanelId::Monitor` (`activity_bar.rs`) exists today only as a label/icon wired to a generic
`StubPanel` placeholder (`src/panels/stub.rs`) — no data-gathering path, no real panel, exists
at all (confirmed in the roadmap's own "Current state" table). `SshSession` (`terminal/ssh.rs`)
has a shell channel (interactive PTY, used by the terminal view) and an SFTP subsystem channel
(used by `SftpPanel`) but no mechanism for running a one-off, non-interactive command and
collecting its output — needed to gather remote-host stats (nyaterm monitors the *remote*
machine, not the local client, so this can't be a local `sysinfo`-crate call).

`russh` 0.61.2 (already pinned) exposes exactly what's needed:
`Handle::channel_open_session().await -> Result<Channel<Msg>>` (already used by
`open_shell_channel`, `terminal/ssh.rs:527-543`) and `Channel::exec<A: Into<Vec<u8>>>(&self,
want_reply: bool, command: A) -> Result<(), Error>` (confirmed at
`russh-0.61.2/src/channels/mod.rs:540`) — a non-pty channel that runs one command and streams
its stdout back as `ChannelMsg::Data`, exactly the same message type `shell_pump` already
reads (`terminal/ssh.rs:547-572`), just without ever calling `request_pty`/`request_shell`.

`SftpPanel` already establishes the "per-host panel instance" pattern this spec reuses
wholesale: `Workspace` keeps `sftp_panels: HashMap<String, AnyView>` keyed by
`SshConfig::key()` (`user@host:port`), an `active_sftp: Option<String>` pointer, and a
`sftp_placeholder: AnyView` fallback; `resolve(PanelId::Sftp)` (`workspace.rs:487-498`) reads
through that fallback chain; `open_ssh`'s `cx.on_focus` handler (`workspace.rs:264-267`) calls
`show_sftp` on every terminal-tab focus to keep the shown-panel in sync with whichever host's
terminal is active. `PanelId::Monitor` is on `Side::Right` (`activity_bar.rs`), whose default
occupant is `PanelId::SavedConnections` (`right_active: Option<PanelId> =
Some(PanelId::SavedConnections)`, `workspace.rs:174`) — switched manually via
`toggle_panel` (`workspace.rs:502-513`), an existing generic mechanism that needs no changes
to support a second `Side::Right` panel.

The settings page (item 1, done) already has a draft-state `SettingsWindow` with a Terminal
tab (`panels/settings_window.rs`'s `render_terminal_tab`, currently font family/size only) and
a persisted `TerminalSettings` struct (`settings.rs`) — the natural home for this round's
enable-toggle + poll-interval fields, matching nyaterm's own placement ("remote-monitoring
toggles each with a nested poll-interval field" under its Terminal settings tab).

## Decisions (confirmed with user)

### Scope: round A only, Linux remote hosts only

This spec covers only the SSH-exec primitive and the 基础 (basic system stats) panel. GPU,
进程管理 (process manager), and Docker 管理 are each deferred to their own later round. Remote
hosts are assumed Linux (reads `/proc/*`, uses `df`) — a non-Linux remote just shows an error
in this panel, same as the "no session" / "disabled in settings" empty states nyaterm already
has for this exact panel. No macOS/BSD remote-host parsing path this round.

### Data gathering: one combined shell script per poll, over a fresh exec channel

New `SshSession::exec_command(&self, command: String) -> flume::Receiver<Result<String>>`,
backed by a new `SessionCmd::Exec { command: String, reply: flume::Sender<Result<String>> }`
variant serviced in `command_loop` by spawning a `tokio::spawn`-ed task (mirroring how
`OpenShell` spawns `shell_pump` rather than blocking the command loop, `terminal/ssh.rs:479-493`)
that opens a fresh `channel_open_session()`, calls `channel.exec(true, command)`, and collects
`ChannelMsg::Data`/`ExtendedData` bytes until `Eof`/`Close`/`None` (the exact same read-loop
shape as `shell_pump`'s read half, `terminal/ssh.rs:554-572`, minus the write side since exec
sends no input). Each poll runs ONE shell command that concatenates every needed read with an
`echo ===SECTION===` marker between them, so one round-trip (not 8+) yields all the raw text
the panel needs:

```
echo ===HOSTNAME===; hostname
echo ===UNAME===; uname -srm
echo ===UPTIME===; cat /proc/uptime
echo ===LOADAVG===; cat /proc/loadavg
echo ===MEMINFO===; cat /proc/meminfo
echo ===STAT===; cat /proc/stat
echo ===NETDEV===; cat /proc/net/dev
echo ===DF===; df -B1 -x tmpfs -x devtmpfs -x overlay
```

The reply parses by splitting on `===SECTION===` markers into named blocks, then each block is
parsed independently (whitespace-separated fields per line for `/proc/stat`/`/proc/net/dev`/
`/proc/meminfo`/`df`, single values for `/proc/uptime`/`/proc/loadavg`/`hostname`/`uname`).

### CPU% and network rates need two samples — the poll loop keeps the previous one

`/proc/stat`'s per-CPU jiffie counters and `/proc/net/dev`'s per-NIC byte counters are
cumulative since boot, not instantaneous rates. `MonitorPanel` keeps the previous poll's raw
counters; each new poll computes `cpu_percent = 1 - (idle_delta / total_delta)` (standard
`/proc/stat` CPU-usage formula) and `rate = byte_delta / elapsed_secs` per NIC. The very first
poll after the panel is created (or after re-enabling) has no prior sample — it shows a
"预热中…" (warming up) placeholder for CPU%/network rows instead of a wrong or undefined
number, and the second poll onward shows real rates.

### Panel architecture: per-host, mirrors `SftpPanel` exactly

New `src/panels/monitor.rs`, `MonitorPanel` struct — one instance per connected host, not a
singleton. `Workspace` gains `monitor_panels: HashMap<String, AnyView>`, `active_monitor:
Option<String>`, `monitor_placeholder: AnyView` (mirroring `sftp_panels`/`active_sftp`/
`sftp_placeholder` field-for-field). A new `show_monitor(config: SshConfig, window, cx)`
method (mirroring `show_sftp`, `workspace.rs:448-465`) creates-or-reuses the host's
`MonitorPanel` and updates `active_monitor` — called from `open_ssh`'s `cx.on_focus` handler
(`workspace.rs:264-267`) right alongside the existing `show_sftp` call, so a `MonitorPanel`
instance always exists and stays current for whichever host's terminal has focus. Unlike
`show_sftp`, `show_monitor` does **not** force `right_active = Some(PanelId::Monitor)` — the
right dock's default occupant (`SavedConnections`) stays visible unless the user manually
clicks the Monitor activity-bar icon (the existing generic `toggle_panel`, unchanged); only
*which host's data* the Monitor panel shows follows focus automatically, not *whether it's
currently on screen*. `resolve(PanelId::Monitor)` (`workspace.rs:487-498`) mirrors
`resolve(PanelId::Sftp)`'s exact fallback-to-placeholder pattern.

Each `MonitorPanel` owns its own poll loop: `enabled` (read from `TerminalSettings` at
creation, live-updated if settings change — mirroring how `TerminalView`'s font already
broadcasts from `Workspace.terminal_views`) gates whether `cx.spawn` schedules
`exec_command` + a `cx.background_executor().timer(interval)` loop at all; a manual refresh
button in the panel header (spinning while a poll is in flight) works regardless of `enabled`,
matching nyaterm's own header-action pattern. A 3-consecutive-failure cutoff (nyaterm's own
rule, reused as-is) clears stale data and shows an error state rather than an ever-more-stale
last-known reading.

### Settings: `TerminalSettings` gains enable + interval, off by default

```rust
#[serde(default)]
pub monitor_basic_enabled: bool,
#[serde(default = "default_monitor_interval_secs")]
pub monitor_basic_interval_secs: u32,
```
`default_monitor_interval_secs() -> u32 { 5 }` (5-second poll, nyaterm's own typical default
for lightweight stats panels — GPU/进程管理/Docker rounds will each add their own analogous
pair). `monitor_basic_enabled` defaults to `false` (nyaterm: "all off by default"). Settings →
Terminal tab (`settings_window.rs`'s `render_terminal_tab`) gains a new section: a toggle
switch + a poll-interval number input, following the existing draft-state pattern (a new
`monitor_enabled` bool + `monitor_interval_input: Entity<InputState>` on `SettingsWindow`,
committed to `TerminalSettings` on Apply — same shape as the existing `font_family_input`/
`font_size_input` fields).

### UI: simplified bars/text, not nyaterm's SVG ring-gauge donuts

Matches every prior round's pattern of scoping down nyaterm's richer visuals to what
`gpui-component` already provides directly. Layout, top to bottom:
- **System** row: hostname, OS (from `uname -srm`), uptime (formatted from `/proc/uptime`'s
  seconds value) — plain text, no 2×2 grid.
- **CPU**: a linear progress bar showing aggregate CPU%, core count, and Load 1/5/15 as three
  small text badges. No per-core breakdown, no collapsible detail — deferred; the aggregate
  number is what most users check first.
- **Memory**: one progress bar (used/total, red ≥90% / amber ≥70% / default threshold
  coloring) + a text line for Available/Cached.

Progress bars: `sftp.rs`'s transfer rows already render one, but as a hand-rolled
colored-div-with-proportional-width (`render_transfer_body`, `sftp.rs:1427+`), not
`gpui-component`'s separate `progress` widget (`crates/ui/src/progress/progress.rs`, which
exists in the vendored library but isn't used anywhere in caracal yet). The implementation
plan should pick one of the two — reuse `sftp.rs`'s proven manual-div pattern (zero new API
surface to learn) or adopt `gpui-component`'s `Progress` widget (untested in this codebase,
but arguably the more "correct" long-term choice) — rather than this spec dictating it.
- **Network**: one row per NIC, ↑tx/↓rx rate (human-readable, reusing `sftp.rs`'s existing
  `human_size`-style formatting logic for the byte-rate numbers).
- **Disk**: one row per mount from `df`'s output, %-used colored by the same red/amber/default
  threshold as Memory, progress bar, total+available text.

Threshold coloring (red ≥90% / amber ≥70% / default otherwise) is nyaterm's own convention,
not a reuse of anything already in caracal — no percentage-driven threshold coloring exists
in this codebase today (only a binary success/danger status color on transfer rows,
`sftp.rs`'s `TransferStatus`). `cx.theme().danger` (already used for that) is the reusable
color token for the red band; amber and the default band's exact tokens are a small, low-risk
decision left to the implementation plan.

Empty/error states: no active SSH session for this host (shouldn't normally be reachable,
since the panel only exists once a host is connected, but the placeholder fallback covers
it), monitoring disabled in settings (shows a message + a shortcut button that opens Settings
→ Terminal), 3-consecutive-failure cutoff reached (shows the last error, a manual retry
button).

## Component structure

- `src/terminal/ssh.rs` — new `SessionCmd::Exec { command, reply }` variant; new
  `SshSession::exec_command(&self, command: String) -> flume::Receiver<Result<String>>`; new
  `command_loop` dispatch arm spawning a fresh one-shot exec task; no changes to the existing
  shell/SFTP paths.
- `src/panels/monitor.rs` (new file) — `MonitorPanel` (poll loop, parsing of the combined
  script's output into structured `SystemStats`, previous-sample storage for CPU%/network-rate
  deltas, 3-failure cutoff, manual refresh, rendering).
- `src/workspace.rs` — `monitor_panels`/`active_monitor`/`monitor_placeholder` fields (mirror
  `sftp_panels`/`active_sftp`/`sftp_placeholder`); new `show_monitor`; `resolve` gains a
  `PanelId::Monitor` arm; `open_ssh`'s `cx.on_focus` handler calls `show_monitor` alongside
  the existing `show_sftp` call.
- `src/panels/activity_bar.rs` — no structural change; `PanelId::Monitor` already exists and
  already routes to `Side::Right` — this spec just gives it a real panel instead of a stub.
- `src/settings.rs` — `TerminalSettings` gains `monitor_basic_enabled`/
  `monitor_basic_interval_secs`.
- `src/panels/settings_window.rs` — `render_terminal_tab` gains the enable-toggle + interval-
  input section; `SettingsWindow` gains the matching draft-state fields.

## Testing

- `src/terminal/ssh.rs`: no unit tests for `exec_command`/the new `SessionCmd::Exec` path —
  matches the existing zero-test convention for every other `SessionCmd`/`SftpRequest`
  variant (all thin wrappers around a live SSH session, exercised manually).
- `src/panels/monitor.rs`: the combined-script OUTPUT PARSING (splitting on `===SECTION===`
  markers, extracting CPU-jiffie/net-byte counters, computing percent/rate deltas from two
  samples) is pure-function logic over plain strings/structs with no GPUI or network
  dependency — this is exactly the kind of logic this codebase already unit-tests elsewhere
  (e.g. `terminal/telnet.rs`'s codec tests, `config.rs`'s serde round-trip tests). Write it as
  standalone functions (`fn parse_combined_output(raw: &str) -> RawSample`, `fn
  compute_stats(prev: &RawSample, cur: &RawSample, elapsed_secs: f64) -> SystemStats`) with
  unit tests covering: a normal two-sample delta computation, the first-poll-no-prior-sample
  case, a malformed/truncated section (missing marker, empty block) not panicking.
  Threshold-coloring logic (≥90% red / ≥70% amber / else default) is also a pure function,
  worth a test each for the three bands and their boundaries.
- `src/settings.rs`: a backward-compat test matching the existing
  `old_settings_file_without_terminal_table_still_deserializes`-style test already in this
  file, confirming an old `settings.toml` without the new monitor fields still parses
  (`monitor_basic_enabled` defaults `false`, `monitor_basic_interval_secs` defaults `5`).
- No unit tests for the poll loop itself, the exec-channel plumbing, or panel rendering —
  matches the existing zero-test convention for `panels/*.rs` and the async session-thread
  code in `ssh.rs`.
- Manual smoke test must cover: enable monitoring in Settings → Terminal with a real SSH
  connection open, confirm the panel populates within one poll interval; confirm CPU%/network
  rows show "预热中…" on the very first poll and real numbers from the second poll onward;
  disconnect the remote host mid-session (or block the command temporarily) and confirm the
  3-failure cutoff clears stale data and shows an error rather than an increasingly-stale
  reading; manual refresh button works and spins while in flight; open two different hosts'
  terminals and confirm the Monitor panel's content follows whichever terminal currently has
  focus, independent of whether the Monitor panel is the visible right-dock occupant at that
  moment; confirm clicking the Monitor activity-bar icon doesn't fight with SavedConnections
  for the right dock (manual toggle still works as it did before this round).

## Non-goals

- GPU monitoring, 进程管理 (process manager), Docker 管理 — each deferred to its own later
  round, per the user's explicit decomposition decision.
- No macOS/BSD remote-host support — Linux `/proc`-based parsing only.
- No destructive actions of any kind — this round is read-only stats display; process-kill,
  container-stop/remove, etc. belong to their respective later rounds.
- No per-core CPU breakdown, no collapsible per-GPU/per-process detail views — this round
  shows aggregate numbers only, matching the UI-simplification decision above.
- No historical graphing/sparklines — each poll replaces the previous displayed values, no
  chart, no retained history beyond the one previous sample needed for delta computation.
- No cross-panel data sharing (e.g. a future 进程管理 round reusing this round's exec
  primitive is expected and fine; this round doesn't attempt to build a generic "metrics bus"
  or pre-emptively abstract for panels that don't exist yet).
