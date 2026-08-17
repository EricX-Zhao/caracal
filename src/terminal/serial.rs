//! Serial-port backend: a blocking `serialport::SerialPort`, split into
//! reader/writer threads exactly like `LocalPty` in `backend.rs`. No live
//! resize (physical ports have no notion of size) and no async runtime —
//! same one-tab-one-handle shape as `telnet.rs`.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// The reader thread's blocking-read timeout — also the upper bound on how
/// long after a tab closes the previous generation's reader thread can still
/// be sitting inside `read()` before it notices `shutdown` and exits (see
/// `SerialBackend::shutdown`'s doc comment). Kept short so that window is
/// short too.
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// How many times `open` retries a `NoDevice` failure, and how long it waits
/// between attempts — see the retry loop in `open` for why.
const REOPEN_RETRY_ATTEMPTS: u32 = 3;
const REOPEN_RETRY_DELAY: Duration = READ_TIMEOUT;

/// Detected ports for the new-connection form's picker. Never errors (an
/// empty `Vec` on detection failure just means the form's list stays empty
/// and the user falls back to typing the path manually).
pub fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

/// A physical/virtual serial port opened via the `serialport` crate.
#[derive(Debug)]
pub struct SerialBackend {
    write_tx: flume::Sender<Vec<u8>>,
    /// Set by `Drop` and polled by the reader thread (which wakes every
    /// `read()` timeout regardless of whether data arrived) so closing the
    /// tab actually releases the OS-level device — without this, the reader
    /// thread loops forever holding its own `try_clone()`'d port handle open,
    /// even after every other reference to `SerialBackend` is gone.
    shutdown: Arc<AtomicBool>,
}

impl SerialBackend {
    /// Open the port and spawn the reader/writer threads. Blocks until the
    /// port is open (or fails), matching `TelnetBackend::connect`'s and
    /// `SshSession::connect`'s synchronous-connect contract.
    pub fn open(config: SerialConfig, bytes_tx: flume::Sender<Vec<u8>>) -> Result<Self> {
        let port_for_error = config.port.clone();
        let mut attempt = 0;
        let port = loop {
            let result = serialport::new(&config.port, config.baud_rate)
                .data_bits(map_data_bits(config.data_bits))
                .parity(map_parity(&config.parity))
                .stop_bits(map_stop_bits(config.stop_bits))
                .flow_control(map_flow_control(&config.flow_control))
                // A finite read timeout, not a truly blocking read: the reader
                // loop below treats a timeout as "no data yet" and keeps
                // polling, so the thread also notices promptly when the write
                // channel closes (tab closed) instead of blocking forever on a
                // port that never sends anything.
                .timeout(READ_TIMEOUT)
                .open();
            match result {
                Ok(port) => break port,
                // On Unix, `serialport` opens exclusively (TIOCEXCL + flock);
                // an `EBUSY`/`EWOULDBLOCK` from that surfaces here as
                // `NoDevice`. The dominant real-world cause of that — a
                // previous tab's `SerialBackend` for this same port not yet
                // released — is now fixed at the source (see
                // docs/superpowers/specs/2026-08-17-serial-reopen-busy-design.md);
                // this retry stays as a defensive net for the remaining,
                // genuinely time-bounded case: this backend's *own* reader
                // thread hasn't looped back to notice `shutdown` and drop
                // its cloned handle yet (up to `READ_TIMEOUT`), plus
                // whatever similar latency other software (udev,
                // ModemManager, …) might add outside our control.
                Err(e)
                    if e.kind() == serialport::ErrorKind::NoDevice
                        && attempt < REOPEN_RETRY_ATTEMPTS =>
                {
                    attempt += 1;
                    std::thread::sleep(REOPEN_RETRY_DELAY);
                }
                Err(e) => return Err(anyhow!("open serial port {port_for_error:?}: {e}")),
            }
        };

        let mut reader = port
            .try_clone()
            .map_err(|e| anyhow!("clone serial port for reader: {e}"))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let reader_shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("caracal-serial-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    if reader_shutdown.load(Ordering::Relaxed) {
                        break;
                    }
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

        Ok(Self { write_tx, shutdown })
    }
}

impl Drop for SerialBackend {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
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
