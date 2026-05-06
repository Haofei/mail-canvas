use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, ValueEnum};
use mail_canvas_core::{EmailRenderer, RenderDiagnosticsReport, RenderRequest, ResourcePolicy};
use mail_canvas_native::{MailCanvasRenderer, build_document_from_files};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PdfMode {
    Raster,
}

#[derive(Debug, Parser)]
#[command(name = "mail-canvas")]
#[command(about = "Render an HTML/CSS email template to a PNG")]
struct Args {
    /// HTML file to render.
    #[arg(long)]
    html: PathBuf,

    /// Optional CSS file to inject into the document head.
    #[arg(long)]
    css: Option<PathBuf>,

    /// PNG output path.
    #[arg(short, long)]
    output: PathBuf,

    /// Optional JSON diagnostics output path for structured renderer warnings.
    #[arg(long)]
    warnings_json: Option<PathBuf>,

    /// Optional JSON layout dump output path for renderer box geometry.
    #[arg(long)]
    layout_json: Option<PathBuf>,

    /// Optional raster PDF output path.
    #[arg(long)]
    pdf_output: Option<PathBuf>,

    /// PDF output mode.
    #[arg(long, value_enum, default_value_t = PdfMode::Raster)]
    pdf_mode: PdfMode,

    /// CSS viewport width.
    #[arg(long, default_value_t = 600)]
    width: u32,

    /// Initial CSS viewport height used before full-height measurement.
    #[arg(long, default_value_t = 800)]
    viewport_height: u32,

    /// Minimum final CSS output height.
    #[arg(long, default_value_t = 1)]
    min_height: u32,

    /// Maximum final CSS output height.
    #[arg(long)]
    max_height: Option<u32>,

    /// Device pixel scale. Use 2.0 for retina output.
    #[arg(long, default_value_t = 1.0)]
    scale: f32,

    /// Reserved compatibility option; the pure Rust renderer does not load pages.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Resource timeout in milliseconds. Overrides --timeout.
    #[arg(long)]
    timeout_ms: Option<u64>,

    /// Reserved compatibility option; the pure Rust renderer does not wait for scripts.
    #[arg(long, default_value_t = 100)]
    settle_ms: u64,

    /// Base URL for resolving relative assets. Defaults to the HTML file directory.
    #[arg(long)]
    base_url: Option<String>,

    /// Allow remote http(s) image resources.
    #[arg(long)]
    allow_remote: bool,

    /// Allow non-HTTPS remote resources when --allow-remote is set.
    #[arg(long)]
    allow_http: bool,

    /// Maximum encoded image resource size in bytes.
    #[arg(long, default_value_t = 10 * 1024 * 1024)]
    max_image_bytes: usize,

    /// Maximum decoded image size in pixels.
    #[arg(long, default_value_t = 16_000_000)]
    max_decoded_pixels: u64,

    /// Maximum total resource bytes across all fetched assets in a single render.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_total_resource_bytes: usize,

    /// Maximum number of external resources fetched in a single render.
    #[arg(long, default_value_t = 128)]
    max_resource_count: usize,

    /// Allow private/localhost network resource access when remote loading is enabled.
    #[arg(long)]
    allow_private_network: bool,

    /// Maximum DOM nodes accepted before rendering.
    #[arg(long, default_value_t = 100_000)]
    max_dom_nodes: usize,

    /// Maximum nested layout depth before nested content is truncated.
    #[arg(long, default_value_t = 64)]
    max_layout_depth: usize,

    /// Maximum expanded table cell slots accepted during table layout.
    #[arg(long, default_value_t = 100_000)]
    max_table_cells: usize,

    /// Font files to load instead of scanning system fonts.
    #[arg(long = "font-file")]
    font_files: Vec<PathBuf>,

    /// Directories containing font files to load instead of scanning system fonts.
    #[arg(long = "font-dir")]
    font_dirs: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let font_paths = collect_font_paths(&args.font_files, &args.font_dirs)?;

    let document = build_document_from_files(
        &args.html,
        args.css.as_deref(),
        args.base_url.as_deref(),
        args.width,
    )?;
    let request = RenderRequest {
        html: document.html,
        width: args.width,
        viewport_height: args.viewport_height,
        min_height: args.min_height,
        scale: args.scale,
        settle: Duration::from_millis(args.settle_ms),
        base_url: document.base_url,
        max_height: args.max_height,
        resource_policy: ResourcePolicy {
            allow_remote: args.allow_remote,
            https_only: !args.allow_http,
            deny_private_networks: !args.allow_private_network,
            timeout: args
                .timeout_ms
                .map(Duration::from_millis)
                .unwrap_or_else(|| Duration::from_secs(args.timeout)),
            max_resource_bytes: args.max_image_bytes,
            max_total_resource_bytes: args.max_total_resource_bytes,
            max_decoded_pixels: args.max_decoded_pixels,
            max_resource_count: args.max_resource_count,
        },
        max_dom_nodes: args.max_dom_nodes,
        max_layout_depth: args.max_layout_depth,
        max_table_cells: args.max_table_cells,
        text_hints: Vec::new(),
    };

    let mut renderer = MailCanvasRenderer::with_fonts(
        request.width,
        request.viewport_height,
        request.scale,
        font_paths,
    )?;
    let image = renderer.render_png(request.clone())?;

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&args.output, &image.png)
        .with_context(|| format!("failed to write {}", args.output.display()))?;

    if let Some(path) = &args.warnings_json {
        write_warnings_json(path, &image.diagnostics())?;
    }
    if let Some(path) = &args.layout_json {
        write_layout_json(path, &image.layout, &image.text_rects)?;
    }

    eprintln!(
        "rendered {}x{} CSS px at {}x scale -> {}x{} px ({})",
        image.css_width,
        image.css_height,
        image.scale,
        image.pixel_width,
        image.pixel_height,
        args.output.display()
    );

    for message in &image.console_messages {
        eprintln!("console.{}: {}", message.level, message.message);
    }

    if let Some(pdf_output) = args.pdf_output {
        let pdf = match args.pdf_mode {
            PdfMode::Raster => renderer.render_pdf(request)?,
        };
        if let Some(parent) = pdf_output.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&pdf_output, &pdf.pdf)
            .with_context(|| format!("failed to write {}", pdf_output.display()))?;
        eprintln!(
            "rendered raster PDF {}x{} px ({})",
            pdf.pixel_width,
            pdf.pixel_height,
            pdf_output.display()
        );
        for message in &pdf.console_messages {
            eprintln!("console.{}: {}", message.level, message.message);
        }
    }

    Ok(())
}

fn write_warnings_json(path: &std::path::Path, report: &RenderDiagnosticsReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(&report).context("failed to serialize warnings JSON")?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn write_layout_json(
    path: &std::path::Path,
    layout: &mail_canvas_core::LayoutNodeSnapshot,
    text_rects: &[mail_canvas_core::TextRectSnapshot],
) -> Result<()> {
    #[derive(serde::Serialize)]
    struct LayoutDump<'a> {
        tree: &'a mail_canvas_core::LayoutNodeSnapshot,
        text_rects: &'a [mail_canvas_core::TextRectSnapshot],
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(&LayoutDump {
        tree: layout,
        text_rects,
    })
    .context("failed to serialize layout JSON")?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn collect_font_paths(font_files: &[PathBuf], font_dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = font_files.to_vec();
    for dir in font_dirs {
        let entries =
            fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_file() && is_font_file(&path) {
                paths.push(path);
            }
        }
    }
    if paths.is_empty() && (!font_files.is_empty() || !font_dirs.is_empty()) {
        bail!("no font files found in supplied --font-file/--font-dir paths");
    }
    Ok(paths)
}

fn is_font_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        })
        .unwrap_or(false)
}
