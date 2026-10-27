#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod engine;
mod vcam;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow};
use windows::core::w;

const TITLE: &str = "CamLights";

/// Returns false when another CamLights is already running, after bringing its window forward.
/// A second instance would fight the first for the camera and register a second virtual camera.
fn claim_single_instance() -> bool {
    // The handle is intentionally kept for the life of the process.
    let Ok(_mutex) = (unsafe { CreateMutexW(None, false, w!("Local\\CamLights.SingleInstance")) }) else { return true };
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return true;
    }
    if let Ok(window) = unsafe { FindWindowW(None, w!("CamLights")) } {
        unsafe {
            let _ = ShowWindow(window, SW_RESTORE);
            let _ = SetForegroundWindow(window);
        }
    }
    false
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if !claim_single_instance() {
        log::info!("CamLights is already running");
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([1200.0, 720.0])
            .with_min_inner_size([800.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(TITLE, options, Box::new(|cc| Ok(Box::new(app::CamLightsApp::new(cc)))))
}
