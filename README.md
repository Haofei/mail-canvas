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
    let request = RenderRequest::defaults_for_html(html, 600, 800, 1.0);
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
2. Main thread posts those bytes to `demo/worker.js`.
3. The worker registers assets in `mail-canvas-wasm`.
4. The worker returns PNG bytes and diagnostics JSON.

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
npm run test:playwright-regression
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
- total pixel diff is still reported for investigation, but it is observational
  unless `maxTotalDiffPercent` is explicitly configured.

Run the full corpus comparison with the same semantic gate:

```sh
npm run compare:playwright
```

Run only the modern editor/generated marketing corpus:

```sh
npm run compare:playwright-editors
```

This filters the local, vendored corpus to Beefree, Stripo, and MJML templates.
It is intended for renderer-fidelity work on valid generated email HTML, not
legacy hand-written compatibility fixtures.

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

```sh
node scripts/playwright_compare.mjs \
  --expectations scripts/playwright_expectations.json \
  --work-dir /tmp/mail-canvas-playwright-all-semantic \
  --timeout-ms 30000 \
  --all
```

Artifacts are written under `/tmp/mail-canvas-playwright-regression` or
`/tmp/mail-canvas-playwright-compare`, including browser screenshots, MailCanvas
screenshots, diff images, side-by-side images, `comparison.json`,
`comparison.report.json`, and `report.md`.

`comparison.report.json` is the machine-readable summary intended for CI and
artifact upload.

For detailed layout investigation:

```sh
npm run dump:chrome-layout -- \
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
npm run compare:layout-rects -- \
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

Arimo is used as the Arial-compatible Latin sans fixture for editor-generated
marketing templates; Tinos is used as the Times-compatible serif fixture for
editor templates that fall back to `serif`; Noto Sans remains the broader
fallback fixture.

`npm run test:playwright-regression` passes `--fixture-fonts` so GitHub Actions
does not depend on the Linux runner's installed fonts. Pass `--fixture-fonts` to
`scripts/playwright_compare.mjs` for other host-independent runs when stable
wrapping is more important than matching Chromium's local font fallback.

### Examples

- Node wrapper: `examples/node-render.mjs`
- HTTP service shell: `examples/http-service.mjs`

Both examples currently shell out to the native CLI instead of embedding a Node
native module. That keeps the example path simple while the Rust public API
stabilizes.

### Memory Benchmark

Compare one corpus template against Chromium:

```sh
npm run benchmark:memory -- --template colorlib-template-1 --out /tmp/mail-canvas-benchmark.json
```

This writes RSS and elapsed-time measurements for:

- `mail-canvas` CLI
- Chromium screenshot capture through Playwright

### Development Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
npm run test:playwright-regression
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

### Rust API

```rust
use mail_canvas_core::{EmailRenderer, RenderRequest};
use mail_canvas_native::MailCanvasRenderer;

fn main() -> anyhow::Result<()> {
    let html = "<table><tr><td>Hello</td></tr></table>".to_string();
    let request = RenderRequest::defaults_for_html(html, 600, 800, 1.0);
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
2. 主线程把字节通过 `postMessage` 发给 `demo/worker.js`；
3. worker 在 `mail-canvas-wasm` 里注册资源并渲染；
4. worker 返回 PNG 字节和 diagnostics JSON。

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
npm run test:playwright-regression
```

这个 gate 仍然使用 Chromium 作为参考，但不要求总像素完全一致。真正的通过条件
是更宽容的语义检查：

- 固定回归集里的 renderer warnings 必须为 0；
- viewport 宽度和最终渲染高度必须落在语义容差内；
- 文字、媒体、非文字非媒体区域必须落在粗粒度 diff 限制内，用来发现内容缺失
  或明显布局错误；媒体检查也带绝对像素容差，避免小图标抗锯齿差异被过度惩罚；
- total pixel diff 仍会输出，方便排查，但除非显式配置 `maxTotalDiffPercent`，
  否则不作为失败条件。

使用同一套 semantic gate 跑完整语料：

```sh
npm run compare:playwright
```

只跑现代编辑器/生成器模板：

```sh
npm run compare:playwright-editors
```

这个命令只筛选本地 vendored 语料里的 Beefree、Stripo 和 MJML 模板，主要用于
合法生成 HTML 的渲染质量迭代，不针对老旧手写兼容 hack。

`scripts/templates.mjs` 现在是带 metadata 的语料：provider、category、status、
expected warning count，以及 known warning 的原因。全量语料里，known-warning
模板会先跳过，除非它被显式列在 `scripts/playwright_expectations.json`；这样上游
已经失效的图片链接和模板变量图片不会污染通过率。

```sh
node scripts/playwright_compare.mjs \
  --expectations scripts/playwright_expectations.json \
  --work-dir /tmp/mail-canvas-playwright-all-semantic \
  --timeout-ms 30000 \
  --all
```

输出会在 `/tmp/mail-canvas-playwright-regression` 或
`/tmp/mail-canvas-playwright-compare`，里面有浏览器截图、MailCanvas 截图、
diff 图、side-by-side 图、`comparison.json` 和 `report.md`。

查看 Chromium 的具体布局：

```sh
npm run dump:chrome-layout -- \
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
npm run compare:layout-rects -- \
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
- JavaScript、form、video、canvas、完整 positioning、完整 flex/grid、完整浏览器
  painting 都不在当前范围内。
- 远程资源默认关闭，需要显式开启。DOM、layout depth、table cell、编码字节数和
  解码像素数限制默认开启。
- 视觉效果以 Chromium 为参考，但采用语义化容差；由于 Chromium/Skia 和纯 Rust
  文本栈的文字栅格化不同，不要求 total pixel diff 完全一致。
- 固定 Playwright 回归集目前可以通过。total pixel diff 作为诊断信号保留，gate
  主要关注内容是否存在、布局是否稳定、媒体区域和非文字非媒体结构是否正确。

### 开发检查

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
npm run test:playwright-regression
```
