import { clearCache, layoutWithLines, prepareWithSegments } from "./vendor/pretext/layout.js";

const htmlInput = document.querySelector("#html-input");
const widthInput = document.querySelector("#width");
const viewportHeightInput = document.querySelector("#viewport-height");
const scaleInput = document.querySelector("#scale");
const baseUrlInput = document.querySelector("#base-url");
const usePretextInput = document.querySelector("#use-pretext");
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
            <p class="summary">The page fetches linked assets on the main thread, injects them into a worker, then renders the final HTML to a PNG without launching Chrome.</p>
          </td>
        </tr>
        <tr>
          <td class="content">
            <table class="metric-grid" role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0">
              <tr>
                <td class="metric-cell">
                  <div class="metric-box">
                    <div class="metric-label">Runtime</div>
                    <div class="metric-value">Worker + WASM</div>
                  </div>
                </td>
                <td class="metric-cell">
                  <div class="metric-box">
                    <div class="metric-label">Asset Flow</div>
                    <div class="metric-value">Fetch -> postMessage -> register_asset -> render</div>
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

const PRETEXT_TARGET_TAGS = new Set([
  "A",
  "DIV",
  "H1",
  "H2",
  "H3",
  "H4",
  "H5",
  "H6",
  "P",
  "SPAN",
]);

const assetPattern = /url\(\s*(['"]?)([^'")]+)\1\s*\)|@import\s+(?:url\(\s*)?(['"]?)([^'")\s]+)\3\s*\)?/gi;

let worker;
let currentObjectUrl = null;
let nextRequestId = 1;
let measurementFrame = null;

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

async function fetchAssetGraph(html, baseUrl) {
  const visited = new Set();
  const assets = [];
  const pending = [...collectHtmlAssetUrls(html)].map((url) => absoluteUrl(url, baseUrl));

  while (pending.length > 0) {
    const url = pending.pop();
    if (!url || visited.has(url)) {
      continue;
    }
    visited.add(url);

    const bytes = await fetchBytes(url);
    assets.push({ url, bytes });

    if (url.endsWith(".css") || isStylesheetBytes(bytes)) {
      const cssText = new TextDecoder().decode(bytes);
      for (const nested of extractCssUrls(cssText)) {
        pending.push(absoluteUrl(nested, url));
      }
    }
  }

  return assets;
}

function isStylesheetBytes(bytes) {
  if (bytes.length === 0) {
    return false;
  }
  const head = new TextDecoder().decode(bytes.slice(0, Math.min(bytes.length, 32))).trimStart();
  return head.startsWith("@") || head.includes("{");
}

async function fetchFontAssets() {
  return Promise.all(
    DEMO_FONTS.map(async (url) => ({
      url: new URL(url, window.location.href).toString(),
      bytes: await fetchBytes(url),
    })),
  );
}

function callWorker(message, transfer = []) {
  return new Promise((resolve, reject) => {
    const requestId = nextRequestId++;
    const onMessage = (event) => {
      if (event.data?.requestId !== requestId) {
        return;
      }
      worker.removeEventListener("message", onMessage);
      worker.removeEventListener("error", onError);
      if (event.data.ok) {
        resolve(event.data);
      } else {
        reject(new Error(event.data.error || "worker request failed"));
      }
    };
    const onError = (event) => {
      worker.removeEventListener("message", onMessage);
      worker.removeEventListener("error", onError);
      reject(event.error || new Error("worker crashed"));
    };
    worker.addEventListener("message", onMessage);
    worker.addEventListener("error", onError);
    worker.postMessage({ ...message, requestId }, transfer);
  });
}

function ensureMeasurementFrame() {
  if (measurementFrame) {
    return measurementFrame;
  }
  measurementFrame = document.createElement("iframe");
  measurementFrame.setAttribute("aria-hidden", "true");
  measurementFrame.style.position = "fixed";
  measurementFrame.style.left = "-100000px";
  measurementFrame.style.top = "0";
  measurementFrame.style.width = "0";
  measurementFrame.style.height = "0";
  measurementFrame.style.opacity = "0";
  measurementFrame.style.pointerEvents = "none";
  document.body.appendChild(measurementFrame);
  return measurementFrame;
}

function normalizeText(text) {
  return (text || "").replace(/\s+/g, " ").trim();
}

function assignPretextIds(sourceDocument) {
  let nextId = 1;
  let candidateCount = 0;
  for (const element of sourceDocument.body.querySelectorAll("*")) {
    if (!PRETEXT_TARGET_TAGS.has(element.tagName)) {
      continue;
    }
    if (element.children.length > 0) {
      continue;
    }
    if (!normalizeText(element.textContent)) {
      continue;
    }
    if (element.querySelector("img,table,svg,ul,ol,li")) {
      continue;
    }
    element.setAttribute("data-pretext-id", String(nextId++));
    candidateCount += 1;
  }
  return candidateCount;
}

function serializeDocumentWithBase(sourceDocument, baseUrl) {
  if (!sourceDocument.head) {
    const head = sourceDocument.createElement("head");
    sourceDocument.documentElement.insertBefore(head, sourceDocument.body || null);
  }
  let base = sourceDocument.head.querySelector("base");
  if (!base) {
    base = sourceDocument.createElement("base");
    sourceDocument.head.prepend(base);
  }
  base.setAttribute("href", baseUrl);
  return `<!doctype html>\n${sourceDocument.documentElement.outerHTML}`;
}

function parsePx(rawValue, fallback) {
  if (!rawValue || rawValue === "normal") {
    return fallback;
  }
  const value = Number.parseFloat(rawValue);
  return Number.isFinite(value) ? value : fallback;
}

function pretextOptions(computedStyle) {
  const letterSpacing = computedStyle.letterSpacing === "normal"
    ? 0
    : Number.parseFloat(computedStyle.letterSpacing);
  return {
    whiteSpace: computedStyle.whiteSpace.includes("pre") ? "pre-wrap" : "normal",
    wordBreak: computedStyle.wordBreak === "keep-all" ? "keep-all" : "normal",
    letterSpacing: Number.isFinite(letterSpacing) ? letterSpacing : 0,
  };
}

function canvasFontShorthand(computedStyle) {
  const fontStyle = computedStyle.fontStyle || "normal";
  const fontWeight = computedStyle.fontWeight || "400";
  const fontSize = computedStyle.fontSize || "16px";
  const fontFamily = computedStyle.fontFamily || "sans-serif";
  return `${fontStyle} ${fontWeight} ${fontSize} ${fontFamily}`;
}

async function loadMeasurementDocument(html, baseUrl, width) {
  const frame = ensureMeasurementFrame();
  frame.style.width = `${width}px`;
  frame.style.height = "1200px";

  await new Promise((resolve, reject) => {
    frame.onload = () => resolve();
    frame.onerror = () => reject(new Error("failed to load measurement iframe"));
    frame.srcdoc = html.includes("<base")
      ? html
      : html.replace("<head>", `<head><base href="${baseUrl}">`);
  });

  const frameWindow = frame.contentWindow;
  if (!frameWindow) {
    throw new Error("measurement iframe window unavailable");
  }
  frameWindow.document.documentElement.style.width = `${width}px`;
  frameWindow.document.body.style.margin = "0";
  if (frameWindow.document.fonts?.ready) {
    await frameWindow.document.fonts.ready;
  }
  await new Promise((resolve) => setTimeout(resolve, 30));
  return frameWindow.document;
}

function escapeHtml(text) {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

async function applyPretextHints(html, baseUrl, width) {
  const sourceDocument = new DOMParser().parseFromString(html, "text/html");
  const candidateCount = assignPretextIds(sourceDocument);
  if (candidateCount === 0) {
    return {
      html,
      report: { enabled: true, candidates: 0, applied: 0, changed: [] },
    };
  }

  clearCache();
  const preparedHtml = serializeDocumentWithBase(sourceDocument, baseUrl);
  const measurementDocument = await loadMeasurementDocument(preparedHtml, baseUrl, width);
  const changed = [];

  for (const measuredElement of measurementDocument.querySelectorAll("[data-pretext-id]")) {
    const text = normalizeText(measuredElement.textContent);
    if (!text) {
      continue;
    }

    const rect = measuredElement.getBoundingClientRect();
    if (rect.width < 20 || rect.height <= 0) {
      continue;
    }

    const computedStyle = measurementDocument.defaultView.getComputedStyle(measuredElement);
    const fontSize = parsePx(computedStyle.fontSize, 16);
    const lineHeight = parsePx(computedStyle.lineHeight, fontSize * 1.2);
    const prepared = prepareWithSegments(
      text,
      canvasFontShorthand(computedStyle),
      pretextOptions(computedStyle),
    );
    const result = layoutWithLines(prepared, rect.width, lineHeight);
    if (result.lineCount <= 1) {
      continue;
    }

    const target = sourceDocument.querySelector(
      `[data-pretext-id="${measuredElement.getAttribute("data-pretext-id")}"]`,
    );
    if (!target) {
      continue;
    }

    const lineMarkup = result.lines.map((line) => escapeHtml(line.text)).join("<br>");
    target.innerHTML = lineMarkup;
    changed.push({
      tag: target.tagName.toLowerCase(),
      text: text.slice(0, 80),
      lines: result.lineCount,
      width: Math.round(rect.width * 100) / 100,
      height: Math.round(result.height * 100) / 100,
      domHeight: Math.round(rect.height * 100) / 100,
    });
  }

  for (const node of sourceDocument.querySelectorAll("[data-pretext-id]")) {
    node.removeAttribute("data-pretext-id");
  }

  return {
    html: `<!doctype html>\n${sourceDocument.documentElement.outerHTML}`,
    report: {
      enabled: true,
      candidates: candidateCount,
      applied: changed.length,
      changed: changed.slice(0, 20),
    },
  };
}

async function boot() {
  setStatus("Starting worker...");
  worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  htmlInput.value = SAMPLE_HTML;
  baseUrlInput.value = new URL(".", window.location.href).toString();
  diagnostics.textContent = JSON.stringify(
    { warnings: [], assets: [], console_messages: [], pretext: { enabled: false } },
    null,
    2,
  );

  const fontAssets = await fetchFontAssets();
  setStatus("Loading wasm module in worker...");
  await callWorker(
    {
      type: "init",
      fonts: fontAssets.map((asset) => ({ url: asset.url, bytes: asset.bytes.buffer })),
    },
    fontAssets.map((asset) => asset.bytes.buffer),
  );
  setStatus("Ready");
}

async function renderCurrentTemplate() {
  const rawHtml = htmlInput.value;
  const width = Number(widthInput.value);
  const viewportHeight = Number(viewportHeightInput.value);
  const scale = Number(scaleInput.value);
  const baseUrl = baseUrlInput.value.trim() || window.location.href;

  renderButton.disabled = true;
  setStatus("Preparing HTML...");

  try {
    let html = rawHtml;
    let pretextReport = { enabled: false };

    if (usePretextInput.checked) {
      setStatus("Measuring text with Pretext...");
      const prepared = await applyPretextHints(rawHtml, baseUrl, width);
      html = prepared.html;
      pretextReport = prepared.report;
    }

    setStatus("Fetching linked assets...");
    const assets = await fetchAssetGraph(html, baseUrl);
    setStatus(`Rendering with ${assets.length} injected assets...`);
    const response = await callWorker(
      {
        type: "render",
        html,
        width,
        viewportHeight,
        scale,
        baseUrl,
        assets: assets.map((asset) => ({ url: asset.url, bytes: asset.bytes.buffer })),
      },
      assets.map((asset) => asset.bytes.buffer),
    );

    if (currentObjectUrl) {
      URL.revokeObjectURL(currentObjectUrl);
    }
    currentObjectUrl = URL.createObjectURL(
      new Blob([new Uint8Array(response.png)], { type: "image/png" }),
    );
    preview.src = currentObjectUrl;
    diagnostics.textContent = JSON.stringify(
      {
        ...response.diagnostics,
        pretext: pretextReport,
      },
      null,
      2,
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
