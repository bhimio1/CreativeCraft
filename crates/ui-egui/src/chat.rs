//! The AI chat page. Each send runs one turn of the chosen agent CLI (see `creativecraft-agent`);
//! streamed events are folded into the transcript as they arrive.

use crate::{CreativeCraftApp, theme};
use creativecraft_engine::Command;
use creativecraft_engine::agent::{self, Backend, BackendStatus, ChatEvent, McpServer, Role, TurnRequest};
use egui::RichText;

const SUGGESTIONS: &[&str] = &[
    "Which of my apps can you control?",
    "Open PhotoCraft and start a 1080×1080 canvas with a warm gradient background",
    "Install VectorCraft and trace logo.png into a vector",
    "Sync my projects, then tell me what changed",
];

pub(crate) struct ChatState {
    pub convo: agent::Conversation,
    pub file: std::path::PathBuf,
    pub input: String,
    pub running: Option<agent::Running>,
    /// (backend, binary setting, checked-at time, result): `PATH` is scanned at most every 3 s.
    pub status: Option<(Backend, Option<String>, f64, BackendStatus)>,
}

impl ChatState {
    pub fn new(file: std::path::PathBuf) -> ChatState {
        ChatState { convo: agent::Conversation::load(&file), file, input: String::new(), running: None, status: None }
    }
}

impl CreativeCraftApp {
    fn backend_status(&mut self, now: f64) -> BackendStatus {
        let (b, bin) = (self.draft.agent.backend, self.draft.agent.binary.clone());
        match &self.chat.status {
            Some((cb, cbin, at, st)) if *cb == b && *cbin == bin && now - at < 3.0 => st.clone(),
            _ => {
                let st = b.status(bin.as_deref().filter(|s| !s.trim().is_empty()));
                self.chat.status = Some((b, bin, now, st.clone()));
                st
            }
        }
    }

    pub(crate) fn pump_chat(&mut self) {
        let Some(r) = &self.chat.running else { return };
        let mut done = false;
        while let Ok(ev) = r.events.try_recv() {
            done |= ev == ChatEvent::Done;
            self.chat.convo.apply(&ev);
        }
        if done {
            self.chat.running = None;
            if let Err(e) = self.chat.convo.save(&self.chat.file) {
                log::warn!("couldn't save the chat: {e}");
            }
        } else {
            // the reader thread doesn't know about egui; poll while a turn runs
            self.ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn send(&mut self) {
        let prompt = self.chat.input.trim().to_string();
        if prompt.is_empty() || self.chat.running.is_some() {
            return;
        }
        self.chat.input.clear();
        let backend = self.draft.agent.backend;
        self.chat.convo.push_user(backend, &prompt);
        let hub = McpServer::hub();
        let state = agent::state_dir(&self.draft.projects_dir);
        let _ = std::fs::create_dir_all(&self.draft.projects_dir);
        let session = self.chat.convo.session_for(backend).map(str::to_string);
        let req = TurnRequest {
            settings: &self.draft.agent,
            workspace: &self.draft.projects_dir,
            state_dir: &state,
            hub: &hub,
            prompt: &prompt,
            session: session.as_deref(),
        };
        match agent::start_turn(&req) {
            Ok(r) => self.chat.running = Some(r),
            Err(e) => {
                self.chat.convo.apply(&ChatEvent::Error(e.to_string()));
                let _ = self.chat.convo.save(&self.chat.file);
            }
        }
    }

    pub(crate) fn chat_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        let status = self.backend_status(now);

        // ── header: backend picker and its state
        ui.horizontal(|ui| {
            ui.heading("AI chat");
            ui.add_space(12.0);
            let before = self.draft.agent.backend;
            egui::ComboBox::from_id_salt("backend").selected_text(before.name()).show_ui(ui, |ui| {
                for b in Backend::ALL {
                    ui.selectable_value(&mut self.draft.agent.backend, b, b.name());
                }
            });
            if self.draft.agent.backend != before {
                self.save_draft();
            }
            match &status {
                BackendStatus::Found(p) => {
                    ui.label(RichText::new("● ready").color(t.ok)).on_hover_text(p.display().to_string());
                }
                BackendStatus::NeedsSetup(_, why) => {
                    ui.label(RichText::new(format!("needs setup: {why}")).color(t.accent));
                    if ui.add_enabled(self.busy.is_none(), theme::primary("Set up")).clicked() {
                        self.run("Set up chat", Command::SetupAgent);
                        self.chat.status = None;
                    }
                }
                BackendStatus::Missing if self.draft.agent.backend == Backend::Custom => {
                    let ok = !self.draft.agent.custom_command.trim().is_empty();
                    ui.label(RichText::new(if ok { "● custom command" } else { "set a command in Settings" }).color(if ok { t.ok } else { t.muted }));
                }
                BackendStatus::Missing => {
                    ui.label(RichText::new("not installed").color(t.danger));
                    ui.hyperlink_to("Get it ↗", self.draft.agent.backend.homepage());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(self.chat.running.is_none(), egui::Button::new("New chat")).clicked() {
                    self.chat.convo = agent::Conversation::default();
                    let _ = self.chat.convo.save(&self.chat.file);
                }
            });
        });
        ui.label(
            RichText::new("Your own agent CLI does the thinking; CreativeCraft gives it your apps as tools. Nothing here needs an API key.")
                .size(12.0)
                .color(t.muted),
        );
        ui.add_space(8.0);

        // ── transcript
        let input_h = 96.0;
        let list_h = (ui.available_height() - input_h - 16.0).max(120.0);
        egui::ScrollArea::vertical().id_salt("chat_log").stick_to_bottom(true).auto_shrink(false).max_height(list_h).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.chat.convo.messages.is_empty() {
                ui.add_space(24.0);
                ui.label(RichText::new("Try asking:").color(t.muted));
                for s in SUGGESTIONS {
                    if ui.add(egui::Button::new(RichText::new(*s).color(t.ink)).fill(t.card)).clicked() {
                        self.chat.input = (*s).to_string();
                    }
                }
            }
            for m in &self.chat.convo.messages {
                match m.role {
                    Role::User => {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            egui::Frame::NONE.fill(t.accent_soft).corner_radius(10).inner_margin(10).show(ui, |ui| {
                                ui.set_max_width(ui.available_width() * 0.75);
                                ui.label(RichText::new(&m.text).color(t.ink));
                            });
                        });
                    }
                    Role::Assistant => {
                        egui::Frame::NONE.fill(t.card).stroke(egui::Stroke::new(1.0, t.line)).corner_radius(10).inner_margin(10).show(ui, |ui| {
                            ui.set_max_width(ui.available_width() * 0.9);
                            ui.label(RichText::new(m.text.trim()).color(t.ink));
                        });
                    }
                    Role::Tool => {
                        ui.label(RichText::new(&m.text).monospace().size(11.0).color(t.muted));
                    }
                    Role::Notice => {
                        ui.label(RichText::new(&m.text).size(12.0).color(t.danger));
                    }
                }
                ui.add_space(6.0);
            }
            if self.chat.running.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(format!("{} is working…", self.draft.agent.backend.name())).color(t.muted));
                });
            }
        });

        // ── input
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let running = self.chat.running.is_some();
            let edit = ui.add_sized(
                [ui.available_width() - 90.0, input_h - 16.0],
                egui::TextEdit::multiline(&mut self.chat.input).hint_text("Ask CreativeCraft to… (Enter to send, Shift+Enter for a new line)"),
            );
            let enter = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            ui.vertical(|ui| {
                if running {
                    if ui.button("Stop").clicked()
                        && let Some(r) = &self.chat.running
                    {
                        r.cancel();
                    }
                } else if ui.add(theme::primary("Send")).clicked() || enter {
                    // the multiline edit already inserted the Enter's newline
                    while self.chat.input.ends_with('\n') {
                        self.chat.input.pop();
                    }
                    self.send();
                }
            });
        });
    }
}
