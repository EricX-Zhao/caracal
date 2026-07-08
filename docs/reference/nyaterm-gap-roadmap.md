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

1. **设置页面 (Settings page)** — ✅ Done (2026-07-07, `docs/superpowers/specs/2026-07-07-settings-page-design.md` /
   `docs/superpowers/plans/2026-07-07-settings-page.md`). Standalone window confirmed
   feasible (`cx.open_window` works mid-session, not just at startup) and built with a
   draft/Apply/Confirm/Cancel model, a new `settings.toml`, and a working Appearance tab
   (font family/size, theme). General/Terminal tabs are placeholders for items 2 and 6
   below to fill in. Built first because it's the foundation everything else
   needs a place to configure into (font config already has methods with nowhere to live;
   quick commands, resource-monitor poll intervals, and file-browser preferences will all
   want settings entries too). Also the first place to resolve an open technical question:
   does gpui/gpui_platform support opening a second native OS window the way nyaterm's
   "child windows" do, or does settings need to live as an in-window overlay instead? That
   answer shapes every later "standalone window" item below.
2. **快捷命令 (Quick Commands)** — ✅ Done (2026-07-07,
   `docs/superpowers/specs/2026-07-07-quick-commands-design.md` /
   `docs/superpowers/plans/2026-07-07-quick-commands.md`). Built as a new bottom drawer
   (not the `PanelId` side-dock system) toggled from an enhanced status bar, which also
   gained a live terminal cursor-position readout (`row:col`). Minimal v1: flat list,
   inline add/edit form, execute-vs-append send modes, persisted to a new
   `quick_commands.toml` — no categories/search/sort/pin/tags/import/`{{variable}}`
   templating (deferred). Added two small reusable pieces along the way:
   `TerminalView::send_text`/`cursor_position`, and `Workspace.focused_terminal`
   (`WeakEntity`-tracked, same pattern as the settings page's `terminal_views`
   broadcast list) — both available for later items to build on.
3. **新建连接 查漏补缺 (New Connection gaps)** — ✅ Done (2026-07-08,
   `docs/superpowers/specs/2026-07-07-new-connection-window-design.md` /
   `docs/superpowers/plans/2026-07-07-new-connection-window.md`). The ~700-line inline
   `ConnForm` was ported into a standalone `NewConnectionWindow` (third reuse of the
   settings page's `cx.open_window` + `Root::new` recipe), shared by create and edit. Added
   an icon picker (dropdown list, not nyaterm's grid — gpui-component's popover primitive
   here is list-oriented) and SSH private-key authentication (`SshAuth` enum, using
   `russh` 0.61's already-vendored `keys` module — no new dependency). Jump-host/2FA/
   algorithm-order remain explicitly deferred (see the spec's Non-goals) — investigation
   showed those need much deeper `russh::client::Config` work than key auth did. Two gpui
   patterns with no prior precedent in this codebase were worked out from first
   principles and verified against the vendored source: icon-picker clicks need
   `WeakEntity::update` (not `cx.listener`, since `PopupMenuItem::on_click` is a plain
   closure), and native-file-picker-to-text-input wiring needs `cx.spawn_in` +
   `AsyncWindowContext::update` (plain `cx.spawn` can't produce the `&mut Window`
   `InputState::set_value` requires).
4. **已保存连接 查漏补缺 (Saved Connections gaps)** — ✅ Done (2026-07-08,
   `docs/superpowers/specs/2026-07-08-saved-connections-gaps-design.md` /
   `docs/superpowers/plans/2026-07-08-saved-connections-gaps.md`). Added a
   `SavedConnection.sort_order: i32` field (mirroring the existing group field) as the
   ordering source of truth; `SortMode::Default` now sorts by it instead of leaving `Vec`
   order alone. In-group drag reorder uses GPUI's `on_drag_move` (fires per-row, filtered by
   `bounds.contains(cursor)`) to detect before/after drop position relative to the hovered
   row's vertical midpoint, storing a transient hint consumed by that row's `on_drop` —
   verified against the actual pinned gpui fork that this correctly coexists with the
   existing cross-group "drop on folder header" and "drop on blank area to ungroup" targets
   (GPUI's drop dispatch stops at the first consumer via `cx.active_drag.take()` +
   `cx.stop_propagation()`, so a same-scope reorder never also triggers the ancestor ungroup
   handler). The previously-unused `tooltip_lines()` method is now wired to a
   `Tooltip::element(...)` hover card on each row. TOML export/import (reusing `AppConfig`'s
   existing serde shape, native `cx.prompt_for_new_path`/`cx.prompt_for_paths` dialogs) was
   added to the "更多" dropdown, which required converting that trigger from a plain `div()`
   to a `gpui_component::button::Button` (`.dropdown_menu(...)` is only implemented for
   `Button`, not arbitrary elements). Scope held to the spec's Non-goals: no cross-group
   drag+reposition combo, no import merge/dedup, TOML-only (no other tools' formats), no
   selective export, no jump-host in the hover card.
5. **文件浏览器 查漏补缺 (File Explorer gaps)** — split into two rounds (context menu,
   rename/move, properties cluster vs. hidden-files/bookmarks/cwd-sync cluster; see below).
   - **Round A** (context menu, rename, properties) — ✅ Done
     (2026-07-08, `docs/superpowers/specs/2026-07-08-file-explorer-gaps-round-a-design.md` /
     `docs/superpowers/plans/2026-07-08-file-explorer-gaps-round-a.md`). Added a
     `SftpRequest::Rename` backend variant (backed by `russh_sftp`'s `rename()`, mirroring the
     existing `SftpRequest::Remove`'s shape); rename reuses the panel's existing `pending_op`
     inline-banner mechanism (previously only used by 新建文件/新建文件夹) via a new
     `PendingOpKind::Rename(usize)` variant, and rejects any typed name containing `/` so it
     can never silently cross directories (caught and fixed during the final whole-branch
     review — the first attempt at that guard had an unconditional `return` that also broke
     新建文件/新建文件夹 for names containing `/`; corrected to scope the check to the
     `Rename` case only). Properties is a `gpui_component::AlertDialog` with a custom
     key/value grid (name/path/type/size/mtime/permissions), built entirely from the
     already-fetched `SftpEntry` — no new SFTP round-trip, no owner/group (deferred).
     `TableDelegate::context_menu` (a first-class `DataTable` hook, not custom per-row
     wiring) drives the six-item menu (打开/下载/重命名/属性/复制路径/删除);
     `FileTableDelegate` gained a `WeakEntity<SftpPanel>` field so menu clicks route back to
     panel methods, the same `WeakEntity::update` pattern used throughout items 3-4. Explicit
     move (dragging a file to a different directory) was dropped from this round — no
     directory-tree view exists to make it discoverable, deferred to a later round.
   - **Round B** — not started: hidden-files toggle, bookmarks/history, cwd sync.
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
