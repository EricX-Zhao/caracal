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
#[derive(Debug)]
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
