//! Live loops for a device and for the record server.
//!
//! The device retries HELLO until the record server acks it. The record server
//! also opens and lists conversations, and saves messages, for an approved
//! device. A new message is announced to each other attached device, which
//! then asks for history. A device also serves the conversation list. Tests
//! drive those same types on the fake socket.

use std::net::{TcpListener, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::create_list::{is_create_req, is_list_req, on_create, on_list};
use crate::directory::{
    approved_own_discordium, device_of_app, parse_get_data, record_discordium, Directory,
};
use crate::hello::{is_hello, DeviceHello, ServerHello, HELLO_BYTES};
use crate::node_api::{decode_push, NodeClient};
use crate::notice::{accept_post, DeviceHistory};
use crate::page::{is_reply, poll_page, ConversationList};
use crate::post_history::{is_history_req, is_post, on_history};
use crate::store::Store;

pub fn run_device(
    client: &NodeClient,
    push: &UdpSocket,
    token: [u8; 16],
    mut directory: Directory,
    http_port: u16,
) -> Result<(), String> {
    push.set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|err| format!("push socket: {err}"))?;
    let page_socket = TcpListener::bind(("127.0.0.1", http_port))
        .map_err(|err| format!("bind 127.0.0.1:{http_port}: {err}"))?;
    page_socket
        .set_nonblocking(true)
        .map_err(|err| format!("page socket: {err}"))?;
    println!("Page at http://127.0.0.1:{http_port}/");
    let mut hello = DeviceHello::new();
    let mut history = DeviceHistory::new();
    let mut conversations = ConversationList::new();
    hello.note_directory(&directory);
    history.note_directory(&directory);
    conversations.note_directory(&directory);
    let mut announced = false;
    loop {
        if !hello.is_acked() {
            if hello.hello_to_send().is_none() {
                refresh(client, &token, &mut directory);
                hello.note_directory(&directory);
                history.note_directory(&directory);
                conversations.note_directory(&directory);
            }
            if let Some(server) = hello.hello_to_send() {
                match client.send(&token, &server.device, &server.app, &HELLO_BYTES) {
                    Ok(()) => {
                        if !announced {
                            println!("Hello sent.");
                            announced = true;
                        }
                    }
                    Err(err) => eprintln!("discordium: hello: {err}"),
                }
            }
        }
        poll_page(&page_socket, &mut conversations, |request| {
            ask_record(
                client,
                push,
                &token,
                &directory,
                &mut hello,
                &mut history,
                request,
            )
        });
        if let Some((sender, payload)) = recv_push(push) {
            on_device_push(client, &token, &mut hello, &mut history, sender, &payload);
        }
    }
}

pub fn run_record(
    client: &NodeClient,
    push: &UdpSocket,
    token: [u8; 16],
    mut directory: Directory,
    mut store: Store,
) -> Result<(), String> {
    push.set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|err| format!("push socket: {err}"))?;
    let mut server = ServerHello::new();
    loop {
        let Some((sender, payload)) = recv_push(push) else {
            continue;
        };
        if known_request(&payload) && !approved_own_discordium(&directory, &sender) {
            refresh(client, &token, &mut directory);
        }
        let before = server.attached().len();
        if let Some(ack) = server.on_hello(&directory, sender, &payload) {
            if server.attached().len() > before {
                println!("Attached {}.", prefix(&sender));
            }
            send_to_sender(client, &token, &directory, &sender, &ack, "hello ack");
            continue;
        }
        if is_post(&payload) {
            if let Some(accepted) = accept_post(
                &directory,
                &mut store,
                server.attached(),
                sender,
                &payload,
                now_ms(),
            ) {
                send_to_sender(
                    client,
                    &token,
                    &directory,
                    &sender,
                    &accepted.ack,
                    "post ack",
                );
                for (app, notice) in accepted.notices {
                    send_to_sender(client, &token, &directory, &app, &notice, "notice");
                }
            }
            continue;
        }
        let reply = if is_create_req(&payload) {
            on_create(&directory, &mut store, sender, &payload, now_ms())
        } else if is_list_req(&payload) {
            on_list(&directory, &store, sender, &payload)
        } else if is_history_req(&payload) {
            on_history(&directory, &store, sender, &payload)
        } else {
            None
        };
        if let Some(reply) = reply {
            send_to_sender(client, &token, &directory, &sender, &reply, "send");
        }
    }
}

fn known_request(payload: &[u8]) -> bool {
    is_hello(payload)
        || is_create_req(payload)
        || is_list_req(payload)
        || is_post(payload)
        || is_history_req(payload)
}

fn ask_record(
    client: &NodeClient,
    push: &UdpSocket,
    token: &[u8; 16],
    directory: &Directory,
    hello: &mut DeviceHello,
    history: &mut DeviceHistory,
    request: &[u8],
) -> Option<Vec<u8>> {
    let server = record_discordium(directory)?;
    client
        .send(token, &server.device, &server.app, request)
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let Some((sender, payload)) = recv_push(push) else {
            continue;
        };
        let matched = sender == server.app && is_reply(request, &payload);
        on_device_push(client, token, hello, history, sender, &payload);
        if matched {
            return Some(payload);
        }
    }
    None
}

fn on_device_push(
    client: &NodeClient,
    token: &[u8; 16],
    hello: &mut DeviceHello,
    history: &mut DeviceHistory,
    sender: [u8; 16],
    payload: &[u8],
) {
    let was_acked = hello.is_acked();
    hello.on_push(sender, payload);
    if hello.is_acked() && !was_acked {
        println!("Hello acked.");
    }
    if let Some((peer, request)) = history.on_notice(sender, payload) {
        if let Err(err) = client.send(token, &peer.device, &peer.app, &request) {
            eprintln!("discordium: history: {err}");
        }
    }
    if let Some((peer, request)) = history.on_history(sender, payload) {
        if let Err(err) = client.send(token, &peer.device, &peer.app, &request) {
            eprintln!("discordium: history: {err}");
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn send_to_sender(
    client: &NodeClient,
    token: &[u8; 16],
    directory: &Directory,
    sender: &[u8; 16],
    payload: &[u8],
    label: &str,
) {
    let Some(device) = device_of_app(directory, sender) else {
        return;
    };
    if let Err(err) = client.send(token, &device, sender, payload) {
        eprintln!("discordium: {label}: {err}");
    }
}

fn refresh(client: &NodeClient, token: &[u8; 16], directory: &mut Directory) {
    match client.get_data(token) {
        Ok(reply) => match parse_get_data(&reply) {
            Ok(fresh) => *directory = fresh,
            Err(err) => eprintln!("discordium: get-data reply: {err}"),
        },
        Err(err) => eprintln!("discordium: get-data failed: {err}"),
    }
}

fn recv_push(push: &UdpSocket) -> Option<([u8; 16], Vec<u8>)> {
    let mut buf = [0u8; 65535];
    let n = push.recv(&mut buf).ok()?;
    let (sender, payload) = decode_push(&buf[..n])?;
    Some((sender, payload.to_vec()))
}

fn prefix(id: &[u8; 16]) -> String {
    id.iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
