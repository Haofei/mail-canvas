#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

#[cfg(test)]
use cosmic_text::{
    Attrs, Buffer, Color as TextColor, FontSystem, Metrics, Shaping, Weight as FontWeight,
};
#[cfg(test)]
use kuchiki::traits::TendrilSink as _;
mod api;
mod css;
mod debug;
mod document;
mod dom;
mod font_catalog;
mod fonts;
mod layout;
mod output;
mod paint;
mod render;
mod resource;
mod style;
mod table;
#[cfg(test)]
mod test_support;
mod text;

pub use api::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ConsoleMessage, EmailRenderer,
    RenderDebugOptions, RenderDiagnosticsReport, RenderRequest, RenderWarning, RenderWarningCode,
    RenderedImage, RenderedPdf, RenderedRgba, ResourcePolicy,
};
#[cfg(test)]
use css::css_declarations;
#[cfg(test)]
use css::{inline_css, strip_hidden_conditional_comments};
pub use debug::{
    ImageDiagnosticKind, ImageLayoutDiagnostic, IntrinsicSizeSnapshot, LayoutNodeSnapshot,
    LayoutStyleSnapshot, RectSnapshot, RenderDebugSnapshot, TextRectSnapshot,
};
pub use document::{PreparedDocument, build_document};
#[cfg(test)]
use dom::find_first_tag;
pub use fonts::MailCanvasFontFallback;
#[cfg(test)]
use fonts::{
    FontFamilyIndex, WebFontFace, font_face_covers_basic_latin, stylesheet_link_urls,
    system_font_database,
};
#[cfg(test)]
use layout::{LayoutBox, LayoutEngine, LayoutKind, RenderLimits};
pub use output::OutputBackend as RenderOutputBackend;
#[cfg(test)]
use paint::{
    ImageFitPaint, apply_text_base_alpha, apply_text_opacity, draw_background_image,
    draw_image_with_fit, object_fit_rect, point_in_rounded_rect, sample_image_area,
    sample_image_bilinear,
};
pub use render::RendererCore;
#[cfg(test)]
use render::validate_request;
pub use resource::{ImageData, ResourceProvider, ResourceProviderFactory, repair_png_chunk_crcs};
#[cfg(test)]
use style::BackgroundPosition;
#[cfg(test)]
use style::{
    BackgroundImagePaint, BackgroundRepeat, BackgroundSize, BorderLineStyle, Display, Edges,
    ObjectFit, ObjectPosition, PositionAxis, Rect, Rgba, Style, TextAlign, TextSpan,
    style_for_node,
};
pub(crate) use style::{Length, parse_font_style, parse_length};
#[cfg(test)]
use test_support::{MailCanvasRenderer, layout_for_test, resource_policy_for_test};
#[cfg(test)]
use text::{normalize_text, rich_text_baseline_leading_offset, spans_text};
#[cfg(test)]
use tiny_skia::Pixmap;

const HARD_BREAK: char = '\u{000B}';
const HARD_BREAK_STR: &str = "\u{000B}";

#[cfg(test)]
mod tests;
