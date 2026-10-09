//! The Assistant page: the chosen agent CLI's own interactive UI, running in a terminal in the
//! projects folder with CreativeCraft's MCP server wired in (see `creativecraft-agent`).
//! CreativeCraft never talks to a model itself; the CLI keeps its own account and settings.

use crate::terminal::Terminal;
use crate::{CreativeCraftApp, Page, theme};
use creativecraft_engine::Command;
use creativecraft_engine::agent::{self, Backend, BackendStatus, McpServer, SessionRequest};
use egui::{RichText, vec2};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct AssistantState {
    term: Option<Terminal>,
    /// Which agent the running session is.
    running: Option<Backend>,
    error: Option<String>,
    /// Start automatically the first time the page is shown.
    autostarted: bool,
    /// backend → (checked at, status): `PATH` is scanned at most every 3 s per backend.
    pub status: HashMap<Backend, (f64, BackendStatus)>,
}

impl CreativeCraftApp {
    pub(crate) fn cached_status(&mut self, b: Backend, now: f64) -> BackendStatus {
        if let Some((at, st)) = self.assistant.status.get(&b)
            && now - at < 3.0
        {
            return st.clone();
        }
        let bin = if b == self.draft.agent.backend { self.draft.agent.binary.clone() } else { None };
        let st = b.status(bin.as_deref().filter(|s| !s.trim().is_empty()));
        self.assistant.status.insert(b, (now, st.clone()));
        st
    }

    fn start_session(&mut self) {
        self.stop_session();
        let _ = std::fs::create_dir_all(&self.draft.projects_dir);
        let hub = McpServer::hub();
        let state = agent::state_dir(&self.draft.projects_dir);
        let req = SessionRequest { settings: &self.draft.agent, workspace: &self.draft.projects_dir, state_dir: &state, hub: &hub };
        match agent::interactive_launch(&req).map_err(|e| e.to_string()).and_then(|spec| Terminal::spawn(&spec, &self.ctx)) {
            Ok(term) => {
                self.assistant.term = Some(term);
                self.assistant.running = Some(self.draft.agent.backend);
                self.assistant.error = None;
            }
            Err(e) => self.assistant.error = Some(e),
        }
    }

    fn stop_session(&mut self) {
        if let Some(mut t) = self.assistant.term.take() {
            t.kill();
        }
        self.assistant.running = None;
    }

    pub(crate) fn assistant_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        let backend = self.draft.agent.backend;
        let status = self.cached_status(backend, now);
        let ready = matches!(status, BackendStatus::Found(_)) || (backend == Backend::Custom && !self.draft.agent.custom_command.trim().is_empty());
        if !self.assistant.autostarted && self.assistant.term.is_none() && ready {
            self.assistant.autostarted = true;
            self.start_session();
        }
        let exited = self.assistant.term.as_ref().is_some_and(Terminal::exited);
        let live = self.assistant.term.is_some() && !exited;

        // ── header: agent picker with status, session controls
        ui.horizontal(|ui| {
            ui.label(RichText::new("Assistant").size(22.0).strong().color(t.ink));
            ui.add_space(12.0);
            let before = self.draft.agent.backend;
            egui::ComboBox::from_id_salt("agent_pick").selected_text(before.name()).width(170.0).show_ui(ui, |ui| {
                for b in Backend::ALL {
                    ui.selectable_value(&mut self.draft.agent.backend, b, b.name());
                }
            });
            if self.draft.agent.backend != before {
                self.save_draft();
                self.assistant.error = None;
                self.assistant.autostarted = false; // start the newly chosen agent
                self.stop_session();
            }
            ui.add_space(4.0);
            let (dot, text) = match (&self.assistant.running, exited) {
                (Some(_), false) => (t.ok, "Running"),
                (Some(_), true) => (t.muted, "Ended"),
                (None, _) if ready => (t.muted, "Ready"),
                (None, _) => (t.danger, "Not available"),
            };
            theme::status_pill(ui, dot, text);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("⚙").size(15.0)).frame(false)).on_hover_text("Assistant settings").clicked() {
                    self.settings_section = crate::settings::Section::Assistant;
                    self.page = Page::Settings;
                }
                if live {
                    if ui.add(theme::secondary("End")).on_hover_text("Stop the agent").clicked() {
                        self.stop_session();
                    }
                    if ui.add(theme::secondary("Restart")).on_hover_text("Start a fresh session").clicked() {
                        self.start_session();
                    }
                }
            });
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Runs in your projects folder with CreativeCraft's tools. Your agent uses its own account and model.").size(12.0).color(t.muted))
            .on_hover_text(self.draft.projects_dir.display().to_string());
        ui.add_space(12.0);

        // ── body
        if let Some(term) = &mut self.assistant.term {
            if !exited {
                term.show(ui);
                return;
            }
            // keep the last screen visible above a restart bar
            let avail = ui.available_size();
            ui.allocate_ui(vec2(avail.x, (avail.y - 52.0).max(80.0)), |ui| term.show(ui));
            ui.add_space(10.0);
            let restart = ui
                .horizontal(|ui| {
                    ui.label(RichText::new("The session ended.").color(t.muted));
                    ui.add(theme::primary("Start new session")).clicked()
                })
                .inner;
            if restart {
                self.start_session();
            }
            return;
        }
        self.assistant_empty(ui, backend, &status, ready);
    }

    /// Nothing running: say why and offer the one next step.
    fn assistant_empty(&mut self, ui: &mut egui::Ui, backend: Backend, status: &BackendStatus, ready: bool) {
        let t = theme::Tokens::get();
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 12.0, t.row);
        ui.painter().rect_stroke(rect, 12.0, egui::Stroke::new(1.0, t.line), egui::StrokeKind::Inside);
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(32.0)).layout(egui::Layout::top_down(egui::Align::Center)));
        let ui = &mut inner;
        ui.add_space((rect.height() / 2.0 - 110.0).max(10.0));
        let (tile, _) = ui.allocate_exact_size(vec2(56.0, 56.0), egui::Sense::hover());
        theme::letter_tile(ui, tile, backend.name());
        ui.add_space(12.0);
        let idle = self.busy.is_none();
        match status {
            _ if ready => {
                ui.label(RichText::new(format!("Start {}", backend.name())).size(18.0).strong().color(t.ink));
                ui.label(RichText::new("It opens here, already connected to your apps.").color(t.muted));
                ui.add_space(14.0);
                if ui.add(theme::primary("Start session")).clicked() {
                    self.start_session();
                }
            }
            BackendStatus::NeedsSetup(_, why) => {
                ui.label(RichText::new(format!("{} needs one step", backend.name())).size(18.0).strong().color(t.ink));
                ui.label(RichText::new(format!("CreativeCraft will {why}.")).color(t.muted));
                ui.add_space(14.0);
                if ui.add_enabled(idle, theme::primary("Set up")).clicked() {
                    self.run("Set up assistant", Command::SetupAgent);
                    self.assistant.status.clear();
                }
            }
            _ if backend == Backend::Custom => {
                ui.label(RichText::new("Set your command").size(18.0).strong().color(t.ink));
                ui.label(RichText::new("Tell CreativeCraft which program to run in Settings.").color(t.muted));
                ui.add_space(14.0);
                if ui.add(theme::primary("Open settings")).clicked() {
                    self.settings_section = crate::settings::Section::Assistant;
                    self.page = Page::Settings;
                }
            }
            _ => {
                ui.label(RichText::new(format!("{} isn't installed", backend.name())).size(18.0).strong().color(t.ink));
                ui.label(RichText::new("Install it and sign in once in a terminal, or pick another agent above.").color(t.muted));
                ui.add_space(14.0);
                if ui.add(theme::primary("Get it")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(backend.homepage()));
                }
            }
        }
        if let Some(e) = &self.assistant.error {
            ui.add_space(14.0);
            ui.label(RichText::new(e).size(12.0).color(t.danger));
        }
    }
}
