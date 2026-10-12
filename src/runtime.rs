//! Live loops for a client and for the record server.
//!
//! The client retries HELLO until the record server acks it. The record server
//! also opens and lists conversations, and saves messages, for an approved
//! client. A new message is announced to each other attached client, which
//! then asks for history. The client draws that in a window. The server
//! process does not. Tests drive those same types on the fake socket.

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::create_list::{is_create_req, is_list_req, on_list};
use crate::directory::{
    approved_own_discordium, device_of_app, parse_get_data, record_discordium, Directory,
};
use crate::hello::{is_hello, DeviceHello, ServerHello, HELLO_BYTES};
use crate::node_api::{decode_push, NodeClient};
use crate::notice::{accept_create, accept_post, decode_added, DeviceHistory};
use crate::page::{is_reply, ConversationList, View};
use crate::post_history::{is_history_req, is_post, on_history};
use crate::store::{Conversation, Store};

pub fn run_device(
    client: NodeClient,
    push: UdpSocket,
    token: [u8; 16],
    directory: Directory,
) -> Result<(), String> {
    let mut session = DeviceSession::new(client, push, token, directory);
    if let Some(path) = drive_path() {
        return session.run_drive(Path::new(&path));
    }
    println!("Window open.");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    crate::desktop::run(session)
}

/// The client session the window drives. A drive file uses the same asks.
pub struct DeviceSession {
    client: NodeClient,
    push: UdpSocket,
    token: [u8; 16],
    directory: Directory,
    hello: DeviceHello,
    history: DeviceHistory,
    conversations: ConversationList,
    announced: bool,
    last_hello: Option<Instant>,
    notice: Option<View>,
    added: Option<Vec<Conversation>>,
    queued_added: VecDeque<([u8; 16], Vec<u8>)>,
}

impl DeviceSession {
    fn new(client: NodeClient, push: UdpSocket, token: [u8; 16], directory: Directory) -> Self {
        let mut hello = DeviceHello::new();
        let mut history = DeviceHistory::new();
        let mut conversations = ConversationList::new();
        hello.note_directory(&directory);
        history.note_directory(&directory);
        conversations.note_directory(&directory);
        Self {
            client,
            push,
            token,
            directory,
            hello,
            history,
            conversations,
            announced: false,
            last_hello: None,
            notice: None,
            added: None,
            queued_added: VecDeque::new(),
        }
    }

    pub fn hello_acked(&self) -> bool {
        self.hello.is_acked()
    }

    pub fn take_notice(&mut self) -> Option<View> {
        self.notice.take()
    }

    pub fn take_added(&mut self) -> Option<Vec<Conversation>> {
        self.added.take()
    }

    pub fn show_list(&mut self) -> View {
        self.with_ask(|conversations, ask| conversations.show_list(ask))
    }

    pub fn open_title(&mut self, title: &str) -> View {
        self.with_ask(|conversations, ask| conversations.open(title, ask))
    }

    pub fn open_thread(&mut self, id: [u8; 16]) -> View {
        self.with_ask(|conversations, ask| conversations.open_thread(id, ask))
    }

    pub fn send_text(&mut self, id: [u8; 16], text: &str) -> View {
        self.with_ask(|conversations, ask| conversations.send(id, text, ask))
    }

    pub fn pump(&mut self) {
        self.pump_hello();
        let _ = self.push.set_read_timeout(Some(Duration::from_millis(1)));
        while let Some((sender, payload)) = recv_push(&self.push) {
            self.on_push(sender, payload);
        }
    }

    fn run_drive(&mut self, path: &Path) -> Result<(), String> {
        let script =
            std::fs::read_to_string(path).map_err(|err| format!("read drive file: {err}"))?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.hello.is_acked() && Instant::now() < deadline {
            self.pump();
        }
        for line in script.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let view = if let Some(title) = line.strip_prefix("open ") {
                self.open_title(title)
            } else if line == "list" {
                self.show_list()
            } else {
                return Err(format!("unknown drive command: {line}"));
            };
            print_view(&view);
        }
        Ok(())
    }

    fn with_ask(
        &mut self,
        call: impl FnOnce(&mut ConversationList, &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>) -> View,
    ) -> View {
        let view = {
            let DeviceSession {
                client,
                push,
                token,
                directory,
                hello,
                history,
                conversations,
                queued_added,
                ..
            } = self;
            let mut state = AskState {
                hello,
                history,
                queued_added,
            };
            let mut ask =
                |request: &[u8]| ask_record(client, push, token, directory, &mut state, request);
            call(conversations, &mut ask)
        };
        self.drain_added();
        view
    }

    fn pump_hello(&mut self) {
        if self.hello.is_acked() {
            return;
        }
        let due = self
            .last_hello
            .map(|sent| sent.elapsed() >= Duration::from_secs(1))
            .unwrap_or(true);
        if !due {
            return;
        }
        if self.hello.hello_to_send().is_none() {
            refresh(&self.client, &self.token, &mut self.directory);
            self.hello.note_directory(&self.directory);
            self.history.note_directory(&self.directory);
            self.conversations.note_directory(&self.directory);
        }
        let Some(server) = self.hello.hello_to_send() else {
            return;
        };
        self.last_hello = Some(Instant::now());
        match self
            .client
            .send(&self.token, &server.device, &server.app, &HELLO_BYTES)
        {
            Ok(()) => {
                if !self.announced {
                    println!("Hello sent.");
                    self.announced = true;
                }
            }
            Err(err) => eprintln!("discordium: hello: {err}"),
        }
    }

    fn on_push(&mut self, sender: [u8; 16], payload: Vec<u8>) {
        on_device_push(
            &self.client,
            &self.token,
            &mut self.hello,
            &mut self.history,
            sender,
            &payload,
        );
        // A notice that arrives while an ask is waiting stays with DeviceHistory.
        // This read of the push socket must not run inside that wait.
        // An ADDED that arrives during that wait is queued and read after it.
        if let Some(view) = self.read_notice(sender, &payload) {
            self.notice = Some(view);
        }
        if let Some(rows) = self.read_added(sender, &payload) {
            self.added = Some(rows);
        }
        self.drain_added();
    }

    fn read_notice(&mut self, sender: [u8; 16], payload: &[u8]) -> Option<View> {
        let DeviceSession {
            client,
            push,
            token,
            directory,
            hello,
            history,
            conversations,
            queued_added,
            ..
        } = self;
        let mut state = AskState {
            hello,
            history,
            queued_added,
        };
        conversations.on_notice(sender, payload, |request| {
            ask_record(client, push, token, directory, &mut state, request)
        })
    }

    fn read_added(&mut self, sender: [u8; 16], payload: &[u8]) -> Option<Vec<Conversation>> {
        let DeviceSession {
            client,
            push,
            token,
            directory,
            hello,
            history,
            conversations,
            queued_added,
            ..
        } = self;
        let mut state = AskState {
            hello,
            history,
            queued_added,
        };
        conversations.on_added(sender, payload, |request| {
            ask_record(client, push, token, directory, &mut state, request)
        })
    }

    fn drain_added(&mut self) {
        while let Some((sender, payload)) = self.queued_added.pop_front() {
            if let Some(rows) = self.read_added(sender, &payload) {
                self.added = Some(rows);
            }
        }
    }
}

fn drive_path() -> Option<String> {
    let path = std::env::var("DISCORDIUM_DRIVE").ok()?;
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn print_view(view: &View) {
    if let Some(note) = view.note {
        println!("note {note}");
    }
    if view.thread.is_none() {
        for row in &view.conversations {
            println!("conversation {}", row.title);
        }
    } else {
        for message in &view.messages {
            println!("message {}", message.text);
        }
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
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
        if is_create_req(&payload) {
            if let Some(accepted) = accept_create(
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
                    &accepted.resp,
                    "create",
                );
                for (app, notice) in accepted.notices {
                    send_to_sender(client, &token, &directory, &app, &notice, "added");
                }
            }
            continue;
        }
        let reply = if is_list_req(&payload) {
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

/// Client state an ask updates while it waits for the record server.
struct AskState<'a> {
    hello: &'a mut DeviceHello,
    history: &'a mut DeviceHistory,
    queued_added: &'a mut VecDeque<([u8; 16], Vec<u8>)>,
}

fn ask_record(
    client: &NodeClient,
    push: &UdpSocket,
    token: &[u8; 16],
    directory: &Directory,
    state: &mut AskState<'_>,
    request: &[u8],
) -> Option<Vec<u8>> {
    let server = record_discordium(directory)?;
    client
        .send(token, &server.device, &server.app, request)
        .ok()?;
    // The window drains the socket on a short timeout. A reply wait must not
    // spin on that same timeout.
    let _ = push.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let Some((sender, payload)) = recv_push(push) else {
            continue;
        };
        let matched = sender == server.app && is_reply(request, &payload);
        on_device_push(client, token, state.hello, state.history, sender, &payload);
        if sender == server.app && decode_added(&payload).is_some() {
            state.queued_added.push_back((sender, payload.clone()));
        }
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
