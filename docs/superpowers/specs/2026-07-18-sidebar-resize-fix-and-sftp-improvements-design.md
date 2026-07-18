# Sidebar resize bug fix + SFTP browser improvements

## Background

Four independent items surfaced from real usage, bundled into one spec since
they're all small-to-medium and mostly touch two files
([workspace.rs](../../../src/workspace.rs) and
[sftp.rs](../../../src/panels/sftp.rs)):

1. A layout bug: closing either sidebar corrupts the other panels' resize
   behavior (wrong minimum width, flickers while dragging).
2. SFTP file browser: support uploading/downloading whole folders.
3. SFTP file browser: right-click on blank list area should show a context
   menu.
4. SFTP file browser: add an icon to the transfers section header to clear
   completed transfer records.

## 1. Bug fix: sidebar resize corruption

### Root cause

The body row is one `h_resizable("body-split")` group
([workspace.rs:1330](../../../src/workspace.rs#L1330)) with three permanent
children — left region, center dock, right region. Closing a sidebar only
toggles `.visible(false)`; the panel is never removed from the group (a
deliberate earlier fix — removing/re-adding shifted `ResizableState`'s
positional index and made a freshly reopened panel render full-width, per the
comment at [workspace.rs:1311-1324](../../../src/workspace.rs#L1311-L1324)).

`gpui_component`'s `ResizablePanel::render`
([panel.rs:271-276](https://github.com/longbridge/gpui-component/blob/1505b1487131adbb443f6c69e87847db35bfa2d1/crates/ui/src/resizable/panel.rs#L271))
special-cases invisible panels: it renders a bare `div()` with no
`on_prepaint` hook. The shared `ResizableState`'s stored `bounds`/`size` for
that panel index therefore freeze at whatever they were right before it was
hidden — never zeroed.

Every drag calls `sync_real_panel_sizes`
([mod.rs:220-224](https://github.com/longbridge/gpui-component/blob/1505b1487131adbb443f6c69e87847db35bfa2d1/crates/ui/src/resizable/mod.rs#L220)),
which re-derives *all* panels' sizes — including the hidden one — from those
frozen bounds. The hidden panel's stale, non-zero width is counted as if it
still occupies space, so the computed total overshoots the real container
width. The overflow-correction in `resize_panel_at_handle`
([mod.rs:289-294](https://github.com/longbridge/gpui-component/blob/1505b1487131adbb443f6c69e87847db35bfa2d1/crates/ui/src/resizable/mod.rs#L289))
then claws that phantom overflow back out of the panel actually being
dragged, clamping it toward its minimum regardless of mouse position, and
recomputing this differently on every mouse-move frame — the flicker.

This is an upstream `gpui_component` bug (confirmed by reading its source at
the pinned rev `1505b1487131adbb443f6c69e87847db35bfa2d1`), not a caracal
logic error.

### Fix: hand-roll the body-split layout

Replace `h_resizable`/`ResizableState` for the body row entirely, following
the pattern already established for the quick-commands drawer
(`quick_commands_drag` / `on_quick_commands_drag_move` /
`start_quick_commands_resize` / `stop_quick_commands_resize` in
[workspace.rs](../../../src/workspace.rs)):

- New `Workspace` fields: `left_width: Pixels` (default `px(200.0)`),
  `right_width: Pixels` (default `px(220.0)`), `left_drag: Option<(Pixels,
  Pixels)>`, `right_drag: Option<(Pixels, Pixels)>` — each storing
  `(mouse_x_at_drag_start, width_at_drag_start)`.
- A thin drag-handle strip between left|center (rendered only when
  `left_active.is_some()`) and between center|right (rendered only when
  `right_active.is_some()`), each wired to `on_mouse_down` →
  `start_{left,right}_resize`, with move/up handled at the top-level
  `Render for Workspace` div (matching how quick-commands resize keeps
  tracking even if the cursor strays off the thin handle).
- Widths clamp to the existing ranges: left `180.0..560.0`, right
  `200.0..500.0` (same numbers as today's `size_range` calls).
- Widths are **absolute pixels that do not rescale when the window
  resizes** (confirmed with user) — sidebars keep whatever width the user
  last set, VSCode-style; the center region is a plain
  `div().flex_1().min_w(px(0.0))` that absorbs all remaining width and
  simply gets clamped by the container when the window shrinks.
- A hidden sidebar renders at zero width with no drag handle — there is no
  shared resize state left to go stale.
- `gpui_component::resizable::{ResizableState, resizable_panel,
  h_resizable}` imports and the `body_resize` field are removed. The
  quick-commands drawer's own hand-rolled height logic is untouched (it
  never used `h_resizable` and doesn't have this bug).

## 2. Feature: SFTP folder upload/download

### Backend ([ssh.rs](../../../src/terminal/ssh.rs))

Add `sftp_download_dir(remote: String, local: PathBuf)` and
`sftp_upload_dir(local: PathBuf, remote: String)` alongside the existing
single-file `sftp_download`/`sftp_upload`
([ssh.rs:330](../../../src/terminal/ssh.rs#L330),
[ssh.rs:348](../../../src/terminal/ssh.rs#L348)). Each:

- Walks the source tree up front to build the full file list and total byte
  count — remote side via recursive `sftp_read_dir` calls, local side via
  recursive `std::fs::read_dir` — creating destination directories as
  needed (`std::fs::create_dir_all` for downloads, `sftp_mkdir` for
  uploads, mirroring the existing single-file `sftp_mkdir` usage at
  [ssh.rs:378](../../../src/terminal/ssh.rs#L378)).
- Transfers files **one at a time, sequentially**, over the existing SFTP
  session/channel — no new concurrency, keeping the same connection model
  the single-file path already uses.
- Reuses the existing `TransferEvent` enum
  ([ssh.rs:160](../../../src/terminal/ssh.rs#L160)) but with **aggregate**
  semantics across the whole folder: `Started{total}` is the sum of every
  file's size computed during the walk; `Progress{transferred}` is
  cumulative bytes across all files transferred so far (not per-file).
- Per-file errors (permission denied, disk full, etc.) are logged and
  skipped — the folder job keeps going (confirmed with user). At the end,
  if one or more files failed, the terminal event reports the failure
  count and skipped paths; if none failed, it reports `Done` exactly like
  today's single-file path.
- `TransferStatus` (in [sftp.rs](../../../src/panels/sftp.rs#L55)) gains a
  variant for "completed with N failures", distinct from plain `Done`
  (e.g. rendered in an amber/warning color instead of the success color),
  carrying the list of failed relative paths for display on hover/click.

### Frontend ([sftp.rs](../../../src/panels/sftp.rs))

- `download_selected` ([sftp.rs:610](../../../src/panels/sftp.rs#L610)) and
  `upload` ([sftp.rs:636](../../../src/panels/sftp.rs#L636)) gain a
  directory branch: instead of today's warning-and-return for `is_dir`
  ([sftp.rs:626-631](../../../src/panels/sftp.rs#L626-L631)), call the new
  `_dir` backend methods.
- Per user's choice, a folder transfer is **one `Transfer` row** for the
  whole folder (not one row per file) — showing the folder name and the
  aggregate progress bar, exactly like a file row does today. This keeps
  the transfer list from growing unbounded on large folder transfers.
- New toolbar button **"上传文件夹" (upload folder)**, placed next to the
  existing upload button ([sftp.rs:1423](../../../src/panels/sftp.rs#L1423)),
  using `cx.prompt_for_paths` with `directories: true, files: false` (the
  existing upload button keeps `files: true, directories: false` —
  native pickers don't reliably support mixed file/folder selection, so a
  dedicated button avoids that ambiguity rather than adding a submenu).
- Folder download is triggered via the existing download toolbar
  button/row-context-menu item with a directory row selected — no new UI
  entry point needed there, since a folder can already be the current
  selection. (Double-click on a directory row still navigates into it via
  `enter_dir`, unchanged — only single-file rows double-click-to-download
  today, per `wire_table_events` at
  [sftp.rs:1568](../../../src/panels/sftp.rs#L1568).)

## 3. Feature: right-click blank-area context menu

`FileTableDelegate::context_menu`
([sftp.rs:188](../../../src/panels/sftp.rs#L188)) currently only fires for
table **rows** (via `DataTable`'s per-row context-menu hook,
`TableEvent::RightClickedRow` in
[sftp.rs:1589](../../../src/panels/sftp.rs#L1589)). A right-click on empty
space below the last row isn't wired to anything.

- Investigate during implementation whether `gpui_component`'s
  `DataTable`/`TableDelegate` exposes an empty-area context-menu hook; if
  not, wrap `render_file_list`
  ([sftp.rs:1558](../../../src/panels/sftp.rs#L1558))'s container in its own
  `on_mouse_down(MouseButton::Right, ...)` that opens the same `PopupMenu`
  machinery used for row menus, scoped to the panel rather than a row
  index.
- Menu items, reusing existing panel methods 1:1 (no new logic needed):
  - 新建文件 → `new_file`
  - 新建文件夹 → `new_folder`
  - 刷新 → `refresh`
  - 上传文件到当前目录 → `upload`
  - 上传文件夹到当前目录 → new `upload_dir` method from Section 2 (added
    for consistency with the new toolbar button, per user confirmation —
    not just the four items originally listed, so a folder upload started
    from the blank menu isn't a dead end when the toolbar has it)
  - 复制当前路径 → `copy_path`

## 4. Feature: "clear completed transfers" icon

In `render_transfer_header`
([sftp.rs:1641](../../../src/panels/sftp.rs#L1641)) — currently a plain
`flex_row` div with just a label — add an icon `Button` right-aligned via
`justify_between` (or an explicit spacer div), calling a new
`clear_completed_transfers` method:

```rust
self.transfers.retain(|t| matches!(t.status, TransferStatus::Queued | TransferStatus::Active));
```

This drops `Done`, `Failed`, `Cancelled`, and the new partial-failure status
from Section 2 (confirmed: "success + failed + cancelled" all count as
"completed" for clearing purposes) — keeping only in-flight transfers. The
icon is disabled/dimmed when `self.transfers` has nothing matching the
clear-eligible statuses, so it's not a dead click when there's nothing to
clear.

## Testing

- Bug fix: manually verify (per project convention — GUI changes aren't
  screenshot-driven, ask the user to check on their real desktop) that
  dragging either sidebar handle is smooth with no flicker, and that the
  min-width clamp is correct, in all four combinations of
  left-open/closed × right-open/closed.
- Folder transfers: manual test uploading/downloading a folder with a mix
  of subdirectories and at least one intentionally-unreadable file, to
  exercise both the aggregate-progress path and the partial-failure path.
- Blank-area context menu and clear-completed icon: manual verification in
  the running app.
