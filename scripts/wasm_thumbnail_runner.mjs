import { spawn } from "node:child_process";
import path from "node:path";
import { chromium } from "playwright";

export async function runWasmThumbnail(rootDir) {
  const server = await startDemoServer(rootDir);
  try {
    return await runInBrowser(server.url);
  } finally {
    server.process.kill("SIGTERM");
  }
}

async function runInBrowser(baseUrl) {
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({
      viewport: { width: 1200, height: 900 },
      deviceScaleFactor: 1,
    });
    try {
      await page.goto(baseUrl, { waitUntil: "load" });
      return await page.evaluate(async () => {
        const [{ createMailCanvasRenderer }, { createHeroDataUrl, thumbnailHtml }] =
          await Promise.all([
            import("/browser/mail-canvas-browser.js"),
            import("/scripts/wasm_thumbnail_fixture.mjs"),
          ]);
        const hero = await createHeroDataUrl(1400, 650);
        const renderer = await createMailCanvasRenderer({
          baseUrl: window.location.href,
          workerUrl: new URL("/browser/mail-canvas-worker.js", window.location.href),
          fonts: ["./assets/NotoSans-Regular.ttf", "./assets/NotoSans-Bold.ttf"],
          limits: {
            maxAssetBytes: 10 * 1024 * 1024,
            maxTotalAssetBytes: 64 * 1024 * 1024,
            maxAssetCount: 128,
          },
        });
        try {
          const started = performance.now();
          const result = await renderer.renderThumbnail({
            html: thumbnailHtml(hero),
            width: 800,
            height: 1200,
            scale: 1,
            baseUrl: window.location.href,
          });
          const totalMs = performance.now() - started;
          return {
            generatedAt: new Date().toISOString(),
            case: "wasm-thumbnail-800x1200",
            width: 800,
            height: 1200,
            pngBytes: result.png.byteLength,
            timing: {
              totalMs,
              fetchMs: result.timing.fetchMs,
              renderMs: result.timing.renderMs,
            },
            diagnostics: {
              warnings: result.diagnostics.warnings.length,
              assets: result.diagnostics.assets.length,
              registeredAssets: result.assets.registered,
            },
          };
        } finally {
          renderer.destroy();
        }
      });
    } finally {
      await page.close();
    }
  } finally {
    await browser.close();
  }
}

function startDemoServer(rootDir) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(rootDir, "scripts", "serve_demo.mjs")], {
      cwd: rootDir,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const onData = (chunk) => {
      const text = chunk.toString("utf8");
      const match = text.match(/Demo server running at (http:\/\/[^\s]+)/);
      if (match) {
        child.stdout.off("data", onData);
        resolve({ process: child, url: match[1] });
      }
    };
    child.stdout.on("data", onData);
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code !== null && code !== 0) {
        reject(new Error(`demo server exited with code ${code}`));
      }
    });
  });
}
