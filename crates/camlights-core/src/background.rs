//! Green screen: chroma keying with a replacement background.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{Frame, fit};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Replacement {
    Color([u8; 3]),
    Image(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChromaKey {
    pub enabled: bool,
    pub key: [u8; 3],
    /// Pixels whose chroma is within this distance of the key are fully removed. Distances are
    /// relative to the key's own chroma, so neutral greys sit at 1.0 whatever the key.
    pub similarity: f32,
    /// Width of the soft edge beyond `similarity`.
    pub smoothness: f32,
    /// 0.0..=1.0: how strongly the key colour is removed from the remaining foreground.
    pub spill: f32,
    pub replacement: Replacement,
}

impl Default for ChromaKey {
    fn default() -> Self {
        Self {
            enabled: false,
            key: [0, 177, 64],
            similarity: 0.45,
            smoothness: 0.15,
            spill: 0.5,
            replacement: Replacement::Color([32, 32, 40]),
        }
    }
}

/// BT.709 chroma (Cb, Cr) of an sRGB colour, each roughly in -0.5..=0.5.
#[inline]
fn chroma(r: f32, g: f32, b: f32) -> (f32, f32) {
    (-0.1146 * r - 0.3854 * g + 0.5 * b, 0.5 * r - 0.4542 * g - 0.0458 * b)
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Keys `frame` in place against `background`, which must have the same size.
pub fn chroma_key(frame: &mut Frame, key: &ChromaKey, background: &Frame) {
    assert_eq!((frame.width, frame.height), (background.width, background.height), "background must match the frame");
    let to_unit = |c: u8| c as f32 / 255.0;
    let (key_cb, key_cr) = chroma(to_unit(key.key[0]), to_unit(key.key[1]), to_unit(key.key[2]));
    // A near-grey key has no chroma to measure against; fall back to absolute distances.
    let key_strength = (key_cb * key_cb + key_cr * key_cr).sqrt().max(0.05);
    // Spill suppression acts on the key's dominant channel (green for a green screen).
    let dominant = (0..3).max_by_key(|&c| key.key[c]).unwrap_or(1);
    let (low, high) = (key.similarity, key.similarity + key.smoothness);
    let spill = key.spill.clamp(0.0, 1.0);

    frame.data.par_chunks_exact_mut(4).zip(background.data.par_chunks_exact(4)).for_each(|(px, bg)| {
        let (r, g, b) = (to_unit(px[0]), to_unit(px[1]), to_unit(px[2]));
        let (cb, cr) = chroma(r, g, b);
        let distance = ((cb - key_cb).powi(2) + (cr - key_cr).powi(2)).sqrt() / key_strength;
        let alpha = smoothstep(low, high, distance);
        if alpha <= 0.0 {
            px[..3].copy_from_slice(&bg[..3]);
            return;
        }
        if spill > 0.0 {
            let others = (0..3).filter(|&c| c != dominant).map(|c| px[c]).max().unwrap_or(0);
            if px[dominant] > others {
                px[dominant] -= ((px[dominant] - others) as f32 * spill).round() as u8;
            }
        }
        if alpha < 1.0 {
            for c in 0..3 {
                px[c] = (px[c] as f32 * alpha + bg[c] as f32 * (1.0 - alpha)).round() as u8;
            }
        }
    });
}

/// Applies [`ChromaKey`] settings, keeping the replacement background scaled to the output.
#[derive(Default)]
pub struct BackgroundCompositor {
    /// Decoded replacement image and its path.
    source: Option<(PathBuf, Result<Frame, String>)>,
    /// The replacement prepared for the current output size.
    prepared: Option<(Replacement, (u32, u32), Frame)>,
}

impl BackgroundCompositor {
    /// Keys `frame` when enabled. Returns why the background could not be applied, if it could not.
    pub fn apply(&mut self, frame: &mut Frame, key: &ChromaKey) -> Option<String> {
        if !key.enabled {
            return None;
        }
        let size = (frame.width, frame.height);
        let ready = self.prepared.as_ref().is_some_and(|(r, s, _)| *r == key.replacement && *s == size);
        if !ready {
            let background = match &key.replacement {
                Replacement::Color(rgb) => {
                    let mut solid = Frame::new(size.0, size.1);
                    for px in solid.data.chunks_exact_mut(4) {
                        px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                    }
                    solid
                }
                Replacement::Image(path) => match self.load(path) {
                    Ok(image) => fit(image, size.0, size.1),
                    Err(e) => return Some(e),
                },
            };
            self.prepared = Some((key.replacement.clone(), size, background));
        }
        let (_, _, background) = self.prepared.as_ref().expect("prepared above");
        chroma_key(frame, key, background);
        None
    }

    fn load(&mut self, path: &Path) -> Result<&Frame, String> {
        if self.source.as_ref().is_none_or(|(p, _)| p != path) {
            let decoded = image::open(path).map_err(|e| format!("{}: {e}", path.display())).map(|image| {
                let image = image.to_rgba8();
                let (width, height) = image.dimensions();
                Frame { width, height, data: image.into_raw() }
            });
            self.source = Some((path.to_path_buf(), decoded));
        }
        self.source.as_ref().expect("loaded above").1.as_ref().map_err(Clone::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgb: [u8; 3]) -> Frame {
        Frame { width: 1, height: 1, data: vec![rgb[0], rgb[1], rgb[2], 255] }
    }

    fn keyed(rgb: [u8; 3]) -> [u8; 3] {
        let mut frame = pixel(rgb);
        let key = ChromaKey { enabled: true, ..Default::default() };
        chroma_key(&mut frame, &key, &pixel([10, 20, 30]));
        [frame.data[0], frame.data[1], frame.data[2]]
    }

    #[test]
    fn green_screen_is_replaced() {
        assert_eq!(keyed([0, 177, 64]), [10, 20, 30]);
        // Darker, less saturated green from uneven lighting is still keyed.
        assert_eq!(keyed([30, 140, 60]), [10, 20, 30]);
    }

    #[test]
    fn skin_and_neutral_tones_are_kept() {
        assert_eq!(keyed([230, 180, 150]), [230, 180, 150]);
        assert_eq!(keyed([128, 128, 128]), [128, 128, 128]);
    }

    #[test]
    fn spill_pulls_green_down_towards_the_other_channels() {
        // A green-tinged foreground pixel, as on hair or shoulders lit by the screen.
        let [r, g, b] = keyed([150, 200, 140]);
        assert_eq!((r, b), (150, 140));
        assert!(g < 200 && g >= 150, "green {g} should be reduced but not below red");
    }

    #[test]
    fn disabled_key_leaves_the_frame_alone() {
        let mut frame = pixel([0, 177, 64]);
        assert!(BackgroundCompositor::default().apply(&mut frame, &ChromaKey::default()).is_none());
        assert_eq!(&frame.data[..3], &[0, 177, 64]);
    }

    #[test]
    fn colour_replacement_is_prepared_for_the_output_size() {
        let mut frame = Frame::new(4, 2);
        for px in frame.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 177, 64, 255]);
        }
        let key = ChromaKey { enabled: true, replacement: Replacement::Color([1, 2, 3]), ..Default::default() };
        assert!(BackgroundCompositor::default().apply(&mut frame, &key).is_none());
        assert!(frame.data.chunks_exact(4).all(|px| px[..3] == [1, 2, 3]));
    }
}
