//! Camera sources. Every backend yields processed-ready RGBA [`Frame`]s through [`FrameSource`].

pub mod canon;
pub mod mf;

use std::path::Path;
use std::rc::Rc;

use anyhow::{Result, anyhow};
use camlights_core::Frame;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Backend {
    MediaFoundation,
    Canon,
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
        if self.width == 0 {
            // Sources such as camera live view only learn their size from the first frame.
            return self.encoding.clone();
        }
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

/// Every camera backend, owned by the thread that captures. Media Foundation must be started on
/// that thread ([`mf::MfRuntime`]); EDSDK is optional and bound to it.
pub struct Backends {
    canon: Result<Rc<canon::Sdk>, String>,
}

impl Backends {
    /// Loads EDSDK from `edsdk_dir`, or from the default locations next to the executable.
    pub fn new(edsdk_dir: Option<&Path>) -> Self {
        Self { canon: canon::Sdk::load(edsdk_dir).map_err(|e| format!("{e:#}")) }
    }

    /// Media Foundation only, holding no SDK.
    pub fn unloaded() -> Self {
        Self { canon: Err("not loaded".to_owned()) }
    }

    /// Where EDSDK was loaded from, or why it is unavailable.
    pub fn canon_status(&self) -> Result<&Path, &str> {
        self.canon.as_ref().map(|sdk| sdk.path()).map_err(String::as_str)
    }

    pub fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let mut devices = mf::list_devices()?;
        if let Ok(sdk) = &self.canon {
            match canon::list_devices(sdk) {
                Ok(cameras) => devices.extend(cameras),
                Err(e) => log::warn!("listing Canon cameras failed: {e:#}"),
            }
        }
        Ok(devices)
    }

    /// Opens `device`, using `format` when the device still offers it and a sensible default otherwise.
    pub fn open(&self, device: &DeviceInfo, format: Option<&VideoFormat>) -> Result<Box<dyn FrameSource>> {
        match device.backend {
            Backend::MediaFoundation => Ok(Box::new(mf::MfSource::open(device, format)?)),
            Backend::Canon => {
                let sdk = self.canon.as_ref().map_err(|e| anyhow!("Canon EDSDK is unavailable: {e}"))?;
                Ok(Box::new(canon::CanonSource::open(sdk.clone(), device)?))
            }
        }
    }
}
