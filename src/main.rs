use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::Parser;
use email_render::{EmailRenderer, RenderRequest, RustEmailRenderer, build_document_from_files};

#[derive(Debug, Parser)]
#[command(name = "email-render")]
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

    /// CSS viewport width.
    #[arg(long, default_value_t = 600)]
    width: u32,

    /// Initial CSS viewport height used before full-height measurement.
    #[arg(long, default_value_t = 800)]
    viewport_height: u32,

    /// Minimum final CSS output height.
    #[arg(long, default_value_t = 1)]
    min_height: u32,

    /// Device pixel scale. Use 2.0 for retina output.
    #[arg(long, default_value_t = 1.0)]
    scale: f32,

    /// Reserved compatibility option; the pure Rust renderer does not load pages.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Reserved compatibility option; the pure Rust renderer does not wait for scripts.
    #[arg(long, default_value_t = 100)]
    settle_ms: u64,

    /// Base URL for resolving relative assets. Defaults to the HTML file directory.
    #[arg(long)]
    base_url: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

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
        timeout: Duration::from_secs(args.timeout),
        settle: Duration::from_millis(args.settle_ms),
    };

    let mut renderer =
        RustEmailRenderer::new(request.width, request.viewport_height, request.scale)?;
    let image = renderer.render_png(request)?;

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&args.output, &image.png)
        .with_context(|| format!("failed to write {}", args.output.display()))?;

    eprintln!(
        "rendered {}x{} CSS px at {}x scale -> {}x{} px ({})",
        image.css_width,
        image.css_height,
        image.scale,
        image.pixel_width,
        image.pixel_height,
        args.output.display()
    );

    for message in image.console_messages {
        eprintln!("console.{}: {}", message.level, message.message);
    }

    Ok(())
}
