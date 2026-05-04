# email-render

Pure Rust HTML/CSS to PNG/PDF renderer for email templates.

This is a narrow email renderer, not a small browser. It pre-inlines CSS,
parses HTML, lays out a conservative block/table/text/image subset, shapes text
with `cosmic-text`, and paints with `tiny-skia`. It intentionally does not run
JavaScript or implement browser layout features such as flexbox, grid, floats,
or positioning.

## Usage

```sh
cargo run -- \
  --html examples/basic.html \
  --css examples/basic.css \
  --output out.png \
  --pdf-output out.pdf \
  --width 600
```

Important options:

- `--width`: CSS viewport width for the email, default `600`.
- `--scale`: output pixel scale, default `1.0`. Use `2.0` for retina PNGs.
- `--viewport-height`: compatibility guard for maximum initial allocation checks.
- `--min-height`: minimum final CSS height, default `1`.
- `--max-height`: fail when rendered content exceeds this CSS height.
- `--pdf-output`: optional raster PDF output. This embeds the PNG pipeline result
  as a single-page PDF.
- `--pdf-mode raster`: PDF output mode. Vector PDF is intentionally not exposed
  until the backend can preserve selectable text correctly.
- `--base-url`: base URL for relative images. Defaults to the HTML file
  directory.
- `--allow-remote`: allow remote `http(s)` image resources. Remote resources are
  disabled by default.
- `--allow-http`: allow non-HTTPS remote resources when `--allow-remote` is set.
- `--timeout-ms`: resource timeout in milliseconds. `--timeout` remains as a
  seconds-based compatibility option.
- `--max-image-bytes`: encoded image byte limit, default `10485760`.
- `--max-decoded-pixels`: decoded image pixel limit, default `16000000`.
- `--font-file` / `--font-dir`: load explicit fonts instead of scanning system
  fonts. Directories are scanned non-recursively for `.ttf`, `.otf`, `.ttc`, and
  `.otc`.
- `--settle-ms`: compatibility option; the pure Rust renderer does not wait for
  scripts or page load events.

## Test external templates

```sh
scripts/external_template_smoke.sh
```

The smoke script downloads a small set of open-source transactional email
templates, renders PNG and raster PDF outputs, and writes artifacts under
`/tmp/email-render-external`.

## Current Limits

- PNG output is the primary target. PDF output is raster-only for now, so text is
  not searchable in generated PDFs.
- CSS from `<style>` blocks and `--css` is inlined before rendering. Remote
  stylesheets are disabled.
- Supported layout is the email subset: block flow, nested tables, cells,
  `rowspan`/`colspan`, `col` widths, padding, margin, simple borders,
  backgrounds, text, and images.
- Supported image sources are `data:`, local `file:`, and opt-in remote
  `http(s)` URLs. Failed or blocked images render as placeholders with warnings.
- CSS support is intentionally narrow and email-oriented. It does not implement
  selectors/layout after CSS inlining beyond the inline declarations the layout
  engine understands.
