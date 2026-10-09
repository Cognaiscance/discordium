//! Open a conversation, and list the conversations already saved.
//!
//! `CREATE_REQ` carries a client id and a title. The same id returns the
//! conversation already stored, with its original title and time. `LIST_REQ`
//! returns those conversations in the order the server accepted them. A list
//! stays near 1 KiB. When more conversations remain, the next request
//! continues after the last id in the reply.
//!
//! The sender is an approved `discordium-client` on one of this user's own devices.
//! A prior HELLO is not required. Any other sender is ignored, and an app
//! that is missing from the directory is not remembered as a refusal.

use crate::directory::{approved_own_discordium, Directory};
use crate::hello::VERSION;
use crate::store::{Conversation, CreateOutcome, Store};

pub const CREATE_REQ: u8 = 0x03;
pub const CREATE_RESP: u8 = 0x04;
pub const LIST_REQ: u8 = 0x05;
pub const LIST_RESP: u8 = 0x06;

const PAGE_BUDGET: usize = 1024;
const DATAGRAM_MAX: usize = 4096;
const LIST_HEADER: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ListPage {
    pub conversations: Vec<Conversation>,
    pub more: bool,
}

pub fn is_create_req(payload: &[u8]) -> bool {
    decode_create_req(payload).is_some()
}

pub fn is_list_req(payload: &[u8]) -> bool {
    decode_list_req(payload).is_some()
}

/// Save a conversation, or return the one already saved for this client id.
pub fn on_create(
    dir: &Directory,
    store: &mut Store,
    sender: [u8; 16],
    payload: &[u8],
    now_ms: u64,
) -> Option<Vec<u8>> {
    let (id, title) = decode_create_req(payload)?;
    if !approved_own_discordium(dir, &sender) {
        return None;
    }
    let outcome = match store.create_conversation(id, &title, now_ms) {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!("discordium: create: {err}");
            return None;
        }
    };
    let conversation = match outcome {
        CreateOutcome::Created(conversation) | CreateOutcome::Existing(conversation) => {
            conversation
        }
    };
    encode_create_resp(&conversation)
}

/// Conversations after `LIST_REQ`'s cursor. An unknown cursor has no reply.
pub fn on_list(
    dir: &Directory,
    store: &Store,
    sender: [u8; 16],
    payload: &[u8],
) -> Option<Vec<u8>> {
    let after = decode_list_req(payload)?;
    if !approved_own_discordium(dir, &sender) {
        return None;
    }
    encode_list_page(&store.list_conversations(), after)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn encode_create_req(id: [u8; 16], title: &str) -> Option<Vec<u8>> {
    let bytes = title.as_bytes();
    if bytes.is_empty() || bytes.len() > 255 {
        return None;
    }
    let mut out = Vec::with_capacity(19 + bytes.len());
    out.push(VERSION);
    out.push(CREATE_REQ);
    out.extend_from_slice(&id);
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
    Some(out)
}

pub fn decode_create_req(payload: &[u8]) -> Option<([u8; 16], String)> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(CREATE_REQ) {
        return None;
    }
    let id: [u8; 16] = payload.get(2..18)?.try_into().ok()?;
    let title_len = usize::from(*payload.get(18)?);
    if title_len == 0 || payload.len() != 19 + title_len {
        return None;
    }
    let title = std::str::from_utf8(payload.get(19..19 + title_len)?).ok()?;
    Some((id, title.to_string()))
}

fn encode_create_resp(conversation: &Conversation) -> Option<Vec<u8>> {
    let title = conversation.title.as_bytes();
    if title.len() > 255 {
        return None;
    }
    let mut out = Vec::with_capacity(27 + title.len());
    out.push(VERSION);
    out.push(CREATE_RESP);
    out.extend_from_slice(&conversation.id);
    out.extend_from_slice(&conversation.created_ms.to_be_bytes());
    out.push(title.len() as u8);
    out.extend_from_slice(title);
    Some(out)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn decode_create_resp(payload: &[u8]) -> Option<Conversation> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(CREATE_RESP) {
        return None;
    }
    let id: [u8; 16] = payload.get(2..18)?.try_into().ok()?;
    let created_ms = u64::from_be_bytes(payload.get(18..26)?.try_into().ok()?);
    let title_len = usize::from(*payload.get(26)?);
    if payload.len() != 27 + title_len {
        return None;
    }
    let title = std::str::from_utf8(payload.get(27..27 + title_len)?).ok()?;
    Some(Conversation {
        id,
        title: title.to_string(),
        created_ms,
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn encode_list_req(after: Option<[u8; 16]>) -> Vec<u8> {
    let mut out = vec![VERSION, LIST_REQ];
    match after {
        None => out.push(0),
        Some(id) => {
            out.push(1);
            out.extend_from_slice(&id);
        }
    }
    out
}

pub fn decode_list_req(payload: &[u8]) -> Option<Option<[u8; 16]>> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(LIST_REQ) {
        return None;
    }
    match payload.get(2).copied() {
        Some(0) if payload.len() == 3 => Some(None),
        Some(1) if payload.len() == 19 => {
            let id: [u8; 16] = payload.get(3..19)?.try_into().ok()?;
            Some(Some(id))
        }
        _ => None,
    }
}

fn encode_list_page(conversations: &[Conversation], after: Option<[u8; 16]>) -> Option<Vec<u8>> {
    let start = match after {
        None => 0,
        Some(id) => conversations.iter().position(|item| item.id == id)? + 1,
    };
    let rest = &conversations[start..];
    let mut taken = 0usize;
    let mut size = LIST_HEADER;
    while taken < rest.len() && taken < 255 {
        let Some(entry) = encoded_entry_len(&rest[taken]) else {
            if taken == 0 {
                return None;
            }
            break;
        };
        let budget = if taken == 0 && LIST_HEADER + entry > PAGE_BUDGET {
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
    if taken == 0 && !rest.is_empty() {
        return None;
    }
    let more = taken < rest.len();
    Some(write_list_resp(&rest[..taken], more))
}

fn encoded_entry_len(conversation: &Conversation) -> Option<usize> {
    let title = conversation.title.len();
    if title > 255 {
        None
    } else {
        Some(25 + title)
    }
}

fn write_list_resp(conversations: &[Conversation], more: bool) -> Vec<u8> {
    let mut out = vec![
        VERSION,
        LIST_RESP,
        u8::from(more),
        conversations.len() as u8,
    ];
    for conversation in conversations {
        out.extend_from_slice(&conversation.id);
        out.extend_from_slice(&conversation.created_ms.to_be_bytes());
        out.push(conversation.title.len() as u8);
        out.extend_from_slice(conversation.title.as_bytes());
    }
    out
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn decode_list_resp(payload: &[u8]) -> Option<ListPage> {
    if payload.first().copied() != Some(VERSION) || payload.get(1).copied() != Some(LIST_RESP) {
        return None;
    }
    let more = match payload.get(2).copied() {
        Some(0) => false,
        Some(1) => true,
        _ => return None,
    };
    let count = usize::from(*payload.get(3)?);
    let mut rest = payload.get(4..)?;
    let mut conversations = Vec::with_capacity(count);
    for _ in 0..count {
        if rest.len() < 25 {
            return None;
        }
        let id: [u8; 16] = rest[..16].try_into().ok()?;
        let created_ms = u64::from_be_bytes(rest[16..24].try_into().ok()?);
        let title_len = usize::from(rest[24]);
        if rest.len() < 25 + title_len {
            return None;
        }
        let title = std::str::from_utf8(&rest[25..25 + title_len])
            .ok()?
            .to_string();
        rest = &rest[25 + title_len..];
        conversations.push(Conversation {
            id,
            title,
            created_ms,
        });
    }
    if !rest.is_empty() {
        return None;
    }
    Some(ListPage {
        conversations,
        more,
    })
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

    fn two_nodes() -> (Directory, [u8; 16], [u8; 16], [u8; 16]) {
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
        (dir, laptop_app, home, home_app)
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

    fn reply(
        server: Option<Vec<u8>>,
        link: &mut FakeSocket,
        from: [u8; 16],
        to: [u8; 16],
    ) -> Vec<u8> {
        let bytes = server.expect("reply");
        link.deliver(from, to, &bytes);
        let (_sender, payload) = link.take(to).unwrap();
        payload
    }

    #[test]
    fn created_conversation_appears_in_the_list_once() {
        let (dir, laptop_app, _home, home_app) = two_nodes();
        let tmp = Temp::new("once");
        let mut store = Store::open(tmp.path()).unwrap();
        let mut link = FakeSocket::new();
        let created_ms = 0x0102_0304_0506_0708;

        let empty = encode_list_req(None);
        link.deliver(laptop_app, home_app, &empty);
        let (sender, payload) = link.take(home_app).unwrap();
        let page = decode_list_resp(&reply(
            on_list(&dir, &store, sender, &payload),
            &mut link,
            home_app,
            laptop_app,
        ))
        .unwrap();
        assert!(page.conversations.is_empty());
        assert!(!page.more);

        let id = ident(0x41);
        let req = encode_create_req(id, "general").unwrap();
        link.deliver(laptop_app, home_app, &req);
        let (sender, payload) = link.take(home_app).unwrap();
        let resp = reply(
            on_create(&dir, &mut store, sender, &payload, created_ms),
            &mut link,
            home_app,
            laptop_app,
        );
        assert_eq!(&resp[18..26], &created_ms.to_be_bytes());
        let created = decode_create_resp(&resp).unwrap();
        assert_eq!(
            created,
            Conversation {
                id,
                title: "general".to_string(),
                created_ms,
            }
        );

        let retry = encode_create_req(id, "other title").unwrap();
        link.deliver(laptop_app, home_app, &retry);
        let (sender, payload) = link.take(home_app).unwrap();
        let again = decode_create_resp(&reply(
            on_create(&dir, &mut store, sender, &payload, 9_000),
            &mut link,
            home_app,
            laptop_app,
        ))
        .unwrap();
        assert_eq!(again, created);

        link.deliver(laptop_app, home_app, &encode_list_req(None));
        let (sender, payload) = link.take(home_app).unwrap();
        let page = decode_list_resp(&reply(
            on_list(&dir, &store, sender, &payload),
            &mut link,
            home_app,
            laptop_app,
        ))
        .unwrap();
        assert_eq!(page.conversations, vec![created]);
        assert!(!page.more);
        assert_eq!(store.list_conversations().len(), 1);
    }

    #[test]
    fn other_senders_do_not_open_a_conversation() {
        let (mut dir, laptop_app, _home, home_app) = two_nodes();
        dir.own_devices[0]
            .apps
            .push(app(ident(0x12), CLIENT_ALIAS, false));
        dir.own_devices[0]
            .apps
            .push(app(ident(0x13), "notes", true));
        let tmp = Temp::new("others");
        let mut store = Store::open(tmp.path()).unwrap();
        let mut link = FakeSocket::new();
        let req = encode_create_req(ident(0x41), "nope").unwrap();

        for sender in [ident(0x31), ident(0x12), ident(0x13), ident(0x99)] {
            link.deliver(sender, home_app, &req);
            let (from, payload) = link.take(home_app).unwrap();
            assert!(on_create(&dir, &mut store, from, &payload, 1).is_none());
            link.deliver(sender, home_app, &encode_list_req(None));
            let (from, payload) = link.take(home_app).unwrap();
            assert!(on_list(&dir, &store, from, &payload).is_none());
            assert!(link.take(sender).is_none());
        }
        assert!(store.list_conversations().is_empty());

        let mut empty_title = vec![VERSION, CREATE_REQ];
        empty_title.extend_from_slice(&ident(1));
        empty_title.push(0);
        let mut bad_utf8 = vec![VERSION, CREATE_REQ];
        bad_utf8.extend_from_slice(&ident(1));
        bad_utf8.push(1);
        bad_utf8.push(0xff);
        let mut trailing = encode_create_req(ident(1), "x").unwrap();
        trailing.push(0);
        for bad in [
            vec![2, CREATE_REQ],
            trailing,
            empty_title,
            bad_utf8,
            vec![VERSION, LIST_REQ],
            vec![VERSION, LIST_REQ, 0, 0],
            vec![VERSION, LIST_REQ, 2],
            vec![VERSION, LIST_REQ, 1],
        ] {
            assert!(on_create(&dir, &mut store, laptop_app, &bad, 1).is_none());
            assert!(on_list(&dir, &store, laptop_app, &bad).is_none());
        }
        assert!(store.list_conversations().is_empty());

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
            decode_create_resp(&on_create(&dir, &mut store, from, &payload, 5).unwrap()).unwrap();
        assert_eq!(saved.title, "nope");
        assert_eq!(saved.created_ms, 5);

        link.deliver(ident(0x31), home_app, &encode_list_req(None));
        let (from, payload) = link.take(home_app).unwrap();
        assert!(on_list(&dir, &store, from, &payload).is_none());
        assert_eq!(store.list_conversations(), vec![saved]);
    }

    #[test]
    fn unknown_list_cursor_has_no_reply() {
        let (dir, laptop_app, _home, _home_app) = two_nodes();
        let tmp = Temp::new("cursor");
        let mut store = Store::open(tmp.path()).unwrap();
        let id = ident(0x41);
        let req = encode_create_req(id, "general").unwrap();
        on_create(&dir, &mut store, laptop_app, &req, 10).unwrap();

        assert!(on_list(
            &dir,
            &store,
            laptop_app,
            &encode_list_req(Some(ident(0x99)))
        )
        .is_none());
        let page = decode_list_resp(
            &on_list(&dir, &store, laptop_app, &encode_list_req(Some(id))).unwrap(),
        )
        .unwrap();
        assert!(page.conversations.is_empty());
        assert!(!page.more);

        let page =
            decode_list_resp(&on_list(&dir, &store, laptop_app, &encode_list_req(None)).unwrap())
                .unwrap();
        assert_eq!(page.conversations.len(), 1);
        assert!(!page.more);
    }

    #[test]
    fn a_long_list_says_more_remain() {
        let (dir, laptop_app, _home, _home_app) = two_nodes();
        let tmp = Temp::new("more");
        let mut store = Store::open(tmp.path()).unwrap();
        // A 200-byte title encodes to 225 bytes. Four entries plus the 4-byte
        // header fit in 1024; the fifth does not.
        let title = "t".repeat(200);
        let mut ids = Vec::new();
        for n in 0..4u8 {
            let id = ident(0x50 + n);
            ids.push(id);
            let req = encode_create_req(id, &title).unwrap();
            let saved = decode_create_resp(
                &on_create(&dir, &mut store, laptop_app, &req, 1_000 - u64::from(n)).unwrap(),
            )
            .unwrap();
            assert_eq!(saved.created_ms, 1_000 - u64::from(n));
        }

        let page =
            decode_list_resp(&on_list(&dir, &store, laptop_app, &encode_list_req(None)).unwrap())
                .unwrap();
        assert_eq!(page.conversations.len(), 4);
        assert!(!page.more);
        assert_eq!(
            page.conversations
                .iter()
                .map(|conversation| conversation.created_ms)
                .collect::<Vec<_>>(),
            vec![1_000, 999, 998, 997]
        );

        let fifth = ident(0x54);
        ids.push(fifth);
        let req = encode_create_req(fifth, &title).unwrap();
        on_create(&dir, &mut store, laptop_app, &req, 1).unwrap();

        let first =
            decode_list_resp(&on_list(&dir, &store, laptop_app, &encode_list_req(None)).unwrap())
                .unwrap();
        assert!(first.more);
        assert_eq!(first.conversations.len(), 4);
        let again =
            decode_list_resp(&on_list(&dir, &store, laptop_app, &encode_list_req(None)).unwrap())
                .unwrap();
        assert_eq!(again, first);

        let cursor = first.conversations.last().unwrap().id;
        let second = decode_list_resp(
            &on_list(&dir, &store, laptop_app, &encode_list_req(Some(cursor))).unwrap(),
        )
        .unwrap();
        assert!(!second.more);
        assert_eq!(second.conversations.len(), 1);
        assert_eq!(second.conversations[0].id, fifth);

        let seen: Vec<_> = first
            .conversations
            .iter()
            .chain(second.conversations.iter())
            .map(|conversation| conversation.id)
            .collect();
        assert_eq!(seen, ids);
    }
}
