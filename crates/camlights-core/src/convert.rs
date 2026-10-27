use rayon::prelude::*;

use crate::{Frame, ImageControls, OutputSize, process};

/// Scales `src` to cover `width`×`height`, centre-cropping the excess.
pub fn fit(src: &Frame, width: u32, height: u32) -> Frame {
    process(src, &ImageControls { output: OutputSize::Fixed { width, height }, ..Default::default() })
}

// BT.709 limited-range coefficients scaled by 256.
const Y_COEFFS: [i32; 3] = [47, 157, 16];
const U_COEFFS: [i32; 3] = [-26, -86, 112];
const V_COEFFS: [i32; 3] = [112, -102, -10];

#[inline]
fn dot(c: [i32; 3], r: i32, g: i32, b: i32) -> i32 {
    (c[0] * r + c[1] * g + c[2] * b + 128) >> 8
}

/// Writes `src` as NV12 (BT.709, limited range) into `out`, which must hold `width * height * 3 / 2` bytes.
/// Both dimensions must be even.
pub fn rgba_to_nv12(src: &Frame, out: &mut [u8]) {
    let (w, h) = (src.width as usize, src.height as usize);
    assert!(w % 2 == 0 && h % 2 == 0, "NV12 needs even dimensions, got {w}×{h}");
    assert!(out.len() >= w * h * 3 / 2, "NV12 output buffer is too small");
    let (luma, chroma) = out[..w * h * 3 / 2].split_at_mut(w * h);

    luma.par_chunks_exact_mut(w).zip(src.data.par_chunks_exact(w * 4)).for_each(|(dst, row)| {
        for (y, px) in dst.iter_mut().zip(row.chunks_exact(4)) {
            *y = (16 + dot(Y_COEFFS, px[0] as i32, px[1] as i32, px[2] as i32)) as u8;
        }
    });

    chroma.par_chunks_exact_mut(w).enumerate().for_each(|(by, dst)| {
        let top = &src.data[by * 2 * w * 4..(by * 2 + 1) * w * 4];
        let bottom = &src.data[(by * 2 + 1) * w * 4..(by * 2 + 2) * w * 4];
        for (bx, uv) in dst.chunks_exact_mut(2).enumerate() {
            let i = bx * 8;
            let sum = |c: usize| top[i + c] as i32 + top[i + 4 + c] as i32 + bottom[i + c] as i32 + bottom[i + 4 + c] as i32;
            let (r, g, b) = ((sum(0) + 2) / 4, (sum(1) + 2) / 4, (sum(2) + 2) / 4);
            uv[0] = (128 + dot(U_COEFFS, r, g, b)).clamp(16, 240) as u8;
            uv[1] = (128 + dot(V_COEFFS, r, g, b)).clamp(16, 240) as u8;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgb: [u8; 3]) -> Frame {
        let mut frame = Frame::new(2, 2);
        for px in frame.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        frame
    }

    #[test]
    fn white_and_black_hit_limited_range_ends() {
        let mut out = [0u8; 6];
        rgba_to_nv12(&solid([255, 255, 255]), &mut out);
        assert_eq!(out, [235, 235, 235, 235, 128, 128]);
        rgba_to_nv12(&solid([0, 0, 0]), &mut out);
        assert_eq!(out, [16, 16, 16, 16, 128, 128]);
    }

    #[test]
    fn pure_red_has_high_cr() {
        let mut out = [0u8; 6];
        rgba_to_nv12(&solid([255, 0, 0]), &mut out);
        assert_eq!(out[0], 63);
        assert!(out[4] < 128 && out[5] == 240);
    }
}
