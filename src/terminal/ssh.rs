//! SSH session: one `russh` connection on a dedicated OS thread + current-thread
//! tokio runtime (CLAUDE.md §2 — the async world lives here; it talks to the rest
//! of Caracal only through `flume`).
//!
//! **One Session = one connection.** The shell (terminal) channel and the SFTP
//! subsystem channel are opened on the *same* `Handle` — SFTP does not dial a
//! second connection. A command loop on the session thread multiplexes: it
//! spawns the shell pump as a task and services SFTP requests inline.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::collections::HashMap;

use anyhow::{Result, anyhow};
use russh::client::{self, Handle, Msg};
use russh::keys::ssh_key::PublicKey;
use russh::keys::{PrivateKeyWithHashAlg, decode_secret_key};
use russh::{Channel, ChannelMsg, Disconnect};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::terminal::backend::PtyBackend;

/// Authentication method for an SSH connection.
#[derive(Clone, Debug)]
pub enum SshAuth {
    Password(String),
    /// `content` is the private key's raw file bytes (decrypted from the
    /// vault by the caller — see `config::SavedConnection::to_ssh_config`);
    /// `passphrase` decrypts it if it's passphrase-protected.
    /// `russh::keys::decode_secret_key` handles both encrypted and plain
    /// keys through the same call, parsing from an in-memory string instead
    /// of a file path (the vault stores key *content*, not a path).
    PrivateKeyContent {
        content: Vec<u8>,
        passphrase: Option<String>,
    },
}

/// Connection parameters — password or private-key authentication.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
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
    /// Resolve `path` to an absolute canonical path (SFTP `realpath`).
    /// Used to turn the initial "." shown on the SFTP dock into the
    /// user's actual remote home dir.
    Realpath {
        path: String,
        reply: flume::Sender<Result<String>>,
    },
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
    /// Create a directory on the remote.
    Mkdir {
        path: String,
        reply: flume::Sender<Result<()>>,
    },
    /// Create an empty file on the remote.
    CreateFile {
        path: String,
        reply: flume::Sender<Result<()>>,
    },
    /// Remove a file or directory. If `recursive` is true and `path` is a
    /// directory, walks and removes all descendants.
    Remove {
        path: String,
        recursive: bool,
        reply: flume::Sender<Result<()>>,
    },
    /// Rename (or move within the same directory — this round's UI only
    /// rewrites the filename, never the directory) a remote path.
    Rename {
        old: String,
        new: String,
        reply: flume::Sender<Result<()>>,
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
    /// Transfer was cancelled by the user. Partial bytes may have been
    /// transferred; the streaming loop stopped and the remote/local file is
    /// left in whatever state was reached before the abort.
    Cancelled {
        transferred: u64,
    },
    /// A directory transfer (`sftp_download_dir`/`sftp_upload_dir`) finished
    /// after skipping one or more files that failed individually
    /// (permission denied, vanished mid-walk, etc.) — the rest of the
    /// directory still completed. Never emitted for single-file transfers.
    DoneWithFailures {
        transferred: u64,
        failed_paths: Vec<String>,
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
    /// Run a one-off, non-interactive command over a fresh channel on the
    /// same connection (used for resource-monitoring polls). Not a shell —
    /// no PTY, no persistent state between calls.
    Exec {
        command: String,
        reply: flume::Sender<Result<String>>,
    },
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
                cancels: Arc::new(std::sync::Mutex::new(HashMap::new())),
                pauses: Arc::new(std::sync::Mutex::new(HashMap::new())),
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

    /// Resolve `path` to an absolute canonical path (SFTP `realpath`). Returns
    /// a receiver that yields the resolved string once the session thread has
    /// serviced it. Used to turn the SFTP panel's initial "." into the user's
    /// actual remote home directory.
    pub fn sftp_realpath(&self, path: String) -> flume::Receiver<Result<String>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self
            .cmd_tx
            .send(SessionCmd::Sftp(SftpRequest::Realpath { path, reply }));
        rx
    }

    /// Download `remote` to `local`. Returns a one-shot receiver that yields
    /// a [`TransferHandle`] whose `events` stream carries progress and the
    /// final outcome. The actual work runs as a background tokio task on
    /// the session thread, so the shell channel and other SFTP requests
    /// stay responsive.
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

    /// Create a directory on the remote. Single-shot result via the returned
    /// receiver.
    pub fn sftp_mkdir(&self, path: String) -> flume::Receiver<Result<()>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Mkdir {
            path,
            reply,
        }));
        rx
    }

    /// Create an empty remote file (no truncation of an existing file is
    /// performed — caller should pick a unique name or rely on
    /// `OpenFlags::CREATE` semantics of the underlying protocol).
    pub fn sftp_create_file(&self, path: String) -> flume::Receiver<Result<()>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::CreateFile {
            path,
            reply,
        }));
        rx
    }

    /// Remove a file or directory. If `path` is a directory and `recursive`
    /// is false, the server may refuse with an error if the dir is non-empty.
    pub fn sftp_remove(&self, path: String, recursive: bool) -> flume::Receiver<Result<()>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Remove {
            path,
            recursive,
            reply,
        }));
        rx
    }

    /// Rename `old` to `new` (SFTP `rename`). This round's UI only calls
    /// this with `old`/`new` sharing the same parent directory (rename, not
    /// move) — the backend itself doesn't enforce that.
    pub fn sftp_rename(&self, old: String, new: String) -> flume::Receiver<Result<()>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Rename {
            old,
            new,
            reply,
        }));
        rx
    }

    /// Run `command` on the remote host over a fresh, non-interactive
    /// channel (no PTY) and collect its stdout as a `String`. Used for
    /// resource-monitoring polls — a separate channel from the shell/SFTP,
    /// same connection (CLAUDE.md §2: one Session, one connection).
    pub fn exec_command(&self, command: String) -> flume::Receiver<Result<String>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Exec { command, reply });
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
        auth,
    } = config;

    let cfg = Arc::new(client::Config::default());
    let mut session = client::connect(cfg, (host.as_str(), port), ClientHandler)
        .await
        .map_err(|e| anyhow!("connect to {host}:{port} failed: {e}"))?;

    let auth_result = match auth {
        SshAuth::Password(password) => session.authenticate_password(user, password).await?,
        SshAuth::PrivateKeyContent { content, passphrase } => {
            let key_str = String::from_utf8(content)
                .map_err(|e| anyhow!("private key content is not valid UTF-8: {e}"))?;
            let key = decode_secret_key(&key_str, passphrase.as_deref())
                .map_err(|e| anyhow!("failed to parse private key: {e}"))?;
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            session.authenticate_publickey(user, key).await?
        }
    };
    if !auth_result.success() {
        return Err(anyhow!("authentication failed"));
    }
    Ok(session)
}

async fn command_loop(handle: Handle<ClientHandler>, cmd_rx: flume::Receiver<SessionCmd>) {
    let handle = Arc::new(handle);
    // The SFTP subsystem channel is opened lazily on first SFTP use and
    // shared via `Arc<SftpSession>` so multiple concurrent background
    // transfer tasks can borrow it (`SftpSession` methods take `&self`,
    // and the struct is `Send + Sync` because its fields are
    // `Arc<RawSftpSession>` + `Copy Features`). The `Mutex` only protects
    // the *initialization* — once populated we clone out an
    // `Arc<SftpSession>` and drop the lock before the task starts.
    let sftp_slot: Arc<tokio::sync::Mutex<Option<Arc<SftpSession>>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Periodic keepalive so a long-idle connection (e.g. the shell tab
    // sitting in an interactive nested `ssh` session on the remote, with
    // no SFTP/exec traffic of our own for a while) is less likely to hit
    // a server-side idle timeout on this connection. Runs until the
    // connection itself is gone (`send_keepalive` starts failing).
    {
        let handle = handle.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if handle.send_keepalive(false).await.is_err() {
                    break;
                }
            }
        });
    }

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
            SessionCmd::Exec { command, reply } => {
                // Long-running-ish (network round-trip); spawn so the
                // command loop keeps servicing shell/SFTP concurrently,
                // same rationale as `OpenShell`'s `tokio::spawn`.
                let handle = handle.clone();
                tokio::spawn(async move {
                    let result = run_exec(&handle, &command).await;
                    let _ = reply.send(result);
                });
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

/// Run `command` over a fresh non-interactive channel and collect its
/// stdout. Mirrors `shell_pump`'s read loop (`ChannelMsg::Data` until
/// `Eof`/`Close`), minus the write side — exec sends no input, just runs
/// one command and streams the reply. Captures stderr and the exit status
/// too: a nonzero exit is reported as an `Err` (with stderr, if any, in
/// the message) rather than silently returning `Ok("")` — the
/// resource-monitoring poll loop this feeds relies on this `Result` to
/// detect failures.
async fn run_exec(handle: &Arc<Handle<ClientHandler>>, command: &str) -> Result<String> {
    let mut channel = handle.channel_open_session().await?;
    channel.exec(true, command).await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_status: Option<u32> = None;
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { ref data }) => stdout.extend_from_slice(data),
            Some(ChannelMsg::ExtendedData { ref data, .. }) => stderr.extend_from_slice(data),
            Some(ChannelMsg::ExitStatus { exit_status: status }) => exit_status = Some(status),
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    if let Some(status) = exit_status
        && status != 0
    {
        let stderr_text = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(anyhow!("command exited with status {status}: {stderr_text}"));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
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
                Err(e) => {
                    log::error!("sftp: read_dir {path:?} failed: {e:#}");
                    invalidate_sftp_if_dead(sftp_slot, e).await;
                }
            }
            let _ = reply.send(result);
        }
        SftpRequest::Realpath { path, reply } => {
            // Same lock-and-clone pattern as ReadDir — short call, inline.
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
            log::info!("sftp: realpath {path:?}");
            match sftp.canonicalize(&path).await {
                Ok(resolved) => {
                    log::info!("sftp: realpath {path:?} -> {resolved:?}");
                    let _ = reply.send(Ok(resolved));
                }
                Err(e) => {
                    log::error!("sftp: realpath {path:?} failed: {e:#}");
                    let err = anyhow!("realpath {path:?}: {e}");
                    invalidate_sftp_if_dead(sftp_slot, &err).await;
                    let _ = reply.send(Err(err));
                }
            }
        }
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
        SftpRequest::Mkdir { path, reply } => {
            let sftp = match clone_sftp(sftp_slot).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Err(e));
                    return;
                }
            };
            log::info!("sftp: mkdir {path:?}");
            let result = sftp
                .create_dir(&path)
                .await
                .map_err(|e| anyhow!("mkdir {path:?}: {e}"));
            if let Err(ref e) = result {
                log::error!("sftp: mkdir {path:?} failed: {e:#}");
                invalidate_sftp_if_dead(sftp_slot, e).await;
            }
            let _ = reply.send(result);
        }
        SftpRequest::CreateFile { path, reply } => {
            let sftp = match clone_sftp(sftp_slot).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Err(e));
                    return;
                }
            };
            log::info!("sftp: create_file {path:?}");
            // Open with CREATE | WRITE | TRUNCATE then immediately close —
            // that leaves a zero-length file on the remote.
            let result = match sftp
                .open_with_flags(&path, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE)
                .await
            {
                Ok(mut f) => f.shutdown().await.map_err(|e| anyhow!("close {path:?}: {e}")),
                Err(e) => Err(anyhow!("create {path:?}: {e}")),
            };
            if let Err(ref e) = result {
                log::error!("sftp: create_file {path:?} failed: {e:#}");
                invalidate_sftp_if_dead(sftp_slot, e).await;
            }
            let _ = reply.send(result);
        }
        SftpRequest::Remove { path, recursive, reply } => {
            let sftp = match clone_sftp(sftp_slot).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Err(e));
                    return;
                }
            };
            log::info!("sftp: remove {path:?} recursive={recursive}");
            let result = sftp_remove(&sftp, &path, recursive).await;
            if let Err(ref e) = result {
                log::error!("sftp: remove {path:?} failed: {e:#}");
                invalidate_sftp_if_dead(sftp_slot, e).await;
            }
            let _ = reply.send(result);
        }
        SftpRequest::Rename { old, new, reply } => {
            let sftp = match clone_sftp(sftp_slot).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Err(e));
                    return;
                }
            };
            log::info!("sftp: rename {old:?} -> {new:?}");
            let result = sftp
                .rename(old.clone(), new.clone())
                .await
                .map_err(|e| anyhow!("rename {old:?} -> {new:?}: {e}"));
            if let Err(ref e) = result {
                log::error!("sftp: rename {old:?} -> {new:?} failed: {e:#}");
                invalidate_sftp_if_dead(sftp_slot, e).await;
            }
            let _ = reply.send(result);
        }
    }
}

/// A dead SFTP subsystem channel (server closed it, network blip, etc.)
/// bubbles up as this specific text — either directly from `russh_sftp`
/// (its background pump task ending, `rawsession.rs`'s "session closed"
/// check) or as an EOF on the underlying stream. `sftp_slot` otherwise
/// caches the `SftpSession` forever once opened (see `command_loop`'s
/// `SessionCmd::Sftp` handler), so without this check every request after
/// the channel dies fails identically, forever — this was the actual bug
/// reported 2026-07-15 (SFTP and monitor "died" together after the user
/// ran `ssh` to a second host from inside an already-open shell tab).
fn is_sftp_session_dead(e: &anyhow::Error) -> bool {
    let text = e.to_string();
    text.contains("session closed") || text.contains("Unexpected EOF")
}

/// Clears the cached SFTP session so the *next* request reopens a fresh
/// subsystem channel (`command_loop`'s existing "if guard.is_none()"
/// bootstrap path already does this) instead of reusing a dead one and
/// failing the same way forever. The request that triggered this still
/// fails this one time — there's nothing to retroactively recover — but
/// every subsequent request self-heals.
async fn invalidate_sftp_if_dead(
    sftp_slot: &Arc<tokio::sync::Mutex<Option<Arc<SftpSession>>>>,
    e: &anyhow::Error,
) {
    if is_sftp_session_dead(e) {
        log::warn!("sftp: session appears dead ({e:#}), will reopen on next request");
        *sftp_slot.lock().await = None;
    }
}

/// Helper: clone out the current `SftpSession` Arc (or return an error
/// describing the not-yet-initialized case).
async fn clone_sftp(
    sftp_slot: &Arc<tokio::sync::Mutex<Option<Arc<SftpSession>>>>,
) -> Result<Arc<SftpSession>> {
    let g = sftp_slot.lock().await;
    g.clone()
        .ok_or_else(|| anyhow!("sftp session not initialized"))
}

/// Recursive remove. If `path` is a file, remove it directly. If it's a
/// directory and `recursive` is true, walk entries, removing each child
/// (recursing into subdirectories) and finally remove the directory itself.
/// If the directory is non-empty and `recursive` is false, return an error.
async fn sftp_remove(sftp: &SftpSession, path: &str, recursive: bool) -> Result<()> {
    // First, check what this path is.
    let lstat = sftp
        .metadata(path)
        .await
        .map_err(|e| anyhow!("stat {path:?}: {e}"))?;
    let is_dir = lstat.is_dir();
    if !is_dir {
        sftp.remove_file(path)
            .await
            .map_err(|e| anyhow!("remove {path:?}: {e}"))?;
        return Ok(());
    }
    // It's a directory.
    if !recursive {
        sftp.remove_dir(path)
            .await
            .map_err(|e| anyhow!("remove dir {path:?}: {e}"))?;
        return Ok(());
    }
    // Recursive: walk entries and remove each.
    let entries = sftp.read_dir(path).await.map_err(|e| anyhow!("read_dir {path:?}: {e}"))?;
    for entry in entries {
        let child_name = entry.file_name();
        let child_path = remote_join(path, &child_name);
        let child_is_dir = entry.file_type().is_dir();
        if child_is_dir {
            Box::pin(sftp_remove(sftp, &child_path, true)).await?;
        } else {
            sftp.remove_file(&child_path)
                .await
                .map_err(|e| anyhow!("remove {child_path:?}: {e}"))?;
        }
    }
    sftp.remove_dir(path)
        .await
        .map_err(|e| anyhow!("remove dir {path:?}: {e}"))?;
    Ok(())
}

/// Join a remote directory and child name with a single separator.
fn remote_join(base: &str, name: &str) -> String {
    if base == "." || base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

fn reply_sftp_error(request: SftpRequest, err: anyhow::Error) {
    match request {
        SftpRequest::ReadDir { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        SftpRequest::Realpath { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        SftpRequest::Download { reply, id, .. }
        | SftpRequest::Upload { reply, id, .. }
        | SftpRequest::DownloadDir { reply, id, .. }
        | SftpRequest::UploadDir { reply, id, .. } => {
            let (events_tx, events_rx) = flume::bounded::<TransferEvent>(1);
            let _ = events_tx.send(TransferEvent::Failed {
                error: format!("{err:#}"),
            });
            let _ = reply.send(TransferHandle { id, events: events_rx });
        }
        SftpRequest::Mkdir { reply, .. }
        | SftpRequest::CreateFile { reply, .. }
        | SftpRequest::Remove { reply, .. }
        | SftpRequest::Rename { reply, .. } => {
            let _ = reply.send(Err(err));
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

/// One file to transfer as part of a recursive directory job: its remote
/// path, its local path, and its size (known up front from the initial
/// directory walk — used to size the job's aggregate progress bar).
struct DirTransferItem {
    remote: String,
    local: PathBuf,
    size: u64,
}

/// Recursively walk `remote_root` via SFTP `read_dir`, creating the
/// matching local directory tree under `local_root` as it goes, and
/// collecting every remote *file* (not directory) found along the way.
async fn walk_remote_dir(
    sftp: &SftpSession,
    remote_root: &str,
    local_root: &Path,
) -> Result<Vec<DirTransferItem>> {
    let mut items = Vec::new();
    let mut stack = vec![(remote_root.to_string(), local_root.to_path_buf())];
    while let Some((remote_dir, local_dir)) = stack.pop() {
        tokio::fs::create_dir_all(&local_dir)
            .await
            .map_err(|e| anyhow!("create_dir_all {local_dir:?}: {e}"))?;
        for entry in sftp_read_dir(sftp, &remote_dir).await? {
            let remote_path = remote_join(&remote_dir, &entry.name);
            let local_path = local_dir.join(&entry.name);
            if entry.is_dir {
                stack.push((remote_path, local_path));
            } else {
                items.push(DirTransferItem {
                    remote: remote_path,
                    local: local_path,
                    size: entry.size,
                });
            }
        }
    }
    Ok(items)
}

/// Recursively walk `local_root` on disk, creating the matching remote
/// directory tree under `remote_root` via SFTP `mkdir` as it goes (best
/// effort — a directory that already exists on the remote is not an error
/// here), and collecting every local *file* found along the way.
async fn walk_local_dir(
    sftp: &SftpSession,
    local_root: &Path,
    remote_root: &str,
) -> Result<Vec<DirTransferItem>> {
    let mut items = Vec::new();
    let mut stack = vec![(local_root.to_path_buf(), remote_root.to_string())];
    while let Some((local_dir, remote_dir)) = stack.pop() {
        let _ = sftp.create_dir(&remote_dir).await;
        let mut rd = tokio::fs::read_dir(&local_dir)
            .await
            .map_err(|e| anyhow!("read_dir {local_dir:?}: {e}"))?;
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| anyhow!("read_dir {local_dir:?}: {e}"))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| anyhow!("file_type {:?}: {e}", entry.path()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let remote_path = remote_join(&remote_dir, &name);
            if file_type.is_dir() {
                stack.push((entry.path(), remote_path));
            } else {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                items.push(DirTransferItem {
                    remote: remote_path,
                    local: entry.path(),
                    size,
                });
            }
        }
    }
    Ok(items)
}

/// Download a single file within a directory job. Unlike
/// `sftp_download_streaming`, this never emits `Progress` events itself —
/// the caller (`run_download_dir`) reports progress once per completed
/// file, using a running total across the whole directory, not per-chunk.
/// Honors `cancel` and `paused` via `wait_while_paused`, checked both
/// before opening any file handle and once per chunk.
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

/// Upload a single file within a directory job. Mirrors `download_one_file`
/// — no intermediate `Progress` events, just a final byte count. Same
/// `cancel`/`paused` handling via `wait_while_paused`.
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

/// Download every item in `items` sequentially, emitting one aggregate
/// `Started`/`Progress` pair across the whole job (not per-file) plus a
/// final `Done`/`DoneWithFailures`/`Cancelled`. A single file's failure is
/// logged and skipped — the job keeps going (matches `sftp_remove`'s
/// existing best-effort-recursion style).
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

/// Mirror of `run_download_dir` for uploads.
async fn run_upload_dir(
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
        match upload_one_file(sftp, &item.local, &item.remote, cancel, paused).await {
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
                log::warn!("sftp: upload {:?} failed: {e:#}", item.remote);
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

/// Outcome of a streaming transfer — distinguishes a clean completion from
/// a user-initiated cancel. Both carry the number of bytes that were already
/// written when the loop ended.
enum StreamingOutcome {
    Completed(u64),
    Cancelled(u64),
}

/// Streaming download. Reads from the remote SFTP file in 32 KiB chunks and
/// writes each chunk to a local `tokio::fs::File`. Emits a `Progress` event
/// every `PROGRESS_INTERVAL` bytes (clamped so we never spam events on tiny
/// writes). If `cancel` is observed set during the loop, returns
/// `StreamingOutcome::Cancelled`. If `paused` is set, blocks via
/// `wait_while_paused` (still watching `cancel`) without closing either
/// file handle, both before opening them and once per chunk.
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

/// Streaming upload. Reads the local file in 32 KiB chunks and writes them
/// to the remote SFTP file (created with `CREATE | WRITE | TRUNCATE`).
/// Progress event cadence matches `sftp_download_streaming`. Same
/// `cancel`/`paused` handling.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_a_dead_sftp_session_by_its_error_text() {
        assert!(is_sftp_session_dead(&anyhow!("session closed")));
        // As actually wrapped by call sites like `anyhow!("realpath {path:?}: {e}")`.
        assert!(is_sftp_session_dead(&anyhow!(
            "realpath \"/home\": Unexpected behavior: session closed"
        )));
        assert!(is_sftp_session_dead(&anyhow!("Unexpected EOF on stream")));
    }

    #[test]
    fn does_not_misclassify_unrelated_sftp_errors_as_a_dead_session() {
        assert!(!is_sftp_session_dead(&anyhow!("sftp session not initialized")));
        assert!(!is_sftp_session_dead(&anyhow!(
            "mkdir \"/nope\": No such file or directory"
        )));
        assert!(!is_sftp_session_dead(&anyhow!(
            "remove \"/etc/passwd\": Permission denied"
        )));
    }
}
