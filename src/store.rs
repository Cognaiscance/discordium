//! Conversation record for the server that holds it.
//!
//! The directory is `~/.pnet/discordium/`. This file is `record`, beside the
//! token file from registration. One process writes it. Order is the order
//! the server accepted each row, not the order of the timestamps.
//!
//! A message id is chosen by the sending device and is unique inside its
//! conversation. Saving that id again returns the row already stored.
//!
//! The record role opens the store. Adding, appending, and reading are what
//! a later step calls once a device can ask. This step's tests cover them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"DMR1";
const MAX_TEXT: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub id: [u8; 16],
    pub title: String,
    pub created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: [u8; 16],
    pub conversation: [u8; 16],
    pub device: [u8; 16],
    pub sent_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Page {
    pub messages: Vec<Message>,
    pub more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum CreateOutcome {
    Created(Conversation),
    Existing(Conversation),
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum AppendOutcome {
    Saved(Message),
    Existing(Message),
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Corrupt(&'static str),
    #[cfg_attr(not(test), allow(dead_code))]
    UnknownConversation,
    #[cfg_attr(not(test), allow(dead_code))]
    UnknownCursor,
    TooLarge,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(err) => write!(f, "{err}"),
            StoreError::Corrupt(reason) => write!(f, "record file is corrupt: {reason}"),
            StoreError::UnknownConversation => f.write_str("no conversation with that id"),
            StoreError::UnknownCursor => {
                f.write_str("cursor is not a message in that conversation")
            }
            StoreError::TooLarge => f.write_str("text is too long"),
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(err: io::Error) -> Self {
        StoreError::Io(err)
    }
}

struct Row {
    conversation: Conversation,
    messages: Vec<Message>,
}

pub struct Store {
    dir: PathBuf,
    rows: Vec<Row>,
}

impl Store {
    /// Create the directory if it is missing, or load the record already there.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(dir)?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        let path = record_path(dir);
        let rows = if path.exists() {
            decode(&fs::read(&path)?)?
        } else {
            Vec::new()
        };
        let store = Store {
            dir: dir.to_path_buf(),
            rows,
        };
        if !path.exists() {
            store.save()?;
        }
        Ok(store)
    }

    pub fn list_conversations(&self) -> Vec<Conversation> {
        self.rows
            .iter()
            .map(|row| row.conversation.clone())
            .collect()
    }

    /// Add a conversation. The same id returns the conversation already saved
    /// and does not change its title or created time.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn create_conversation(
        &mut self,
        id: [u8; 16],
        title: &str,
        created_ms: u64,
    ) -> Result<CreateOutcome, StoreError> {
        if let Some(row) = self.rows.iter().find(|row| row.conversation.id == id) {
            return Ok(CreateOutcome::Existing(row.conversation.clone()));
        }
        check_text(title)?;
        let conversation = Conversation {
            id,
            title: title.to_string(),
            created_ms,
        };
        self.rows.push(Row {
            conversation: conversation.clone(),
            messages: Vec::new(),
        });
        if let Err(err) = self.save() {
            self.rows.pop();
            return Err(err);
        }
        Ok(CreateOutcome::Created(conversation))
    }

    /// Append one message. The same device message id in this conversation
    /// returns the copy already saved, including when the new text differs.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn append_message(
        &mut self,
        conversation: [u8; 16],
        id: [u8; 16],
        device: [u8; 16],
        sent_ms: u64,
        text: &str,
    ) -> Result<AppendOutcome, StoreError> {
        let Some(row_index) = self
            .rows
            .iter()
            .position(|row| row.conversation.id == conversation)
        else {
            return Err(StoreError::UnknownConversation);
        };
        if let Some(existing) = self.rows[row_index]
            .messages
            .iter()
            .find(|message| message.id == id)
        {
            return Ok(AppendOutcome::Existing(existing.clone()));
        }
        check_text(text)?;
        let message = Message {
            id,
            conversation,
            device,
            sent_ms,
            text: text.to_string(),
        };
        self.rows[row_index].messages.push(message.clone());
        if let Err(err) = self.save() {
            self.rows[row_index].messages.pop();
            return Err(err);
        }
        Ok(AppendOutcome::Saved(message))
    }

    /// Messages that follow `after`, in server order. `after: None` starts at
    /// the first message. `more` is true when the conversation has messages
    /// past this page.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn messages_after(
        &self,
        conversation: &[u8; 16],
        after: Option<[u8; 16]>,
        limit: usize,
    ) -> Result<Page, StoreError> {
        let Some(row) = self
            .rows
            .iter()
            .find(|row| row.conversation.id == *conversation)
        else {
            return Err(StoreError::UnknownConversation);
        };
        let start = match after {
            None => 0,
            Some(id) => {
                let position = row
                    .messages
                    .iter()
                    .position(|message| message.id == id)
                    .ok_or(StoreError::UnknownCursor)?;
                position + 1
            }
        };
        let rest = &row.messages[start..];
        let take = limit.min(rest.len());
        Ok(Page {
            messages: rest[..take].to_vec(),
            more: rest.len() > take,
        })
    }

    fn save(&self) -> Result<(), StoreError> {
        let bytes = encode(&self.rows)?;
        let tmp = self.dir.join(".record.tmp");
        let dest = record_path(&self.dir);
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &dest)?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
        if let Ok(dir) = File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

fn record_path(dir: &Path) -> PathBuf {
    dir.join("record")
}

fn encode(rows: &[Row]) -> Result<Vec<u8>, StoreError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    push_u32(&mut buf, count(rows.len())?);
    for row in rows {
        buf.extend_from_slice(&row.conversation.id);
        push_u64(&mut buf, row.conversation.created_ms);
        push_text(&mut buf, &row.conversation.title)?;
        push_u32(&mut buf, count(row.messages.len())?);
        for message in &row.messages {
            buf.extend_from_slice(&message.id);
            buf.extend_from_slice(&message.device);
            push_u64(&mut buf, message.sent_ms);
            push_text(&mut buf, &message.text)?;
        }
    }
    Ok(buf)
}

fn count(len: usize) -> Result<u32, StoreError> {
    u32::try_from(len).map_err(|_| StoreError::TooLarge)
}

fn check_text(text: &str) -> Result<(), StoreError> {
    if text.len() > MAX_TEXT {
        Err(StoreError::TooLarge)
    } else {
        Ok(())
    }
}

fn push_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn push_text(buf: &mut Vec<u8>, text: &str) -> Result<(), StoreError> {
    push_u32(buf, count(text.len())?);
    buf.extend_from_slice(text.as_bytes());
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<Vec<Row>, StoreError> {
    let mut reader = Reader {
        data: bytes,
        pos: 0,
    };
    let magic: [u8; 4] = reader.array()?;
    if &magic != MAGIC {
        return Err(StoreError::Corrupt("unrecognized record"));
    }
    let count = reader.u32()? as usize;
    let mut rows = Vec::new();
    for _ in 0..count {
        let id = reader.array()?;
        let created_ms = reader.u64()?;
        let title = reader.text()?;
        let message_count = reader.u32()? as usize;
        let mut messages = Vec::new();
        for _ in 0..message_count {
            let message_id = reader.array()?;
            let device = reader.array()?;
            let sent_ms = reader.u64()?;
            let text = reader.text()?;
            messages.push(Message {
                id: message_id,
                conversation: id,
                device,
                sent_ms,
                text,
            });
        }
        rows.push(Row {
            conversation: Conversation {
                id,
                title,
                created_ms,
            },
            messages,
        });
    }
    reader.finish()?;
    Ok(rows)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], StoreError> {
        let slice = self.take(N)?;
        let mut array = [0u8; N];
        array.copy_from_slice(slice);
        Ok(array)
    }

    fn u32(&mut self) -> Result<u32, StoreError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, StoreError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn text(&mut self) -> Result<String, StoreError> {
        let len = self.u32()? as usize;
        if len > MAX_TEXT {
            return Err(StoreError::TooLarge);
        }
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| StoreError::Corrupt("text is not utf-8"))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], StoreError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(StoreError::Corrupt("truncated"))?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or(StoreError::Corrupt("truncated"))?;
        self.pos = end;
        Ok(slice)
    }

    fn finish(self) -> Result<(), StoreError> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(StoreError::Corrupt("trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn ident(byte: u8) -> [u8; 16] {
        [byte; 16]
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

    #[test]
    fn empty_store_reopens_empty() {
        let dir = Temp::new("empty");
        let store = Store::open(dir.path()).unwrap();
        assert!(store.list_conversations().is_empty());
        assert!(matches!(
            store.messages_after(&ident(1), None, 10),
            Err(StoreError::UnknownConversation)
        ));
        let dir_mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        let file_mode = fs::metadata(dir.path().join("record"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        drop(store);

        let store = Store::open(dir.path()).unwrap();
        assert!(store.list_conversations().is_empty());
    }

    #[test]
    fn repeated_device_message_id_is_one_row() {
        let dir = Temp::new("repeat");
        let mut store = Store::open(dir.path()).unwrap();
        let conversation = ident(1);
        let created = store
            .create_conversation(conversation, "general", 1_000)
            .unwrap();
        assert_eq!(
            created,
            CreateOutcome::Created(Conversation {
                id: conversation,
                title: "general".to_string(),
                created_ms: 1_000,
            })
        );
        let again = store
            .create_conversation(conversation, "renamed", 9_000)
            .unwrap();
        assert_eq!(
            again,
            CreateOutcome::Existing(Conversation {
                id: conversation,
                title: "general".to_string(),
                created_ms: 1_000,
            })
        );

        let message_id = ident(9);
        let device = ident(4);
        let saved = store
            .append_message(conversation, message_id, device, 50, "hello")
            .unwrap();
        assert!(matches!(saved, AppendOutcome::Saved(_)));
        let repeated = store
            .append_message(conversation, message_id, ident(5), 99, "hello again")
            .unwrap();
        assert_eq!(
            repeated,
            AppendOutcome::Existing(Message {
                id: message_id,
                conversation,
                device,
                sent_ms: 50,
                text: "hello".to_string(),
            })
        );

        let page = store.messages_after(&conversation, None, 10).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert!(!page.more);
        assert_eq!(store.list_conversations().len(), 1);
    }

    #[test]
    fn messages_follow_server_order_after_a_cursor() {
        let dir = Temp::new("cursor");
        let mut store = Store::open(dir.path()).unwrap();
        let first = ident(1);
        let second = ident(2);
        store.create_conversation(first, "one", 1).unwrap();
        store.create_conversation(second, "two", 2).unwrap();
        assert_eq!(
            store
                .list_conversations()
                .iter()
                .map(|conversation| conversation.title.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );

        // Timestamps go backwards. Server order is append order.
        store
            .append_message(first, ident(10), ident(7), 300, "a")
            .unwrap();
        store
            .append_message(first, ident(11), ident(7), 100, "b")
            .unwrap();
        store
            .append_message(first, ident(12), ident(7), 200, "c")
            .unwrap();

        let page = store.messages_after(&first, Some(ident(10)), 1).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].text, "b");
        assert!(page.more);

        let page = store.messages_after(&first, Some(ident(11)), 10).unwrap();
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["c"]
        );
        assert!(!page.more);

        let page = store.messages_after(&first, Some(ident(12)), 10).unwrap();
        assert!(page.messages.is_empty());
        assert!(!page.more);

        assert!(matches!(
            store.messages_after(&first, Some(ident(99)), 10),
            Err(StoreError::UnknownCursor)
        ));
        assert!(matches!(
            store.append_message(ident(8), ident(1), ident(7), 1, "nope"),
            Err(StoreError::UnknownConversation)
        ));
    }

    #[test]
    fn store_leaves_the_token_file() {
        let dir = Temp::new("token");
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join("token"), [0xAB; 16]).unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.create_conversation(ident(1), "general", 1).unwrap();
        assert_eq!(fs::read(dir.path().join("token")).unwrap(), vec![0xAB; 16]);
    }

    #[test]
    fn corrupt_record_is_refused() {
        let dir = Temp::new("corrupt");
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join("record"), b"nope").unwrap();
        assert!(matches!(
            Store::open(dir.path()),
            Err(StoreError::Corrupt(_))
        ));
    }

    #[test]
    fn reopen_after_process_exit() {
        let dir = Temp::new("exit");
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "store::tests::write_record_and_exit",
            ])
            .env("DISCORDIUM_RECORD_FIXTURE", dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "child status {}: {stdout}{stderr}",
            output.status
        );
        assert!(
            stdout.contains("1 passed"),
            "child did not write the record: {stdout}{stderr}"
        );

        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.list_conversations().len(), 1);
        assert_eq!(store.list_conversations()[0].title, "general");
        let page = store.messages_after(&ident(1), None, 10).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].text, "kept");
        assert!(!page.more);
    }

    #[test]
    #[ignore = "spawned by reopen_after_process_exit"]
    fn write_record_and_exit() {
        let dir = std::env::var("DISCORDIUM_RECORD_FIXTURE").expect("fixture directory");
        let mut store = Store::open(Path::new(&dir)).unwrap();
        let conversation = ident(1);
        store
            .create_conversation(conversation, "general", 1_700)
            .unwrap();
        store
            .append_message(conversation, ident(2), ident(3), 1_800, "kept")
            .unwrap();
    }
}
