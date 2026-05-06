import init, { WasmRenderer } from "./pkg/mail_canvas_wasm.js";

const htmlInput = document.querySelector("#html-input");
const widthInput = document.querySelector("#width");
const viewportHeightInput = document.querySelector("#viewport-height");
const scaleInput = document.querySelector("#scale");
const baseUrlInput = document.querySelector("#base-url");
const renderButton = document.querySelector("#render-button");
const preview = document.querySelector("#preview");
const diagnostics = document.querySelector("#diagnostics");
const status = document.querySelector("#status");

const SAMPLE_HTML = `<!doctype html>
<html>
  <head>
    <link rel="stylesheet" href="./assets/sample-email.css">
  </head>
  <body>
    <div class="email-root">
      <table class="card" role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0">
        <tr>
          <td class="hero">
            <div class="eyebrow">MailCanvas Demo</div>
            <h1 class="title">Generate email previews in the browser.</h1>
            <p class="summary">The page fetches linked assets, registers them in wasm, then renders the final HTML to a PNG without launching Chrome.</p>
          </td>
        </tr>
        <tr>
          <td class="content">
            <table class="metric-grid" role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0">
              <tr>
                <td class="metric-cell">
                  <div class="metric-box">
                    <div class="metric-label">Runtime</div>
                    <div class="metric-value">Browser + WASM</div>
                  </div>
                </td>
                <td class="metric-cell">
                  <div class="metric-box">
                    <div class="metric-label">Asset Flow</div>
                    <div class="metric-value">Fetch -> register_asset -> render</div>
                  </div>
                </td>
              </tr>
            </table>
            <p class="body-copy">Edit the HTML on the left, keep linked CSS or images relative to the base URL, then render again. Diagnostics below show which assets were loaded or blocked.</p>
            <a class="button" href="https://github.com/Haofei/mail-canvas">Open Repository</a>
            <div class="footer">Bundled demo fonts: Noto Sans Regular + Bold.</div>
          </td>
        </tr>
      </table>
    </div>
  </body>
</html>`;

const DEMO_FONTS = [
  "./assets/NotoSans-Regular.ttf",
  "./assets/NotoSans-Bold.ttf",
];

let renderer;
let currentObjectUrl = null;

const assetPattern = /url\(\s*(['"]?)([^'")]+)\1\s*\)|@import\s+(?:url\(\s*)?(['"]?)([^'")\s]+)\3\s*\)?/gi;

function setStatus(message) {
  status.textContent = message;
}

function uniquePush(set, value) {
  if (value && !value.startsWith("data:")) {
    set.add(value);
  }
}

function extractCssUrls(cssText) {
  const urls = new Set();
  let match;
  while ((match = assetPattern.exec(cssText)) !== null) {
    uniquePush(urls, match[2] || match[4] || "");
  }
  return urls;
}

function collectHtmlAssetUrls(html) {
  const document = new DOMParser().parseFromString(html, "text/html");
  const urls = new Set();

  for (const image of document.querySelectorAll("[src]")) {
    uniquePush(urls, image.getAttribute("src") || "");
  }
  for (const link of document.querySelectorAll('link[rel="stylesheet"][href]')) {
    uniquePush(urls, link.getAttribute("href") || "");
  }
  for (const node of document.querySelectorAll("[background]")) {
    uniquePush(urls, node.getAttribute("background") || "");
  }
  for (const styleBlock of document.querySelectorAll("style")) {
    for (const url of extractCssUrls(styleBlock.textContent || "")) {
      uniquePush(urls, url);
    }
  }
  for (const styled of document.querySelectorAll("[style]")) {
    for (const url of extractCssUrls(styled.getAttribute("style") || "")) {
      uniquePush(urls, url);
    }
  }

  return urls;
}

function absoluteUrl(rawUrl, baseUrl) {
  return new URL(rawUrl, baseUrl).toString();
}

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`failed to fetch ${url}: ${response.status} ${response.statusText}`);
  }
  return new Uint8Array(await response.arrayBuffer());
}

async function registerAssetGraph(html, baseUrl) {
  renderer.clear_assets();
  const visited = new Set();
  const pending = [...collectHtmlAssetUrls(html)].map((url) => absoluteUrl(url, baseUrl));

  while (pending.length > 0) {
    const url = pending.pop();
    if (!url || visited.has(url)) {
      continue;
    }
    visited.add(url);

    const bytes = await fetchBytes(url);
    renderer.register_asset(url, bytes);

    if (url.endsWith(".css") || isStylesheetBytes(bytes)) {
      const cssText = new TextDecoder().decode(bytes);
      for (const nested of extractCssUrls(cssText)) {
        pending.push(absoluteUrl(nested, url));
      }
    }
  }
}

function isStylesheetBytes(bytes) {
  if (bytes.length === 0) {
    return false;
  }
  const head = new TextDecoder().decode(bytes.slice(0, Math.min(bytes.length, 32))).trimStart();
  return head.startsWith("@") || head.includes("{");
}

async function loadDemoFonts() {
  for (const fontPath of DEMO_FONTS) {
    const bytes = await fetchBytes(fontPath);
    renderer.register_font(bytes);
  }
}

async function boot() {
  setStatus("Loading wasm module...");
  await init();
  renderer = new WasmRenderer();
  await loadDemoFonts();
  htmlInput.value = SAMPLE_HTML;
  baseUrlInput.value = new URL(".", window.location.href).toString();
  diagnostics.textContent = JSON.stringify(
    { warnings: [], assets: [], console_messages: [] },
    null,
    2
  );
  setStatus("Ready");
}

async function renderCurrentTemplate() {
  const html = htmlInput.value;
  const width = Number(widthInput.value);
  const viewportHeight = Number(viewportHeightInput.value);
  const scale = Number(scaleInput.value);
  const baseUrl = baseUrlInput.value.trim() || window.location.href;

  renderButton.disabled = true;
  setStatus("Fetching linked assets...");

  try {
    await registerAssetGraph(html, baseUrl);
    setStatus(`Rendering with ${renderer.asset_count()} registered assets...`);
    const pngBytes = renderer.render_png_with_base_url(
      html,
      width,
      viewportHeight,
      scale,
      baseUrl
    );

    if (currentObjectUrl) {
      URL.revokeObjectURL(currentObjectUrl);
    }
    currentObjectUrl = URL.createObjectURL(
      new Blob([pngBytes], { type: "image/png" })
    );
    preview.src = currentObjectUrl;
    diagnostics.textContent = JSON.stringify(
      JSON.parse(renderer.diagnostics_json()),
      null,
      2
    );
    setStatus("Render complete");
  } catch (error) {
    diagnostics.textContent = String(error);
    setStatus("Render failed");
    throw error;
  } finally {
    renderButton.disabled = false;
  }
}

renderButton.addEventListener("click", () => {
  renderCurrentTemplate().catch((error) => {
    console.error(error);
  });
});

boot().catch((error) => {
  setStatus("Failed to initialize wasm demo");
  diagnostics.textContent = String(error);
  console.error(error);
});
