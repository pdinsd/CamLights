//! Frame representation and image processing shared by every CamLights crate.

mod controls;
mod frame;

pub use controls::{ImageControls, OutputSize, Rotation, process};
pub use frame::Frame;
