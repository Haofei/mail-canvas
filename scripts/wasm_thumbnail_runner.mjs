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
        async function rejectsWithMessage(action, expectedMessage) {
          try {
            await action();
          } catch (error) {
            return String(error?.message || error).includes(expectedMessage);
          }
          return false;
        }

        const [
          { createMailCanvasRenderer },
          { createHeroDataUrl, repeatedImageHtml, thumbnailHtml },
        ] = await Promise.all([
          import("/browser/mail-canvas-browser.js"),
          import("/scripts/wasm_thumbnail_fixture.mjs"),
        ]);
        const hero = await createHeroDataUrl(1400, 650);
        const heroBlob = await fetch(hero).then((response) => response.blob());
        const heroBlobUrl = URL.createObjectURL(heroBlob);
        const workerUrl = new URL("/browser/mail-canvas-worker.js", window.location.href);
        const originalFetch = window.fetch.bind(window);
        let emojiFontFetches = 0;
        window.fetch = (input, init) => {
          const url = typeof input === "string" ? input : String(input?.url || input);
          if (url.includes("NotoColorEmoji.ttf")) {
            emojiFontFetches += 1;
          }
          return originalFetch(input, init);
        };
        const renderer = await createMailCanvasRenderer({
          baseUrl: window.location.href,
          workerUrl,
          fonts: [
            "./assets/NotoSans-Regular.ttf",
            "./assets/NotoSans-Bold.ttf",
          ],
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
          const repeatedStarted = performance.now();
          const repeatedResult = await renderer.renderThumbnail({
            html: repeatedImageHtml(heroBlobUrl),
            width: 800,
            height: 1200,
            scale: 1,
            baseUrl: window.location.href,
          });
          const repeatedTotalMs = performance.now() - repeatedStarted;
          const repeatedCachedStarted = performance.now();
          const repeatedCachedResult = await renderer.renderThumbnail({
            html: repeatedImageHtml(heroBlobUrl),
            width: 800,
            height: 1200,
            scale: 1,
            baseUrl: window.location.href,
          });
          const repeatedCachedTotalMs = performance.now() - repeatedCachedStarted;
          renderer.destroy();
          const destroyRejects = await rejectsWithMessage(
            () =>
              renderer.renderThumbnail({
                html: "<p>after destroy</p>",
                width: 100,
                height: 100,
              }),
            "destroyed",
          );
          const limitRejects = await rejectsWithMessage(async () => {
            const limitedRenderer = await createMailCanvasRenderer({
              baseUrl: window.location.href,
              workerUrl,
              limits: {
                maxAssetBytes: 1,
                maxTotalAssetBytes: 1024,
                maxAssetCount: 8,
              },
            });
            try {
              await limitedRenderer.renderThumbnail({
                html: '<link rel="stylesheet" href="./assets/sample-email.css"><p>limit</p>',
                width: 120,
                height: 120,
                baseUrl: window.location.href,
              });
            } finally {
              limitedRenderer.destroy();
            }
          }, "maxAssetBytes");
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
            repeatedImage: {
              pngBytes: repeatedResult.png.byteLength,
              totalMs: repeatedTotalMs,
              fetchMs: repeatedResult.timing.fetchMs,
              renderMs: repeatedResult.timing.renderMs,
              diagnosticsAssets: repeatedResult.diagnostics.assets.length,
              transferredAssets: repeatedResult.assets.transferred,
            },
            repeatedImageCached: {
              pngBytes: repeatedCachedResult.png.byteLength,
              totalMs: repeatedCachedTotalMs,
              fetchMs: repeatedCachedResult.timing.fetchMs,
              renderMs: repeatedCachedResult.timing.renderMs,
              diagnosticsAssets: repeatedCachedResult.diagnostics.assets.length,
              transferredAssets: repeatedCachedResult.assets.transferred,
            },
            wrapperChecks: {
              destroyRejects,
              limitRejects,
              defaultEmojiLoads: emojiFontFetches === 1,
            },
          };
        } finally {
          window.fetch = originalFetch;
          URL.revokeObjectURL(heroBlobUrl);
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
