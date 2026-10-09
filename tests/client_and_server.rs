//! One node, two processes: the client serves pages and the server answers.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CLIENT_ALIAS: &str = "discordium-client";
const SERVER_ALIAS: &str = "discordium-server";
const PROTOCOL: &str = "application/discordium";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Registration {
    alias: String,
    port: u16,
    protocol: String,
}

struct Node {
    running: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<Mutex<Vec<Registration>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Proc {
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn client_and_server_share_one_node() {
    let node_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let node_addr = node_socket.local_addr().unwrap();
    let node = start_node(node_socket);

    let root = TempDir::new();
    std::fs::create_dir_all(root.path().join("client-record")).unwrap();
    std::fs::create_dir_all(root.path().join("server-record")).unwrap();

    let client_push = free_udp();
    let server_push = free_udp();
    let client_http = free_tcp();
    let server_http = free_tcp();

    let mut server = spawn(
        &bin_path("discordium-server"),
        &[
            ("PNET_ADDR", &node_addr.to_string()),
            (
                "DISCORDIUM_TOKEN_FILE",
                root.path().join("server-token").to_str().unwrap(),
            ),
            (
                "DISCORDIUM_DIR",
                root.path().join("server-record").to_str().unwrap(),
            ),
            ("DISCORDIUM_PUSH_PORT", &server_push.to_string()),
            ("DISCORDIUM_HTTP_PORT", &server_http.to_string()),
        ],
    );
    wait_log(&mut server, "discordium-server: record");

    let mut client = spawn(
        &bin_path("discordium-client"),
        &[
            ("PNET_ADDR", &node_addr.to_string()),
            (
                "DISCORDIUM_TOKEN_FILE",
                root.path().join("client-token").to_str().unwrap(),
            ),
            (
                "DISCORDIUM_DIR",
                root.path().join("client-record").to_str().unwrap(),
            ),
            ("DISCORDIUM_PUSH_PORT", &client_push.to_string()),
            ("DISCORDIUM_HTTP_PORT", &client_http.to_string()),
        ],
    );
    wait_log(
        &mut client,
        &format!("Page at http://127.0.0.1:{client_http}/"),
    );

    let seen = node.seen.lock().unwrap().clone();
    let aliases: Vec<_> = seen.iter().map(|row| row.alias.as_str()).collect();
    assert!(aliases.contains(&CLIENT_ALIAS), "{seen:?}");
    assert!(aliases.contains(&SERVER_ALIAS), "{seen:?}");
    assert!(seen.iter().all(|row| row.protocol == PROTOCOL));

    let (status, body) = exchange(client_http, "GET", "/", "");
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains("The server did not answer."));
    assert!(!body.contains("The server is not visible yet."));

    let (status, body) = exchange(client_http, "POST", "/", "title=same-machine");
    assert_eq!(status, 303, "{body}");

    let (status, body) = exchange(client_http, "GET", "/", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body.matches("same-machine").count(), 1, "{body}");

    let refused = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], server_http)),
        Duration::from_millis(200),
    );
    assert!(refused.is_err(), "the server listened on {server_http}");
    assert!(!root.path().join("client-record").join("record").exists());
    assert!(root.path().join("server-record").join("record").exists());
    assert_eq!(
        std::fs::read(root.path().join("client-token"))
            .unwrap()
            .len(),
        16
    );
    assert_eq!(
        std::fs::read(root.path().join("server-token"))
            .unwrap()
            .len(),
        16
    );

    drop(client);
    drop(server);
    drop(node);
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "discordium-both-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn start_node(socket: UdpSocket) -> Node {
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let flag = Arc::clone(&running);
    let registrations = Arc::clone(&seen);
    let thread = std::thread::spawn(move || node_loop(socket, flag, registrations));
    Node {
        running,
        seen,
        thread: Some(thread),
    }
}

fn node_loop(
    socket: UdpSocket,
    running: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<Mutex<Vec<Registration>>>,
) {
    let mut ports = HashMap::<[u8; 16], u16>::new();
    let mut buf = [0u8; 65535];
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        let (n, from) = match socket.recv_from(&mut buf) {
            Ok(pair) => pair,
            Err(err) if idle(&err) => continue,
            Err(_) => break,
        };
        let packet = &buf[..n];
        match packet.first().copied() {
            Some(0x00) => {
                let Some((alias, port, protocol)) = parse_register(packet) else {
                    let _ = socket.send_to(&[0x01], from);
                    continue;
                };
                seen.lock().unwrap().push(Registration {
                    alias: alias.clone(),
                    port,
                    protocol,
                });
                let Some((token, app)) = identity(&alias) else {
                    let _ = socket.send_to(&[0x01, 0x01], from);
                    continue;
                };
                ports.insert(app, port);
                let mut reply = vec![0x00];
                reply.extend_from_slice(&token);
                let _ = socket.send_to(&reply, from);
            }
            Some(0x02) if packet.len() == 17 => {
                let mut token = [0u8; 16];
                token.copy_from_slice(&packet[1..17]);
                let reply = match kind_of(&token) {
                    Some(alias) => get_data(alias),
                    None => vec![0x01],
                };
                let _ = socket.send_to(&reply, from);
            }
            Some(0x03) if packet.len() >= 49 => {
                let mut token = [0u8; 16];
                token.copy_from_slice(&packet[1..17]);
                let mut dest = [0u8; 16];
                dest.copy_from_slice(&packet[33..49]);
                let Some((_, sender)) = identity_of_token(&token) else {
                    continue;
                };
                let Some(port) = ports.get(&dest).copied() else {
                    continue;
                };
                let mut push = vec![0x04];
                push.extend_from_slice(&sender);
                push.extend_from_slice(&packet[49..]);
                let _ = socket.send_to(&push, SocketAddr::from(([127, 0, 0, 1], port)));
            }
            _ => {}
        }
    }
}

fn identity(alias: &str) -> Option<([u8; 16], [u8; 16])> {
    match alias {
        CLIENT_ALIAS => Some(([0xC1; 16], [0x11; 16])),
        SERVER_ALIAS => Some(([0x51; 16], [0x21; 16])),
        _ => None,
    }
}

fn identity_of_token(token: &[u8; 16]) -> Option<(&'static str, [u8; 16])> {
    if token == &[0xC1; 16] {
        Some((CLIENT_ALIAS, [0x11; 16]))
    } else if token == &[0x51; 16] {
        Some((SERVER_ALIAS, [0x21; 16]))
    } else {
        None
    }
}

fn kind_of(token: &[u8; 16]) -> Option<&'static str> {
    identity_of_token(token).map(|(alias, _)| alias)
}

fn get_data(local_alias: &str) -> Vec<u8> {
    let (local_app, local_token) = identity(local_alias).unwrap();
    let mut buf = vec![0x00];
    buf.extend_from_slice(&local_app);
    push_str(&mut buf, local_alias);
    buf.extend_from_slice(&[127, 0, 0, 1]);
    buf.extend_from_slice(&8790u16.to_be_bytes());
    buf.push(1);
    buf.extend_from_slice(&local_token);
    buf.extend_from_slice(&[0x10; 16]);
    push_str(&mut buf, "owner");
    buf.extend_from_slice(&[0x44; 16]);
    buf.push(1);
    buf.extend_from_slice(&[0x10; 16]);
    push_str(&mut buf, "laptop");
    buf.push(0);
    buf.push(0);
    buf.push(0);
    buf.extend_from_slice(&[0xA5; 32]);
    buf.extend_from_slice(&[0x5A; 32]);
    buf.extend_from_slice(&[0x11; 64]);
    buf.extend_from_slice(&42u64.to_le_bytes());
    push_str(&mut buf, "cert");
    buf.push(2);
    push_app(&mut buf, [0x11; 16], CLIENT_ALIAS, 8790);
    push_app(&mut buf, [0x21; 16], SERVER_ALIAS, 8791);
    buf.push(0);
    buf
}

fn push_app(buf: &mut Vec<u8>, id: [u8; 16], alias: &str, port: u16) {
    buf.extend_from_slice(&id);
    push_str(buf, alias);
    buf.extend_from_slice(&[127, 0, 0, 1]);
    buf.extend_from_slice(&port.to_be_bytes());
    buf.push(1);
}

fn push_str(buf: &mut Vec<u8>, text: &str) {
    buf.push(text.len() as u8);
    buf.extend_from_slice(text.as_bytes());
}

fn parse_register(packet: &[u8]) -> Option<(String, u16, String)> {
    if packet.first() != Some(&0x00) || packet.len() < 2 {
        return None;
    }
    let alias_len = packet[1] as usize;
    let alias_end = 2 + alias_len;
    if packet.len() < alias_end + 2 {
        return None;
    }
    let alias = std::str::from_utf8(&packet[2..alias_end]).ok()?.to_string();
    let port = u16::from_be_bytes(packet[alias_end..alias_end + 2].try_into().ok()?);
    let proto_at = alias_end + 2;
    let proto_len = *packet.get(proto_at)? as usize;
    let proto_end = proto_at + 1 + proto_len;
    if packet.len() != proto_end {
        return None;
    }
    let protocol = std::str::from_utf8(&packet[proto_at + 1..proto_end])
        .ok()?
        .to_string();
    Some((alias, port, protocol))
}

fn spawn(bin: &str, env: &[(&str, &str)]) -> Proc {
    let mut command = Command::new(bin);
    command
        .env_remove("DISCORDIUM_TOKEN_FILE")
        .env_remove("DISCORDIUM_DIR")
        .env_remove("DISCORDIUM_PUSH_PORT")
        .env_remove("DISCORDIUM_HTTP_PORT")
        .env_remove("PNET_ADDR")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let log = Arc::new(Mutex::new(String::new()));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    pump(stdout, Arc::clone(&log));
    pump(stderr, Arc::clone(&log));
    Proc { child, log }
}

fn pump(mut pipe: impl Read + Send + 'static, log: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => log
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    });
}

fn bin_path(name: &str) -> String {
    let key = format!("CARGO_BIN_EXE_{}", name.replace('-', "_"));
    if let Ok(path) = std::env::var(&key) {
        return path;
    }
    let target = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| format!("{}/target", env!("CARGO_MANIFEST_DIR")));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    format!("{target}/{profile}/{name}")
}

fn wait_log(proc: &mut Proc, needle: &str) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if proc.log.lock().unwrap().contains(needle) {
            return;
        }
        match proc.child.try_wait() {
            Ok(Some(status)) => panic!("exited {status}: {}", proc.log.lock().unwrap()),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("{err}: {}", proc.log.lock().unwrap()),
        }
    }
    panic!(
        "timed out waiting for {needle}: {}",
        proc.log.lock().unwrap()
    );
}

fn exchange(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let head = if body.is_empty() {
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    stream.write_all(head.as_bytes()).unwrap();
    let mut buf = Vec::new();
    loop {
        let mut tmp = [0u8; 2048];
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(err) if idle(&err) => break,
            Err(err) => panic!("{err}"),
        }
    }
    let text = String::from_utf8(buf).unwrap();
    let status = text
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn free_udp() -> u16 {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap().port()
}

fn free_tcp() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn idle(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::TimedOut
}
