//! The capture thread: owns Media Foundation, the open camera and frame processing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camlights_capture::{DeviceInfo, FrameSource, VideoFormat, mf::MfRuntime};
use camlights_core::{Frame, ImageControls, process};

pub enum Command {
    RefreshDevices,
    Open { device: DeviceInfo, format: Option<VideoFormat> },
    Close,
}

#[derive(Clone, Default)]
pub struct Status {
    pub devices: Vec<DeviceInfo>,
    /// Set once the first device listing has completed, even when it found nothing.
    pub devices_listed: bool,
    pub active: Option<DeviceInfo>,
    pub formats: Vec<VideoFormat>,
    pub format: Option<VideoFormat>,
    pub fps: f32,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Shared {
    pub status: Mutex<Status>,
    pub controls: Mutex<ImageControls>,
    frame: Mutex<Option<Arc<Frame>>>,
    frame_seq: AtomicU64,
}

impl Shared {
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
    pub fn start(controls: ImageControls, notify: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared { controls: Mutex::new(controls), ..Default::default() });
        let (commands, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || run(rx, &thread_shared, &notify))
            .expect("spawning the capture thread");
        Self { commands: Some(commands), shared, thread: Some(thread) }
    }

    pub fn send(&self, command: Command) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
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

fn refresh_devices(shared: &Shared) {
    let listed = camlights_capture::list_devices();
    shared.update_status(|s| {
        s.devices_listed = true;
        match listed {
            Ok(devices) => s.devices = devices,
            Err(e) => s.error = Some(format!("Listing cameras failed: {e:#}")),
        }
    });
}

fn run(commands: mpsc::Receiver<Command>, shared: &Shared, notify: &dyn Fn()) {
    let _runtime = match MfRuntime::start() {
        Ok(runtime) => runtime,
        Err(e) => {
            shared.update_status(|s| s.error = Some(format!("Media Foundation is unavailable: {e:#}")));
            notify();
            return;
        }
    };
    refresh_devices(shared);
    notify();

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
                Command::RefreshDevices => refresh_devices(shared),
                Command::Open { device, format } => {
                    // Release the current device before opening, in case it is the same one.
                    source = None;
                    match camlights_capture::open(&device, format.as_ref()) {
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
            }
            notify();
        }

        let Some(camera) = source.as_mut() else { continue };
        match camera.next_frame() {
            Ok(raw) => {
                let controls = shared.controls.lock().unwrap().clone();
                shared.publish(process(&raw, &controls));
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
