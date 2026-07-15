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

use std::sync::Arc;

use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Styled, WeakEntity, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Sizable};
use gpui_component::button::{Button, ButtonVariants};

use crate::panels::icons::{AppIcon, icon};
use crate::terminal::ssh::SshSession;
use crate::workspace::Workspace;

/// One combined shell script per poll — one round-trip instead of 7+,
/// each section preceded by an `echo ===SECTION===` marker so the reply
/// parses with `parse_combined_output`.
const POLL_SCRIPT: &str = "echo ===UNAME===; uname -srmn\n\
echo ===UPTIME===; cat /proc/uptime\n\
echo ===LOADAVG===; cat /proc/loadavg\n\
echo ===MEMINFO===; cat /proc/meminfo\n\
echo ===STAT===; cat /proc/stat\n\
echo ===NETDEV===; cat /proc/net/dev\n\
echo ===DF===; df -B1 -x tmpfs -x devtmpfs -x overlay\n\
true\n";

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
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0)
}

/// `/proc/loadavg`: "1.04 0.92 0.56 2/2388 4129263" — first three fields.
pub fn parse_loadavg(section: &str) -> (f32, f32, f32) {
    let fields: Vec<&str> = section.split_whitespace().collect();
    let get = |i: usize| fields.get(i).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
    (get(0), get(1), get(2))
}

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

pub struct MonitorPanel {
    focus_handle: FocusHandle,
    session: Arc<SshSession>,
    label: SharedString,
    /// Back-reference so the disabled empty-state's "打开设置" button can
    /// open the Settings window — mirrors `SftpPanel.workspace`.
    workspace: WeakEntity<Workspace>,
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
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = crate::settings::load();
        let this = Self {
            focus_handle: cx.focus_handle(),
            session,
            label: label.into(),
            workspace,
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
                    .child(rust_i18n::t!("Monitor.title", label = self.label.clone())),
            )
            .child(
                Button::new("monitor-refresh")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Refresh))
                    .tooltip(rust_i18n::t!("Monitor.refresh_tooltip"))
                    .loading(self.polling)
                    .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
            )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.enabled {
            let workspace = self.workspace.clone();
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(rust_i18n::t!("Monitor.disabled_message")),
                )
                .child(
                    Button::new("monitor-open-settings")
                        .xsmall()
                        .ghost()
                        .label(rust_i18n::t!("Monitor.open_settings"))
                        .on_click(move |_ev, window, cx| {
                            let _ = workspace.update(cx, |ws, cx| ws.open_settings(window, cx));
                        }),
                )
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
                        .child(rust_i18n::t!("Monitor.poll_failed", msg = msg)),
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
                .child(rust_i18n::t!("Monitor.loading"))
                .into_any_element();
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_2()
            .pb_4()
            .flex_1()
            .overflow_y_scrollbar()
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
            .child(rust_i18n::t!("Monitor.host_label", hostname = stats.hostname.clone()))
            .child(rust_i18n::t!("Monitor.os_label", os = stats.os.clone()))
            .child(rust_i18n::t!(
                "Monitor.uptime_label",
                uptime = format_uptime(stats.uptime_secs)
            ))
            .text_color(cx.theme().muted_foreground)
    }

    fn render_cpu_section(&self, stats: &SystemStats, cx: &Context<Self>) -> impl IntoElement {
        let (bar, label) = match stats.cpu_percent {
            Some(pct) => (usage_bar(pct, cx), format!("{pct:.1}%")),
            None => (usage_bar(0.0, cx), rust_i18n::t!("Monitor.warming_up").to_string()),
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
                    .child(rust_i18n::t!(
                        "Monitor.cpu_cores_load",
                        cores = stats.core_count,
                        load = format!("{:.2} {:.2} {:.2}", stats.load1, stats.load5, stats.load15)
                    )),
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
                    .child(rust_i18n::t!("Monitor.memory_label"))
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
                    .child(rust_i18n::t!(
                        "Monitor.memory_available_cached",
                        available = human_bytes(stats.mem_available),
                        cached = human_bytes(stats.mem_cached)
                    )),
            )
    }

    fn render_network_section(&self, stats: &SystemStats, cx: &Context<Self>) -> impl IntoElement {
        let mut section = div().flex().flex_col().gap_0p5().text_xs();
        section = section.child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(rust_i18n::t!("Monitor.network_label")),
        );
        for nic in &stats.net {
            let rates = match (nic.rx_rate, nic.tx_rate) {
                (Some(rx), Some(tx)) => {
                    format!("↓{}/s ↑{}/s", human_bytes(rx as u64), human_bytes(tx as u64))
                }
                _ => rust_i18n::t!("Monitor.warming_up").to_string(),
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
        section = section.child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(rust_i18n::t!("Monitor.disk_label")),
        );
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
                            .child(rust_i18n::t!(
                                "Monitor.disk_used_available",
                                used = human_bytes(disk.used),
                                available = human_bytes(disk.available)
                            )),
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
        rust_i18n::t!("Monitor.title", label = self.label.clone())
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
        rust_i18n::t!("Monitor.placeholder_title")
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
            .child(rust_i18n::t!("Monitor.no_ssh_host"))
    }
}

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
