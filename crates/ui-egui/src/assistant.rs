//! The Assistant page: agent tabs. Each tab runs an agent CLI's own interactive UI in a terminal in
//! the projects folder, with CreativeCraft's MCP server wired in (see `creativecraft-agent`), so
//! several agents can work on different tasks at once. Tabs keep running while another is shown.
//!
//! Every agent gets its own hub process, so its app instances are its own: two agents never type
//! into the same app document. CreativeCraft never talks to a model itself; each CLI keeps its own
//! account and settings.

use crate::terminal::Terminal;
use crate::{CreativeCraftApp, Page, theme};
use creativecraft_engine::Command;
use creativecraft_engine::agent::{self, Backend, BackendStatus, McpServer, SessionRequest};
use egui::{RichText, vec2};
use std::collections::HashMap;

struct AgentTab {
    id: u64,
    title: String,
    backend: Backend,
    term: Option<Terminal>,
    error: Option<String>,
    /// Whether this tab has tried to start on its own yet.
    autostarted: bool,
    /// The title being edited (double-click a tab to rename it).
    renaming: Option<String>,
}

impl AgentTab {
    fn live(&self) -> bool {
        self.term.as_ref().is_some_and(|t| !t.exited())
    }

    fn stop(&mut self) {
        if let Some(mut t) = self.term.take() {
            t.kill();
        }
    }
}

#[derive(Default)]
pub(crate) struct AssistantState {
    tabs: Vec<AgentTab>,
    active: usize,
    next_id: u64,
    /// A running tab waiting for "End and close".
    confirm_close: Option<u64>,
    /// backend → (checked at, status): `PATH` is scanned at most every 3 s per backend.
    pub status: HashMap<Backend, (f64, BackendStatus)>,
}

impl AssistantState {
    /// Add a tab for `backend`, titled after it ("Claude Code", "Claude Code 2", …), and show it.
    fn add_tab(&mut self, backend: Backend) {
        let base = backend.name().to_string();
        let taken = |t: &str| self.tabs.iter().any(|x| x.title == t);
        let title = if taken(&base) { (2..).map(|n| format!("{base} {n}")).find(|t| !taken(t)).unwrap_or(base) } else { base };
        self.next_id += 1;
        self.tabs.push(AgentTab { id: self.next_id, title, backend, term: None, error: None, autostarted: false, renaming: None });
        self.active = self.tabs.len() - 1;
    }

    #[cfg(debug_assertions)]
    pub(crate) fn debug_add_tab(&mut self, backend: Backend) {
        self.add_tab(backend);
    }

    fn close_tab(&mut self, id: u64) {
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs[i].stop();
            self.tabs.remove(i);
            if self.active >= i && self.active > 0 {
                self.active -= 1;
            }
        }
        self.confirm_close = None;
    }
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

    fn backend_ready(&mut self, b: Backend, now: f64) -> bool {
        match self.cached_status(b, now) {
            BackendStatus::Found(_) => true,
            _ => b == Backend::Custom && !self.draft.agent.custom_command.trim().is_empty(),
        }
    }

    /// (Re)start the agent in tab `i`.
    fn start_tab(&mut self, i: usize) {
        let Some(tab) = self.assistant.tabs.get_mut(i) else { return };
        tab.stop();
        // the tab's agent, with the saved settings; a custom program path belongs to the default agent
        let mut settings = self.draft.agent.clone();
        if settings.backend != tab.backend {
            settings.binary = None;
            settings.backend = tab.backend;
        }
        let _ = std::fs::create_dir_all(&self.draft.projects_dir);
        let hub = McpServer::hub();
        let state = agent::state_dir(&self.draft.projects_dir);
        let req = SessionRequest { settings: &settings, workspace: &self.draft.projects_dir, state_dir: &state, hub: &hub };
        match agent::interactive_launch(&req).map_err(|e| e.to_string()).and_then(|spec| Terminal::spawn(&spec, &self.ctx, self.draft.terminal.scrollback)) {
            Ok(term) => {
                tab.term = Some(term);
                tab.error = None;
            }
            Err(e) => tab.error = Some(e),
        }
    }

    pub(crate) fn assistant_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        if self.assistant.tabs.is_empty() {
            self.assistant.add_tab(self.draft.agent.backend);
        }
        self.assistant.active = self.assistant.active.min(self.assistant.tabs.len() - 1);
        let active = self.assistant.active;
        // a new tab starts by itself when its agent is ready
        let backend = self.assistant.tabs[active].backend;
        let ready = self.backend_ready(backend, now);
        if !self.assistant.tabs[active].autostarted && self.assistant.tabs[active].term.is_none() {
            self.assistant.tabs[active].autostarted = true;
            if ready {
                self.start_tab(active);
            }
        }

        // ── header
        ui.horizontal(|ui| {
            ui.label(RichText::new("Assistant").size(22.0).strong().color(t.ink));
            ui.add_space(10.0);
            let running = self.assistant.tabs.iter().filter(|t| t.live()).count();
            if running > 0 {
                theme::status_pill(ui, t.ok, &if running == 1 { "1 agent running".to_string() } else { format!("{running} agents running") });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("⚙").size(15.0)).frame(false)).on_hover_text("Assistant settings").clicked() {
                    self.settings_section = crate::settings::Section::Assistant;
                    self.page = Page::Settings;
                }
                if self.assistant.tabs[active].live() {
                    if ui.add(theme::secondary("End")).on_hover_text("Stop this agent").clicked() {
                        self.assistant.tabs[active].stop();
                    }
                    if ui.add(theme::secondary("Restart")).on_hover_text("Start this agent again with a fresh session").clicked() {
                        self.start_tab(active);
                    }
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            RichText::new("Run several agents at once, one per tab. Each works in your projects folder with its own app instances.").size(12.0).color(t.muted),
        )
        .on_hover_text(self.draft.projects_dir.display().to_string());
        ui.add_space(10.0);

        self.tab_bar(ui, now);
        ui.add_space(8.0);

        // ── confirm closing a running agent
        if let Some(id) = self.assistant.confirm_close {
            let title = self.assistant.tabs.iter().find(|t| t.id == id).map(|t| t.title.clone()).unwrap_or_default();
            let mut decision = None;
            egui::Frame::NONE.fill(t.row).stroke(egui::Stroke::new(1.0, t.line)).corner_radius(10).inner_margin(10).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{title} is still running. Close the tab and stop it?")).color(t.ink));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::primary("End and close")).clicked() {
                            decision = Some(true);
                        }
                        if ui.add(theme::secondary("Cancel")).clicked() {
                            decision = Some(false);
                        }
                    });
                });
            });
            match decision {
                Some(true) => self.assistant.close_tab(id),
                Some(false) => self.assistant.confirm_close = None,
                None => {}
            }
            ui.add_space(8.0);
            if self.assistant.tabs.is_empty() {
                return;
            }
        }

        // ── body: the active tab
        let active = self.assistant.active.min(self.assistant.tabs.len() - 1);
        let backend = self.assistant.tabs[active].backend;
        let exited = self.assistant.tabs[active].term.as_ref().is_some_and(Terminal::exited);
        let settings = self.draft.terminal.clone();
        // Ctrl +/−/0 in the terminal: the text size setting (saved like any other)
        if let Some(zoom) = self.assistant.tabs[active].term.as_mut().and_then(|t| t.zoom.take()) {
            let size = &mut self.draft.terminal.font_size;
            *size = match zoom {
                0 => creativecraft_engine::TerminalSettings::default().font_size,
                z => (i16::from(*size) + i16::from(z)).clamp(9, 28) as u8,
            };
        }
        if let Some(term) = &mut self.assistant.tabs[active].term {
            if !exited {
                term.show(ui, &settings);
                return;
            }
            // keep the last screen visible above a restart bar
            let avail = ui.available_size();
            ui.allocate_ui(vec2(avail.x, (avail.y - 52.0).max(80.0)), |ui| term.show(ui, &settings));
            ui.add_space(10.0);
            let restart = ui
                .horizontal(|ui| {
                    ui.label(RichText::new("This agent's session ended.").color(t.muted));
                    ui.add(theme::primary("Start new session")).clicked()
                })
                .inner;
            if restart {
                self.start_tab(active);
            }
            return;
        }
        let status = self.cached_status(backend, now);
        self.assistant_empty(ui, active, backend, &status, ready);
    }

    /// One chip per agent tab (status dot, title, close), then "+ New agent".
    fn tab_bar(&mut self, ui: &mut egui::Ui, now: f64) {
        let t = theme::Tokens::get();
        let mut select = None;
        let mut close = None;
        let mut new_tab = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (i, tab) in self.assistant.tabs.iter_mut().enumerate() {
                let selected = i == self.assistant.active;
                let fill = if selected { t.row_hover } else { t.row };
                egui::Frame::NONE
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, if selected { t.muted } else { t.line }))
                    .corner_radius(8)
                    .inner_margin(egui::Margin { left: 10, right: 4, top: 4, bottom: 4 })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let dot = if tab.live() {
                                t.ok
                            } else if tab.error.is_some() {
                                t.danger
                            } else {
                                t.muted
                            };
                            let (r, _) = ui.allocate_exact_size(vec2(7.0, 7.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 3.5, dot);
                            if let Some(name) = &mut tab.renaming {
                                let edit = ui.add(egui::TextEdit::singleline(name).desired_width(120.0));
                                edit.request_focus();
                                if edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    if !name.trim().is_empty() {
                                        tab.title = name.trim().to_string();
                                    }
                                    tab.renaming = None;
                                }
                            } else {
                                let label = ui
                                    .add(
                                        egui::Label::new(RichText::new(&tab.title).size(13.0).color(if selected { t.ink } else { t.muted }))
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text(format!("{} · double-click to rename", tab.backend.name()));
                                if label.double_clicked() {
                                    tab.renaming = Some(tab.title.clone());
                                } else if label.clicked() {
                                    select = Some(i);
                                }
                            }
                            if ui.add(egui::Button::new(RichText::new("×").size(14.0).color(t.muted)).frame(false)).on_hover_text("Close tab").clicked() {
                                close = Some((tab.id, tab.live()));
                            }
                        });
                    });
            }
            // a new tab with any agent; the default one first
            let default = self.draft.agent.backend;
            ui.menu_button(RichText::new("+ New agent").size(13.0), |ui| {
                let mut order = vec![default];
                order.extend(Backend::ALL.into_iter().filter(|b| *b != default));
                for b in order {
                    let installed = matches!(self.assistant.status.get(&b).map(|(_, s)| s), Some(BackendStatus::Found(_)));
                    let label = if b == default { format!("{} (default)", b.name()) } else { b.name().to_string() };
                    let label = if installed || b == Backend::Custom { label } else { format!("{label} · not found") };
                    if ui.button(label).clicked() {
                        new_tab = Some(b);
                        ui.close();
                    }
                }
            });
        });
        if let Some(i) = select {
            self.assistant.active = i;
        }
        if let Some(b) = new_tab {
            // check the agent before its tab tries to start
            self.cached_status(b, now);
            self.assistant.add_tab(b);
        }
        match close {
            Some((id, true)) => {
                self.assistant.confirm_close = Some(id);
                if let Some(i) = self.assistant.tabs.iter().position(|t| t.id == id) {
                    self.assistant.active = i;
                }
            }
            Some((id, false)) => self.assistant.close_tab(id),
            None => {}
        }
        // keep install status fresh for the "+ New agent" menu
        for b in Backend::ALL {
            self.cached_status(b, now);
        }
    }

    /// Nothing running in this tab: say why and offer the one next step.
    fn assistant_empty(&mut self, ui: &mut egui::Ui, tab: usize, backend: Backend, status: &BackendStatus, ready: bool) {
        let t = theme::Tokens::get();
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 12.0, t.row);
        ui.painter().rect_stroke(rect, 12.0, egui::Stroke::new(1.0, t.line), egui::StrokeKind::Inside);
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(32.0)).layout(egui::Layout::top_down(egui::Align::Center)));
        let ui = &mut inner;
        ui.add_space((rect.height() / 2.0 - 130.0).max(10.0));
        let (tile, _) = ui.allocate_exact_size(vec2(56.0, 56.0), egui::Sense::hover());
        theme::letter_tile(ui, tile, backend.name());
        ui.add_space(12.0);

        // this tab's agent can be changed until it starts
        let mut pick = backend;
        egui::ComboBox::from_id_salt(("tab_agent", tab)).selected_text(pick.name()).width(180.0).show_ui(ui, |ui| {
            for b in Backend::ALL {
                ui.selectable_value(&mut pick, b, b.name());
            }
        });
        if pick != backend
            && let Some(tab) = self.assistant.tabs.get_mut(tab)
        {
            tab.backend = pick;
            tab.error = None;
            tab.autostarted = false;
        }
        ui.add_space(10.0);

        let idle = self.busy.is_none();
        match status {
            _ if ready => {
                ui.label(RichText::new(format!("Start {}", backend.name())).size(18.0).strong().color(t.ink));
                ui.label(RichText::new("It opens here, already connected to your apps.").color(t.muted));
                ui.add_space(14.0);
                if ui.add(theme::primary("Start session")).clicked() {
                    self.start_tab(tab);
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
        if let Some(e) = self.assistant.tabs.get(tab).and_then(|t| t.error.clone()) {
            ui.add_space(14.0);
            ui.label(RichText::new(e).size(12.0).color(t.danger));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_titles_and_closing() {
        let mut s = AssistantState::default();
        s.add_tab(Backend::ClaudeCode);
        s.add_tab(Backend::ClaudeCode);
        s.add_tab(Backend::OpenCode);
        let titles: Vec<&str> = s.tabs.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Claude Code", "Claude Code 2", "OpenCode"]);
        assert_eq!(s.active, 2);
        let first = s.tabs[0].id;
        s.close_tab(first);
        assert_eq!(s.active, 1);
        assert_eq!(s.tabs[s.active].title, "OpenCode");
        // the freed title is used again
        s.add_tab(Backend::ClaudeCode);
        assert_eq!(s.tabs.last().map(|t| t.title.as_str()), Some("Claude Code"));
    }
}
