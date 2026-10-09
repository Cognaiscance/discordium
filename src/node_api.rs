//! Register and get-data against the local pNet node, and the saved token.
//!
//! The token file is the only thing this step writes. It does not open the
//! conversation store.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::{SocketAddr, UdpSocket};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::directory::APP_ALIAS;

pub const APP_PROTOCOL: &str = "application/discordium";
pub const DEFAULT_PUSH_PORT: u16 = 8790;
pub const DEFAULT_PNET_ADDR: &str = "127.0.0.1:7777";

const OP_REGISTER: u8 = 0x00;
const OP_GET_DATA: u8 = 0x02;
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

pub fn token_path() -> PathBuf {
    if let Ok(path) = std::env::var("DISCORDIUM_TOKEN_FILE") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".pnet/discordium/token");
        }
    }
    PathBuf::from("discordium-token")
}

/// Directory the record-holding server owns. The token file lives here too.
/// `DISCORDIUM_DIR` overrides it. A device or a standby does not open this.
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

pub fn register_packet(push_port: u16) -> Vec<u8> {
    let mut buf = vec![OP_REGISTER];
    push_str(&mut buf, APP_ALIAS);
    buf.extend_from_slice(&push_port.to_be_bytes());
    push_str(&mut buf, APP_PROTOCOL);
    buf
}

pub fn get_data_packet(token: &[u8; 16]) -> Vec<u8> {
    let mut buf = vec![OP_GET_DATA];
    buf.extend_from_slice(token);
    buf
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

    pub fn register(&self, push_port: u16) -> Result<[u8; 16], NodeError> {
        let reply = self.round_trip(&register_packet(push_port), 5)?;
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

fn is_timeout(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::TimedOut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_packet_names_discordium() {
        let packet = register_packet(8790);
        let mut expected = vec![0x00, 10];
        expected.extend_from_slice(b"discordium");
        expected.extend_from_slice(&8790u16.to_be_bytes());
        expected.push(22);
        expected.extend_from_slice(b"application/discordium");
        assert_eq!(packet, expected);
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
