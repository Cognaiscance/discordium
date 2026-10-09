//! Discordium. One binary, started on each machine that takes part.
//!
//! The process registers with the local node, remembers the token, and chooses
//! device, record, or standby from get-data. The record role keeps conversations,
//! acks HELLO from an approved device, and opens, lists, or extends a conversation
//! when that device asks. A device retries HELLO until the ack. A standby does not
//! open the store and does not answer.

mod create_list;
mod directory;
mod hello;
mod link;
mod node_api;
mod post_history;
mod runtime;
mod store;

use std::net::SocketAddr;
use std::process::ExitCode;

use directory::{choose_role, parse_get_data, role_summary, Role, APP_ALIAS};
use node_api::{
    load_token, record_dir, save_token, token_path, NodeClient, DEFAULT_PNET_ADDR,
    DEFAULT_PUSH_PORT,
};
use store::Store;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("discordium: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let pnet_addr = pnet_addr()?;
    let push_port = push_port()?;
    let token_path = token_path();

    let push = std::net::UdpSocket::bind(("127.0.0.1", push_port))
        .map_err(|err| format!("bind 127.0.0.1:{push_port}: {err}"))?;
    let client =
        NodeClient::connect(pnet_addr).map_err(|err| format!("connect to {pnet_addr}: {err}"))?;

    eprintln!("discordium: registering with {pnet_addr}");
    let token = match client.register(push_port) {
        Ok(token) => {
            save_token(&token_path, &token).map_err(|err| format!("save token: {err}"))?;
            token
        }
        Err(err) => match load_token(&token_path) {
            Ok(Some(token)) => {
                eprintln!("discordium: register failed ({err}); using the saved token");
                token
            }
            Ok(None) => return Err(format!("register failed: {err}")),
            Err(load_err) => {
                return Err(format!("register failed: {err}; saved token: {load_err}"));
            }
        },
    };

    let reply = client
        .get_data(&token)
        .map_err(|err| format!("get-data failed: {err}"))?;
    let dir = parse_get_data(&reply).map_err(|err| err.to_string())?;
    if dir.local_app_alias != APP_ALIAS {
        return Err(format!(
            "this token is for '{}', not {APP_ALIAS}",
            dir.local_app_alias
        ));
    }
    let role = choose_role(&dir).map_err(|err| err.to_string())?;

    println!("discordium: {role}");
    println!("{}", role_summary(&dir, role));
    if !dir.local_app_approved {
        println!("The local node has not approved this app yet.");
    }
    println!(
        "token {}… at {}",
        token_prefix(&token),
        token_path.display()
    );
    println!("Push port {push_port}. Stop with Ctrl-C.");
    match role {
        Role::Record => {
            let path = record_dir();
            let store = Store::open(&path).map_err(|err| format!("open record: {err}"))?;
            println!(
                "Record directory {} ({} conversations).",
                path.display(),
                store.list_conversations().len()
            );
            runtime::run_record(&client, &push, token, dir, store)
        }
        Role::Device => runtime::run_device(&client, &push, token, dir),
        Role::Standby => {
            std::thread::park();
            Ok(())
        }
    }
}

fn pnet_addr() -> Result<SocketAddr, String> {
    let text = env_or("PNET_ADDR", DEFAULT_PNET_ADDR);
    let addr: SocketAddr = text
        .parse()
        .map_err(|_| format!("PNET_ADDR '{text}' is not a socket address"))?;
    if !addr.is_ipv4() {
        return Err("PNET_ADDR must be IPv4".to_string());
    }
    Ok(addr)
}

fn push_port() -> Result<u16, String> {
    let text = env_or("DISCORDIUM_PUSH_PORT", &DEFAULT_PUSH_PORT.to_string());
    let port: u16 = text
        .parse()
        .map_err(|_| format!("DISCORDIUM_PUSH_PORT '{text}' is not a port"))?;
    if port == 0 {
        return Err("DISCORDIUM_PUSH_PORT must not be 0".to_string());
    }
    Ok(port)
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => value,
        _ => default.to_string(),
    }
}

fn token_prefix(token: &[u8; 16]) -> String {
    token
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
