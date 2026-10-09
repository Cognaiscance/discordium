//! Register and get-data against the local pNet node, and the saved token.
//!
//! Each process writes only its own token file. This module does not open the
//! conversation store.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::{SocketAddr, UdpSocket};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::directory::ProcessKind;

pub const APP_PROTOCOL: &str = "application/discordium";
pub const DEFAULT_PNET_ADDR: &str = "127.0.0.1:7777";

pub fn default_push_port(kind: ProcessKind) -> u16 {
    match kind {
        ProcessKind::Client => 8790,
        ProcessKind::Server => 8791,
    }
}

const OP_REGISTER: u8 = 0x00;
const OP_GET_DATA: u8 = 0x02;
const OP_SEND: u8 = 0x03;
const OP_PUSH: u8 = 0x04;
const STATUS_OK: u8 = 0x00;
const STATUS_ERR: u8 = 0x01;

#[derive(Debug)]
pub enum NodeError {
    Timeout,
    Io(io::Error),
    Rejected { code: Option<u8> },
    Unexpected,
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::Timeout => f.write_str("no reply from the local node"),
            NodeError::Io(err) => write!(f, "{err}"),
            NodeError::Rejected { code: Some(code) } => {
                write!(f, "node rejected the request ({code:#04x})")
            }
            NodeError::Rejected { code: None } => f.write_str("node rejected the request"),
            NodeError::Unexpected => f.write_str("unexpected reply from the local node"),
        }
    }
}

pub fn token_path(kind: ProcessKind) -> PathBuf {
    if let Ok(path) = std::env::var("DISCORDIUM_TOKEN_FILE") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let home = std::env::var("HOME").ok();
    default_token_path(kind, home.as_deref())
}

pub fn default_token_path(kind: ProcessKind, home: Option<&str>) -> PathBuf {
    let dir = match kind {
        ProcessKind::Client => "discordium-client",
        ProcessKind::Server => "discordium-server",
    };
    if let Some(home) = home {
        if !home.is_empty() {
            return PathBuf::from(home).join(".pnet").join(dir).join("token");
        }
    }
    PathBuf::from(format!("{dir}-token"))
}

/// Directory the record-holding server owns. `DISCORDIUM_DIR` overrides it.
/// The client and a standby do not open this. Token files live elsewhere.
pub fn record_dir() -> PathBuf {
    if let Ok(path) = std::env::var("DISCORDIUM_DIR") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".pnet/discordium");
        }
    }
    PathBuf::from(".pnet/discordium")
}

pub fn save_token(path: &Path, token: &[u8; 16]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn load_token(path: &Path) -> io::Result<Option<[u8; 16]>> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() == 16 => {
            let mut token = [0u8; 16];
            token.copy_from_slice(&bytes);
            Ok(Some(token))
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token file must be 16 bytes",
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn register_packet(alias: &str, push_port: u16) -> Vec<u8> {
    let mut buf = vec![OP_REGISTER];
    push_str(&mut buf, alias);
    buf.extend_from_slice(&push_port.to_be_bytes());
    push_str(&mut buf, APP_PROTOCOL);
    buf
}

pub fn get_data_packet(token: &[u8; 16]) -> Vec<u8> {
    let mut buf = vec![OP_GET_DATA];
    buf.extend_from_slice(token);
    buf
}

/// Op 0x03. Success from the node is silent, so this is only the request.
pub fn send_packet(
    token: &[u8; 16],
    dest_device: &[u8; 16],
    dest_app: &[u8; 16],
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 48 + payload.len());
    buf.push(OP_SEND);
    buf.extend_from_slice(token);
    buf.extend_from_slice(dest_device);
    buf.extend_from_slice(dest_app);
    buf.extend_from_slice(payload);
    buf
}

/// Op 0x04 push: sender app id, then the opaque payload.
pub fn decode_push(datagram: &[u8]) -> Option<([u8; 16], &[u8])> {
    if datagram.first() != Some(&OP_PUSH) || datagram.len() < 17 {
        return None;
    }
    let mut sender = [0u8; 16];
    sender.copy_from_slice(&datagram[1..17]);
    Some((sender, &datagram[17..]))
}

fn push_str(buf: &mut Vec<u8>, text: &str) {
    buf.push(text.len() as u8);
    buf.extend_from_slice(text.as_bytes());
}

pub struct NodeClient {
    socket: UdpSocket,
}

impl NodeClient {
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.connect(addr)?;
        socket.set_read_timeout(Some(Duration::from_secs(1)))?;
        Ok(Self { socket })
    }

    pub fn register(&self, alias: &str, push_port: u16) -> Result<[u8; 16], NodeError> {
        let reply = self.round_trip(&register_packet(alias, push_port), 5)?;
        if reply.len() != 17 || reply[0] != STATUS_OK {
            return Err(NodeError::Unexpected);
        }
        let mut token = [0u8; 16];
        token.copy_from_slice(&reply[1..17]);
        Ok(token)
    }

    pub fn get_data(&self, token: &[u8; 16]) -> Result<Vec<u8>, NodeError> {
        self.round_trip(&get_data_packet(token), 3)
    }

    /// Send an app payload. A timeout means the node accepted it, because
    /// success has no reply. An error reply is returned.
    pub fn send(
        &self,
        token: &[u8; 16],
        dest_device: &[u8; 16],
        dest_app: &[u8; 16],
        payload: &[u8],
    ) -> Result<(), NodeError> {
        self.socket
            .send(&send_packet(token, dest_device, dest_app, payload))
            .map_err(NodeError::Io)?;
        self.socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(NodeError::Io)?;
        let result = read_send_result(&self.socket);
        self.socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(NodeError::Io)?;
        result
    }

    fn round_trip(&self, packet: &[u8], attempts: u32) -> Result<Vec<u8>, NodeError> {
        let mut last = NodeError::Timeout;
        for attempt in 0..attempts {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(200));
            }
            if let Err(err) = self.socket.send(packet) {
                last = NodeError::Io(err);
                continue;
            }
            let mut buf = [0u8; 65535];
            match self.socket.recv(&mut buf) {
                Ok(0) => last = NodeError::Unexpected,
                Ok(n) => {
                    let reply = buf[..n].to_vec();
                    if reply.first() == Some(&STATUS_ERR) {
                        return Err(NodeError::Rejected {
                            code: reply.get(1).copied(),
                        });
                    }
                    if reply.first() != Some(&STATUS_OK) {
                        return Err(NodeError::Unexpected);
                    }
                    return Ok(reply);
                }
                Err(err) if is_timeout(&err) => last = NodeError::Timeout,
                Err(err) => last = NodeError::Io(err),
            }
        }
        Err(last)
    }
}

fn read_send_result(socket: &UdpSocket) -> Result<(), NodeError> {
    let mut buf = [0u8; 64];
    match socket.recv(&mut buf) {
        Ok(0) => Err(NodeError::Unexpected),
        Ok(n) => {
            if n >= 1 && buf[0] == STATUS_ERR {
                let code = if n >= 2 { Some(buf[1]) } else { None };
                Err(NodeError::Rejected { code })
            } else {
                Err(NodeError::Unexpected)
            }
        }
        Err(err) if is_timeout(&err) => Ok(()),
        Err(err) => Err(NodeError::Io(err)),
    }
}

fn is_timeout(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::TimedOut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{ProcessKind, CLIENT_ALIAS, SERVER_ALIAS};

    #[test]
    fn register_packets_use_different_names_and_ports() {
        assert_eq!(
            register_packet(CLIENT_ALIAS, 8790),
            named_register(CLIENT_ALIAS, 8790)
        );
        assert_eq!(
            register_packet(SERVER_ALIAS, 8791),
            named_register(SERVER_ALIAS, 8791)
        );
        assert_ne!(
            register_packet(CLIENT_ALIAS, 8790),
            register_packet(SERVER_ALIAS, 8791)
        );
    }

    fn named_register(alias: &str, port: u16) -> Vec<u8> {
        let mut expected = vec![0x00, alias.len() as u8];
        expected.extend_from_slice(alias.as_bytes());
        expected.extend_from_slice(&port.to_be_bytes());
        expected.push(22);
        expected.extend_from_slice(b"application/discordium");
        expected
    }

    #[test]
    fn token_paths_and_push_ports_differ() {
        assert_eq!(
            default_token_path(ProcessKind::Client, Some("/home/person")),
            PathBuf::from("/home/person/.pnet/discordium-client/token")
        );
        assert_eq!(
            default_token_path(ProcessKind::Server, Some("/home/person")),
            PathBuf::from("/home/person/.pnet/discordium-server/token")
        );
        assert_eq!(
            default_token_path(ProcessKind::Client, None),
            PathBuf::from("discordium-client-token")
        );
        assert_eq!(
            default_token_path(ProcessKind::Server, Some("")),
            PathBuf::from("discordium-server-token")
        );
        assert_eq!(default_push_port(ProcessKind::Client), 8790);
        assert_eq!(default_push_port(ProcessKind::Server), 8791);
    }

    #[test]
    fn send_packet_names_the_destination() {
        let token = [0x11; 16];
        let device = [0x22; 16];
        let app = [0x33; 16];
        let packet = send_packet(&token, &device, &app, &[1, 1]);
        assert_eq!(packet[0], 0x03);
        assert_eq!(&packet[1..17], &token);
        assert_eq!(&packet[17..33], &device);
        assert_eq!(&packet[33..49], &app);
        assert_eq!(&packet[49..], &[1, 1]);
    }

    #[test]
    fn decode_push_returns_the_sender_and_payload() {
        let mut datagram = vec![0x04];
        datagram.extend_from_slice(&[0xAB; 16]);
        datagram.extend_from_slice(&[9, 8, 7]);
        let (sender, payload) = decode_push(&datagram).unwrap();
        assert_eq!(sender, [0xAB; 16]);
        assert_eq!(payload, &[9, 8, 7]);
        assert!(decode_push(&[0x04; 16]).is_none());
        assert!(decode_push(&[0x02, 0, 0]).is_none());
    }

    #[test]
    fn get_data_packet_carries_the_token() {
        let token = [0xAB; 16];
        let mut expected = vec![0x02];
        expected.extend_from_slice(&token);
        assert_eq!(get_data_packet(&token), expected);
    }

    #[test]
    fn token_file_round_trip_is_private() {
        let dir = std::env::temp_dir().join(format!(
            "discordium-token-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = dir.join("token");
        let token = [0x5A; 16];
        save_token(&path, &token).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(load_token(&path).unwrap(), Some(token));
        fs::write(&path, [1, 2, 3]).unwrap();
        assert!(load_token(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
