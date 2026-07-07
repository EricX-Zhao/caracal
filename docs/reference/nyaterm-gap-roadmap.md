# caracal vs. nyaterm: gap analysis and roadmap

Date: 2026-07-07
Companion to: [nyaterm-ui-layout-analysis.md](./nyaterm-ui-layout-analysis.md) (structural
reference for nyaterm's UI). This document is the other half: where caracal actually stands
today against each of the 6 areas, and the agreed build order.

Each numbered item below is an independent sub-project. Per the project's planning process,
each one gets its own brainstorm → spec → plan → implementation cycle when its turn comes —
this document only fixes scope boundaries and order, not field-level design.

## Current state (verified against source, not assumed from the nyaterm doc)

| Area | nyaterm | caracal today |
|---|---|---|
| **文件浏览器** | Toolbar + path bar (bookmarks/history) + 6-column sortable virtual list + multi-select + ~20-item context menu + hidden-files toggle + bidirectional cwd sync + external drag-drop upload + properties dialog | [`sftp.rs`](../../src/panels/sftp.rs): toolbar (new file/folder/upload/download/delete/refresh) + path bar + 4-column sortable `DataTable` + multi-select (download/delete selected) + transfer queue + delete confirmation dialog. **Missing:** rename/move, properties, hidden-files toggle, bookmarks/history, cwd sync, right-click context menu, external drag-drop upload |
| **已保存的连接** | Group tree + drag reorder (before/after/inside) + per-type hover detail card + import/export + jump-chain resolution | [`saved_connections.rs`](../../src/panels/saved_connections.rs): nested group tree, search, cycling sort mode, drag-into-group, inline new-connection form, duplicate/delete/rename group — already fairly complete. **Missing:** in-group drag reorder (only move-between-groups exists; no per-connection `sort_order`), hover detail card, import/export, jump-host chain UI |
| **新建连接** | Standalone child window, 4 protocol tabs, icon picker, group picker (inline-create), SSH advanced config (jump host / 2FA / algorithm order) | Inline form embedded in the saved-connections panel (`ConnForm`), covers host/port/user/password/shell_path/working_dir + full serial field set (data_bits/parity/stop_bits/flow_control). **Missing:** standalone window, icon picker, SSH advanced config section |
| **资源监控** | 4 independent panels (basic/GPU/process/Docker), threshold-colored gauges, polling with failure cutoff, destructive-action confirmation | Only [`PanelId::Monitor`](../../src/panels/activity_bar.rs) wired to a [`StubPanel`](../../src/panels/stub.rs) placeholder. **Not implemented at all** — no data-gathering path exists yet (open question: local `sysinfo` crate vs. remote-host stats gathered over the existing SSH channel, since nyaterm's version monitors the *remote* host, not the client machine) |
| **快捷命令** | Dedicated panel (3 view modes) + standalone edit window + `{{variable}}` templating + execute-vs-append send mode + import | **Does not exist.** Only an unused `AppIcon::QuickCmd` enum value and an unrelated "quick SSH" TODO button |
| **设置页面** | Standalone child window, draft-state + Apply/Confirm, 6 categories × multiple tabs | **Does not exist.** Only a stray `AppIcon::Settings` icon and font-config methods (`set_font_family`/`set_font_size`/`set_font_config`) waiting on a UI to call them |

## Agreed build order

1. **设置页面 (Settings page)** — built first because it's the foundation everything else
   needs a place to configure into (font config already has methods with nowhere to live;
   quick commands, resource-monitor poll intervals, and file-browser preferences will all
   want settings entries too). Also the first place to resolve an open technical question:
   does gpui/gpui_platform support opening a second native OS window the way nyaterm's
   "child windows" do, or does settings need to live as an in-window overlay instead? That
   answer shapes every later "standalone window" item below.
2. **快捷命令 (Quick Commands)** — independent new feature, no dependency on the other 5,
   lowest-risk place to validate a new panel + new persisted-config pattern end to end.
3. **新建连接 查漏补缺 (New Connection gaps)** — converting the inline form to a standalone
   window reuses whatever window/overlay mechanism gets built for settings.
4. **已保存连接 查漏补缺 (Saved Connections gaps)** — in-group reorder, hover detail card,
   import/export on top of the existing solid foundation.
5. **文件浏览器 查漏补缺 (File Explorer gaps)** — context menu, rename/move, properties,
   hidden files, bookmarks, cwd sync.
6. **资源监控 (Resource Monitoring)** — largest single scope (4 sub-panels, new
   data-gathering pipeline), done last once the settings page exists to hold its per-panel
   enable toggles and poll intervals (mirroring nyaterm, where all 4 remote-monitor panels
   are off by default and only appear once enabled in Settings → Terminal).

Rationale for this order (as discussed): settings first because it's structurally a
dependency for the rest; quick commands next as the cheapest fully-independent win;
the three "fill known gaps" items next since they build on already-solid foundations;
resource monitoring last since it's the biggest 0-to-1 effort and explicitly benefits from
having settings already in place.

## Next step

When ready to start item 1, brainstorm 设置页面 specifically (window/overlay mechanism,
draft+Apply/Confirm pattern, initial tab set) through its own spec → plan cycle, per
[superpowers:brainstorming](../superpowers/specs/).
