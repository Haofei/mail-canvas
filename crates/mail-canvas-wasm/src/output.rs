use anyhow::{Result, bail};
use mail_canvas_core::RenderOutputBackend;
use tiny_skia::Pixmap;

#[derive(Default)]
pub(crate) struct WasmOutputBackend;

impl RenderOutputBackend for WasmOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        pixmap.encode_png().map_err(Into::into)
    }

    fn encode_pdf(&self, _rendered: &mail_canvas_core::RenderedRgba) -> Result<Vec<u8>> {
        bail!("PDF is not supported in wasm")
    }
}
