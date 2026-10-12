//! The client window. The server process does not open one.
//!
//! The list and the thread show only what the record server returned.

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
                ui.text_edit_singleline(&mut self.title);
                if ui.button("Open").clicked() {
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
                let enter = ui.text_edit_singleline(&mut self.draft).lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if ui.button("Send").clicked() || enter {
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
