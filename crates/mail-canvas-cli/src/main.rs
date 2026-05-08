use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mail_canvas_core::{
    EmailRenderer, RenderDebugOptions, RenderRequest, RenderedImage, ResourcePolicy,
};
use mail_canvas_native::{MailCanvasRenderer, build_document_from_files, raster_pdf_from_png};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PdfMode {
    Raster,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RenderProfile {
    Generic,
    #[value(name = "desktop-800")]
    Desktop800,
    #[value(name = "mobile-375")]
    Mobile375,
    #[value(name = "mobile-390")]
    Mobile390,
    #[value(name = "mobile-414")]
    Mobile414,
    Thumbnail,
    #[value(name = "gmail-ish")]
    GmailIsh,
    #[value(name = "apple-mail-ish")]
    AppleMailIsh,
    #[value(name = "outlook-ish")]
    OutlookIsh,
    #[value(name = "images-blocked")]
    ImagesBlocked,
}

impl RenderProfile {
    fn defaults(self) -> ProfileDefaults {
        match self {
            Self::Generic => ProfileDefaults {
                width: 600,
                viewport_height: 800,
                scale: 1.0,
            },
            Self::Desktop800 | Self::Thumbnail | Self::AppleMailIsh | Self::ImagesBlocked => {
                ProfileDefaults {
                    width: 800,
                    viewport_height: 1200,
                    scale: 1.0,
                }
            }
            Self::Mobile375 => ProfileDefaults {
                width: 375,
                viewport_height: 812,
                scale: 1.0,
            },
            Self::Mobile390 => ProfileDefaults {
                width: 390,
                viewport_height: 844,
                scale: 1.0,
            },
            Self::Mobile414 => ProfileDefaults {
                width: 414,
                viewport_height: 896,
                scale: 1.0,
            },
            Self::GmailIsh | Self::OutlookIsh => ProfileDefaults {
                width: 600,
                viewport_height: 800,
                scale: 1.0,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ProfileDefaults {
    width: u32,
    viewport_height: u32,
    scale: f32,
}

impl Default for ProfileDefaults {
    fn default() -> Self {
        Self {
            width: 600,
            viewport_height: 800,
            scale: 1.0,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "mail-canvas")]
#[command(about = "Render and inspect HTML/CSS email templates without launching Chrome")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    legacy_render: RenderArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Render an HTML/CSS email template to a PNG or raster PDF.
    Render(Box<RenderArgs>),
    /// List built-in viewport/profile presets.
    Profiles,
}

#[derive(Debug, Parser)]
struct RenderArgs {
    /// HTML file to render.
    #[arg(long)]
    html: Option<PathBuf>,

    /// Optional CSS file to inject into the document head.
    #[arg(long)]
    css: Option<PathBuf>,

    /// PNG output path.
    #[arg(short, long)]
    output: Option<PathBuf>,

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
    #[arg(long)]
    width: Option<u32>,

    /// Initial CSS viewport height used before full-height measurement.
    #[arg(long)]
    viewport_height: Option<u32>,

    /// Minimum final CSS output height.
    #[arg(long)]
    min_height: Option<u32>,

    /// Maximum final CSS output height.
    #[arg(long)]
    max_height: Option<u32>,

    /// Device pixel scale. Use 2.0 for retina output.
    #[arg(long)]
    scale: Option<f32>,

    /// Viewport preset. This changes width, viewport-height, and scale unless explicitly set.
    #[arg(long, value_enum)]
    profile: Option<RenderProfile>,

    /// Reserved compatibility option; the pure Rust renderer does not load pages.
    #[arg(long)]
    timeout: Option<u64>,

    /// Resource timeout in milliseconds. Overrides --timeout.
    #[arg(long)]
    timeout_ms: Option<u64>,

    /// Deprecated compatibility option; ignored by the pure Rust renderer.
    #[arg(long, hide = true)]
    settle_ms: Option<u64>,

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
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Render(args)) => run_render(*args),
        Some(Command::Profiles) => {
            print_profiles();
            Ok(())
        }
        None => run_render(cli.legacy_render),
    }
}

fn run_render(args: RenderArgs) -> Result<()> {
    let _ignored_settle_ms = args.settle_ms;
    let profile_defaults = args
        .profile
        .map(RenderProfile::defaults)
        .unwrap_or_default();
    let width = args.width.unwrap_or(profile_defaults.width);
    let viewport_height = args
        .viewport_height
        .unwrap_or(profile_defaults.viewport_height);
    let min_height = args.min_height.unwrap_or(1);
    let scale = args.scale.unwrap_or(profile_defaults.scale);
    let html = args
        .html
        .as_ref()
        .context("missing --html; use `mail-canvas render --html email.html --output out.png`")?;
    let output = args
        .output
        .as_ref()
        .context("missing --output; use `mail-canvas render --html email.html --output out.png`")?;
    let font_paths = collect_font_paths(&args.font_files, &args.font_dirs)?;

    let document =
        build_document_from_files(html, args.css.as_deref(), args.base_url.as_deref(), width)?;
    let request = RenderRequest {
        html: document.html,
        width,
        viewport_height,
        min_height,
        scale,
        base_url: document.base_url,
        max_height: args.max_height,
        resource_policy: ResourcePolicy {
            allow_remote: args.allow_remote,
            https_only: !args.allow_http,
            deny_private_networks: !args.allow_private_network,
            timeout: args
                .timeout_ms
                .map(Duration::from_millis)
                .unwrap_or_else(|| Duration::from_secs(args.timeout.unwrap_or(30))),
            max_resource_bytes: args.max_image_bytes,
            max_total_resource_bytes: args.max_total_resource_bytes,
            max_decoded_pixels: args.max_decoded_pixels,
            max_resource_count: args.max_resource_count,
        },
        max_dom_nodes: args.max_dom_nodes,
        max_layout_depth: args.max_layout_depth,
        max_table_cells: args.max_table_cells,
        debug: if args.layout_json.is_some() {
            RenderDebugOptions::layout_dump()
        } else {
            RenderDebugOptions::none()
        },
    };

    let mut renderer = MailCanvasRenderer::with_fonts(
        request.width,
        request.viewport_height,
        request.scale,
        font_paths,
    )?;
    let image = renderer.render_png(request)?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(output, &image.png)
        .with_context(|| format!("failed to write {}", output.display()))?;

    if let Some(path) = &args.warnings_json {
        write_warnings_json(path, &image)?;
    }
    if let Some(path) = &args.layout_json {
        let debug = image
            .debug
            .as_ref()
            .context("renderer did not return a debug snapshot for --layout-json")?;
        write_layout_json(path, debug)?;
    }

    eprintln!(
        "rendered {}x{} CSS px at {}x scale -> {}x{} px ({})",
        image.css_width,
        image.css_height,
        image.scale,
        image.pixel_width,
        image.pixel_height,
        output.display()
    );

    for message in &image.console_messages {
        eprintln!("console.{}: {}", message.level, message.message);
    }

    if let Some(pdf_output) = args.pdf_output {
        let pdf = match args.pdf_mode {
            PdfMode::Raster => raster_pdf_from_png(&image)?,
        };
        if let Some(parent) = pdf_output.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&pdf_output, &pdf)
            .with_context(|| format!("failed to write {}", pdf_output.display()))?;
        eprintln!(
            "rendered raster PDF {}x{} px ({})",
            image.pixel_width,
            image.pixel_height,
            pdf_output.display()
        );
    }

    Ok(())
}

fn print_profiles() {
    println!("generic        width=600 viewport-height=800 scale=1");
    println!("desktop-800    width=800 viewport-height=1200 scale=1");
    println!("thumbnail      width=800 viewport-height=1200 scale=1");
    println!("mobile-375     width=375 viewport-height=812 scale=1");
    println!("mobile-390     width=390 viewport-height=844 scale=1");
    println!("mobile-414     width=414 viewport-height=896 scale=1");
    println!("gmail-ish      width=600 viewport-height=800 scale=1");
    println!("apple-mail-ish width=800 viewport-height=1200 scale=1");
    println!("outlook-ish    width=600 viewport-height=800 scale=1");
    println!("images-blocked width=800 viewport-height=1200 scale=1");
}

fn write_warnings_json(path: &std::path::Path, image: &RenderedImage) -> Result<()> {
    #[derive(serde::Serialize)]
    struct BorrowedDiagnostics<'a> {
        warnings: &'a [mail_canvas_core::RenderWarning],
        assets: &'a [mail_canvas_core::AssetReport],
        console_messages: &'a [mail_canvas_core::ConsoleMessage],
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let report = BorrowedDiagnostics {
        warnings: &image.warnings,
        assets: &image.assets,
        console_messages: &image.console_messages,
    };
    let json = serde_json::to_vec_pretty(&report).context("failed to serialize warnings JSON")?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn write_layout_json(
    path: &std::path::Path,
    debug: &mail_canvas_core::RenderDebugSnapshot,
) -> Result<()> {
    #[derive(serde::Serialize)]
    struct LayoutDump<'a> {
        tree: &'a mail_canvas_core::LayoutNodeSnapshot,
        text_rects: &'a [mail_canvas_core::TextRectSnapshot],
        image_diagnostics: &'a [mail_canvas_core::ImageLayoutDiagnostic],
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tree = debug
        .layout
        .as_ref()
        .context("debug snapshot did not include layout tree")?;
    let json = serde_json::to_vec_pretty(&LayoutDump {
        tree,
        text_rects: &debug.text_rects,
        image_diagnostics: &debug.image_diagnostics,
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
            ["ttf", "otf", "ttc", "otc"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn font_file_extensions_are_case_insensitive_without_normalizing() {
        assert!(is_font_file(Path::new("NotoSans.TTF")));
        assert!(is_font_file(Path::new("NotoSans.otc")));
        assert!(!is_font_file(Path::new("NotoSans.woff2")));
    }

    #[test]
    fn mobile_profiles_use_phone_viewports() {
        let profile = RenderProfile::Mobile390.defaults();
        assert_eq!(profile.width, 390);
        assert_eq!(profile.viewport_height, 844);
        assert_eq!(profile.scale, 1.0);

        let profile = RenderProfile::Mobile414.defaults();
        assert_eq!(profile.width, 414);
        assert_eq!(profile.viewport_height, 896);
        assert_eq!(profile.scale, 1.0);
    }

    #[test]
    fn desktop_and_thumbnail_profiles_share_desktop_viewport() {
        let desktop = RenderProfile::Desktop800.defaults();
        let thumbnail = RenderProfile::Thumbnail.defaults();

        assert_eq!(desktop.width, 800);
        assert_eq!(desktop.viewport_height, 1200);
        assert_eq!(desktop.scale, 1.0);
        assert_eq!(thumbnail.width, desktop.width);
        assert_eq!(thumbnail.viewport_height, desktop.viewport_height);
        assert_eq!(thumbnail.scale, desktop.scale);
    }
}
