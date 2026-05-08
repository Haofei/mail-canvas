use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use cosmic_text::FontSystem;
use mail_canvas_core::{
    EmailRenderer, MailCanvasFontFallback, RenderRequest, RenderedImage, RenderedPdf, RendererCore,
};

mod fonts;
mod image;
mod output;
mod pdf;
mod resource;

pub use mail_canvas_core::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ConsoleMessage, PreparedDocument,
    RenderWarning, RenderWarningCode, build_document,
};
pub use pdf::{raster_pdf_from_png, raster_pdf_from_rgba};
pub use resource::NativeResourceProviderFactory;

use fonts::{
    font_database_from_paths, html_needs_emoji_font, load_default_emoji_font_if_missing,
    system_font_database,
};
use output::NativeOutputBackend;

pub struct MailCanvasRenderer {
    inner: RendererCore,
}

impl MailCanvasRenderer {
    pub fn new(width: u32, viewport_height: u32, scale: f32) -> Result<Self> {
        Self::with_fonts(width, viewport_height, scale, [])
    }

    pub fn with_fonts(
        width: u32,
        viewport_height: u32,
        scale: f32,
        font_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        validate_scale(scale)?;
        let _ = scaled_dimension(width, scale, "width")?;
        let _ = scaled_dimension(viewport_height.max(1), scale, "viewport-height")?;
        let font_paths: Vec<PathBuf> = font_paths.into_iter().collect();
        let font_db = if font_paths.is_empty() {
            system_font_database()
        } else {
            font_database_from_paths(font_paths)?
        };
        let font_system = FontSystem::new_with_locale_and_db_and_fallback(
            "en-US".to_string(),
            font_db,
            MailCanvasFontFallback,
        );
        Ok(Self {
            inner: RendererCore::new(font_system),
        })
    }
}

pub type RustEmailRenderer = MailCanvasRenderer;
pub type ServoEmailRenderer = MailCanvasRenderer;

pub fn build_document_from_files(
    html_path: &Path,
    css_path: Option<&Path>,
    base_url: Option<&str>,
    width: u32,
) -> Result<PreparedDocument> {
    let html = fs::read_to_string(html_path)
        .with_context(|| format!("failed to read {}", html_path.display()))?;
    let css = match css_path {
        Some(path) => Some(
            fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        ),
        None => None,
    };

    let base = match base_url {
        Some(raw) => {
            Some(url::Url::parse(raw).with_context(|| format!("invalid --base-url {raw}"))?)
        }
        None => {
            let dir = html_parent_dir(html_path);
            let dir = dir.canonicalize().with_context(|| {
                format!("failed to resolve HTML parent directory: {}", dir.display())
            })?;
            Some(url::Url::from_directory_path(&dir).map_err(|()| {
                anyhow!(
                    "failed to convert HTML parent directory to base URL: {}",
                    dir.display()
                )
            })?)
        }
    };

    Ok(PreparedDocument {
        html: build_document(&html, css.as_deref(), base.as_ref(), width),
        base_url: base,
    })
}

fn html_parent_dir(html_path: &Path) -> &Path {
    html_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

impl EmailRenderer for MailCanvasRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage> {
        self.load_default_emoji_font_for_html(&request.html);
        self.inner.render_png_with(
            request,
            &NativeResourceProviderFactory,
            &NativeOutputBackend,
        )
    }

    fn render_pdf(&mut self, request: RenderRequest) -> Result<RenderedPdf> {
        self.load_default_emoji_font_for_html(&request.html);
        self.inner.render_pdf_with(
            request,
            &NativeResourceProviderFactory,
            &NativeOutputBackend,
        )
    }
}

impl MailCanvasRenderer {
    fn load_default_emoji_font_for_html(&mut self, html: &str) {
        if !html_needs_emoji_font(html) {
            return;
        }
        load_default_emoji_font_if_missing(self.inner.font_system_mut().db_mut());
    }
}

fn validate_scale(scale: f32) -> Result<()> {
    if !scale.is_finite() || scale <= 0.0 {
        bail!("scale must be a finite positive number");
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scaled_dimension(value: u32, scale: f32, label: &str) -> Result<u32> {
    const MAX_RENDER_PIXELS_PER_AXIS: u32 = 16_384;
    if value == 0 {
        bail!("{label} must be greater than zero");
    }
    let scaled = f64::from(value) * f64::from(scale);
    if scaled > f64::from(MAX_RENDER_PIXELS_PER_AXIS) {
        bail!(
            "{label} at requested scale is too large: {scaled:.0}px > {MAX_RENDER_PIXELS_PER_AXIS}px"
        );
    }
    Ok(scaled.ceil().max(1.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::font_family_available;

    #[test]
    fn explicit_font_database_loads_default_emoji_fixture_on_demand() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("fonts");
        let mut renderer = MailCanvasRenderer::with_fonts(
            240,
            120,
            1.0,
            [
                root.join("NotoSans-Regular.ttf"),
                root.join("NotoSans-Bold.ttf"),
            ],
        )
        .expect("renderer");

        assert!(!font_family_available(
            renderer.inner.font_system_mut().db(),
            "Noto Color Emoji"
        ));

        renderer.load_default_emoji_font_for_html("<p>Emoji &#x1f60d;</p>");
        assert!(font_family_available(
            renderer.inner.font_system_mut().db(),
            "Noto Color Emoji"
        ));
    }

    #[test]
    fn default_emoji_fixture_is_not_loaded_for_plain_html() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("fonts");
        let mut renderer = MailCanvasRenderer::with_fonts(
            240,
            120,
            1.0,
            [
                root.join("NotoSans-Regular.ttf"),
                root.join("NotoSans-Bold.ttf"),
            ],
        )
        .expect("renderer");

        renderer.load_default_emoji_font_for_html("<p>No emoji here</p>");
        assert!(!font_family_available(
            renderer.inner.font_system_mut().db(),
            "Noto Color Emoji"
        ));
    }

    #[test]
    fn bare_html_filename_uses_current_directory_as_base_url() {
        assert_eq!(html_parent_dir(Path::new("email.html")), Path::new("."));
    }

    #[test]
    fn nested_html_filename_uses_explicit_parent_directory() {
        assert_eq!(
            html_parent_dir(Path::new("templates/email.html")),
            Path::new("templates")
        );
    }
}
