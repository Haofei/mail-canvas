use anyhow::{Context as _, Result};
use tiny_skia::Pixmap;

use crate::{RenderedImage, pdf::raster_pdf_from_png};

pub(crate) trait OutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>>;
    fn encode_pdf(&self, rendered: &RenderedImage) -> Result<Vec<u8>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct NativeOutputBackend;

impl OutputBackend for NativeOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        pixmap.encode_png().context("failed to encode PNG")
    }

    fn encode_pdf(&self, rendered: &RenderedImage) -> Result<Vec<u8>> {
        raster_pdf_from_png(rendered)
    }
}
