use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use cosmic_text::FontSystem;
use fontdb::Database;
use mail_canvas_core::{
    EmailRenderer, MailCanvasFontFallback, RenderOutputBackend, RenderRequest, RenderedImage,
    RenderedPdf, RenderedRgba, RendererCore,
};
use tiny_skia::Pixmap;

mod pdf;
mod resource;

pub use mail_canvas_core::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ConsoleMessage, PreparedDocument,
    RenderWarning, RenderWarningCode, build_document,
};
pub use pdf::{raster_pdf_from_png, raster_pdf_from_rgba};
pub use resource::NativeResourceProviderFactory;

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
            font_database_from_paths(&font_paths)?
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
        self.inner.render_png_with(
            request,
            &NativeResourceProviderFactory,
            &NativeOutputBackend,
        )
    }

    fn render_pdf(&mut self, request: RenderRequest) -> Result<RenderedPdf> {
        self.inner.render_pdf_with(
            request,
            &NativeResourceProviderFactory,
            &NativeOutputBackend,
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct NativeOutputBackend;

impl RenderOutputBackend for NativeOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        pixmap.encode_png().map_err(Into::into)
    }

    fn encode_pdf(&self, rendered: &RenderedRgba) -> Result<Vec<u8>> {
        Ok(pdf::raster_pdf_from_rgba(rendered))
    }
}

fn system_font_database() -> Database {
    let mut db = Database::new();
    db.load_system_fonts();
    #[cfg(target_os = "macos")]
    db.load_fonts_dir("/System/Library/Fonts/Supplemental");
    load_default_emoji_font_if_missing(&mut db);
    set_generic_font_families(&mut db);
    db
}

fn font_database_from_paths(paths: &[PathBuf]) -> Result<Database> {
    let mut db = Database::new();
    for path in paths {
        if !path.is_file() {
            bail!("font path is not a file: {}", path.display());
        }
        db.load_font_source(fontdb::Source::File(path.clone()));
    }
    if db.is_empty() {
        bail!("no valid font faces found in supplied font files");
    }
    load_default_emoji_font_if_missing(&mut db);
    set_generic_font_families(&mut db);
    Ok(db)
}

fn load_default_emoji_font_if_missing(db: &mut Database) {
    if emoji_font_available(db) {
        return;
    }
    let path = default_emoji_font_path();
    if path.is_file() {
        db.load_font_source(fontdb::Source::File(path));
    }
}

fn default_emoji_font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("fonts")
        .join("NotoColorEmoji.ttf")
}

fn emoji_font_available(db: &Database) -> bool {
    ["Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji"]
        .iter()
        .any(|family| font_family_available(db, family))
}

fn set_generic_font_families(db: &mut Database) {
    let fallback_family = db
        .faces()
        .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
        .next();
    let sans = first_available_family(
        db,
        &[
            "Arial",
            "Helvetica",
            "Helvetica Neue",
            "Avenir",
            "Segoe UI",
            "Tahoma",
            "Trebuchet MS",
            "Verdana",
            "Roboto",
            "Open Sans",
            "DejaVu Sans",
            "Arimo",
            "Noto Sans",
        ],
    )
    .or_else(|| fallback_family.clone());
    let serif = first_available_family(
        db,
        &[
            "Times",
            "Times New Roman",
            "Georgia",
            "Palatino",
            "Palatino Linotype",
            "Iowan Old Style",
            "DejaVu Serif",
            "Tinos",
            "Noto Serif",
        ],
    )
    .or_else(|| fallback_family.clone());
    let mono = first_available_family(
        db,
        &[
            "Courier New",
            "Menlo",
            "Monaco",
            "Consolas",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
        ],
    )
    .or(fallback_family);

    if let Some(sans) = sans {
        db.set_sans_serif_family(sans);
    }
    if let Some(serif) = serif {
        db.set_serif_family(serif);
    }
    if let Some(mono) = mono {
        db.set_monospace_family(mono);
    }
}

fn first_available_family(db: &Database, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| font_family_available(db, candidate))
        .map(|candidate| (*candidate).to_string())
}

fn font_family_available(db: &Database, candidate: &str) -> bool {
    db.faces().any(|face| {
        face.families
            .iter()
            .any(|(family, _)| family.eq_ignore_ascii_case(candidate))
    })
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

    #[test]
    fn explicit_font_database_adds_default_emoji_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("fonts");
        let db = font_database_from_paths(&[
            root.join("NotoSans-Regular.ttf"),
            root.join("NotoSans-Bold.ttf"),
        ])
        .expect("font database");

        assert!(font_family_available(&db, "Noto Color Emoji"));
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
