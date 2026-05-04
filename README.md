# email-render

Pure Rust HTML/CSS to PNG renderer for email templates.

This is a narrow email renderer, not a small browser. It pre-inlines CSS,
parses HTML, lays out a conservative block/table/text subset, shapes text with
`cosmic-text`, and paints PNGs with `tiny-skia`. It intentionally does not run
JavaScript or implement browser layout features such as flexbox, grid, floats,
or positioning.

## Usage

```sh
cargo run -- \
  --html examples/basic.html \
  --css examples/basic.css \
  --output out.png \
  --width 600
```

Important options:

- `--width`: CSS viewport width for the email, default `600`.
- `--scale`: output pixel scale, default `1.0`. Use `2.0` for retina PNGs.
- `--viewport-height`: compatibility guard for maximum initial allocation checks.
- `--min-height`: minimum final CSS height, default `1`.
- `--settle-ms`: compatibility option; the pure Rust renderer does not wait for
  scripts or page load events.

## Current Limits

- PNG output only.
- CSS from `<style>` blocks and `--css` is inlined before rendering. Remote
  stylesheets are disabled.
- Supported layout is the email subset: block flow, nested tables, cells,
  padding, margin, simple borders, backgrounds, text, and image placeholders.
- Images are not fetched/decoded yet; `<img>` elements render as placeholders
  using their width/height.
- Relative asset URLs can still be represented via `<base>` when `--base-url` is
  provided, but network fetching is not part of the renderer yet.
