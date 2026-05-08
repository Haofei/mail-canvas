use anyhow::Result;
use mail_canvas_core::{RenderOutputBackend, RenderedRgba};
use tiny_skia::Pixmap;

use crate::pdf;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct NativeOutputBackend;

impl RenderOutputBackend for NativeOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        pixmap.encode_png().map_err(Into::into)
    }

    fn encode_pdf(&self, rendered: &RenderedRgba) -> Result<Vec<u8>> {
        Ok(pdf::raster_pdf_from_rgba(rendered))
    }
}
