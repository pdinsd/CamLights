#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod engine;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("CamLights")
            .with_inner_size([1200.0, 720.0])
            .with_min_inner_size([800.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("CamLights", options, Box::new(|cc| Ok(Box::new(app::CamLightsApp::new(cc)))))
}
