# SFTP transfer list: pause / resume / delete via right-click

Date: 2026-08-12
Files under change: `src/terminal/ssh.rs`, `src/panels/sftp.rs`, `src/panels/icons.rs`,
`locales/app.yml`.

Extends the SFTP panel's transfer list (bottom section of [sftp.rs](../../../src/panels/sftp.rs))
with a right-click context menu on in-flight rows (Queued/Active/Paused — today only
Done/DoneWithFailures rows get one) offering Pause/Resume/Cancel, plus a new
row-only "删除" alongside the existing local-file-deleting action for every
terminal state (Done/DoneWithFailures/Failed/Cancelled — today Failed/Cancelled
have no context menu at all).

## Background

Every transfer (single-file or directory) streams through a chunked read/write
loop in [ssh.rs](../../../src/terminal/ssh.rs) — `sftp_download_streaming`,
`sftp_upload_streaming`, and their directory-job counterparts `download_one_file`/
`upload_one_file` (called per-item from `run_download_dir`/`run_upload_dir`).
Every one of these loops already checks a per-transfer `cancel: &AtomicBool` once
per 32 KiB chunk; `SshSession` allocates that flag in `sftp_download`/`sftp_upload`/
`sftp_download_dir`/`sftp_upload_dir`, stores it in a `cancels: Arc<Mutex<HashMap<u64,
Arc<AtomicBool>>>>` keyed by transfer id, and `sftp_cancel(id)` just flips it. This
spec adds a second, parallel flag/map pair (`paused`/`pauses`) using the exact same
shape, so pausing needs no new protocol or file-handle management — the local and
remote file handles simply sit idle mid-transfer until resumed.

`Transfer` ([sftp.rs](../../../src/panels/sftp.rs)) already has `started_at: Instant`
and (as of the most recent change) `speed_bytes_per_sec()`, an average-since-start
rate used in the Active status text. This spec must adjust that calculation so a
paused interval doesn't get counted as elapsed transfer time — otherwise resuming a
transfer that was paused for a while would show a permanently and misleadingly low
speed for the rest of its life.

The transfer row's context menu today only exists for `is_done` rows (`Done` /
`DoneWithFailures`) — open file / open folder / properties / delete (which deletes
the local file after a confirm dialog, then removes the row). `Failed` and
`Cancelled` rows have no context menu at all; their only removal path is the bulk
"清除已完成" button. The small inline "✕" cancel icon shown on running rows stays
as-is — the new context menu is additive, not a replacement (confirmed with user).

## Decisions (confirmed with user)

### Pause/resume is session-local only, no cross-restart persistence

A paused transfer's flags live only in `SshSession`'s in-memory maps. Closing
caracal or losing the SSH connection invalidates any paused transfer — there is
no on-disk offset tracking, no remote-file-unchanged verification, and no
resuming a `Failed`/`Cancelled` transfer from a byte offset. Resume only ever
un-pauses a transfer this same session itself paused. This keeps the change to a
UI-flag-and-sleep-loop extension of the existing cancel mechanism, not a new
resumable-download protocol.

### The inline cancel button stays; the context menu is additive

Running rows keep their existing small "✕" icon button for one-click cancel.
The right-click menu adds Pause/Resume/Cancel on top, for both a full write-up
of options and consistency with how Done rows already use a context menu.

### Delete gets two distinct menu items on every terminal-state row

Failed/Cancelled rows currently have no context menu; this spec gives all of
Done/DoneWithFailures/Failed/Cancelled two delete-related items instead of the
current single combined one:
- **"删除"** (new) — removes the row only, no confirmation, no filesystem touch.
- **"删除本地文件"** (renamed from the current generic "删除") — the existing
  confirm-dialog + `std::fs::remove_file`/`remove_dir_all` + row-removal flow,
  unchanged in behavior, just relabeled so it reads distinctly from the file
  browser's own "删除" (which deletes a *remote* file — an unrelated action
  that keeps its existing generic `Sftp.delete` label).
- Open file / Open folder / Properties remain Done/DoneWithFailures-only — a
  Failed/Cancelled transfer's local file is likely partial or never created.

## Architecture

### Backend (`src/terminal/ssh.rs`)

- `SshSession` gains a `pauses: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>` field,
  constructed empty alongside `cancels` in `connect`.
- `sftp_download`/`sftp_upload`/`sftp_download_dir`/`sftp_upload_dir` each
  additionally allocate a `paused = Arc::new(AtomicBool::new(false))`, insert it
  into `pauses` keyed by the same `id` as `cancel`, and pass it into the
  corresponding `SftpRequest` variant.
- `SftpRequest::{Download, Upload, DownloadDir, UploadDir}` each gain a `paused:
  Arc<AtomicBool>` field and a `pauses: Arc<Mutex<HashMap<...>>>` field (mirroring
  their existing `cancel`/`cancels` fields exactly).
- New public methods, mirroring `sftp_cancel`:
  ```rust
  pub fn sftp_pause(&self, id: u64) -> bool {
      if let Some(flag) = self.pauses.lock().unwrap().get(&id).cloned() {
          flag.store(true, Ordering::Relaxed);
          true
      } else {
          false
      }
  }

  pub fn sftp_resume(&self, id: u64) -> bool {
      if let Some(flag) = self.pauses.lock().unwrap().get(&id).cloned() {
          flag.store(false, Ordering::Relaxed);
          true
      } else {
          false
      }
  }
  ```
- Every `cancels.lock().unwrap().remove(&id)` cleanup site in the four
  `SftpRequest` handler arms (each has several — one per early-return error path
  plus one after the final outcome match, matching the existing `cancels`
  cleanup pattern) gains a paired `pauses.lock().unwrap().remove(&id)`.
- `sftp_download_streaming`, `sftp_upload_streaming`, `download_one_file`,
  `upload_one_file` each gain a `paused: &AtomicBool` parameter. Their per-chunk
  loop head changes from:
  ```rust
  if cancel.load(Ordering::Relaxed) {
      return Ok(StreamingOutcome::Cancelled(transferred));
  }
  ```
  to:
  ```rust
  loop {
      if cancel.load(Ordering::Relaxed) {
          return Ok(StreamingOutcome::Cancelled(transferred));
      }
      if !paused.load(Ordering::Relaxed) {
          break;
      }
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  }
  ```
  placed immediately before the existing chunk read, so a paused transfer does
  no I/O while waiting, rechecks `cancel` every 100ms (cancel still works on a
  paused transfer), and resumes from the exact same file cursor once unset — no
  re-open, no offset bookkeeping.
- `run_download_dir`/`run_upload_dir` thread `paused` through to their per-item
  `download_one_file`/`upload_one_file` calls exactly as they already thread
  `cancel`.
- No new `TransferEvent` variant — pause/resume never needs a round-trip
  acknowledgment from the background task; the panel sets its own status
  optimistically (see below).

### Panel (`src/panels/sftp.rs`)

- `TransferStatus` gains `Paused`.
- `Transfer` gains two fields: `paused_duration: Duration` (cumulative, starts
  at `Duration::ZERO`) and `paused_since: Option<Instant>` (`None` unless
  currently paused).
- `Transfer::speed_bytes_per_sec` changes from
  `self.transferred as f64 / self.started_at.elapsed().as_secs_f64().max(0.05)`
  to subtract paused time from the elapsed denominator:
  ```rust
  fn speed_bytes_per_sec(&self) -> f64 {
      let mut paused = self.paused_duration;
      if let Some(since) = self.paused_since {
          paused += since.elapsed();
      }
      let elapsed = (self.started_at.elapsed().saturating_sub(paused))
          .as_secs_f64()
          .max(0.05);
      self.transferred as f64 / elapsed
  }
  ```
- `is_running` (currently `matches!(status, Queued | Active)`, used for the
  inline cancel button) widens to `Queued | Active | Paused`.
- `clear_completed_transfers`'s retain predicate widens the same way, so a bulk
  clear never touches a paused row.
- Two new panel methods:
  ```rust
  fn pause_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
      if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
          if matches!(t.status, TransferStatus::Active) {
              t.status = TransferStatus::Paused;
              t.paused_since = Some(Instant::now());
              self.session.sftp_pause(id);
              cx.notify();
          }
      }
  }

  fn resume_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
      if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
          if let Some(since) = t.paused_since.take() {
              t.paused_duration += since.elapsed();
          }
          t.status = TransferStatus::Active;
          self.session.sftp_resume(id);
          cx.notify();
      }
  }
  ```
- New `remove_transfer(&mut self, id: u64, cx: &mut Context<Self>)` — the
  row-only "删除": `self.transfers.retain(|t| t.id != id); cx.notify();`. No
  confirm dialog (matches `clear_completed_transfers`'s existing no-confirm
  convention for pure list bookkeeping).
- `delete_transfer_file` (existing, unchanged behavior) now also gets attached
  to Failed/Cancelled rows, not just Done/DoneWithFailures.
- Row rendering (`render_transfer_body`): the `is_done`-only `.context_menu(...)`
  attachment becomes two attachments — the existing one (now also firing for
  Failed/Cancelled, with its `open_file`/`open_folder` items only added
  `.when(is_done, ...)` and its two delete items always added) and a new one
  for `is_running` rows offering Pause (only `.when(status == Active)`) /
  Resume (only `.when(status == Paused)`) / Cancel (always, while running).
  Status text for `Paused` uses a new `"{size_a} / {size_b} · 已暂停"` form (no
  speed). Progress-bar color match gains `TransferStatus::Paused =>
  cx.theme().warning`.

## Component structure

- `src/panels/icons.rs` — two new `AppIcon` variants: `Pause` → `IconName::Pause`
  (bundled `pause.svg`, confirmed present in gpui-component's icon assets),
  `Resume` → `IconName::Play` (mirrors the existing `Record` variant's reuse of
  `IconName::Play`).
- `locales/app.yml` — new keys under `Sftp`: `pause` ("暂停"/"Pause"), `resume`
  ("继续"/"Resume"), `remove_transfer` ("删除"/"Delete" — the new row-only
  action), `delete_local_file` ("删除本地文件"/"Delete Local File" — the
  renamed existing action), `transfer_paused` ("已暂停"/"Paused" — status-text
  suffix). Reuses the existing `cancel_transfer_tooltip` ("取消传输"/"Cancel
  Transfer") as the new Cancel menu item's label — no new key needed for that
  one.

## Testing

- `transfer_progress_tests` (existing pure-logic module in `sftp.rs`) gains:
  - `speed_bytes_per_sec` with a nonzero `paused_duration` excludes it from the
    elapsed denominator (construct a `Transfer` with a known `started_at` in
    the past and a known `paused_duration`, assert the computed rate matches).
  - `speed_bytes_per_sec` with `paused_since: Some(...)` (currently paused)
    also excludes the ongoing pause interval.
  - `clear_completed_transfers` leaves a `Paused` row untouched (extends the
    existing `clear_completed_transfers_keeps_only_in_flight_rows` test).
- No new tests in `ssh.rs` — its existing test module covers pure string
  parsing (`is_sftp_session_dead`), not live-session behavior; the new
  pause/resume plumbing is a mechanical mirror of the already-untested `cancel`
  mechanism, consistent with this file's existing test scope.
- Manual smoke test: pause an in-flight download (progress bar freezes at
  `warning` color, status shows "已暂停", no speed shown); resume it (continues
  from the same byte count, never restarts, never regresses); cancel a paused
  transfer (still works); pause/resume mid-file inside a directory transfer
  (verify only the current file's progress freezes, not the whole job crashing);
  "删除" on a Cancelled/Failed row removes only the row; "删除本地文件" on the
  same row still prompts and deletes the partial local file as before.

## Non-goals

- No persistence of pause state or byte offsets across an app restart or SSH
  reconnect.
- No resuming a `Failed`/`Cancelled` transfer from a byte offset — resume only
  un-pauses a transfer this session itself paused.
- No change to `Queued`'s semantics or a real concurrency-limited queue.
- No new keyboard shortcuts for these actions — context-menu only, matching
  every other row-level action in this panel.
