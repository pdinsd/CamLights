//! Text and image overlays composited onto the processed output.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use serde::{Deserialize, Serialize};

use crate::{Frame, ImageControls, OutputSize, process};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OverlayContent {
    Text {
        text: String,
        /// Straight (unpremultiplied) RGBA.
        color: [u8; 4],
        background: Option<[u8; 4]>,
    },
    Image {
        path: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    pub enabled: bool,
    pub content: OverlayContent,
    /// Centre of the overlay in output coordinates, each axis 0.0..=1.0.
    pub position: [f32; 2],
    /// Text: line height as a fraction of the output height. Image: width as a fraction of the output width.
    pub size: f32,
    /// 0.0..=1.0.
    pub opacity: f32,
}

impl Default for Overlay {
    fn default() -> Self {
        Self::text("CamLights")
    }
}

impl Overlay {
    /// A lower-third caption: white text on a translucent dark box.
    pub fn text(text: &str) -> Self {
        Self {
            enabled: true,
            content: OverlayContent::Text { text: text.to_owned(), color: [255, 255, 255, 255], background: Some([0, 0, 0, 160]) },
            position: [0.5, 0.88],
            size: 0.06,
            opacity: 1.0,
        }
    }

    /// A logo in the top-right corner.
    pub fn image(path: PathBuf) -> Self {
        Self { enabled: true, content: OverlayContent::Image { path }, position: [0.88, 0.14], size: 0.18, opacity: 1.0 }
    }
}

/// An overlay rendered for one output size.
struct Rendered {
    overlay: Overlay,
    output: (u32, u32),
    /// `None` when there is nothing to draw, such as empty text.
    bitmap: Result<Option<Frame>, String>,
}

/// Renders overlays once per settings change and composites them every frame.
pub struct OverlayRenderer {
    font: Option<FontArc>,
    /// Decoded source images, by path.
    images: HashMap<PathBuf, Result<Frame, String>>,
    rendered: Vec<Option<Rendered>>,
    bounds: Vec<Option<[f32; 4]>>,
}

impl OverlayRenderer {
    /// `font_data` is a TrueType/OpenType font used for text; text overlays report an error without one.
    pub fn new(font_data: Option<Vec<u8>>) -> Self {
        let font = font_data.and_then(|data| FontArc::try_from_vec(data).ok());
        Self { font, images: HashMap::new(), rendered: Vec::new(), bounds: Vec::new() }
    }

    /// Where the last [`Self::apply`] drew each overlay, as normalised `[left, top, right, bottom]`
    /// in output coordinates; `None` for overlays that drew nothing.
    pub fn bounds(&self) -> &[Option<[f32; 4]>] {
        &self.bounds
    }

    /// Draws every enabled overlay onto `frame` and returns the problems that prevented drawing some.
    pub fn apply(&mut self, frame: &mut Frame, overlays: &[Overlay]) -> Vec<String> {
        let output = (frame.width, frame.height);
        self.rendered.resize_with(overlays.len(), || None);
        // Forget images no overlay refers to any more.
        self.images.retain(|path, _| overlays.iter().any(|o| matches!(&o.content, OverlayContent::Image { path: p } if p == path)));

        let mut errors = Vec::new();
        self.bounds.clear();
        self.bounds.resize(overlays.len(), None);
        for (index, overlay) in overlays.iter().enumerate() {
            if !overlay.enabled {
                continue;
            }
            let fresh = self.rendered[index].as_ref().is_some_and(|r| r.overlay == *overlay && r.output == output);
            if !fresh {
                let bitmap = self.render(overlay, output);
                self.rendered[index] = Some(Rendered { overlay: overlay.clone(), output, bitmap });
            }
            match &self.rendered[index].as_ref().expect("rendered above").bitmap {
                Ok(Some(bitmap)) => {
                    blend(frame, bitmap, overlay.position, overlay.opacity);
                    let half_w = bitmap.width as f32 / frame.width as f32 / 2.0;
                    let half_h = bitmap.height as f32 / frame.height as f32 / 2.0;
                    let [x, y] = overlay.position;
                    self.bounds[index] = Some([x - half_w, y - half_h, x + half_w, y + half_h]);
                }
                Ok(None) => {}
                Err(e) => errors.push(e.clone()),
            }
        }
        errors
    }

    fn render(&mut self, overlay: &Overlay, (width, height): (u32, u32)) -> Result<Option<Frame>, String> {
        match &overlay.content {
            OverlayContent::Text { text, color, background } => {
                let font = self.font.as_ref().ok_or("No font is available for text overlays")?;
                let px = (overlay.size.clamp(0.01, 1.0) * height as f32).max(4.0);
                Ok(render_text(font, text, px, *color, *background))
            }
            OverlayContent::Image { path } => {
                let source = self.images.entry(path.clone()).or_insert_with(|| load_image(path)).as_ref().map_err(Clone::clone)?;
                let target_width = (overlay.size.clamp(0.01, 1.0) * width as f32).round().max(1.0) as u32;
                let target_height = ((target_width as f32 * source.height as f32 / source.width as f32).round() as u32).max(1);
                let controls = ImageControls { output: OutputSize::Fixed { width: target_width, height: target_height }, ..Default::default() };
                Ok(Some(process(source, &controls)))
            }
        }
    }
}

fn load_image(path: &Path) -> Result<Frame, String> {
    let image = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(Frame { width, height, data: image.into_raw() })
}

/// Lays out `text` (one line per `\n`) at `px` pixels per line, with an optional padded background box.
fn render_text(font: &FontArc, text: &str, px: f32, color: [u8; 4], background: Option<[u8; 4]>) -> Option<Frame> {
    let font = font.as_scaled(PxScale::from(px));
    let lines: Vec<&str> = text.lines().collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let line_advance = font.height() + font.line_gap();
    let line_width = |line: &str| {
        let mut previous = None;
        let mut width = 0.0;
        for c in line.chars() {
            let id = font.glyph_id(c);
            if let Some(previous) = previous {
                width += font.kern(previous, id);
            }
            width += font.h_advance(id);
            previous = Some(id);
        }
        width
    };

    let padding = if background.is_some() { (px * 0.3).ceil() } else { (px * 0.05).ceil() };
    let text_width = lines.iter().map(|l| line_width(l)).fold(0.0, f32::max);
    let width = (text_width + 2.0 * padding).ceil() as u32;
    let height = (lines.len() as f32 * line_advance - font.line_gap() + 2.0 * padding).ceil() as u32;

    // Glyph coverage per pixel, combined with max so overlapping glyph edges do not double up.
    let mut coverage = vec![0.0f32; width as usize * height as usize];
    for (index, line) in lines.iter().enumerate() {
        let baseline = padding + font.ascent() + index as f32 * line_advance;
        let mut caret = padding + (text_width - line_width(line)) / 2.0;
        let mut previous = None;
        for c in line.chars() {
            let id = font.glyph_id(c);
            if let Some(previous) = previous {
                caret += font.kern(previous, id);
            }
            let glyph = id.with_scale_and_position(font.scale(), point(caret, baseline));
            caret += font.h_advance(id);
            previous = Some(id);
            let Some(outlined) = font.outline_glyph(glyph) else { continue };
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, c| {
                let x = bounds.min.x as i64 + gx as i64;
                let y = bounds.min.y as i64 + gy as i64;
                if (0..width as i64).contains(&x) && (0..height as i64).contains(&y) {
                    let cell = &mut coverage[y as usize * width as usize + x as usize];
                    *cell = cell.max(c);
                }
            });
        }
    }

    let mut bitmap = Frame::new(width, height);
    let back = background.unwrap_or([0, 0, 0, 0]);
    let back_alpha = back[3] as f32 / 255.0;
    for (px, &cover) in bitmap.data.chunks_exact_mut(4).zip(&coverage) {
        // Text over background, both straight alpha.
        let text_alpha = cover.clamp(0.0, 1.0) * color[3] as f32 / 255.0;
        let alpha = text_alpha + back_alpha * (1.0 - text_alpha);
        if alpha <= 0.0 {
            continue;
        }
        for c in 0..3 {
            let value = (color[c] as f32 * text_alpha + back[c] as f32 * back_alpha * (1.0 - text_alpha)) / alpha;
            px[c] = value.round() as u8;
        }
        px[3] = (alpha * 255.0).round() as u8;
    }
    Some(bitmap)
}

/// Alpha-blends `overlay` centred at `centre` (normalised output coordinates).
fn blend(frame: &mut Frame, overlay: &Frame, centre: [f32; 2], opacity: f32) {
    let opacity = opacity.clamp(0.0, 1.0);
    let left = (centre[0] * frame.width as f32 - overlay.width as f32 / 2.0).round() as i64;
    let top = (centre[1] * frame.height as f32 - overlay.height as f32 / 2.0).round() as i64;
    let frame_stride = frame.row_bytes();
    for oy in 0..overlay.height as i64 {
        let y = top + oy;
        if !(0..frame.height as i64).contains(&y) {
            continue;
        }
        for ox in 0..overlay.width as i64 {
            let x = left + ox;
            if !(0..frame.width as i64).contains(&x) {
                continue;
            }
            let src = &overlay.data[(oy as usize * overlay.width as usize + ox as usize) * 4..][..4];
            let alpha = src[3] as f32 / 255.0 * opacity;
            if alpha <= 0.0 {
                continue;
            }
            let dst = &mut frame.data[y as usize * frame_stride + x as usize * 4..][..3];
            for c in 0..3 {
                dst[c] = (src[c] as f32 * alpha + dst[c] as f32 * (1.0 - alpha)).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_overlay_replaces_pixels_and_clips_at_edges() {
        let mut frame = Frame::new(4, 4);
        let mut overlay = Frame::new(2, 2);
        for px in overlay.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[200, 100, 50, 255]);
        }
        // Centred on the top-left corner: only one overlay pixel lands inside the frame.
        blend(&mut frame, &overlay, [0.0, 0.0], 1.0);
        assert_eq!(&frame.data[..4], &[200, 100, 50, 0]);
        assert_eq!(&frame.data[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn opacity_mixes_with_the_frame() {
        let mut frame = Frame::new(2, 2);
        let mut overlay = Frame::new(2, 2);
        for px in overlay.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[255, 255, 255, 255]);
        }
        blend(&mut frame, &overlay, [0.5, 0.5], 0.5);
        assert_eq!(frame.data[0], 128);
    }

    #[test]
    fn missing_image_reports_an_error() {
        let mut renderer = OverlayRenderer::new(None);
        let mut frame = Frame::new(8, 8);
        let errors = renderer.apply(&mut frame, &[Overlay::image(PathBuf::from("Z:/no/such/logo.png"))]);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn text_without_a_font_reports_an_error() {
        let mut renderer = OverlayRenderer::new(None);
        let mut frame = Frame::new(8, 8);
        assert_eq!(renderer.apply(&mut frame, &[Overlay::text("hi")]).len(), 1);
    }

    #[test]
    fn text_renders_with_a_system_font() {
        let Ok(data) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
        let font = FontArc::try_from_vec(data).unwrap();
        let bitmap = render_text(&font, "Hi", 40.0, [255, 255, 255, 255], None).unwrap();
        assert!(bitmap.width > 20 && bitmap.height >= 40);
        assert!(bitmap.data.chunks_exact(4).any(|px| px[3] == 255), "some glyph pixels are fully covered");
        assert!(render_text(&font, "  ", 40.0, [255; 4], None).is_none());
    }
}
