use anyhow::Result;
use tiny_skia::Pixmap;

use crate::RenderedRgba;

pub trait OutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>>;
    fn encode_pdf(&self, rendered: &RenderedRgba) -> Result<Vec<u8>>;
}
