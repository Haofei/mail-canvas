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
cargo run -- \
  --html examples/basic.html \
  --css examples/basic.css \
  --output out.png \
  --pdf-output out.pdf \
  --width 600
```

Important options:

- `--width`: CSS viewport width, default `600`.
- `--scale`: output pixel scale, default `1.0`.
- `--min-height`: minimum final CSS height.
- `--max-height`: fail if rendered content exceeds this CSS height.
- `--pdf-output`: optional raster PDF output path.
- `--base-url`: base URL for relative assets; defaults to the HTML file
  directory.
- `--allow-remote`: allow remote `http(s)` images and fonts.
- `--allow-http`: allow non-HTTPS remote resources when remote loading is
  enabled.
- `--timeout-ms`: resource timeout in milliseconds.
- `--max-image-bytes`: encoded image byte limit.
- `--max-decoded-pixels`: decoded image pixel limit.
- `--font-file` / `--font-dir`: load explicit fonts instead of scanning system
  fonts.

### Rust API

```rust
use mail_canvas::{EmailRenderer, MailCanvasRenderer, RenderRequest};

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

### Project Shape

- `src/lib.rs`: renderer API, layout tree, email-oriented layout rules, and
  painting.
- `src/css.rs`: CSS inlining helpers, `lightningcss` declaration parsing,
  active media extraction, and `@font-face` extraction.
- `src/resource.rs`: bounded local, data URL, and opt-in remote resource loading.
- `src/document.rs`: document wrapping and head injection helpers.
- `src/main.rs`: CLI wrapper around the library API.
- `scripts/`: Chromium comparison, layout dump, template corpus, and Blink
  reference helpers.

### Fidelity Workflow

Run the fixed Playwright regression set:

```sh
npm run test:playwright-regression
```

Run a broader comparison:

```sh
npm run compare:playwright
```

Artifacts are written under `/tmp/mail-canvas-playwright-regression` or
`/tmp/mail-canvas-playwright-compare`, including browser screenshots, MailCanvas
screenshots, diff images, side-by-side images, `comparison.json`, and
`report.md`.

For detailed layout investigation:

```sh
npm run dump:chrome-layout -- \
  --template colorlib-template-1 \
  --selector '.email-section, .text-services, td, img' \
  --y 2066 \
  --out /tmp/colorlib-1-layout.json
```

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
  declarations are ignored.
- JavaScript, forms, video, canvas, full positioning, full flex/grid, and full
  browser painting are out of scope.
- Remote resources are disabled by default and must be enabled explicitly.
- Pixel fidelity is measured against Chromium, but some templates still differ
  because font rasterization and long-tail layout rules are not fully matched.
- The fixed Playwright regression suite currently passes. Several templates are
  already below 2% total pixel diff; the larger remaining diffs are dominated by
  text rasterization differences between Chromium/Skia and the pure Rust text
  stack.

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
cargo run -- \
  --html examples/basic.html \
  --css examples/basic.css \
  --output out.png \
  --pdf-output out.pdf \
  --width 600
```

常用参数：

- `--width`: CSS viewport 宽度，默认 `600`。
- `--scale`: 输出像素倍率，默认 `1.0`。
- `--min-height`: 最小 CSS 输出高度。
- `--max-height`: 内容超过该 CSS 高度时失败。
- `--pdf-output`: 可选的栅格 PDF 输出路径。
- `--base-url`: 相对资源的 base URL，默认是 HTML 文件目录。
- `--allow-remote`: 允许远程 `http(s)` 图片和字体。
- `--allow-http`: 开启远程资源后，允许非 HTTPS。
- `--timeout-ms`: 资源加载超时，单位毫秒。
- `--max-image-bytes`: 编码后图片字节限制。
- `--max-decoded-pixels`: 解码后图片像素数限制。
- `--font-file` / `--font-dir`: 使用指定字体，避免扫描系统字体。

### Rust API

```rust
use mail_canvas::{EmailRenderer, MailCanvasRenderer, RenderRequest};

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

### 项目结构

- `src/lib.rs`: renderer API、layout tree、邮件布局规则和绘制逻辑。
- `src/css.rs`: CSS inlining、`lightningcss` declaration 解析、active media
  提取和 `@font-face` 提取。
- `src/resource.rs`: 带限制的本地资源、data URL、可选远程资源加载。
- `src/document.rs`: HTML document 包装和 head 注入。
- `src/main.rs`: 基于库 API 的 CLI。
- `scripts/`: Chromium 对比、布局 dump、模板语料和 Blink 参考代码工具。

### 对比和调试

固定回归集：

```sh
npm run test:playwright-regression
```

更多模板对比：

```sh
npm run compare:playwright
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

下载固定版本的 Blink 参考代码：

```sh
scripts/fetch_blink_reference.sh
```

`blink-reference/` 不进入 Git。它只用于理解算法：先用 Chromium 抓布局和样式，
再看 Blink 对应模块，然后在 Rust 里实现最小的邮件相关规则，不复制 Blink
源码。

### 当前限制

- PNG 是主要输出；PDF 目前是栅格 PDF，文字不可选中、不可搜索。
- CSS 支持是邮件模板导向的子集，不支持的 declaration 会被忽略。
- JavaScript、form、video、canvas、完整 positioning、完整 flex/grid、完整浏览器
  painting 都不在当前范围内。
- 远程资源默认关闭，需要显式开启。
- 像素效果以 Chromium 为参考，但字体栅格化和长尾布局规则还没有完全对齐。
- 固定 Playwright 回归集目前可以通过。部分模板总像素差已经低于 2%；剩余较大
  差距主要来自 Chromium/Skia 和纯 Rust 文本栈之间的字体栅格化差异。

### 开发检查

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
npm run test:playwright-regression
```
