use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use camlights_capture::VideoFormat;
use camlights_core::{ImageControls, OutputSize, Overlay, OverlayContent, Rotation};
use camlights_vcam_shared::FRIENDLY_NAME;
use eframe::egui::{self, Button, CentralPanel, Color32, ComboBox, Panel, ScrollArea, Slider, TextureHandle, TextureOptions};
use serde::{Deserialize, Serialize};

use crate::engine::{Command, Config, Engine, Status};
use crate::vcam;

const STORAGE_KEY: &str = "camlights";

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    controls: ImageControls,
    overlays: Vec<Overlay>,
    device_id: Option<String>,
    format: Option<VideoFormat>,
    virtual_camera: bool,
    edsdk_dir: Option<PathBuf>,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            controls: ImageControls::default(),
            overlays: Vec::new(),
            device_id: None,
            format: None,
            virtual_camera: true,
            edsdk_dir: None,
        }
    }
}

/// Progress of an elevated install or uninstall of the virtual camera.
#[derive(Default)]
enum Registration {
    #[default]
    Idle,
    Running,
    Failed(String),
}

pub struct CamLightsApp {
    engine: Engine,
    controls: ImageControls,
    overlays: Vec<Overlay>,
    /// The camera to reopen on the next start, and its chosen format.
    device_id: Option<String>,
    format: Option<VideoFormat>,
    virtual_camera: bool,
    edsdk_dir: Option<PathBuf>,
    registration: Arc<Mutex<Registration>>,
    auto_opened: bool,
    texture: Option<TextureHandle>,
    frame_seq: u64,
}

impl CamLightsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let persisted: Persisted = cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY)).unwrap_or_default();
        let ctx = cc.egui_ctx.clone();
        let config = Config {
            controls: persisted.controls.clone(),
            overlays: persisted.overlays.clone(),
            virtual_camera: persisted.virtual_camera,
            edsdk_dir: persisted.edsdk_dir.clone(),
        };
        let engine = Engine::start(config, move || ctx.request_repaint());
        Self {
            engine,
            controls: persisted.controls,
            overlays: persisted.overlays,
            device_id: persisted.device_id,
            format: persisted.format,
            virtual_camera: persisted.virtual_camera,
            edsdk_dir: persisted.edsdk_dir,
            registration: Arc::default(),
            auto_opened: false,
            texture: None,
            frame_seq: 0,
        }
    }

    /// Runs the elevated installer on a helper thread so the window stays responsive during the UAC prompt.
    fn change_registration(&self, unregister: bool, ctx: &egui::Context) {
        let Some(commands) = self.engine.sender() else { return };
        let registration = self.registration.clone();
        let ctx = ctx.clone();
        *registration.lock().unwrap() = Registration::Running;
        std::thread::spawn(move || {
            let outcome = vcam::change_installation(unregister);
            *registration.lock().unwrap() = match outcome {
                Ok(()) => Registration::Idle,
                Err(e) => Registration::Failed(format!("{e:#}")),
            };
            let _ = commands.send(Command::ReloadVirtualCamera);
            ctx.request_repaint();
        });
    }

    /// Opens the remembered camera (or the first one) once the device list arrives.
    fn auto_open(&mut self, status: &Status) {
        if self.auto_opened || !status.devices_listed {
            return;
        }
        self.auto_opened = true;
        let remembered = status.devices.iter().find(|d| Some(&d.id) == self.device_id.as_ref());
        if let Some(device) = remembered.or(status.devices.first()) {
            let format = if remembered.is_some() { self.format.clone() } else { None };
            self.engine.send(Command::Open { device: device.clone(), format });
        }
    }

    fn update_texture(&mut self, ctx: &egui::Context) {
        let Some((seq, frame)) = self.engine.shared().latest_frame(self.frame_seq) else { return };
        self.frame_seq = seq;
        let image = egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.data);
        match &mut self.texture {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR)),
        }
    }

    fn camera_ui(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("Camera");
        ui.horizontal(|ui| {
            let selected = status.active.as_ref().map_or("None", |d| d.name.as_str());
            ComboBox::from_id_salt("device").width(220.0).selected_text(selected).show_ui(ui, |ui| {
                for device in &status.devices {
                    let active = status.active.as_ref() == Some(device);
                    if ui.selectable_label(active, &device.name).clicked() && !active {
                        self.device_id = Some(device.id.clone());
                        self.format = None;
                        self.engine.send(Command::Open { device: device.clone(), format: None });
                    }
                }
                if status.active.is_some() && ui.selectable_label(false, "None").clicked() {
                    self.engine.send(Command::Close);
                }
            });
            if ui.button("⟳").on_hover_text("Refresh camera list").clicked() {
                self.engine.send(Command::RefreshDevices);
            }
        });

        if let (Some(device), Some(current)) = (&status.active, &status.format) {
            ComboBox::from_id_salt("format").width(260.0).selected_text(current.label()).show_ui(ui, |ui| {
                for format in &status.formats {
                    if ui.selectable_label(format == current, format.label()).clicked() && format != current {
                        self.format = Some(format.clone());
                        self.engine.send(Command::Open { device: device.clone(), format: Some(format.clone()) });
                    }
                }
            });
            ui.label(format!("{:.1} fps", status.fps));
        } else if status.devices_listed && status.devices.is_empty() {
            ui.label("No cameras found.");
        }
        if let Some(error) = &status.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }

        ui.collapsing("Canon EOS (EDSDK)", |ui| {
            match &status.canon {
                Ok(path) => ui.label(format!("Loaded from {}", path.display())),
                Err(reason) => ui.colored_label(Color32::LIGHT_YELLOW, reason),
            };
            ui.label("Canon cameras need your own copy of Canon's EDSDK. Choose the folder containing EDSDK.dll.");
            ui.horizontal(|ui| {
                if ui.button("Choose folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_title("Folder containing EDSDK.dll").pick_folder()
                {
                    self.edsdk_dir = Some(dir.clone());
                    self.engine.send(Command::SetEdsdkDir(Some(dir)));
                }
                if self.edsdk_dir.is_some() && ui.button("Use default").clicked() {
                    self.edsdk_dir = None;
                    self.engine.send(Command::SetEdsdkDir(None));
                }
            });
        });
    }

    fn virtual_camera_ui(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("Virtual camera");
        let state = &status.virtual_camera;
        let busy = matches!(*self.registration.lock().unwrap(), Registration::Running);
        if state.installed {
            if ui.checkbox(&mut self.virtual_camera, format!("Publish as “{FRIENDLY_NAME}”")).changed() {
                self.engine.send(Command::SetVirtualCamera(self.virtual_camera));
            }
            match (state.running, state.client) {
                (true, Some((w, h))) => ui.label(format!("An app is watching at {w}×{h}.")),
                (true, None) => ui.label(format!("Ready: choose {FRIENDLY_NAME} in your video app.")),
                (false, _) => ui.label("Off."),
            };
            ui.horizontal(|ui| {
                if state.outdated
                    && ui
                        .add_enabled(!busy, Button::new("Update…"))
                        .on_hover_text("Install the virtual camera from this CamLights build (requires administrator approval)")
                        .clicked()
                {
                    self.change_registration(false, ui.ctx());
                }
                if ui.add_enabled(!busy, Button::new("Uninstall…")).on_hover_text("Requires administrator approval").clicked() {
                    self.change_registration(true, ui.ctx());
                }
            });
            if state.outdated {
                ui.colored_label(Color32::LIGHT_YELLOW, "The installed virtual camera is from a different CamLights build.");
            }
        } else {
            ui.label("Install the virtual camera so Zoom, Teams, OBS and browsers can use CamLights.");
            if ui.add_enabled(!busy, Button::new("Install…")).on_hover_text("Requires administrator approval").clicked() {
                self.change_registration(false, ui.ctx());
            }
        }
        if busy {
            ui.label("Waiting for administrator approval…");
        }
        if let Registration::Failed(error) = &*self.registration.lock().unwrap() {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        if let Some(error) = &state.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
    }

    fn image_ui(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.controls;
        ui.heading("Output");
        ComboBox::from_id_salt("output").width(260.0).selected_text(c.output.label()).show_ui(ui, |ui| {
            for preset in OutputSize::PRESETS {
                ui.selectable_value(&mut c.output, preset, preset.label());
            }
        });

        ui.add_space(8.0);
        ui.heading("Image");
        ui.horizontal(|ui| {
            ui.checkbox(&mut c.mirror, "Mirror");
            ui.checkbox(&mut c.flip_vertical, "Flip vertically");
        });
        ui.horizontal(|ui| {
            ui.label("Rotate");
            for rotation in Rotation::ALL {
                ui.selectable_value(&mut c.rotation, rotation, rotation.label());
            }
        });
        ui.add(Slider::new(&mut c.zoom, 1.0..=4.0).text("Zoom"));
        ui.add_enabled_ui(c.zoom > 1.0, |ui| {
            ui.add(Slider::new(&mut c.pan[0], -1.0..=1.0).text("Pan X"));
            ui.add(Slider::new(&mut c.pan[1], -1.0..=1.0).text("Pan Y"));
        });
        ui.add(Slider::new(&mut c.brightness, -1.0..=1.0).text("Brightness"));
        ui.add(Slider::new(&mut c.contrast, 0.0..=2.0).text("Contrast"));
        ui.add(Slider::new(&mut c.saturation, 0.0..=2.0).text("Saturation"));
        if ui.button("Reset image").clicked() {
            *c = ImageControls { output: c.output, ..Default::default() };
        }
    }

    fn overlays_ui(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("Overlays");
        ui.horizontal(|ui| {
            if ui.button("Add text").clicked() {
                self.overlays.push(Overlay::text("Your name"));
            }
            if ui.button("Add image…").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg"]).pick_file()
            {
                self.overlays.push(Overlay::image(path));
            }
        });

        let mut remove = None;
        for (index, overlay) in self.overlays.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut overlay.enabled, "");
                        match &mut overlay.content {
                            OverlayContent::Text { text, .. } => {
                                ui.add(egui::TextEdit::multiline(text).desired_rows(1).desired_width(190.0));
                            }
                            OverlayContent::Image { path } => {
                                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                                ui.label(name).on_hover_text(path.display().to_string());
                            }
                        }
                        if ui.small_button("✕").on_hover_text("Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                    if let OverlayContent::Text { color, background, .. } = &mut overlay.content {
                        ui.horizontal(|ui| {
                            ui.label("Text");
                            ui.color_edit_button_srgba_unmultiplied(color);
                            let mut boxed = background.is_some();
                            if ui.checkbox(&mut boxed, "Background").changed() {
                                *background = boxed.then_some([0, 0, 0, 160]);
                            }
                            if let Some(back) = background {
                                ui.color_edit_button_srgba_unmultiplied(back);
                            }
                        });
                    }
                    let size_range = if matches!(overlay.content, OverlayContent::Text { .. }) { 0.02..=0.3 } else { 0.03..=1.0 };
                    ui.add(Slider::new(&mut overlay.size, size_range).text("Size"));
                    ui.add(Slider::new(&mut overlay.position[0], 0.0..=1.0).text("X"));
                    ui.add(Slider::new(&mut overlay.position[1], 0.0..=1.0).text("Y"));
                    ui.add(Slider::new(&mut overlay.opacity, 0.0..=1.0).text("Opacity"));
                });
            });
        }
        if let Some(index) = remove {
            self.overlays.remove(index);
        }
        for error in &status.overlay_errors {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
    }

    fn preview_ui(&self, ui: &mut egui::Ui) {
        match &self.texture {
            Some(texture) => {
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new(egui::load::SizedTexture::from_handle(texture)).shrink_to_fit());
                });
            }
            None => {
                ui.centered_and_justified(|ui| ui.label("No camera"));
            }
        }
    }
}

impl eframe::App for CamLightsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let status = self.engine.shared().status.lock().unwrap().clone();
        self.auto_open(&status);
        self.update_texture(ui.ctx());

        let before = (self.controls.clone(), self.overlays.clone());
        Panel::left("controls").resizable(false).exact_size(300.0).show(ui, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                self.camera_ui(ui, &status);
                ui.separator();
                self.virtual_camera_ui(ui, &status);
                ui.separator();
                self.image_ui(ui);
                ui.separator();
                self.overlays_ui(ui, &status);
            });
        });
        if self.controls != before.0 {
            *self.engine.shared().controls.lock().unwrap() = self.controls.clone();
        }
        if self.overlays != before.1 {
            *self.engine.shared().overlays.lock().unwrap() = self.overlays.clone();
        }

        CentralPanel::default().show(ui, |ui| self.preview_ui(ui));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let persisted = Persisted {
            controls: self.controls.clone(),
            overlays: self.overlays.clone(),
            device_id: self.device_id.clone(),
            format: self.format.clone(),
            virtual_camera: self.virtual_camera,
            edsdk_dir: self.edsdk_dir.clone(),
        };
        eframe::set_value(storage, STORAGE_KEY, &persisted);
    }
}
