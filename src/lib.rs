//! Receive and save JPEG frames from the observed type-8 Wi-Fi microscope route.
//!
//! [`Microscope::poll`] is nonblocking; call it regularly from an event loop. The
//! receiver binds UDP/10900 by default and the client sends the observed startup
//! datagrams plus a `JHCMD D0 01` heartbeat every 500 ms.

use chrono::{Local, Utc};
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

const DEFAULT_DEVICE_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 29, 1);
const DEFAULT_RECEIVE_PORT: u16 = 10_900;
const DEFAULT_CONTROL_PORT: u16 = 20_000;
const DEFAULT_STARTUP_PORT: u16 = 20_001;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_DATAGRAM_BYTES: usize = 65_535;
const STARTUP_MARKER: [u8; 11] = [
    0x46, 0x44, 0x57, 0x4e, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
];
const HEARTBEAT: [u8; 7] = [b'J', b'H', b'C', b'M', b'D', 0xd0, 0x01];

/// Network settings for the captured UDP route.
#[derive(Clone, Debug)]
pub struct MicroscopeConfig {
    /// Microscope IPv4 address. The observed device uses `192.168.29.1`.
    pub device_ip: Ipv4Addr,
    /// Local address for JPEG datagrams. Port 10900 is the observed receiver port.
    pub receive_addr: SocketAddrV4,
    /// UDP destination port for JHCMD control datagrams.
    pub control_port: u16,
    /// UDP destination port for FDWN initialization datagrams.
    pub startup_port: u16,
    /// Repeat interval for the stream-start / heartbeat command.
    pub heartbeat_interval: Duration,
}

impl MicroscopeConfig {
    /// Settings for a microscope using the observed ports and default device address.
    pub fn new(device_ip: Ipv4Addr) -> Self {
        Self {
            device_ip,
            receive_addr: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DEFAULT_RECEIVE_PORT),
            control_port: DEFAULT_CONTROL_PORT,
            startup_port: DEFAULT_STARTUP_PORT,
            heartbeat_interval: Duration::from_millis(500),
        }
    }
}

impl Default for MicroscopeConfig {
    fn default() -> Self {
        Self::new(DEFAULT_DEVICE_IP)
    }
}

/// A completely reassembled JPEG frame.
#[derive(Debug)]
pub struct Frame {
    /// The little-endian 16-bit frame counter from the UDP header.
    pub sequence: u16,
    /// JPEG bytes, beginning with SOI and ending with EOI.
    pub jpeg: Vec<u8>,
    /// Arrival time of the frame's first fragment.
    pub received_at: SystemTime,
}

impl Frame {
    /// Save this frame's JPEG bytes to `path`.
    pub fn save_jpeg(&self, path: impl AsRef<Path>) -> io::Result<()> {
        fs::write(path, &self.jpeg)
    }
}

/// Connected UDP client. Call [`poll`](Self::poll) regularly to receive frames
/// and service the heartbeat; it never waits for network input.
pub struct Microscope {
    receiver: UdpSocket,
    sender: UdpSocket,
    device_ip: Ipv4Addr,
    control_addr: SocketAddrV4,
    heartbeat_interval: Duration,
    next_heartbeat: Instant,
    receive_buffer: Vec<u8>,
    assembler: FrameAssembler,
}

impl Microscope {
    /// Bind the local frame socket and send the observed startup messages.
    pub fn connect(config: MicroscopeConfig) -> io::Result<Self> {
        if config.heartbeat_interval.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "heartbeat interval must be greater than zero",
            ));
        }

        let receiver = UdpSocket::bind(SocketAddr::V4(config.receive_addr))?;
        receiver.set_nonblocking(true)?;
        let sender = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
        let control_addr = SocketAddrV4::new(config.device_ip, config.control_port);
        let startup_addr = SocketAddrV4::new(config.device_ip, config.startup_port);

        send_datagram(&sender, &STARTUP_MARKER, startup_addr)?;
        let sync_packet = build_time_sync_packet();
        send_datagram(&sender, &sync_packet, startup_addr)?;
        send_datagram(&sender, &HEARTBEAT, control_addr)?;

        Ok(Self {
            receiver,
            sender,
            device_ip: config.device_ip,
            control_addr,
            heartbeat_interval: config.heartbeat_interval,
            next_heartbeat: Instant::now() + config.heartbeat_interval,
            receive_buffer: vec![0; MAX_DATAGRAM_BYTES],
            assembler: FrameAssembler::default(),
        })
    }

    /// Process pending datagrams and send a due heartbeat. Returns the newest
    /// complete JPEG frame received since the previous call, if any.
    pub fn poll(&mut self) -> io::Result<Option<Frame>> {
        let now = Instant::now();
        if now >= self.next_heartbeat {
            send_datagram(&self.sender, &HEARTBEAT, self.control_addr)?;
            self.next_heartbeat = now + self.heartbeat_interval;
        }

        let mut newest_frame = None;
        loop {
            match self.receiver.recv_from(&mut self.receive_buffer) {
                Ok((length, peer)) => {
                    if peer.ip() != self.device_ip {
                        continue;
                    }
                    if let Some(frame) = self.assembler.push(&self.receive_buffer[..length]) {
                        newest_frame = Some(frame);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        Ok(newest_frame)
    }

    /// The local UDP address used for incoming JPEG frames.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.receiver.local_addr()
    }
}

fn send_datagram(socket: &UdpSocket, bytes: &[u8], destination: SocketAddrV4) -> io::Result<()> {
    let sent = socket.send_to(bytes, destination)?;
    if sent != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "incomplete UDP datagram send",
        ));
    }
    Ok(())
}

fn build_time_sync_packet() -> [u8; 26] {
    let local = Local::now();
    let utc = local.with_timezone(&Utc);
    let mut packet = [0_u8; 26];
    packet[..10].copy_from_slice(&[0x46, 0x44, 0x57, 0x4e, 0x20, 0x00, 0x01, 0x00, 0x10, 0x00]);
    packet[10..16].copy_from_slice(&calendar_tuple(
        utc.year(),
        utc.month(),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second(),
    ));
    packet[16] = (local.offset().local_minus_utc() / 3600) as i8 as u8;
    packet[17..20].copy_from_slice(&[0x00, 0x52, 0x91]);
    packet[20..26].copy_from_slice(&calendar_tuple(
        local.year(),
        local.month(),
        local.day(),
        local.hour(),
        local.minute(),
        local.second(),
    ));
    packet
}

use chrono::Datelike;
use chrono::Timelike;

fn calendar_tuple(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> [u8; 6] {
    [
        (year - 2000) as u8,
        month as u8,
        day as u8,
        hour as u8,
        minute as u8,
        second as u8,
    ]
}

#[derive(Default)]
struct FrameAssembler {
    current: Option<PartialFrame>,
}

struct PartialFrame {
    sequence: u16,
    next_packet: u8,
    jpeg: Vec<u8>,
    received_at: SystemTime,
}

impl FrameAssembler {
    fn push(&mut self, datagram: &[u8]) -> Option<Frame> {
        if datagram.len() < 8 {
            return None;
        }
        let sequence = u16::from_le_bytes([datagram[0], datagram[1]]);
        let packet_number = datagram[3];
        let fragment = &datagram[8..];

        if packet_number == 0 {
            let completed = self.current.take().and_then(PartialFrame::finish);
            if fragment.len() <= MAX_FRAME_BYTES {
                let mut jpeg = Vec::with_capacity(fragment.len().max(64 * 1024));
                jpeg.extend_from_slice(fragment);
                self.current = Some(PartialFrame {
                    sequence,
                    next_packet: 1,
                    jpeg,
                    received_at: SystemTime::now(),
                });
            }
            return completed;
        }

        let Some(current) = self.current.as_mut() else {
            return None;
        };
        if current.sequence != sequence || current.next_packet != packet_number {
            self.current = None;
            return None;
        }
        if current.jpeg.len().saturating_add(fragment.len()) > MAX_FRAME_BYTES {
            self.current = None;
            return None;
        }
        current.jpeg.extend_from_slice(fragment);
        current.next_packet = current.next_packet.wrapping_add(1);
        None
    }
}

impl PartialFrame {
    fn finish(self) -> Option<Frame> {
        if self.jpeg.starts_with(&[0xff, 0xd8]) && self.jpeg.ends_with(&[0xff, 0xd9]) {
            Some(Frame {
                sequence: self.sequence,
                jpeg: self.jpeg,
                received_at: self.received_at,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::thread;

    fn packet(sequence: u16, packet_number: u8, fragment: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 8];
        bytes[..2].copy_from_slice(&sequence.to_le_bytes());
        bytes[3] = packet_number;
        bytes.extend_from_slice(fragment);
        bytes
    }

    #[test]
    fn reassembles_contiguous_packets_at_the_next_frame_boundary() {
        let mut assembler = FrameAssembler::default();
        assert!(assembler.push(&packet(7, 0, &[0xff, 0xd8, 0x01])).is_none());
        assert!(assembler.push(&packet(7, 1, &[0x02, 0xff, 0xd9])).is_none());
        let frame = assembler
            .push(&packet(8, 0, &[0xff, 0xd8, 0xff, 0xd9]))
            .unwrap();
        assert_eq!(frame.sequence, 7);
        assert_eq!(frame.jpeg, [0xff, 0xd8, 0x01, 0x02, 0xff, 0xd9]);
    }

    #[test]
    fn drops_a_frame_after_a_missing_fragment() {
        let mut assembler = FrameAssembler::default();
        assert!(assembler.push(&packet(7, 0, &[0xff, 0xd8])).is_none());
        assert!(assembler.push(&packet(7, 2, &[0xff, 0xd9])).is_none());
        assert!(
            assembler
                .push(&packet(8, 0, &[0xff, 0xd8, 0xff, 0xd9]))
                .is_none()
        );
        let frame = assembler
            .push(&packet(9, 0, &[0xff, 0xd8, 0xff, 0xd9]))
            .unwrap();
        assert_eq!(frame.sequence, 8);
    }

    #[test]
    fn connects_sends_startup_and_receives_local_frames() {
        let control = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let startup = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        control
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        startup
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();

        let receive_probe = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let receive_port = receive_probe.local_addr().unwrap().port();
        drop(receive_probe);
        let config = MicroscopeConfig {
            device_ip: Ipv4Addr::LOCALHOST,
            receive_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, receive_port),
            control_port: control.local_addr().unwrap().port(),
            startup_port: startup.local_addr().unwrap().port(),
            heartbeat_interval: Duration::from_millis(10),
        };
        let mut microscope = Microscope::connect(config).unwrap();

        let mut buffer = [0; MAX_DATAGRAM_BYTES];
        let (length, _) = control.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], &HEARTBEAT);
        let (length, _) = startup.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], &STARTUP_MARKER);
        let (length, _) = startup.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..4], b"FDWN");
        assert_eq!(length, 26);
        assert_eq!(&buffer[8..10], &[16, 0]);

        thread::sleep(Duration::from_millis(15));
        assert!(microscope.poll().unwrap().is_none());
        let (length, _) = control.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], &HEARTBEAT);

        let injector = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let destination = microscope.local_addr().unwrap();
        injector
            .send_to(&packet(41, 0, &[0xff, 0xd8, 0x01, 0xff, 0xd9]), destination)
            .unwrap();
        injector
            .send_to(&packet(42, 0, &[0xff, 0xd8, 0xff, 0xd9]), destination)
            .unwrap();
        let frame = microscope.poll().unwrap().unwrap();
        assert_eq!(frame.sequence, 41);
        assert_eq!(frame.jpeg, [0xff, 0xd8, 0x01, 0xff, 0xd9]);

        let path = std::env::temp_dir().join(format!(
            "microscope-{}-{}.jpg",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        frame.save_jpeg(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), frame.jpeg);
        std::fs::remove_file(path).unwrap();
    }
}
