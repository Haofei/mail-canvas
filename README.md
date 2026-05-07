# MailCanvas

Chrome-free HTML/CSS email template renderer written in Rust. MailCanvas turns
email HTML into PNG and raster PDF output without launching Chromium, WebKit, or
Servo.

MailCanvas is not a browser. It is a focused email rendering engine for
server-side preview, snapshot testing, and template export where memory usage
matters more than full web-platform coverage.

## English

### What It Does

- Parses email HTML with `kuchiki` and folds supported CSS into inline styles.
- Uses `lightningcss` for declaration parsing, media rule handling, CSS values,
  and `@font-face` extraction.
- Lays out the email-oriented subset: block flow, inline text, nested tables,
  table cells, `rowspan`/`colspan`, cellpadding/cellspacing behavior, images,
  background images, a flexbox subset through `taffy`, and basic `float` /
  `clear`.
- Shapes text with `cosmic-text`, loads system fonts or explicit font files, and
  supports a constrained web-font path for common email templates.
- Paints to PNG with `tiny-skia`; optional PDF output embeds the same raster
  result as a single-page PDF.
- Resolves local, `data:`, and opt-in remote resources with byte, timeout, and
  decoded-pixel limits.

### Why Not Chrome

Chromium gives the best fidelity, but it also has a large memory footprint for
high-volume template rendering. MailCanvas keeps the process small by
implementing only the HTML/CSS behavior that email templates usually depend on.
For fidelity work, Chromium/Blink is still used as the oracle through the
Playwright comparison tools in this repository.

### Build

```sh
cargo build
npm install
npx playwright install chromium
```

### CLI Usage

```sh
cargo run -p mail-canvas-cli -- \
  --html examples/basic.html \
  --css examples/basic.css \
  --output out.png \
  --warnings-json warnings.json \
  --pdf-output out.pdf \
  --width 600
```

Important options:

- `--width`: CSS viewport width, default `600`.
- `--scale`: output pixel scale, default `1.0`.
- `--min-height`: minimum final CSS height.
- `--max-height`: fail if rendered content exceeds this CSS height.
- `--warnings-json`: optional JSON diagnostics output path for structured
  renderer warnings and asset reports.
- `--layout-json`: optional JSON layout dump output path for MailCanvas box
  geometry and text rects.
- `--pdf-output`: optional raster PDF output path.
- `--base-url`: base URL for relative assets; defaults to the HTML file
  directory.
- `--allow-remote`: allow remote `http(s)` images and fonts.
- `--allow-http`: allow non-HTTPS remote resources when remote loading is
  enabled.
- `--timeout-ms`: resource timeout in milliseconds.
- `--max-image-bytes`: encoded image byte limit.
- `--max-total-resource-bytes`: aggregate encoded byte limit across all fetched assets.
- `--max-resource-count`: aggregate fetched asset count limit.
- `--max-decoded-pixels`: decoded image pixel limit.
- `--allow-private-network`: allow localhost/private IP fetches when remote loading is enabled.
- `--max-dom-nodes`: maximum DOM nodes accepted before rendering.
- `--max-layout-depth`: maximum nested layout depth before truncating nested
  content with a structured warning.
- `--max-table-cells`: maximum expanded table cell slots accepted during table
  layout.
- `--font-file` / `--font-dir`: load explicit fonts instead of scanning system
  fonts.

The diagnostics JSON currently includes:

- `warnings`: machine-readable renderer warnings
- `assets`: every attempted stylesheet, image, and web font load with final
  status
- `console_messages`: the compatibility stderr messages printed by the CLI
- `image_diagnostics`: image/background intrinsic size, target rect, and crop
  diagnostics

### Developer Preview, Diff, Snapshot, and Check

The product-facing developer tools live in `scripts/mail_canvas_tools.mjs` and
wrap the native CLI without adding Chromium to the MailCanvas render path.

Render once, or run a lightweight local preview server that re-renders when the
HTML directory changes:

```sh
npm run preview -- examples/basic.html
npm run preview -- examples/basic.html --watch --port 4177
```

Developer tool commands accept `--profile generic`, `--profile desktop-800`,
`--profile mobile-375`, `--profile thumbnail`, `--profile gmail-ish`,
`--profile apple-mail-ish`, `--profile outlook-ish`, and
`--profile images-blocked`. These are practical product profiles, not exact
client emulators. The `*-ish` and `images-blocked` profiles select useful
viewports and add conservative diagnostics in `check`. Explicit `--width`,
`--viewport-height`, and `--scale` values override the selected profile.

Generate a MailCanvas-only before/after visual diff:

```sh
npm run diff -- before.html after.html --out /tmp/mail-canvas-diff
```

This writes `before.png`, `after.png`, `diff.png`, `side-by-side.png`,
`report.json`, and `report.md`.

Create or check local snapshot baselines for CI:

```sh
npm run snapshot -- "templates/**/*.html" --baseline snapshots --update
npm run snapshot -- "templates/**/*.html" --baseline snapshots
```

Run a fast diagnostics check for one template:

```sh
npm run check -- examples/basic.html --warnings-json /tmp/basic.warnings.json
```

`check` writes a combined QA report with renderer warnings, asset failures,
HTML-size warnings, missing image alt text, empty links, linked stylesheet
notices, and profile compatibility diagnostics. It exits non-zero when renderer
warnings or failed assets are present. Use the Playwright comparison commands
below when you need Chromium as the oracle; use these tools when you need fast
local rendering, MailCanvas-only diffs, or snapshot gating.

The repository also exposes a composite GitHub Action:

```yaml
- uses: Haofei/mail-canvas@main
  with:
    command: snapshot
    patterns: "templates/**/*.html"
    baseline: snapshots
    profile: desktop-800
```

Resource policy defaults:

- per-resource bytes: `10 MiB`
- total fetched bytes per render: `64 MiB`
- max fetched resource count: `128`
- decoded image pixels: `16,000,000`
- remote loading: disabled by default
- private-network access: denied by default when remote loading is enabled

### Rust API

```rust
use mail_canvas_core::{EmailRenderer, RenderRequest};
use mail_canvas_native::MailCanvasRenderer;

fn main() -> anyhow::Result<()> {
    let html = "<table><tr><td>Hello</td></tr></table>".to_string();
    let request = RenderRequest::defaults_for_html(html, 600, 800, 1.0)
        .with_max_height(Some(8000));
    let mut renderer = MailCanvasRenderer::new(600, 800, 1.0)?;
    let image = renderer.render_png(request)?;
    std::fs::write("out.png", image.png)?;
    Ok(())
}
```

`RustEmailRenderer` and `ServoEmailRenderer` remain as compatibility aliases for
older local experiments.

### WASM API

The wasm crate is now a real browser-facing shell. It does not fetch by itself.
The intended flow is:

1. JS fetches remote stylesheets, images, or fonts.
2. JS calls `register_asset(url, bytes)` for every fetched resource.
3. JS optionally calls `register_font(bytes)` for bundled fallback fonts.
4. JS renders with `render_png_with_base_url(...)` or
   `render_rgba_with_base_url(...)`.
5. JS reads `diagnostics_json()` for warnings, blocked assets, and load results.

Minimal sketch:

```js
const renderer = new WasmRenderer();

const logoBytes = new Uint8Array(await (await fetch("https://cdn.example.com/logo.png")).arrayBuffer());
renderer.register_asset("https://cdn.example.com/logo.png", logoBytes);

const png = renderer.render_png_with_base_url(
  '<img src="./logo.png" width="120" alt="">',
  600,
  800,
  1.0,
  "https://cdn.example.com/email.html"
);

const diagnostics = JSON.parse(renderer.diagnostics_json());
```

Current wasm boundary:

- supported: HTML input, registered font bytes, `data:` URLs, pre-registered
  stylesheet/image/font assets, relative URL resolution through `base_url`
- not supported yet: direct wasm-side fetch, PDF output, automatic remote asset
  loading

The browser demo now runs in a worker and uses explicit asset injection:

1. Main thread fetches stylesheet/image/font bytes.
2. Main thread posts those bytes to `browser/mail-canvas-worker.js`.
3. The worker registers assets in `mail-canvas-wasm`.
4. The worker returns PNG bytes and diagnostics JSON.

The generated `browser/pkg/` wasm bundle is intentionally ignored by Git.
Rebuild it with `npm run build:browser` or start the full local demo with
`npm run demo`.

### Project Shape

- `crates/mail-canvas-core/`: parse, style, layout, paint model, diagnostics,
  and resource/font/output traits. No filesystem, HTTP, CLI, or system-font
  scanning.
- `crates/mail-canvas-native/`: native resource loading, filesystem helpers,
  system font discovery, PNG output, and raster PDF output.
- `crates/mail-canvas-wasm/`: `wasm-bindgen` wrapper with a minimal
  `HTML + registered fonts + data:image` rendering path for browser workers.
- `crates/mail-canvas-cli/`: CLI wrapper around the native renderer.
- `scripts/`: Chromium comparison, layout dump, template corpus, and Blink
  reference helpers.

Inside `mail-canvas-core`, the large renderer implementation is physically split
across:

- `src/layout.rs`
- `src/style.rs`
- `src/paint.rs`

The crate root still owns shared helpers and tests, but the renderer is no
longer a single-file implementation.

### Fidelity Workflow

Run the fixed Playwright semantic visual regression set:

```sh
npm run test:visual
```

This gate uses Chromium as the reference, but it does not require strict total
pixel equality. The pass/fail checks are semantic and tolerant:

- renderer warnings must stay at zero for the fixed regression set;
- viewport width and rendered height must stay within configured semantic
  tolerances;
- text, media, and non-text/non-media regions must remain within coarse diff
  limits that catch missing content or major placement regressions; media checks
  also use an absolute pixel tolerance so small anti-aliased logos are not
  over-penalized;
- reports include `Media Rect Δ` so image/background placement can be judged
  separately from JPEG/PNG resampling differences;
- total pixel diff is still reported for investigation, but it is observational
  unless `maxTotalDiffPercent` is explicitly configured.

Run the full golden corpus comparison for diagnostics:

```sh
npm run compare:corpus
```

Run only the modern editor/generated marketing corpus:

```sh
npm run compare:editors
```

This filters the local, vendored corpus to Beefree, Stripo, and MJML templates.
It is intended for renderer-fidelity work on valid generated email HTML, not
legacy hand-written compatibility fixtures.

Download a small authorized Really Good Emails sample and compare it:

```sh
RGE_EMAIL=you@example.com RGE_PASSWORD='...' npm run corpus:rge -- \
  --category promotional \
  --limit 12 \
  --replace-provider \
  --login
npm run compare:rge
```

`corpus:rge` uses Playwright to collect public detail pages from
`reallygoodemails.com`, extracts the raw email HTML from the page payload, and
mirrors linked assets into `corpus/reallygoodemails/` so the regression run does
not depend on external assets later. Credentials are optional for currently
public samples and are read only from environment variables; `.rge-auth/` is
ignored by Git. The scheduled `reallygoodemails-regression` workflow runs daily
when enabled and uploads the vendored sample plus diff artifacts.

Compare one local real-world HTML file, such as an exported editor template:

```sh
npm run compare:local -- \
  --html ./cnn.html \
  --name cnn-local
```

The local compare path uses the same Chromium screenshot, MailCanvas render,
diff, side-by-side image, diagnostics, and `layout-json` artifact generation as
the corpus runs. Outputs default to `/tmp/mail-canvas-playwright-local`; pass
`--work-dir` after `--` when you want a different directory. Local one-off
templates are not committed by default, especially when they include private
preview links or email addresses.

The corpus lives in `scripts/templates.mjs` as structured metadata: provider,
category, status, expected warning count, and the reason for any known warning.
Known-warning templates are skipped in the broad corpus unless they are
explicitly listed in `scripts/playwright_expectations.json`; this keeps broken
upstream image URLs and unfilled template-variable images out of the pass rate.

The committed corpus index is also exported to:

- `corpus/manifest.json`

Refresh it with:

```sh
npm run corpus:manifest
```

Artifacts are written under the command's `/tmp/mail-canvas-*` work directory,
including browser screenshots, MailCanvas screenshots, diff images,
side-by-side images, `comparison.json`, `comparison.report.json`, and
`report.md`.

`comparison.report.json` is the machine-readable summary intended for CI and
artifact upload.

For detailed layout investigation:

```sh
npm run layout:chrome -- \
  --template colorlib-template-1 \
  --selector '.email-section, .text-services, td, img' \
  --y 2066 \
  --out /tmp/colorlib-1-layout.json
```

MailCanvas can dump its own layout tree too:

```sh
cargo run -p mail-canvas-cli -- \
  --html examples/basic.html \
  --output /tmp/out.png \
  --layout-json /tmp/mail-canvas-layout.json
```

And you can compare Chrome vs MailCanvas rects:

```sh
npm run layout:compare -- \
  --browser /tmp/colorlib-1-layout.json \
  --rust /tmp/mail-canvas-layout.json
```

The Playwright comparison report now also includes:

- `firstBadRegion`: the first 100px horizontal band where the diff spikes
- `textCoverage`: text ink coverage delta
- `textRects`: text position/wrap delta
- `textPixel`: text rasterization delta

To download the pinned Blink reference subset locally:

```sh
scripts/fetch_blink_reference.sh
```

`blink-reference/` is ignored by Git. Use it as an algorithm reference only:
capture Chromium behavior first, read the matching Blink module, then implement
the smallest email-relevant rule in Rust.

### Current Limits

- PNG is the primary output. PDF output is raster-only, so text is not
  selectable or searchable.
- CSS support is intentionally narrow and tied to email templates. Unsupported
  declarations are ignored today; structured renderer warnings report resource,
  web font, and layout-limit issues. Diagnostics JSON also includes per-asset
  load results.
- JavaScript, forms, video, canvas, full positioning, full flex/grid, and full
  browser painting are out of scope.
- Remote resources are disabled by default and must be enabled explicitly. DOM,
  layout depth, table cell, encoded byte, and decoded pixel limits are enforced
  by default.
- Visual fidelity is measured against Chromium with semantic tolerances. Strict
  total pixel equality is not required because text rasterization differs between
  Chromium/Skia and the pure Rust text stack.
- The fixed Playwright regression suite currently passes. Total pixel diff is
  reported as a diagnostic signal, while the gate focuses on content presence,
  layout stability, media regions, and non-text/non-media structure.

### Supported CSS Matrix

| Area | Supported | Notes |
|---|---|---|
| Block flow | `display:block`, margins, padding, borders, background color/image | Email-oriented subset only |
| Inline text | `font-*`, `line-height`, `letter-spacing`, `text-align`, `text-transform`, `white-space:nowrap` | Uses `cosmic-text`, not Skia |
| Tables | nested tables, `rowspan`, `colspan`, `cellpadding`, `cellspacing`, `table-layout:fixed`, `col` width hints | Primary modern email target |
| Images | `img`, `background-image`, `object-fit`, `object-position`, width/height attributes | Remote and `data:` assets supported |
| Media queries | `screen`/`all`, `only`, `not`, `min/max-width`, width ranges, `orientation` | Active rules are expanded in source order before inlining |
| Flex subset | `display:flex`, direction, wrap, align/justify, gap | Only common email-safe subset |
| Float subset | `float:left/right`, `clear`, basic wrap avoidance | Supported for modern templates only |
| Positioning | static, relative, absolute/fixed child placement | No full browser stacking model |
| Unsupported / partial | JS, forms, video, canvas, grid, VML/MSO, malformed table DOM repair, legacy hybrid hacks | Out of scope |

### Deterministic Regression Fonts

The full Playwright corpus comparison uses the host system fonts by default so
the Chromium reference and MailCanvas resolve common email families such as
Arial through the same local font stack. This gives better
Beefree/marketing-template fidelity on developer machines and CI images with
compatible system fonts.

Committed font fixtures remain available for explicit deterministic runs:

- `fixtures/fonts/Arimo-Regular.ttf`
- `fixtures/fonts/Arimo-Bold.ttf`
- `fixtures/fonts/Tinos-Regular.ttf`
- `fixtures/fonts/Tinos-Bold.ttf`
- `fixtures/fonts/NotoSans-Regular.ttf`
- `fixtures/fonts/NotoSans-Bold.ttf`
- `fixtures/fonts/NotoSansMath-Regular.ttf`

Arimo is used as the Arial-compatible Latin sans fixture for editor-generated
marketing templates; Tinos is used as the Times-compatible serif fixture for
editor templates that fall back to `serif`; Noto Sans remains the broader
fallback fixture. Noto Sans Math covers common symbols and arrows that editor
templates often place in CTA labels.

`npm run test:visual` passes `--fixture-fonts` so GitHub Actions
does not depend on the Linux runner's installed fonts. Pass `--fixture-fonts` to
`scripts/playwright_compare.mjs` for other host-independent runs when stable
wrapping is more important than matching Chromium's local font fallback.

### Examples

- Node wrapper: `examples/node-render.mjs`
- HTTP service: `examples/http-service.mjs`

Both examples currently shell out to the native CLI instead of embedding a Node
native module. That keeps the example path simple while the Rust public API
stabilizes.

Run the HTTP service locally:

```sh
cargo build -p mail-canvas-cli
npm run serve:http
curl -sS http://127.0.0.1:8787/render \
  -H 'content-type: application/json' \
  --data '{"html":"<table><tr><td>Hello</td></tr></table>","width":600}' \
  --output email.png
```

The service supports `POST /render` and `GET /healthz`. `POST /render` returns
PNG by default, or JSON with `pngBase64` and diagnostics when the request body
sets `"output":"json"` or the `Accept` header requests JSON. The Dockerfile
builds the release CLI and runs this service:

```sh
docker build -t mail-canvas .
docker run --rm -p 8787:8787 mail-canvas
```

### Browser Thumbnail API

The browser-oriented wrapper lives in `browser/mail-canvas-browser.js`, with
types in `browser/mail-canvas-browser.d.ts`:

```js
import { createMailCanvasRenderer } from "./browser/mail-canvas-browser.js";

const renderer = await createMailCanvasRenderer({
  workerUrl: new URL("./browser/mail-canvas-worker.js", import.meta.url),
  fonts: ["./assets/NotoSans-Regular.ttf", "./assets/NotoSans-Bold.ttf"],
  limits: {
    maxAssetBytes: 10 * 1024 * 1024,
    maxTotalAssetBytes: 64 * 1024 * 1024,
    maxAssetCount: 128,
  },
});

const result = await renderer.renderThumbnail({
  html,
  width: 800,
  height: 1200,
  scale: 1,
  baseUrl: window.location.href,
});

renderer.destroy();
```

API responsibilities:

- `createMailCanvasRenderer(options)` creates a worker-backed renderer and
  registers the provided font files or font bytes.
- `renderThumbnail({ html, width, height, scale, baseUrl })` returns a PNG
  `Uint8Array`, a `Blob`, normalized diagnostics, asset summary, and timing.
- Linked assets are fetched in the browser, de-duplicated by absolute URL, and
  injected into the worker before render; stylesheet links are converted to
  `<style>` blocks so the WASM path does not perform network fetches.
- `limits.maxAssetBytes`, `limits.maxTotalAssetBytes`, and
  `limits.maxAssetCount` bound browser-side resource collection before bytes are
  sent to WASM.
- `clearCache()` releases wrapper asset cache and worker-registered assets.
- `destroy()` terminates the worker and should be called when the renderer is no
  longer needed.

The demo app uses this wrapper instead of calling raw wasm-bindgen APIs
directly. Treat the wrapper as the browser/WASM thumbnail product surface; the
raw `WasmRenderer` binding remains a lower-level implementation detail.

### Memory Benchmark

Compare one corpus template against Chromium:

```sh
npm run benchmark:memory -- --template colorlib-template-1 --out /tmp/mail-canvas-benchmark.json
```

This writes RSS and elapsed-time measurements for:

- `mail-canvas` CLI
- Chromium screenshot capture through Playwright

Use the built-in repeated-image case to stress decoded image reuse and image
buffer sharing:

```sh
npm run benchmark:memory -- --case repeated-image --width 800 --out /tmp/mail-canvas-repeated-image.json
```

This synthetic case repeats the same large PNG many times in one email, so it is
useful for catching regressions in per-render image caching and clone behavior.

Use the fixed thumbnail case for the production-style `800x1200` preview path:

```sh
npm run benchmark:thumbnail -- --out /tmp/mail-canvas-thumbnail-800x1200.json
```

This case uses a local `800x1200` marketing email with one large hero image and
no remote resources. The benchmark builds the release CLI and compares it
against Chromium screenshot capture through Playwright.

Use the browser wrapper benchmark to exercise the worker/WASM product API:

```sh
npm run benchmark:wasm-thumbnail -- --out /tmp/mail-canvas-wasm-thumbnail.json --markdown-out /tmp/mail-canvas-wasm-thumbnail.md
```

This builds `browser/pkg`, starts the local demo server, calls
`createMailCanvasRenderer()` and `renderThumbnail()` from Playwright, then writes
JSON and optional Markdown timing output.

### Development Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
npm run test:tools
npm run test:wasm-thumbnail
npm run test:visual
```

## 中文

### 这是做什么的

MailCanvas 是一个不用 Chrome 的 Rust 邮件模板渲染器。它把 email HTML/CSS
渲染成 PNG，也可以输出栅格 PDF。目标场景是服务端预览、截图测试、模板导出，
尤其适合不能长期启动 Chromium 的环境。

它不是一个小浏览器，而是只实现邮件模板常用的 HTML/CSS 子集。

### 主要能力

- 用 `kuchiki` 解析 HTML，并把支持的 CSS 合并到 inline style。
- 用 `lightningcss` 解析 CSS declaration、媒体规则、CSS value 和
  `@font-face`。
- 支持邮件常见布局：block flow、inline text、嵌套 table、table cell、
  `rowspan` / `colspan`、cellpadding/cellspacing、图片、背景图、部分 flex
  布局，以及基础 `float` / `clear`。
- 用 `taffy` 处理 flex 子集。
- 用 `cosmic-text` 排版文字，支持系统字体、指定字体文件，以及受限的 web
  font 加载路径。
- 用 `tiny-skia` 绘制 PNG；PDF 输出复用同一份栅格结果。
- 支持本地资源、`data:` URL、可选远程资源，并带有超时、大小和像素数限制。

### 为什么不用 Chrome

Chrome 的效果最好，但内存占用高，批量渲染邮件模板时成本很明显。MailCanvas
选择只实现邮件模板需要的规则，把内存和部署复杂度降下来。对齐浏览器行为时，
仓库里的 Playwright 工具仍然会用 Chromium/Blink 作为参考标准。

### 构建

```sh
cargo build
npm install
npx playwright install chromium
```

### 命令行使用

```sh
cargo run -p mail-canvas-cli -- \
  --html examples/basic.html \
  --css examples/basic.css \
  --output out.png \
  --warnings-json warnings.json \
  --pdf-output out.pdf \
  --width 600
```

常用参数：

- `--width`: CSS viewport 宽度，默认 `600`。
- `--scale`: 输出像素倍率，默认 `1.0`。
- `--min-height`: 最小 CSS 输出高度。
- `--max-height`: 内容超过该 CSS 高度时失败。
- `--warnings-json`: 可选的 JSON diagnostics 输出，包含结构化 renderer
  warnings 和 asset report。
- `--layout-json`: 可选的 MailCanvas layout dump JSON 输出，包含 box 几何和
  text rect。
- `--pdf-output`: 可选的栅格 PDF 输出路径。
- `--base-url`: 相对资源的 base URL，默认是 HTML 文件目录。
- `--allow-remote`: 允许远程 `http(s)` 图片和字体。
- `--allow-http`: 开启远程资源后，允许非 HTTPS。
- `--timeout-ms`: 资源加载超时，单位毫秒。
- `--max-image-bytes`: 编码后图片字节限制。
- `--max-total-resource-bytes`: 单次渲染所有外部资源累计字节限制。
- `--max-resource-count`: 单次渲染允许拉取的外部资源总数。
- `--max-decoded-pixels`: 解码后图片像素数限制。
- `--allow-private-network`: 开启远程资源后，允许访问 localhost / 私网地址。
- `--max-dom-nodes`: 渲染前允许的最大 DOM node 数。
- `--max-layout-depth`: 最大嵌套 layout 深度，超过后截断嵌套内容并输出结构化
  warning。
- `--max-table-cells`: table layout 中允许展开的最大 cell slot 数。
- `--font-file` / `--font-dir`: 使用指定字体，避免扫描系统字体。

当前 diagnostics JSON 包含：

- `warnings`: 机器可读的 renderer warning
- `assets`: 每一次 stylesheet、image、web font 加载的最终状态
- `console_messages`: CLI 为兼容性保留的 stderr 文本消息
- `image_diagnostics`: image/background 的 intrinsic size、目标 rect 和 crop
  诊断

### 本地预览、Diff、Snapshot 和 Check

面向日常开发的工具入口在 `scripts/mail_canvas_tools.mjs`。它复用 native CLI，
不会把 Chromium 加入 MailCanvas 的渲染链路。

单次渲染预览，或启动一个轻量本地预览服务，在 HTML 目录变化时自动重新渲染：

```sh
npm run preview -- examples/basic.html
npm run preview -- examples/basic.html --watch --port 4177
```

这些工具命令支持 `--profile generic`、`--profile desktop-800`、
`--profile mobile-375`、`--profile thumbnail`、`--profile gmail-ish`、
`--profile apple-mail-ish`、`--profile outlook-ish` 和
`--profile images-blocked`。这些是实用产品 profile，不是精确客户端模拟器。
`*-ish` 和 `images-blocked` profile 会选择合适的 viewport，并在 `check` 里增加
保守的兼容性诊断。显式传入的 `--width`、`--viewport-height` 和 `--scale` 会覆盖
profile。

生成 MailCanvas-only 的 before/after 视觉 diff：

```sh
npm run diff -- before.html after.html --out /tmp/mail-canvas-diff
```

输出包括 `before.png`、`after.png`、`diff.png`、`side-by-side.png`、
`report.json` 和 `report.md`。

为 CI 创建或检查本地 snapshot baseline：

```sh
npm run snapshot -- "templates/**/*.html" --baseline snapshots --update
npm run snapshot -- "templates/**/*.html" --baseline snapshots
```

快速检查一个模板的 renderer warnings 和资源失败：

```sh
npm run check -- examples/basic.html --warnings-json /tmp/basic.warnings.json
```

`check` 会写出合并 QA report，包含 renderer warnings、资源失败、HTML 体积、
缺失图片 alt、空链接、外链 stylesheet 提示和 profile 兼容性诊断。出现 renderer
warning 或资源加载失败时返回非零退出码。需要 Chromium 作为 oracle 时继续用下面的
Playwright 对比命令；需要快速本地渲染、MailCanvas 内部 diff 或 snapshot gate
时用这些工具。

仓库也提供 composite GitHub Action：

```yaml
- uses: Haofei/mail-canvas@main
  with:
    command: snapshot
    patterns: "templates/**/*.html"
    baseline: snapshots
    profile: desktop-800
```

### Rust API

```rust
use mail_canvas_core::{EmailRenderer, RenderRequest};
use mail_canvas_native::MailCanvasRenderer;

fn main() -> anyhow::Result<()> {
    let html = "<table><tr><td>Hello</td></tr></table>".to_string();
    let request = RenderRequest::defaults_for_html(html, 600, 800, 1.0)
        .with_max_height(Some(8000));
    let mut renderer = MailCanvasRenderer::new(600, 800, 1.0)?;
    let image = renderer.render_png(request)?;
    std::fs::write("out.png", image.png)?;
    Ok(())
}
```

`RustEmailRenderer` 和 `ServoEmailRenderer` 暂时保留为兼容别名。

### WASM API

wasm crate 现在已经是独立的浏览器侧壳，但它本身不做 fetch。推荐链路是：

1. JS 先拉远程 stylesheet、image、font。
2. JS 对每个资源调用 `register_asset(url, bytes)`。
3. 如果要带兜底字体，额外调用 `register_font(bytes)`。
4. 用 `render_png_with_base_url(...)` 或 `render_rgba_with_base_url(...)`
   渲染。
5. 用 `diagnostics_json()` 读取 warning、blocked asset 和加载结果。

最小示意：

```js
const renderer = new WasmRenderer();

const logoBytes = new Uint8Array(await (await fetch("https://cdn.example.com/logo.png")).arrayBuffer());
renderer.register_asset("https://cdn.example.com/logo.png", logoBytes);

const png = renderer.render_png_with_base_url(
  '<img src="./logo.png" width="120" alt="">',
  600,
  800,
  1.0,
  "https://cdn.example.com/email.html"
);

const diagnostics = JSON.parse(renderer.diagnostics_json());
```

当前 wasm 边界：

- 已支持：HTML 输入、注册字体字节、`data:` URL、预注册的
  stylesheet/image/font 资源、通过 `base_url` 解析相对 URL
- 暂不支持：wasm 内直接 fetch、PDF 输出、自动远程资源加载

浏览器 demo 现在已经改成 worker 模式：

1. 主线程负责 fetch stylesheet / image / font；
2. 主线程把字节通过 `postMessage` 发给 `browser/mail-canvas-worker.js`；
3. worker 在 `mail-canvas-wasm` 里注册资源并渲染；
4. worker 返回 PNG 字节和 diagnostics JSON。

生成出来的 `browser/pkg/` wasm bundle 不进入 Git。需要时用
`npm run build:browser` 重新生成，或者直接用 `npm run demo` 启动本地 demo。

### 项目结构

- `crates/mail-canvas-core/`: parse、style、layout、paint model、diagnostics，
  以及 resource/font/output trait。这里不做文件读取、HTTP、CLI 或系统字体扫描。
- `crates/mail-canvas-native/`: native 资源加载、文件系统 helper、系统字体发现、
  PNG 输出和 raster PDF 输出。
- `crates/mail-canvas-wasm/`: `wasm-bindgen` 封装，先支持浏览器 worker 里的最小
  `HTML + 注册字体 + data:image` 渲染链路。
- `crates/mail-canvas-cli/`: 基于 native renderer 的 CLI。
- `scripts/`: Chromium 对比、布局 dump、模板语料和 Blink 参考代码工具。

语料索引会导出到：

- `corpus/manifest.json`

可用下面命令刷新：

```sh
npm run corpus:manifest
```

### 对比和调试

固定 Playwright 语义视觉回归集：

```sh
npm run test:visual
```

这个 gate 仍然使用 Chromium 作为参考，但不要求总像素完全一致。真正的通过条件
是更宽容的语义检查：

- 固定回归集里的 renderer warnings 必须为 0；
- viewport 宽度和最终渲染高度必须落在语义容差内；
- 文字、媒体、非文字非媒体区域必须落在粗粒度 diff 限制内，用来发现内容缺失
  或明显布局错误；媒体检查也带绝对像素容差，避免小图标抗锯齿差异被过度惩罚；
- 报告会输出 `Media Rect Δ`，把图片/背景的位置和尺寸差异从图片内部重采样差异里
  拆出来看；
- total pixel diff 仍会输出，方便排查，但除非显式配置 `maxTotalDiffPercent`，
  否则不作为失败条件。

跑完整 golden corpus 对比（诊断用，不作为必须通过 gate）：

```sh
npm run compare:corpus
```

只跑现代编辑器/生成器模板：

```sh
npm run compare:editors
```

这个命令只筛选本地 vendored 语料里的 Beefree、Stripo 和 MJML 模板，主要用于
合法生成 HTML 的渲染质量迭代，不针对老旧手写兼容 hack。

下载一小批授权的 Really Good Emails 样本并对比：

```sh
RGE_EMAIL=you@example.com RGE_PASSWORD='...' npm run corpus:rge -- \
  --category promotional \
  --limit 12 \
  --replace-provider \
  --login
npm run compare:rge
```

`corpus:rge` 会用 Playwright 收集 `reallygoodemails.com` 的公开 detail 页面，
从页面 payload 中提取原始 email HTML，并把 linked assets 镜像到
`corpus/reallygoodemails/`，这样后续回归不依赖外部资源。账号密码只从环境变量
读取；`.rge-auth/` 不进入 Git。`reallygoodemails-regression` workflow 可以每天
跑一次，并上传下载到的样本和 diff artifacts。

对比一个本地真实 HTML 文件，比如从编辑器导出的模板：

```sh
npm run compare:local -- \
  --html ./cnn.html \
  --name cnn-local
```

本地模板对比和语料对比使用同一套 Chromium 截图、MailCanvas 渲染、diff、
side-by-side、diagnostics 和 `layout-json` 输出。默认输出目录是
`/tmp/mail-canvas-playwright-local`；如果要换目录，可以在 `--` 后面传
`--work-dir`。一次性的真实模板默认不提交，尤其是包含私有预览链接或邮箱地址时。

`scripts/templates.mjs` 现在是带 metadata 的语料：provider、category、status、
expected warning count，以及 known warning 的原因。全量语料里，known-warning
模板会先跳过，除非它被显式列在 `scripts/playwright_expectations.json`；这样上游
已经失效的图片链接和模板变量图片不会污染通过率。

输出会在对应命令的 `/tmp/mail-canvas-*` 工作目录，里面有浏览器截图、
MailCanvas 截图、diff 图、side-by-side 图、`comparison.json`、
`comparison.report.json` 和 `report.md`。

查看 Chromium 的具体布局：

```sh
npm run layout:chrome -- \
  --template colorlib-template-1 \
  --selector '.email-section, .text-services, td, img' \
  --y 2066 \
  --out /tmp/colorlib-1-layout.json
```

MailCanvas 自己也可以输出 layout tree：

```sh
cargo run -p mail-canvas-cli -- \
  --html examples/basic.html \
  --output /tmp/out.png \
  --layout-json /tmp/mail-canvas-layout.json
```

然后直接对比 Chrome 和 MailCanvas 的 rect：

```sh
npm run layout:compare -- \
  --browser /tmp/colorlib-1-layout.json \
  --rust /tmp/mail-canvas-layout.json
```

现在 Playwright 对比报告还会额外给出：

- `firstBadRegion`: 第一个 diff 明显升高的 100px 横向 band
- `textCoverage`: 文字墨水覆盖差异
- `textRects`: 文字位置/换行差异
- `textPixel`: 文字栅格化差异

下载固定版本的 Blink 参考代码：

```sh
scripts/fetch_blink_reference.sh
```

`blink-reference/` 不进入 Git。它只用于理解算法：先用 Chromium 抓布局和样式，
再看 Blink 对应模块，然后在 Rust 里实现最小的邮件相关规则，不复制 Blink
源码。

### 当前限制

- PNG 是主要输出；PDF 目前是栅格 PDF，文字不可选中、不可搜索。
- CSS 支持是邮件模板导向的子集；暂时不支持的 declaration 会被忽略，
  结构化 renderer warnings 已覆盖资源、web font 和 layout limit 问题。
  diagnostics JSON 还会输出逐个 asset 的加载结果。
- `<style>` 中的 `@media` 会在 CSS inline 前按当前 viewport 求值并按原始顺序展开；
  已覆盖 `screen`/`all`、`only`、`not`、`min/max-width`、宽度 range 和
  `orientation`。
- JavaScript、form、video、canvas、完整 positioning、完整 flex/grid、完整浏览器
  painting 都不在当前范围内。
- 远程资源默认关闭，需要显式开启。DOM、layout depth、table cell、编码字节数和
  解码像素数限制默认开启。
- 视觉效果以 Chromium 为参考，但采用语义化容差；由于 Chromium/Skia 和纯 Rust
  文本栈的文字栅格化不同，不要求 total pixel diff 完全一致。
- 固定 Playwright 回归集目前可以通过。total pixel diff 作为诊断信号保留，gate
  主要关注内容是否存在、布局是否稳定、媒体区域和非文字非媒体结构是否正确。

### 内存 Benchmark

对比一个 corpus 模板和 Chromium：

```sh
npm run benchmark:memory -- --template colorlib-template-1 --out /tmp/mail-canvas-benchmark.json
```

重复大图场景用于验证单次 render 内的图片解码缓存和像素 buffer 共享：

```sh
npm run benchmark:memory -- --case repeated-image --width 800 --out /tmp/mail-canvas-repeated-image.json
```

固定缩略图场景用于验证实际会用到的 `800x1200` 预览路径：

```sh
npm run benchmark:thumbnail -- --out /tmp/mail-canvas-thumbnail-800x1200.json
```

这个 case 使用本地营销邮件、一张大 hero 图、无远程资源，并用 release CLI 对比
Playwright/Chromium 截图。

浏览器 wrapper benchmark 会走 worker/WASM 产品 API：

```sh
npm run benchmark:wasm-thumbnail -- --out /tmp/mail-canvas-wasm-thumbnail.json --markdown-out /tmp/mail-canvas-wasm-thumbnail.md
```

它会构建 `browser/pkg`，启动本地 demo server，然后在 Playwright 页面里调用
`createMailCanvasRenderer()` 和 `renderThumbnail()`。

### HTTP Service 和 Docker

本地启动 HTTP service：

```sh
cargo build -p mail-canvas-cli
npm run serve:http
curl -sS http://127.0.0.1:8787/render \
  -H 'content-type: application/json' \
  --data '{"html":"<table><tr><td>Hello</td></tr></table>","width":600}' \
  --output email.png
```

service 支持 `POST /render` 和 `GET /healthz`。`POST /render` 默认返回 PNG；
如果请求体里设置 `"output":"json"`，或者 `Accept` header 请求 JSON，则返回
包含 `pngBase64` 和 diagnostics 的 JSON。Dockerfile 会构建 release CLI 并运行
这个 service：

```sh
docker build -t mail-canvas .
docker run --rm -p 8787:8787 mail-canvas
```

### 浏览器 Thumbnail API

浏览器产品层 wrapper 位于 `browser/mail-canvas-browser.js`，TypeScript 类型位于
`browser/mail-canvas-browser.d.ts`。

```js
const renderer = await createMailCanvasRenderer({
  workerUrl: new URL("./browser/mail-canvas-worker.js", import.meta.url),
  fonts: ["./assets/NotoSans-Regular.ttf", "./assets/NotoSans-Bold.ttf"],
  limits: {
    maxAssetBytes: 10 * 1024 * 1024,
    maxTotalAssetBytes: 64 * 1024 * 1024,
    maxAssetCount: 128,
  },
});

const result = await renderer.renderThumbnail({
  html,
  width: 800,
  height: 1200,
  scale: 1,
  baseUrl: window.location.href,
});
```

这个 wrapper 负责 worker 生命周期、字体注册、链接资源抓取、URL 去重、资源限制和
diagnostics 解析。

API 边界：

- `createMailCanvasRenderer(options)` 创建 worker-backed renderer，并注册传入的
  font 文件或 font bytes。
- `renderThumbnail({ html, width, height, scale, baseUrl })` 返回 PNG
  `Uint8Array`、`Blob`、标准化 diagnostics、asset summary 和 timing。
- 链接资源在浏览器侧 fetch，按绝对 URL 去重后注入 worker；stylesheet link 会转成
  `<style>`，WASM 路径本身不做网络请求。
- `limits.maxAssetBytes`、`limits.maxTotalAssetBytes`、`limits.maxAssetCount`
  在资源进入 WASM 前限制浏览器侧收集的资源规模。
- `clearCache()` 清理 wrapper asset cache 和 worker 中注册的 assets。
- `destroy()` 终止 worker，renderer 不再使用时应该调用。

demo app 不再直接调用 raw wasm-bindgen API。这个 wrapper 是浏览器/WASM
thumbnail 产品层；底层 `WasmRenderer` 只是实现细节。

### 开发检查

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
npm run test:tools
npm run test:wasm-thumbnail
npm run test:visual
```
