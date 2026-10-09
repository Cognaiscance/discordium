//! Save one message, and read messages after a cursor.
//!
//! `POST` carries a device message id, a conversation id, and the text. The
//! same id in that conversation returns the copy already stored, including
//! when a retry changes the text. The device keeps sending that post until
//! `POST_ACK`. `HISTORY_REQ` returns the messages that follow a cursor, in
//! the order the server accepted them, and says when more remain.
//!
//! The sender is an approved `discordium-client` on one of this user's own devices.
//! The stored device id is that sender's device, not a claim in the payload.
//! Any other sender is ignored, and an app that is missing from the directory
//! is not remembered as a refusal.

use crate::directory::{
    approved_own_discordium, device_of_app, record_discordium, Directory, Peer,
};
use crate::hello::VERSION;
use crate::store::{AppendOutcome, Message, Store, StoreError};

pub const POST: u8 = 0x07;
pub const POST_ACK: u8 = 0x08;
pub const HISTORY_REQ: u8 = 0x09;
pub const HISTORY_RESP: u8 = 0x0A;

const DATAGRAM_MAX: usize = 4096;
const PAGE_BUDGET: usize = 1024;
const POST_FIXED: usize = 2 + 16 + 16 + 2;
const ACK_FIXED: usize = 2 + 16 + 16 + 16 + 8 + 2;
const HISTORY_HEADER: usize = 2 + 16 + 1 + 1;
const HISTORY_ENTRY: usize = 16 + 16 + 8 + 2;
/// A saved message has to fit in `POST_ACK` and alone in one history datagram.
const MAX_TEXT: usize = DATAGRAM_MAX - HISTORY_HEADER - HISTORY_ENTRY;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct HistoryPage {
    pub conversation: [u8; 16],
    pub messages: Vec<Message>,
    pub more: bool,
}

pub fn is_post(payload: &[u8]) -> bool {
    decode_post(payload).is_some()
}

pub fn is_history_req(payload: &[u8]) -> bool {
    decode_history_req(payload).is_some()
}

/// Save a message, or return the copy already saved for this device message id.
pub fn on_post(
    dir: &Directory,
    store: &mut Store,
    sender: [u8; 16],
    payload: &[u8],
    now_ms: u64,
) -> Option<Vec<u8>> {
    let (id, conversation, text) = decode_post(payload)?;
    if !approved_own_discordium(dir, &sender) {
        return None;
    }
    let device = device_of_app(dir, &sender)?;
    let outcome = match store.append_message(conversation, id, device, now_ms, &text) {
        Ok(outcome) => outcome,
        Err(StoreError::UnknownConversation) => return None,
        Err(err) => {
            eprintln!("discordium: post: {err}");
            return None;
        }
    };
    let message = match outcome {
        AppendOutcome::Saved(message) | AppendOutcome::Existing(message) => message,
    };
    encode_post_ack(&message)
}

/// Messages after `HISTORY_REQ`'s cursor. An unknown conversation or cursor
/// has no reply.
pub fn on_history(
    dir: &Directory,
    store: &Store,
    sender: [u8; 16],
    payload: &[u8],
) -> Option<Vec<u8>> {
    let (conversation, after) = decode_history_req(payload)?;
    if !approved_own_discordium(dir, &sender) {
        return None;
    }
    let page = match store.messages_after(&conversation, after, usize::MAX) {
        Ok(page) => page,
        Err(StoreError::UnknownConversation | StoreError::UnknownCursor) => return None,
        Err(err) => {
            eprintln!("discordium: history: {err}");
            return None;
        }
    };
    encode_history_page(conversation, &page.messages)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn encode_post(id: [u8; 16], conversation: [u8; 16], text: &str) -> Option<Vec<u8>> {
    if !text_fits(text) {
        return None;
    }
    let mut out = Vec::with_capacity(POST_FIXED + text.len());
    out.push(VERSION);
    out.push(POST);
    out.extend_from_slice(&id);
    out.extend_from_slice(&conversation);
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    Some(out)
}

pub fn decode_post(payload: &[u8]) -> Option<([u8; 16], [u8; 16], String)> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(POST) {
        return None;
    }
    if payload.len() < POST_FIXED || payload.len() > DATAGRAM_MAX {
        return None;
    }
    let id: [u8; 16] = payload.get(2..18)?.try_into().ok()?;
    let conversation: [u8; 16] = payload.get(18..34)?.try_into().ok()?;
    let text_len = usize::from(u16::from_be_bytes(payload.get(34..36)?.try_into().ok()?));
    if !fits_text_len(text_len) || payload.len() != POST_FIXED + text_len {
        return None;
    }
    let text = std::str::from_utf8(payload.get(POST_FIXED..)?)
        .ok()?
        .to_string();
    Some((id, conversation, text))
}

fn encode_post_ack(message: &Message) -> Option<Vec<u8>> {
    if !text_fits(&message.text) {
        return None;
    }
    let mut out = Vec::with_capacity(ACK_FIXED + message.text.len());
    out.push(VERSION);
    out.push(POST_ACK);
    out.extend_from_slice(&message.id);
    out.extend_from_slice(&message.conversation);
    out.extend_from_slice(&message.device);
    out.extend_from_slice(&message.sent_ms.to_be_bytes());
    out.extend_from_slice(&(message.text.len() as u16).to_be_bytes());
    out.extend_from_slice(message.text.as_bytes());
    Some(out)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn decode_post_ack(payload: &[u8]) -> Option<Message> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(POST_ACK) {
        return None;
    }
    if payload.len() < ACK_FIXED || payload.len() > DATAGRAM_MAX {
        return None;
    }
    let id: [u8; 16] = payload.get(2..18)?.try_into().ok()?;
    let conversation: [u8; 16] = payload.get(18..34)?.try_into().ok()?;
    let device: [u8; 16] = payload.get(34..50)?.try_into().ok()?;
    let sent_ms = u64::from_be_bytes(payload.get(50..58)?.try_into().ok()?);
    let text_len = usize::from(u16::from_be_bytes(payload.get(58..60)?.try_into().ok()?));
    if !fits_text_len(text_len) || payload.len() != ACK_FIXED + text_len {
        return None;
    }
    let text = std::str::from_utf8(payload.get(ACK_FIXED..)?)
        .ok()?
        .to_string();
    Some(Message {
        id,
        conversation,
        device,
        sent_ms,
        text,
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn encode_history_req(conversation: [u8; 16], after: Option<[u8; 16]>) -> Vec<u8> {
    let mut out = vec![VERSION, HISTORY_REQ];
    out.extend_from_slice(&conversation);
    match after {
        None => out.push(0),
        Some(id) => {
            out.push(1);
            out.extend_from_slice(&id);
        }
    }
    out
}

pub fn decode_history_req(payload: &[u8]) -> Option<([u8; 16], Option<[u8; 16]>)> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(HISTORY_REQ) {
        return None;
    }
    let conversation: [u8; 16] = payload.get(2..18)?.try_into().ok()?;
    match payload.get(18).copied() {
        Some(0) if payload.len() == 19 => Some((conversation, None)),
        Some(1) if payload.len() == 35 => {
            let id: [u8; 16] = payload.get(19..35)?.try_into().ok()?;
            Some((conversation, Some(id)))
        }
        _ => None,
    }
}

fn encode_history_page(conversation: [u8; 16], messages: &[Message]) -> Option<Vec<u8>> {
    let mut taken = 0usize;
    let mut size = HISTORY_HEADER;
    while taken < messages.len() && taken < 255 {
        let Some(entry) = encoded_entry_len(&messages[taken]) else {
            if taken == 0 {
                return None;
            }
            break;
        };
        let budget = if taken == 0 && HISTORY_HEADER + entry > PAGE_BUDGET {
            DATAGRAM_MAX
        } else {
            PAGE_BUDGET
        };
        if size + entry > budget {
            break;
        }
        size += entry;
        taken += 1;
        if budget == DATAGRAM_MAX {
            break;
        }
    }
    if taken == 0 && !messages.is_empty() {
        return None;
    }
    let more = taken < messages.len();
    Some(write_history_resp(conversation, &messages[..taken], more))
}

fn encoded_entry_len(message: &Message) -> Option<usize> {
    if text_fits(&message.text) {
        Some(HISTORY_ENTRY + message.text.len())
    } else {
        None
    }
}

fn write_history_resp(conversation: [u8; 16], messages: &[Message], more: bool) -> Vec<u8> {
    let mut out = vec![VERSION, HISTORY_RESP];
    out.extend_from_slice(&conversation);
    out.push(u8::from(more));
    out.push(messages.len() as u8);
    for message in messages {
        out.extend_from_slice(&message.id);
        out.extend_from_slice(&message.device);
        out.extend_from_slice(&message.sent_ms.to_be_bytes());
        out.extend_from_slice(&(message.text.len() as u16).to_be_bytes());
        out.extend_from_slice(message.text.as_bytes());
    }
    out
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn decode_history_resp(payload: &[u8]) -> Option<HistoryPage> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(HISTORY_RESP) {
        return None;
    }
    if payload.len() < HISTORY_HEADER {
        return None;
    }
    let conversation: [u8; 16] = payload.get(2..18)?.try_into().ok()?;
    let more = match payload.get(18).copied() {
        Some(0) => false,
        Some(1) => true,
        _ => return None,
    };
    let count = usize::from(*payload.get(19)?);
    let mut rest = payload.get(HISTORY_HEADER..)?;
    let mut messages = Vec::with_capacity(count);
    for _ in 0..count {
        if rest.len() < HISTORY_ENTRY {
            return None;
        }
        let id: [u8; 16] = rest[..16].try_into().ok()?;
        let device: [u8; 16] = rest[16..32].try_into().ok()?;
        let sent_ms = u64::from_be_bytes(rest[32..40].try_into().ok()?);
        let text_len = usize::from(u16::from_be_bytes(rest[40..42].try_into().ok()?));
        if rest.len() < HISTORY_ENTRY + text_len {
            return None;
        }
        let text = std::str::from_utf8(&rest[HISTORY_ENTRY..HISTORY_ENTRY + text_len])
            .ok()?
            .to_string();
        rest = &rest[HISTORY_ENTRY + text_len..];
        messages.push(Message {
            id,
            conversation,
            device,
            sent_ms,
            text,
        });
    }
    if !rest.is_empty() {
        return None;
    }
    Some(HistoryPage {
        conversation,
        messages,
        more,
    })
}

fn text_fits(text: &str) -> bool {
    fits_text_len(text.len())
}

fn fits_text_len(len: usize) -> bool {
    (1..=MAX_TEXT).contains(&len)
}

/// One post the device keeps sending until the record server acks it.
#[cfg_attr(not(test), allow(dead_code))]
pub struct DevicePost {
    server: Option<Peer>,
    pending: Option<PendingPost>,
}

struct PendingPost {
    conversation: [u8; 16],
    id: [u8; 16],
    text: String,
}

#[cfg_attr(not(test), allow(dead_code))]
impl DevicePost {
    pub fn new() -> Self {
        Self {
            server: None,
            pending: None,
        }
    }

    pub fn note_directory(&mut self, dir: &Directory) {
        self.server = record_discordium(dir);
    }

    /// Remember one post. False when a post is already waiting or the text
    /// cannot be sent in one datagram.
    pub fn start(&mut self, conversation: [u8; 16], id: [u8; 16], text: &str) -> bool {
        if self.pending.is_some() || encode_post(id, conversation, text).is_none() {
            return false;
        }
        self.pending = Some(PendingPost {
            conversation,
            id,
            text: text.to_string(),
        });
        true
    }

    /// Where the next copy of the pending post goes. None once acked, or
    /// until the record server is visible.
    pub fn post_to_send(&self) -> Option<(Peer, Vec<u8>)> {
        let server = self.server?;
        let pending = self.pending.as_ref()?;
        let bytes = encode_post(pending.id, pending.conversation, &pending.text)?;
        Some((server, bytes))
    }

    /// The saved message when this push is its `POST_ACK`.
    pub fn on_push(&mut self, sender: [u8; 16], payload: &[u8]) -> Option<Message> {
        let server = self.server?;
        let pending = self.pending.as_ref()?;
        if sender != server.app {
            return None;
        }
        let saved = decode_post_ack(payload)?;
        if saved.id != pending.id || saved.conversation != pending.conversation {
            return None;
        }
        self.pending = None;
        Some(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{
        AppRecord, ContactRecord, DeviceRecord, Directory, Grade, CLIENT_ALIAS, SERVER_ALIAS,
    };
    use crate::link::FakeSocket;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn ident(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn app(id: [u8; 16], alias: &str, approved: bool) -> AppRecord {
        AppRecord {
            id,
            alias: alias.to_string(),
            approved,
        }
    }

    fn dev(
        id: [u8; 16],
        alias: &str,
        grade: Grade,
        rank: u8,
        apps: Vec<AppRecord>,
    ) -> DeviceRecord {
        DeviceRecord {
            id,
            alias: alias.to_string(),
            grade,
            sg_rank: rank,
            apps,
        }
    }

    fn directory(
        local: [u8; 16],
        own: Vec<DeviceRecord>,
        contacts: Vec<ContactRecord>,
    ) -> Directory {
        Directory {
            local_app_id: ident(1),
            local_app_alias: CLIENT_ALIAS.into(),
            local_app_approved: true,
            token: ident(2),
            local_device: local,
            owner_alias: "owner".into(),
            owner_id: ident(3),
            own_devices: own,
            contacts,
        }
    }

    fn two_nodes() -> (Directory, [u8; 16], [u8; 16], [u8; 16], [u8; 16]) {
        let laptop = ident(0x10);
        let laptop_app = ident(0x11);
        let home = ident(0x20);
        let home_app = ident(0x21);
        let dir = directory(
            laptop,
            vec![
                dev(
                    laptop,
                    "laptop",
                    Grade::Device,
                    0,
                    vec![app(laptop_app, CLIENT_ALIAS, true)],
                ),
                dev(
                    home,
                    "home",
                    Grade::Server,
                    1,
                    vec![app(home_app, SERVER_ALIAS, true)],
                ),
            ],
            vec![ContactRecord {
                alias: "other".into(),
                id: ident(0x30),
                devices: vec![dev(
                    ident(0x01),
                    "other-sg",
                    Grade::Server,
                    1,
                    vec![app(ident(0x31), SERVER_ALIAS, true)],
                )],
            }],
        );
        (dir, laptop, laptop_app, home, home_app)
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

    fn post_bytes(id: [u8; 16], conversation: [u8; 16], text: &[u8]) -> Vec<u8> {
        let mut out = vec![VERSION, POST];
        out.extend_from_slice(&id);
        out.extend_from_slice(&conversation);
        let len = u16::try_from(text.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(text);
        out
    }

    #[test]
    fn saved_message_is_read_back_after_a_cursor() {
        let (dir, laptop, laptop_app, _home, home_app) = two_nodes();
        let tmp = Temp::new("cursor");
        let mut store = Store::open(tmp.path()).unwrap();
        let conversation = ident(0x41);
        store
            .create_conversation(conversation, "general", 1_700)
            .unwrap();
        let mut link = FakeSocket::new();
        let mut device = DevicePost::new();
        device.note_directory(&dir);

        let empty = encode_history_req(conversation, None);
        link.deliver(laptop_app, home_app, &empty);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = on_history(&dir, &store, sender, &payload).unwrap();
        link.deliver(home_app, laptop_app, &resp);
        let (_sender, payload) = link.take(laptop_app).unwrap();
        let page = decode_history_resp(&payload).unwrap();
        assert_eq!(page.conversation, conversation);
        assert!(page.messages.is_empty());
        assert!(!page.more);

        assert!(device.start(conversation, ident(0x61), "first"));
        let (peer, bytes) = device.post_to_send().unwrap();
        assert_eq!(peer.app, home_app);
        link.deliver(laptop_app, peer.app, &bytes);
        let (sender, payload) = link.take(home_app).unwrap();
        let ack = on_post(&dir, &mut store, sender, &payload, 300).unwrap();
        assert_eq!(&ack[50..58], &300u64.to_be_bytes());
        link.deliver(home_app, laptop_app, &ack);
        let (sender, payload) = link.take(laptop_app).unwrap();
        let first = device.on_push(sender, &payload).unwrap();
        assert!(device.post_to_send().is_none());
        assert_eq!(first.device, laptop);
        assert_eq!(first.text, "first");
        assert_eq!(first.sent_ms, 300);

        assert!(device.start(conversation, ident(0x62), "second"));
        let (_peer, bytes) = device.post_to_send().unwrap();
        link.deliver(laptop_app, home_app, &bytes);
        let (sender, payload) = link.take(home_app).unwrap();
        let ack = on_post(&dir, &mut store, sender, &payload, 100).unwrap();
        link.deliver(home_app, laptop_app, &ack);
        let (sender, payload) = link.take(laptop_app).unwrap();
        let second = device.on_push(sender, &payload).unwrap();
        assert_eq!(second.sent_ms, 100);

        let from_start = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, None),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(from_start.messages, vec![first.clone(), second.clone()]);
        assert!(!from_start.more);

        assert!(on_history(
            &dir,
            &store,
            laptop_app,
            &encode_history_req(conversation, Some(ident(0x99))),
        )
        .is_none());
        let req = encode_history_req(conversation, Some(first.id));
        link.deliver(laptop_app, home_app, &req);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = on_history(&dir, &store, sender, &payload).unwrap();
        link.deliver(home_app, laptop_app, &resp);
        let (_sender, payload) = link.take(laptop_app).unwrap();
        let page = decode_history_resp(&payload).unwrap();
        assert_eq!(page.messages, vec![second]);
        assert!(!page.more);

        let tail = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, Some(ident(0x62))),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(tail.messages.is_empty());
        assert!(!tail.more);
    }

    #[test]
    fn repeated_post_is_one_row() {
        let (dir, laptop, laptop_app, _home, home_app) = two_nodes();
        let tmp = Temp::new("retry");
        let mut store = Store::open(tmp.path()).unwrap();
        let conversation = ident(0x41);
        store
            .create_conversation(conversation, "general", 1)
            .unwrap();
        let mut link = FakeSocket::new();
        let mut device = DevicePost::new();
        device.note_directory(&dir);
        let message_id = ident(0x61);
        assert!(device.start(conversation, message_id, "hello"));

        let (_peer, post) = device.post_to_send().unwrap();
        link.deliver(laptop_app, home_app, &post);
        let (sender, payload) = link.take(home_app).unwrap();
        let lost = on_post(&dir, &mut store, sender, &payload, 50).unwrap();
        assert!(link.take(laptop_app).is_none());
        assert!(device.post_to_send().is_some());
        assert!(device.on_push(ident(0x31), &lost).is_none());
        assert!(device.on_push(home_app, &[VERSION, POST_ACK]).is_none());
        assert!(device.post_to_send().is_some());

        link.deliver(laptop_app, home_app, &post);
        let (sender, payload) = link.take(home_app).unwrap();
        let ack = on_post(&dir, &mut store, sender, &payload, 90).unwrap();
        let saved = decode_post_ack(&ack).unwrap();
        assert_eq!(saved.text, "hello");
        assert_eq!(saved.sent_ms, 50);
        assert_eq!(saved.device, laptop);

        let other = encode_post(message_id, conversation, "hello again").unwrap();
        let again =
            decode_post_ack(&on_post(&dir, &mut store, laptop_app, &other, 99).unwrap()).unwrap();
        assert_eq!(again, saved);

        link.deliver(home_app, laptop_app, &ack);
        let (sender, payload) = link.take(laptop_app).unwrap();
        assert_eq!(device.on_push(sender, &payload).unwrap(), saved);
        assert!(device.post_to_send().is_none());

        let page = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, None),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page.messages, vec![saved.clone()]);
        assert!(!page.more);
        let after = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, Some(saved.id)),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(after.messages.is_empty());
        assert!(!after.more);
    }

    #[test]
    fn other_senders_do_not_save_a_message() {
        let (mut dir, _laptop, laptop_app, _home, home_app) = two_nodes();
        dir.own_devices[0]
            .apps
            .push(app(ident(0x12), CLIENT_ALIAS, false));
        dir.own_devices[0]
            .apps
            .push(app(ident(0x13), "notes", true));
        let tmp = Temp::new("others");
        let mut store = Store::open(tmp.path()).unwrap();
        let conversation = ident(0x41);
        store
            .create_conversation(conversation, "general", 1)
            .unwrap();
        let mut link = FakeSocket::new();
        let req = encode_post(ident(0x61), conversation, "nope").unwrap();

        for sender in [ident(0x31), ident(0x12), ident(0x13), ident(0x99)] {
            link.deliver(sender, home_app, &req);
            let (from, payload) = link.take(home_app).unwrap();
            assert!(on_post(&dir, &mut store, from, &payload, 1).is_none());
            link.deliver(sender, home_app, &encode_history_req(conversation, None));
            let (from, payload) = link.take(home_app).unwrap();
            assert!(on_history(&dir, &store, from, &payload).is_none());
            assert!(link.take(sender).is_none());
        }

        let mut trailing = encode_post(ident(1), conversation, "x").unwrap();
        trailing.push(0);
        let mut history = encode_history_req(conversation, None);
        history.push(0);
        for bad in [
            vec![2, POST],
            vec![VERSION, POST],
            trailing,
            post_bytes(ident(1), conversation, b""),
            post_bytes(ident(1), conversation, &[0xff]),
            post_bytes(ident(1), conversation, &vec![b'x'; MAX_TEXT + 1]),
            vec![VERSION, HISTORY_REQ],
            history,
            {
                let mut short = encode_history_req(conversation, None);
                short[18] = 2;
                short
            },
            encode_history_req(conversation, Some(ident(1)))[..20].to_vec(),
        ] {
            assert!(on_post(&dir, &mut store, laptop_app, &bad, 1).is_none());
            assert!(on_history(&dir, &store, laptop_app, &bad).is_none());
        }
        assert!(encode_post(ident(1), conversation, "").is_none());
        assert!(on_post(
            &dir,
            &mut store,
            laptop_app,
            &encode_post(ident(1), ident(0x42), "missing").unwrap(),
            1,
        )
        .is_none());
        let page = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, None),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(page.messages.is_empty());

        dir.own_devices.push(dev(
            ident(0x40),
            "phone",
            Grade::Device,
            0,
            vec![app(ident(0x99), CLIENT_ALIAS, true)],
        ));
        link.deliver(ident(0x99), home_app, &req);
        let (from, payload) = link.take(home_app).unwrap();
        let saved =
            decode_post_ack(&on_post(&dir, &mut store, from, &payload, 5).unwrap()).unwrap();
        assert_eq!(saved.text, "nope");
        assert_eq!(saved.device, ident(0x40));
        assert_eq!(saved.sent_ms, 5);

        link.deliver(
            ident(0x31),
            home_app,
            &encode_history_req(conversation, None),
        );
        let (from, payload) = link.take(home_app).unwrap();
        assert!(on_history(&dir, &store, from, &payload).is_none());
        let page = decode_history_resp(
            &on_history(
                &dir,
                &store,
                ident(0x99),
                &encode_history_req(conversation, None),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page.messages, vec![saved]);
    }

    #[test]
    fn a_long_history_says_more_remain() {
        let (dir, _laptop, laptop_app, _home, _home_app) = two_nodes();
        let tmp = Temp::new("more");
        let mut store = Store::open(tmp.path()).unwrap();
        let conversation = ident(0x41);
        store
            .create_conversation(conversation, "general", 1)
            .unwrap();
        // A 400-byte message encodes to 442 bytes. Two entries plus the
        // 20-byte header fit in 1024; the third does not.
        let text = "t".repeat(400);
        let mut ids = Vec::new();
        for n in 0..3u8 {
            let id = ident(0x70 + n);
            ids.push(id);
            let req = encode_post(id, conversation, &text).unwrap();
            let saved = decode_post_ack(
                &on_post(&dir, &mut store, laptop_app, &req, 1_000 - u64::from(n)).unwrap(),
            )
            .unwrap();
            assert_eq!(saved.sent_ms, 1_000 - u64::from(n));
        }

        let first = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, None),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(first.messages.len(), 2);
        assert!(first.more);
        assert_eq!(
            first
                .messages
                .iter()
                .map(|message| message.sent_ms)
                .collect::<Vec<_>>(),
            vec![1_000, 999]
        );
        let again = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, None),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(again, first);

        let cursor = first.messages.last().unwrap().id;
        let second = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, Some(cursor)),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(!second.more);
        assert_eq!(second.messages.len(), 1);
        assert_eq!(second.messages[0].id, ids[2]);
        let seen: Vec<_> = first
            .messages
            .iter()
            .chain(second.messages.iter())
            .map(|message| message.id)
            .collect();
        assert_eq!(seen, ids);

        let long_id = ident(0x80);
        let long = "u".repeat(2000);
        on_post(
            &dir,
            &mut store,
            laptop_app,
            &encode_post(long_id, conversation, &long).unwrap(),
            2,
        )
        .unwrap();
        on_post(
            &dir,
            &mut store,
            laptop_app,
            &encode_post(ident(0x81), conversation, "end").unwrap(),
            3,
        )
        .unwrap();
        let alone = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, Some(ids[2])),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(alone.messages.len(), 1);
        assert_eq!(alone.messages[0].id, long_id);
        assert_eq!(alone.messages[0].text.len(), 2000);
        assert!(alone.more);
        let tail = decode_history_resp(
            &on_history(
                &dir,
                &store,
                laptop_app,
                &encode_history_req(conversation, Some(long_id)),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(tail.messages.len(), 1);
        assert_eq!(tail.messages[0].text, "end");
        assert!(!tail.more);
    }
}
