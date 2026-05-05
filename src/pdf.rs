use anyhow::{Context as _, Result};
use pdf_writer::{Content, Name, Pdf, Rect as PdfRect, Ref};

use crate::RenderedImage;

pub(crate) fn raster_pdf_from_png(rendered: &RenderedImage) -> Result<Vec<u8>> {
    let width = rendered.pixel_width.max(1);
    let height = rendered.pixel_height.max(1);
    let rgb = image::load_from_memory(&rendered.png)
        .context("failed to decode rendered PNG for PDF output")?
        .to_rgb8();
    let mut pdf = Pdf::new();
    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);
    let page_id = Ref::new(3);
    let image_id = Ref::new(4);
    let content_id = Ref::new(5);

    pdf.catalog(catalog_id).pages(page_tree_id);
    pdf.pages(page_tree_id).kids([page_id]).count(1);

    {
        let mut page = pdf.page(page_id);
        page.parent(page_tree_id);
        page.media_box(PdfRect::new(0.0, 0.0, width as f32, height as f32));
        page.resources().x_objects().pair(Name(b"Im1"), image_id);
        page.contents(content_id);
    }

    {
        let mut image = pdf.image_xobject(image_id, rgb.as_raw());
        image.width(width as i32);
        image.height(height as i32);
        image.color_space().device_rgb();
        image.bits_per_component(8);
    }

    let mut content = Content::new();
    content.save_state();
    content.transform([width as f32, 0.0, 0.0, height as f32, 0.0, 0.0]);
    content.x_object(Name(b"Im1"));
    content.restore_state();
    pdf.stream(content_id, &content.finish());

    Ok(pdf.finish())
}
