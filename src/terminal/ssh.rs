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
        reply: flume::Sender<Result<u64>>,
    },
    Upload {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<Result<u64>>,
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
    _thread: thread::JoinHandle<()>,
}

impl SshSession {
    /// Connect + authenticate. Blocks until the connection is established (or
    /// fails), so callers can surface errors immediately. Returns an `Arc` so the
    /// shell backend and SFTP panel can share the one connection.
    pub fn connect(config: SshConfig) -> Result<Arc<Self>> {
        let (cmd_tx, cmd_rx) = flume::unbounded::<SessionCmd>();
        let (ready_tx, ready_rx) = flume::bounded::<Result<()>>(1);

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

    /// Download `remote` to `local`. Yields the byte count on success.
    pub fn sftp_download(&self, remote: String, local: PathBuf) -> flume::Receiver<Result<u64>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Download {
            remote,
            local,
            reply,
        }));
        rx
    }

    /// Upload `local` to `remote`. Yields the byte count on success.
    pub fn sftp_upload(&self, local: PathBuf, remote: String) -> flume::Receiver<Result<u64>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Upload {
            local,
            remote,
            reply,
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
    // The SFTP subsystem channel is opened lazily on first SFTP use and reused.
    let mut sftp: Option<SftpSession> = None;

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
                if sftp.is_none() {
                    match open_sftp(&handle).await {
                        Ok(s) => sftp = Some(s),
                        Err(e) => {
                            reply_sftp_error(request, anyhow!("open sftp failed: {e}"));
                            continue;
                        }
                    }
                }
                service_sftp(sftp.as_ref().unwrap(), request).await;
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
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
    channel
        .request_pty(false, &term, cols as u32, rows as u32, 0, 0, &[])
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
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = SftpSession::new(channel.into_stream()).await?;
    Ok(sftp)
}

async fn service_sftp(sftp: &SftpSession, request: SftpRequest) {
    match request {
        SftpRequest::ReadDir { path, reply } => {
            let _ = reply.send(sftp_read_dir(sftp, &path).await);
        }
        SftpRequest::Download {
            remote,
            local,
            reply,
        } => {
            let _ = reply.send(sftp_download(sftp, &remote, &local).await);
        }
        SftpRequest::Upload {
            local,
            remote,
            reply,
        } => {
            let _ = reply.send(sftp_upload(sftp, &local, &remote).await);
        }
    }
}

fn reply_sftp_error(request: SftpRequest, err: anyhow::Error) {
    match request {
        SftpRequest::ReadDir { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        SftpRequest::Download { reply, .. } | SftpRequest::Upload { reply, .. } => {
            let _ = reply.send(Err(err));
        }
    }
}

async fn sftp_read_dir(sftp: &SftpSession, path: &str) -> Result<Vec<SftpEntry>> {
    let mut entries = Vec::new();
    for entry in sftp.read_dir(path).await? {
        entries.push(SftpEntry {
            name: entry.file_name(),
            is_dir: entry.file_type().is_dir(),
            size: entry.metadata().size.unwrap_or(0),
        });
    }
    // Directories first, then alphabetical.
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

async fn sftp_download(sftp: &SftpSession, remote: &str, local: &PathBuf) -> Result<u64> {
    let mut file = sftp.open(remote).await?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await?;
    tokio::fs::write(local, &buf).await?;
    Ok(buf.len() as u64)
}

async fn sftp_upload(sftp: &SftpSession, local: &PathBuf, remote: &str) -> Result<u64> {
    let data = tokio::fs::read(local).await?;
    let mut file = sftp
        .open_with_flags(
            remote,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
        )
        .await?;
    file.write_all(&data).await?;
    file.shutdown().await?;
    Ok(data.len() as u64)
}
