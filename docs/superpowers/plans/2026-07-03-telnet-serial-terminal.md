# Telnet and Serial Terminal Backends Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Telnet and serial-port terminal backends to Caracal, selectable from the existing new-connection form alongside SSH and local-shell.

**Architecture:** Two new `PtyBackend` implementations (`TelnetBackend` over `std::net::TcpStream`, `SerialBackend` over the `serialport` crate), each spawning a reader/writer thread pair exactly like the existing `LocalPty` — no tokio, no shared-connection cache (unlike SSH+SFTP, neither protocol multiplexes a second channel). `ConnectionType` grows two variants; the new-connection form grows a third/fourth pill and per-type fields; `Workspace` grows `open_telnet`/`open_serial` mirroring `open_local_with`.

**Tech Stack:** Rust, `serialport = "4.9"` (new dependency), existing `gpui`/`gpui-component`/`flume`/`anyhow`.

## Global Constraints

- No stored Telnet credentials — the form only collects host + port (default port 23); login happens interactively in the terminal, matching a raw `telnet` client.
- Telnet implements "basic negotiation" per RFC 854: parse/strip IAC sequences, answer `SUPPRESS_GO_AHEAD`/`TERMINAL_TYPE`/`ECHO` negotiation, refuse (`DONT`/`WONT`) everything else. No NAWS (live resize) — `TelnetBackend::resize` is a documented no-op.
- Serial gets full configuration: port, baud rate, data bits, parity, stop bits, flow control — not hardcoded 8N1.
- The new-connection type selector stays a hand-rolled pill row (4 pills: SSH / 本地终端 / Telnet / 串口), not a dropdown, consistent with the existing SSH/Local pills.
- No new UI widget dependency — serial's data-bits/parity/stop-bits/flow-control fields are pill-toggles (same visual language as the type selector), not `gpui-component`'s `Select`.
- Source of truth for all of the above: `docs/superpowers/specs/2026-07-03-telnet-serial-terminal-design.md`.

---

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | New `serialport` dependency |
| `src/terminal/telnet.rs` (new) | `TelnetConfig`, IAC parser (`TelnetCodec`), `TelnetBackend` (`PtyBackend` impl) |
| `src/terminal/serial.rs` (new) | `SerialConfig`, baud/parity/etc. mapping helpers, `list_ports()`, `SerialBackend` (`PtyBackend` impl) |
| `src/terminal/mod.rs` | Register the two new modules |
| `src/terminal/backend.rs` | Doc-comment refresh only (no behavior change) |
| `src/panels/icons.rs` | `AppIcon::Telnet` / `AppIcon::SerialPort` |
| `src/terminal/view.rs` | `TerminalView::new_telnet` / `::new_serial` constructors |
| `src/config.rs` | `ConnectionType` variants, `SavedConnection` serial fields, `to_telnet_config`/`to_serial_config`, display/subtitle/icon/tooltip match arms |
| `src/panels/saved_connections.rs` | `SavedConnectionsEvent` variants, `open_event()` helper, 4-pill type selector, serial form fields + port picker |
| `src/workspace.rs` | `open_telnet`/`open_serial`, event subscription wiring |

**Sequencing note:** `ConnectionType` is matched exhaustively in both `config.rs` and `saved_connections.rs`, and `SavedConnectionsEvent` is matched exhaustively in `workspace.rs`. Rust's exhaustiveness check means the crate will not compile if these are updated separately — Task 6 below intentionally updates `config.rs` + `saved_connections.rs` + `workspace.rs` together as one atomic, one-commit unit. Tasks 1–5 are additive/standalone (new files, new enum variants nothing consumes yet) and each compiles independently.

---

### Task 1: Add the `serialport` dependency

**Files:**
- Modify: `Cargo.toml`

**Interfaces:**
- Produces: the `serialport` crate (v4.x) available to the rest of the plan.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, under the `# Terminal model + transport` section, change:

```toml
# Terminal model + transport
alacritty_terminal = "0.26.0"
portable-pty = "0.9.0"

russh = "0.61"
```

to:

```toml
# Terminal model + transport
alacritty_terminal = "0.26.0"
portable-pty = "0.9.0"
# Serial-port I/O for the serial terminal backend. Default features include
# `libudev` on Linux for `available_ports()` port detection — requires
# libudev headers at build time (already present on this dev machine; on a
# fresh Linux box install `libudev-dev` / `systemd-devel`).
serialport = "4.9"

russh = "0.61"
```

- [ ] **Step 2: Build to confirm it downloads and links**

Run: `cargo build`
Expected: succeeds (downloads `serialport` and its transitive deps, links against `libudev` on Linux). If this fails with a `libudev`/`pkg-config` error, install the platform's libudev dev package before continuing — every later task depends on this compiling.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add serialport dependency for the serial terminal backend"
```

---

### Task 2: Telnet backend (`src/terminal/telnet.rs`)

**Files:**
- Create: `src/terminal/telnet.rs`
- Modify: `src/terminal/mod.rs`

**Interfaces:**
- Consumes: `crate::terminal::backend::PtyBackend` (trait, `fn write(&self, bytes: &[u8])`, `fn resize(&self, cols: u16, rows: u16)`).
- Produces: `pub struct TelnetConfig { pub host: String, pub port: u16 }`, `pub struct TelnetBackend` implementing `PtyBackend`, `TelnetBackend::connect(config: TelnetConfig, bytes_tx: flume::Sender<Vec<u8>>) -> anyhow::Result<Self>`. Both consumed by `view.rs` (Task 5) and `config.rs`/`saved_connections.rs`/`workspace.rs` (Task 6).

- [ ] **Step 1: Write the file with the IAC codec, backend, and both test modules**

Create `src/terminal/telnet.rs`:

```rust
//! Telnet backend: a raw `TcpStream` with minimal RFC 854 IAC (Interpret As
//! Command) handling — enough to open cleanly against real telnetd/network
//! gear without hanging on option negotiation, but not a full per-option
//! implementation (no NAWS/live-resize; see the design spec). No tokio: like
//! `LocalPty`, one tab = one socket, no multiplexed sub-protocol to justify an
//! async session thread (that's what SSH's shared connection + SFTP needs).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};

use anyhow::{Result, anyhow};

use crate::terminal::backend::PtyBackend;

/// Connection parameters. No credentials: telnet login happens interactively
/// in the terminal, same as typing at a raw `telnet` prompt.
#[derive(Clone, Debug)]
pub struct TelnetConfig {
    pub host: String,
    pub port: u16,
}

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const OPT_ECHO: u8 = 1;
const OPT_SUPPRESS_GO_AHEAD: u8 = 3;
const OPT_TERMINAL_TYPE: u8 = 24;

const TT_IS: u8 = 0;
const TT_SEND: u8 = 1;

const TERMINAL_TYPE_NAME: &[u8] = b"xterm-256color";

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum CodecState {
    #[default]
    Data,
    Iac,
    Will,
    Wont,
    Do,
    Dont,
    Sb,
    SbIac,
}

/// Parses IAC sequences out of a raw telnet byte stream, splitting each
/// `feed()` call's input into display bytes (forwarded to the terminal) and
/// reply bytes (sent back over the wire). Pure/stateful and independent of
/// any actual socket, so it's unit-testable without a network connection —
/// see `codec_tests` below.
#[derive(Default)]
struct TelnetCodec {
    state: CodecState,
    sb_buf: Vec<u8>,
}

impl TelnetCodec {
    /// Feed newly-received bytes; returns `(display_bytes, reply_bytes)`.
    fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut display = Vec::new();
        let mut reply = Vec::new();
        for &b in input {
            match self.state {
                CodecState::Data => {
                    if b == IAC {
                        self.state = CodecState::Iac;
                    } else {
                        display.push(b);
                    }
                }
                CodecState::Iac => match b {
                    IAC => {
                        display.push(0xFF);
                        self.state = CodecState::Data;
                    }
                    WILL => self.state = CodecState::Will,
                    WONT => self.state = CodecState::Wont,
                    DO => self.state = CodecState::Do,
                    DONT => self.state = CodecState::Dont,
                    SB => {
                        self.sb_buf.clear();
                        self.state = CodecState::Sb;
                    }
                    _ => self.state = CodecState::Data,
                },
                CodecState::Will => {
                    if b == OPT_SUPPRESS_GO_AHEAD || b == OPT_ECHO {
                        reply.extend_from_slice(&[IAC, DO, b]);
                    } else {
                        reply.extend_from_slice(&[IAC, DONT, b]);
                    }
                    self.state = CodecState::Data;
                }
                CodecState::Wont => {
                    reply.extend_from_slice(&[IAC, DONT, b]);
                    self.state = CodecState::Data;
                }
                CodecState::Do => {
                    if b == OPT_TERMINAL_TYPE || b == OPT_SUPPRESS_GO_AHEAD {
                        reply.extend_from_slice(&[IAC, WILL, b]);
                    } else {
                        reply.extend_from_slice(&[IAC, WONT, b]);
                    }
                    self.state = CodecState::Data;
                }
                CodecState::Dont => {
                    reply.extend_from_slice(&[IAC, WONT, b]);
                    self.state = CodecState::Data;
                }
                CodecState::Sb => {
                    if b == IAC {
                        self.state = CodecState::SbIac;
                    } else {
                        self.sb_buf.push(b);
                    }
                }
                CodecState::SbIac => {
                    if b == SE {
                        if self.sb_buf.first() == Some(&OPT_TERMINAL_TYPE)
                            && self.sb_buf.get(1) == Some(&TT_SEND)
                        {
                            reply.extend_from_slice(&[IAC, SB, OPT_TERMINAL_TYPE, TT_IS]);
                            reply.extend_from_slice(TERMINAL_TYPE_NAME);
                            reply.extend_from_slice(&[IAC, SE]);
                        }
                        self.state = CodecState::Data;
                    } else if b == IAC {
                        self.sb_buf.push(0xFF);
                        self.state = CodecState::Sb;
                    } else {
                        // Malformed (bare IAC inside SB not followed by SE or
                        // another IAC): resume collecting, be lenient.
                        self.sb_buf.push(b);
                        self.state = CodecState::Sb;
                    }
                }
            }
        }
        (display, reply)
    }
}

/// Escape `0xFF` bytes in outgoing data as `IAC IAC`, per RFC 854 — otherwise
/// a literal 0xFF byte the user types or pastes would be misparsed by the
/// remote as the start of a command sequence.
fn escape_iac(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        out.push(b);
        if b == IAC {
            out.push(IAC);
        }
    }
    out
}

/// Raw-TCP telnet session. One tab = one socket (see module doc).
pub struct TelnetBackend {
    write_tx: flume::Sender<Vec<u8>>,
    /// Kept only so `Drop` can `shutdown()` the socket, which unblocks the
    /// reader thread's blocking `read()` (mirrors `LocalPty` dropping
    /// `pair.slave` to trigger PTY EOF).
    stream: TcpStream,
}

impl TelnetBackend {
    /// Connect and spawn the reader/writer threads. Blocks until the TCP
    /// connect succeeds or fails, so callers can surface errors immediately
    /// (matches `SshSession::connect`'s synchronous-connect contract).
    pub fn connect(config: TelnetConfig, bytes_tx: flume::Sender<Vec<u8>>) -> Result<Self> {
        let stream = TcpStream::connect((config.host.as_str(), config.port))
            .map_err(|e| anyhow!("connect to {}:{} failed: {e}", config.host, config.port))?;
        stream.set_nodelay(true).ok();

        let mut reader = stream
            .try_clone()
            .map_err(|e| anyhow!("clone telnet socket for reader: {e}"))?;
        let mut negotiation_writer = stream
            .try_clone()
            .map_err(|e| anyhow!("clone telnet socket for negotiation replies: {e}"))?;

        std::thread::Builder::new()
            .name("caracal-telnet-reader".into())
            .spawn(move || {
                let mut codec = TelnetCodec::default();
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let (display, reply) = codec.feed(&buf[..n]);
                            if !reply.is_empty() && negotiation_writer.write_all(&reply).is_err() {
                                break;
                            }
                            if !display.is_empty() && bytes_tx.send(display).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })?;

        let (write_tx, write_rx) = flume::unbounded::<Vec<u8>>();
        let mut writer = stream
            .try_clone()
            .map_err(|e| anyhow!("clone telnet socket for writer: {e}"))?;
        std::thread::Builder::new()
            .name("caracal-telnet-writer".into())
            .spawn(move || {
                while let Ok(bytes) = write_rx.recv() {
                    let escaped = escape_iac(&bytes);
                    if writer.write_all(&escaped).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            })?;

        Ok(Self { write_tx, stream })
    }
}

impl PtyBackend for TelnetBackend {
    fn write(&self, bytes: &[u8]) {
        let _ = self.write_tx.send(bytes.to_vec());
    }

    /// No-op: NAWS (live window-resize) is out of scope for the "basic
    /// negotiation" tier this backend implements (see design spec §2).
    fn resize(&self, _cols: u16, _rows: u16) {}
}

impl Drop for TelnetBackend {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod codec_tests {
    use super::*;

    #[test]
    fn plain_data_passes_through() {
        let mut codec = TelnetCodec::default();
        let (display, reply) = codec.feed(b"hello");
        assert_eq!(display, b"hello");
        assert!(reply.is_empty());
    }

    #[test]
    fn escaped_iac_becomes_literal_0xff() {
        let mut codec = TelnetCodec::default();
        let (display, reply) = codec.feed(&[b'a', IAC, IAC, b'b']);
        assert_eq!(display, vec![b'a', 0xFFu8, b'b']);
        assert!(reply.is_empty());
    }

    #[test]
    fn will_suppress_go_ahead_replies_do() {
        let mut codec = TelnetCodec::default();
        let (display, reply) = codec.feed(&[IAC, WILL, OPT_SUPPRESS_GO_AHEAD]);
        assert!(display.is_empty());
        assert_eq!(reply, vec![IAC, DO, OPT_SUPPRESS_GO_AHEAD]);
    }

    #[test]
    fn will_unknown_option_replies_dont() {
        let mut codec = TelnetCodec::default();
        let (_, reply) = codec.feed(&[IAC, WILL, 99]);
        assert_eq!(reply, vec![IAC, DONT, 99]);
    }

    #[test]
    fn do_terminal_type_replies_will() {
        let mut codec = TelnetCodec::default();
        let (_, reply) = codec.feed(&[IAC, DO, OPT_TERMINAL_TYPE]);
        assert_eq!(reply, vec![IAC, WILL, OPT_TERMINAL_TYPE]);
    }

    #[test]
    fn do_unknown_option_replies_wont() {
        let mut codec = TelnetCodec::default();
        let (_, reply) = codec.feed(&[IAC, DO, 99]);
        assert_eq!(reply, vec![IAC, WONT, 99]);
    }

    #[test]
    fn terminal_type_subnegotiation_replies_with_name() {
        let mut codec = TelnetCodec::default();
        let input = [IAC, SB, OPT_TERMINAL_TYPE, TT_SEND, IAC, SE];
        let (display, reply) = codec.feed(&input);
        assert!(display.is_empty());
        let mut expected = vec![IAC, SB, OPT_TERMINAL_TYPE, TT_IS];
        expected.extend_from_slice(TERMINAL_TYPE_NAME);
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(reply, expected);
    }

    #[test]
    fn unrecognized_subnegotiation_is_discarded_without_reply() {
        let mut codec = TelnetCodec::default();
        let input = [IAC, SB, 39, 1, 2, 3, IAC, SE, b'x'];
        let (display, reply) = codec.feed(&input);
        assert_eq!(display, b"x");
        assert!(reply.is_empty());
    }

    #[test]
    fn feed_can_be_called_incrementally_across_a_split_sequence() {
        let mut codec = TelnetCodec::default();
        let (d1, r1) = codec.feed(&[IAC]);
        assert!(d1.is_empty() && r1.is_empty());
        let (d2, r2) = codec.feed(&[WILL]);
        assert!(d2.is_empty() && r2.is_empty());
        let (d3, r3) = codec.feed(&[OPT_ECHO]);
        assert!(d3.is_empty());
        assert_eq!(r3, vec![IAC, DO, OPT_ECHO]);
    }

    #[test]
    fn escape_iac_doubles_0xff_bytes() {
        assert_eq!(escape_iac(&[0xFF, b'a']), vec![0xFF, 0xFF, b'a']);
        assert_eq!(escape_iac(b"plain"), b"plain".to_vec());
    }
}

#[cfg(test)]
mod backend_tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Duration;

    /// Full round-trip against a local fake telnet server: verifies
    /// `TelnetBackend::connect` actually negotiates, forwards display bytes,
    /// escapes outgoing data, and shuts the socket down on drop.
    #[test]
    fn connect_negotiates_and_forwards_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            sock.set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set_read_timeout");

            // Server offers SUPPRESS_GO_AHEAD; expect the client to accept it.
            sock.write_all(&[IAC, WILL, OPT_SUPPRESS_GO_AHEAD]).unwrap();
            let mut reply = [0u8; 3];
            sock.read_exact(&mut reply).expect("read negotiation reply");
            assert_eq!(reply, [IAC, DO, OPT_SUPPRESS_GO_AHEAD]);

            // Server sends plain text.
            sock.write_all(b"welcome\r\n").unwrap();

            // Server reads back whatever the client writes (echo of a typed
            // 0xFF byte, to prove outgoing IAC-escaping happens).
            let mut echoed = [0u8; 4];
            sock.read_exact(&mut echoed).expect("read escaped byte");
            assert_eq!(echoed, [0xFF, 0xFF, b'h', b'i']);

            // Keep the connection open until the client drops it, then
            // confirm we observe EOF (proves TelnetBackend::drop() shuts the
            // socket down). Bounded by the read timeout above so a bug here
            // fails the test instead of hanging it.
            let mut buf = [0u8; 1];
            match sock.read(&mut buf) {
                Ok(0) => {}
                other => panic!("expected EOF after TelnetBackend was dropped, got {other:?}"),
            }
        });

        let (bytes_tx, bytes_rx) = flume::unbounded::<Vec<u8>>();
        let backend = TelnetBackend::connect(
            TelnetConfig {
                host: "127.0.0.1".into(),
                port: addr.port(),
            },
            bytes_tx,
        )
        .expect("connect");

        let received = bytes_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("expected display bytes from server");
        assert_eq!(received, b"welcome\r\n");

        backend.write(&[0xFF, b'h', b'i']);

        // Give the writer thread a moment to flush, then drop to trigger
        // the shutdown-on-drop the server thread is waiting to observe.
        std::thread::sleep(Duration::from_millis(100));
        drop(backend);

        server.join().expect("server thread panicked");
    }
}
```

- [ ] **Step 2: Register the module**

In `src/terminal/mod.rs`, change:

```rust

pub mod backend;
pub mod batching;
pub mod bridge;
pub mod grid_snapshot;
pub mod keymap;
pub mod model;
pub mod render;
pub mod scrollback;
pub mod selection;
pub mod ssh;
pub mod view;
```

to:

```rust

pub mod backend;
pub mod batching;
pub mod bridge;
pub mod grid_snapshot;
pub mod keymap;
pub mod model;
pub mod render;
pub mod scrollback;
pub mod selection;
pub mod serial;
pub mod ssh;
pub mod telnet;
pub mod view;
```

(`serial` doesn't exist yet — Task 3 creates it. Adding both `mod` lines now means `cargo build` will fail until Task 3 lands `serial.rs`; that's fine, this task's own verification step is `cargo test -p caracal telnet::`, scoped to the new module, not a full build. Task 3 restores a full green build.)

- [ ] **Step 3: Run the new tests**

Run: `cargo test --bin caracal telnet::`
Expected: all `codec_tests::*` and `backend_tests::connect_negotiates_and_forwards_bytes` pass. (This will only succeed once `mod serial;` also resolves — if Task 3 hasn't landed yet in your working tree, temporarily comment out the `pub mod serial;` line to run this step in isolation, then restore it before committing, OR do Steps 2–3 of this task and Task 3 back-to-back before running tests. Either order is fine; just don't commit Task 2 with a `mod serial;` line pointing at a nonexistent file.)

Given that constraint, the simplest path: **do Task 2 Step 1 (write telnet.rs) and Task 3 Step 1 (write serial.rs) before registering either module**, then do both `mod` registrations together, then run `cargo test --bin caracal telnet:: serial::` once for both. Treat Task 2's "module registration + test run" as deferred until Task 3's Step 1 is also done; commit both files together if that's easier than juggling a half-registered module. The important invariant is: **never commit a `mod` line whose file doesn't exist yet.**

- [ ] **Step 4: Commit (once `serial.rs` also exists — see Task 3)**

```bash
git add src/terminal/telnet.rs src/terminal/serial.rs src/terminal/mod.rs src/terminal/backend.rs
git commit -m "feat: add Telnet and serial-port PtyBackend implementations"
```

(This commit intentionally bundles Task 2 and Task 3 — see Task 3's note on why.)

---

### Task 3: Serial backend (`src/terminal/serial.rs`)

**Files:**
- Create: `src/terminal/serial.rs`
- Modify: `src/terminal/backend.rs` (doc comment)

**Interfaces:**
- Consumes: `crate::terminal::backend::PtyBackend`.
- Produces: `pub struct SerialConfig { pub port: String, pub baud_rate: u32, pub data_bits: u8, pub parity: String, pub stop_bits: u8, pub flow_control: String }`, `pub struct SerialBackend` implementing `PtyBackend`, `SerialBackend::open(config: SerialConfig, bytes_tx: flume::Sender<Vec<u8>>) -> anyhow::Result<Self>`, `pub fn list_ports() -> Vec<String>`. All consumed by `view.rs` (Task 5) and `config.rs`/`saved_connections.rs`/`workspace.rs` (Task 6).

**Note on why this task is bundled with Task 2 at commit time:** `src/terminal/mod.rs` needs both `pub mod telnet;` and `pub mod serial;` added before the crate builds again (Task 2 alone leaves a dangling `mod serial;` if done strictly in isolation). Write both files first, register both modules, then run tests and commit once — that's a completely normal "one logical change, one commit" outcome; it doesn't mean skipping either task's own test coverage.

- [ ] **Step 1: Write the file with mapping helpers, backend, and tests**

Create `src/terminal/serial.rs`:

```rust
//! Serial-port backend: a blocking `serialport::SerialPort`, split into
//! reader/writer threads exactly like `LocalPty` in `backend.rs`. No live
//! resize (physical ports have no notion of size) and no async runtime —
//! same one-tab-one-handle shape as `telnet.rs`.

use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::terminal::backend::PtyBackend;

/// Connection parameters for opening a serial port.
#[derive(Clone, Debug)]
pub struct SerialConfig {
    /// Device path, e.g. `/dev/ttyUSB0` (Unix) or `COM3` (Windows).
    pub port: String,
    pub baud_rate: u32,
    /// 5/6/7/8.
    pub data_bits: u8,
    /// `"none" | "odd" | "even"`.
    pub parity: String,
    /// 1/2.
    pub stop_bits: u8,
    /// `"none" | "software" | "hardware"`.
    pub flow_control: String,
}

fn map_data_bits(n: u8) -> serialport::DataBits {
    match n {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        _ => serialport::DataBits::Eight,
    }
}

fn map_parity(s: &str) -> serialport::Parity {
    match s {
        "odd" => serialport::Parity::Odd,
        "even" => serialport::Parity::Even,
        _ => serialport::Parity::None,
    }
}

fn map_stop_bits(n: u8) -> serialport::StopBits {
    match n {
        2 => serialport::StopBits::Two,
        _ => serialport::StopBits::One,
    }
}

fn map_flow_control(s: &str) -> serialport::FlowControl {
    match s {
        "software" => serialport::FlowControl::Software,
        "hardware" => serialport::FlowControl::Hardware,
        _ => serialport::FlowControl::None,
    }
}

/// Detected ports for the new-connection form's picker. Never errors (an
/// empty `Vec` on detection failure just means the form's list stays empty
/// and the user falls back to typing the path manually).
pub fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

/// A physical/virtual serial port opened via the `serialport` crate.
pub struct SerialBackend {
    write_tx: flume::Sender<Vec<u8>>,
}

impl SerialBackend {
    /// Open the port and spawn the reader/writer threads. Blocks until the
    /// port is open (or fails), matching `TelnetBackend::connect`'s and
    /// `SshSession::connect`'s synchronous-connect contract.
    pub fn open(config: SerialConfig, bytes_tx: flume::Sender<Vec<u8>>) -> Result<Self> {
        let port_for_error = config.port.clone();
        let port = serialport::new(&config.port, config.baud_rate)
            .data_bits(map_data_bits(config.data_bits))
            .parity(map_parity(&config.parity))
            .stop_bits(map_stop_bits(config.stop_bits))
            .flow_control(map_flow_control(&config.flow_control))
            // A finite read timeout, not a truly blocking read: the reader
            // loop below treats a timeout as "no data yet" and keeps
            // polling, so the thread also notices promptly when the write
            // channel closes (tab closed) instead of blocking forever on a
            // port that never sends anything.
            .timeout(Duration::from_millis(200))
            .open()
            .map_err(|e| anyhow!("open serial port {port_for_error:?}: {e}"))?;

        let mut reader = port
            .try_clone()
            .map_err(|e| anyhow!("clone serial port for reader: {e}"))?;
        std::thread::Builder::new()
            .name("caracal-serial-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => continue,
                        Ok(n) => {
                            if bytes_tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                        Err(_) => break,
                    }
                }
            })?;

        let (write_tx, write_rx) = flume::unbounded::<Vec<u8>>();
        let mut writer = port;
        std::thread::Builder::new()
            .name("caracal-serial-writer".into())
            .spawn(move || {
                while let Ok(bytes) = write_rx.recv() {
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            })?;

        Ok(Self { write_tx })
    }
}

impl PtyBackend for SerialBackend {
    fn write(&self, bytes: &[u8]) {
        let _ = self.write_tx.send(bytes.to_vec());
    }

    /// No-op: a physical serial port has no notion of terminal size.
    fn resize(&self, _cols: u16, _rows: u16) {}
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    #[test]
    fn data_bits_map_known_values_and_default_to_eight() {
        assert!(matches!(map_data_bits(5), serialport::DataBits::Five));
        assert!(matches!(map_data_bits(6), serialport::DataBits::Six));
        assert!(matches!(map_data_bits(7), serialport::DataBits::Seven));
        assert!(matches!(map_data_bits(8), serialport::DataBits::Eight));
        assert!(matches!(map_data_bits(0), serialport::DataBits::Eight));
    }

    #[test]
    fn parity_maps_known_values_and_defaults_to_none() {
        assert!(matches!(map_parity("odd"), serialport::Parity::Odd));
        assert!(matches!(map_parity("even"), serialport::Parity::Even));
        assert!(matches!(map_parity("none"), serialport::Parity::None));
        assert!(matches!(map_parity("bogus"), serialport::Parity::None));
    }

    #[test]
    fn stop_bits_map_known_values_and_default_to_one() {
        assert!(matches!(map_stop_bits(1), serialport::StopBits::One));
        assert!(matches!(map_stop_bits(2), serialport::StopBits::Two));
        assert!(matches!(map_stop_bits(0), serialport::StopBits::One));
    }

    #[test]
    fn flow_control_maps_known_values_and_defaults_to_none() {
        assert!(matches!(
            map_flow_control("software"),
            serialport::FlowControl::Software
        ));
        assert!(matches!(
            map_flow_control("hardware"),
            serialport::FlowControl::Hardware
        ));
        assert!(matches!(map_flow_control("none"), serialport::FlowControl::None));
        assert!(matches!(map_flow_control("bogus"), serialport::FlowControl::None));
    }

    #[test]
    fn list_ports_does_not_panic() {
        // No hardware is guaranteed present in a test/CI environment; this
        // only asserts detection itself doesn't panic and always returns a
        // (possibly empty) Vec.
        let _ = list_ports();
    }

    #[test]
    fn open_reports_a_descriptive_error_for_a_nonexistent_port() {
        let (tx, _rx) = flume::unbounded::<Vec<u8>>();
        let err = SerialBackend::open(
            SerialConfig {
                port: "/dev/__caracal_test_nonexistent__".into(),
                baud_rate: 115_200,
                data_bits: 8,
                parity: "none".into(),
                stop_bits: 1,
                flow_control: "none".into(),
            },
            tx,
        )
        .expect_err("nonexistent port must fail to open");
        assert!(err.to_string().contains("__caracal_test_nonexistent__"));
    }
}
```

- [ ] **Step 2: Refresh `backend.rs`'s doc comment**

Now that all four backends exist, update the stale "Phase 1 only implements `LocalPty`" note. In `src/terminal/backend.rs`, change:

```rust
//! Backend abstraction: every transport (local PTY, SSH, serial) implements the
//! same `PtyBackend`. `TerminalView` is agnostic to which one it talks to
//! (CLAUDE.md §2). Phase 1 only implements `LocalPty`.
```

to:

```rust
//! Backend abstraction: every transport (local PTY, SSH, Telnet, serial)
//! implements the same `PtyBackend`. `TerminalView` is agnostic to which one
//! it talks to — see the per-backend modules `ssh.rs`, `telnet.rs`,
//! `serial.rs`. This file implements `LocalPty`.
```

- [ ] **Step 3: `mod.rs` already updated in Task 2 Step 2** — confirm both lines are present:

```rust
pub mod serial;
pub mod ssh;
pub mod telnet;
```

- [ ] **Step 4: Build and run both new modules' tests**

Run: `cargo build`
Expected: succeeds (full crate, both new modules now resolve).

Run: `cargo test --bin caracal telnet:: serial::`
Expected: all tests from `telnet.rs` (Task 2) and `serial.rs` (this task) pass — 9 codec tests + 1 backend test + 6 mapping/open tests, 16 total.

- [ ] **Step 5: Commit (bundles Task 2 + Task 3, per the note above)**

```bash
git add src/terminal/telnet.rs src/terminal/serial.rs src/terminal/mod.rs src/terminal/backend.rs
git commit -m "feat: add Telnet and serial-port PtyBackend implementations"
```

---

### Task 4: Icons (`src/panels/icons.rs`)

**Files:**
- Modify: `src/panels/icons.rs`

**Interfaces:**
- Produces: `AppIcon::Telnet`, `AppIcon::SerialPort` variants, both mapped to upstream `IconName`s. Consumed by `config.rs`'s `resolve_icon()` (Task 6).

- [ ] **Step 1: Add `Debug` to `AppIcon`'s derive (needed for `assert_eq!` in Task 6's config.rs tests)**

In `src/panels/icons.rs`, change:

```rust
/// 活动栏 / 面板里用到的语义图标。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppIcon {
```

to:

```rust
/// 活动栏 / 面板里用到的语义图标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppIcon {
```

- [ ] **Step 2: Add the two new variants**

Change:

```rust
    Pencil,
    Sort,
    ChevronRight,
    ChevronDown,
}
```

to:

```rust
    Pencil,
    Sort,
    ChevronRight,
    ChevronDown,
    Telnet,
    SerialPort,
}
```

- [ ] **Step 3: Map them to upstream icons**

Change:

```rust
            ChevronRight => IconName::ChevronRight,
            ChevronDown => IconName::ChevronDown,
            // Upload / Download / Pencil / SavedConnections are handled by custom SVG in
            // `icon()`, not reachable here.
            Upload | Download | Pencil | SavedConnections => unreachable!(),
```

to:

```rust
            ChevronRight => IconName::ChevronRight,
            ChevronDown => IconName::ChevronDown,
            Telnet => IconName::Network,
            SerialPort => IconName::Cpu,
            // Upload / Download / Pencil / SavedConnections are handled by custom SVG in
            // `icon()`, not reachable here.
            Upload | Download | Pencil | SavedConnections => unreachable!(),
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: succeeds (both variants are unused so far — `cargo build` warns at most about dead code on `Telnet`/`SerialPort` not yet being constructed; that's expected until Task 6 wires `resolve_icon()`, and dead-variant warnings don't fail the build).

- [ ] **Step 5: Commit**

```bash
git add src/panels/icons.rs
git commit -m "feat: add Telnet/SerialPort connection-type icons"
```

---

### Task 5: `TerminalView` constructors (`src/terminal/view.rs`)

**Files:**
- Modify: `src/terminal/view.rs`

**Interfaces:**
- Consumes: `TelnetBackend::connect`, `TelnetConfig` (Task 2); `SerialBackend::open`, `SerialConfig` (Task 3); `TerminalView::with_backend` (existing, unchanged).
- Produces: `TerminalView::new_telnet(window, cx, TelnetConfig) -> Self`, `TerminalView::new_serial(window, cx, SerialConfig) -> Self`. Consumed by `workspace.rs` (Task 6).

- [ ] **Step 1: Add imports**

In `src/terminal/view.rs`, change:

```rust
use crate::terminal::backend::{LocalPty, PtyBackend};
use crate::terminal::bridge::{run_drain, run_feeder};
use crate::terminal::keymap::{PastePayload, encode_key, encode_paste};
use crate::terminal::model::{SharedTerm, new_term};
use crate::terminal::render::terminal_canvas;
use crate::terminal::scrollback;
use crate::terminal::selection;
use crate::terminal::ssh::SshSession;
```

to:

```rust
use crate::terminal::backend::{LocalPty, PtyBackend};
use crate::terminal::bridge::{run_drain, run_feeder};
use crate::terminal::keymap::{PastePayload, encode_key, encode_paste};
use crate::terminal::model::{SharedTerm, new_term};
use crate::terminal::render::terminal_canvas;
use crate::terminal::scrollback;
use crate::terminal::selection;
use crate::terminal::serial::{SerialBackend, SerialConfig};
use crate::terminal::ssh::SshSession;
use crate::terminal::telnet::{TelnetBackend, TelnetConfig};
```

- [ ] **Step 2: Add the two constructors after `new_ssh_shell`**

Find this block (existing code, unchanged so far):

```rust
    /// A terminal backed by a shell channel on an already-connected [`SshSession`]
    /// (shared with the SFTP panel — one connection per host, CLAUDE.md §2).
    pub fn new_ssh_shell(
        window: &mut Window,
        cx: &mut Context<Self>,
        session: Arc<SshSession>,
    ) -> Self {
        Self::with_backend(window, cx, move |cols, rows, bytes_tx| {
            session.open_shell(cols, rows, bytes_tx)
        })
    }
```

Add immediately after it:

```rust

    /// A terminal backed by a raw Telnet connection (`TelnetBackend`). Each
    /// tab dials its own socket — unlike SSH, telnet has no SFTP-style
    /// second channel to justify a shared connection.
    pub fn new_telnet(window: &mut Window, cx: &mut Context<Self>, config: TelnetConfig) -> Self {
        Self::with_backend(window, cx, move |_cols, _rows, bytes_tx| {
            Arc::new(TelnetBackend::connect(config, bytes_tx).expect("telnet connect failed"))
        })
    }

    /// A terminal backed by a serial port (`SerialBackend`).
    pub fn new_serial(window: &mut Window, cx: &mut Context<Self>, config: SerialConfig) -> Self {
        Self::with_backend(window, cx, move |_cols, _rows, bytes_tx| {
            Arc::new(SerialBackend::open(config, bytes_tx).expect("serial open failed"))
        })
    }
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: succeeds. (`new_telnet`/`new_serial` are unused so far — `dead_code` warning at most, not an error, until Task 6 calls them from `workspace.rs`.)

- [ ] **Step 4: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: add TerminalView::new_telnet and ::new_serial constructors"
```

---

### Task 6: Wire it all together — data model, events, UI form, workspace

**Why this is one task:** `ConnectionType` (in `config.rs`) is matched exhaustively inside `saved_connections.rs` (the form's field-set switch and `save_form`'s switch), and `SavedConnectionsEvent` (in `saved_connections.rs`) is matched exhaustively inside `workspace.rs`'s subscription handler. Updating any one of these three files without the other two leaves the crate in a non-compiling state — there is no smaller unit that both compiles and is independently reviewable. This task's steps are grouped into three phases (A: `config.rs`, B: `saved_connections.rs`, C: `workspace.rs`) purely for readability; **the crate will not build until all three phases are done**, so run the build/test steps only at the end of the task, not between phases.

**Files:**
- Modify: `src/config.rs`
- Modify: `src/panels/saved_connections.rs`
- Modify: `src/workspace.rs`

**Interfaces:**
- Consumes: `TelnetConfig`/`TelnetBackend` (Task 2), `SerialConfig`/`SerialBackend`/`list_ports()` (Task 3), `AppIcon::Telnet`/`AppIcon::SerialPort` (Task 4), `TerminalView::new_telnet`/`::new_serial` (Task 5).
- Produces: `ConnectionType::Telnet`/`::Serial`, `SavedConnection::to_telnet_config()`/`::to_serial_config()`, `SavedConnectionsEvent::OpenTelnet`/`::OpenSerial`, `Workspace::open_telnet`/`::open_serial`. Nothing further downstream consumes these — this is the last task before manual verification (Task 7).

#### Phase A: `src/config.rs`

- [ ] **Step 1: Replace the whole file**

The change touches every function in this file (imports, the enum, the struct, all four match-based methods, plus new tests) — replace the full file rather than patching piecemeal. Overwrite `src/config.rs` with:

```rust
//! Persisted app config: the list of saved connections shown in the
//! right-dock "已保存的连接" panel. Plain Rust — **no `gpui_component`** here
//! (CLAUDE.md §1 boundary); the panel calls [`load`]/[`save`].
//!
//! Stored at `$XDG_CONFIG_HOME/caracal/connections.toml` (else
//! `~/.config/caracal/connections.toml`).
//!
//! ⚠️ SECURITY / TODO: `password` is persisted in **plaintext**, matching the
//! current Phase-4 plaintext-password reality (see `SshConfig`). This is a known
//! limitation — a later phase should move secrets to the OS keyring.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::panels::icons::AppIcon;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::SshConfig;
use crate::terminal::telnet::TelnetConfig;

/// Connection type: SSH, local terminal, Telnet, or serial port.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionType {
    Ssh,
    Local,
    Telnet,
    Serial,
}

impl Default for ConnectionType {
    fn default() -> Self {
        ConnectionType::Ssh
    }
}

/// A group (folder) that contains connections or other groups.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedConnectionGroup {
    /// Unique identifier (UUID).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Parent group ID. `None` means this is a root-level group.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Sort order among siblings.
    #[serde(default)]
    pub sort_order: i32,
}

/// One saved connection entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedConnection {
    /// Display name (falls back to `user@host` if empty).
    #[serde(default)]
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub password: String,
    /// Group this connection belongs to. `None` means ungrouped (root level).
    #[serde(default)]
    pub group_id: Option<String>,
    /// Connection type (SSH, local terminal, Telnet, or serial port).
    #[serde(default)]
    pub conn_type: ConnectionType,
    /// User-selected icon name. `None` means auto-resolve from `conn_type`.
    #[serde(default)]
    pub icon: Option<String>,
    /// Shell path for local terminal connections.
    #[serde(default)]
    pub shell_path: Option<String>,
    /// Working directory for local terminal connections.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Device path, e.g. "/dev/ttyUSB0" or "COM3". Serial only.
    #[serde(default)]
    pub serial_port: Option<String>,
    /// Serial only. Defaults to 115200 if unset.
    #[serde(default)]
    pub baud_rate: Option<u32>,
    /// Serial only: 5/6/7/8. Defaults to 8 if unset.
    #[serde(default)]
    pub data_bits: Option<u8>,
    /// Serial only: "none" | "odd" | "even". Defaults to "none" if unset.
    #[serde(default)]
    pub parity: Option<String>,
    /// Serial only: 1 | 2. Defaults to 1 if unset.
    #[serde(default)]
    pub stop_bits: Option<u8>,
    /// Serial only: "none" | "software" | "hardware". Defaults to "none" if unset.
    #[serde(default)]
    pub flow_control: Option<String>,
    /// Optional description shown in tooltip.
    #[serde(default)]
    pub description: Option<String>,
}

fn default_port() -> u16 {
    22
}

impl SavedConnection {
    /// The connection parameters used to actually dial (see `workspace.rs`).
    pub fn to_ssh_config(&self) -> SshConfig {
        SshConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            password: self.password.clone(),
        }
    }

    /// Telnet connection parameters. No credentials: telnet login happens
    /// interactively in the terminal, same as typing at a raw `telnet` prompt.
    pub fn to_telnet_config(&self) -> TelnetConfig {
        TelnetConfig {
            host: self.host.clone(),
            port: self.port,
        }
    }

    /// Serial port parameters, applying the documented defaults (115200 8N1,
    /// no flow control) for any field that was never set.
    pub fn to_serial_config(&self) -> SerialConfig {
        SerialConfig {
            port: self.serial_port.clone().unwrap_or_default(),
            baud_rate: self.baud_rate.unwrap_or(115_200),
            data_bits: self.data_bits.unwrap_or(8),
            parity: self.parity.clone().unwrap_or_else(|| "none".to_string()),
            stop_bits: self.stop_bits.unwrap_or(1),
            flow_control: self
                .flow_control
                .clone()
                .unwrap_or_else(|| "none".to_string()),
        }
    }

    /// What to show as the row's primary label.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            match self.conn_type {
                ConnectionType::Ssh => format!("{}@{}", self.user, self.host),
                ConnectionType::Local => {
                    if let Some(ref shell) = self.shell_path {
                        shell.split('/').last().unwrap_or(shell).to_string()
                    } else {
                        "local".to_string()
                    }
                }
                ConnectionType::Telnet => format!("{}:{}", self.host, self.port),
                ConnectionType::Serial => {
                    if let Some(ref port) = self.serial_port {
                        port.split('/').last().unwrap_or(port).to_string()
                    } else {
                        "serial".to_string()
                    }
                }
            }
        } else {
            self.name.clone()
        }
    }

    /// Secondary/muted label shown below the name.
    pub fn subtitle(&self) -> String {
        match self.conn_type {
            ConnectionType::Ssh => format!("{}@{}:{}", self.user, self.host, self.port),
            ConnectionType::Local => {
                if let Some(ref wd) = self.working_dir {
                    wd.clone()
                } else if let Some(ref shell) = self.shell_path {
                    shell.clone()
                } else {
                    "local terminal".to_string()
                }
            }
            ConnectionType::Telnet => format!("{}:{}", self.host, self.port),
            ConnectionType::Serial => {
                let port = self.serial_port.as_deref().unwrap_or("?");
                let baud = self.baud_rate.unwrap_or(115_200);
                format!("{port} @ {baud}bps")
            }
        }
    }

    /// Resolve the icon for this connection.
    pub fn resolve_icon(&self) -> AppIcon {
        if let Some(ref icon_name) = self.icon {
            // Try to match user-specified icon name
            match icon_name.as_str() {
                "terminal" => return AppIcon::Terminal,
                "laptop" | "code" => return AppIcon::LocalTerminal,
                "server" | "harddrive" => return AppIcon::SavedConnections,
                "network" => return AppIcon::Network,
                "telnet" => return AppIcon::Telnet,
                "serial" | "cpu" => return AppIcon::SerialPort,
                _ => {}
            }
        }
        // Auto-resolve from connection type
        match self.conn_type {
            ConnectionType::Ssh => AppIcon::Terminal,
            ConnectionType::Local => AppIcon::LocalTerminal,
            ConnectionType::Telnet => AppIcon::Telnet,
            ConnectionType::Serial => AppIcon::SerialPort,
        }
    }

    /// Lines shown in the tooltip. Each line is (label, value).
    #[allow(dead_code)]
    pub fn tooltip_lines(&self) -> Vec<(String, String)> {
        let mut lines = Vec::new();
        match self.conn_type {
            ConnectionType::Ssh => {
                lines.push(("Host".to_string(), self.host.clone()));
                lines.push(("Port".to_string(), self.port.to_string()));
                lines.push(("User".to_string(), self.user.clone()));
            }
            ConnectionType::Local => {
                if let Some(ref shell) = self.shell_path {
                    lines.push(("Shell".to_string(), shell.clone()));
                }
                if let Some(ref wd) = self.working_dir {
                    lines.push(("Working Dir".to_string(), wd.clone()));
                }
            }
            ConnectionType::Telnet => {
                lines.push(("Host".to_string(), self.host.clone()));
                lines.push(("Port".to_string(), self.port.to_string()));
            }
            ConnectionType::Serial => {
                if let Some(ref port) = self.serial_port {
                    lines.push(("Port".to_string(), port.clone()));
                }
                lines.push((
                    "Baud".to_string(),
                    self.baud_rate.unwrap_or(115_200).to_string(),
                ));
            }
        }
        if let Some(ref desc) = self.description {
            if !desc.trim().is_empty() {
                lines.push(("Description".to_string(), desc.clone()));
            }
        }
        lines
    }
}

/// The whole persisted config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
    #[serde(default)]
    pub groups: Vec<SavedConnectionGroup>,
}

/// `~/.config/caracal/connections.toml`.
pub fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("caracal").join("connections.toml")
}

/// Load the config. Missing file → default (empty). A parse error is logged and
/// also yields the default, so a corrupt file never crashes startup.
pub fn load() -> AppConfig {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return AppConfig::default(),
    };
    match toml::from_str(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            AppConfig::default()
        }
    }
}

/// Persist the config, creating the parent directory if needed.
pub fn save(cfg: &AppConfig) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg)?;
    std::fs::write(&path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_connection(conn_type: ConnectionType) -> SavedConnection {
        SavedConnection {
            name: String::new(),
            host: "example.com".to_string(),
            port: 23,
            user: String::new(),
            password: String::new(),
            group_id: None,
            conn_type,
            icon: None,
            shell_path: None,
            working_dir: None,
            serial_port: None,
            baud_rate: None,
            data_bits: None,
            parity: None,
            stop_bits: None,
            flow_control: None,
            description: None,
        }
    }

    #[test]
    fn telnet_display_name_and_subtitle_are_host_port() {
        let conn = base_connection(ConnectionType::Telnet);
        assert_eq!(conn.display_name(), "example.com:23");
        assert_eq!(conn.subtitle(), "example.com:23");
    }

    #[test]
    fn telnet_to_telnet_config_carries_host_and_port_only() {
        let conn = base_connection(ConnectionType::Telnet);
        let cfg = conn.to_telnet_config();
        assert_eq!(cfg.host, "example.com");
        assert_eq!(cfg.port, 23);
    }

    #[test]
    fn serial_display_name_uses_last_path_component() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        assert_eq!(conn.display_name(), "ttyUSB0");
    }

    #[test]
    fn serial_display_name_falls_back_when_port_unset() {
        let conn = base_connection(ConnectionType::Serial);
        assert_eq!(conn.display_name(), "serial");
    }

    #[test]
    fn serial_subtitle_shows_port_and_baud() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        conn.baud_rate = Some(9600);
        assert_eq!(conn.subtitle(), "/dev/ttyUSB0 @ 9600bps");
    }

    #[test]
    fn to_serial_config_applies_documented_defaults() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        let cfg = conn.to_serial_config();
        assert_eq!(cfg.port, "/dev/ttyUSB0");
        assert_eq!(cfg.baud_rate, 115_200);
        assert_eq!(cfg.data_bits, 8);
        assert_eq!(cfg.parity, "none");
        assert_eq!(cfg.stop_bits, 1);
        assert_eq!(cfg.flow_control, "none");
    }

    #[test]
    fn to_serial_config_honors_explicit_values() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        conn.baud_rate = Some(9600);
        conn.data_bits = Some(7);
        conn.parity = Some("even".to_string());
        conn.stop_bits = Some(2);
        conn.flow_control = Some("hardware".to_string());
        let cfg = conn.to_serial_config();
        assert_eq!(cfg.baud_rate, 9600);
        assert_eq!(cfg.data_bits, 7);
        assert_eq!(cfg.parity, "even");
        assert_eq!(cfg.stop_bits, 2);
        assert_eq!(cfg.flow_control, "hardware");
    }

    #[test]
    fn resolve_icon_auto_resolves_new_connection_types() {
        assert_eq!(
            base_connection(ConnectionType::Telnet).resolve_icon(),
            AppIcon::Telnet
        );
        assert_eq!(
            base_connection(ConnectionType::Serial).resolve_icon(),
            AppIcon::SerialPort
        );
    }

    #[test]
    fn old_config_without_new_fields_still_deserializes() {
        // Simulates a `connections.toml` written before this change: no
        // serial_port/baud_rate/etc keys at all.
        let toml_text = r#"
            [[connections]]
            host = "old.example.com"
            user = "root"
            conn_type = "ssh"
        "#;
        let cfg: AppConfig =
            toml::from_str(toml_text).expect("old-format config must still parse");
        assert_eq!(cfg.connections.len(), 1);
        assert_eq!(cfg.connections[0].serial_port, None);
        assert_eq!(cfg.connections[0].baud_rate, None);
    }
}
```

#### Phase B: `src/panels/saved_connections.rs`

- [ ] **Step 2: Update imports**

Change:

```rust
use gpui::{
    Action, App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::ContextMenuExt;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, StyledExt, WindowExt};
use serde::Deserialize;

use crate::panels::icons::{AppIcon, icon};

use crate::config::{self, AppConfig, ConnectionType, SavedConnection, SavedConnectionGroup};
use crate::terminal::ssh::SshConfig;
```

to:

```rust
use gpui::{
    Action, App, AppContext, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::ContextMenuExt;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, StyledExt, WindowExt};
use serde::Deserialize;

use crate::panels::icons::{AppIcon, icon};

use crate::config::{self, AppConfig, ConnectionType, SavedConnection, SavedConnectionGroup};
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::SshConfig;
use crate::terminal::telnet::TelnetConfig;
```

- [ ] **Step 3: Extend `SavedConnectionsEvent` and add the `open_event` helper**

Change:

```rust
/// Emitted when the user picks a saved connection to open.
pub enum SavedConnectionsEvent {
    /// Open an SSH shell terminal.
    Open(SshConfig),
    /// Open an SFTP browser (routed to the bottom "SFTP" dock).
    #[allow(dead_code)]
    OpenSftp(SshConfig),
    /// Open a local terminal.
    OpenLocal(String, String),
}
```

to:

```rust
/// Emitted when the user picks a saved connection to open.
pub enum SavedConnectionsEvent {
    /// Open an SSH shell terminal.
    Open(SshConfig),
    /// Open an SFTP browser (routed to the bottom "SFTP" dock).
    #[allow(dead_code)]
    OpenSftp(SshConfig),
    /// Open a local terminal.
    OpenLocal(String, String),
    /// Open a raw Telnet terminal.
    OpenTelnet(TelnetConfig),
    /// Open a serial-port terminal.
    OpenSerial(SerialConfig),
}

/// Build the event that opens `conn`, dispatching on its connection type.
/// Shared by the row's double-click handler and the context menu's "打开"
/// action so the two call sites can't drift.
fn open_event(conn: &SavedConnection) -> SavedConnectionsEvent {
    match conn.conn_type {
        ConnectionType::Ssh => SavedConnectionsEvent::Open(conn.to_ssh_config()),
        ConnectionType::Local => SavedConnectionsEvent::OpenLocal(
            conn.shell_path.clone().unwrap_or_default(),
            conn.working_dir.clone().unwrap_or_default(),
        ),
        ConnectionType::Telnet => SavedConnectionsEvent::OpenTelnet(conn.to_telnet_config()),
        ConnectionType::Serial => SavedConnectionsEvent::OpenSerial(conn.to_serial_config()),
    }
}
```

- [ ] **Step 4: Simplify `on_action_open_connection` to use the helper**

Change:

```rust
    fn on_action_open_connection(
        &mut self,
        action: &OpenConnection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.connections.get(action.ix) else {
            return;
        };
        match conn.conn_type {
            ConnectionType::Ssh => cx.emit(SavedConnectionsEvent::Open(conn.to_ssh_config())),
            ConnectionType::Local => cx.emit(SavedConnectionsEvent::OpenLocal(
                conn.shell_path.clone().unwrap_or_default(),
                conn.working_dir.clone().unwrap_or_default(),
            )),
        }
    }
```

to:

```rust
    fn on_action_open_connection(
        &mut self,
        action: &OpenConnection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.connections.get(action.ix) else {
            return;
        };
        cx.emit(open_event(conn));
    }
```

- [ ] **Step 5: Add `data_bits`/`parity`/`stop_bits`/`flow_control`/`detected_ports` to `ConnForm`, plus `serial_port`/`baud_rate` inputs**

Change:

```rust
/// The inline "add connection" form.
struct ConnForm {
    name: Entity<InputState>,
    conn_type: ConnectionType,
    group_id: Option<String>,
    // SSH fields
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    // Local fields
    shell_path: Entity<InputState>,
    working_dir: Entity<InputState>,
    /// `Some(ix)` when this form is editing an existing connection in place
    /// (opened via the Edit hover icon or context-menu item); `None` when
    /// adding a brand new connection.
    edit_ix: Option<usize>,
    /// Kept alive so the `InputEvent::PressEnter` subscriptions (submit on
    /// Enter from any field, see `watch_enter_to_submit`) keep firing —
    /// dropping a `Subscription` cancels it.
    _enter_subs: Vec<Subscription>,
}
```

to:

```rust
/// The inline "add connection" form.
struct ConnForm {
    name: Entity<InputState>,
    conn_type: ConnectionType,
    group_id: Option<String>,
    // SSH fields (host/port doubly used by Telnet, minus user/password)
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    // Local fields
    shell_path: Entity<InputState>,
    working_dir: Entity<InputState>,
    // Serial fields
    serial_port: Entity<InputState>,
    baud_rate: Entity<InputState>,
    data_bits: u8,
    parity: String,
    stop_bits: u8,
    flow_control: String,
    /// Populated by the "刷新" button (`serial::list_ports()`); empty until
    /// pressed.
    detected_ports: Vec<String>,
    /// `Some(ix)` when this form is editing an existing connection in place
    /// (opened via the Edit hover icon or context-menu item); `None` when
    /// adding a brand new connection.
    edit_ix: Option<usize>,
    /// Kept alive so the `InputEvent::PressEnter` subscriptions (submit on
    /// Enter from any field, see `watch_enter_to_submit`) keep firing —
    /// dropping a `Subscription` cancels it.
    _enter_subs: Vec<Subscription>,
}
```

- [ ] **Step 6: Extend `open_new_connection_form`**

Change:

```rust
        let working_dir = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("工作目录(默认 $HOME)")
                .submit_on_enter(true)
        });

        let _enter_subs = vec![
            self.watch_enter_to_submit(&name, window, cx),
            self.watch_enter_to_submit(&host, window, cx),
            self.watch_enter_to_submit(&port, window, cx),
            self.watch_enter_to_submit(&user, window, cx),
            self.watch_enter_to_submit(&password, window, cx),
            self.watch_enter_to_submit(&shell_path, window, cx),
            self.watch_enter_to_submit(&working_dir, window, cx),
        ];

        self.form = Some(ConnForm {
            name,
            conn_type: ConnectionType::Ssh,
            group_id,
            host,
            port,
            user,
            password,
            shell_path,
            working_dir,
            edit_ix: None,
            _enter_subs,
        });
        cx.notify();
    }
```

to:

```rust
        let working_dir = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("工作目录(默认 $HOME)")
                .submit_on_enter(true)
        });
        let serial_port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/dev/ttyUSB0")
                .submit_on_enter(true)
        });
        let baud_rate = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("115200")
                .submit_on_enter(true)
        });

        let _enter_subs = vec![
            self.watch_enter_to_submit(&name, window, cx),
            self.watch_enter_to_submit(&host, window, cx),
            self.watch_enter_to_submit(&port, window, cx),
            self.watch_enter_to_submit(&user, window, cx),
            self.watch_enter_to_submit(&password, window, cx),
            self.watch_enter_to_submit(&shell_path, window, cx),
            self.watch_enter_to_submit(&working_dir, window, cx),
            self.watch_enter_to_submit(&serial_port, window, cx),
            self.watch_enter_to_submit(&baud_rate, window, cx),
        ];

        self.form = Some(ConnForm {
            name,
            conn_type: ConnectionType::Ssh,
            group_id,
            host,
            port,
            user,
            password,
            shell_path,
            working_dir,
            serial_port,
            baud_rate,
            data_bits: 8,
            parity: "none".to_string(),
            stop_bits: 1,
            flow_control: "none".to_string(),
            detected_ports: Vec::new(),
            edit_ix: None,
            _enter_subs,
        });
        cx.notify();
    }
```

- [ ] **Step 7: Extend `start_edit`**

Change:

```rust
        let working_dir = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(conn.working_dir.as_deref().unwrap_or(""))
                .submit_on_enter(true)
        });

        let _enter_subs = vec![
            self.watch_enter_to_submit(&name, window, cx),
            self.watch_enter_to_submit(&host, window, cx),
            self.watch_enter_to_submit(&port, window, cx),
            self.watch_enter_to_submit(&user, window, cx),
            self.watch_enter_to_submit(&password, window, cx),
            self.watch_enter_to_submit(&shell_path, window, cx),
            self.watch_enter_to_submit(&working_dir, window, cx),
        ];

        self.form = Some(ConnForm {
            name,
            conn_type,
            group_id,
            host,
            port,
            user,
            password,
            shell_path,
            working_dir,
            edit_ix: Some(ix),
            _enter_subs,
        });
        cx.notify();
    }
```

to:

```rust
        let working_dir = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(conn.working_dir.as_deref().unwrap_or(""))
                .submit_on_enter(true)
        });
        let serial_port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/dev/ttyUSB0")
                .default_value(conn.serial_port.as_deref().unwrap_or(""))
                .submit_on_enter(true)
        });
        let baud_rate = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("115200")
                .default_value(conn.baud_rate.unwrap_or(115_200).to_string())
                .submit_on_enter(true)
        });
        let data_bits = conn.data_bits.unwrap_or(8);
        let parity = conn.parity.clone().unwrap_or_else(|| "none".to_string());
        let stop_bits = conn.stop_bits.unwrap_or(1);
        let flow_control = conn
            .flow_control
            .clone()
            .unwrap_or_else(|| "none".to_string());

        let _enter_subs = vec![
            self.watch_enter_to_submit(&name, window, cx),
            self.watch_enter_to_submit(&host, window, cx),
            self.watch_enter_to_submit(&port, window, cx),
            self.watch_enter_to_submit(&user, window, cx),
            self.watch_enter_to_submit(&password, window, cx),
            self.watch_enter_to_submit(&shell_path, window, cx),
            self.watch_enter_to_submit(&working_dir, window, cx),
            self.watch_enter_to_submit(&serial_port, window, cx),
            self.watch_enter_to_submit(&baud_rate, window, cx),
        ];

        self.form = Some(ConnForm {
            name,
            conn_type,
            group_id,
            host,
            port,
            user,
            password,
            shell_path,
            working_dir,
            serial_port,
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            flow_control,
            detected_ports: Vec::new(),
            edit_ix: Some(ix),
            _enter_subs,
        });
        cx.notify();
    }
```

- [ ] **Step 8: Extend `save_form`**

Change:

```rust
        let conn = match conn_type {
            ConnectionType::Ssh => {
                let host = form.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host,
                    port: form.port.read(cx).value().trim().parse().unwrap_or(22),
                    user: form.user.read(cx).value().trim().to_string(),
                    password: form.password.read(cx).value().to_string(),
                    group_id,
                    conn_type,
                    icon: None,
                    shell_path: None,
                    working_dir: None,
                    description: None,
                }
            }
            ConnectionType::Local => {
                let shell_path = form.shell_path.read(cx).value().trim().to_string();
                let working_dir = form.working_dir.read(cx).value().trim().to_string();
                SavedConnection {
                    name,
                    host: String::new(),
                    port: 0,
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type,
                    icon: None,
                    shell_path: if shell_path.is_empty() { None } else { Some(shell_path) },
                    working_dir: if working_dir.is_empty() { None } else { Some(working_dir) },
                    description: None,
                }
            }
        };
```

to:

```rust
        let conn = match conn_type {
            ConnectionType::Ssh => {
                let host = form.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host,
                    port: form.port.read(cx).value().trim().parse().unwrap_or(22),
                    user: form.user.read(cx).value().trim().to_string(),
                    password: form.password.read(cx).value().to_string(),
                    group_id,
                    conn_type,
                    icon: None,
                    shell_path: None,
                    working_dir: None,
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                }
            }
            ConnectionType::Local => {
                let shell_path = form.shell_path.read(cx).value().trim().to_string();
                let working_dir = form.working_dir.read(cx).value().trim().to_string();
                SavedConnection {
                    name,
                    host: String::new(),
                    port: 0,
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type,
                    icon: None,
                    shell_path: if shell_path.is_empty() { None } else { Some(shell_path) },
                    working_dir: if working_dir.is_empty() { None } else { Some(working_dir) },
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                }
            }
            ConnectionType::Telnet => {
                let host = form.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host,
                    port: form.port.read(cx).value().trim().parse().unwrap_or(23),
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type,
                    icon: None,
                    shell_path: None,
                    working_dir: None,
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                }
            }
            ConnectionType::Serial => {
                let serial_port = form.serial_port.read(cx).value().trim().to_string();
                if serial_port.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host: String::new(),
                    port: 0,
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type,
                    icon: None,
                    shell_path: None,
                    working_dir: None,
                    serial_port: Some(serial_port),
                    baud_rate: Some(
                        form.baud_rate.read(cx).value().trim().parse().unwrap_or(115_200),
                    ),
                    data_bits: Some(form.data_bits),
                    parity: Some(form.parity.clone()),
                    stop_bits: Some(form.stop_bits),
                    flow_control: Some(form.flow_control.clone()),
                    description: None,
                }
            }
        };
```

- [ ] **Step 9: Replace the row's `clickable` binding with `open_event`**

Change (the full `if conn.conn_type == ConnectionType::Ssh { .. } else { .. }` block):

```rust
        // The clickable part (opens the connection)
        let clickable = if conn.conn_type == ConnectionType::Ssh {
            let spec = conn.to_ssh_config();
            div()
                .id(("conn", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .flex_1()
                .child(icon(conn_icon).text_color(cx.theme().foreground))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(subtitle),
                        ),
                )
                .on_click(cx.listener(move |_this, ev: &ClickEvent, _w, cx| {
                    if ev.click_count() >= 2 {
                        cx.emit(SavedConnectionsEvent::Open(spec.clone()));
                    }
                }))
        } else {
            let shell = conn.shell_path.clone().unwrap_or_default();
            let cwd = conn.working_dir.clone().unwrap_or_default();
            div()
                .id(("conn", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .flex_1()
                .child(icon(conn_icon).text_color(cx.theme().foreground))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(subtitle),
                        ),
                )
                .on_click(cx.listener(move |_this, ev: &ClickEvent, _w, cx| {
                    if ev.click_count() >= 2 {
                        cx.emit(SavedConnectionsEvent::OpenLocal(shell.clone(), cwd.clone()));
                    }
                }))
        };
```

to:

```rust
        // The clickable part (opens the connection). One block for all four
        // connection types now — `open_event` dispatches on `conn_type`, so
        // this doesn't need per-type branching the way it used to.
        let clickable = {
            let conn_for_click = conn.clone();
            div()
                .id(("conn", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .flex_1()
                .child(icon(conn_icon).text_color(cx.theme().foreground))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(subtitle),
                        ),
                )
                .on_click(cx.listener(move |_this, ev: &ClickEvent, _w, cx| {
                    if ev.click_count() >= 2 {
                        cx.emit(open_event(&conn_for_click));
                    }
                }))
        };
```

- [ ] **Step 10: Add the `pill`/`field_label` helpers and refactor `field` to use `field_label`**

Change:

```rust
    fn field(&self, label: &str, state: &Entity<InputState>, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(label.to_string())),
            )
            .child(Input::new(state))
    }
}
```

to:

```rust
    fn field(&self, label: &str, state: &Entity<InputState>, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(label, cx))
            .child(Input::new(state))
    }

    /// Label caption shown above both text-input fields (`field`) and
    /// pill-group fields (`data_bits_field`/`parity_field`/etc.).
    fn field_label(&self, label: &str, cx: &App) -> Div {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(label.to_string()))
    }

    /// One toggle pill's visual styling — shared by the connection-type
    /// selector and the serial data-bits/parity/stop-bits/flow-control
    /// fields. Callers attach their own `.on_click(...)`.
    fn pill(id: &'static str, label: &str, active: bool, cx: &App) -> Stateful<Div> {
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(if active { cx.theme().primary } else { cx.theme().accent })
            .text_color(if active {
                cx.theme().primary_foreground
            } else {
                cx.theme().foreground
            })
            .child(label.to_string())
    }

    /// The serial-only device-path field: a free-text input (so headless /
    /// manual entry always works) plus a "刷新" button that lists detected
    /// ports via `serial::list_ports()`.
    fn serial_port_field(&self, form: &ConnForm, cx: &mut Context<Self>) -> impl IntoElement {
        let target = form.serial_port.clone();
        let mut col = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("串口设备", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&form.serial_port)))
                    .child(
                        div()
                            .id("serial-refresh")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child("刷新")
                            .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                let ports = crate::terminal::serial::list_ports();
                                if let Some(ref mut f) = this.form {
                                    f.detected_ports = ports;
                                }
                                cx.notify();
                            })),
                    ),
            );
        if !form.detected_ports.is_empty() {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(form.detected_ports.iter().enumerate().map(|(i, path)| {
                        let path = path.clone();
                        let target = target.clone();
                        div()
                            .id(("detected-port", i))
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(path.clone())
                            .on_click(cx.listener(move |_this, _ev: &ClickEvent, window, cx| {
                                target.update(cx, |s, cx| {
                                    s.set_value(path.clone(), window, cx);
                                });
                            }))
                    })),
            );
        }
        col
    }

    fn data_bits_field(&self, form: &ConnForm, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("数据位", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("data-bits-5", "5", form.data_bits == 5, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.data_bits = 5;
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-6", "6", form.data_bits == 6, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.data_bits = 6;
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-7", "7", form.data_bits == 7, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.data_bits = 7;
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-8", "8", form.data_bits == 8, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.data_bits = 8;
                                }
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }

    fn parity_field(&self, form: &ConnForm, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("校验位", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("parity-none", "无", form.parity == "none", cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.parity = "none".to_string();
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("parity-odd", "奇校验", form.parity == "odd", cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.parity = "odd".to_string();
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("parity-even", "偶校验", form.parity == "even", cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.parity = "even".to_string();
                                }
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }

    fn stop_bits_field(&self, form: &ConnForm, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("停止位", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("stop-bits-1", "1", form.stop_bits == 1, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.stop_bits = 1;
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("stop-bits-2", "2", form.stop_bits == 2, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.stop_bits = 2;
                                }
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }

    fn flow_control_field(&self, form: &ConnForm, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label("流控", cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("flow-none", "无", form.flow_control == "none", cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.flow_control = "none".to_string();
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill(
                            "flow-software",
                            "软件(XON/XOFF)",
                            form.flow_control == "software",
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            if let Some(ref mut f) = this.form {
                                f.flow_control = "software".to_string();
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        Self::pill(
                            "flow-hardware",
                            "硬件(RTS/CTS)",
                            form.flow_control == "hardware",
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            if let Some(ref mut f) = this.form {
                                f.flow_control = "hardware".to_string();
                            }
                            cx.notify();
                        })),
                    ),
            )
    }
}
```

(Note the trailing `}` — this closes the `impl SavedConnectionsPanel` block that `field` was already the last member of; everything above from `field_label` through `flow_control_field` are new methods added inside that same `impl` block, before its closing brace.)

- [ ] **Step 11: Replace `render_form`'s type selector and field-set match**

Change:

```rust
    /// Render the inline add-connection form.
    fn render_form(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let form = self.form.as_ref()?;
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .m_2()
                .p_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .child(
                    // Connection type selector
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            div()
                                .id("type-ssh")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .bg(if form.conn_type == ConnectionType::Ssh {
                                    cx.theme().primary
                                } else {
                                    cx.theme().accent
                                })
                                .text_color(if form.conn_type == ConnectionType::Ssh {
                                    cx.theme().primary_foreground
                                } else {
                                    cx.theme().foreground
                                })
                                .child("SSH")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    if let Some(ref mut f) = this.form {
                                        f.conn_type = ConnectionType::Ssh;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("type-local")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .bg(if form.conn_type == ConnectionType::Local {
                                    cx.theme().primary
                                } else {
                                    cx.theme().accent
                                })
                                .text_color(if form.conn_type == ConnectionType::Local {
                                    cx.theme().primary_foreground
                                } else {
                                    cx.theme().foreground
                                })
                                .child("本地终端")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    if let Some(ref mut f) = this.form {
                                        f.conn_type = ConnectionType::Local;
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .child(self.field("名称", &form.name, cx))
                .children(match form.conn_type {
                    ConnectionType::Ssh => vec![
                        self.field("主机", &form.host, cx).into_any_element(),
                        self.field("端口", &form.port, cx).into_any_element(),
                        self.field("用户名", &form.user, cx).into_any_element(),
                        self.field("密码", &form.password, cx).into_any_element(),
                    ],
                    ConnectionType::Local => vec![
                        self.field("Shell 路径", &form.shell_path, cx).into_any_element(),
                        self.field("工作目录", &form.working_dir, cx).into_any_element(),
                    ],
                })
```

to:

```rust
    /// Render the inline add-connection form.
    fn render_form(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let form = self.form.as_ref()?;
        let conn_type = form.conn_type.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .m_2()
                .p_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .child(
                    // Connection type selector
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Self::pill("type-ssh", "SSH", conn_type == ConnectionType::Ssh, cx)
                                .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                    if let Some(ref mut f) = this.form {
                                        f.conn_type = ConnectionType::Ssh;
                                        let port = f.port.clone();
                                        port.update(cx, |s, cx| {
                                            s.set_placeholder("端口 (默认 22)", window, cx);
                                        });
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Self::pill(
                                "type-local",
                                "本地终端",
                                conn_type == ConnectionType::Local,
                                cx,
                            )
                            .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.conn_type = ConnectionType::Local;
                                }
                                cx.notify();
                            })),
                        )
                        .child(
                            Self::pill(
                                "type-telnet",
                                "Telnet",
                                conn_type == ConnectionType::Telnet,
                                cx,
                            )
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.conn_type = ConnectionType::Telnet;
                                    let port = f.port.clone();
                                    port.update(cx, |s, cx| {
                                        s.set_placeholder("端口 (默认 23)", window, cx);
                                    });
                                }
                                cx.notify();
                            })),
                        )
                        .child(
                            Self::pill(
                                "type-serial",
                                "串口",
                                conn_type == ConnectionType::Serial,
                                cx,
                            )
                            .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                if let Some(ref mut f) = this.form {
                                    f.conn_type = ConnectionType::Serial;
                                }
                                cx.notify();
                            })),
                        ),
                )
                .child(self.field("名称", &form.name, cx))
                .children(match conn_type {
                    ConnectionType::Ssh => vec![
                        self.field("主机", &form.host, cx).into_any_element(),
                        self.field("端口", &form.port, cx).into_any_element(),
                        self.field("用户名", &form.user, cx).into_any_element(),
                        self.field("密码", &form.password, cx).into_any_element(),
                    ],
                    ConnectionType::Local => vec![
                        self.field("Shell 路径", &form.shell_path, cx).into_any_element(),
                        self.field("工作目录", &form.working_dir, cx).into_any_element(),
                    ],
                    ConnectionType::Telnet => vec![
                        self.field("主机", &form.host, cx).into_any_element(),
                        self.field("端口", &form.port, cx).into_any_element(),
                    ],
                    ConnectionType::Serial => vec![
                        self.serial_port_field(form, cx).into_any_element(),
                        self.field("波特率", &form.baud_rate, cx).into_any_element(),
                        self.data_bits_field(form, cx).into_any_element(),
                        self.parity_field(form, cx).into_any_element(),
                        self.stop_bits_field(form, cx).into_any_element(),
                        self.flow_control_field(form, cx).into_any_element(),
                    ],
                })
```

(Everything below this point in `render_form` — the cancel/save button row and the function's closing `)` / `}` — is unchanged.)

#### Phase C: `src/workspace.rs`

- [ ] **Step 12: Update imports**

Change:

```rust
use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::TerminalPanel;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::view::TerminalView;
```

to:

```rust
use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::TerminalPanel;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::terminal::view::TerminalView;
```

- [ ] **Step 13: Wire the two new events in the subscription match**

Change:

```rust
        let saved_sub =
            cx.subscribe_in(&saved, window, |this, _panel, event, window, cx| match event {
                SavedConnectionsEvent::Open(config) => this.open_ssh(config.clone(), window, cx),
                SavedConnectionsEvent::OpenSftp(config) => {
                    this.show_sftp(config.clone(), window, cx)
                }
                SavedConnectionsEvent::OpenLocal(shell, cwd) => {
                    this.open_local_with(shell.clone(), cwd.clone(), window, cx)
                }
            });
```

to:

```rust
        let saved_sub =
            cx.subscribe_in(&saved, window, |this, _panel, event, window, cx| match event {
                SavedConnectionsEvent::Open(config) => this.open_ssh(config.clone(), window, cx),
                SavedConnectionsEvent::OpenSftp(config) => {
                    this.show_sftp(config.clone(), window, cx)
                }
                SavedConnectionsEvent::OpenLocal(shell, cwd) => {
                    this.open_local_with(shell.clone(), cwd.clone(), window, cx)
                }
                SavedConnectionsEvent::OpenTelnet(config) => {
                    this.open_telnet(config.clone(), window, cx)
                }
                SavedConnectionsEvent::OpenSerial(config) => {
                    this.open_serial(config.clone(), window, cx)
                }
            });
```

- [ ] **Step 14: Add `open_telnet`/`open_serial` after `open_ssh`**

Find (existing code, unchanged so far):

```rust
    pub fn open_ssh(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.ssh_session(&config) {
            let terminal = cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session));
            let follow = config.clone();
            let handle = terminal.read(cx).focus_handle(cx);
            let term_weak = terminal.downgrade();
            let sub = cx.on_focus(&handle, window, move |this, window, cx| {
                this.set_active_title_from(&term_weak, cx);
                this.show_sftp(follow.clone(), window, cx);
            });
            self._subscriptions.push(sub);
            let panel = cx.new(|_cx| TerminalPanel::new(terminal));
            self.add_center(Arc::new(panel), window, cx);
            self.show_sftp(config, window, cx);
        }
    }
```

Add immediately after it (before `/// Update the header's active title...`):

```rust

    /// Open a raw Telnet terminal as a new central tab. No shared-connection
    /// cache (unlike SSH): telnet has no SFTP-style second channel to
    /// justify one, so each tab dials its own socket, same as a local shell.
    pub fn open_telnet(&mut self, config: TelnetConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_telnet(window, cx, config));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
    }

    /// Open a serial-port terminal as a new central tab. Same
    /// no-shared-cache rationale as `open_telnet`.
    pub fn open_serial(&mut self, config: SerialConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_serial(window, cx, config));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
    }
```

#### Phase D: verify and commit

- [ ] **Step 15: Build**

Run: `cargo build`
Expected: succeeds with no errors (warnings about `#[allow(dead_code)]`-less unused items, if any, should not appear — everything added in this task is now consumed).

- [ ] **Step 16: Run the full test suite**

Run: `cargo test`
Expected: all tests pass — the pre-existing suites (`keymap`, `batching`, `grid_snapshot`) plus this plan's new ones (`telnet::codec_tests`, `telnet::backend_tests`, `serial::mapping_tests`, `config::tests`).

- [ ] **Step 17: Commit**

```bash
git add src/config.rs src/panels/saved_connections.rs src/workspace.rs
git commit -m "feat: wire Telnet and serial connections into the saved-connections UI"
```

---

### Task 7: End-to-end verification

**Files:** none (manual verification only — this task produces no diff besides what Task 6 already committed).

**Interfaces:** N/A.

- [ ] **Step 1: Full build + test sweep**

Run: `cargo build && cargo test`
Expected: clean build, all tests green.

- [ ] **Step 2: Launch the app**

Run: `cargo run`
Expected: window opens, "已保存的连接" panel visible on the right with the `server.svg` icon (unrelated prior fix, should still be intact).

- [ ] **Step 3: Verify the 4-pill new-connection form**

In the UI: click "新建连接". Confirm four pills render (SSH / 本地终端 / Telnet / 串口), wrapping to two rows if the panel is narrow. Click each pill in turn and confirm the field set below it changes:
- SSH: 主机/端口/用户名/密码
- 本地终端: Shell 路径/工作目录
- Telnet: 主机/端口 (only two fields — no user/password)
- 串口: 串口设备 (with a 刷新 button) / 波特率 / 数据位 / 校验位 / 停止位 / 流控 (four pill-groups)

Click "刷新" under 串口设备 and confirm it doesn't crash (a detected-ports list appears if any real serial hardware is attached; an empty result is fine on a dev machine with none).

- [ ] **Step 4: Telnet round-trip against a local listener (no external network needed)**

In a separate terminal, start a throwaway raw-TCP listener that echoes back what it receives:

```bash
ncat -l -k -p 2323 -c cat
```

(If `ncat`/`nc` isn't available, `socat -d -d TCP-LISTEN:2323,reuseaddr,fork EXEC:cat` works too.)

In Caracal: 新建连接 → Telnet, host `127.0.0.1`, port `2323`, save (or just fill and open without saving, if the form supports opening directly — otherwise save then double-click the row). Type some text in the resulting terminal tab and confirm it echoes back (since the listener pipes to `cat`). Close the tab; confirm the listener process exits or its connection drops (proves `TelnetBackend`'s shutdown-on-drop reached the peer).

- [ ] **Step 5: Serial UI round-trip (hardware-optional)**

If a real serial device or USB-serial adapter is attached, create a 串口 connection pointing at it with the correct baud rate and confirm bytes flow both directions.

If no hardware is available, this step is optional — on Linux, `socat -d -d pty,raw,echo=0,link=/tmp/caracal-test-pty pty,raw,echo=0` creates a loopback virtual serial pair; point the form at `/tmp/caracal-test-pty`, and pipe test bytes into/out of `socat`'s other endpoint from a separate terminal (the path `socat` prints for the second pty) using `cat`. Skip this step entirely if `socat` isn't installed and no hardware is available — the important thing already verified in Step 3 is that the form itself renders and saves/loads correctly; the transport was already covered by Task 3's `open_reports_a_descriptive_error_for_a_nonexistent_port` test and the mapping-function tests.

- [ ] **Step 6: Regression check — SSH and local terminal still work**

Open an existing SSH saved connection (or create one against any reachable host) and confirm the shell + SFTP panel still work as before. Open a local terminal tab (⚡ / "本地终端" toolbar action) and confirm it still opens. This confirms the `open_event`/row-rendering refactor in Task 6 Step 9 didn't regress the two pre-existing connection types.

- [ ] **Step 7: Report results**

No commit in this task (Task 6 already committed everything). If any verification step surfaces a bug, fix it as a follow-up commit on top of Task 6's, re-run `cargo test`, and repeat the relevant verification step.
