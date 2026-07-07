# nyaterm UI layout analysis (reference for caracal)

Source: [github.com/nyakang/nyaterm](https://github.com/nyakang/nyaterm) (Tauri + React +
Rust, ~765 stars). Analyzed from a shallow clone (commit at analysis time: 2026-07-07) plus
its shipped marketing screenshots (`docs-site/static/img/home/*.png`) and its Chinese docs
site (`docs-site/docs/guide/*.md`). All `nyaterm:path:line` citations below refer to files
in *that* repo, not caracal — re-clone it if you need to check current line numbers.

This is a structural reference for reusing nyaterm's UX decisions, not a spec for caracal.
caracal is Rust/GPUI (immediate-ish retained-mode GUI, not React/DOM/CSS), so component
boundaries won't map 1:1 — but panel composition, information hierarchy, empty/loading
states, and interaction patterns (sort/filter/context-menu shapes) transfer directly.

## Common chrome pattern

Every side panel (file explorer, transfer queue, resource monitor, GPU monitor, process
manager, Docker manager, quick commands, saved connections) shares one header shape
(`nyaterm:src/components/layout/PanelHeader.tsx:12-52`): a ~36px border-bottom bar with an
uppercase small-caps title on the left, an optional `meta` string (e.g. connection count,
Docker engine version), and a right-aligned row of icon-button actions (refresh, search
toggle, add, more-menu). Worth carrying into caracal's own panel shell as a shared
convention rather than reinventing header chrome per panel.

Global layout (from `layout-and-workspace.md` + `overview-light.png`): a **vertical icon
rail** on the far right edge of the window (file/link/history/monitor/eye/gpu-card/
docker-cup/history-clock/lightning/send/record/lock icons) toggles which panel is docked
open beside it; only one right-side panel is open at a time, sized independently of the
central tab/pane area. Left side has an equivalent icon rail for file explorer / network /
security-auth / sync / settings. The center is tabs + horizontal/vertical split panes, with
tab drag-to-dock between split regions. Bottom strip hosts quick commands / serial-send /
recording / lock controls without permanently occupying a side panel slot.

---

## 1. 文件浏览器 (File Explorer)

Only available for SSH sessions (SFTP-backed); not shown for local/Telnet/serial sessions.

**Screenshot reference:** `files-light.png` — left-docked panel, header "FILE EXPLORER",
below it a 4-icon toolbar (new file / new folder / upload / download / delete / go-up /
refresh, overflowing into a compact icon row), then a flat scrollable file list (icon +
name + size, right-aligned), then a **file transfer queue sub-panel** stacked directly
below the file list within the same dock (not a separate right-side panel) showing
per-item progress with pause/cancel icons and a resolved download-directory footer.

**Structure** (`nyaterm:src/components/panel/file-explorer/`):
`FileExplorerView` → `PanelHeader` → `FileExplorerToolbar` → `FileExplorerPathBar` →
virtualized `<ul>` of `FileListItem` rows → status footer → `FileExplorerDialogs`
(Delete/Move/NewItem/NewSymlink/Properties/UnknownFileType, mounted conditionally).
`FileTransfer.tsx` is a sibling panel, not nested inside the explorer.

- **Toolbar**: New File, New Folder, divider, Upload dropdown (files vs folder), Download
  selected, Delete selected (disabled at zero selection), divider, Go-up, Refresh, divider,
  right-aligned: toggle hidden files, expanding inline search overlay (Escape collapses).
- **Path bar**: click-to-edit breadcrumb (`~`-relativized), Enter navigates/Escape cancels;
  bookmark dropdown (add current dir, list of favorites, each removable inline); while
  editing, a directory-history popup (up to 5 visible rows, scrollable beyond that).
- **List header**: sticky, 6 resizable+sortable columns — name, mtime, size (right-aligned),
  permissions, owner, group — each with a drag handle and asc/desc sort arrow. Column widths
  persist in state.
- **Rows**: manually virtualized (fixed 30px row height, 8-row overscan, no external virt.
  library). Icon + name (double-click-to-rename via delayed single-click-when-selected),
  then the other 5 columns. Shift-range / Ctrl-toggle multi-select, drag-select. ~20-item
  right-click menu grouped by separators: Open (internal/external editor) / Refresh /
  Upload / Download / Rename / Move / Delete / Add to Favorites (dirs only) / Copy path
  (full/name/dir) / Send to terminal (full/name/dir) / AI actions / Properties.
- **Sort/filter state**: `{ column, direction }` with per-column default direction; search
  is plain substring match on name; directories always sort before files, natural-name
  tiebreak.
- **Per-session cache**: `{ files, currentPath, homeDir, history, historyIndex,
  visitedHistory }` keyed by session id in a module-level map, so tab-switching restores
  last directory/scroll without a refetch.
- **Empty/edge states**: no active session / unsupported session type (non-SSH) / loading
  spinner / error text / no search results / empty directory — all centered + muted, first
  two get a folder-off icon.
- **Footer**: item count + total size of visible files (left); manual CWD-sync (disabled
  unless the session supports cwd tracking), toggle auto-sync-cwd (highlighted when on),
  "send current path to terminal" (right).
- External OS drag-and-drop upload supported via a dedicated hook + drop overlay.

**Transfer queue panel**: header actions = Pause All / Resume All / Cancel All / Clear
Completed / Clear All (each disabled based on aggregate state). Flat (non-virtualized) list
of rows: direction icon (upload=green/download=blue) or folder icon for directory
transfers, filename, byte or item-count progress, colored status pill (queued / paused /
completed / error / cancelled) with a thin progress bar while active. Row context menu:
Pause/Resume/Retry/Cancel/Open target dir (download only)/Delete. Footer: resolved download
directory (click opens in OS file manager). Empty state: swap-icon + "no transfers".

---

## 2. 已保存的连接 (Saved Connections)

**Screenshot reference:** `overview-light.png` — right-docked panel, header "SAVED
CONNECTIONS" with a count badge, filter row (search input + sort/folder/add/more icons)
below it, then a folder tree (chevron + folder icon + name + child-count badge) with leaf
connection rows (small colored square icon + hostname) indented under their folder.

**Structure** (`nyaterm:src/components/panel/saved-connections/`): `index.tsx` is the
stateful container providing a React Context consumed by `GroupNodeItem` (recursive folder
rows) and `ConnectionItem` (leaf rows) — avoids prop drilling through the tree.

- **Header row**: connection count as panel `meta`; filter bar = search input (clearable),
  cycle-sort-mode icon (default → name-asc → name-desc), "temporary SSH link" quick-connect,
  New Folder, New Connection, and a "More" dropdown (Export config / Import config /
  separator / Clear All — destructive).
- **Tree building**: connections/groups filtered by search (name/host/username substring),
  sorted per mode, folded into a `{group, children, connections, totalCount}` tree with
  auto-collapse-empty / auto-expand-matching on search, restoring manual expand state when
  search clears.
- **List body**: root group nodes, then a divider, then ungrouped connections. Drag-and-drop
  reorder/reparent (HTML5 DnD + a pointer-based fallback for WebKit/macOS) computes
  before/after/inside drop position from cursor Y fraction of row height; persisted via one
  `reorder_items` backend call plus `save_connection`/`save_group`.
- **Folder row**: chevron (rotates when collapsed) + open/closed folder icon + name +
  descendant-count badge; drop target highlights differently for "inside" (ring) vs
  "before/after" (thin colored line). Context menu: New Connection (in this folder) / New
  Subfolder / separator / Open All Connections (if any) / Rename Folder / Delete Folder.
- **Connection row**: per-type icon + name; multi-select (shift/ctrl); on hover reveals 3
  inline action buttons (Connect / Edit / Delete) as a sticky-right chip; 350ms-delayed
  hover opens a details tooltip — a 2-column key/value grid whose **fields differ by
  connection type**:
  - local_terminal → Terminal Path / Shell Args / Working Dir / Description
  - telnet → Host / Port / Description
  - serial → Serial Port / Baud Rate / Data Bits / (Parity) / (Stop Bits) / Description
  - ssh (default) → Host / Port / User / Jump Host Chain (resolved by walking
    `proxy_jump_id` links, with cycle/missing detection) / Description
  Row context menu: Connect(-Selected) / Edit / separator / Rename / Copy / separator /
  Delete.
- Dialogs: DeleteConnection, DeleteFolder, Folder (create/rename), RenameConnection,
  ClearAll, OpenGroupConnections (confirms opening N at once), ImportDialog.
- **Data model**: `SavedConnection { id, name, type, host, port, username, group_id,
  sort_order, icon, description, network.proxy_jump_id, …type-specific fields }`,
  `Group { id, name, parent_id, sort_order }`.

**Relevance to caracal**: caracal's `src/config.rs` already has a flat
`ConnectionType::{Ssh,Local,Telnet,Serial}` + `SavedConnection` and a pill-row session-type
picker (per `docs/superpowers/specs/2026-07-03-telnet-serial-terminal-design.md`). nyaterm's
per-type detail-tooltip-field-set and folder tree (with drag reorder/reparent) are the two
patterns most worth adopting if/when caracal grows folders — currently
`src/panels/saved_connections.rs` is a flat list.

---

## 3. 新建连接 (New Connection)

Opens as a **separate child window**, not a modal over the main window — deliberate, so
creating a connection doesn't interrupt the active session (see "子窗口" section of
`layout-and-workspace.md`).

**Structure** (`nyaterm:src/pages/NewSessionPage.tsx`, 1061 lines): `ChildWindowHeader` +
a 4-column tab bar (SSH / Local Terminal / Telnet / Serial) + shared chrome row + the
active tab's form + a Description textarea + error box + footer (Cancel / Save).

- **Shared chrome row** (icon picker + Name + Group), present above all 4 protocol forms:
  - Icon picker: popover, 7-column grid split into "server icons" and "system icons"
    sections; click selects and closes.
  - Group: searchable popover-combobox listing existing groups indented by computed tree
    depth, with an inline "create new group" text field at the bottom (Enter to create) and
    a hint line showing where the new group will nest.
- **SSH form** (`SshForm.tsx`, 1287 lines — the largest single form): Host+Port row;
  Username; nested 3-way Auth tabs (None / Password / Private Key) — Password itself has a
  nested 3-way source tabs (ask-when-connecting / direct password with show/hide+clear /
  saved-password picker with a "manage passwords" dialog launcher); Key has a searchable
  dropdown + "manage keys" launcher. A collapsible "Advanced config" holds: (Proxy select /
  Proxy-Jump combobox, SSH-only + jump-chain-aware filtering / 2FA: OTP combobox + auto-fill
  switch), (post-login command + delay-ms / X11 forwarding switch / Backspace Mode select),
  and an **SSH Algorithms card**: mode select (compatible/secure/custom) → when custom, 4
  sub-tabs (KEX/Ciphers/MACs/HostKeys) each an ordered checklist with per-item risk badges
  and reorder arrows.
- **Local Terminal form**: Shell Path select (built-ins: powershell.exe, cmd.exe, bash,
  wsl.exe, wt.exe, Custom…) paired with a read-only path field + native folder-picker button
  (shown for Custom or an unrecognized path); Shell Arguments; Working Directory.
- **Telnet form**: Host+Port; collapsible Advanced config with 2 inner tabs — Input
  (Backspace Mode, Enter-sends-as: CRLF/CR/LF) and Compatibility (a "raw TCP / embedded
  debug port" switch that forces Enter mode to CR when on, plus Local Echo / Local line
  editing / Force character-at-a-time / Send NAWS / Send SGA switches — the last two
  disabled under raw-TCP mode).
- **Serial form**: Serial Port select (lazy-loads on open; loading/empty/error states; marks
  stale ports "(Unavailable)"); a custom Baud Rate picker popover (preset grid + custom
  numeric entry with inline min/max validation + Apply); a 4-column grid: Data Bits (5-8) /
  Parity (none/odd/even/mark/space) / Stop Bits (1/1.5/2) / Backspace Mode.

**Relevance to caracal**: caracal's serial/telnet field set
(`docs/superpowers/specs/2026-07-03-telnet-serial-terminal-design.md`) already matches
nyaterm's serial form almost field-for-field (data bits/parity/stop bits/flow control vs
nyaterm's data bits/parity/stop bits, backspace mode). The SSH form's "Advanced config"
collapsible-section pattern (keeping jump-host/2FA/algorithm-order out of the default view)
is the main idea worth borrowing if caracal's SSH form grows past host/port/user/auth.

---

## 4. 资源监控 (Resource Monitoring)

Only meaningful for SSH sessions; each of the 4 panels below is independently toggled on/off
in Settings → Terminal (default: off) and has its own poll interval. All 4 share one
lifecycle: `enabled` flag + interval from settings → poll loop with a 3-consecutive-failure
cutoff before clearing stale data → manual refresh button (spins while in flight) in the
panel header.

### 资源监控 (basic) — `product-light.png`
Vertical stack of bordered `SectionCard`s: **System** (hostname/arch/OS/uptime, 2×2 grid),
**CPU** (SVG ring-gauge donut + linear bar + core count, a row of Load-1/5/15 badges, a
collapsible per-core list), **Memory** (ring gauge + used/total + Available/Cached chips),
**Network** (per-NIC ↑tx/↓rx rate rows), **Disk** (per-mount rows: path, %-used colored by
threshold — ≥90% red / ≥70% amber / else primary — progress bar, total+available chip).
Empty states differ by cause (no session / disabled in settings / fetch error).

### GPU 监控 — `monitoring-nvidia.png`
Header `meta` = driver + CUDA version. Body: a 2×2 summary grid (GPU count, max
utilization%, memory used/total, max temperature), then one card per GPU — "GPU #N" badge +
name + P-state badge + collapse chevron; always-visible GPU-util and mem-util bars (colored
by the same red/amber/emerald thresholds); collapsed detail grid (UUID, temp, power
draw/limit, fan%, free memory) only when expanded. Below the cards: a search input over a
**virtualized** per-GPU process list (name, PID, GPU index, memory used).

### 进程管理 — `monitoring-ps.png`
Search input + "Total: N" pill in the header. Sortable table (Process/PID/CPU/MEM/User),
columns adaptively dropped as the panel narrows (drops MEM+User <430px, User <540px,
switches to stacked cards <320px) driving a virtualized row list. Selecting a row expands an
inline detail panel below it: state badge, CPU/Memory/RSS/Elapsed 4-metric grid, scrollable
full-command-line box with copy button, a nice-value input + Apply. Per-row "⋮" menu:
Copy PID/Command, separator, SIGTERM/SIGHUP/SIGSTOP/SIGCONT, separator, SIGKILL
(destructive — routes through a confirm dialog showing the literal `kill -SIGNAL -- PID`).

### Docker 管理 — `monitoring-docker.png`
Header `meta` = engine version; header actions = refresh + "⋮" → `docker system prune -f
--volumes` (destructive, confirmed). Body: an overview strip (running/stopped/image counts),
search input, then an adaptive tab bar — Containers / Images / Volumes / Networks / Compose
(Compose only if available) — that measures rendered tab widths and collapses overflow into
a "More ▾" dropdown, each tab showing a count badge. Non-container tabs lazy-load on first
activation. All tabs share one virtualized-list wrapper. Row shapes: `ContainerRow`
(name+state badge, image+short-id, state-colored left border, "⋮" menu:
Logs/Enter-if-running/Start/Stop/Restart/separator/Kill-if-running(destructive)/Remove
(destructive)); generic `DockerObjectRow` (title/meta/detail/id + trash button, reused for
Images/Volumes/Networks); `ComposeRow` (expandable project row → lazy-loaded
`ComposeServiceRow`s). All destructive actions show the literal docker CLI command in a
confirm dialog before running.

**Relevance to caracal**: caracal is a pure terminal client today with no remote-host
telemetry. If this is ever in scope, the pattern worth copying wholesale is: independent
opt-in panels (not one monolithic "monitor" view), a shared poll/failure-cutoff lifecycle,
and destructive actions always previewing the literal shell command they'll run.

---

## 5. 快捷命令 (Quick Commands)

**Docked panel** (`nyaterm:src/components/panel/QuickCommands.tsx`): header actions (all in
the `PanelHeader.actions` slot, `|`-divided): search input, Sort dropdown (Created / Name /
Use Count), View Mode dropdown (List / Compact / Tile), Add (opens the edit form in a
**separate child window**), Import, and an AI-generate popover (prompt input + Generate).

- **Body**: fixed-width (~11rem) left sidebar of categories (All / each category /
  Uncategorized) as pill rows with dot + name + count badge (right-click on user categories
  → Edit/Delete); right pane renders commands in one of 3 layouts:
  - **tile**: wrapped small pill buttons (icon/color-dot + pin icon + label only); hover/click
    shows a rich tooltip-card (category badge, execution-mode, description, full command in
    a scrollable `<pre>`).
  - **list**: full-width rows (icon, pin flag, label, command preview line) with a
    hover-revealed action cluster (execution-mode badge, Send button, "view details"
    popover, "⋮" Edit/Send-to-all/Delete).
  - **compact**: single-line condensed rows, same actions, no badge.
  - All 3 share one context menu (Edit / Send to All / Delete) and route Edit to the same
    child-window form as Add.
- **Sort**: pinned always first, then name (locale-aware) / use-count (desc) /
  created-at (oldest-first).
- **Data model**: `QuickCommand { id, label, command, category_id?, description?,
  color_tag?, icon_tag?, pinned, execution_mode: "execute"|"append", use_count?, created_at?,
  updated_at? }`. Debounce-saved (300ms) to the backend on local edits; cross-window sync via
  Tauri events (`quick-command-saved` / `quick-commands-changed`).
- **Empty state**: dashed box, terminal icon, "no commands found", inline Add button.

**Add/Edit form** (`nyaterm:src/pages/QuickCommandPage.tsx`, child window): Label+Category
side-by-side (Category is the same searchable-combobox-with-inline-create pattern as New
Connection's group picker); Description; a combined "Color Tag & Pinned" card (6 color-dot
radios + a "+" icon-grid dropdown — picking an icon clears the color and vice versa — with a
Pin switch alongside); Execution Mode as a 2-tab toggle (Execute Immediately / Append Only)
with a hint line that changes per selection; a full-height monospace Command/Script textarea
at the bottom with inline validation.

**Relevance to caracal**: caracal has no quick-commands feature yet. The two ideas most
worth taking wholesale: (1) **execution mode** as a first-class per-command field (send +
Enter vs. paste-into-input-only-for-review) rather than always auto-executing, and (2) the
`{{variable}}` templating with a fill-in dialog at send time (documented in
`quick-commands.md` — not fully covered in the source-level pass above, but worth revisiting
the source for the variable-substitution dialog implementation if this gets built).

---

## 6. 设置页面 (Settings)

Child window with a **draft-settings pattern**: clones committed settings into local draft
state, every tab reads/writes the draft via context, and nothing persists until
Apply/Confirm (`nyaterm:src/pages/SettingsPage.tsx:337-366`).

**Layout**: left sidebar (icon-only when narrow, expands to labeled at `sm:`/`lg:`
breakpoints) grouped into 6 categories, each a flat button or an expandable group with
animated child-tab reveal:

- **Workspace** — General, Appearance, Interaction, Keybindings
- **Terminal & Session** — General, Search, Translation
- **AI** — General, Models, Rules
- **Transfer** — Transfer
- **Security** — Security
- **Sync & Backup** — Sync & Backup

Right side: active-tab header, a scrollable `max-w-5xl` centered content area (scroll
position remembered per tab), and a persistent footer (Cancel / Apply / Confirm-and-close).
Apply/Confirm disable with a tooltip when settings are in an invalid combination (e.g. cloud
sync enabled without a master password set), and the footer shows a warning banner with a
"jump to blocking tab" button in that case.

Every tab is built from two shared primitives (`SettingFormItems.tsx`): `SettingSection`
(bordered card, optional title/description/action header) and `SettingRow`/
`SettingFieldGrid` (label+description above a control, capped width, 2-up on wide layouts).

**Tab contents (skimmed):**
- **General**: language, restore-on-startup (+ restore-window-layout sub-switch), minimize
  to tray, confirm on close, a separate Diagnostics section (log level/retention, open logs,
  export diagnostics bundle).
- **Appearance**: background image (file picker, fit mode, opacity, content-opacity),
  UI theme + terminal theme (with "follow UI theme") + min-contrast-ratio, panel
  multi-open switch, UI/Terminal font family pickers (primary + addable fallbacks, checked
  against installed system fonts), font sizes/weights, cursor style + blink.
- **Terminal**: scrollback lines, keep-alive interval, X11 display, hardware acceleration,
  workspace padding, line numbers/timestamps(+ms), then remote-monitoring toggles each with
  a nested poll-interval field (Remote Stats / GPU / Process / Docker monitors — all off by
  default), action-links (3 sub-matchers: IPv4, host:port, archive filenames), and an
  "experimental" keyword-highlighting section with a full custom-rule editor — the single
  most complex tab.
- **Transfer**: download path, ask-save-location, duplicate strategy
  (overwrite/skip/rename/ask), editor type (external vs internal + path), a recording
  sub-section (path, auto-start, IO labels, timestamps, memory limit), concurrency/retries/
  buffer-size/default-permissions grid, preserve-timestamps, resume-broken-transfer.
- **Security**: master password (enable/set, locked when cloud sync requires it), session
  security (screen-lock enable + idle-lock minutes), host-key policy
  (strict/prompt/accept).
- **Proxy**: enable switch + protocol/host/port — a small, seemingly legacy single-proxy
  tab; the richer per-connection proxy config actually lives in the SSH new-connection
  form's Advanced section.
- **Sync & Backup** (largest tab): status readout (provider/last-checked/last-synced/
  current-op), enable switch (blocked + inline link to Security tab if no master password),
  provider select + device name + namespace, then provider-specific credential blocks
  (WebDAV, S3, Google Drive OAuth-like flow, GitHub Gist device-code flow with polling,
  Gitee Snippet) — only the selected provider's block renders.
- **AI**: split into General / Models (per-model list with API key fields) / Rules tabs.
- **Keyboard Shortcuts**: search, Reset All, a Custom-bindings section, inline
  conflict warnings, a record-new-shortcut capture UI, per-row reset.
- **Interaction**: 4 sections — Clipboard/Mouse (copy-on-select, OSC52 write, right-click
  paste), Command Input (suggestions + min/max-chars, word separators, duplicate-session
  delay, Alt-as-Meta, macOS IME compat), Tab Mouse Actions (double/middle/right-click
  selects), Encoding (default encoding).
- **Search**: built-in search-engine list (name/URL template/icon/show-in-menu + test
  action) plus a custom-engines section with `{query}`-placeholder validation.
- **Translation**: target language + a provider-cards section, each needing different
  credential fields, with configured/not-configured status badges.

**Relevance to caracal**: caracal has no settings UI yet (font config already exposes
`set_font_family`/`set_font_size`/`set_font_config` per
`docs/superpowers/specs/2026-07-07-bundled-fonts-cjk-design.md`, waiting on a UI). The
draft-settings-with-Apply/Confirm pattern and the shared `SettingSection`/`SettingRow`
primitives are directly reusable ideas regardless of GPUI vs React — build one layout
primitive, reuse it per tab, don't special-case each settings page's chrome.
