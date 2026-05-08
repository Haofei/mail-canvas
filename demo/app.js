import { createMailCanvasRenderer } from "../browser/mail-canvas-browser.js";

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
            <p class="body-copy">Edit the HTML on the left, keep linked CSS or images relative to the base URL, then render again. Diagnostics below show which assets were loaded or blocked. Emoji fallback is bundled for preview QA 🚀</p>
            <a class="button" href="https://github.com/Haofei/mail-canvas">Open Repository</a>
            <div class="footer">Bundled demo fonts: Noto Sans Regular + Bold + Noto Color Emoji.</div>
          </td>
        </tr>
      </table>
    </div>
  </body>
</html>`;

const DEMO_FONTS = [
  "./assets/NotoSans-Regular.ttf",
  "./assets/NotoSans-Bold.ttf",
  "/fixtures/fonts/NotoColorEmoji.ttf",
];

let renderer;
let currentObjectUrl = null;

function setStatus(message) {
  status.textContent = message;
}

async function boot() {
  setStatus("Starting worker...");
  htmlInput.value = SAMPLE_HTML;
  baseUrlInput.value = new URL(".", window.location.href).toString();
  diagnostics.textContent = JSON.stringify(
    { warnings: [], assets: [], console_messages: [] },
    null,
    2,
  );

  renderer = await createMailCanvasRenderer({
    baseUrl: baseUrlInput.value,
    workerUrl: new URL("../browser/mail-canvas-worker.js", import.meta.url),
    fonts: DEMO_FONTS,
    limits: {
      maxAssetBytes: 10 * 1024 * 1024,
      maxTotalAssetBytes: 64 * 1024 * 1024,
      maxAssetCount: 128,
    },
  });
  setStatus("Ready");
}

async function renderCurrentTemplate() {
  const html = htmlInput.value;
  const width = Number(widthInput.value);
  const height = Number(viewportHeightInput.value);
  const scale = Number(scaleInput.value);
  const baseUrl = baseUrlInput.value.trim() || window.location.href;

  renderButton.disabled = true;
  setStatus("Rendering...");

  try {
    const result = await renderer.renderThumbnail({
      html,
      width,
      height,
      scale,
      baseUrl,
    });

    if (currentObjectUrl) {
      URL.revokeObjectURL(currentObjectUrl);
    }
    currentObjectUrl = URL.createObjectURL(result.blob);
    preview.src = currentObjectUrl;
    diagnostics.textContent = JSON.stringify(
      {
        timing: result.timing,
        assets: result.assets,
        diagnostics: result.diagnostics,
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
