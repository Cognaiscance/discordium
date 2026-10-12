//! The client window. The server process does not open one.
//!
//! The list and the thread show only what the record server returned.
//! Enter in the title box opens a conversation. Enter in the message box
//! sends that message.

use std::time::{Duration, Instant};

use eframe::egui;

use crate::page::View;
use crate::runtime::DeviceSession;

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
        }
        self.last_refresh = Instant::now();
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
                if self.view.thread.is_none() {
                    if let Some(note) = self.view.note {
                        ui.label(note);
                    }
                }
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
            }
            Some(id) => {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for message in &self.view.messages {
                            ui.label(&message.text);
                        }
                    });
                if let Some(note) = self.view.note {
                    ui.label(note);
                }
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
}
