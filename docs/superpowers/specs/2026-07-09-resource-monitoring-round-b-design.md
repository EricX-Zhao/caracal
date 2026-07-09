# Resource Monitoring, round B: GPU monitoring

Date: 2026-07-09
Files under change: `src/panels/monitor.rs`, `src/settings.rs`, `src/panels/settings_window.rs`.

Item 6 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md), round B. Round A
(done) built the foundational remote-exec primitive (`SshSession::exec_command`) and the
基础 (basic system stats) panel. This round adds GPU monitoring as a second tab in the same
`MonitorPanel`, reusing round A's per-host panel, poll loop, and failure-cutoff machinery
rather than standing up a second independent panel.

## Background

nyaterm's own GPU panel (`docs/reference/nyaterm-ui-layout-analysis.md`, "GPU 监控") is
NVIDIA-only (`nvidia-smi`-driven): a 2×2 summary grid, one card per GPU with always-visible
utilization/memory bars plus a collapsible detail grid (UUID, temp, power draw/limit, fan%,
free memory), and a virtualized per-GPU process list below the cards.

`nvidia-smi --query-gpu=<fields> --format=csv,noheader,nounits` gives one CSV line per GPU
with no header row and no unit suffixes on numeric fields — a stable, scriptable format
purpose-built for exactly this use case, no custom parsing heuristics needed the way `/proc`
parsing required in round A.

`MonitorPanel` (round A, `src/panels/monitor.rs`) already has everything this round needs to
build on: a per-host instance (`Workspace.monitor_panels`), a single poll loop
(`start_poll_loop`, `cx.spawn` + `cx.background_executor().timer(...)`, self-terminating),
`SshSession::exec_command` for the remote round-trip, a 3-consecutive-failure cutoff, and the
`POLL_SCRIPT` combined-script pattern (`echo ===SECTION===` markers, ending in a trailing
`true` so one failing sub-command doesn't trip the whole script's exit status — see round A's
final-review fix). `TerminalSettings` already has `monitor_basic_enabled`/
`monitor_basic_interval_secs`, and `settings_window.rs`'s Terminal tab already has the
draft-state toggle-pill pattern (`monitor_enabled_pill`) to copy for a second toggle.

## Decisions (confirmed with user)

### Scope: GPU is a tab in the existing panel, not a new activity-bar panel

nyaterm treats 基础/GPU/进程管理/Docker as 4 independent panels, each with its own enable
toggle and poll interval. This round deliberately diverges from that: GPU becomes a second
tab inside the existing `MonitorPanel` (a new `active_tab: MonitorTab` field, `Basic`/`Gpu`
variants, a small tab-bar row). This avoids re-deriving round A's entire `Workspace`
integration (`PanelId` variant, activity-bar icon, per-host `HashMap`, the 10-call-site
focus-following wiring) a second time for what is, functionally, more monitoring data on the
same host. Every prior roadmap item already scoped down nyaterm's structure in favor of
caracal's simpler layout — this is the same kind of simplification, applied to architecture
rather than visuals this time.

### Polling: one shared loop and interval, independent enable toggle

No second poll loop, no second interval setting. `TerminalSettings` gains
`monitor_gpu_enabled: bool` (default `false`) — independent of `monitor_basic_enabled` in
principle, but since GPU monitoring rides the same poll loop that only starts when
`monitor_basic_enabled` is true, **GPU monitoring requires basic monitoring to also be
enabled** this round (stated as a Non-goal below, not silently left ambiguous). When GPU is
enabled, the combined poll script's build function includes a `===GPU===` section running
`nvidia-smi --query-gpu=... --format=csv,noheader,nounits 2>/dev/null`; when disabled, that
section is omitted from the script entirely (no wasted round-trip invoking a command the user
doesn't want data from). This means `POLL_SCRIPT` stops being a fixed `const &str` and
becomes a small function of `gpu_enabled: bool`.

### No `nvidia-smi` present → empty GPU list, not an error

If the remote host has no NVIDIA GPU (or no `nvidia-smi` on `PATH`), the `===GPU===` section's
`nvidia-smi` invocation fails and produces no stdout for that section (its stderr is
redirected to `/dev/null` at the shell level, and would land in `ChannelMsg::ExtendedData`
rather than stdout even without that redirect, per round A's channel handling). An empty or
missing `GPU` section parses to an empty `Vec<GpuStats>` — the same "missing section defaults
gracefully" pattern every round-A parser already uses, no special-casing needed. The GPU tab
renders "未检测到 NVIDIA GPU" for an empty list, distinct from the "GPU 监控已在设置中禁用"
message shown when `monitor_gpu_enabled` is off (mirroring round A's settings-shortcut-button
pattern for the disabled state).

### Fields captured, and what's deliberately dropped from nyaterm's version

`nvidia-smi --query-gpu=index,name,driver_version,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw,power.limit,fan.speed --format=csv,noheader,nounits`
— index, name, driver version, temperature (°C), utilization (%), memory used/total (MiB,
converted to bytes to match `SystemStats`'s existing byte-based convention), power draw/limit
(W, `Option<f32>` since not every GPU reports these), fan speed (%, `Option<f32>`, same
reason — some GPUs, especially in servers/blades, report `[N/A]` for fan speed).

Dropped from nyaterm's version: CUDA version (not exposed by `--query-gpu`, would need
separately parsing `nvidia-smi`'s plain-text banner output — not worth a second command for a
cosmetic header field), P-state badge, UUID, the collapse/expand per-card interaction
(everything renders flat, matching round A's simplification of nyaterm's CPU/memory
sections), and the per-GPU process list (nyaterm's own GPU panel has one, but it's scope that
belongs with the already-planned 进程管理 round, not duplicated here).

### UI: a card per GPU, flat layout matching round A's style

Tab bar (基础 / GPU) added between the header and body. GPU tab: one card per detected GPU —
name + driver version as a header line, always-visible utilization and memory-used/total bars
(same `usage_bar`/threshold-coloring helper round A already built, reused as-is for
utilization; memory bar reuses the same helper keyed on used/total), then temperature/power
draw-limit/fan-speed as a text line below (power/fan show "—" when the GPU doesn't report
them, i.e. the `Option` is `None`).

## Component structure

- `src/panels/monitor.rs`:
  - New `GpuStats` struct (index, name, driver_version, temperature_c, utilization_percent,
    memory_used, memory_total, power_draw_w: Option<f32>, power_limit_w: Option<f32>,
    fan_speed_percent: Option<f32>).
  - New `parse_gpu_section(section: &str) -> Vec<GpuStats>` — pure function, CSV-per-line
    parsing, tested the same way round A's parsers were (hand-traced fixtures, no live GPU
    needed to write correct tests since the format is fully documented and stable).
  - `POLL_SCRIPT` (`const &str`) replaced by `fn build_poll_script(gpu_enabled: bool) ->
    String`.
  - `MonitorPanel` gains `gpu_enabled: bool` (read once at construction, matching
    `enabled`'s existing read-once convention — the round A "打开设置" shortcut-button
    pattern is reused for GPU's disabled state, not a new live-reload mechanism), `active_tab:
    MonitorTab`, `gpu_stats: Vec<GpuStats>` (updated alongside `stats` on each successful
    poll).
  - New `MonitorTab` enum (`Basic`, `Gpu`) and a `render_tab_bar` method.
  - New `render_gpu_tab`/`render_gpu_card` methods, reusing `usage_bar`/`human_bytes` as-is.
- `src/settings.rs` — `TerminalSettings` gains `monitor_gpu_enabled: bool` (`#[serde(default)]`,
  `false`).
- `src/panels/settings_window.rs` — a second toggle pill (mirroring `monitor_enabled_pill`)
  for GPU monitoring, in the same Settings → Terminal section as the existing basic-monitoring
  toggle; no new interval input (shared with basic).

## Testing

- `parse_gpu_section`: unit tests using hand-written CSV fixtures matching
  `nvidia-smi --query-gpu=... --format=csv,noheader,nounits`'s documented output shape —
  single GPU, multiple GPUs, a GPU reporting `[N/A]` for fan speed (must parse to
  `fan_speed_percent: None`, not panic or default to a misleading `0.0`), and empty input
  (must return an empty `Vec`, not panic) — matches round A's parser-testing rigor.
- `build_poll_script`: a unit test confirming the `===GPU===` section is present when
  `gpu_enabled` is `true` and absent when `false` (a simple substring check on the returned
  `String`).
- `src/settings.rs`: a backward-compat test confirming an old `settings.toml` without
  `monitor_gpu_enabled` still parses, defaulting to `false` — matches round A's existing test
  shape for `monitor_basic_enabled`.
- No unit tests for GPUI rendering (tab bar, GPU cards) or the poll loop's GPU-specific
  branch — matches round A's and every other panel's zero-test convention for UI code.
- Manual smoke test must cover: enable GPU monitoring on a host with an NVIDIA GPU, confirm
  the GPU tab populates within one poll interval with plausible utilization/memory/temp
  numbers; enable it on a host without one, confirm "未检测到 NVIDIA GPU" rather than an
  error or the 3-failure cutoff firing; disable GPU monitoring, confirm the tab shows the
  disabled message + settings-shortcut button; confirm switching between the 基础/GPU tabs
  doesn't disrupt the poll loop or lose the other tab's last-known data.

## Non-goals

- No independent GPU poll interval — shares `monitor_basic_interval_secs`.
- No new activity-bar panel/`PanelId` — GPU is a tab, not a separate panel (see Decisions).
- GPU monitoring cannot be enabled independently of basic monitoring this round — both ride
  the one poll loop, which only starts when `monitor_basic_enabled` is true.
- No CUDA version, P-state, UUID, or collapse/expand per-card interaction.
- No per-GPU process list — deferred to the 进程管理 (process manager) round.
- No AMD/Intel GPU support — `nvidia-smi` only, matching nyaterm's own NVIDIA-only
  implementation.
- No live-reload of `monitor_gpu_enabled` on an already-open panel — same read-once-at-
  construction limitation round A accepted for `monitor_basic_enabled`, mitigated the same
  way (a settings-shortcut button in the disabled state, not a broadcast mechanism).
