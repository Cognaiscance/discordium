//! Conversation list and one thread on the device.
//!
//! `GET /` renders only the conversations `LIST_RESP` returned. `POST /`
//! opens one and waits for `CREATE_RESP` before sending the browser to that
//! list. The row is not drawn from the form.
//!
//! `GET /t/<id>` renders only the messages `HISTORY_RESP` returned. `POST`
//! there waits for `POST_ACK` and draws the saved text. A `NOTICE` for that
//! conversation sends `HISTORY_REQ` and adds the new messages.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::create_list::{
    decode_create_req, decode_create_resp, decode_list_resp, encode_create_req, encode_list_req,
    is_create_req, is_list_req,
};
use crate::directory::{record_discordium, Directory};
use crate::notice::decode_notice;
use crate::post_history::{
    decode_history_req, decode_history_resp, decode_post, decode_post_ack, encode_history_req,
    encode_post, is_history_req, is_post,
};
use crate::store::{Conversation, Message};

pub const DEFAULT_HTTP_PORT: u16 = 8788;

pub struct PageRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

pub struct PageResponse {
    pub status: u16,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

pub struct ConversationList {
    seq: u64,
    server: Option<[u8; 16]>,
    threads: HashMap<[u8; 16], Vec<Message>>,
}

impl ConversationList {
    pub fn new() -> Self {
        Self {
            seq: 0,
            server: None,
            threads: HashMap::new(),
        }
    }

    pub fn note_directory(&mut self, dir: &Directory) {
        self.server = record_discordium(dir).map(|peer| peer.app);
    }

    pub fn handle(
        &mut self,
        request: &PageRequest,
        mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> PageResponse {
        if request.path == "/" {
            return match request.method.as_str() {
                "GET" => {
                    let (rows, note) = self.fetch(&mut ask);
                    self.render(&rows, note)
                }
                "POST" => self.open(request, &mut ask),
                _ => text(405, None, "Method not allowed"),
            };
        }
        let Some(id) = parse_thread(&request.path) else {
            return text(404, None, "Not found");
        };
        match request.method.as_str() {
            "GET" => self.show_thread(id, &mut ask),
            "POST" => self.send_message(id, request, &mut ask),
            _ => text(405, None, "Method not allowed"),
        }
    }

    /// History for this notice, then the thread with those messages added.
    ///
    /// None when `sender` is not the record server or the payload is not a notice.
    pub fn on_notice(
        &mut self,
        sender: [u8; 16],
        payload: &[u8],
        mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Option<PageResponse> {
        if self.server != Some(sender) {
            return None;
        }
        let conversation = decode_notice(payload)?;
        let after = self
            .threads
            .get(&conversation)
            .and_then(|rows| rows.last().map(|message| message.id));
        let (new_rows, note) = self.load_history(conversation, after, &mut ask);
        {
            let rows = self.threads.entry(conversation).or_default();
            for message in new_rows {
                if rows.iter().any(|have| have.id == message.id) {
                    continue;
                }
                rows.push(message);
            }
        }
        Some(self.render_thread(&conversation, note))
    }

    fn open(
        &mut self,
        request: &PageRequest,
        ask: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> PageResponse {
        let Some(title) = form_title(&request.body) else {
            let (rows, _) = self.fetch(ask);
            return self.render(&rows, Some("A title is required."));
        };
        if title.is_empty() {
            let (rows, _) = self.fetch(ask);
            return self.render(&rows, Some("A title is required."));
        }
        if self.server.is_none() {
            return self.render(&[], Some("The server is not visible yet."));
        }
        let Some(payload) = encode_create_req(self.next_id(), &title) else {
            let (rows, _) = self.fetch(ask);
            return self.render(&rows, Some("That title is too long."));
        };
        let created = ask(&payload).and_then(|resp| decode_create_resp(&resp));
        let id = decode_create_req(&payload).map(|(id, _)| id);
        if created.is_some_and(|conversation| Some(conversation.id) == id) {
            return PageResponse {
                status: 303,
                location: Some("/".to_string()),
                body: Vec::new(),
            };
        }
        let (rows, _) = self.fetch(ask);
        self.render(&rows, Some("The server did not answer."))
    }

    fn fetch(
        &self,
        ask: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> (Vec<Conversation>, Option<&'static str>) {
        if self.server.is_none() {
            return (Vec::new(), Some("The server is not visible yet."));
        }
        let mut rows = Vec::new();
        let mut after = None;
        loop {
            let Some(resp) = ask(&encode_list_req(after)) else {
                return (rows, Some("The server did not answer."));
            };
            let Some(page) = decode_list_resp(&resp) else {
                return (rows, Some("The server did not answer."));
            };
            let last = page
                .conversations
                .last()
                .map(|conversation| conversation.id);
            let more = page.more;
            rows.extend(page.conversations);
            if !more {
                return (rows, None);
            }
            let Some(last) = last else {
                return (rows, None);
            };
            after = Some(last);
        }
    }

    fn render(&self, rows: &[Conversation], note: Option<&str>) -> PageResponse {
        let mut body = String::from(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Discordium</title></head><body><h1>Conversations</h1>",
        );
        if let Some(note) = note {
            body.push_str("<p>");
            body.push_str(&escape(note));
            body.push_str("</p>");
        }
        body.push_str("<ul>");
        for row in rows {
            body.push_str("<li class=\"conversation\"><a href=\"t/");
            body.push_str(&hex_id(&row.id));
            body.push_str("\">");
            body.push_str(&escape(&row.title));
            body.push_str("</a></li>");
        }
        body.push_str(
            "</ul><form method=\"post\" action=\"/\"><label for=\"title\">Title</label> <input id=\"title\" name=\"title\" maxlength=\"255\"> <button type=\"submit\">Open</button></form></body></html>",
        );
        text(200, None, &body)
    }

    fn show_thread(
        &mut self,
        id: [u8; 16],
        ask: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> PageResponse {
        if self.server.is_none() {
            return self.render_thread(&id, Some("The server is not visible yet."));
        }
        let (rows, note) = self.load_history(id, None, ask);
        if note.is_none() || !rows.is_empty() {
            self.threads.insert(id, rows);
        }
        self.render_thread(&id, note)
    }

    fn send_message(
        &mut self,
        id: [u8; 16],
        request: &PageRequest,
        ask: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> PageResponse {
        let Some(text) = form_field(&request.body, "text") else {
            return self.thread_with_note(id, ask, "A message is required.");
        };
        if text.is_empty() {
            return self.thread_with_note(id, ask, "A message is required.");
        }
        if self.server.is_none() {
            return self.render_thread(&id, Some("The server is not visible yet."));
        }
        let Some(payload) = encode_post(self.next_id(), id, &text) else {
            return self.thread_with_note(id, ask, "That message is too long.");
        };
        let saved = ask(&payload).and_then(|resp| decode_post_ack(&resp));
        let sent = decode_post(&payload).map(|(message_id, _, _)| message_id);
        let saved = saved.filter(|message| Some(message.id) == sent && message.conversation == id);
        let Some(saved) = saved else {
            return self.thread_with_note(id, ask, "The server did not answer.");
        };
        let (mut rows, note) = self.load_history(id, None, ask);
        if note.is_some() {
            rows = self.threads.get(&id).cloned().unwrap_or_default();
        }
        if let Some(slot) = rows.iter_mut().find(|message| message.id == saved.id) {
            *slot = saved;
        } else {
            rows.push(saved);
        }
        self.threads.insert(id, rows);
        self.render_thread(&id, None)
    }

    fn thread_with_note(
        &mut self,
        id: [u8; 16],
        ask: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
        note: &'static str,
    ) -> PageResponse {
        if self.server.is_some() {
            let (rows, err) = self.load_history(id, None, ask);
            if err.is_none() {
                self.threads.insert(id, rows);
            }
        }
        self.render_thread(&id, Some(note))
    }

    fn load_history(
        &self,
        id: [u8; 16],
        mut after: Option<[u8; 16]>,
        ask: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> (Vec<Message>, Option<&'static str>) {
        let mut rows = Vec::new();
        loop {
            let Some(resp) = ask(&encode_history_req(id, after)) else {
                return (rows, Some("The server did not answer."));
            };
            let Some(page) = decode_history_resp(&resp) else {
                return (rows, Some("The server did not answer."));
            };
            if page.conversation != id {
                return (rows, Some("The server did not answer."));
            }
            let last = page.messages.last().map(|message| message.id);
            let more = page.more;
            rows.extend(page.messages);
            if !more {
                return (rows, None);
            }
            let Some(last) = last else {
                return (rows, None);
            };
            after = Some(last);
        }
    }

    fn render_thread(&self, id: &[u8; 16], note: Option<&str>) -> PageResponse {
        let mut body = String::from(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Discordium</title></head><body><p><a href=\"/\">Conversations</a></p><ul>",
        );
        if let Some(rows) = self.threads.get(id) {
            for message in rows {
                body.push_str("<li class=\"message\">");
                body.push_str(&escape(&message.text));
                body.push_str("</li>");
            }
        }
        body.push_str("</ul>");
        if let Some(note) = note {
            body.push_str("<p>");
            body.push_str(&escape(note));
            body.push_str("</p>");
        }
        body.push_str("<form method=\"post\" action=\"/t/");
        body.push_str(&hex_id(id));
        body.push_str(
            "\"><label for=\"text\">Message</label> <input id=\"text\" name=\"text\"> <button type=\"submit\">Send</button></form></body></html>",
        );
        text(200, None, &body)
    }

    fn next_id(&mut self) -> [u8; 16] {
        self.seq = self.seq.wrapping_add(1);
        let mut id = [0u8; 16];
        id[8..].copy_from_slice(&self.seq.to_be_bytes());
        id
    }
}

/// True when `response` is the record server's reply to this request.
pub fn is_reply(request: &[u8], response: &[u8]) -> bool {
    if is_create_req(request) {
        let Some((id, _)) = decode_create_req(request) else {
            return false;
        };
        return decode_create_resp(response).is_some_and(|conversation| conversation.id == id);
    }
    if is_list_req(request) {
        return decode_list_resp(response).is_some();
    }
    if is_post(request) {
        let Some((id, conversation, _)) = decode_post(request) else {
            return false;
        };
        return decode_post_ack(response)
            .is_some_and(|message| message.id == id && message.conversation == conversation);
    }
    if is_history_req(request) {
        let Some((conversation, _)) = decode_history_req(request) else {
            return false;
        };
        return decode_history_resp(response).is_some_and(|page| page.conversation == conversation);
    }
    false
}

pub fn poll_page(
    listener: &TcpListener,
    page: &mut ConversationList,
    mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) {
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                match read_request(&mut stream) {
                    Ok(request) => {
                        let response = page.handle(&request, &mut ask);
                        let _ = write_response(&mut stream, &response);
                    }
                    Err(err) => eprintln!("discordium: page: {err}"),
                }
            }
            Err(err) if idle(&err) => break,
            Err(err) => {
                eprintln!("discordium: page: {err}");
                break;
            }
        }
    }
}

pub fn read_request(stream: &mut TcpStream) -> Result<PageRequest, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        if buf.len() > 65536 {
            return Err("request is too large".to_string());
        }
        let n = stream.read(&mut tmp).map_err(|err| err.to_string())?;
        if n == 0 {
            return Err("connection closed".to_string());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = find_header_end(&buf) {
            break end;
        }
    };
    let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| "request is not utf-8")?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or("/").to_string();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .trim()
                .parse()
                .map_err(|_| "bad content-length".to_string())?;
        }
    }
    if content_length > 8192 {
        return Err("body is too large".to_string());
    }
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).map_err(|err| err.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    Ok(PageRequest { method, path, body })
}

pub fn write_response(stream: &mut TcpStream, response: &PageResponse) -> Result<(), String> {
    let reason = match response.status {
        303 => "See Other",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        response.status,
        response.body.len()
    );
    if let Some(location) = &response.location {
        head.push_str("Location: ");
        head.push_str(location);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|err| err.to_string())?;
    stream
        .write_all(&response.body)
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn text(status: u16, location: Option<String>, body: &str) -> PageResponse {
    PageResponse {
        status,
        location,
        body: body.as_bytes().to_vec(),
    }
}

fn form_title(body: &[u8]) -> Option<String> {
    form_field(body, "title")
}

fn form_field(body: &[u8], name: &str) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    for pair in text.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return Some(percent_decode(value));
        }
    }
    None
}

fn parse_thread(path: &str) -> Option<[u8; 16]> {
    let hex = path.strip_prefix("/t/")?;
    if hex.len() != 32
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let mut id = [0u8; 16];
    for index in 0..16 {
        id[index] = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(id)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    index += 3;
                } else {
                    out.push(b'%');
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn idle(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_list::{on_create, on_list, CREATE_REQ, LIST_REQ};
    use crate::directory::{AppRecord, DeviceRecord, Directory, Grade};
    use crate::hello::{ServerHello, HELLO_BYTES};
    use crate::link::FakeSocket;
    use crate::notice::accept_post;
    use crate::post_history::{
        decode_history_req, decode_post, encode_post, on_history, on_post, HISTORY_REQ,
        HISTORY_RESP, POST, POST_ACK,
    };
    use crate::store::Store;
    use std::fs;
    use std::net::TcpStream;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn ident(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn room() -> (Directory, [u8; 16], [u8; 16]) {
        let laptop_app = ident(0x11);
        let home_app = ident(0x21);
        let dir = Directory {
            local_app_id: ident(1),
            local_app_alias: "discordium".into(),
            local_app_approved: true,
            token: ident(2),
            local_device: ident(0x10),
            owner_alias: "owner".into(),
            owner_id: ident(3),
            own_devices: vec![
                DeviceRecord {
                    id: ident(0x10),
                    alias: "laptop".into(),
                    grade: Grade::Device,
                    sg_rank: 0,
                    apps: vec![AppRecord {
                        id: laptop_app,
                        alias: "discordium".into(),
                        approved: true,
                    }],
                },
                DeviceRecord {
                    id: ident(0x20),
                    alias: "home".into(),
                    grade: Grade::Server,
                    sg_rank: 1,
                    apps: vec![AppRecord {
                        id: home_app,
                        alias: "discordium".into(),
                        approved: true,
                    }],
                },
            ],
            contacts: Vec::new(),
        };
        (dir, laptop_app, home_app)
    }

    struct Temp(PathBuf);

    impl Temp {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("discordium-{label}-{}-{nanos}", std::process::id()));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn answer(
        dir: &Directory,
        store: &mut Store,
        from: [u8; 16],
        to: [u8; 16],
        payload: &[u8],
        seen: &mut Vec<u8>,
    ) -> Option<Vec<u8>> {
        seen.push(*payload.get(1)?);
        let mut link = FakeSocket::new();
        link.deliver(from, to, payload);
        let (sender, got) = link.take(to)?;
        let reply = if is_create_req(&got) {
            on_create(dir, store, sender, &got, 1_700)
        } else if is_list_req(&got) {
            on_list(dir, store, sender, &got)
        } else if is_post(&got) {
            on_post(dir, store, sender, &got, 1_700)
        } else if is_history_req(&got) {
            on_history(dir, store, sender, &got)
        } else {
            None
        }?;
        link.deliver(to, from, &reply);
        let (_sender, back) = link.take(from)?;
        Some(back)
    }

    fn exchange(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
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
        let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }

    fn rows(body: &str) -> usize {
        body.matches("class=\"conversation\"").count()
    }

    #[test]
    fn page_lists_only_the_created_conversation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (dir, laptop_app, home_app) = room();
            let tmp = Temp::new("page");
            let mut store = Store::open(tmp.path()).unwrap();
            let mut page = ConversationList::new();
            page.note_directory(&dir);
            let mut seen = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream).unwrap();
                let response = page.handle(&request, |payload| {
                    answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
                });
                write_response(&mut stream, &response).unwrap();
            }
            seen
        });

        let (status, body) = exchange(port, "GET", "/", "");
        assert_eq!(status, 200);
        assert_eq!(rows(&body), 0);
        assert!(!body.contains("general"));
        assert!(!body.contains("http://"));

        let (status, body) = exchange(port, "POST", "/", "title=general");
        assert_eq!(status, 303);
        assert!(body.is_empty());

        let (status, body) = exchange(port, "GET", "/", "");
        assert_eq!(status, 200);
        assert_eq!(rows(&body), 1);
        assert_eq!(body.matches("general").count(), 1);
        assert!(body.contains("href=\"t/"));
        assert!(!body.contains("http://"));
        assert!(!body.contains("other"));

        let seen = server.join().unwrap();
        assert_eq!(seen, vec![LIST_REQ, CREATE_REQ, LIST_REQ]);
    }

    #[test]
    fn page_does_not_show_a_title_the_server_did_not_save() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let request = PageRequest {
            method: "POST".into(),
            path: "/".into(),
            body: b"title=hidden".to_vec(),
        };
        let response = page.handle(&request, |_| None);
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(rows(&body), 0);
        assert!(!body.contains("hidden"));
        assert!(body.contains("The server did not answer."));
    }

    #[test]
    fn page_follows_every_list_response() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("pages");
        let mut store = Store::open(tmp.path()).unwrap();
        for n in 0..5u8 {
            let prefix = format!("row{n}-");
            let title = format!("{prefix}{}", "x".repeat(200 - prefix.len()));
            store
                .create_conversation(ident(0x30 + n), &title, u64::from(n))
                .unwrap();
        }
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let response = page.handle(
            &PageRequest {
                method: "GET".into(),
                path: "/".into(),
                body: Vec::new(),
            },
            |payload| answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen),
        );
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(rows(&body), 5);
        assert!(seen.iter().all(|kind| *kind == LIST_REQ));
        assert!(seen.len() > 1);
        for n in 0..5u8 {
            assert!(body.contains(&format!("row{n}-")));
        }
        assert!(!body.contains("row5-"));
    }

    #[test]
    fn page_escapes_the_title_from_the_list() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("escape");
        let mut store = Store::open(tmp.path()).unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let opened = page.handle(
            &PageRequest {
                method: "POST".into(),
                path: "/".into(),
                body: b"title=a%3Cb+c".to_vec(),
            },
            |payload| answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen),
        );
        assert_eq!(opened.status, 303);
        let response = page.handle(
            &PageRequest {
                method: "GET".into(),
                path: "/".into(),
                body: Vec::new(),
            },
            |payload| answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen),
        );
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(rows(&body), 1);
        assert!(body.contains("a&lt;b c"));
        assert!(!body.contains("a<b"));
    }

    fn messages(body: &str) -> usize {
        body.matches("class=\"message\"").count()
    }

    fn thread_path(id: &[u8; 16]) -> String {
        format!("/t/{}", hex_id(id))
    }

    fn house() -> (Directory, [u8; 16], [u8; 16], [u8; 16]) {
        let (mut dir, laptop_app, home_app) = room();
        let phone_app = ident(0x31);
        dir.own_devices.push(DeviceRecord {
            id: ident(0x30),
            alias: "phone".into(),
            grade: Grade::Device,
            sg_rank: 0,
            apps: vec![AppRecord {
                id: phone_app,
                alias: "discordium".into(),
                approved: true,
            }],
        });
        (dir, laptop_app, phone_app, home_app)
    }

    fn attach(
        link: &mut FakeSocket,
        server: &mut ServerHello,
        dir: &Directory,
        app: [u8; 16],
        home_app: [u8; 16],
    ) {
        link.deliver(app, home_app, &HELLO_BYTES);
        let (sender, payload) = link.take(home_app).unwrap();
        let ack = server.on_hello(dir, sender, &payload).unwrap();
        link.deliver(home_app, app, &ack);
        let _ = link.take(app).unwrap();
    }

    #[test]
    fn page_shows_an_empty_thread_then_the_saved_text_and_a_notice() {
        let (dir, laptop_app, phone_app, home_app) = house();
        let tmp = Temp::new("thread");
        let mut store = Store::open(tmp.path()).unwrap();
        let id = ident(0x71);
        store.create_conversation(id, "general", 1).unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let path = thread_path(&id);
        let mut seen = Vec::new();

        let opened = page.handle(
            &PageRequest {
                method: "GET".into(),
                path: path.clone(),
                body: Vec::new(),
            },
            |payload| answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen),
        );
        let body = String::from_utf8(opened.body).unwrap();
        assert_eq!(opened.status, 200);
        assert_eq!(messages(&body), 0);
        assert!(!body.contains("hello"));
        assert!(!body.contains("from-phone"));
        assert!(!body.contains("general"));
        assert!(body.contains("href=\"/\""));
        assert!(!body.contains("http://"));
        assert_eq!(seen, vec![HISTORY_REQ]);

        let sent = page.handle(
            &PageRequest {
                method: "POST".into(),
                path,
                body: b"text=hello".to_vec(),
            },
            |payload| answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen),
        );
        let body = String::from_utf8(sent.body).unwrap();
        assert_eq!(sent.status, 200);
        assert_eq!(messages(&body), 1);
        assert_eq!(body.matches("hello").count(), 1);
        assert!(!body.contains("from-phone"));
        assert!(!body.contains("http://"));
        assert_eq!(seen, vec![HISTORY_REQ, POST, HISTORY_REQ]);

        let mut link = FakeSocket::new();
        let mut server = ServerHello::new();
        attach(&mut link, &mut server, &dir, laptop_app, home_app);
        attach(&mut link, &mut server, &dir, phone_app, home_app);
        let post = encode_post(ident(0x62), id, "from-phone").unwrap();
        let accepted =
            accept_post(&dir, &mut store, server.attached(), phone_app, &post, 1_800).unwrap();
        assert_eq!(accepted.notices.len(), 1);
        assert_eq!(accepted.notices[0].0, laptop_app);

        let mut asked = false;
        assert!(page
            .on_notice(ident(0x99), &accepted.notices[0].1, |_| {
                asked = true;
                None
            })
            .is_none());
        assert!(!asked);

        let mut notice_after = None;
        let updated = page
            .on_notice(home_app, &accepted.notices[0].1, |payload| {
                if notice_after.is_none() {
                    notice_after = decode_history_req(payload).map(|(_, after)| after);
                }
                answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
            })
            .unwrap();
        let saved = store.messages_after(&id, None, 10).unwrap();
        assert_eq!(saved.messages.len(), 2);
        assert_eq!(saved.messages[0].text, "hello");
        assert_eq!(notice_after, Some(Some(saved.messages[0].id)));
        let body = String::from_utf8(updated.body).unwrap();
        assert_eq!(messages(&body), 2);
        assert_eq!(body.matches("hello").count(), 1);
        assert_eq!(body.matches("from-phone").count(), 1);
        assert_eq!(seen, vec![HISTORY_REQ, POST, HISTORY_REQ, HISTORY_REQ]);
    }

    #[test]
    fn page_draws_the_acked_text() {
        let (dir, _laptop_app, _home_app) = room();
        let id = ident(0x71);
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let response = page.handle(
            &PageRequest {
                method: "POST".into(),
                path: thread_path(&id),
                body: b"text=typed-line".to_vec(),
            },
            |payload| {
                seen.push(*payload.get(1).unwrap());
                if is_post(payload) {
                    let (message_id, conversation, _) = decode_post(payload).unwrap();
                    assert_eq!(conversation, id);
                    return Some(post_ack(message_id, conversation, "saved-line"));
                }
                if is_history_req(payload) {
                    return Some(empty_history(id));
                }
                None
            },
        );
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(messages(&body), 1);
        assert!(body.contains("saved-line"));
        assert!(!body.contains("typed-line"));
        assert_eq!(seen, vec![POST, HISTORY_REQ]);
    }

    #[test]
    fn page_does_not_show_a_message_the_server_did_not_save() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let missing = page.handle(
            &PageRequest {
                method: "GET".into(),
                path: "/t/nope".into(),
                body: Vec::new(),
            },
            |_| panic!("asked"),
        );
        assert_eq!(missing.status, 404);
        let response = page.handle(
            &PageRequest {
                method: "POST".into(),
                path: thread_path(&ident(0x71)),
                body: b"text=hidden".to_vec(),
            },
            |_| None,
        );
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(messages(&body), 0);
        assert!(!body.contains("hidden"));
        assert!(body.contains("The server did not answer."));
    }

    #[test]
    fn page_follows_every_history_response() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("history-pages");
        let mut store = Store::open(tmp.path()).unwrap();
        let id = ident(0x71);
        store.create_conversation(id, "general", 1).unwrap();
        for n in 0..3u8 {
            let prefix = format!("n{n}-");
            let text = format!("{prefix}{}", "m".repeat(400 - prefix.len()));
            store
                .append_message(id, ident(0x80 + n), ident(0x10), u64::from(n), &text)
                .unwrap();
        }
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let response = page.handle(
            &PageRequest {
                method: "GET".into(),
                path: thread_path(&id),
                body: Vec::new(),
            },
            |payload| answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen),
        );
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(messages(&body), 3);
        assert!(seen.iter().all(|kind| *kind == HISTORY_REQ));
        assert!(seen.len() > 1);
        for n in 0..3u8 {
            assert!(body.contains(&format!("n{n}-")));
        }
        assert!(!body.contains("n3-"));
    }

    #[test]
    fn page_escapes_the_message_from_history() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("message-escape");
        let mut store = Store::open(tmp.path()).unwrap();
        let id = ident(0x71);
        store.create_conversation(id, "general", 1).unwrap();
        store
            .append_message(id, ident(0x81), ident(0x10), 1, "a<b c")
            .unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let response = page.handle(
            &PageRequest {
                method: "GET".into(),
                path: thread_path(&id),
                body: Vec::new(),
            },
            |payload| answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen),
        );
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(messages(&body), 1);
        assert!(body.contains("a&lt;b c"));
        assert!(!body.contains("a<b"));
    }

    #[test]
    fn page_serves_a_thread_over_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let id = ident(0x71);
        let path = thread_path(&id);
        let server = thread::spawn(move || {
            let (dir, laptop_app, home_app) = room();
            let tmp = Temp::new("thread-http");
            let mut store = Store::open(tmp.path()).unwrap();
            store.create_conversation(id, "general", 1).unwrap();
            let mut page = ConversationList::new();
            page.note_directory(&dir);
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream).unwrap();
                let response = page.handle(&request, |payload| {
                    answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
                });
                write_response(&mut stream, &response).unwrap();
            }
            seen
        });

        let (status, body) = exchange(port, "GET", &path, "");
        assert_eq!(status, 200);
        assert_eq!(messages(&body), 0);
        assert!(!body.contains("hello"));
        assert!(!body.contains("http://"));
        assert!(body.contains("href=\"/\""));

        let (status, body) = exchange(port, "POST", &path, "text=hello");
        assert_eq!(status, 200);
        assert_eq!(messages(&body), 1);
        assert_eq!(body.matches("hello").count(), 1);
        assert!(!body.contains("http://"));

        let seen = server.join().unwrap();
        assert_eq!(seen, vec![HISTORY_REQ, POST, HISTORY_REQ]);
    }

    fn post_ack(id: [u8; 16], conversation: [u8; 16], text: &str) -> Vec<u8> {
        let mut out = vec![crate::hello::VERSION, POST_ACK];
        out.extend_from_slice(&id);
        out.extend_from_slice(&conversation);
        out.extend_from_slice(&ident(0x10));
        out.extend_from_slice(&1_700u64.to_be_bytes());
        out.extend_from_slice(&(text.len() as u16).to_be_bytes());
        out.extend_from_slice(text.as_bytes());
        out
    }

    fn empty_history(conversation: [u8; 16]) -> Vec<u8> {
        let mut out = vec![crate::hello::VERSION, HISTORY_RESP];
        out.extend_from_slice(&conversation);
        out.push(0);
        out.push(0);
        out
    }
}
