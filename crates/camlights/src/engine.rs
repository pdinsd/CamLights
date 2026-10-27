//! The capture thread: owns Media Foundation, the open camera and frame processing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camlights_capture::{Backends, DeviceInfo, FrameSource, VideoFormat, mf::MfRuntime};
use camlights_core::{BackgroundCompositor, ChromaKey, Frame, ImageControls, Overlay, OverlayRenderer, process};

use crate::vcam::{self, VirtualCamera};

/// Settings the engine starts with.
pub struct Config {
    pub controls: ImageControls,
    pub chroma_key: ChromaKey,
    pub overlays: Vec<Overlay>,
    pub virtual_camera: bool,
    /// Folder holding the user's `EDSDK.dll`; `None` searches next to the executable.
    pub edsdk_dir: Option<PathBuf>,
}

pub enum Command {
    RefreshDevices,
    Open { device: DeviceInfo, format: Option<VideoFormat> },
    Close,
    /// Turns the virtual camera on or off.
    SetVirtualCamera(bool),
    /// Re-reads the registration after installing or uninstalling, and restarts the virtual camera.
    ReloadVirtualCamera,
    /// Reloads Canon EDSDK from another folder and lists cameras again.
    SetEdsdkDir(Option<PathBuf>),
}

#[derive(Clone, Default)]
pub struct VirtualCameraStatus {
    pub installed: bool,
    /// The installed copy is from a different build than this executable's.
    pub outdated: bool,
    pub running: bool,
    /// The resolution an application is currently reading.
    pub client: Option<(u32, u32)>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Status {
    pub devices: Vec<DeviceInfo>,
    /// Set once the first device listing has completed, even when it found nothing.
    pub devices_listed: bool,
    pub active: Option<DeviceInfo>,
    pub formats: Vec<VideoFormat>,
    pub format: Option<VideoFormat>,
    pub fps: f32,
    pub error: Option<String>,
    pub virtual_camera: VirtualCameraStatus,
    /// Where Canon EDSDK was loaded from, or why it is unavailable.
    pub canon: Result<PathBuf, String>,
    /// Why some overlays could not be drawn.
    pub overlay_errors: Vec<String>,
    /// Where each overlay was last drawn, normalised `[left, top, right, bottom]`.
    pub overlay_bounds: Vec<Option<[f32; 4]>>,
    /// Why the green screen background could not be applied.
    pub background_error: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            devices_listed: false,
            active: None,
            formats: Vec::new(),
            format: None,
            fps: 0.0,
            error: None,
            virtual_camera: VirtualCameraStatus::default(),
            canon: Err("not loaded yet".to_owned()),
            overlay_errors: Vec::new(),
            overlay_bounds: Vec::new(),
            background_error: None,
        }
    }
}

#[derive(Default)]
pub struct Shared {
    pub status: Mutex<Status>,
    pub controls: Mutex<ImageControls>,
    pub overlays: Mutex<Vec<Overlay>>,
    pub chroma_key: Mutex<ChromaKey>,
    /// While set, frames are published unkeyed so a key colour can be picked from the preview.
    pub picking_key: AtomicBool,
    frame: Mutex<Option<Arc<Frame>>>,
    frame_seq: AtomicU64,
}

impl Shared {
    /// The colour around normalised point `at` in the latest frame, averaged over a few pixels.
    pub fn sample_color(&self, at: [f32; 2]) -> Option<[u8; 3]> {
        let frame = self.frame.lock().unwrap().clone()?;
        let cx = (at[0].clamp(0.0, 1.0) * (frame.width - 1) as f32).round() as i64;
        let cy = (at[1].clamp(0.0, 1.0) * (frame.height - 1) as f32).round() as i64;
        let mut sum = [0u32; 3];
        let mut count = 0;
        for y in (cy - 2).max(0)..=(cy + 2).min(frame.height as i64 - 1) {
            for x in (cx - 2).max(0)..=(cx + 2).min(frame.width as i64 - 1) {
                let px = &frame.data[(y as usize * frame.width as usize + x as usize) * 4..][..3];
                for c in 0..3 {
                    sum[c] += px[c] as u32;
                }
                count += 1;
            }
        }
        (count > 0).then(|| sum.map(|s| (s / count) as u8))
    }

    /// Returns the latest processed frame if it is newer than `seen`.
    pub fn latest_frame(&self, seen: u64) -> Option<(u64, Arc<Frame>)> {
        let seq = self.frame_seq.load(Ordering::Acquire);
        if seq == seen {
            return None;
        }
        self.frame.lock().unwrap().clone().map(|frame| (seq, frame))
    }

    fn publish(&self, frame: Frame) {
        *self.frame.lock().unwrap() = Some(Arc::new(frame));
        self.frame_seq.fetch_add(1, Ordering::Release);
    }

    fn update_status(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap());
    }
}

pub struct Engine {
    commands: Option<mpsc::Sender<Command>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    /// Starts the capture thread. `notify` is called whenever status or the frame changes.
    pub fn start(config: Config, notify: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            controls: Mutex::new(config.controls.clone()),
            overlays: Mutex::new(config.overlays.clone()),
            chroma_key: Mutex::new(config.chroma_key.clone()),
            ..Default::default()
        });
        let (commands, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || run(rx, &thread_shared, config, &notify))
            .expect("spawning the capture thread");
        Self { commands: Some(commands), shared, thread: Some(thread) }
    }

    pub fn send(&self, command: Command) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }

    /// A handle for sending commands from other threads.
    pub fn sender(&self) -> Option<mpsc::Sender<Command>> {
        self.commands.clone()
    }

    pub fn shared(&self) -> &Shared {
        &self.shared
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Closing the channel stops the thread after its current frame.
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct FpsCounter {
    since: Instant,
    frames: u32,
}

impl FpsCounter {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0 }
    }

    /// Counts a frame and returns the rate once per second.
    fn tick(&mut self) -> Option<f32> {
        self.frames += 1;
        let elapsed = self.since.elapsed();
        if elapsed < Duration::from_secs(1) {
            return None;
        }
        let fps = self.frames as f32 / elapsed.as_secs_f32();
        *self = Self::new();
        Some(fps)
    }
}

/// The font for text overlays: Segoe UI, falling back to Arial.
fn system_font() -> Option<Vec<u8>> {
    let fonts = std::path::Path::new(&std::env::var_os("SystemRoot")?).join("Fonts");
    ["segoeui.ttf", "arial.ttf"].iter().find_map(|name| std::fs::read(fonts.join(name)).ok())
}

fn load_backends(edsdk_dir: Option<&std::path::Path>, shared: &Shared) -> Backends {
    let backends = Backends::new(edsdk_dir);
    let canon = backends.canon_status().map(|p| p.to_path_buf()).map_err(str::to_owned);
    shared.update_status(|s| s.canon = canon);
    backends
}

fn refresh_devices(backends: &Backends, shared: &Shared) {
    let listed = backends.list_devices();
    shared.update_status(|s| {
        s.devices_listed = true;
        match listed {
            // Capturing our own virtual camera would feed its output back into itself.
            Ok(devices) => s.devices = devices.into_iter().filter(|d| d.name != camlights_vcam_shared::FRIENDLY_NAME).collect(),
            Err(e) => s.error = Some(format!("Listing cameras failed: {e:#}")),
        }
    });
}

/// Starts or stops the virtual camera to match `enabled` and the current registration.
fn sync_virtual_camera(camera: &mut Option<VirtualCamera>, enabled: bool, shared: &Shared) {
    let registered = vcam::registered_dll();
    let installed = registered.is_some();
    let outdated = registered.as_deref().is_some_and(vcam::registered_is_outdated);
    let mut error = None;
    if !(enabled && installed) {
        *camera = None;
    } else if camera.is_none() {
        match VirtualCamera::start() {
            Ok(started) => *camera = Some(started),
            Err(e) => error = Some(format!("{e:#}")),
        }
    }
    let running = camera.is_some();
    shared.update_status(|s| s.virtual_camera = VirtualCameraStatus { installed, outdated, running, client: None, error });
}

fn run(commands: mpsc::Receiver<Command>, shared: &Shared, config: Config, notify: &dyn Fn()) {
    let _runtime = match MfRuntime::start() {
        Ok(runtime) => runtime,
        Err(e) => {
            shared.update_status(|s| s.error = Some(format!("Media Foundation is unavailable: {e:#}")));
            notify();
            return;
        }
    };
    let mut backends = load_backends(config.edsdk_dir.as_deref(), shared);
    refresh_devices(&backends, shared);
    let mut virtual_camera_enabled = config.virtual_camera;
    let mut virtual_camera: Option<VirtualCamera> = None;
    sync_virtual_camera(&mut virtual_camera, virtual_camera_enabled, shared);
    notify();

    let mut overlay_renderer = OverlayRenderer::new(system_font());
    let mut background = BackgroundCompositor::default();

    // Declared after `backends` so an open camera is dropped before the SDK that serves it.
    let mut source: Option<Box<dyn FrameSource>> = None;
    let mut fps = FpsCounter::new();
    loop {
        // Block for commands while idle; otherwise only drain what is pending between frames.
        let first = if source.is_some() {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        };
        for command in first.into_iter().chain(commands.try_iter()) {
            match command {
                Command::RefreshDevices => refresh_devices(&backends, shared),
                Command::Open { device, format } => {
                    // Release the current device before opening, in case it is the same one.
                    source = None;
                    match backends.open(&device, format.as_ref()) {
                        Ok(opened) => {
                            shared.update_status(|s| {
                                s.active = Some(opened.device().clone());
                                s.formats = opened.formats().to_vec();
                                s.format = Some(opened.format().clone());
                                s.fps = 0.0;
                                s.error = None;
                            });
                            fps = FpsCounter::new();
                            source = Some(opened);
                        }
                        Err(e) => shared.update_status(|s| {
                            s.active = None;
                            s.error = Some(format!("{e:#}"));
                        }),
                    }
                }
                Command::Close => {
                    source = None;
                    shared.update_status(|s| s.active = None);
                }
                Command::SetVirtualCamera(enabled) => {
                    virtual_camera_enabled = enabled;
                    sync_virtual_camera(&mut virtual_camera, virtual_camera_enabled, shared);
                }
                Command::ReloadVirtualCamera => {
                    virtual_camera = None;
                    sync_virtual_camera(&mut virtual_camera, virtual_camera_enabled, shared);
                }
                Command::SetEdsdkDir(dir) => {
                    // A Canon camera holds the old SDK; close it before that SDK is terminated.
                    if source.as_ref().is_some_and(|s| s.device().backend == camlights_capture::Backend::Canon) {
                        source = None;
                        shared.update_status(|s| s.active = None);
                    }
                    // Terminate the old SDK before initialising again: both may be the same loaded DLL.
                    drop(std::mem::replace(&mut backends, Backends::unloaded()));
                    backends = load_backends(dir.as_deref(), shared);
                    refresh_devices(&backends, shared);
                }
            }
            notify();
        }

        let Some(camera) = source.as_mut() else { continue };
        match camera.next_frame() {
            Ok(raw) => {
                let controls = shared.controls.lock().unwrap().clone();
                let mut processed = process(&raw, &controls);
                if !shared.picking_key.load(Ordering::Relaxed) {
                    let chroma_key = shared.chroma_key.lock().unwrap().clone();
                    let background_error = background.apply(&mut processed, &chroma_key);
                    if shared.status.lock().unwrap().background_error != background_error {
                        shared.update_status(|s| s.background_error = background_error);
                    }
                }
                let overlays = shared.overlays.lock().unwrap().clone();
                let overlay_errors = overlay_renderer.apply(&mut processed, &overlays);
                let overlay_bounds = overlay_renderer.bounds();
                let changed = {
                    let status = shared.status.lock().unwrap();
                    status.overlay_errors != overlay_errors || status.overlay_bounds != overlay_bounds
                };
                if changed {
                    shared.update_status(|s| {
                        s.overlay_errors = overlay_errors;
                        s.overlay_bounds = overlay_bounds.to_vec();
                    });
                }
                if let Some(camera) = virtual_camera.as_mut() {
                    let client = camera.send(&processed);
                    if shared.status.lock().unwrap().virtual_camera.client != client {
                        shared.update_status(|s| s.virtual_camera.client = client);
                    }
                }
                shared.publish(processed);
                if let Some(rate) = fps.tick() {
                    shared.update_status(|s| s.fps = rate);
                }
            }
            Err(e) => {
                source = None;
                shared.update_status(|s| {
                    s.active = None;
                    s.error = Some(format!("{e:#}"));
                });
            }
        }
        notify();
    }
}
