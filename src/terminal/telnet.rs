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
