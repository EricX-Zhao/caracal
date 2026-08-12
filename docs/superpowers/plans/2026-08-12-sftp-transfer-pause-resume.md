# SFTP transfer pause/resume/delete Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add right-click Pause/Resume/Cancel to in-flight SFTP transfer rows, and a row-only "删除" alongside the existing local-file-deleting action on every terminal-state row (today Failed/Cancelled rows have no context menu at all).

**Architecture:** The backend (`src/terminal/ssh.rs`) already threads a per-transfer `cancel: &AtomicBool` through every chunked read/write loop, checked once per 32 KiB chunk. Pause adds a second, identically-shaped `paused: &AtomicBool` flag/map pair; a shared `wait_while_paused` helper blocks the loop (without closing any file handle) while set, still rechecking `cancel` every 100ms. No new `TransferEvent` variant — the panel flips its own status optimistically and calls the new `sftp_pause`/`sftp_resume` methods, since the flag's take-effect latency is under one chunk.

**Tech Stack:** Rust, tokio (async streaming loops on a dedicated session thread), gpui/gpui-component (`PopupMenu`/`PopupMenuItem`/`ContextMenuExt`), rust-i18n.

## Global Constraints

- CLAUDE.md §2: one Session = one connection — this plan adds no new channels, only new flags on the existing transfer tasks.
- Pause/resume is session-local only: no persistence of pause state or byte offsets across an app restart or SSH reconnect (confirmed with user).
- The existing inline "✕" cancel icon button on running rows stays as-is; the new context menu is additive, not a replacement (confirmed with user).
- Every terminal-state row (Done/DoneWithFailures/Failed/Cancelled) gets exactly two delete-related menu items: "删除" (row-only, no confirmation) and "删除本地文件" (existing confirm-dialog + file-delete flow, relabeled) (confirmed with user).
- Open file / Open folder / Properties stay Done/DoneWithFailures-only.
- No new keyboard shortcuts — context-menu only, matching every other row-level action in this panel.

**Deviation from the committed design spec:** the spec's "Component structure" section
proposed new `AppIcon::Pause`/`AppIcon::Resume` icons for the menu items. Re-checking
`render_transfer_body`'s actual current code while writing this plan: every existing
item in this exact context menu (open file / open folder / properties / delete) is a
plain text `PopupMenuItem::new(label)` with no icon at all — this specific menu's own
established convention is text-only. Adding icons here would be inconsistent with it
for no functional benefit, so this plan drops the `icons.rs` changes and keeps every
new menu item (Pause/Resume/Cancel/Delete/Delete Local File) text-only, matching the
menu's existing siblings. This is an implementation-level simplification, not a
behavior or scope change.

---

### Task 1: Backend pause/resume plumbing

**Files:**
- Modify: `src/terminal/ssh.rs`

**Interfaces:**
- Consumes: nothing new from other tasks (this task is foundational).
- Produces: `SshSession::sftp_pause(&self, id: u64) -> bool`, `SshSession::sftp_resume(&self, id: u64) -> bool` (mirroring the existing `sftp_cancel(&self, id: u64) -> bool`). Consumed by Task 2's `pause_transfer`/`resume_transfer` panel methods.

No dedicated unit tests for this task — this file's existing test module covers only pure string parsing (`is_sftp_session_dead`), not live-session behavior; the new pause/resume plumbing is a mechanical mirror of the already-untested `cancel` mechanism. Verify via `cargo build` + `cargo test --locked` (existing tests must still pass).

- [ ] **Step 1: Add the `wait_while_paused` helper**

In `src/terminal/ssh.rs`, immediately before the `StreamingOutcome` enum (find `enum StreamingOutcome {`), insert:

```rust
/// Blocks while `paused` is set, rechecking every 100ms so a concurrent
/// `cancel` still takes effect promptly (a paused transfer can still be
/// cancelled). Returns `true` if `cancel` became set while waiting (the
/// caller should abort as cancelled), `false` once `paused` cleared
/// normally (the caller should proceed). Never closes or reopens any file
/// handle — the caller's already-open handles just sit idle.
async fn wait_while_paused(paused: &AtomicBool, cancel: &AtomicBool) -> bool {
    while paused.load(Ordering::Relaxed) {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}
```

- [ ] **Step 2: Thread `paused` through `SftpRequest`'s four transfer variants**

In `src/terminal/ssh.rs`, the `SftpRequest` enum's `Download`/`Upload`/`DownloadDir`/`UploadDir` variants currently read:

```rust
    Download {
        remote: String,
        local: PathBuf,
        reply: flume::Sender<TransferHandle>,
        /// Pre-allocated transfer id (so the caller can register UI state
        /// before the work even starts).
        id: u64,
        /// Cancellation flag; when set, the streaming loop aborts.
        cancel: Arc<AtomicBool>,
        /// Shared map of cancel flags; cleaned up after the terminal event.
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    Upload {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Recursively download every file under `remote` into `local`,
    /// creating matching subdirectories as needed. One `TransferHandle` /
    /// event stream for the whole job — see `TransferEvent::DoneWithFailures`.
    DownloadDir {
        remote: String,
        local: PathBuf,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Recursively upload every file under `local` into `remote`. Mirrors
    /// `DownloadDir`.
    UploadDir {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
```

Replace with (each gains a `paused`/`pauses` pair mirroring `cancel`/`cancels`):

```rust
    Download {
        remote: String,
        local: PathBuf,
        reply: flume::Sender<TransferHandle>,
        /// Pre-allocated transfer id (so the caller can register UI state
        /// before the work even starts).
        id: u64,
        /// Cancellation flag; when set, the streaming loop aborts.
        cancel: Arc<AtomicBool>,
        /// Shared map of cancel flags; cleaned up after the terminal event.
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
        /// Pause flag; when set, the streaming loop stops doing I/O (without
        /// closing any file handle) until cleared.
        paused: Arc<AtomicBool>,
        /// Shared map of pause flags; cleaned up after the terminal event.
        pauses: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    Upload {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
        paused: Arc<AtomicBool>,
        pauses: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Recursively download every file under `remote` into `local`,
    /// creating matching subdirectories as needed. One `TransferHandle` /
    /// event stream for the whole job — see `TransferEvent::DoneWithFailures`.
    DownloadDir {
        remote: String,
        local: PathBuf,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
        paused: Arc<AtomicBool>,
        pauses: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Recursively upload every file under `local` into `remote`. Mirrors
    /// `DownloadDir`.
    UploadDir {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
        paused: Arc<AtomicBool>,
        pauses: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
```

- [ ] **Step 3: Add the `pauses` map to `SshSession`**

Currently:

```rust
pub struct SshSession {
    cmd_tx: flume::Sender<SessionCmd>,
    /// Monotonic id allocator for transfers (`sftp_download` / `sftp_upload`).
    /// Each transfer task in the session thread gets a fresh id; the panel
    /// uses the id to find the matching `Transfer` row when it processes
    /// events.
    next_id: Arc<AtomicU64>,
    /// Cancellation flags keyed by transfer id. Allocated when a transfer is
    /// started, cleaned up when its terminal event (Done/Failed/Cancelled)
    /// is emitted. Wrapped in Mutex so the public `sftp_cancel` API can set
    /// the flag from the panel's async task.
    cancels: Arc<std::sync::Mutex<std::collections::HashMap<u64, Arc<std::sync::atomic::AtomicBool>>>>,
    _thread: thread::JoinHandle<()>,
}
```

Replace with:

```rust
pub struct SshSession {
    cmd_tx: flume::Sender<SessionCmd>,
    /// Monotonic id allocator for transfers (`sftp_download` / `sftp_upload`).
    /// Each transfer task in the session thread gets a fresh id; the panel
    /// uses the id to find the matching `Transfer` row when it processes
    /// events.
    next_id: Arc<AtomicU64>,
    /// Cancellation flags keyed by transfer id. Allocated when a transfer is
    /// started, cleaned up when its terminal event (Done/Failed/Cancelled)
    /// is emitted. Wrapped in Mutex so the public `sftp_cancel` API can set
    /// the flag from the panel's async task.
    cancels: Arc<std::sync::Mutex<std::collections::HashMap<u64, Arc<std::sync::atomic::AtomicBool>>>>,
    /// Pause flags keyed by transfer id. Mirrors `cancels` exactly — same
    /// allocation/cleanup lifecycle, set from `sftp_pause`/`sftp_resume`.
    pauses: Arc<std::sync::Mutex<std::collections::HashMap<u64, Arc<std::sync::atomic::AtomicBool>>>>,
    _thread: thread::JoinHandle<()>,
}
```

- [ ] **Step 4: Initialize `pauses` in `connect`**

Currently:

```rust
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Arc::new(Self {
                cmd_tx,
                next_id,
                cancels: Arc::new(std::sync::Mutex::new(HashMap::new())),
                _thread: thread,
            })),
```

Replace with:

```rust
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Arc::new(Self {
                cmd_tx,
                next_id,
                cancels: Arc::new(std::sync::Mutex::new(HashMap::new())),
                pauses: Arc::new(std::sync::Mutex::new(HashMap::new())),
                _thread: thread,
            })),
```

- [ ] **Step 5: Allocate + register + pass `paused` in the four `sftp_*` methods**

Currently:

```rust
    pub fn sftp_download(&self, remote: String, local: PathBuf) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Download {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
        }));
        rx
    }

    /// Upload `local` to `remote`. Same progress-stream shape as
    /// [`Self::sftp_download`].
    pub fn sftp_upload(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Upload {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
        }));
        rx
    }

    /// Recursively download `remote` (a directory) into `local`. Same
    /// `TransferHandle`/event-stream shape as [`Self::sftp_download`], but
    /// `Started.total`/`Progress.transferred` are aggregate across every
    /// file in the tree, and the terminal event may be
    /// [`TransferEvent::DoneWithFailures`] if some files failed.
    pub fn sftp_download_dir(&self, remote: String, local: PathBuf) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::DownloadDir {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
        }));
        rx
    }

    /// Recursively upload `local` (a directory) into `remote`. Mirrors
    /// [`Self::sftp_download_dir`].
    pub fn sftp_upload_dir(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::UploadDir {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
        }));
        rx
    }

    /// Request cancellation of an in-flight transfer. Returns true if the
    /// id was found and a cancel was issued. The streaming loop checks
    /// the flag each chunk and aborts, emitting `TransferEvent::Cancelled`.
    pub fn sftp_cancel(&self, id: u64) -> bool {
        if let Some(flag) = self.cancels.lock().unwrap().get(&id).cloned() {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
```

Replace with:

```rust
    pub fn sftp_download(&self, remote: String, local: PathBuf) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let paused = Arc::new(AtomicBool::new(false));
        self.pauses.lock().unwrap().insert(id, paused.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Download {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
            paused,
            pauses: self.pauses.clone(),
        }));
        rx
    }

    /// Upload `local` to `remote`. Same progress-stream shape as
    /// [`Self::sftp_download`].
    pub fn sftp_upload(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let paused = Arc::new(AtomicBool::new(false));
        self.pauses.lock().unwrap().insert(id, paused.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Upload {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
            paused,
            pauses: self.pauses.clone(),
        }));
        rx
    }

    /// Recursively download `remote` (a directory) into `local`. Same
    /// `TransferHandle`/event-stream shape as [`Self::sftp_download`], but
    /// `Started.total`/`Progress.transferred` are aggregate across every
    /// file in the tree, and the terminal event may be
    /// [`TransferEvent::DoneWithFailures`] if some files failed.
    pub fn sftp_download_dir(&self, remote: String, local: PathBuf) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let paused = Arc::new(AtomicBool::new(false));
        self.pauses.lock().unwrap().insert(id, paused.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::DownloadDir {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
            paused,
            pauses: self.pauses.clone(),
        }));
        rx
    }

    /// Recursively upload `local` (a directory) into `remote`. Mirrors
    /// [`Self::sftp_download_dir`].
    pub fn sftp_upload_dir(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let paused = Arc::new(AtomicBool::new(false));
        self.pauses.lock().unwrap().insert(id, paused.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::UploadDir {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
            paused,
            pauses: self.pauses.clone(),
        }));
        rx
    }

    /// Request cancellation of an in-flight transfer. Returns true if the
    /// id was found and a cancel was issued. The streaming loop checks
    /// the flag each chunk and aborts, emitting `TransferEvent::Cancelled`.
    pub fn sftp_cancel(&self, id: u64) -> bool {
        if let Some(flag) = self.cancels.lock().unwrap().get(&id).cloned() {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Pause an in-flight transfer. Returns true if the id was found and a
    /// pause was issued. The streaming loop stops doing I/O (leaving its
    /// file handles open) until `sftp_resume` clears the flag, or
    /// `sftp_cancel` aborts it while paused.
    pub fn sftp_pause(&self, id: u64) -> bool {
        if let Some(flag) = self.pauses.lock().unwrap().get(&id).cloned() {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Resume a paused transfer. Returns true if the id was found and a
    /// resume was issued.
    pub fn sftp_resume(&self, id: u64) -> bool {
        if let Some(flag) = self.pauses.lock().unwrap().get(&id).cloned() {
            flag.store(false, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
```

- [ ] **Step 6: Thread `paused` through the four `SftpRequest` handler arms**

These four arms live in the function containing the big `match request { ... }` that starts with `SftpRequest::ReadDir { ... } => { ... }` (search for `SftpRequest::Download {` to find the first one). Replace each arm in full.

Currently (`Download` arm):

```rust
        SftpRequest::Download {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels,
        } => {
            // Spawn the download on a background task. The reply goes back
            // to the panel immediately with the TransferHandle so the UI
            // can register a placeholder row; the actual work happens
            // asynchronously and the panel pumps events via
            // `handle.events`.
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let total = match sftp.open(&remote).await {
                    Ok(f) => f.metadata().await.map(|m| m.size.unwrap_or(0)),
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("open {remote:?}: {e}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        return;
                    }
                };
                let total = match total {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("stat {remote:?}: {e}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        return;
                    }
                };
                let _ = events_tx.send(TransferEvent::Started { total });
                let outcome = sftp_download_streaming(&sftp, &remote, &local, total, &events_tx, &cancel).await;
                match outcome {
                    Ok(StreamingOutcome::Completed(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Done { transferred });
                    }
                    Ok(StreamingOutcome::Cancelled(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Cancelled { transferred });
                    }
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                    }
                }
                cancels.lock().unwrap().remove(&id);
            });
        }
```

Replace with:

```rust
        SftpRequest::Download {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels,
            paused,
            pauses,
        } => {
            // Spawn the download on a background task. The reply goes back
            // to the panel immediately with the TransferHandle so the UI
            // can register a placeholder row; the actual work happens
            // asynchronously and the panel pumps events via
            // `handle.events`.
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            pauses.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let total = match sftp.open(&remote).await {
                    Ok(f) => f.metadata().await.map(|m| m.size.unwrap_or(0)),
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("open {remote:?}: {e}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        pauses.lock().unwrap().remove(&id);
                        return;
                    }
                };
                let total = match total {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("stat {remote:?}: {e}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        pauses.lock().unwrap().remove(&id);
                        return;
                    }
                };
                let _ = events_tx.send(TransferEvent::Started { total });
                let outcome =
                    sftp_download_streaming(&sftp, &remote, &local, total, &events_tx, &cancel, &paused)
                        .await;
                match outcome {
                    Ok(StreamingOutcome::Completed(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Done { transferred });
                    }
                    Ok(StreamingOutcome::Cancelled(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Cancelled { transferred });
                    }
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                    }
                }
                cancels.lock().unwrap().remove(&id);
                pauses.lock().unwrap().remove(&id);
            });
        }
```

Currently (`Upload` arm):

```rust
        SftpRequest::Upload {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                // For uploads, total = local file size (the data we'll push).
                let total = match tokio::fs::metadata(&local).await {
                    Ok(m) => m.len(),
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("stat {local:?}: {e}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        return;
                    }
                };
                let _ = events_tx.send(TransferEvent::Started { total });
                let outcome = sftp_upload_streaming(&sftp, &local, &remote, total, &events_tx, &cancel).await;
                match outcome {
                    Ok(StreamingOutcome::Completed(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Done { transferred });
                    }
                    Ok(StreamingOutcome::Cancelled(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Cancelled { transferred });
                    }
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                    }
                }
                cancels.lock().unwrap().remove(&id);
            });
        }
```

Replace with:

```rust
        SftpRequest::Upload {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels,
            paused,
            pauses,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            pauses.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                // For uploads, total = local file size (the data we'll push).
                let total = match tokio::fs::metadata(&local).await {
                    Ok(m) => m.len(),
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("stat {local:?}: {e}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        pauses.lock().unwrap().remove(&id);
                        return;
                    }
                };
                let _ = events_tx.send(TransferEvent::Started { total });
                let outcome =
                    sftp_upload_streaming(&sftp, &local, &remote, total, &events_tx, &cancel, &paused).await;
                match outcome {
                    Ok(StreamingOutcome::Completed(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Done { transferred });
                    }
                    Ok(StreamingOutcome::Cancelled(transferred)) => {
                        let _ = events_tx.send(TransferEvent::Cancelled { transferred });
                    }
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                    }
                }
                cancels.lock().unwrap().remove(&id);
                pauses.lock().unwrap().remove(&id);
            });
        }
```

Currently (`DownloadDir` arm):

```rust
        SftpRequest::DownloadDir {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let items = match walk_remote_dir(&sftp, &remote, &local).await {
                    Ok(items) => items,
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        return;
                    }
                };
                run_download_dir(&sftp, items, &events_tx, &cancel).await;
                cancels.lock().unwrap().remove(&id);
            });
        }
```

Replace with:

```rust
        SftpRequest::DownloadDir {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels,
            paused,
            pauses,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            pauses.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let items = match walk_remote_dir(&sftp, &remote, &local).await {
                    Ok(items) => items,
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        pauses.lock().unwrap().remove(&id);
                        return;
                    }
                };
                run_download_dir(&sftp, items, &events_tx, &cancel, &paused).await;
                cancels.lock().unwrap().remove(&id);
                pauses.lock().unwrap().remove(&id);
            });
        }
```

Currently (`UploadDir` arm):

```rust
        SftpRequest::UploadDir {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let items = match walk_local_dir(&sftp, &local, &remote).await {
                    Ok(items) => items,
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        return;
                    }
                };
                run_upload_dir(&sftp, items, &events_tx, &cancel).await;
                cancels.lock().unwrap().remove(&id);
            });
        }
```

Replace with:

```rust
        SftpRequest::UploadDir {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels,
            paused,
            pauses,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            pauses.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let items = match walk_local_dir(&sftp, &local, &remote).await {
                    Ok(items) => items,
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        pauses.lock().unwrap().remove(&id);
                        return;
                    }
                };
                run_upload_dir(&sftp, items, &events_tx, &cancel, &paused).await;
                cancels.lock().unwrap().remove(&id);
                pauses.lock().unwrap().remove(&id);
            });
        }
```

(`reply_sftp_error`'s combined match arm `SftpRequest::Download { reply, id, .. } | SftpRequest::Upload { reply, id, .. } | SftpRequest::DownloadDir { reply, id, .. } | SftpRequest::UploadDir { reply, id, .. } => { ... }` uses `..` to ignore every other field — it needs no edit; the new `paused`/`pauses` fields are automatically covered by `..`.)

- [ ] **Step 7: Add `paused: &AtomicBool` to the four streaming functions**

Currently (`sftp_download_streaming`):

```rust
async fn sftp_download_streaming(
    sftp: &SftpSession,
    remote: &str,
    local: &PathBuf,
    _total: u64,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    const PROGRESS_INTERVAL: u64 = 64 * 1024;

    let mut remote_file = sftp
        .open(remote)
        .await
        .map_err(|e| anyhow!("open {remote:?}: {e}"))?;
    let mut local_file = tokio::fs::File::create(local).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    let mut last_progress_at: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = remote_file
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("read {remote:?}: {e}"))?;
        if n == 0 {
            break;
        }
        local_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {local:?}: {e}"))?;
        transferred += n as u64;
        if transferred - last_progress_at >= PROGRESS_INTERVAL {
            let _ = events.send(TransferEvent::Progress { transferred });
            last_progress_at = transferred;
        }
    }
    local_file.flush().await?;
    let _ = events.send(TransferEvent::Progress { transferred });
    Ok(StreamingOutcome::Completed(transferred))
}
```

Replace with:

```rust
async fn sftp_download_streaming(
    sftp: &SftpSession,
    remote: &str,
    local: &PathBuf,
    _total: u64,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
    paused: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    const PROGRESS_INTERVAL: u64 = 64 * 1024;

    if wait_while_paused(paused, cancel).await {
        return Ok(StreamingOutcome::Cancelled(0));
    }
    let mut remote_file = sftp
        .open(remote)
        .await
        .map_err(|e| anyhow!("open {remote:?}: {e}"))?;
    let mut local_file = tokio::fs::File::create(local).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    let mut last_progress_at: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        if wait_while_paused(paused, cancel).await {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = remote_file
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("read {remote:?}: {e}"))?;
        if n == 0 {
            break;
        }
        local_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {local:?}: {e}"))?;
        transferred += n as u64;
        if transferred - last_progress_at >= PROGRESS_INTERVAL {
            let _ = events.send(TransferEvent::Progress { transferred });
            last_progress_at = transferred;
        }
    }
    local_file.flush().await?;
    let _ = events.send(TransferEvent::Progress { transferred });
    Ok(StreamingOutcome::Completed(transferred))
}
```

Currently (`sftp_upload_streaming`):

```rust
async fn sftp_upload_streaming(
    sftp: &SftpSession,
    local: &PathBuf,
    remote: &str,
    _total: u64,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
) -> Result<StreamingOutcome> {
    use tokio::io::AsyncReadExt;

    const CHUNK: usize = 32 * 1024;
    const PROGRESS_INTERVAL: u64 = 64 * 1024;

    let mut local_file = tokio::fs::File::open(local).await?;
    let mut remote_file = sftp
        .open_with_flags(
            remote,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
        )
        .await
        .map_err(|e| anyhow!("create {remote:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    let mut last_progress_at: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = local_file
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("read {local:?}: {e}"))?;
        if n == 0 {
            break;
        }
        remote_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {remote:?}: {e}"))?;
        transferred += n as u64;
        if transferred - last_progress_at >= PROGRESS_INTERVAL {
            let _ = events.send(TransferEvent::Progress { transferred });
            last_progress_at = transferred;
        }
    }
    remote_file
        .shutdown()
        .await
        .map_err(|e| anyhow!("close {remote:?}: {e}"))?;
    let _ = events.send(TransferEvent::Progress { transferred });
    Ok(StreamingOutcome::Completed(transferred))
}
```

Replace with:

```rust
async fn sftp_upload_streaming(
    sftp: &SftpSession,
    local: &PathBuf,
    remote: &str,
    _total: u64,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
    paused: &AtomicBool,
) -> Result<StreamingOutcome> {
    use tokio::io::AsyncReadExt;

    const CHUNK: usize = 32 * 1024;
    const PROGRESS_INTERVAL: u64 = 64 * 1024;

    if wait_while_paused(paused, cancel).await {
        return Ok(StreamingOutcome::Cancelled(0));
    }
    let mut local_file = tokio::fs::File::open(local).await?;
    let mut remote_file = sftp
        .open_with_flags(
            remote,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
        )
        .await
        .map_err(|e| anyhow!("create {remote:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    let mut last_progress_at: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        if wait_while_paused(paused, cancel).await {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = local_file
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("read {local:?}: {e}"))?;
        if n == 0 {
            break;
        }
        remote_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {remote:?}: {e}"))?;
        transferred += n as u64;
        if transferred - last_progress_at >= PROGRESS_INTERVAL {
            let _ = events.send(TransferEvent::Progress { transferred });
            last_progress_at = transferred;
        }
    }
    remote_file
        .shutdown()
        .await
        .map_err(|e| anyhow!("close {remote:?}: {e}"))?;
    let _ = events.send(TransferEvent::Progress { transferred });
    Ok(StreamingOutcome::Completed(transferred))
}
```

- [ ] **Step 8: Add `paused: &AtomicBool` to `download_one_file`/`upload_one_file` and their callers `run_download_dir`/`run_upload_dir`**

Currently (`download_one_file`):

```rust
async fn download_one_file(
    sftp: &SftpSession,
    remote: &str,
    local: &PathBuf,
    cancel: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    if let Some(parent) = local.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let mut remote_file = sftp.open(remote).await.map_err(|e| anyhow!("open {remote:?}: {e}"))?;
    let mut local_file = tokio::fs::File::create(local)
        .await
        .map_err(|e| anyhow!("create {local:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = remote_file.read(&mut buf).await.map_err(|e| anyhow!("read {remote:?}: {e}"))?;
        if n == 0 {
            break;
        }
        local_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {local:?}: {e}"))?;
        transferred += n as u64;
    }
    local_file.flush().await.map_err(|e| anyhow!("flush {local:?}: {e}"))?;
    Ok(StreamingOutcome::Completed(transferred))
}
```

Replace with:

```rust
async fn download_one_file(
    sftp: &SftpSession,
    remote: &str,
    local: &PathBuf,
    cancel: &AtomicBool,
    paused: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    // Checked before touching the filesystem at all, not just inside the
    // read/write loop below — otherwise pausing right as a directory job
    // moves to its next file would still create that file's empty local
    // placeholder immediately, even though nothing should happen until
    // resumed.
    if wait_while_paused(paused, cancel).await {
        return Ok(StreamingOutcome::Cancelled(0));
    }
    if let Some(parent) = local.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let mut remote_file = sftp.open(remote).await.map_err(|e| anyhow!("open {remote:?}: {e}"))?;
    let mut local_file = tokio::fs::File::create(local)
        .await
        .map_err(|e| anyhow!("create {local:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        if wait_while_paused(paused, cancel).await {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = remote_file.read(&mut buf).await.map_err(|e| anyhow!("read {remote:?}: {e}"))?;
        if n == 0 {
            break;
        }
        local_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {local:?}: {e}"))?;
        transferred += n as u64;
    }
    local_file.flush().await.map_err(|e| anyhow!("flush {local:?}: {e}"))?;
    Ok(StreamingOutcome::Completed(transferred))
}
```

Currently (`upload_one_file`):

```rust
async fn upload_one_file(
    sftp: &SftpSession,
    local: &PathBuf,
    remote: &str,
    cancel: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    let mut local_file = tokio::fs::File::open(local).await.map_err(|e| anyhow!("open {local:?}: {e}"))?;
    let mut remote_file = sftp
        .open_with_flags(remote, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE)
        .await
        .map_err(|e| anyhow!("create {remote:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = local_file.read(&mut buf).await.map_err(|e| anyhow!("read {local:?}: {e}"))?;
        if n == 0 {
            break;
        }
        remote_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {remote:?}: {e}"))?;
        transferred += n as u64;
    }
    remote_file.shutdown().await.map_err(|e| anyhow!("close {remote:?}: {e}"))?;
    Ok(StreamingOutcome::Completed(transferred))
}
```

Replace with:

```rust
async fn upload_one_file(
    sftp: &SftpSession,
    local: &PathBuf,
    remote: &str,
    cancel: &AtomicBool,
    paused: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    if wait_while_paused(paused, cancel).await {
        return Ok(StreamingOutcome::Cancelled(0));
    }
    let mut local_file = tokio::fs::File::open(local).await.map_err(|e| anyhow!("open {local:?}: {e}"))?;
    let mut remote_file = sftp
        .open_with_flags(remote, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE)
        .await
        .map_err(|e| anyhow!("create {remote:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        if wait_while_paused(paused, cancel).await {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = local_file.read(&mut buf).await.map_err(|e| anyhow!("read {local:?}: {e}"))?;
        if n == 0 {
            break;
        }
        remote_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {remote:?}: {e}"))?;
        transferred += n as u64;
    }
    remote_file.shutdown().await.map_err(|e| anyhow!("close {remote:?}: {e}"))?;
    Ok(StreamingOutcome::Completed(transferred))
}
```

Currently (`run_download_dir`):

```rust
async fn run_download_dir(
    sftp: &SftpSession,
    items: Vec<DirTransferItem>,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
) {
    let total: u64 = items.iter().map(|i| i.size).sum();
    let _ = events.send(TransferEvent::Started { total });

    let mut transferred: u64 = 0;
    let mut failed_paths = Vec::new();
    let mut cancelled = false;

    for item in items {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        match download_one_file(sftp, &item.remote, &item.local, cancel).await {
            Ok(StreamingOutcome::Completed(n)) => {
                transferred += n;
                let _ = events.send(TransferEvent::Progress { transferred });
            }
            Ok(StreamingOutcome::Cancelled(n)) => {
                transferred += n;
                cancelled = true;
                break;
            }
            Err(e) => {
                log::warn!("sftp: download {:?} failed: {e:#}", item.remote);
                failed_paths.push(item.remote);
            }
        }
    }

    if cancelled {
        let _ = events.send(TransferEvent::Cancelled { transferred });
    } else if failed_paths.is_empty() {
        let _ = events.send(TransferEvent::Done { transferred });
    } else {
        let _ = events.send(TransferEvent::DoneWithFailures { transferred, failed_paths });
    }
}
```

Replace with:

```rust
async fn run_download_dir(
    sftp: &SftpSession,
    items: Vec<DirTransferItem>,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
    paused: &AtomicBool,
) {
    let total: u64 = items.iter().map(|i| i.size).sum();
    let _ = events.send(TransferEvent::Started { total });

    let mut transferred: u64 = 0;
    let mut failed_paths = Vec::new();
    let mut cancelled = false;

    for item in items {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        match download_one_file(sftp, &item.remote, &item.local, cancel, paused).await {
            Ok(StreamingOutcome::Completed(n)) => {
                transferred += n;
                let _ = events.send(TransferEvent::Progress { transferred });
            }
            Ok(StreamingOutcome::Cancelled(n)) => {
                transferred += n;
                cancelled = true;
                break;
            }
            Err(e) => {
                log::warn!("sftp: download {:?} failed: {e:#}", item.remote);
                failed_paths.push(item.remote);
            }
        }
    }

    if cancelled {
        let _ = events.send(TransferEvent::Cancelled { transferred });
    } else if failed_paths.is_empty() {
        let _ = events.send(TransferEvent::Done { transferred });
    } else {
        let _ = events.send(TransferEvent::DoneWithFailures { transferred, failed_paths });
    }
}
```

For `run_upload_dir` (mirrors `run_download_dir` exactly, uploading instead of downloading — find it immediately after `run_download_dir`), apply the identical shape of change: add a `paused: &AtomicBool` parameter to its signature, and pass `paused` as the extra argument to its `upload_one_file(sftp, &item.local, &item.remote, cancel, paused)` call.

- [ ] **Step 9: Update the two remaining call sites for `run_download_dir`/`run_upload_dir`**

These are the calls already rewritten inside Step 6's `DownloadDir`/`UploadDir` arms above (`run_download_dir(&sftp, items, &events_tx, &cancel, &paused).await;` / `run_upload_dir(&sftp, items, &events_tx, &cancel, &paused).await;`) — already correct from Step 6, nothing further to do here. This step is a checkpoint: re-read both arms and confirm both calls now pass `&paused` as their fifth argument.

- [ ] **Step 10: Build and test**

Run: `cargo build`
Expected: clean build, no errors. (A `cargo build` at this point may still show pre-existing warnings unrelated to this change — e.g. the `proc-macro-error2` future-incompat note — but no new warnings from `ssh.rs` itself, since every new field/parameter is consumed somewhere in this same task's edits.)

Run: `cargo test --locked`
Expected: all existing tests still pass (this task adds no new tests, per this task's own header).

- [ ] **Step 11: Commit**

```bash
git add src/terminal/ssh.rs
git commit -m "feat: add SFTP transfer pause/resume plumbing to the SSH backend"
```

---

### Task 2: Panel data model — Paused status, speed accounting, pause/resume/remove methods

**Files:**
- Modify: `src/panels/sftp.rs`

**Interfaces:**
- Consumes: `SshSession::sftp_pause(&self, id: u64) -> bool`, `SshSession::sftp_resume(&self, id: u64) -> bool` (Task 1).
- Produces: `TransferStatus::Paused` variant; `SftpPanel::pause_transfer(&mut self, id: u64, cx: &mut Context<Self>)`, `SftpPanel::resume_transfer(&mut self, id: u64, cx: &mut Context<Self>)`, `SftpPanel::remove_transfer(&mut self, id: u64, cx: &mut Context<Self>)`. Consumed by Task 4's row rendering.

- [ ] **Step 1: Add `Duration` to the file's time imports**

In `src/panels/sftp.rs`, change:

```rust
use std::time::{Instant, UNIX_EPOCH};
```

to:

```rust
use std::time::{Duration, Instant, UNIX_EPOCH};
```

- [ ] **Step 2: Add `TransferStatus::Paused`**

Change:

```rust
enum TransferStatus {
    Queued,
    Active,
    Done,
    /// A directory transfer completed but skipped one or more files
    /// (`TransferEvent::DoneWithFailures`) — carries their remote paths for
    /// display. Never constructed for single-file transfers.
    DoneWithFailures(Vec<String>),
    Failed(String),
    Cancelled,
}
```

to:

```rust
enum TransferStatus {
    Queued,
    Active,
    /// User-paused: the streaming loop stopped doing I/O but its file
    /// handles are still open (see `SshSession::sftp_pause`). Set locally
    /// by `pause_transfer`/cleared by `resume_transfer` — no backend
    /// round-trip event exists for this transition.
    Paused,
    Done,
    /// A directory transfer completed but skipped one or more files
    /// (`TransferEvent::DoneWithFailures`) — carries their remote paths for
    /// display. Never constructed for single-file transfers.
    DoneWithFailures(Vec<String>),
    Failed(String),
    Cancelled,
}
```

- [ ] **Step 3: Add `paused_duration`/`paused_since` to `Transfer` and update `speed_bytes_per_sec`**

Change:

```rust
/// One row in the transfer list (the bottom section of the panel).
struct Transfer {
    id: u64,
    name: String,
    direction: TransferDirection,
    total: u64,
    transferred: u64,
    status: TransferStatus,
    started_at: Instant,
    /// The local-disk path of this transfer (download destination or upload
    /// source) — powers the completed-transfer context menu's "open
    /// file"/"open folder"/"delete"/"properties" actions, all of which are
    /// OS-level operations on the local copy regardless of transfer
    /// direction. Empty until known: for uploads, the path isn't picked
    /// until the async file-picker dialog resolves.
    local_path: PathBuf,
}

impl Transfer {
    /// 0.0..=1.0 — safe when `total == 0` (treats as fully done if status
    /// says so).
    fn progress(&self) -> f32 {
        if self.total == 0 {
            match self.status {
                TransferStatus::Done | TransferStatus::DoneWithFailures(_) => 1.0,
                _ => 0.0,
            }
        } else {
            (self.transferred as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// Average transfer rate since the transfer started, in bytes/sec.
    /// Averaged over the whole elapsed duration (not a rolling window) —
    /// simple, needs no extra tracked state, and re-derives itself from
    /// `transferred`/`started_at` on every render as new progress events
    /// arrive, so it stays live without a separate poll loop. `elapsed` is
    /// floored at 50ms so a transfer whose first render already has a
    /// large `transferred` (a fast local disk, or a big first chunk
    /// arriving before much wall-clock time has passed) doesn't produce a
    /// spuriously huge rate from dividing by a near-zero duration.
    fn speed_bytes_per_sec(&self) -> f64 {
        let elapsed = self.started_at.elapsed().as_secs_f64().max(0.05);
        self.transferred as f64 / elapsed
    }
}
```

to:

```rust
/// One row in the transfer list (the bottom section of the panel).
struct Transfer {
    id: u64,
    name: String,
    direction: TransferDirection,
    total: u64,
    transferred: u64,
    status: TransferStatus,
    started_at: Instant,
    /// Cumulative time this transfer has spent `Paused`, excluding any
    /// currently-ongoing pause (see `paused_since`) — added there instead.
    /// Kept out of `speed_bytes_per_sec`'s elapsed-time denominator so a
    /// paused transfer's average speed doesn't permanently understate
    /// itself after resuming.
    paused_duration: Duration,
    /// `Some(when this pause began)` while `status == Paused`, `None`
    /// otherwise. `resume_transfer` folds the elapsed time into
    /// `paused_duration` and clears this back to `None`.
    paused_since: Option<Instant>,
    /// The local-disk path of this transfer (download destination or upload
    /// source) — powers the completed-transfer context menu's "open
    /// file"/"open folder"/"delete"/"properties" actions, all of which are
    /// OS-level operations on the local copy regardless of transfer
    /// direction. Empty until known: for uploads, the path isn't picked
    /// until the async file-picker dialog resolves.
    local_path: PathBuf,
}

impl Transfer {
    /// 0.0..=1.0 — safe when `total == 0` (treats as fully done if status
    /// says so).
    fn progress(&self) -> f32 {
        if self.total == 0 {
            match self.status {
                TransferStatus::Done | TransferStatus::DoneWithFailures(_) => 1.0,
                _ => 0.0,
            }
        } else {
            (self.transferred as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// Average transfer rate since the transfer started, in bytes/sec,
    /// excluding any time spent `Paused` (both completed pauses, in
    /// `paused_duration`, and a currently-ongoing one via `paused_since`).
    /// Averaged over the whole (pause-adjusted) elapsed duration — simple,
    /// needs no extra polling, and re-derives itself from
    /// `transferred`/`started_at` on every render as new progress events
    /// arrive. `elapsed` is floored at 50ms so a transfer whose first
    /// render already has a large `transferred` (a fast local disk, or a
    /// big first chunk arriving before much wall-clock time has passed)
    /// doesn't produce a spuriously huge rate from dividing by a
    /// near-zero duration.
    fn speed_bytes_per_sec(&self) -> f64 {
        let mut paused = self.paused_duration;
        if let Some(since) = self.paused_since {
            paused += since.elapsed();
        }
        let elapsed = self
            .started_at
            .elapsed()
            .saturating_sub(paused)
            .as_secs_f64()
            .max(0.05);
        self.transferred as f64 / elapsed
    }
}
```

- [ ] **Step 4: Add `paused_duration`/`paused_since` to every `Transfer { ... }` construction site**

Four sites in `src/panels/sftp.rs`, each currently shaped like:

```rust
        self.transfers.push(Transfer {
            id: 0,
            name: display_name.clone(),
            direction: TransferDirection::Download,
            total: 0,
            transferred: 0,
            status: TransferStatus::Queued,
            started_at: Instant::now(),
            local_path: local.clone(),
        });
```

(the other three differ only in `name`/`direction`/`local_path` — one in the `download` method around line 576, one in `download_dir_entry` around line 662, two in the two upload-initiating methods around lines 719 and 806, both with `name: "(selecting…)".into()` and `local_path: PathBuf::new()`).

In all four, insert two lines right after `started_at: Instant::now(),` and before `local_path: ...,`:

```rust
            paused_duration: Duration::ZERO,
            paused_since: None,
```

- [ ] **Step 5: Add `paused_duration`/`paused_since` to the test helper `transfer_with`**

In the `transfer_progress_tests` module, change:

```rust
    fn transfer_with(status: TransferStatus, total: u64, transferred: u64) -> Transfer {
        Transfer {
            id: 1,
            name: "test".to_string(),
            direction: TransferDirection::Download,
            total,
            transferred,
            status,
            started_at: Instant::now(),
            local_path: PathBuf::new(),
        }
    }
```

to:

```rust
    fn transfer_with(status: TransferStatus, total: u64, transferred: u64) -> Transfer {
        Transfer {
            id: 1,
            name: "test".to_string(),
            direction: TransferDirection::Download,
            total,
            transferred,
            status,
            started_at: Instant::now(),
            paused_duration: Duration::ZERO,
            paused_since: None,
            local_path: PathBuf::new(),
        }
    }
```

- [ ] **Step 6: Write the failing tests**

Add to the `transfer_progress_tests` module, right after `speed_bytes_per_sec_floors_elapsed_to_avoid_a_spike_at_transfer_start`:

```rust
    #[test]
    fn speed_bytes_per_sec_excludes_completed_paused_duration() {
        let mut t = transfer_with(TransferStatus::Paused, 1_000_000, 500_000);
        // Started 3s ago, but 1s of that was spent paused — only 2s should
        // count, same expected rate as the "computes_average_rate" test.
        t.started_at = Instant::now() - Duration::from_secs(3);
        t.paused_duration = Duration::from_secs(1);
        let speed = t.speed_bytes_per_sec();
        assert!((speed - 250_000.0).abs() < 5_000.0, "expected ~250000 B/s, got {speed}");
    }

    #[test]
    fn speed_bytes_per_sec_excludes_an_ongoing_pause() {
        let mut t = transfer_with(TransferStatus::Paused, 1_000_000, 500_000);
        // Started 3s ago; still paused, and became paused 1s ago — same
        // pause-adjusted 2s of real transfer time as the test above.
        t.started_at = Instant::now() - Duration::from_secs(3);
        t.paused_since = Some(Instant::now() - Duration::from_secs(1));
        let speed = t.speed_bytes_per_sec();
        assert!((speed - 250_000.0).abs() < 5_000.0, "expected ~250000 B/s, got {speed}");
    }
```

- [ ] **Step 7: Run tests to verify they fail**

Run: `cargo test --locked speed_bytes_per_sec_excludes -- --test-threads=1`
Expected: FAIL — `speed_bytes_per_sec_excludes_completed_paused_duration` and `speed_bytes_per_sec_excludes_an_ongoing_pause` both fail (they'll compute the *unadjusted* rate, ~166,666 B/s, not ~250,000 B/s), because Step 3's implementation isn't fixed yet if you're doing TDD strictly — since Step 3 above is written before the tests in this plan for readability, in practice apply Step 3's code change and Steps 6's tests together, then run this verification. If Step 3 was already applied, skip straight to Step 8's "verify passes" run and confirm PASS instead.

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --locked --lib panels::sftp -- --test-threads=1`
Expected: PASS for all of `transfer_progress_tests::*`, including the two new tests.

- [ ] **Step 9: Widen `is_running`'s definition wherever it's checked for row grouping, and `clear_completed_transfers`**

This task only touches the non-rendering `clear_completed_transfers` method and its test (the `is_running`/`is_finished` local bindings inside `render_transfer_body` belong to Task 4). Change:

```rust
    /// Drops every transfer whose status is terminal (`Done`,
    /// `DoneWithFailures`, `Failed`, or `Cancelled`) — success, partial
    /// failure, hard failure, and user-cancelled all count as "completed"
    /// for clearing purposes. In-flight transfers (`Queued`/`Active`) are
    /// untouched.
    fn clear_completed_transfers(&mut self, cx: &mut Context<Self>) {
        self.transfers
            .retain(|t| matches!(t.status, TransferStatus::Queued | TransferStatus::Active));
        cx.notify();
    }
```

to:

```rust
    /// Drops every transfer whose status is terminal (`Done`,
    /// `DoneWithFailures`, `Failed`, or `Cancelled`) — success, partial
    /// failure, hard failure, and user-cancelled all count as "completed"
    /// for clearing purposes. In-flight transfers (`Queued`/`Active`/
    /// `Paused`) are untouched — a paused transfer is still in flight, not
    /// completed.
    fn clear_completed_transfers(&mut self, cx: &mut Context<Self>) {
        self.transfers.retain(|t| {
            matches!(
                t.status,
                TransferStatus::Queued | TransferStatus::Active | TransferStatus::Paused
            )
        });
        cx.notify();
    }
```

And update its existing test — change:

```rust
    #[test]
    fn clear_completed_transfers_keeps_only_in_flight_rows() {
        let mut transfers = vec![
            transfer_with(TransferStatus::Queued, 100, 0),
            transfer_with(TransferStatus::Active, 100, 50),
            transfer_with(TransferStatus::Done, 100, 100),
            transfer_with(TransferStatus::DoneWithFailures(vec!["a".into()]), 100, 90),
            transfer_with(TransferStatus::Failed("boom".into()), 100, 0),
            transfer_with(TransferStatus::Cancelled, 100, 10),
        ];
        transfers.retain(|t| matches!(t.status, TransferStatus::Queued | TransferStatus::Active));
        assert_eq!(transfers.len(), 2);
    }
```

to:

```rust
    #[test]
    fn clear_completed_transfers_keeps_only_in_flight_rows() {
        let mut transfers = vec![
            transfer_with(TransferStatus::Queued, 100, 0),
            transfer_with(TransferStatus::Active, 100, 50),
            transfer_with(TransferStatus::Paused, 100, 50),
            transfer_with(TransferStatus::Done, 100, 100),
            transfer_with(TransferStatus::DoneWithFailures(vec!["a".into()]), 100, 90),
            transfer_with(TransferStatus::Failed("boom".into()), 100, 0),
            transfer_with(TransferStatus::Cancelled, 100, 10),
        ];
        transfers.retain(|t| {
            matches!(
                t.status,
                TransferStatus::Queued | TransferStatus::Active | TransferStatus::Paused
            )
        });
        assert_eq!(transfers.len(), 3);
    }
```

- [ ] **Step 10: Run the updated test**

Run: `cargo test --locked clear_completed_transfers_keeps_only_in_flight_rows`
Expected: PASS.

- [ ] **Step 11: Add `pause_transfer`/`resume_transfer`/`remove_transfer` methods**

Immediately after `cancel_transfer` (currently):

```rust
    fn cancel_transfer(&self, id: u64) {
        self.session.sftp_cancel(id);
    }
```

insert:

```rust
    fn cancel_transfer(&self, id: u64) {
        self.session.sftp_cancel(id);
    }

    /// Pauses an Active transfer: flips its status locally and tells the
    /// backend to stop doing I/O on that transfer id (see
    /// `SshSession::sftp_pause`). No-op if the transfer isn't currently
    /// Active (e.g. already Paused, or already finished).
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

    /// Resumes a Paused transfer: folds the just-ended pause into
    /// `paused_duration`, flips status back to Active, and tells the
    /// backend to resume I/O on that transfer id.
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

    /// Removes a transfer row from the list without touching any local
    /// file — pure list bookkeeping, no confirmation (matches
    /// `clear_completed_transfers`'s existing no-confirm convention).
    fn remove_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        self.transfers.retain(|t| t.id != id);
        cx.notify();
    }
```

- [ ] **Step 12: Build and test**

Run: `cargo build`
Expected: clean build. `pause_transfer`/`resume_transfer`/`remove_transfer` will show `dead_code` warnings at this point — expected, Task 4 wires them into the UI.

Run: `cargo test --locked --lib panels::sftp`
Expected: all pass, including the new and updated tests from Steps 6/9.

- [ ] **Step 13: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: add Paused transfer status, pause-adjusted speed, and pause/resume/remove methods"
```

---

### Task 3: Locale keys

**Files:**
- Modify: `locales/app.yml`

**Interfaces:**
- Produces: `Sftp.transfer_paused`, `Sftp.pause`, `Sftp.resume`, `Sftp.remove_transfer`, `Sftp.delete_local_file` — all consumed by Task 4.

- [ ] **Step 1: Insert the new keys**

In `locales/app.yml`, find the `Sftp:` section's `cancel_transfer_tooltip` key (immediately before `download_to_prefix`):

```yaml
  cancel_transfer_tooltip:
    zh-CN: "取消传输"
    en: "Cancel Transfer"
  download_to_prefix:
    zh-CN: "下载到:"
    en: "Download to:"
```

Insert five new keys between them:

```yaml
  cancel_transfer_tooltip:
    zh-CN: "取消传输"
    en: "Cancel Transfer"
  transfer_paused:
    zh-CN: "已暂停"
    en: "Paused"
  pause:
    zh-CN: "暂停"
    en: "Pause"
  resume:
    zh-CN: "继续"
    en: "Resume"
  remove_transfer:
    zh-CN: "删除"
    en: "Delete"
  delete_local_file:
    zh-CN: "删除本地文件"
    en: "Delete Local File"
  download_to_prefix:
    zh-CN: "下载到:"
    en: "Download to:"
```

(`Sftp.delete` — the existing generic key used by the file browser's own remote-file-delete action — is untouched; `delete_local_file` is a new, separate key so the transfer list's local-file-delete action reads distinctly from it.)

- [ ] **Step 2: Verify the file still parses**

Run: `cargo build` (rust-i18n's `i18n!()` macro parses `locales/*.yml` at compile time)
Expected: builds cleanly.

- [ ] **Step 3: Commit**

```bash
git add locales/app.yml
git commit -m "feat: add locale keys for SFTP transfer pause/resume/delete"
```

---

### Task 4: Panel UI — context menu, status text, bar color

**Files:**
- Modify: `src/panels/sftp.rs`

**Interfaces:**
- Consumes: `TransferStatus::Paused` (Task 2), `SftpPanel::pause_transfer`/`resume_transfer`/`remove_transfer` (Task 2), `Sftp.transfer_paused`/`pause`/`resume`/`remove_transfer`/`delete_local_file` locale keys (Task 3).
- Produces: nothing consumed by a later task — this is the last code-writing task.

No dedicated unit tests — this is gpui render code, needing a live window; matches this file's/this codebase's existing zero-test convention for render functions (`render_transfer_body` itself has never had tests). Verified by `cargo build` + Task 5's manual smoke test.

- [ ] **Step 1: Add the Paused status-text arm**

In `render_transfer_body`, change:

```rust
                let status_text = match &t.status {
                    TransferStatus::Queued => rust_i18n::t!("Sftp.transfer_queued").to_string(),
                    TransferStatus::Active => format!(
                        "{} / {} · {}",
                        human_size(t.transferred),
                        human_size(t.total),
                        human_speed(t.speed_bytes_per_sec())
                    ),
                    TransferStatus::Done => {
                        rust_i18n::t!("Sftp.transfer_done", size = human_size(t.transferred)).to_string()
                    }
                    TransferStatus::DoneWithFailures(failed) => rust_i18n::t!(
                        "Sftp.transfer_done_with_failures",
                        size = human_size(t.transferred),
                        count = failed.len()
                    )
                    .to_string(),
                    TransferStatus::Failed(e) => rust_i18n::t!("Sftp.transfer_failed", error = e).to_string(),
                    TransferStatus::Cancelled => rust_i18n::t!("Sftp.transfer_cancelled").to_string(),
                };
```

to:

```rust
                let status_text = match &t.status {
                    TransferStatus::Queued => rust_i18n::t!("Sftp.transfer_queued").to_string(),
                    TransferStatus::Active => format!(
                        "{} / {} · {}",
                        human_size(t.transferred),
                        human_size(t.total),
                        human_speed(t.speed_bytes_per_sec())
                    ),
                    TransferStatus::Paused => format!(
                        "{} / {} · {}",
                        human_size(t.transferred),
                        human_size(t.total),
                        rust_i18n::t!("Sftp.transfer_paused")
                    ),
                    TransferStatus::Done => {
                        rust_i18n::t!("Sftp.transfer_done", size = human_size(t.transferred)).to_string()
                    }
                    TransferStatus::DoneWithFailures(failed) => rust_i18n::t!(
                        "Sftp.transfer_done_with_failures",
                        size = human_size(t.transferred),
                        count = failed.len()
                    )
                    .to_string(),
                    TransferStatus::Failed(e) => rust_i18n::t!("Sftp.transfer_failed", error = e).to_string(),
                    TransferStatus::Cancelled => rust_i18n::t!("Sftp.transfer_cancelled").to_string(),
                };
```

- [ ] **Step 2: Add the Paused bar color, and widen `is_running`**

Change:

```rust
                let progress = t.progress();
                let bar_color = match t.status {
                    TransferStatus::Failed(_) => cx.theme().danger,
                    TransferStatus::Done => cx.theme().success,
                    TransferStatus::DoneWithFailures(_) => cx.theme().warning,
                    TransferStatus::Cancelled => cx.theme().muted_foreground,
                    _ => cx.theme().primary,
                };
                let is_running = matches!(
                    t.status,
                    TransferStatus::Queued | TransferStatus::Active
                );
                let is_done = matches!(
                    t.status,
                    TransferStatus::Done | TransferStatus::DoneWithFailures(_)
                );
```

to:

```rust
                let progress = t.progress();
                let bar_color = match t.status {
                    TransferStatus::Failed(_) => cx.theme().danger,
                    TransferStatus::Done => cx.theme().success,
                    TransferStatus::DoneWithFailures(_) => cx.theme().warning,
                    TransferStatus::Cancelled => cx.theme().muted_foreground,
                    TransferStatus::Paused => cx.theme().warning,
                    _ => cx.theme().primary,
                };
                // Every `TransferStatus` variant is either still in flight
                // (`is_running`) or in a terminal state (`is_finished`) —
                // these two groupings are exhaustive and mutually exclusive.
                let is_running = matches!(
                    t.status,
                    TransferStatus::Queued | TransferStatus::Active | TransferStatus::Paused
                );
                let is_done = matches!(
                    t.status,
                    TransferStatus::Done | TransferStatus::DoneWithFailures(_)
                );
                let is_finished = matches!(
                    t.status,
                    TransferStatus::Done
                        | TransferStatus::DoneWithFailures(_)
                        | TransferStatus::Failed(_)
                        | TransferStatus::Cancelled
                );
```

(`is_done` stays as-is — it still distinguishes "has a local file worth opening" from `is_finished`'s broader "has a delete menu", used in the next step.)

- [ ] **Step 3: Replace the `if is_done { ... } else { ... }` context-menu attachment**

Change:

```rust
                if is_done {
                    let panel_open = weak_panel.clone();
                    let panel_open_folder = weak_panel.clone();
                    let panel_properties = weak_panel.clone();
                    let panel_delete = weak_panel.clone();
                    row.context_menu(move |menu, _window, _cx| {
                        // `context_menu`'s builder is an `Fn` (invoked on every render of
                        // the menu), so it must not move its captures — clone into locals
                        // per invocation and let the `on_click` closures move those instead
                        // (same reasoning as `delete_selected`'s `open_alert_dialog` builder).
                        let panel_open = panel_open.clone();
                        let panel_open_folder = panel_open_folder.clone();
                        let panel_properties = panel_properties.clone();
                        let panel_delete = panel_delete.clone();
                        menu.item(PopupMenuItem::new(rust_i18n::t!("Sftp.open_file")).on_click(
                            move |_ev, window, cx| {
                                let _ = panel_open.update(cx, |panel, cx| {
                                    panel.open_transfer_file(transfer_id, window, cx)
                                });
                            },
                        ))
                        .item(PopupMenuItem::new(rust_i18n::t!("Sftp.open_folder")).on_click(
                            move |_ev, window, cx| {
                                let _ = panel_open_folder.update(cx, |panel, cx| {
                                    panel.open_transfer_folder(transfer_id, window, cx)
                                });
                            },
                        ))
                        .item(PopupMenuItem::new(rust_i18n::t!("Sftp.properties")).on_click(
                            move |_ev, window, cx| {
                                let _ = panel_properties.update(cx, |panel, cx| {
                                    panel.show_transfer_properties(transfer_id, window, cx)
                                });
                            },
                        ))
                        .item(PopupMenuItem::new(rust_i18n::t!("Sftp.delete")).on_click(
                            move |_ev, window, cx| {
                                let _ = panel_delete.update(cx, |panel, cx| {
                                    panel.delete_transfer_file(transfer_id, window, cx)
                                });
                            },
                        ))
                    })
                    .into_any_element()
                } else {
                    row.into_any_element()
                }
```

to:

```rust
                if is_finished {
                    // Every finished row (Done/DoneWithFailures/Failed/
                    // Cancelled) gets a menu; only Done/DoneWithFailures
                    // (`is_done`) additionally gets open-file/open-folder/
                    // properties — a Failed/Cancelled transfer's local file
                    // is likely partial or was never created.
                    let panel_open = weak_panel.clone();
                    let panel_open_folder = weak_panel.clone();
                    let panel_properties = weak_panel.clone();
                    let panel_remove = weak_panel.clone();
                    let panel_delete_file = weak_panel.clone();
                    row.context_menu(move |menu, _window, _cx| {
                        // `context_menu`'s builder is an `Fn` (invoked on every render of
                        // the menu), so it must not move its captures — clone into locals
                        // per invocation and let the `on_click` closures move those instead
                        // (same reasoning as `delete_selected`'s `open_alert_dialog` builder).
                        let panel_open = panel_open.clone();
                        let panel_open_folder = panel_open_folder.clone();
                        let panel_properties = panel_properties.clone();
                        let panel_remove = panel_remove.clone();
                        let panel_delete_file = panel_delete_file.clone();
                        let mut menu = menu;
                        if is_done {
                            menu = menu
                                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.open_file")).on_click(
                                    move |_ev, window, cx| {
                                        let _ = panel_open.update(cx, |panel, cx| {
                                            panel.open_transfer_file(transfer_id, window, cx)
                                        });
                                    },
                                ))
                                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.open_folder")).on_click(
                                    move |_ev, window, cx| {
                                        let _ = panel_open_folder.update(cx, |panel, cx| {
                                            panel.open_transfer_folder(transfer_id, window, cx)
                                        });
                                    },
                                ))
                                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.properties")).on_click(
                                    move |_ev, window, cx| {
                                        let _ = panel_properties.update(cx, |panel, cx| {
                                            panel.show_transfer_properties(transfer_id, window, cx)
                                        });
                                    },
                                ));
                        }
                        menu.item(PopupMenuItem::new(rust_i18n::t!("Sftp.remove_transfer")).on_click(
                            move |_ev, _window, cx| {
                                let _ = panel_remove.update(cx, |panel, cx| {
                                    panel.remove_transfer(transfer_id, cx)
                                });
                            },
                        ))
                        .item(PopupMenuItem::new(rust_i18n::t!("Sftp.delete_local_file")).on_click(
                            move |_ev, window, cx| {
                                let _ = panel_delete_file.update(cx, |panel, cx| {
                                    panel.delete_transfer_file(transfer_id, window, cx)
                                });
                            },
                        ))
                    })
                    .into_any_element()
                } else if is_running {
                    // Queued/Active/Paused. Pause only shown while Active;
                    // Resume only shown while Paused; Cancel always shown
                    // (matches the inline "✕" button's own `is_running`
                    // condition — this menu is additive to it, not a
                    // replacement).
                    let status_for_menu = t.status.clone();
                    let panel_pause = weak_panel.clone();
                    let panel_resume = weak_panel.clone();
                    let panel_cancel = weak_panel.clone();
                    row.context_menu(move |menu, _window, _cx| {
                        let panel_pause = panel_pause.clone();
                        let panel_resume = panel_resume.clone();
                        let panel_cancel = panel_cancel.clone();
                        let mut menu = menu;
                        if matches!(status_for_menu, TransferStatus::Active) {
                            menu = menu.item(PopupMenuItem::new(rust_i18n::t!("Sftp.pause")).on_click(
                                move |_ev, _window, cx| {
                                    let _ = panel_pause.update(cx, |panel, cx| {
                                        panel.pause_transfer(transfer_id, cx)
                                    });
                                },
                            ));
                        }
                        if matches!(status_for_menu, TransferStatus::Paused) {
                            menu = menu.item(PopupMenuItem::new(rust_i18n::t!("Sftp.resume")).on_click(
                                move |_ev, _window, cx| {
                                    let _ = panel_resume.update(cx, |panel, cx| {
                                        panel.resume_transfer(transfer_id, cx)
                                    });
                                },
                            ));
                        }
                        menu.item(
                            PopupMenuItem::new(rust_i18n::t!("Sftp.cancel_transfer_tooltip")).on_click(
                                move |_ev, _window, cx| {
                                    let _ = panel_cancel.update(cx, |panel, _cx| {
                                        panel.cancel_transfer(transfer_id)
                                    });
                                },
                            ),
                        )
                    })
                    .into_any_element()
                } else {
                    // Unreachable today (is_finished/is_running are
                    // exhaustive over every TransferStatus variant) — kept
                    // as a defensive fallback rather than a `match` that
                    // would panic if a future variant is added and this
                    // grouping isn't updated to match.
                    row.into_any_element()
                }
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: clean build, no warnings about unused `pause_transfer`/`resume_transfer`/`remove_transfer` (now wired in), no new warnings.

- [ ] **Step 5: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: right-click pause/resume/cancel/delete on SFTP transfer rows"
```

---

### Task 5: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full build + test suite**

Run: `cargo build && cargo test --locked`
Expected: clean build, zero new warnings, all tests pass.

- [ ] **Step 2: Manual smoke test** (ask the user to perform this — GUI/live-transfer verification is not something to screenshot-drive; see the project's own convention)

Confirm each of these (from the design spec's Testing section):
1. Pause an in-flight download or upload via right-click → "暂停": the progress bar freezes and turns amber, status text shows "已暂停" with no speed.
2. Resume it via right-click → "继续": it continues from the same byte count (never restarts from 0, never regresses), and the speed shown afterward is a sane figure, not artificially deflated by the paused interval.
3. Cancel a Paused transfer (right-click → "取消传输" or the inline "✕"): it still works.
4. Start a directory download/upload, pause it mid-file, confirm only that job's progress freezes (not a crash), then resume and confirm the whole job still completes.
5. Right-click a Failed or Cancelled row: confirm it now has a menu with "删除" (row-only, no confirm — the row just disappears) and "删除本地文件" (prompts, then deletes the local file if one exists and removes the row) — and no Open file/Open folder/Properties items.
6. Right-click a Done row: confirm it still has Open file/Open folder/Properties, plus the same "删除"/"删除本地文件" pair (relabeled from the single "删除" it had before).
7. The existing inline "✕" cancel button is still present and working on Queued/Active/Paused rows, unchanged.

- [ ] **Step 3: Report results to the user**

No commit for this task — it's verification only. If the manual smoke test surfaces a bug, fix it as a follow-up commit before considering the plan complete.
