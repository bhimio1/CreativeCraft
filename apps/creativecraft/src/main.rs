//! CreativeCraft desktop hub.
//!
//! On Windows, release builds are GUI-subsystem programs (no console window); use `creativecraft-cli`
//! from a terminal for diagnostics.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

/// Reverse-DNS app id: Wayland app id and `.desktop` file name. Replace with your own domain.
const APP_ID: &str = "org.creativecraft.CreativeCraft";

struct App(creativecraft_ui_egui::CreativeCraftApp);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let session = match creativecraft_engine::Session::open() {
        Ok(s) => s,
        Err(e) => {
            log::error!("couldn't start CreativeCraft: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CreativeCraft")
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([760.0, 480.0])
            .with_app_id(APP_ID)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/brand/artcraft-mark.png")).unwrap_or_default()),
        ..Default::default()
    };
    let started = eframe::run_native(
        "CreativeCraft",
        options,
        Box::new(move |cc| Ok(Box::new(App(creativecraft_ui_egui::CreativeCraftApp::new(session, cc.egui_ctx.clone()))))),
    );
    match started {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("the window couldn't open: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
