//! Atelier desktop hub.
//!
//! On Windows, release builds are GUI-subsystem programs (no console window); use `atelier-cli`
//! from a terminal for diagnostics.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

/// Reverse-DNS app id: Wayland app id and `.desktop` file name. Replace with your own domain.
const APP_ID: &str = "org.atelier.Atelier";

struct App(atelier_ui_egui::AtelierApp);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let session = match atelier_engine::Session::open() {
        Ok(s) => s,
        Err(e) => {
            log::error!("couldn't start Atelier: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Atelier")
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([760.0, 480.0])
            .with_app_id(APP_ID),
        ..Default::default()
    };
    let started = eframe::run_native("Atelier", options, Box::new(move |cc| Ok(Box::new(App(atelier_ui_egui::AtelierApp::new(session, cc.egui_ctx.clone()))))));
    match started {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("the window couldn't open: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
