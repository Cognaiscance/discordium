//! Live HELLO loop. The fake socket in tests drives the same hello types.

use std::net::UdpSocket;
use std::time::Duration;

use crate::directory::{approved_own_discordium, device_of_app, parse_get_data, Directory};
use crate::hello::{is_hello, DeviceHello, ServerHello, HELLO_BYTES};
use crate::node_api::{decode_push, NodeClient};
use crate::store::Store;

pub fn run_device(
    client: &NodeClient,
    push: &UdpSocket,
    token: [u8; 16],
    mut directory: Directory,
) -> Result<(), String> {
    push.set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|err| format!("push socket: {err}"))?;
    let mut hello = DeviceHello::new();
    hello.note_directory(&directory);
    let mut announced = false;
    loop {
        if !hello.is_acked() {
            if hello.hello_to_send().is_none() {
                refresh(client, &token, &mut directory);
                hello.note_directory(&directory);
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
            } else {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        if let Some((sender, payload)) = recv_push(push) {
            let was_acked = hello.is_acked();
            hello.on_push(sender, &payload);
            if hello.is_acked() && !was_acked {
                println!("Hello acked.");
            }
        }
    }
}

pub fn run_record(
    client: &NodeClient,
    push: &UdpSocket,
    token: [u8; 16],
    mut directory: Directory,
    _store: Store,
) -> Result<(), String> {
    push.set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|err| format!("push socket: {err}"))?;
    let mut server = ServerHello::new();
    loop {
        let Some((sender, payload)) = recv_push(push) else {
            continue;
        };
        if is_hello(&payload) && !approved_own_discordium(&directory, &sender) {
            refresh(client, &token, &mut directory);
        }
        let before = server.attached().len();
        let Some(ack) = server.on_hello(&directory, sender, &payload) else {
            continue;
        };
        if server.attached().len() > before {
            println!("Attached {}.", prefix(&sender));
        }
        let Some(device) = device_of_app(&directory, &sender) else {
            continue;
        };
        if let Err(err) = client.send(&token, &device, &sender, &ack) {
            eprintln!("discordium: hello ack: {err}");
        }
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
