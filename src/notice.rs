//! Tell other attached devices that a conversation has a new message.
//!
//! A new `POST` is announced with `NOTICE`. The device that posted does not
//! get one, and a repeated post does not send another. Each other attached
//! device sends `HISTORY_REQ` and reads the new message. An app that is not
//! attached is not told.

use std::collections::{HashMap, VecDeque};

use crate::directory::{approved_own_discordium, record_discordium, Directory, Peer};
use crate::hello::VERSION;
use crate::post_history::{decode_history_resp, decode_post, encode_history_req, on_post};
use crate::store::{Message, Store};

pub const NOTICE: u8 = 0x0B;

pub struct AcceptedPost {
    pub ack: Vec<u8>,
    pub notices: Vec<([u8; 16], [u8; 18])>,
}

/// Save a post, and name each other attached device that should hear about it.
///
/// The ack is the copy already saved when the device message id is a retry.
/// A retry has an empty notice list.
pub fn accept_post(
    dir: &Directory,
    store: &mut Store,
    attached: &[[u8; 16]],
    sender: [u8; 16],
    payload: &[u8],
    now_ms: u64,
) -> Option<AcceptedPost> {
    let (_id, conversation, _text) = decode_post(payload)?;
    let before = message_count(store, conversation);
    let ack = on_post(dir, store, sender, payload, now_ms)?;
    let notices = if message_count(store, conversation) > before {
        notice_list(dir, attached, sender, conversation)
    } else {
        Vec::new()
    };
    Some(AcceptedPost { ack, notices })
}

pub fn encode_notice(conversation: [u8; 16]) -> [u8; 18] {
    let mut out = [0u8; 18];
    out[0] = VERSION;
    out[1] = NOTICE;
    out[2..].copy_from_slice(&conversation);
    out
}

pub fn decode_notice(payload: &[u8]) -> Option<[u8; 16]> {
    if payload.len() != 18
        || payload.first().copied() != Some(VERSION)
        || payload.get(1).copied() != Some(NOTICE)
    {
        return None;
    }
    payload.get(2..18)?.try_into().ok()
}

fn notice_list(
    dir: &Directory,
    attached: &[[u8; 16]],
    sender: [u8; 16],
    conversation: [u8; 16],
) -> Vec<([u8; 16], [u8; 18])> {
    let notice = encode_notice(conversation);
    attached
        .iter()
        .copied()
        .filter(|app| *app != sender && approved_own_discordium(dir, app))
        .map(|app| (app, notice))
        .collect()
}

fn message_count(store: &Store, conversation: [u8; 16]) -> usize {
    store
        .messages_after(&conversation, None, usize::MAX)
        .map(|page| page.messages.len())
        .unwrap_or(0)
}

#[derive(Clone, Copy)]
struct Pending {
    conversation: [u8; 16],
    after: Option<[u8; 16]>,
}

/// The device side of a notice: ask for history, and keep asking while more remain.
pub struct DeviceHistory {
    server: Option<Peer>,
    cursors: HashMap<[u8; 16], [u8; 16]>,
    pending: Option<Pending>,
    queue: VecDeque<[u8; 16]>,
    inbox: Vec<Message>,
}

impl DeviceHistory {
    pub fn new() -> Self {
        Self {
            server: None,
            cursors: HashMap::new(),
            pending: None,
            queue: VecDeque::new(),
            inbox: Vec::new(),
        }
    }

    pub fn note_directory(&mut self, dir: &Directory) {
        self.server = record_discordium(dir);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn messages(&self) -> &[Message] {
        &self.inbox
    }

    /// A history request for this notice. None unless the sender is the record server.
    pub fn on_notice(&mut self, sender: [u8; 16], payload: &[u8]) -> Option<(Peer, Vec<u8>)> {
        let server = self.server?;
        if sender != server.app {
            return None;
        }
        let conversation = decode_notice(payload)?;
        if let Some(pending) = self.pending {
            if pending.conversation == conversation {
                return Some((
                    server,
                    encode_history_req(pending.conversation, pending.after),
                ));
            }
        }
        if self.pending.is_some() {
            if !self.queue.contains(&conversation) {
                self.queue.push_back(conversation);
            }
            return None;
        }
        self.start(conversation)
    }

    /// Store a history page from the record server. When more remain, the next request.
    pub fn on_history(&mut self, sender: [u8; 16], payload: &[u8]) -> Option<(Peer, Vec<u8>)> {
        let server = self.server?;
        if sender != server.app {
            return None;
        }
        let pending = self.pending?;
        let page = decode_history_resp(payload)?;
        if page.conversation != pending.conversation {
            return None;
        }
        let last = page.messages.last().map(|message| message.id);
        for message in page.messages {
            let seen = self
                .inbox
                .iter()
                .any(|have| have.conversation == message.conversation && have.id == message.id);
            if !seen {
                self.inbox.push(message);
            }
        }
        if let Some(last) = last {
            self.cursors.insert(page.conversation, last);
        }
        if page.more {
            if let Some(last) = last {
                self.pending = Some(Pending {
                    conversation: page.conversation,
                    after: Some(last),
                });
                return Some((server, encode_history_req(page.conversation, Some(last))));
            }
        }
        self.pending = None;
        self.start_queued()
    }

    fn start(&mut self, conversation: [u8; 16]) -> Option<(Peer, Vec<u8>)> {
        let server = self.server?;
        let after = self.cursors.get(&conversation).copied();
        self.pending = Some(Pending {
            conversation,
            after,
        });
        Some((server, encode_history_req(conversation, after)))
    }

    fn start_queued(&mut self) -> Option<(Peer, Vec<u8>)> {
        let conversation = self.queue.pop_front()?;
        if let Some(request) = self.start(conversation) {
            return Some(request);
        }
        self.queue.push_front(conversation);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{
        AppRecord, ContactRecord, DeviceRecord, Directory, Grade, CLIENT_ALIAS, SERVER_ALIAS,
    };
    use crate::hello::{ServerHello, HELLO_ACK_BYTES, HELLO_BYTES};
    use crate::link::FakeSocket;
    use crate::post_history::{
        decode_history_req, decode_history_resp, decode_post_ack, encode_post, on_history,
    };
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

    fn directory(own: Vec<DeviceRecord>) -> Directory {
        Directory {
            local_app_id: ident(1),
            local_app_alias: SERVER_ALIAS.into(),
            local_app_approved: true,
            token: ident(2),
            local_device: ident(0x20),
            owner_alias: "owner".into(),
            owner_id: ident(3),
            own_devices: own,
            contacts: vec![ContactRecord {
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
        }
    }

    /// Laptop A, phone B, and a tablet that never says hello. Home holds the record.
    fn room() -> (Directory, [u8; 16], [u8; 16], [u8; 16]) {
        let laptop_app = ident(0x11);
        let phone_app = ident(0x42);
        let home_app = ident(0x21);
        let dir = directory(vec![
            dev(
                ident(0x10),
                "laptop",
                Grade::Device,
                0,
                vec![app(laptop_app, CLIENT_ALIAS, true)],
            ),
            dev(
                ident(0x40),
                "phone",
                Grade::Device,
                0,
                vec![app(phone_app, CLIENT_ALIAS, true)],
            ),
            dev(
                ident(0x50),
                "tablet",
                Grade::Device,
                0,
                vec![app(ident(0x52), CLIENT_ALIAS, true)],
            ),
            dev(
                ident(0x20),
                "home",
                Grade::Server,
                1,
                vec![app(home_app, SERVER_ALIAS, true)],
            ),
        ]);
        (dir, laptop_app, phone_app, home_app)
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
        let (sender, payload) = link.take(app).unwrap();
        assert_eq!(sender, home_app);
        assert_eq!(payload, HELLO_ACK_BYTES);
    }

    fn deliver_notice(
        link: &mut FakeSocket,
        phone: &mut DeviceHistory,
        home_app: [u8; 16],
        phone_app: [u8; 16],
        notice: &[u8],
    ) -> Vec<u8> {
        link.deliver(home_app, phone_app, notice);
        let (sender, payload) = link.take(phone_app).unwrap();
        let (peer, request) = phone.on_notice(sender, &payload).unwrap();
        assert_eq!(peer.app, home_app);
        request
    }

    #[test]
    fn device_b_asks_and_receives_the_new_message() {
        let (dir, laptop_app, phone_app, home_app) = room();
        let tmp = Temp::new("notice");
        let mut store = Store::open(tmp.path()).unwrap();
        let conversation = ident(0x71);
        store
            .create_conversation(conversation, "general", 1)
            .unwrap();
        let mut link = FakeSocket::new();
        let mut server = ServerHello::new();
        attach(&mut link, &mut server, &dir, laptop_app, home_app);
        attach(&mut link, &mut server, &dir, phone_app, home_app);
        link.deliver(ident(0x31), home_app, &HELLO_BYTES);
        let (sender, payload) = link.take(home_app).unwrap();
        assert!(server.on_hello(&dir, sender, &payload).is_none());
        assert_eq!(server.attached(), &[laptop_app, phone_app]);

        let mut phone = DeviceHistory::new();
        phone.note_directory(&dir);
        let first_id = ident(0x61);
        let post = encode_post(first_id, conversation, "one").unwrap();
        link.deliver(laptop_app, home_app, &post);
        let (sender, payload) = link.take(home_app).unwrap();
        let accepted =
            accept_post(&dir, &mut store, server.attached(), sender, &payload, 50).unwrap();
        let saved = decode_post_ack(&accepted.ack).unwrap();
        assert_eq!(saved.text, "one");
        assert_eq!(
            accepted
                .notices
                .iter()
                .map(|(app, _)| *app)
                .collect::<Vec<_>>(),
            vec![phone_app]
        );

        let request = deliver_notice(
            &mut link,
            &mut phone,
            home_app,
            phone_app,
            &accepted.notices[0].1,
        );
        let (_conversation, after) = decode_history_req(&request).unwrap();
        assert_eq!(after, None);
        link.deliver(phone_app, home_app, &request);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = on_history(&dir, &store, sender, &payload).unwrap();
        link.deliver(home_app, phone_app, &resp);
        let (sender, payload) = link.take(phone_app).unwrap();
        assert!(phone.on_history(sender, &payload).is_none());
        assert_eq!(phone.messages().len(), 1);
        assert_eq!(&phone.messages()[0], &saved);

        let retry = encode_post(first_id, conversation, "changed").unwrap();
        let again =
            accept_post(&dir, &mut store, server.attached(), laptop_app, &retry, 90).unwrap();
        assert_eq!(decode_post_ack(&again.ack).unwrap(), saved);
        assert!(again.notices.is_empty());
        assert!(phone
            .on_notice(ident(0x31), &encode_notice(conversation))
            .is_none());
        assert!(phone.on_notice(home_app, &[VERSION, NOTICE]).is_none());
        assert!(phone
            .on_notice(home_app, &{
                let mut bad = encode_notice(conversation).to_vec();
                bad.push(0);
                bad
            })
            .is_none());

        let mut attached = server.attached().to_vec();
        attached.push(ident(0x31));
        attached.push(ident(0x99));
        let second_id = ident(0x62);
        let post = encode_post(second_id, conversation, "two").unwrap();
        let accepted = accept_post(&dir, &mut store, &attached, laptop_app, &post, 100).unwrap();
        assert_eq!(
            accepted
                .notices
                .iter()
                .map(|(app, _)| *app)
                .collect::<Vec<_>>(),
            vec![phone_app]
        );
        let request = deliver_notice(
            &mut link,
            &mut phone,
            home_app,
            phone_app,
            &accepted.notices[0].1,
        );
        let (asked, after) = decode_history_req(&request).unwrap();
        assert_eq!(asked, conversation);
        assert_eq!(after, Some(first_id));
        link.deliver(phone_app, home_app, &request);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = on_history(&dir, &store, sender, &payload).unwrap();
        let page = decode_history_resp(&resp).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].text, "two");
        assert!(!page.more);
        link.deliver(home_app, phone_app, &resp);
        let (sender, payload) = link.take(phone_app).unwrap();
        assert!(phone.on_history(sender, &payload).is_none());
        assert_eq!(
            phone
                .messages()
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[test]
    fn device_keeps_asking_when_more_remain() {
        let (dir, laptop_app, phone_app, home_app) = room();
        let tmp = Temp::new("more-notice");
        let mut store = Store::open(tmp.path()).unwrap();
        let conversation = ident(0x71);
        store
            .create_conversation(conversation, "general", 1)
            .unwrap();
        let mut server = ServerHello::new();
        let mut link = FakeSocket::new();
        attach(&mut link, &mut server, &dir, laptop_app, home_app);
        attach(&mut link, &mut server, &dir, phone_app, home_app);

        // A 400-byte message encodes to 442 bytes. Two entries plus the
        // 20-byte header fit in 1024; the third does not.
        let text = "m".repeat(400);
        let mut notices = Vec::new();
        for n in 0..3u8 {
            let post = encode_post(ident(0x81 + n), conversation, &text).unwrap();
            let accepted = accept_post(
                &dir,
                &mut store,
                server.attached(),
                laptop_app,
                &post,
                u64::from(n),
            )
            .unwrap();
            assert_eq!(accepted.notices.len(), 1);
            notices.push(accepted.notices[0].1);
        }

        let mut phone = DeviceHistory::new();
        phone.note_directory(&dir);
        let request = deliver_notice(&mut link, &mut phone, home_app, phone_app, &notices[0]);
        link.deliver(phone_app, home_app, &request);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = on_history(&dir, &store, sender, &payload).unwrap();
        let page = decode_history_resp(&resp).unwrap();
        assert_eq!(page.messages.len(), 2);
        assert!(page.more);
        link.deliver(home_app, phone_app, &resp);
        let (sender, payload) = link.take(phone_app).unwrap();
        let (peer, next) = phone.on_history(sender, &payload).unwrap();
        assert_eq!(peer.app, home_app);
        let (_conversation, after) = decode_history_req(&next).unwrap();
        assert_eq!(after, Some(ident(0x82)));

        link.deliver(phone_app, home_app, &next);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = on_history(&dir, &store, sender, &payload).unwrap();
        link.deliver(home_app, phone_app, &resp);
        let (sender, payload) = link.take(phone_app).unwrap();
        assert!(phone.on_history(sender, &payload).is_none());
        assert_eq!(phone.messages().len(), 3);
        assert_eq!(phone.messages()[2].id, ident(0x83));

        let request = deliver_notice(&mut link, &mut phone, home_app, phone_app, &notices[1]);
        let (_conversation, after) = decode_history_req(&request).unwrap();
        assert_eq!(after, Some(ident(0x83)));
        link.deliver(phone_app, home_app, &request);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = on_history(&dir, &store, sender, &payload).unwrap();
        let page = decode_history_resp(&resp).unwrap();
        assert!(page.messages.is_empty());
        assert!(!page.more);
        link.deliver(home_app, phone_app, &resp);
        let (sender, payload) = link.take(phone_app).unwrap();
        assert!(phone.on_history(sender, &payload).is_none());
        assert_eq!(phone.messages().len(), 3);
    }
}
