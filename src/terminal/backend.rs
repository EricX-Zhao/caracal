//! Backend abstraction: every transport (local PTY, SSH, serial) implements the
//! same `PtyBackend`. `TerminalView` is agnostic to which one it talks to
//! (CLAUDE.md §2). Phase 1 only implements `LocalPty`.

use std::io::{Read, Write};
use std::sync::Mutex;

use anyhow::Result;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Pick the user's default shell and the env var that holds their home
/// directory. Unix: `$SHELL` / `$HOME` (fallback `/bin/sh`). Windows: `%COMSPEC%`
/// (fallback `cmd.exe`) / `%USERPROFILE%`. Done at spawn-time so it picks up
/// env changes (e.g. inside containers / SSH).
fn default_shell_and_home() -> (String, &'static str) {
    #[cfg(windows)]
    {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        (shell, "USERPROFILE")
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        (shell, "HOME")
    }
}

/// Unified byte-stream interface over a backend transport.
///
/// - `write`: bytes destined for the backend (keystrokes / query responses).
/// - `resize`: notify the backend of a new (cols, rows).
///
/// Reading is *not* on this trait: a backend pushes received bytes into the
/// `bytes_tx` channel it was constructed with, and the bridge feeder owns the
/// single point where bytes enter the `Term`.
pub trait PtyBackend: Send + Sync + 'static {
    fn write(&self, bytes: &[u8]);
    fn resize(&self, cols: u16, rows: u16);
}

/// A backend that discards all input. Used as a placeholder when a real backend
/// fails to start (e.g. SSH connection refused) so the `TerminalView` stays a
/// valid entity and can still display the error text fed into its `Term`.
pub struct NullBackend;

impl PtyBackend for NullBackend {
    fn write(&self, _bytes: &[u8]) {}
    fn resize(&self, _cols: u16, _rows: u16) {}
}

/// Local pseudo-terminal running the user's shell, via `portable-pty`.
pub struct LocalPty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Sender to the dedicated writer thread (keeps the single non-Sync writer
    /// confined to one thread).
    write_tx: flume::Sender<Vec<u8>>,
    _child: Mutex<Box<dyn Child + Send + Sync>>,
}

impl LocalPty {
    /// Spawn the user's default shell in a fresh PTY. Bytes read from the PTY are
    /// pushed into `bytes_tx`.
    pub fn spawn(cols: u16, rows: u16, bytes_tx: flume::Sender<Vec<u8>>) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let (shell, home_var) = default_shell_and_home();
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        if let Ok(home) = std::env::var(home_var) {
            cmd.cwd(home);
        }
        let child = pair.slave.spawn_command(cmd)?;
        // Drop the slave so that when the child exits the master read hits EOF.
        drop(pair.slave);

        // Reader thread: PTY -> bytes_tx.
        let mut reader = pair.master.try_clone_reader()?;
        std::thread::Builder::new()
            .name("caracal-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if bytes_tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })?;

        // Writer thread: write_rx -> PTY.
        let writer = pair.master.take_writer()?;
        let (write_tx, write_rx) = flume::unbounded::<Vec<u8>>();
        std::thread::Builder::new()
            .name("caracal-pty-writer".into())
            .spawn(move || {
                let mut writer = writer;
                while let Ok(bytes) = write_rx.recv() {
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            })?;

        Ok(Self {
            master: Mutex::new(pair.master),
            write_tx,
            _child: Mutex::new(child),
        })
    }
}

impl PtyBackend for LocalPty {
    fn write(&self, bytes: &[u8]) {
        let _ = self.write_tx.send(bytes.to_vec());
    }

    fn resize(&self, cols: u16, rows: u16) {
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }
}
