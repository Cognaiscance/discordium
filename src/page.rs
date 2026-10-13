//! Conversation list and one thread for the desktop window.
//!
//! The list shows only the conversations `LIST_RESP` returned. Opening one
//! waits for `CREATE_RESP`, then shows the row the following list returned.
//! The row is not taken from the typed title.
//!
//! A thread shows only the messages `HISTORY_RESP` returned. Sending waits
//! for `POST_ACK` and shows the saved text. The same create and the same post
//! are sent again when the server does not answer. The error stays when those
//! tries are done. A `NOTICE` for that conversation sends `HISTORY_REQ` and
//! adds the new messages. An `ADDED` sends `LIST_REQ` and the list shows those
//! saved rows. The open conversation stays open.

/// One try plus two repeats. The id stays the same, so the server keeps one row.
const OPEN_SEND_ATTEMPTS: usize = 3;

use std::collections::HashMap;

use crate::create_list::{
    decode_create_req, decode_create_resp, decode_list_resp, encode_create_req, encode_list_req,
    is_create_req, is_list_req,
};
use crate::directory::{record_discordium, Directory};
use crate::notice::{decode_added, decode_notice};
use crate::post_history::{
    decode_history_req, decode_history_resp, decode_post, decode_post_ack, encode_history_req,
    encode_post, is_history_req, is_post,
};
use crate::store::{Conversation, Message};

/// What the window shows after one ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub conversations: Vec<Conversation>,
    pub messages: Vec<Message>,
    pub note: Option<&'static str>,
    /// Set when this view is one conversation. None is the list.
    pub thread: Option<[u8; 16]>,
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

    pub fn show_list(&mut self, mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>) -> View {
        let (rows, note) = self.fetch(&mut ask);
        list_view(rows, note)
    }

    pub fn open(&mut self, title: &str, mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>) -> View {
        if title.is_empty() {
            let (rows, _) = self.fetch(&mut ask);
            return list_view(rows, Some("A title is required."));
        }
        if self.server.is_none() {
            return list_view(Vec::new(), Some("The server is not visible yet."));
        }
        let Some(payload) = encode_create_req(self.next_id(), title) else {
            let (rows, _) = self.fetch(&mut ask);
            return list_view(rows, Some("That title is too long."));
        };
        let id = decode_create_req(&payload).map(|(id, _)| id);
        for _ in 0..OPEN_SEND_ATTEMPTS {
            let created = ask(&payload).and_then(|resp| decode_create_resp(&resp));
            if created.is_some_and(|conversation| Some(conversation.id) == id) {
                let (rows, note) = self.fetch(&mut ask);
                return list_view(rows, note);
            }
        }
        let (rows, _) = self.fetch(&mut ask);
        list_view(rows, Some("The server did not answer."))
    }

    pub fn open_thread(
        &mut self,
        id: [u8; 16],
        mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> View {
        if self.server.is_none() {
            return self.thread_view(id, Some("The server is not visible yet."));
        }
        let (rows, note) = self.load_history(id, None, &mut ask);
        if note.is_none() || !rows.is_empty() {
            self.threads.insert(id, rows);
        }
        self.thread_view(id, note)
    }

    pub fn send(
        &mut self,
        id: [u8; 16],
        text: &str,
        mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> View {
        if text.is_empty() {
            return self.thread_with_note(id, &mut ask, "A message is required.");
        }
        if self.server.is_none() {
            return self.thread_view(id, Some("The server is not visible yet."));
        }
        let Some(payload) = encode_post(self.next_id(), id, text) else {
            return self.thread_with_note(id, &mut ask, "That message is too long.");
        };
        let sent = decode_post(&payload).map(|(message_id, _, _)| message_id);
        let mut saved = None;
        for _ in 0..OPEN_SEND_ATTEMPTS {
            saved = ask(&payload)
                .and_then(|resp| decode_post_ack(&resp))
                .filter(|message| Some(message.id) == sent && message.conversation == id);
            if saved.is_some() {
                break;
            }
        }
        let Some(saved) = saved else {
            return self.thread_with_note(id, &mut ask, "The server did not answer.");
        };
        let (mut rows, note) = self.load_history(id, None, &mut ask);
        if note.is_some() {
            rows = self.threads.get(&id).cloned().unwrap_or_default();
        }
        if let Some(slot) = rows.iter_mut().find(|message| message.id == saved.id) {
            *slot = saved;
        } else {
            rows.push(saved);
        }
        self.threads.insert(id, rows);
        self.thread_view(id, None)
    }

    /// History for this notice, then the thread with those messages added.
    ///
    /// None when `sender` is not the record server or the payload is not a notice.
    pub fn on_notice(
        &mut self,
        sender: [u8; 16],
        payload: &[u8],
        mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Option<View> {
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
        Some(self.thread_view(conversation, note))
    }

    /// The saved list after this added notice.
    ///
    /// None when `sender` is not the record server, the payload is not `ADDED`,
    /// or the list ask fails. The open thread is left as it was.
    pub fn on_added(
        &mut self,
        sender: [u8; 16],
        payload: &[u8],
        mut ask: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Option<Vec<Conversation>> {
        if self.server != Some(sender) {
            return None;
        }
        decode_added(payload)?;
        let (rows, note) = self.fetch(&mut ask);
        if note.is_some() {
            return None;
        }
        Some(rows)
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

    fn thread_with_note(
        &mut self,
        id: [u8; 16],
        ask: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
        note: &'static str,
    ) -> View {
        if self.server.is_some() {
            let (rows, err) = self.load_history(id, None, ask);
            if err.is_none() {
                self.threads.insert(id, rows);
            }
        }
        self.thread_view(id, Some(note))
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

    fn thread_view(&self, id: [u8; 16], note: Option<&'static str>) -> View {
        View {
            conversations: Vec::new(),
            messages: self.threads.get(&id).cloned().unwrap_or_default(),
            note,
            thread: Some(id),
        }
    }

    fn next_id(&mut self) -> [u8; 16] {
        self.seq = self.seq.wrapping_add(1);
        let mut id = [0u8; 16];
        id[8..].copy_from_slice(&self.seq.to_be_bytes());
        id
    }
}

fn list_view(conversations: Vec<Conversation>, note: Option<&'static str>) -> View {
    View {
        conversations,
        messages: Vec::new(),
        note,
        thread: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_list::{encode_create_req, on_create, on_list, CREATE_REQ, LIST_REQ};
    use crate::directory::{AppRecord, DeviceRecord, Directory, Grade, CLIENT_ALIAS, SERVER_ALIAS};
    use crate::hello::{ServerHello, HELLO_BYTES, VERSION};
    use crate::link::FakeSocket;
    use crate::notice::{accept_create, accept_post, encode_added, ADDED};
    use crate::post_history::{
        decode_history_req, decode_post, encode_post, on_history, on_post, HISTORY_REQ, POST,
        POST_ACK,
    };
    use crate::store::Store;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn ident(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn room() -> (Directory, [u8; 16], [u8; 16]) {
        let laptop_app = ident(0x11);
        let home_app = ident(0x21);
        let dir = Directory {
            local_app_id: ident(1),
            local_app_alias: CLIENT_ALIAS.into(),
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
                        alias: CLIENT_ALIAS.into(),
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
                        alias: SERVER_ALIAS.into(),
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

    #[test]
    fn list_shows_only_the_created_conversation() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("list");
        let mut store = Store::open(tmp.path()).unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let ask = |payload: &[u8], seen: &mut Vec<u8>, store: &mut Store| {
            answer(&dir, store, laptop_app, home_app, payload, seen)
        };

        let view = page.show_list(|payload| ask(payload, &mut seen, &mut store));
        assert!(view.thread.is_none());
        assert!(view.conversations.is_empty());
        assert!(view.note.is_none());

        let view = page.open("general", |payload| ask(payload, &mut seen, &mut store));
        assert!(view.thread.is_none());
        assert_eq!(view.conversations.len(), 1);
        assert_eq!(view.conversations[0].title, "general");
        assert!(view.note.is_none());
        assert!(view.conversations.iter().all(|row| row.title != "other"));
        assert_eq!(seen, vec![LIST_REQ, CREATE_REQ, LIST_REQ]);
    }

    #[test]
    fn open_does_not_show_a_title_the_server_did_not_save() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let view = page.open("hidden", |_| None);
        assert!(view.conversations.is_empty());
        assert!(view.messages.is_empty());
        assert_eq!(view.note, Some("The server did not answer."));
    }

    #[test]
    fn list_follows_every_list_response() {
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
        let view = page.show_list(|payload| {
            answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
        });
        assert_eq!(view.conversations.len(), 5);
        assert!(seen.iter().all(|kind| *kind == LIST_REQ));
        assert!(seen.len() > 1);
        for n in 0..5u8 {
            assert!(view
                .conversations
                .iter()
                .any(|row| row.title.starts_with(&format!("row{n}-"))));
        }
        assert!(view
            .conversations
            .iter()
            .all(|row| !row.title.starts_with("row5-")));
    }

    #[test]
    fn list_shows_the_title_the_server_saved() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("title");
        let mut store = Store::open(tmp.path()).unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let view = page.open("a<b c", |payload| {
            answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
        });
        assert_eq!(view.conversations.len(), 1);
        assert_eq!(view.conversations[0].title, "a<b c");
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
                alias: CLIENT_ALIAS.into(),
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
    fn thread_shows_an_empty_thread_then_the_saved_text_and_a_notice() {
        let (dir, laptop_app, phone_app, home_app) = house();
        let tmp = Temp::new("thread");
        let mut store = Store::open(tmp.path()).unwrap();
        let id = ident(0x71);
        store.create_conversation(id, "general", 1).unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();

        let opened = page.open_thread(id, |payload| {
            answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
        });
        assert_eq!(opened.thread, Some(id));
        assert!(opened.messages.is_empty());
        assert!(opened.note.is_none());
        assert_eq!(seen, vec![HISTORY_REQ]);

        let sent = page.send(id, "hello", |payload| {
            answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
        });
        assert_eq!(sent.messages.len(), 1);
        assert_eq!(sent.messages[0].text, "hello");
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
        assert_eq!(updated.messages.len(), 2);
        assert_eq!(updated.messages[0].text, "hello");
        assert_eq!(updated.messages[1].text, "from-phone");
        assert_eq!(seen, vec![HISTORY_REQ, POST, HISTORY_REQ, HISTORY_REQ]);
    }

    #[test]
    fn added_shows_the_list_the_server_saved() {
        let (dir, laptop_app, phone_app, home_app) = house();
        let tmp = Temp::new("added");
        let mut store = Store::open(tmp.path()).unwrap();
        let mut link = FakeSocket::new();
        let mut server = ServerHello::new();
        attach(&mut link, &mut server, &dir, laptop_app, home_app);
        attach(&mut link, &mut server, &dir, phone_app, home_app);

        let req = encode_create_req(ident(0x71), "a<b c").unwrap();
        let accepted =
            accept_create(&dir, &mut store, server.attached(), laptop_app, &req, 1_700).unwrap();
        assert_eq!(accepted.notices.len(), 1);
        assert_eq!(accepted.notices[0].0, phone_app);
        let again =
            accept_create(&dir, &mut store, server.attached(), laptop_app, &req, 1_800).unwrap();
        assert!(again.notices.is_empty());

        let id = ident(0x71);
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let opened = page.open_thread(id, |payload| {
            answer(&dir, &mut store, phone_app, home_app, payload, &mut seen)
        });
        assert_eq!(opened.thread, Some(id));
        seen.clear();

        let mut asked = false;
        assert!(page
            .on_added(ident(0x99), &accepted.notices[0].1, |_| {
                asked = true;
                None
            })
            .is_none());
        assert!(!asked);
        assert!(page
            .on_added(home_app, &[VERSION, ADDED], |_| {
                asked = true;
                None
            })
            .is_none());
        assert!(!asked);

        let rows = page
            .on_added(home_app, &encode_added(ident(0x99)), |payload| {
                answer(&dir, &mut store, phone_app, home_app, payload, &mut seen)
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].title, "a<b c");
        assert!(rows.iter().all(|row| row.id != ident(0x99)));
        assert_eq!(seen, vec![LIST_REQ]);

        assert!(page
            .on_added(home_app, &accepted.notices[0].1, |_| None)
            .is_none());
    }

    #[test]
    fn added_follows_every_list_response() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("added-pages");
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
        let rows = page
            .on_added(home_app, &encode_added(ident(0x34)), |payload| {
                answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
            })
            .unwrap();
        assert_eq!(rows.len(), 5);
        assert!(seen.iter().all(|kind| *kind == LIST_REQ));
        assert!(seen.len() > 1);
    }

    #[test]
    fn send_shows_the_acked_text() {
        let (dir, _laptop_app, _home_app) = room();
        let id = ident(0x71);
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let view = page.send(id, "typed-line", |payload| {
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
        });
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.messages[0].text, "saved-line");
        assert_eq!(seen, vec![POST, HISTORY_REQ]);
    }

    #[test]
    fn open_retries_the_same_create_then_shows_the_error() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut creates = Vec::new();
        let view = page.open("general", |payload| {
            if is_create_req(payload) {
                creates.push(payload.to_vec());
            }
            None
        });
        assert_eq!(view.note, Some("The server did not answer."));
        assert!(view.conversations.is_empty());
        assert_eq!(creates.len(), OPEN_SEND_ATTEMPTS);
        assert!(creates.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn open_keeps_the_create_that_answers_on_a_later_try() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("open-retry");
        let mut store = Store::open(tmp.path()).unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut creates = 0;
        let mut seen = Vec::new();
        let view = page.open("general", |payload| {
            if is_create_req(payload) {
                creates += 1;
                if creates < OPEN_SEND_ATTEMPTS {
                    return None;
                }
            }
            answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
        });
        assert_eq!(creates, OPEN_SEND_ATTEMPTS);
        assert!(view.note.is_none());
        assert_eq!(view.conversations.len(), 1);
        assert_eq!(view.conversations[0].title, "general");
        assert_eq!(store.list_conversations().len(), 1);
    }

    #[test]
    fn an_empty_title_is_an_error_without_a_create() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut creates = 0;
        let view = page.open("", |payload| {
            if is_create_req(payload) {
                creates += 1;
            }
            None
        });
        assert_eq!(view.note, Some("A title is required."));
        assert_eq!(creates, 0);
    }

    #[test]
    fn send_retries_the_same_post_then_shows_the_error() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut posts = Vec::new();
        let view = page.send(ident(0x71), "hello", |payload| {
            if is_post(payload) {
                posts.push(payload.to_vec());
            }
            None
        });
        assert_eq!(view.note, Some("The server did not answer."));
        assert!(view.messages.is_empty());
        assert_eq!(posts.len(), OPEN_SEND_ATTEMPTS);
        assert!(posts.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn send_keeps_the_post_that_answers_on_a_later_try() {
        let (dir, _laptop_app, _home_app) = room();
        let id = ident(0x71);
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut posts = Vec::new();
        let view = page.send(id, "typed-line", |payload| {
            if is_post(payload) {
                posts.push(payload.to_vec());
                if posts.len() < OPEN_SEND_ATTEMPTS {
                    return None;
                }
                let (message_id, conversation, _) = decode_post(payload).unwrap();
                return Some(post_ack(message_id, conversation, "saved-line"));
            }
            if is_history_req(payload) {
                return Some(empty_history(id));
            }
            None
        });
        assert_eq!(posts.len(), OPEN_SEND_ATTEMPTS);
        assert!(posts.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(view.note.is_none());
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.messages[0].text, "saved-line");
    }

    #[test]
    fn an_empty_message_is_an_error_without_a_post() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut posts = 0;
        let view = page.send(ident(0x71), "", |payload| {
            if is_post(payload) {
                posts += 1;
            }
            None
        });
        assert_eq!(view.note, Some("A message is required."));
        assert_eq!(posts, 0);
    }

    #[test]
    fn send_does_not_show_a_message_the_server_did_not_save() {
        let (dir, _laptop_app, _home_app) = room();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let view = page.send(ident(0x71), "hidden", |_| None);
        assert!(view.messages.is_empty());
        assert_eq!(view.note, Some("The server did not answer."));
    }

    #[test]
    fn thread_follows_every_history_response() {
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
        let view = page.open_thread(id, |payload| {
            answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
        });
        assert_eq!(view.messages.len(), 3);
        assert!(seen.iter().all(|kind| *kind == HISTORY_REQ));
        assert!(seen.len() > 1);
        for n in 0..3u8 {
            assert!(view
                .messages
                .iter()
                .any(|message| message.text.starts_with(&format!("n{n}-"))));
        }
        assert!(view
            .messages
            .iter()
            .all(|message| !message.text.starts_with("n3-")));
    }

    #[test]
    fn thread_shows_the_message_the_server_saved() {
        let (dir, laptop_app, home_app) = room();
        let tmp = Temp::new("message-text");
        let mut store = Store::open(tmp.path()).unwrap();
        let id = ident(0x71);
        store.create_conversation(id, "general", 1).unwrap();
        store
            .append_message(id, ident(0x81), ident(0x10), 1, "a<b c")
            .unwrap();
        let mut page = ConversationList::new();
        page.note_directory(&dir);
        let mut seen = Vec::new();
        let view = page.open_thread(id, |payload| {
            answer(&dir, &mut store, laptop_app, home_app, payload, &mut seen)
        });
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.messages[0].text, "a<b c");
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
        let mut out = vec![crate::hello::VERSION, crate::post_history::HISTORY_RESP];
        out.extend_from_slice(&conversation);
        out.push(0);
        out.push(0);
        out
    }
}
