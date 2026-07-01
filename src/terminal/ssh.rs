//! SSH session: one `russh` connection on a dedicated OS thread + current-thread
//! tokio runtime (CLAUDE.md §2 — the async world lives here; it talks to the rest
//! of Caracal only through `flume`).
//!
//! **One Session = one connection.** The shell (terminal) channel and the SFTP
//! subsystem channel are opened on the *same* `Handle` — SFTP does not dial a
//! second connection. A command loop on the session thread multiplexes: it
//! spawns the shell pump as a task and services SFTP requests inline.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use anyhow::{Result, anyhow};
use russh::client::{self, Handle, Msg};
use russh::keys::ssh_key::PublicKey;
use russh::{Channel, ChannelMsg, Disconnect};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::terminal::backend::PtyBackend;

/// Connection parameters. Phase 4 supports password auth only.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl SshConfig {
    /// Stable key identifying the connection, so the workspace can share one
    /// `SshSession` between the shell and SFTP tabs of the same host.
    pub fn key(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// A directory entry returned by SFTP `read_dir`.
#[derive(Clone, Debug)]
pub struct SftpEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds of last modification (0 if the server didn't return
    /// `ACMODTIME` — many SFTP servers do, but it's not guaranteed).
    pub mtime: u32,
    /// Unix permission bits, including the file-type mode (e.g. `0o100644`
    /// for a regular file, `0o040755` for a directory). 0 if the server
    /// didn't return `PERMISSIONS`.
    pub perms: u32,
}

/// Control messages to a shell channel's write side.
enum Ctrl {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

/// SFTP requests serviced by the session thread. Each carries a reply channel.
enum SftpRequest {
    ReadDir {
        path: String,
        reply: flume::Sender<Result<Vec<SftpEntry>>>,
    },
    Download {
        remote: String,
        local: PathBuf,
        reply: flume::Sender<TransferHandle>,
        /// Pre-allocated transfer id (so the caller can register UI state
        /// before the work even starts).
        id: u64,
    },
    Upload {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
    },
}

/// Direction of a transfer (used by the panel to render the right icon +
/// label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferDirection {
    Download,
    Upload,
}

/// Handle returned by `sftp_download` / `sftp_upload`. The `id` is allocated
/// synchronously on the session thread so the caller can register the
/// transfer in its UI immediately. The `events` receiver yields lifecycle +
/// progress updates as the background task runs.
pub struct TransferHandle {
    pub id: u64,
    pub events: flume::Receiver<TransferEvent>,
}

/// Lifecycle + progress events for a single transfer.
#[derive(Clone, Debug)]
pub enum TransferEvent {
    /// Emitted once when the transfer has sized itself (knows the total
    /// byte count). Uploads size themselves via `tokio::fs::metadata(local)`
    /// before opening the remote file; downloads use the remote file's
    /// `metadata().size`.
    Started {
        total: u64,
    },
    /// Emitted periodically as bytes flow. Caller updates a progress bar.
    Progress {
        transferred: u64,
    },
    /// Successful completion. `transferred` may equal `total` (Started) or
    /// be slightly less if `total` was unknown at start time.
    Done {
        transferred: u64,
    },
    /// Failure. The transfer task ends; no more events for this id.
    Failed {
        error: String,
    },
}

/// Commands to the session thread.
enum SessionCmd {
    OpenShell {
        cols: u16,
        rows: u16,
        bytes_tx: flume::Sender<Vec<u8>>,
        ctrl_rx: flume::Receiver<Ctrl>,
    },
    Sftp(SftpRequest),
}

/// russh client event handler. Accepts any host key for now (TOFU/known-hosts is
/// a follow-up); the signature pins the current russh key type.
struct ClientHandler;

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _server_public_key: &PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct SshSession {
    cmd_tx: flume::Sender<SessionCmd>,
    /// Monotonic id allocator for transfers (`sftp_download` / `sftp_upload`).
    /// Each transfer task in the session thread gets a fresh id; the panel
    /// uses the id to find the matching `Transfer` row when it processes
    /// events.
    next_id: Arc<AtomicU64>,
    _thread: thread::JoinHandle<()>,
}

impl SshSession {
    /// Connect + authenticate. Blocks until the connection is established (or
    /// fails), so callers can surface errors immediately. Returns an `Arc` so the
    /// shell backend and SFTP panel can share the one connection.
    pub fn connect(config: SshConfig) -> Result<Arc<Self>> {
        let (cmd_tx, cmd_rx) = flume::unbounded::<SessionCmd>();
        let (ready_tx, ready_rx) = flume::bounded::<Result<()>>(1);
        // `next_id` lives on the public `SshSession` so `sftp_download` /
        // `sftp_upload` can allocate ids synchronously (the panel learns
        // its id before the work even starts, which lets it insert a
        // placeholder row in the UI immediately).
        let next_id = Arc::new(AtomicU64::new(1));

        let thread = thread::Builder::new()
            .name("caracal-ssh".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.into()));
                        return;
                    }
                };
                rt.block_on(async move {
                    match connect_and_auth(config).await {
                        Ok(handle) => {
                            let _ = ready_tx.send(Ok(()));
                            command_loop(handle, cmd_rx).await;
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                        }
                    }
                });
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Arc::new(Self {
                cmd_tx,
                next_id,
                _thread: thread,
            })),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("ssh runtime thread exited before reporting readiness")),
        }
    }

    /// Open a shell channel and return a `PtyBackend` for it. Bytes read from the
    /// remote shell are pushed into `bytes_tx` (the feeder's sink).
    pub fn open_shell(
        &self,
        cols: u16,
        rows: u16,
        bytes_tx: flume::Sender<Vec<u8>>,
    ) -> Arc<dyn PtyBackend> {
        let (ctrl_tx, ctrl_rx) = flume::unbounded::<Ctrl>();
        let _ = self.cmd_tx.send(SessionCmd::OpenShell {
            cols,
            rows,
            bytes_tx,
            ctrl_rx,
        });
        Arc::new(SshShellBackend { ctrl_tx })
    }

    /// List a remote directory. The returned receiver yields the result once the
    /// session thread has serviced it.
    pub fn sftp_read_dir(&self, path: String) -> flume::Receiver<Result<Vec<SftpEntry>>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self
            .cmd_tx
            .send(SessionCmd::Sftp(SftpRequest::ReadDir { path, reply }));
        rx
    }

    /// Download `remote` to `local`. Returns a one-shot receiver that yields
    /// a [`TransferHandle`] whose `events` stream carries progress and the
    /// final outcome. The actual work runs as a background tokio task on
    /// the session thread, so the shell channel and other SFTP requests
    /// stay responsive.
    pub fn sftp_download(&self, remote: String, local: PathBuf) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Download {
            remote,
            local,
            reply,
            id,
        }));
        rx
    }

    /// Upload `local` to `remote`. Same progress-stream shape as
    /// [`Self::sftp_download`].
    pub fn sftp_upload(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Upload {
            local,
            remote,
            reply,
            id,
        }));
        rx
    }
}

/// Terminal backend over an SSH shell channel. Only carries the control channel;
/// the session thread owns the russh channel.
struct SshShellBackend {
    ctrl_tx: flume::Sender<Ctrl>,
}

impl PtyBackend for SshShellBackend {
    fn write(&self, bytes: &[u8]) {
        let _ = self.ctrl_tx.send(Ctrl::Data(bytes.to_vec()));
    }
    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.ctrl_tx.send(Ctrl::Resize { cols, rows });
    }
}

impl Drop for SshShellBackend {
    fn drop(&mut self) {
        let _ = self.ctrl_tx.send(Ctrl::Close);
    }
}

// --- session-thread internals ---

async fn connect_and_auth(config: SshConfig) -> Result<Handle<ClientHandler>> {
    let SshConfig {
        host,
        port,
        user,
        password,
    } = config;

    let cfg = Arc::new(client::Config::default());
    let mut session = client::connect(cfg, (host.as_str(), port), ClientHandler)
        .await
        .map_err(|e| anyhow!("connect to {host}:{port} failed: {e}"))?;

    let auth = session.authenticate_password(user, password).await?;
    if !auth.success() {
        return Err(anyhow!("authentication failed"));
    }
    Ok(session)
}

async fn command_loop(handle: Handle<ClientHandler>, cmd_rx: flume::Receiver<SessionCmd>) {
    // The SFTP subsystem channel is opened lazily on first SFTP use and
    // shared via `Arc<SftpSession>` so multiple concurrent background
    // transfer tasks can borrow it (`SftpSession` methods take `&self`,
    // and the struct is `Send + Sync` because its fields are
    // `Arc<RawSftpSession>` + `Copy Features`). The `Mutex` only protects
    // the *initialization* — once populated we clone out an
    // `Arc<SftpSession>` and drop the lock before the task starts.
    let sftp_slot: Arc<tokio::sync::Mutex<Option<Arc<SftpSession>>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    while let Ok(cmd) = cmd_rx.recv_async().await {
        match cmd {
            SessionCmd::OpenShell {
                cols,
                rows,
                bytes_tx,
                ctrl_rx,
            } => match open_shell_channel(&handle, cols, rows).await {
                Ok(channel) => {
                    // Long-running; spawn so the command loop keeps servicing SFTP.
                    tokio::spawn(shell_pump(channel, bytes_tx, ctrl_rx));
                }
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mshell channel failed:\x1b[0m {e}\r\n").into_bytes());
                }
            },
            SessionCmd::Sftp(request) => {
                // Ensure the SFTP session is open. If it isn't yet, drop the
                // lock before the (potentially slow) `open_sftp` await, then
                // re-acquire to store the result.
                {
                    let guard = sftp_slot.lock().await;
                    if guard.is_none() {
                        drop(guard);
                        log::info!("sftp: opening subsystem channel (first use on this connection)");
                        match open_sftp(&handle).await {
                            Ok(s) => {
                                log::info!("sftp: subsystem channel ready");
                                let mut g = sftp_slot.lock().await;
                                *g = Some(Arc::new(s));
                            }
                            Err(e) => {
                                log::error!("sftp: open failed: {e:#}");
                                reply_sftp_error(request, anyhow!("open sftp failed: {e}"));
                                continue;
                            }
                        }
                    }
                }
                service_sftp(&sftp_slot, request).await;
            }
        }
    }

    let _ = handle
        .disconnect(Disconnect::ByApplication, "", "English")
        .await;
}

async fn open_shell_channel(
    handle: &Handle<ClientHandler>,
    cols: u16,
    rows: u16,
) -> Result<Channel<Msg>> {
    let channel = handle.channel_open_session().await?;
    // Report the type Caracal's own renderer (`alacritty_terminal`) actually
    // understands, not the host's local `$TERM` (e.g. Ghostty sets
    // `xterm-ghostty`, which most remote hosts lack a terminfo entry for —
    // "unknown terminal type" from `top`/`clear`/etc). Matches the local-PTY
    // backend's hardcoded value (backend.rs).
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;
    Ok(channel)
}

/// Split the shell channel so read backpressure can't block writes (keystrokes /
/// Ctrl-C stay responsive under heavy remote output).
async fn shell_pump(
    channel: Channel<Msg>,
    bytes_tx: flume::Sender<Vec<u8>>,
    ctrl_rx: flume::Receiver<Ctrl>,
) {
    let (mut read_half, write_half) = channel.split();

    let read_loop = async {
        loop {
            match read_half.wait().await {
                Some(ChannelMsg::Data { ref data }) => {
                    if bytes_tx.send_async(data.to_vec()).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                    if bytes_tx.send_async(data.to_vec()).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::ExitStatus { .. }) => {}
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
    };

    let write_loop = async {
        loop {
            match ctrl_rx.recv_async().await {
                Ok(Ctrl::Data(b)) => {
                    if write_half.data(&b[..]).await.is_err() {
                        break;
                    }
                }
                Ok(Ctrl::Resize { cols, rows }) => {
                    let _ = write_half.window_change(cols as u32, rows as u32, 0, 0).await;
                }
                Ok(Ctrl::Close) | Err(_) => {
                    let _ = write_half.eof().await;
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = read_loop => {}
        _ = write_loop => {}
    }
}

async fn open_sftp(handle: &Handle<ClientHandler>) -> Result<SftpSession> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| anyhow!("channel_open_session: {e}"))?;
    log::info!("sftp: channel opened, requesting \"sftp\" subsystem");
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| anyhow!("request_subsystem(sftp): {e}"))?;
    log::info!("sftp: subsystem accepted, starting SftpSession handshake");
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| anyhow!("SftpSession::new (INIT/VERSION handshake): {e}"))?;
    Ok(sftp)
}

async fn service_sftp(
    sftp_slot: &Arc<tokio::sync::Mutex<Option<Arc<SftpSession>>>>,
    request: SftpRequest,
) {
    match request {
        SftpRequest::ReadDir { path, reply } => {
            // ReadDir stays synchronous (it's a quick metadata call and the
            // panel depends on the reply to know it's done). We hold the
            // lock just long enough to clone out an `Arc<SftpSession>`.
            let sftp = {
                let g = sftp_slot.lock().await;
                match g.clone() {
                    Some(s) => s,
                    None => {
                        let _ = reply.send(Err(anyhow!("sftp session not initialized")));
                        return;
                    }
                }
            };
            log::info!("sftp: read_dir {path:?}");
            let result = sftp_read_dir(&sftp, &path).await;
            match &result {
                Ok(entries) => log::info!("sftp: read_dir {path:?} -> {} entries", entries.len()),
                Err(e) => log::error!("sftp: read_dir {path:?} failed: {e:#}"),
            }
            let _ = reply.send(result);
        }
        SftpRequest::Download {
            remote,
            local,
            reply,
            id,
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
                        return;
                    }
                };
                let total = match total {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("stat {remote:?}: {e}"),
                        });
                        return;
                    }
                };
                let _ = events_tx.send(TransferEvent::Started { total });
                match sftp_download_streaming(&sftp, &remote, &local, total, &events_tx).await {
                    Ok(transferred) => {
                        let _ = events_tx.send(TransferEvent::Done { transferred });
                    }
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                    }
                }
            });
        }
        SftpRequest::Upload {
            local,
            remote,
            reply,
            id,
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
                        return;
                    }
                };
                let _ = events_tx.send(TransferEvent::Started { total });
                match sftp_upload_streaming(&sftp, &local, &remote, total, &events_tx).await {
                    Ok(transferred) => {
                        let _ = events_tx.send(TransferEvent::Done { transferred });
                    }
                    Err(e) => {
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                    }
                }
            });
        }
    }
}

fn reply_sftp_error(request: SftpRequest, err: anyhow::Error) {
    match request {
        SftpRequest::ReadDir { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        SftpRequest::Download { reply, id, .. } | SftpRequest::Upload { reply, id, .. } => {
            // The transfer never got far enough to spawn, so synthesize a
            // handle whose `events` channel starts with a single Failed
            // event. The panel will then mark the row failed immediately.
            let (events_tx, events_rx) = flume::bounded::<TransferEvent>(1);
            let _ = events_tx.send(TransferEvent::Failed {
                error: format!("{err:#}"),
            });
            let _ = reply.send(TransferHandle { id, events: events_rx });
        }
    }
}

async fn sftp_read_dir(sftp: &SftpSession, path: &str) -> Result<Vec<SftpEntry>> {
    let mut entries = Vec::new();
    for entry in sftp.read_dir(path).await? {
        let md = entry.metadata();
        entries.push(SftpEntry {
            name: entry.file_name(),
            is_dir: entry.file_type().is_dir(),
            size: md.size.unwrap_or(0),
            mtime: md.mtime.unwrap_or(0),
            // `permissions` carries the file-type mode bits too (e.g.
            // 0o100644 / 0o040755). The panel slices out the 9 perm bits
            // for display; the file-type bits are ignored for the table.
            perms: md.permissions.unwrap_or(0),
        });
    }
    // Directories first, then alphabetical.
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

/// Streaming download. Reads from the remote SFTP file in 32 KiB chunks and
/// writes each chunk to a local `tokio::fs::File`. Emits a `Progress` event
/// every `PROGRESS_INTERVAL` bytes (clamped so we never spam events on tiny
/// writes).
async fn sftp_download_streaming(
    sftp: &SftpSession,
    remote: &str,
    local: &PathBuf,
    _total: u64,
    events: &flume::Sender<TransferEvent>,
) -> Result<u64> {
    const CHUNK: usize = 32 * 1024;
    const PROGRESS_INTERVAL: u64 = 64 * 1024;

    let mut remote_file = sftp.open(remote).await?;
    let mut local_file = tokio::fs::File::create(local).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    let mut last_progress_at: u64 = 0;
    loop {
        let n = remote_file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        local_file.write_all(&buf[..n]).await?;
        transferred += n as u64;
        if transferred - last_progress_at >= PROGRESS_INTERVAL {
            let _ = events.send(TransferEvent::Progress { transferred });
            last_progress_at = transferred;
        }
    }
    local_file.flush().await?;
    // Final progress tick so the bar reaches 100% even on small files that
    // never crossed the interval threshold.
    let _ = events.send(TransferEvent::Progress { transferred });
    Ok(transferred)
}

/// Streaming upload. Reads the local file in 32 KiB chunks and writes them
/// to the remote SFTP file (created with `CREATE | WRITE | TRUNCATE`).
/// Progress event cadence matches `sftp_download_streaming`.
async fn sftp_upload_streaming(
    sftp: &SftpSession,
    local: &PathBuf,
    remote: &str,
    _total: u64,
    events: &flume::Sender<TransferEvent>,
) -> Result<u64> {
    use tokio::io::AsyncReadExt;

    const CHUNK: usize = 32 * 1024;
    const PROGRESS_INTERVAL: u64 = 64 * 1024;

    let mut local_file = tokio::fs::File::open(local).await?;
    let mut remote_file = sftp
        .open_with_flags(
            remote,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
        )
        .await?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    let mut last_progress_at: u64 = 0;
    loop {
        let n = local_file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        remote_file.write_all(&buf[..n]).await?;
        transferred += n as u64;
        if transferred - last_progress_at >= PROGRESS_INTERVAL {
            let _ = events.send(TransferEvent::Progress { transferred });
            last_progress_at = transferred;
        }
    }
    remote_file.shutdown().await?;
    let _ = events.send(TransferEvent::Progress { transferred });
    Ok(transferred)
}
