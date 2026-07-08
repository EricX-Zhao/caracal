# File Explorer gaps, round A: context menu, rename, properties

Date: 2026-07-08
Files under change: `src/panels/sftp.rs`, `src/terminal/ssh.rs`.

Item 5 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md) bundles 6
independent gaps. Per user decision, split into two rounds: round A (this spec) covers the
file-operations cluster — right-click context menu, rename, properties dialog — all reachable
from the same new context menu. Round B (hidden-files toggle, bookmarks/history, cwd sync)
gets its own brainstorm → spec → plan cycle later.

## Background

`src/panels/sftp.rs`'s `SftpPanel` already has: a toolbar (new file/folder/upload/download/
delete/refresh), a path bar, a 4-column sortable `DataTable` (name/mtime/size/perms — so
permissions are already fetched and rendered, just not in a dedicated properties view), a
transfer queue, and a delete-confirmation dialog (`window.open_alert_dialog`, used by
`delete_selected`). Right-click already selects a row (`wire_table_events`'s
`TableEvent::RightClickedRow` handler, `sftp.rs:1060-1064`) but no menu appears — `gpui_component`'s
`TableDelegate` trait has a `context_menu(&mut self, row_ix, menu, window, cx) -> PopupMenu`
hook (`crates/ui/src/table/delegate.rs:101-109`) that `FileTableDelegate` (`sftp.rs:94-111`)
has never overridden, so it falls through to the no-op default.

There's also an existing inline-input mechanism, `pending_op: Option<(PendingOpKind,
Entity<InputState>)>` (`sftp.rs`), currently used only by `new_file`/`new_folder`: it shows a
single banner row above the table ("新建文件: [___]", `render_pending_op_row`,
`sftp.rs:899-925`) with a submit-on-Enter input, committed by `commit_pending_op`
(`sftp.rs:585-624`). This is the natural mechanism to extend for rename — no new UI
primitive needed.

`SftpEntry` (`terminal/ssh.rs:59-70`) already carries `name`, `is_dir`, `size`, `mtime`,
`perms` — everything a properties view needs except owner/group, which the user decided to
leave out of this round (see Decisions). `russh_sftp::client::session::SftpSession` has a
`rename<O, N>(&self, oldpath: O, newpath: N) -> SftpResult<()>`
(`russh-sftp-2.3.0/src/client/session.rs:220`) directly usable for the new rename backend
call — no protocol-level work needed, same tier of effort as the existing `sftp_remove`.

## Decisions (confirmed with user)

### Scope: round A vs. round B

Round A = context menu + rename + properties (this spec). Round B (separate spec later) =
hidden-files toggle + bookmarks/history + cwd sync.

### "Move" is dropped from this round

`sftp.rs` has no directory-tree view — only a flat single-directory list + breadcrumb path
bar — so there's no drag target for a cross-directory move the way nyaterm's tree view
supports. Rename covers same-directory renames (the common case). Cross-directory move is
deferred to a later round, if/when a tree view or a destination-picker dialog exists to make
it discoverable; not attempted here as a text-only "move to..." bolt-on.

### Context menu: full set, reusing existing panel methods where possible

Right-clicking a row selects it (existing behavior, unchanged) and shows: 打开 (open),
下载 (download), 重命名 (rename), 属性 (properties), 复制路径 (copy path), 删除 (delete).
Four of the six reuse existing panel logic directly (selection is already synced by the time
the menu builds, since right-click sets it first):
- 打开 → same is_dir branch `wire_table_events`'s `DoubleClickedRow` handler already uses:
  directory → `enter_dir`, file → `download`.
- 下载 → `download_selected` (already guards "暂不支持下载文件夹" for directories, already
  reads off the selected row).
- 删除 → `delete_selected` (already shows the existing confirm dialog, already reads off the
  selected row).
- 复制路径 → **not** a reuse of the existing `copy_path` method (`sftp.rs:760-762`), which
  copies the panel's *current directory* path (`self.path`) for the toolbar. This is a new,
  differently-scoped method — copies the *specific row's* full remote path
  (`remote_join(&self.path, &entry.name)`) — named `copy_entry_path` to avoid confusion with
  the pre-existing directory-level `copy_path`.
- 重命名, 属性 → new (see below).

### Rename: extend the existing `pending_op` banner

`PendingOpKind` gains `Rename(usize)` (carries the row index being renamed). Triggering
rename from the context menu creates an `InputState` pre-filled with the entry's current
`name` (mirroring `new_file`/`new_folder`'s `submit_on_enter(true)` input, differing only in
`default_value`), stores `(PendingOpKind::Rename(ix), entity)` in `pending_op`, and
`render_pending_op_row`'s label becomes "重命名「old_name」为:" for this variant.
`commit_pending_op`'s `match kind` gains a `Rename(ix)` arm: reads the new name from the
input, builds `old_remote = remote_join(&self.path, &old_name)` and
`new_remote = remote_join(&self.path, &new_name)` (both under the *same* `self.path` — no
directory crossing, consistent with dropping "move"), calls the new
`session.sftp_rename(old_remote, new_remote)`, and on success updates the entry in place
(`entries[ix].name = new_name`) rather than a full `refresh()` round-trip (cheaper, and
avoids a table-scroll/selection jump — matches how the existing delete path patches
`entries` in place via `.retain(...)` instead of calling `refresh()`).

Backend: `SshSession::sftp_rename(&self, old: String, new: String) ->
flume::Receiver<Result<()>>`, a new `SftpRequest::Rename { old: String, new: String, reply:
flume::Sender<Result<()>> }` variant, serviced in the session-thread dispatch match
(mirroring `SftpRequest::Remove`'s exact shape: `clone_sftp`, log, call, log-on-error, reply)
by calling `sftp.rename(old, new).await.map_err(|e| anyhow!("rename {old:?} -> {new:?}: {e}"))`.

### Properties: `AlertDialog` with a custom key/value grid, no new backend call

`gpui_component::dialog::AlertDialog` (`crates/ui/src/dialog/alert_dialog.rs`) without
`.confirm()` is an OK-only info dialog; its `.description(impl IntoElement)` accepts
arbitrary content (not just text — confirmed at `alert_dialog.rs:176`), so the dialog body is
built as a 2-column grid (same technique as the saved-connections hover card from the
previous roadmap item): 名称, 路径 (`remote_join(&self.path, &entry.name)`), 类型
(文件/文件夹), 大小 (reuse the existing `human_size` formatting already used in the size
column), 修改时间 (reuse the existing `human_mtime` helper), 权限 (reuse the existing
octal-to-rwx formatting already used in the perms column). All six fields come from the
already-fetched `SftpEntry` for that row — no `stat()` round-trip, no new `SftpRequest`
variant, per the user's explicit choice not to add owner/group (which would have required
one).

### `FileTableDelegate` needs a way back to `SftpPanel`

`TableDelegate::context_menu`'s receiver is `&mut Context<TableState<Self>>` (`Self` =
`FileTableDelegate`), not `SftpPanel` — menu item clicks need to reach `SftpPanel`'s methods
(`download_selected`, `delete_selected`, the new rename/properties/copy-path triggers).
`FileTableDelegate` gains a `panel: WeakEntity<SftpPanel>` field, set at construction
(`FileTableDelegate::new(panel: WeakEntity<SftpPanel>)`, called from `SftpPanel::new` with
`cx.entity().downgrade()`). Each `PopupMenuItem::on_click` closure captures a clone of
`panel` and calls `panel.update(cx, |panel, cx| panel.some_method(...))` — the same
`WeakEntity::update` pattern already used for the icon-picker dropdown and the saved-
connections More-menu (no new pattern, direct reuse).

## Component structure

- `src/terminal/ssh.rs` — new `SftpRequest::Rename { old, new, reply }` variant; new
  `SshSession::sftp_rename(&self, old: String, new: String) -> flume::Receiver<Result<()>>`;
  new match arm in the session-thread SFTP dispatch, mirroring `SftpRequest::Remove`'s shape.
- `src/panels/sftp.rs`:
  - `FileTableDelegate` gains `panel: WeakEntity<SftpPanel>`; `TableDelegate::context_menu`
    override builds the 6-item menu described above.
  - `PendingOpKind` gains `Rename(usize)`; `commit_pending_op` gains the rename arm;
    `render_pending_op_row`'s label match gains the rename case.
  - New `SftpPanel` methods: `copy_entry_path(ix: usize, cx)`, `rename_entry(ix: usize,
    window, cx)` (builds the pre-filled input and sets `pending_op`), `show_properties(ix:
    usize, window, cx)` (builds and opens the `AlertDialog`).
  - `remote_join`, `human_size`, `human_mtime`, and the existing octal-perms formatting are
    reused as-is (no changes) by the new properties grid and the rename path-building.

## Testing

- `src/terminal/ssh.rs`: no unit tests for the new `SftpRequest::Rename` path — matches the
  existing convention that `sftp_remove`/`sftp_download`/`sftp_upload` etc. have no unit
  tests either (they're thin wrappers around a live SFTP session, exercised manually; this
  codebase's SSH-session-thread code is integration-tested by hand, not mocked).
- `src/panels/sftp.rs`: no unit tests for the context menu, rename banner, or properties
  dialog — matches the existing zero-test convention across `panels/*.rs`.
- Manual smoke test must cover: right-click a file and a folder (confirm the menu appears
  with all 6 items both times; for a folder, confirm 下载 still surfaces
  `download_selected`'s existing "暂不支持下载文件夹" warning rather than silently doing
  nothing or erroring); rename a file and a
  folder (confirm the list updates without a jarring refresh); attempt renaming to an empty
  name (should no-op, same guard `commit_pending_op` already has for new file/folder);
  attempt renaming to a name that collides with an existing entry (server should error, confirm
  the panel surfaces it via `status` the same way delete-failure does, not a panic); open
  properties on one file and one folder, confirm all 6 fields render correctly; copy path from
  the context menu and paste it somewhere, confirm it's the *entry's* path, not the current
  directory's path (i.e. confirm it differs from the toolbar's existing 复制路径 when not at
  the entry's own directory level — trivially true since entry paths are always one segment
  longer, but worth eyeballing once).

## Non-goals

- No cross-directory move (see Decisions) — deferred to a later round pending a tree view or
  destination-picker.
- No owner/group in the properties dialog (see Decisions) — would need a new `stat()`
  round-trip; deferred, easy to add later if requested.
- No bulk rename (renaming multiple selected rows at once) — the existing multi-select
  toolbar actions (download/delete) don't have a rename equivalent in nyaterm's own scope
  breakdown either; this round's rename is single-row only, matching how the context menu
  itself only opens on a single right-clicked row.
- No permissions *editing* (chmod) in the properties dialog — display-only, matching the
  existing perms column which is also display-only.
- Round B's hidden-files toggle, bookmarks/history, and cwd sync are explicitly out of scope
  for this spec.
