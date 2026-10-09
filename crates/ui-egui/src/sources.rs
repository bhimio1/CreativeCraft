//! The "Install source" dialog: install a Craft app from its official releases, from a fork's
//! releases, or build it from git (the official repository or any fork, any branch).

use crate::{CreativeCraftApp, Row, theme};
use creativecraft_engine::{Command, Settings, Source};
use creativecraft_install::Status;
use egui::RichText;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Release,
    Fork,
    Git,
}

pub(crate) struct SourceDraft {
    id: String,
    name: String,
    installed: bool,
    kind: Kind,
    fork: String,
    git_url: String,
    branch: String,
    error: Option<String>,
}

impl SourceDraft {
    pub(crate) fn for_row(r: &Row, settings: &Settings) -> SourceDraft {
        let mut d = SourceDraft {
            id: r.id.clone(),
            name: r.name.clone(),
            installed: matches!(r.status, Status::Installed { .. }),
            kind: Kind::Release,
            fork: String::new(),
            git_url: String::new(),
            branch: String::new(),
            error: None,
        };
        match settings.app_sources.get(&r.id) {
            Some(Source::Fork { repo }) => {
                d.kind = Kind::Fork;
                d.fork = repo.clone();
            }
            Some(Source::Git { url, branch }) => {
                d.kind = Kind::Git;
                d.git_url = url.clone();
                d.branch = branch.clone();
            }
            _ => {}
        }
        d
    }

    fn source(&self) -> Source {
        match self.kind {
            Kind::Release => Source::Release,
            Kind::Fork => Source::Fork { repo: self.fork.trim().trim_start_matches("https://github.com/").trim_end_matches('/').to_string() },
            Kind::Git => Source::Git { url: self.git_url.trim().to_string(), branch: self.branch.trim().to_string() },
        }
    }
}

impl CreativeCraftApp {
    pub(crate) fn source_dialog_ui(&mut self, ctx: egui::Context) {
        let t = theme::Tokens::get();
        let Some(mut d) = self.source_dialog.take() else { return };
        let building = self.install_progress.as_ref().is_some_and(|(id, f, _)| *id == d.id && *f < 0.0);
        let idle = self.busy.is_none();
        let mut close = false;
        let mut apply: Option<bool> = None; // Some(install now?)

        let modal = egui::Modal::new(egui::Id::new("source_dialog")).show(&ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new(format!("Install source · {}", d.name)).size(18.0).strong().color(t.ink));
            ui.label(RichText::new("Where CreativeCraft gets this app when it installs or updates it.").size(12.0).color(t.muted));
            ui.add_space(12.0);
            theme::segmented(ui, &mut d.kind, &[(Kind::Release, "Official releases"), (Kind::Fork, "A fork's releases"), (Kind::Git, "Build from git")]);
            ui.add_space(12.0);
            match d.kind {
                Kind::Release => {
                    ui.label(RichText::new("Prebuilt downloads from the app's own GitHub releases, checked against their published checksums.").color(t.muted));
                }
                Kind::Fork => {
                    ui.label(RichText::new("GitHub repository").size(12.0).color(t.muted));
                    ui.add(egui::TextEdit::singleline(&mut d.fork).hint_text("owner/name, e.g. alice/photocraft").desired_width(f32::INFINITY));
                    ui.label(RichText::new("Uses the fork's latest release, which must publish builds for this computer.").size(12.0).color(t.muted));
                }
                Kind::Git => {
                    ui.label(RichText::new("Repository (empty: the official one)").size(12.0).color(t.muted));
                    ui.add(egui::TextEdit::singleline(&mut d.git_url).hint_text("https://github.com/alice/photocraft").desired_width(f32::INFINITY));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Branch or tag (empty: the default branch)").size(12.0).color(t.muted));
                    ui.add(egui::TextEdit::singleline(&mut d.branch).hint_text("main").desired_width(220.0));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Builds on this computer with your own git and Rust (cargo). The first build can take a while; updates rebuild only \
                             what changed. Building runs the project's build scripts, so only use sources you trust.",
                        )
                        .size(12.0)
                        .color(t.muted),
                    );
                }
            }
            if let Some(e) = &d.error {
                ui.add_space(6.0);
                ui.label(RichText::new(e).size(12.0).color(t.danger));
            }

            // the build log, while building and after a failed build
            if !self.build_log.is_empty() && (building || d.kind == Kind::Git) {
                ui.add_space(10.0);
                ui.label(RichText::new(if building { "Building…" } else { "Last build output" }).size(12.0).color(t.muted));
                egui::Frame::NONE.fill(t.bar).corner_radius(8).inner_margin(8).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                        for l in &self.build_log {
                            ui.label(RichText::new(l).monospace().size(10.5).color(t.muted));
                        }
                    });
                });
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let verb = match (d.kind, d.installed) {
                        (Kind::Git, true) => "Save and rebuild",
                        (Kind::Git, false) => "Save and build",
                        (_, true) => "Save and reinstall",
                        (_, false) => "Save and install",
                    };
                    if ui.add_enabled(idle, theme::primary(verb)).clicked() {
                        apply = Some(true);
                    }
                    if ui.add(theme::secondary("Save")).on_hover_text("Use this source for the next install or update").clicked() {
                        apply = Some(false);
                    }
                    if ui.add(theme::secondary("Close")).clicked() {
                        close = true;
                    }
                });
            });
        });

        if let Some(install) = apply {
            let source = d.source();
            match source.validate() {
                Err(e) => d.error = Some(e.to_string()),
                Ok(()) => {
                    d.error = None;
                    if source == Source::Release {
                        self.draft.app_sources.remove(&d.id);
                    } else {
                        self.draft.app_sources.insert(d.id.clone(), source);
                    }
                    self.save_draft();
                    if install {
                        self.run(format!("Install {}", d.name), Command::Install(d.id.clone()));
                    } else {
                        close = true;
                    }
                }
            }
        }
        if !close && !modal.should_close() {
            self.source_dialog = Some(d);
        }
    }
}
