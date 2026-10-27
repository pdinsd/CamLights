//! Frame representation and image processing shared by every CamLights crate.

mod controls;
mod convert;
mod frame;
mod overlay;

pub use controls::{ImageControls, OutputSize, Rotation, process};
pub use convert::{fit, rgba_to_nv12};
pub use overlay::{Overlay, OverlayContent, OverlayRenderer};
pub use frame::Frame;
