//! The client window. The server process does not open one.
//!
//! The list and the thread show only what the record server returned.
//! The open conversation's heading is its title. Each message shows the
//! sending device's alias, the local time, and the text. Back returns to
//! the list prompt and leaves the side list. Enter in the title box opens
//! a conversation. Enter in the message box sends that message.

use std::time::{Duration, Instant};

use eframe::egui;

use crate::directory::{DeviceRecord, Directory};
use crate::page::View;
use crate::runtime::DeviceSession;
use crate::store::{Conversation, Message};

pub fn run(session: DeviceSession) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Discordium")
            .with_inner_size(egui::vec2(900.0, 600.0)),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Discordium",
        options,
        Box::new(|_cc| Ok(Box::new(Desktop::new(session)))),
    )
    .map_err(|err| err.to_string())
}

struct Desktop {
    session: DeviceSession,
    view: View,
    list: Vec<crate::store::Conversation>,
    title: String,
    draft: String,
    loaded: bool,
    last_refresh: Instant,
    action: Option<Action>,
}

enum Action {
    Open,
    OpenThread([u8; 16]),
    Send([u8; 16]),
    Back,
}

impl Desktop {
    fn new(session: DeviceSession) -> Self {
        Self {
            session,
            view: View {
                conversations: Vec::new(),
                messages: Vec::new(),
                note: Some("The server is not visible yet."),
                thread: None,
            },
            list: Vec::new(),
            title: String::new(),
            draft: String::new(),
            loaded: false,
            last_refresh: Instant::now(),
            action: None,
        }
    }

    fn remember_list(&mut self) {
        if self.view.thread.is_none() {
            self.list.clone_from(&self.view.conversations);
        }
    }

    fn refresh(&mut self) {
        self.view = match self.view.thread {
            Some(id) => self.session.open_thread(id),
            None => self.session.show_list(),
        };
        self.remember_list();
        self.last_refresh = Instant::now();
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Open => {
                let title = self.title.clone();
                self.view = self.session.open_title(&title);
                if self.view.note.is_none() {
                    self.title.clear();
                }
                self.remember_list();
            }
            Action::OpenThread(id) => {
                self.view = self.session.open_thread(id);
                self.draft.clear();
            }
            Action::Send(id) => {
                let draft = self.draft.clone();
                self.view = self.session.send_text(id, &draft);
                if self.view.note.is_none() {
                    self.draft.clear();
                }
            }
            Action::Back => leave_conversation(&mut self.view),
        }
        self.last_refresh = Instant::now();
    }
}

/// Alias, local time, then the text. The text is not escaped.
fn message_line(dir: &Directory, message: &Message) -> String {
    format!(
        "{} {} {}",
        device_label(dir, &message.device),
        local_time(message.sent_ms),
        message.text
    )
}

fn thread_lines(dir: &Directory, messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message_line(dir, message))
        .collect()
}

fn device_label(dir: &Directory, id: &[u8; 16]) -> String {
    named(dir.own_devices.iter(), id)
        .or_else(|| {
            named(
                dir.contacts
                    .iter()
                    .flat_map(|contact| contact.devices.iter()),
                id,
            )
        })
        .map(str::to_string)
        .unwrap_or_else(|| hex_prefix(id))
}

fn named<'a>(
    mut devices: impl Iterator<Item = &'a DeviceRecord>,
    id: &[u8; 16],
) -> Option<&'a str> {
    devices
        .find(|device| device.id == *id)
        .map(|device| device.alias.as_str())
        .filter(|alias| !alias.is_empty())
}

fn hex_prefix(id: &[u8; 16]) -> String {
    id.iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn local_time(sent_ms: u64) -> String {
    let Ok(secs) = libc::time_t::try_from(sent_ms / 1000) else {
        return sent_ms.to_string();
    };
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    let found = unsafe { libc::localtime_r(&secs, tm.as_mut_ptr()) };
    if found.is_null() {
        return sent_ms.to_string();
    }
    let tm = unsafe { tm.assume_init() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        i64::from(tm.tm_year) + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn conversation_heading(list: &[Conversation], id: [u8; 16]) -> &str {
    list.iter()
        .find(|row| row.id == id)
        .map(|row| row.title.as_str())
        .unwrap_or("")
}

fn leave_conversation(view: &mut View) {
    view.thread = None;
    view.messages.clear();
}

fn show_note(ui: &mut egui::Ui, note: Option<&str>) {
    if let Some(note) = note {
        ui.label(note);
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.session.pump();
        if let Some(view) = self.session.take_notice() {
            if self.view.thread == view.thread {
                self.view.messages = view.messages;
                self.view.note = view.note;
            }
        }
        if let Some(rows) = self.session.take_added() {
            self.list.clone_from(&rows);
            if self.view.thread.is_none() {
                self.view.conversations = rows;
            }
        }
        if self.session.hello_acked() && !self.loaded {
            self.view = self.session.show_list();
            self.remember_list();
            self.loaded = true;
            self.last_refresh = Instant::now();
        } else if self.loaded && self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.refresh();
        }

        egui::SidePanel::left("conversations")
            .resizable(false)
            .exact_width(260.0)
            .show(ctx, |ui| {
                ui.heading("Conversations");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for row in &self.list {
                        let open = self.view.thread == Some(row.id);
                        if ui.selectable_label(open, &row.title).clicked() {
                            self.action = Some(Action::OpenThread(row.id));
                        }
                    }
                });
                ui.separator();
                ui.label("Title");
                if enter_submits(ui, egui::Id::new("conversation-title"), &mut self.title)
                    || ui.button("Open").clicked()
                {
                    self.action = Some(Action::Open);
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| match self.view.thread {
            None => {
                ui.label("Open a conversation.");
                show_note(ui, self.view.note);
            }
            Some(id) => {
                ui.heading(conversation_heading(&self.list, id));
                if ui.button("Back").clicked() {
                    self.action = Some(Action::Back);
                }
                let directory = self.session.directory().clone();
                let lines = thread_lines(&directory, &self.view.messages);
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &lines {
                            ui.label(line);
                        }
                    });
                show_note(ui, self.view.note);
                ui.separator();
                ui.label("Message");
                if enter_submits(ui, egui::Id::new("message-draft"), &mut self.draft)
                    || ui.button("Send").clicked()
                {
                    self.action = Some(Action::Send(id));
                }
            }
        });

        if let Some(action) = self.action.take() {
            self.apply(action);
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

/// Enter while this box is focused.
///
/// The text from before the edit is kept when that key leaves the box empty.
/// The key is left in place when the box is not focused, so the other box can
/// use it.
fn enter_submits(ui: &mut egui::Ui, id: egui::Id, text: &mut String) -> bool {
    let focused = ui.memory(|memory| memory.has_focus(id));
    let typed = text.clone();
    let submit = focused && take_enter(ui);
    let _response = ui.add(egui::TextEdit::singleline(text).id(id));
    if let Some(kept) = text_after_submit(&typed, text, submit) {
        *text = kept;
    }
    submit
}

fn take_enter(ui: &mut egui::Ui) -> bool {
    let mut pressed = false;
    ui.input_mut(|input| {
        input.events.retain(|event| match event {
            egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                ..
            } => {
                pressed = true;
                false
            }
            _ => true,
        });
    });
    pressed
}

/// The text a submit should use. None when this key does not submit.
fn text_after_submit(typed: &str, edited: &str, submit: bool) -> Option<String> {
    if !submit {
        None
    } else if edited.is_empty() {
        Some(typed.to_string())
    } else {
        Some(edited.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn submit_keeps_text_that_the_edit_cleared() {
        assert_eq!(
            text_after_submit("general", "", true).as_deref(),
            Some("general")
        );
        assert_eq!(
            text_after_submit("hello", "hello", true).as_deref(),
            Some("hello")
        );
        assert_eq!(text_after_submit("", "", true).as_deref(), Some(""));
        assert_eq!(text_after_submit("general", "", false), None);
    }

    #[test]
    fn enter_submits_the_focused_box_only() {
        let ctx = egui::Context::default();
        let title_id = egui::Id::new("conversation-title");
        let message_id = egui::Id::new("message-draft");
        let mut title = String::from("general");
        let mut draft = String::from("hello");

        run_boxes(&ctx, &mut title, &mut draft, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(message_id));

        let (title_sent, message_sent) = run_boxes(&ctx, &mut title, &mut draft, vec![enter_key()]);
        assert!(!title_sent);
        assert!(message_sent);
        assert_eq!(title, "general");
        assert_eq!(draft, "hello");
        assert!(ctx.memory(|memory| memory.has_focus(message_id)));
        assert!(!ctx.memory(|memory| memory.has_focus(title_id)));
    }

    fn run_boxes(
        ctx: &egui::Context,
        title: &mut String,
        draft: &mut String,
        events: Vec<egui::Event>,
    ) -> (bool, bool) {
        let mut title_sent = false;
        let mut message_sent = false;
        let _ = ctx.run(input(events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                title_sent |= enter_submits(ui, egui::Id::new("conversation-title"), title);
                message_sent |= enter_submits(ui, egui::Id::new("message-draft"), draft);
            });
        });
        (title_sent, message_sent)
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            focused: true,
            events,
            ..Default::default()
        }
    }

    fn enter_key() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn message_line_uses_the_saved_device_and_time() {
        use crate::directory::{ContactRecord, Directory};
        use crate::store::{AppendOutcome, Store};

        const SENT: u64 = 1_700_000_000_000;
        let laptop = ident(0x10);
        let unnamed = ident(0x40);
        let unknown = [0xab, 0xcd, 0xef, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let cabin = ident(0x50);
        let conversation = ident(0x71);
        let dir = Directory {
            local_app_id: ident(1),
            local_app_alias: "discordium-client".into(),
            local_app_approved: true,
            token: ident(2),
            local_device: laptop,
            owner_alias: "owner".into(),
            owner_id: ident(3),
            own_devices: vec![device(laptop, "laptop"), device(unnamed, "")],
            contacts: vec![ContactRecord {
                alias: "guest".into(),
                id: ident(4),
                devices: vec![device(cabin, "cabin")],
            }],
        };
        let tmp = Temp::new("line");
        {
            let mut store = Store::open(tmp.path()).unwrap();
            store.create_conversation(conversation, "a<b c", 1).unwrap();
            save(&mut store, conversation, ident(0x61), laptop, SENT, "a<b c");
            save(
                &mut store,
                conversation,
                ident(0x62),
                unnamed,
                SENT,
                "blank",
            );
            save(
                &mut store,
                conversation,
                ident(0x63),
                unknown,
                SENT,
                "missing",
            );
            save(&mut store, conversation, ident(0x64), cabin, SENT, "cabin");
        }
        let store = Store::open(tmp.path()).unwrap();
        let page = store.messages_after(&conversation, None, 10).unwrap();
        let lines = thread_lines(&dir, &page.messages);
        let when = local_time(SENT);
        assert_eq!(lines[0], format!("laptop {when} a<b c"));
        assert_eq!(lines[1], format!("40404040 {when} blank"));
        assert_eq!(lines[2], format!("abcdef12 {when} missing"));
        assert_eq!(lines[3], format!("cabin {when} cabin"));
        assert_eq!(page.messages[0].device, laptop);
        assert_eq!(page.messages[0].sent_ms, SENT);

        fn save(
            store: &mut Store,
            conversation: [u8; 16],
            id: [u8; 16],
            device_id: [u8; 16],
            sent_ms: u64,
            text: &str,
        ) {
            match store
                .append_message(conversation, id, device_id, sent_ms, text)
                .unwrap()
            {
                AppendOutcome::Saved(_) => {}
                AppendOutcome::Existing(_) => panic!("new id was already saved"),
            }
        }
    }

    #[test]
    fn heading_is_the_saved_title_and_back_keeps_the_note() {
        let id = ident(0x71);
        let list = vec![Conversation {
            id,
            title: "a<b c".into(),
            created_ms: 1,
        }];
        assert_eq!(conversation_heading(&list, id), "a<b c");
        assert_eq!(conversation_heading(&list, ident(0x72)), "");

        let mut view = View {
            conversations: list.clone(),
            messages: vec![Message {
                id: ident(0x61),
                conversation: id,
                device: ident(0x10),
                sent_ms: 1,
                text: "hello".into(),
            }],
            note: Some("A message is required."),
            thread: Some(id),
        };
        leave_conversation(&mut view);
        assert!(view.thread.is_none());
        assert!(view.messages.is_empty());
        assert_eq!(view.note, Some("A message is required."));
        assert_eq!(view.conversations, list);
        assert_eq!(list[0].title, "a<b c");
    }

    fn ident(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn device(id: [u8; 16], alias: &str) -> crate::directory::DeviceRecord {
        crate::directory::DeviceRecord {
            id,
            alias: alias.to_string(),
            grade: crate::directory::Grade::Device,
            sg_rank: 0,
            apps: Vec::new(),
        }
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
}
