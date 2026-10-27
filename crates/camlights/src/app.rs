use camlights_capture::VideoFormat;
use camlights_core::{ImageControls, OutputSize, Rotation};
use eframe::egui::{self, CentralPanel, Color32, ComboBox, Panel, ScrollArea, Slider, TextureHandle, TextureOptions};
use serde::{Deserialize, Serialize};

use crate::engine::{Command, Engine, Status};

const STORAGE_KEY: &str = "camlights";

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    controls: ImageControls,
    device_id: Option<String>,
    format: Option<VideoFormat>,
}

pub struct CamLightsApp {
    engine: Engine,
    controls: ImageControls,
    /// The camera to reopen on the next start, and its chosen format.
    device_id: Option<String>,
    format: Option<VideoFormat>,
    auto_opened: bool,
    texture: Option<TextureHandle>,
    frame_seq: u64,
}

impl CamLightsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let persisted: Persisted = cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY)).unwrap_or_default();
        let ctx = cc.egui_ctx.clone();
        let engine = Engine::start(persisted.controls.clone(), move || ctx.request_repaint());
        Self {
            engine,
            controls: persisted.controls,
            device_id: persisted.device_id,
            format: persisted.format,
            auto_opened: false,
            texture: None,
            frame_seq: 0,
        }
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

        let before = self.controls.clone();
        Panel::left("controls").resizable(false).exact_size(300.0).show(ui, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                self.camera_ui(ui, &status);
                ui.separator();
                self.image_ui(ui);
            });
        });
        if self.controls != before {
            *self.engine.shared().controls.lock().unwrap() = self.controls.clone();
        }

        CentralPanel::default().show(ui, |ui| self.preview_ui(ui));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let persisted = Persisted { controls: self.controls.clone(), device_id: self.device_id.clone(), format: self.format.clone() };
        eframe::set_value(storage, STORAGE_KEY, &persisted);
    }
}
