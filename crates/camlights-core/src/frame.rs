/// A tightly packed, top-down RGBA8 image.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    /// Builds a frame from 32-bit BGRX rows, as produced by Media Foundation's RGB32.
    ///
    /// `rows` yields each source row top to bottom; every row must hold at least `width * 4` bytes.
    pub fn from_bgrx_rows<'a>(width: u32, height: u32, rows: impl Iterator<Item = &'a [u8]>) -> Self {
        let mut frame = Self::new(width, height);
        let row_bytes = width as usize * 4;
        for (dst, src) in frame.data.chunks_exact_mut(row_bytes).zip(rows) {
            for (d, s) in dst.chunks_exact_mut(4).zip(src[..row_bytes].chunks_exact(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = 255;
            }
        }
        frame
    }

    pub fn row_bytes(&self) -> usize {
        self.width as usize * 4
    }
}
