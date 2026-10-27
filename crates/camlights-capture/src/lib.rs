//! Camera sources. Every backend yields processed-ready RGBA [`Frame`]s through [`FrameSource`].

pub mod mf;

use anyhow::Result;
use camlights_core::Frame;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Backend {
    MediaFoundation,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub backend: Backend,
    pub name: String,
    /// Backend-specific stable identity (the device symbolic link for Media Foundation).
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// The device's native encoding, for display (for example `MJPG`, `YUY2`, `NV12`).
    pub encoding: String,
}

impl VideoFormat {
    pub fn fps(&self) -> f32 {
        if self.fps_den == 0 { 0.0 } else { self.fps_num as f32 / self.fps_den as f32 }
    }

    pub fn label(&self) -> String {
        let fps = format!("{:.2}", self.fps());
        let fps = fps.trim_end_matches('0').trim_end_matches('.');
        format!("{}×{} @ {fps} fps {}", self.width, self.height, self.encoding)
    }
}

/// An open camera. Sources are created and used on a single thread.
pub trait FrameSource {
    fn device(&self) -> &DeviceInfo;
    fn formats(&self) -> &[VideoFormat];
    fn format(&self) -> &VideoFormat;
    /// Blocks until the next frame is available.
    fn next_frame(&mut self) -> Result<Frame>;
}

/// Lists cameras from every backend. Media Foundation must be started on this thread ([`mf::MfRuntime`]).
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    mf::list_devices()
}

/// Opens `device`, using `format` when the device still offers it and a sensible default otherwise.
pub fn open(device: &DeviceInfo, format: Option<&VideoFormat>) -> Result<Box<dyn FrameSource>> {
    match device.backend {
        Backend::MediaFoundation => Ok(Box::new(mf::MfSource::open(device, format)?)),
    }
}
