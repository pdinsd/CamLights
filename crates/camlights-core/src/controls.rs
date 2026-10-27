use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::Frame;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    pub const ALL: [Rotation; 4] = [Rotation::None, Rotation::Cw90, Rotation::Cw180, Rotation::Cw270];

    pub fn label(self) -> &'static str {
        match self {
            Rotation::None => "0°",
            Rotation::Cw90 => "90°",
            Rotation::Cw180 => "180°",
            Rotation::Cw270 => "270°",
        }
    }

    fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Cw90 | Rotation::Cw270)
    }
}

/// Resolution of the processed output. The source is scaled to cover it and centre-cropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSize {
    /// Follow the (rotated) source resolution.
    Source,
    Fixed { width: u32, height: u32 },
}

impl OutputSize {
    pub const PRESETS: [OutputSize; 4] = [
        OutputSize::Source,
        OutputSize::Fixed { width: 640, height: 480 },
        OutputSize::Fixed { width: 1280, height: 720 },
        OutputSize::Fixed { width: 1920, height: 1080 },
    ];

    pub fn label(self) -> String {
        match self {
            OutputSize::Source => "Same as source".to_owned(),
            OutputSize::Fixed { width, height } => format!("{width}×{height}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageControls {
    pub output: OutputSize,
    pub mirror: bool,
    pub flip_vertical: bool,
    pub rotation: Rotation,
    /// 1.0 shows the whole (cropped-to-fit) image; larger values zoom in.
    pub zoom: f32,
    /// Pan within the zoomed image, each axis in -1.0..=1.0.
    pub pan: [f32; 2],
    /// -1.0..=1.0, added as a fraction of full scale.
    pub brightness: f32,
    /// 0.0..=2.0, 1.0 is unchanged.
    pub contrast: f32,
    /// 0.0..=2.0, 1.0 is unchanged.
    pub saturation: f32,
}

impl Default for ImageControls {
    fn default() -> Self {
        Self {
            output: OutputSize::Fixed { width: 1280, height: 720 },
            mirror: false,
            flip_vertical: false,
            rotation: Rotation::None,
            zoom: 1.0,
            pan: [0.0, 0.0],
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
        }
    }
}

/// Maps output pixel centres to source pixel coordinates: `src = m * (x, y) + t`.
#[derive(Clone, Copy, Debug)]
struct Affine {
    m: [[f32; 2]; 2],
    t: [f32; 2],
}

impl Affine {
    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.m[0][0] * x + self.m[0][1] * y + self.t[0],
            self.m[1][0] * x + self.m[1][1] * y + self.t[1],
        )
    }

    fn is_identity(&self) -> bool {
        self.m == [[1.0, 0.0], [0.0, 1.0]] && self.t == [0.0, 0.0]
    }
}

fn output_dims(src: &Frame, controls: &ImageControls) -> (u32, u32) {
    match controls.output {
        OutputSize::Fixed { width, height } => (width.max(1), height.max(1)),
        OutputSize::Source if controls.rotation.swaps_axes() => (src.height, src.width),
        OutputSize::Source => (src.width, src.height),
    }
}

fn mapping(src: &Frame, controls: &ImageControls, ow: u32, oh: u32) -> Affine {
    let (sw, sh) = (src.width as f32, src.height as f32);
    let (rw, rh) = if controls.rotation.swaps_axes() { (sh, sw) } else { (sw, sh) };
    let (ow_f, oh_f) = (ow as f32, oh as f32);

    // Normalised crop window in rotated-source space: cover the output, then zoom and pan.
    let (mut cw, mut ch) = if ow_f / oh_f > rw / rh { (1.0, (rw / rh) / (ow_f / oh_f)) } else { ((ow_f / oh_f) / (rw / rh), 1.0) };
    let zoom = controls.zoom.max(1.0);
    cw /= zoom;
    ch /= zoom;
    let cx = 0.5 + controls.pan[0].clamp(-1.0, 1.0) * (1.0 - cw) / 2.0;
    let cy = 0.5 + controls.pan[1].clamp(-1.0, 1.0) * (1.0 - ch) / 2.0;

    // Output pixel centre (x + 0.5) -> display u in [0, 1], with optional mirroring.
    let (mut ux_scale, mut ux_off) = (1.0 / ow_f, 0.5 / ow_f);
    if controls.mirror {
        ux_scale = -ux_scale;
        ux_off = 1.0 - ux_off;
    }
    let (mut vy_scale, mut vy_off) = (1.0 / oh_f, 0.5 / oh_f);
    if controls.flip_vertical {
        vy_scale = -vy_scale;
        vy_off = 1.0 - vy_off;
    }
    // r = crop_origin + d * crop_size
    let (rx_s, rx_o) = (ux_scale * cw, (cx - cw / 2.0) + ux_off * cw);
    let (ry_s, ry_o) = (vy_scale * ch, (cy - ch / 2.0) + vy_off * ch);

    // Rotated-normalised (r_u, r_v) -> source-normalised (s_u, s_v) for a clockwise display rotation.
    // Each entry: s_u = a*r_u + b*r_v + c ; s_v = d*r_u + e*r_v + f
    let (a, b, c, d, e, f) = match controls.rotation {
        Rotation::None => (1.0, 0.0, 0.0, 0.0, 1.0, 0.0),
        Rotation::Cw90 => (0.0, 1.0, 0.0, -1.0, 0.0, 1.0),
        Rotation::Cw180 => (-1.0, 0.0, 1.0, 0.0, -1.0, 1.0),
        Rotation::Cw270 => (0.0, -1.0, 1.0, 1.0, 0.0, 0.0),
    };

    // Compose: r_u = rx_s * x + rx_o, r_v = ry_s * y + ry_o; source pixel = s * size - 0.5
    Affine {
        m: [[a * rx_s * sw, b * ry_s * sw], [d * rx_s * sh, e * ry_s * sh]],
        t: [(a * rx_o + b * ry_o + c) * sw - 0.5, (d * rx_o + e * ry_o + f) * sh - 0.5],
    }
}

fn tone_lut(controls: &ImageControls) -> Option<[u8; 256]> {
    if controls.brightness == 0.0 && controls.contrast == 1.0 {
        return None;
    }
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let x = (i as f32 - 128.0) * controls.contrast + 128.0 + controls.brightness * 255.0;
        *v = x.round().clamp(0.0, 255.0) as u8;
    }
    Some(lut)
}

#[inline]
fn sample_bilinear(src: &Frame, x: f32, y: f32) -> [u8; 4] {
    let max_x = src.width as f32 - 1.0;
    let max_y = src.height as f32 - 1.0;
    let x = x.clamp(0.0, max_x);
    let y = y.clamp(0.0, max_y);
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (x0, y0) = (x0 as usize, y0 as usize);
    let x1 = (x0 + 1).min(src.width as usize - 1);
    let y1 = (y0 + 1).min(src.height as usize - 1);
    let stride = src.row_bytes();
    let px = |xx: usize, yy: usize| &src.data[yy * stride + xx * 4..yy * stride + xx * 4 + 4];
    let (p00, p10, p01, p11) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] as f32 + (p10[c] as f32 - p00[c] as f32) * fx;
        let bottom = p01[c] as f32 + (p11[c] as f32 - p01[c] as f32) * fx;
        out[c] = (top + (bottom - top) * fy + 0.5) as u8;
    }
    out
}

/// Applies geometry (rotation, mirroring, cover-crop, zoom, pan) and tone controls in one pass.
pub fn process(src: &Frame, controls: &ImageControls) -> Frame {
    let (ow, oh) = output_dims(src, controls);
    let map = mapping(src, controls, ow, oh);
    let lut = tone_lut(controls);
    let saturation = controls.saturation;
    let copy_geometry = ow == src.width && oh == src.height && map.is_identity();

    let mut out = Frame::new(ow, oh);
    let row_bytes = out.row_bytes();
    out.data.par_chunks_exact_mut(row_bytes).enumerate().for_each(|(y, row)| {
        if copy_geometry {
            row.copy_from_slice(&src.data[y * row_bytes..(y + 1) * row_bytes]);
        } else {
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let (sx, sy) = map.apply(x as f32, y as f32);
                px.copy_from_slice(&sample_bilinear(src, sx, sy));
            }
        }
        if saturation != 1.0 {
            for px in row.chunks_exact_mut(4) {
                let luma = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
                for c in &mut px[..3] {
                    *c = (luma + (*c as f32 - luma) * saturation).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        if let Some(lut) = &lut {
            for px in row.chunks_exact_mut(4) {
                px[0] = lut[px[0] as usize];
                px[1] = lut[px[1] as usize];
                px[2] = lut[px[2] as usize];
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2×1 source: red, green.
    fn two_px() -> Frame {
        Frame { width: 2, height: 1, data: vec![255, 0, 0, 255, 0, 255, 0, 255] }
    }

    fn source_size() -> ImageControls {
        ImageControls { output: OutputSize::Source, ..Default::default() }
    }

    #[test]
    fn identity_copies_source() {
        let out = process(&two_px(), &source_size());
        assert_eq!(out.data, two_px().data);
    }

    #[test]
    fn mirror_swaps_columns() {
        let out = process(&two_px(), &ImageControls { mirror: true, ..source_size() });
        assert_eq!(out.data, vec![0, 255, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn rotate_90_turns_row_into_column() {
        let out = process(&two_px(), &ImageControls { rotation: Rotation::Cw90, ..source_size() });
        assert_eq!((out.width, out.height), (1, 2));
        // Clockwise: the left (red) pixel ends up on top.
        assert_eq!(out.data, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn rotate_270_puts_right_pixel_on_top() {
        let out = process(&two_px(), &ImageControls { rotation: Rotation::Cw270, ..source_size() });
        assert_eq!(out.data, vec![0, 255, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn cover_crop_keeps_centre() {
        // 4×2 source scaled into a 2×2 output keeps the middle two columns.
        let mut src = Frame::new(4, 2);
        for y in 0..2 {
            for x in 0..4 {
                let i = (y * 4 + x) * 4;
                src.data[i..i + 4].copy_from_slice(&[x as u8 * 60, 0, 0, 255]);
            }
        }
        let out = process(&src, &ImageControls { output: OutputSize::Fixed { width: 2, height: 2 }, ..Default::default() });
        assert_eq!(out.data[0], 60);
        assert_eq!(out.data[4], 120);
    }

    #[test]
    fn brightness_and_contrast_use_lut() {
        let src = Frame { width: 1, height: 1, data: vec![100, 128, 200, 255] };
        let out = process(&src, &ImageControls { brightness: 0.1, contrast: 2.0, ..source_size() });
        assert_eq!(out.data, vec![98, 154, 255, 255]);
    }
}
