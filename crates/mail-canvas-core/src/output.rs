use anyhow::Result;
use tiny_skia::Pixmap;

use crate::RenderedImage;

pub trait OutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>>;
    fn encode_pdf(&self, rendered: &RenderedImage) -> Result<Vec<u8>>;
}
