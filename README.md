# MailCanvas

AI-era email verification runtime for generated email HTML. MailCanvas provides
the low-cost fast path for screenshot rendering, static diagnostics, and CI
visual QA before a pipeline falls back to Chrome, Litmus, or human review.

MailCanvas is not a browser and does not try to replace Chrome for final
fidelity review. It is a deterministic Rust/WASM email renderer and QA engine
designed to handle most low-risk generated emails without launching Chromium.
The goal is to reserve expensive browser/client testing for the small fraction
of cases that actually need it.

## English

### Why This Exists

AI makes email generation cheap, but it increases verification load. At scale,
the hard problem is no longer "can we generate an email?" It is whether every
generated result can affordably pass:

- HTML sanity checks
- email compatibility linting
- screenshot rendering
- visual/rule QA
- policy and compliance checks
- fallback or regeneration when risk is high

Running Chrome for every generated email does not scale well in memory,
throughput, or cost. MailCanvas is built for a tiered validation architecture:

```text
Agent/editor generates email
  -> cheap static validation
  -> MailCanvas fast render
  -> vision / rule QA
  -> risk score
  -> only high-risk cases go to Chrome / Litmus / human review
```

Chromium still provides the reference oracle for fidelity work through the
Playwright comparison tools in this repository. In production validation,
MailCanvas is intended to make Chrome the last 1% to 5% fallback, not the
default path for every email.

Target product KPIs:

- handle 80%+ of generated email renders without Chrome
- use about 10x less memory per render than a browser process
- deliver 5x to 20x higher render throughput for thumbnail/QA workloads
- produce deterministic same-input/same-output screenshots
- surface a risk score that catches cases needing browser/client fallback

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

Key options (see `--help` for the full list):

| Option | Default | Description |
|---|---|---|
| `--width` | `600` | CSS viewport width |
| `--scale` | `1.0` | Output pixel scale |
| `--viewport-height` | `800` | Initial CSS viewport height |
| `--min-height` | `1` | Minimum final CSS height |
| `--max-height` | - | Fail if content exceeds this CSS height |
| `--warnings-json` | - | JSON diagnostics output path |
| `--layout-json` | - | JSON layout dump output path |
| `--pdf-output` | - | Raster PDF output path |
| `--base-url` | - | Base URL for relative assets |
| `--allow-remote` | off | Allow remote `http(s)` images and fonts |
| `--allow-http` | off | Allow non-HTTPS remote resources |
| `--allow-private-network` | off | Allow localhost/private IP fetches |
| `--timeout-ms` | `30000` | Resource timeout in milliseconds |
| `--max-image-bytes` | `10 MiB` | Encoded image byte limit |
| `--max-total-resource-bytes` | `64 MiB` | Aggregate encoded byte limit |
| `--max-resource-count` | `128` | Aggregate fetched asset count limit |
| `--max-decoded-pixels` | `16M` | Decoded image pixel limit |
| `--max-dom-nodes` | `100000` | Maximum DOM nodes accepted |
| `--max-layout-depth` | `64` | Maximum nested layout depth |
| `--max-table-cells` | `100000` | Maximum expanded table cell slots |
| `--font-file` / `--font-dir` | - | Load explicit fonts instead of system fonts |

### Developer Tools

The product-facing developer tools live in `scripts/mail_canvas_tools.mjs` and
wrap the native CLI without adding Chromium to the MailCanvas render path.

Render once, or run a lightweight local preview server:

```sh
npm run preview -- examples/basic.html
npm run preview -- examples/basic.html --watch --port 4177
```

Generate a MailCanvas-only before/after visual diff:

```sh
npm run diff -- before.html after.html --out /tmp/mail-canvas-diff
```

Create or check local snapshot baselines for CI:

```sh
npm run snapshot -- "templates/**/*.html" --baseline snapshots --update
npm run snapshot -- "templates/**/*.html" --baseline snapshots
```

Run a fast diagnostics check for one template:

```sh
npm run check -- examples/basic.html --warnings-json /tmp/basic.warnings.json
```

Run fixed performance probes against MailCanvas and Chromium. Use `--runs` for
median/min/max timing and RSS summaries when comparing optimizations:

```sh
npm run benchmark:thumbnail -- --fixture-fonts --runs 3
npm run benchmark:memory -- --fixture-fonts --runs 3
```

These tools support `--profile` presets: `generic`, `desktop-800`,
`mobile-375`, `thumbnail`, `gmail-ish`, `apple-mail-ish`, `outlook-ish`, and
`images-blocked`. These are practical product profiles, not exact client
emulators.

### GitHub Action

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

### WASM API

The WASM crate is a browser-facing shell that does not perform network fetches.
The intended flow:

1. JS fetches remote stylesheets, images, or fonts.
2. JS calls `register_asset(url, bytes)` for every fetched resource.
3. JS optionally calls `register_font(bytes)` for bundled fallback fonts.
4. JS renders with `render_png_with_base_url(...)` or
   `render_rgba_with_base_url(...)`.
5. JS reads `diagnostics_json()` for warnings and load results.

```js
const renderer = new WasmRenderer();

const logoBytes = new Uint8Array(await (await fetch("https://cdn.example.com/logo.png")).arrayBuffer());
renderer.register_asset("https://cdn.example.com/logo.png", logoBytes);

const png = renderer.render_png_with_base_url(
  '<img src="./logo.png" width="120" alt="">',
  600, 800, 1.0,
  "https://cdn.example.com/email.html"
);
```

### Browser Thumbnail API

The browser-oriented wrapper lives in `browser/mail-canvas-browser.js`, with
types in `browser/mail-canvas-browser.d.ts`:

```js
import { createMailCanvasRenderer } from "./browser/mail-canvas-browser.js";

const renderer = await createMailCanvasRenderer({
  workerUrl: new URL("./browser/mail-canvas-worker.js", import.meta.url),
  fonts: [
    "./assets/NotoSans-Regular.ttf",
    "./assets/NotoSans-Bold.ttf",
    "./assets/NotoColorEmoji.ttf",
  ],
  limits: {
    maxAssetBytes: 10 * 1024 * 1024,
    maxTotalAssetBytes: 64 * 1024 * 1024,
    maxAssetCount: 128,
  },
});

const result = await renderer.renderThumbnail({
  html, width: 800, height: 1200, scale: 1,
  baseUrl: window.location.href,
});

renderer.destroy();
```

### HTTP Service and Docker

```sh
cargo build -p mail-canvas-cli
npm run serve:http
curl -sS http://127.0.0.1:8787/render \
  -H 'content-type: application/json' \
  --data '{"html":"<table><tr><td>Hello</td></tr></table>","width":600}' \
  --output email.png
```

`POST /render` returns PNG by default, or JSON with `pngBase64` and diagnostics
when `"output":"json"` is set. `GET /healthz` for health checks.

```sh
docker build -t mail-canvas .
docker run --rm -p 8787:8787 mail-canvas
```

### Project Structure

- `crates/mail-canvas-core/` — platform-independent rendering engine: HTML
  parsing, CSS inlining, layout, text shaping, painting, diagnostics, and
  resource/font/output traits. No filesystem, HTTP, CLI, or system-font
  scanning.
- `crates/mail-canvas-native/` — native resource loading, filesystem helpers,
  system font discovery, PNG output, and raster PDF output.
- `crates/mail-canvas-wasm/` — `wasm-bindgen` wrapper for browser workers.
- `crates/mail-canvas-cli/` — CLI wrapper around the native renderer.
- `scripts/` — Chromium comparison, layout dump, template corpus, and
  developer tools.
- `browser/` — browser integration layer with worker management and asset
  fetching.
- `examples/` — Node wrapper and HTTP service examples.

### Fidelity Workflow

Run the fixed Playwright semantic visual regression set:

```sh
npm run test:visual
```

This gate uses Chromium as the reference with semantic tolerances (content
presence, layout stability, media regions) rather than strict pixel equality.

Run the committed golden corpus comparison for diagnostics:

```sh
npm run compare:corpus
```

Run only the committed editor/generated samples:

```sh
npm run compare:editors
```

Run the corpus pipeline for temporary large-template intake, audit, comparison,
triage, and registry updates:

```sh
npm run corpus:pipeline -- \
  --provider reallygoodemails \
  --category saas \
  --limit 10 \
  --random \
  --exclude-seen \
  --work-dir runs/rge-saas
```

The pipeline writes stable artifacts under the run directory:

- `manifest.json` — selected templates and HTML hashes.
- `audit.json` — corpus health issues scoped to the selected templates.
- `compare/` — Chromium reference screenshots, MailCanvas screenshots, layout
  dumps, diagnostics, full side-by-side images, and pixel diff images.
- `first-bad-crops/` — browser/Rust/diff crops around the first divergent
  vertical band.

Committed corpus files are intentionally small. Bulk downloads are tracked by
`corpus/registry.json` with HTML and asset MD5 fingerprints, but only promoted
golden templates stay in git. The pipeline removes newly vendored research
HTML/assets after recording them; pass `--keep-vendored` only when intentionally
inspecting or promoting a template. P0/P1/P2 findings are recorded in
`corpus/issues.json` as `pending`; rerunning a template after the finding
disappears marks it `fixed`. The issue log also includes a `summary.byType`
section so repeated problem classes can be prioritized by pending template
count and total occurrences.
- `triage.json` and `triage.md` — prioritized failures grouped as `P0` to
  `P3` by likely fix value.
- `pipeline.json` — commands, targets, timings, and output paths for the run.

Use `--skip-vendor --only TEMPLATE_NAME` to rerun an existing fixture. Browser
screenshots are cached by prepared HTML and width via `--browser-cache-dir` so
unchanged templates avoid repeated Chromium screenshot work across pipeline
runs.

Refresh deterministic font fixtures when the supported open-source font bundle
changes:

```sh
npm run fonts:download
```

The committed bundle intentionally stays small: email-safe aliases are mapped to
Arimo/Tinos, Noto covers generic fallback/math symbols/default emoji, and common
Google Fonts such as Roboto, Open Sans, Lato, Montserrat, Poppins, Inter, Source
Sans 3, Merriweather, and Nunito Sans are vendored as latin subsets. Avoid
adding template-specific font workarounds to the fixture catalog.

Compare one local HTML file:

```sh
npm run compare:local -- --html ./cnn.html --name cnn-local
```

### CSS Support Matrix

| Area | Supported | Notes |
|---|---|---|
| Block flow | `display:block`, margins, padding, borders, background color/image | Email-oriented subset |
| Inline text | `font-*`, `line-height`, `letter-spacing`, `text-align`, `text-transform`, `white-space:nowrap` | |
| Tables | nested tables, `rowspan`, `colspan`, `cellpadding`, `cellspacing`, `table-layout:fixed`, `col` width hints | Primary modern email target |
| Images | `img`, `background-image`, `object-fit`, `object-position`, width/height attributes | Remote and `data:` assets |
| Media queries | `screen`/`all`, `only`, `not`, `min/max-width`, width ranges, `orientation` | Expanded in source order before inlining |
| Flex subset | `display:flex`, direction, wrap, align/justify, gap | Common email-safe subset |
| Float subset | `float:left/right`, `clear` | Basic wrap avoidance |
| Positioning | static, relative, absolute/fixed child placement | No full browser stacking model |

Out of scope: JavaScript, forms, video, canvas, grid, VML/MSO, full flex/grid,
legacy hybrid hacks.

### Current Limits

- PNG is the primary output. PDF output is raster-only (text is not
  selectable).
- CSS support is intentionally narrow and tied to email templates. Unsupported
  declarations are silently ignored; structured warnings report issues.
- Remote resources are disabled by default and must be enabled explicitly.
- DOM, layout depth, table cell, encoded byte, and decoded pixel limits are
  enforced by default.
- Visual fidelity is measured against Chromium with semantic tolerances. Strict
  total pixel equality is not required because text rasterization differs
  between Chromium/Skia and the pure Rust text stack.

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

### 为什么需要它

AI 让邮件生成成本下降，但验证成本会上升。规模化之后，核心问题不是"能不能生成"，
而是每个生成结果是否都能以可接受成本完成：

- HTML sanity check
- email compatibility lint
- render screenshot
- visual / rule QA
- policy / compliance check
- 高风险时 fallback 或 regenerate

如果每一步都跑 Chrome，内存、吞吐和成本都会成为瓶颈。MailCanvas 面向分层验证：

```text
Agent/editor 生成 email
  -> cheap static validation
  -> MailCanvas fast render
  -> vision / rule QA
  -> risk score
  -> 只有高风险结果进入 Chrome / Litmus / human review
```

Chromium/Blink 仍然是保真度对齐的参考标准（通过仓库里的 Playwright 工具）。在产品
验证链路里，MailCanvas 的定位是让 Chrome 成为最后 1% 到 5% 的 fallback，而不是
每封邮件的默认路径。

目标 KPI：

- 80%+ 生成邮件不需要 Chrome 就能完成 render/QA fast path
- 单次 render 内存比浏览器进程低一个数量级
- thumbnail / QA workload 吞吐提升 5x 到 20x
- same-input same-output deterministic
- risk score 能识别需要浏览器或真实客户端 fallback 的样本

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

常用参数见上方英文部分的选项表，或运行 `--help` 查看完整列表。

### 开发工具

```sh
# 单次渲染或本地预览
npm run preview -- examples/basic.html
npm run preview -- examples/basic.html --watch --port 4177

# before/after 视觉 diff
npm run diff -- before.html after.html --out /tmp/mail-canvas-diff

# snapshot baseline
npm run snapshot -- "templates/**/*.html" --baseline snapshots --update
npm run snapshot -- "templates/**/*.html" --baseline snapshots

# 快速诊断检查
npm run check -- examples/basic.html --warnings-json /tmp/basic.warnings.json

# 固定性能探针；--runs 输出 timing/RSS 的 min/median/max
npm run benchmark:thumbnail -- --fixture-fonts --runs 3
npm run benchmark:memory -- --fixture-fonts --runs 3
```

支持 `--profile` 预设：`generic`、`desktop-800`、`mobile-375`、`thumbnail`、
`gmail-ish`、`apple-mail-ish`、`outlook-ish`、`images-blocked`。

### GitHub Action

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

### WASM API

WASM crate 本身不做网络请求，资源由 JS 侧预加载后注入：

1. JS 拉取远程 stylesheet/image/font。
2. 调用 `register_asset(url, bytes)` 注册每个资源。
3. 可选调用 `register_font(bytes)` 注册兜底字体。
4. 用 `render_png_with_base_url(...)` 渲染。
5. 用 `diagnostics_json()` 读取 warnings 和加载结果。

```js
const renderer = new WasmRenderer();
renderer.register_asset("https://cdn.example.com/logo.png", logoBytes);
const png = renderer.render_png_with_base_url(
  '<img src="./logo.png" width="120" alt="">',
  600, 800, 1.0, "https://cdn.example.com/email.html"
);
```

### 浏览器 Thumbnail API

```js
import { createMailCanvasRenderer } from "./browser/mail-canvas-browser.js";

const renderer = await createMailCanvasRenderer({
  workerUrl: new URL("./browser/mail-canvas-worker.js", import.meta.url),
  fonts: [
    "./assets/NotoSans-Regular.ttf",
    "./assets/NotoSans-Bold.ttf",
    "./assets/NotoColorEmoji.ttf",
  ],
  limits: { maxAssetBytes: 10 * 1024 * 1024, maxTotalAssetBytes: 64 * 1024 * 1024, maxAssetCount: 128 },
});

const result = await renderer.renderThumbnail({
  html, width: 800, height: 1200, scale: 1, baseUrl: window.location.href,
});
renderer.destroy();
```

### HTTP Service 和 Docker

```sh
cargo build -p mail-canvas-cli
npm run serve:http
curl -sS http://127.0.0.1:8787/render \
  -H 'content-type: application/json' \
  --data '{"html":"<table><tr><td>Hello</td></tr></table>","width":600}' \
  --output email.png
```

`POST /render` 默认返回 PNG，设置 `"output":"json"` 返回含 `pngBase64` 和
diagnostics 的 JSON。`GET /healthz` 用于健康检查。

```sh
docker build -t mail-canvas .
docker run --rm -p 8787:8787 mail-canvas
```

### 项目结构

- `crates/mail-canvas-core/` — 平台无关的渲染引擎：HTML 解析、CSS inline、
  layout、文字排版、绘制、diagnostics，以及 resource/font/output trait。
  不涉及文件系统、HTTP、CLI 或系统字体。
- `crates/mail-canvas-native/` — native 资源加载、文件系统、系统字体发现、
  PNG 和 raster PDF 输出。
- `crates/mail-canvas-wasm/` — `wasm-bindgen` 封装，面向浏览器 worker。
- `crates/mail-canvas-cli/` — 基于 native renderer 的 CLI。
- `scripts/` — Chromium 对比、布局 dump、模板语料和开发工具。
- `browser/` — 浏览器集成层，含 worker 管理和资源抓取。
- `examples/` — Node wrapper 和 HTTP service 示例。

### 保真度工作流

```sh
# 固定 Playwright 语义回归集
npm run test:visual

# 已提交 golden corpus 对比（诊断用）
npm run compare:corpus

# 已提交的编辑器/生成器样例对比
npm run compare:editors

# 本地 HTML 对比
npm run compare:local -- --html ./cnn.html --name cnn-local
```

`test:visual` 使用 Chromium 作为参考，通过语义容差（内容存在、布局稳定、
媒体区域）而非严格像素一致性来判定。

仓库只保留少量 golden 模板。批量下载的研究模板通过
`corpus/registry.json` 记录 HTML 和 asset MD5 指纹，不把整批 HTML/图片
提交进 git。

### CSS 支持矩阵

| 领域 | 支持 | 说明 |
|---|---|---|
| Block flow | `display:block`、margin、padding、border、background | 邮件子集 |
| Inline text | `font-*`、`line-height`、`letter-spacing`、`text-align` 等 | |
| Table | 嵌套 table、`rowspan`/`colspan`、cellpadding/cellspacing、`table-layout:fixed` | 主要邮件布局目标 |
| 图片 | `img`、`background-image`、`object-fit`、`object-position` | 支持 remote 和 `data:` |
| Media query | `screen`/`all`、`only`/`not`、`min/max-width`、range、`orientation` | inline 前按源码顺序展开 |
| Flex 子集 | `display:flex`、direction、wrap、align/justify、gap | 常见邮件安全子集 |
| Float 子集 | `float:left/right`、`clear` | |
| Positioning | static、relative、absolute/fixed | 无完整 stacking model |

不在范围内：JavaScript、form、video、canvas、grid、VML/MSO、完整 flex/grid、
legacy hybrid hack。

### 当前限制

- PNG 是主要输出；PDF 为栅格 PDF，文字不可选中。
- CSS 支持面向邮件模板子集，不支持的声明会被静默忽略并报告 warning。
- 远程资源默认关闭，需显式开启。DOM、layout depth、table cell、字节和像素限制默认开启。
- 视觉保真度以 Chromium 为参考，采用语义容差，不要求像素级一致。

### 开发检查

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
npm run test:tools
npm run test:wasm-thumbnail
npm run test:visual
```
