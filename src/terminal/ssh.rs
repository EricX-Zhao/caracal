//! SSH backend: a `russh` client confined to a dedicated OS thread running its
//! own current-thread tokio runtime (CLAUDE.md §2 — the russh async world lives
//! entirely here; it talks to the rest of Caracal only through `flume`).
//!
//! Data flow, all welded by `flume`:
//!   - incoming: `channel.wait()` → `ChannelMsg::Data`/`ExtendedData` →
//!     `bytes_tx` (the same sink `LocalPty` uses; the bridge feeder owns the
//!     single point where bytes enter the `Term`).
//!   - outgoing: `PtyBackend::write` / `resize` → `ctrl_tx` → the runtime thread
//!     → `channel.data()` / `channel.window_change()`.
//!
//! One `Session` = one russh connection (the SFTP subsystem in Phase 7 will
//! reuse this same `Handle` rather than dialing a second connection).

use std::sync::Arc;
use std::thread;

use anyhow::{Result, anyhow};
use russh::client::{self, Handle, Msg};
use russh::keys::ssh_key::PublicKey;
use russh::{Channel, ChannelMsg, Disconnect};

use crate::terminal::backend::PtyBackend;

/// Connection parameters. Phase 4 supports password auth only; key-based auth
/// and known-hosts/TOFU verification come later.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

/// Messages from the GPUI side to the SSH runtime thread.
enum Ctrl {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

/// russh client event handler. Phase 4 accepts any host key (TOFU/known-hosts
/// is a follow-up); the signature pins the current russh key type.
struct ClientHandler;

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _server_public_key: &PublicKey) -> Result<bool, Self::Error> {
        // TODO(phase 4+): verify against known_hosts with trust-on-first-use.
        Ok(true)
    }
}

pub struct SshBackend {
    ctrl_tx: flume::Sender<Ctrl>,
    _thread: thread::JoinHandle<()>,
}

impl SshBackend {
    /// Connect, authenticate, request a PTY + shell, and start the runtime loop.
    /// Blocks until the connection is established (or fails), so the caller can
    /// surface connection errors immediately.
    pub fn spawn(
        config: SshConfig,
        cols: u16,
        rows: u16,
        bytes_tx: flume::Sender<Vec<u8>>,
    ) -> Result<Self> {
        let (ctrl_tx, ctrl_rx) = flume::unbounded::<Ctrl>();
        // One-shot readiness report: Ok once shell is up, or the connect error.
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
                    match connect_and_shell(config, cols, rows).await {
                        Ok((session, channel)) => {
                            let _ = ready_tx.send(Ok(()));
                            run_loop(session, channel, bytes_tx, ctrl_rx).await;
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                        }
                    }
                });
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                ctrl_tx,
                _thread: thread,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("ssh runtime thread exited before reporting readiness")),
        }
    }
}

impl PtyBackend for SshBackend {
    fn write(&self, bytes: &[u8]) {
        let _ = self.ctrl_tx.send(Ctrl::Data(bytes.to_vec()));
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.ctrl_tx.send(Ctrl::Resize { cols, rows });
    }
}

impl Drop for SshBackend {
    fn drop(&mut self) {
        let _ = self.ctrl_tx.send(Ctrl::Close);
    }
}

/// Connect + authenticate + open a shell channel. Errors here are reported to
/// the caller as connection failures.
async fn connect_and_shell(
    config: SshConfig,
    cols: u16,
    rows: u16,
) -> Result<(Handle<ClientHandler>, Channel<Msg>)> {
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

    let channel = session.channel_open_session().await?;
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
    channel
        .request_pty(false, &term, cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;

    Ok((session, channel))
}

/// The post-connection event loop: pump bytes both ways until the channel
/// closes or the view is dropped.
async fn run_loop(
    session: Handle<ClientHandler>,
    mut channel: Channel<Msg>,
    bytes_tx: flume::Sender<Vec<u8>>,
    ctrl_rx: flume::Receiver<Ctrl>,
) {
    loop {
        tokio::select! {
            ctrl = ctrl_rx.recv_async() => {
                match ctrl {
                    Ok(Ctrl::Data(b)) => {
                        if channel.data(&b[..]).await.is_err() {
                            break;
                        }
                    }
                    Ok(Ctrl::Resize { cols, rows }) => {
                        let _ = channel.window_change(cols as u32, rows as u32, 0, 0).await;
                    }
                    Ok(Ctrl::Close) | Err(_) => {
                        let _ = channel.eof().await;
                        break;
                    }
                }
            }
            msg = channel.wait() => {
                match msg {
                    // stdout
                    Some(ChannelMsg::Data { ref data }) => {
                        if bytes_tx.send(data.to_vec()).is_err() {
                            break;
                        }
                    }
                    // stderr arrives as ExtendedData (CLAUDE.md §4) — render it inline.
                    Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        if bytes_tx.send(data.to_vec()).is_err() {
                            break;
                        }
                    }
                    // ExitStatus precedes Close; keep reading until the channel
                    // actually closes so we don't drop trailing output.
                    Some(ChannelMsg::ExitStatus { .. }) => {}
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
        }
    }

    let _ = session
        .disconnect(Disconnect::ByApplication, "", "English")
        .await;
}
