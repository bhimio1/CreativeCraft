//! The Settings page: a short list of sections on the left, one section's cards on the right.
//! Changes save themselves a moment after the last edit; nothing needs a Done button.

use crate::{CreativeCraftApp, theme};
use creativecraft_engine::agent::{Backend, BackendStatus};
use creativecraft_engine::{Appearance, CloudChoice, Command, CustomMcp};
use egui::{RichText, vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    General,
    Appearance,
    Cloud,
    Assistant,
    Connections,
    About,
}

const SECTIONS: [(Section, &str, &str); 6] = [
    (Section::General, "General", "Projects folder and this computer"),
    (Section::Appearance, "Appearance", "Theme and start-up"),
    (Section::Cloud, "Cloud sync", "Where your projects are backed up"),
    (Section::Assistant, "Assistant", "The agent that drives your apps"),
    (Section::Connections, "Connections", "Use CreativeCraft from other tools"),
    (Section::About, "About", "Version and licences"),
];

/// Seconds after the last edit before settings are written.
const SAVE_DELAY: f64 = 0.6;

impl CreativeCraftApp {
    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.horizontal_top(|ui| {
            // section list
            ui.vertical(|ui| {
                ui.set_width(190.0);
                ui.label(RichText::new("Settings").size(22.0).strong().color(t.ink));
                ui.add_space(14.0);
                for (s, label, _) in SECTIONS {
                    let sel = self.settings_section == s;
                    let (rect, resp) = ui.allocate_exact_size(vec2(180.0, 32.0), egui::Sense::click());
                    if sel || resp.hovered() {
                        ui.painter().rect_filled(rect, 8.0, if sel { t.row_hover } else { t.row });
                    }
                    if sel {
                        let bar = egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 8.0), vec2(3.0, 16.0));
                        ui.painter().rect_filled(bar, 1.5, t.ink);
                    }
                    ui.painter().text(
                        rect.left_center() + vec2(14.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(14.0),
                        if sel { t.ink } else { t.muted },
                    );
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.settings_section = s;
                    }
                }
            });
            ui.add_space(24.0);

            // the section
            ui.vertical(|ui| {
                ui.set_max_width(720.0_f32.min(ui.available_width()));
                let (title, sub) = SECTIONS.iter().find(|(s, ..)| *s == self.settings_section).map(|(_, l, d)| (*l, *d)).unwrap_or_default();
                ui.add_space(4.0);
                ui.label(RichText::new(title).size(18.0).strong().color(t.ink));
                ui.label(RichText::new(sub).size(13.0).color(t.muted));
                ui.add_space(14.0);
                egui::ScrollArea::vertical().id_salt("settings_scroll").auto_shrink(false).show(ui, |ui| match self.settings_section {
                    Section::General => self.settings_general(ui),
                    Section::Appearance => self.settings_appearance(ui),
                    Section::Cloud => self.settings_cloud(ui),
                    Section::Assistant => self.settings_assistant(ui),
                    Section::Connections => self.settings_connections(ui),
                    Section::About => self.settings_about(ui),
                });
            });
        });
        self.autosave(ui.input(|i| i.time));
    }

    /// Write the draft once edits have paused.
    fn autosave(&mut self, now: f64) {
        if self.draft == self.saved {
            self.dirty_since = None;
            return;
        }
        let since = *self.dirty_since.get_or_insert(now);
        if now - since >= SAVE_DELAY {
            self.save_draft();
            self.dirty_since = None;
        } else {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            let mut p = self.draft.projects_dir.display().to_string();
            theme::setting_row(ui, "Projects folder", "Save work anywhere inside it; this is the folder that syncs.", |ui| {
                if ui.add(theme::secondary("Show")).clicked() {
                    let _ = std::fs::create_dir_all(&self.draft.projects_dir);
                    let _ = open::that_detached(&self.draft.projects_dir);
                }
                if ui.add(egui::TextEdit::singleline(&mut p).desired_width(260.0)).changed() {
                    self.draft.projects_dir = p.clone().into();
                }
            });
            theme::row_divider(ui);
            theme::setting_row(ui, "Computer name", "Labels the copy kept from this computer when two edits conflict.", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.draft.device_name).desired_width(220.0));
            });
        });
    }

    fn settings_appearance(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            theme::setting_row(ui, "Theme", "Match system follows your computer's light or dark setting.", |ui| {
                theme::segmented(
                    ui,
                    &mut self.draft.appearance,
                    &[(Appearance::System, "Match system"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")],
                );
            });
            theme::row_divider(ui);
            theme::setting_row(ui, "Intro animation", "Plays for about two seconds when CreativeCraft opens. Click to skip it.", |ui| {
                theme::toggle(ui, &mut self.draft.intro_animation);
            });
        });
    }

    fn settings_cloud(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        #[derive(Clone, Copy, PartialEq)]
        enum Kind {
            Off,
            Drive,
            Folder,
        }
        let mut kind = match self.draft.cloud {
            CloudChoice::Off => Kind::Off,
            CloudChoice::GoogleDrive { .. } => Kind::Drive,
            CloudChoice::Folder { .. } => Kind::Folder,
        };
        theme::card(ui, |ui| {
            theme::setting_row(ui, "Sync with", "A synced folder works with any sync app you already use.", |ui| {
                if theme::segmented(ui, &mut kind, &[(Kind::Off, "Off"), (Kind::Drive, "Google Drive"), (Kind::Folder, "A folder")]) {
                    self.draft.cloud = match kind {
                        Kind::Off => CloudChoice::Off,
                        Kind::Drive => CloudChoice::GoogleDrive { client_id: String::new(), client_secret: String::new() },
                        Kind::Folder => CloudChoice::Folder { path: Default::default() },
                    };
                }
            });
            let signed_in = self.signed_in;
            let idle = self.busy.is_none();
            let mut action = None;
            match &mut self.draft.cloud {
                CloudChoice::GoogleDrive { client_id, client_secret } => {
                    theme::row_divider(ui);
                    theme::setting_row(
                        ui,
                        "Account",
                        if signed_in { "Signed in. CreativeCraft can only see files it created." } else { "Not signed in." },
                        |ui| {
                            if signed_in {
                                if ui.add_enabled(idle, theme::secondary("Sign out")).clicked() {
                                    action = Some(("Sign out", Command::SignOutGoogle));
                                }
                            } else if ui.add_enabled(idle && !client_id.is_empty(), theme::primary("Sign in")).clicked() {
                                action = Some(("Sign in", Command::SignInGoogle));
                            }
                        },
                    );
                    theme::row_divider(ui);
                    theme::setting_row(ui, "OAuth client", "The publisher's Desktop-app client ID and secret.", |ui| {
                        ui.vertical(|ui| {
                            ui.add(egui::TextEdit::singleline(client_id).hint_text("Client ID").desired_width(260.0));
                            ui.add(egui::TextEdit::singleline(client_secret).hint_text("Client secret").password(true).desired_width(260.0));
                        });
                    });
                }
                CloudChoice::Folder { path } => {
                    theme::row_divider(ui);
                    let mut p = path.display().to_string();
                    theme::setting_row(ui, "Folder", "A folder your sync app (Dropbox, OneDrive, Nextcloud…) backs up.", |ui| {
                        if ui.add(egui::TextEdit::singleline(&mut p).hint_text("e.g. ~/Dropbox/Art").desired_width(260.0)).changed() {
                            *path = p.clone().into();
                        }
                    });
                }
                CloudChoice::Off => {}
            }
            if let Some((label, cmd)) = action {
                self.run(label, cmd);
            }
            if !matches!(self.draft.cloud, CloudChoice::Off) {
                theme::row_divider(ui);
                theme::setting_row(ui, "Sync automatically", "Every few minutes while CreativeCraft is open. 0 means only when you click Sync.", |ui| {
                    ui.label(RichText::new("minutes").color(t.muted));
                    ui.add(egui::DragValue::new(&mut self.draft.auto_sync_minutes).range(0..=240));
                });
            }
        });
    }

    fn settings_assistant(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        ui.label(RichText::new("Agent").size(13.0).strong().color(t.muted));
        ui.add_space(6.0);
        // one card per agent CLI, with whether it's installed
        let width = ui.available_width();
        let per_row = if width > 640.0 { 3 } else { 2 };
        let card_w = (width - 12.0 * (per_row as f32 - 1.0)) / per_row as f32;
        for chunk in Backend::ALL.chunks(per_row) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                for &b in chunk {
                    let status = self.cached_status(b, now);
                    let sel = self.draft.agent.backend == b;
                    let (rect, resp) = ui.allocate_exact_size(vec2(card_w, 64.0), egui::Sense::click());
                    let fill = if resp.hovered() { t.row_hover } else { t.row };
                    ui.painter().rect_filled(rect, 12.0, fill);
                    ui.painter().rect_stroke(
                        rect,
                        12.0,
                        egui::Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { t.ink } else { t.line }),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(rect.left_top() + vec2(14.0, 14.0), egui::Align2::LEFT_TOP, b.name(), egui::FontId::proportional(14.0), t.ink);
                    let (dot, text) = match &status {
                        BackendStatus::Found(_) => (t.ok, "Installed"),
                        BackendStatus::NeedsSetup(..) => (t.danger, "Needs setup"),
                        BackendStatus::Missing if b == Backend::Custom => (t.muted, "Your own command"),
                        BackendStatus::Missing => (t.muted, "Not found"),
                    };
                    ui.painter().circle_filled(rect.left_bottom() + vec2(17.5, -20.5), 3.5, dot);
                    ui.painter().text(rect.left_bottom() + vec2(27.0, -14.0), egui::Align2::LEFT_BOTTOM, text, egui::FontId::proportional(12.0), t.muted);
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.draft.agent.backend = b;
                    }
                }
            });
            ui.add_space(12.0);
        }

        let b = self.draft.agent.backend;
        let status = self.cached_status(b, now);
        let idle = self.busy.is_none();
        let mut setup = false;
        theme::card(ui, |ui| {
            let a = &mut self.draft.agent;
            if b == Backend::Custom {
                theme::setting_row(ui, "Command", "{mcp_config} becomes CreativeCraft's MCP config file.", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut a.custom_command).hint_text("mytool --mcp {mcp_config}").desired_width(280.0));
                });
            } else {
                let desc = match &status {
                    BackendStatus::Found(p) => format!("Using {}", p.display()),
                    BackendStatus::NeedsSetup(_, why) => format!("Found, but needs one step: {why}."),
                    BackendStatus::Missing => format!("{} wasn't found on this computer. Install it, or point to it here.", b.name()),
                };
                theme::setting_row(ui, "Program", &desc, |ui| {
                    if matches!(status, BackendStatus::NeedsSetup(..)) && ui.add_enabled(idle, theme::primary("Set up")).clicked() {
                        setup = true;
                    }
                    if matches!(status, BackendStatus::Missing) {
                        ui.hyperlink_to("Get it", b.homepage());
                    }
                    let mut bin = a.binary.clone().unwrap_or_default();
                    if ui.add(egui::TextEdit::singleline(&mut bin).hint_text(b.default_binary()).desired_width(200.0)).changed() {
                        a.binary = (!bin.trim().is_empty()).then_some(bin);
                    }
                });
            }
            theme::row_divider(ui);
            theme::setting_row(ui, "Model", "In the agent's own notation. Empty uses its default.", |ui| {
                let mut model = a.model.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut model).hint_text("default").desired_width(200.0)).changed() {
                    a.model = (!model.trim().is_empty()).then_some(model);
                }
            });
            theme::row_divider(ui);
            theme::setting_row(
                ui,
                "Use every tool without asking",
                "Lets the agent run commands and edit files without asking first. Off: only CreativeCraft's own tools are pre-approved.",
                |ui| {
                    theme::toggle(ui, &mut a.auto_approve);
                },
            );
        });
        if setup {
            self.run("Set up assistant", Command::SetupAgent);
            self.assistant.status.clear();
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Changes apply the next time a session starts.").size(12.0).color(t.muted));
    }

    fn settings_connections(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        theme::card(ui, |ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("CreativeCraft as an MCP server").size(14.0).color(t.ink));
            ui.label(RichText::new("The Assistant uses it automatically. To use your apps from another MCP host, add:").size(12.0).color(t.muted));
            ui.add_space(8.0);
            let config = serde_json::json!({ "mcpServers": { "creativecraft": { "command": self.cli_path, "args": ["mcp"] } } });
            let text = serde_json::to_string_pretty(&config).unwrap_or_default();
            egui::Frame::NONE.fill(t.bar).corner_radius(8).inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    ui.monospace(&text);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if ui.add(theme::secondary("Copy")).clicked() {
                            ui.ctx().copy_text(text.clone());
                        }
                    });
                });
            });
            ui.add_space(12.0);
        });
        ui.add_space(16.0);
        ui.label(RichText::new("Extra MCP servers").size(13.0).strong().color(t.muted));
        ui.label(RichText::new("CreativeCraft fronts these too, e.g. a Blender add-on bridge.").size(12.0).color(t.muted));
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            let mut remove = None;
            for (i, c) in self.draft.custom_mcp.iter().enumerate() {
                theme::setting_row(ui, &c.id, &format!("{} {}", c.command, c.args.join(" ")), |ui| {
                    if ui.add(theme::secondary("Remove")).clicked() {
                        remove = Some(i);
                    }
                });
                theme::row_divider(ui);
            }
            if let Some(i) = remove {
                self.draft.custom_mcp.remove(i);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_mcp.id).hint_text("id").desired_width(90.0));
                ui.add(egui::TextEdit::singleline(&mut self.new_mcp.command).hint_text("command").desired_width(180.0));
                let mut args = self.new_mcp.args.join(" ");
                if ui.add(egui::TextEdit::singleline(&mut args).hint_text("arguments").desired_width(140.0)).changed() {
                    self.new_mcp.args = args.split_whitespace().map(String::from).collect();
                }
                let valid = !self.new_mcp.id.is_empty()
                    && self.new_mcp.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    && !self.new_mcp.command.is_empty()
                    && !self.rows.iter().any(|r| r.id == self.new_mcp.id)
                    && !self.draft.custom_mcp.iter().any(|c| c.id == self.new_mcp.id);
                if ui.add_enabled(valid, theme::primary("Add")).clicked() {
                    let mut c = self.new_mcp.clone();
                    c.name = c.id.clone();
                    self.draft.custom_mcp.push(c);
                    self.new_mcp = CustomMcp { id: String::new(), name: String::new(), command: String::new(), args: Vec::new() };
                }
            });
            ui.add_space(10.0);
        });
    }

    fn settings_about(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        theme::card(ui, |ui| {
            theme::setting_row(ui, "CreativeCraft", concat!("Version ", env!("CARGO_PKG_VERSION"), " · MIT OR Apache-2.0"), |ui| {
                // the workspace's `repository` until it's set to a real address
                let repo = env!("CARGO_PKG_REPOSITORY");
                if !repo.is_empty() && !repo.contains("YOUR-ORG") {
                    ui.hyperlink_to("Source", repo);
                }
            });
            theme::row_divider(ui);
            theme::setting_row(
                ui,
                "Report a bug",
                "Opens a new issue with your CreativeCraft version and system filled in. Nothing is sent until you submit it.",
                |ui| {
                    if ui.add_enabled(self.busy.is_none(), theme::secondary("Report a bug")).clicked() {
                        self.run("Report a bug", Command::ReportBug(None));
                    }
                },
            );
            theme::row_divider(ui);
            ui.add_space(10.0);
            ui.label(
                RichText::new(
                    "CreativeCraft is an independent project. It is not affiliated with, endorsed by, or sponsored by the ArtCraft Team \
                     or any other project or company it mentions. App names identify those apps; they remain their owners' marks. \
                     Apps are downloaded from their publishers' own release pages and are never bundled.",
                )
                .size(12.0)
                .color(t.muted),
            );
            ui.add_space(10.0);
        });
    }
}
