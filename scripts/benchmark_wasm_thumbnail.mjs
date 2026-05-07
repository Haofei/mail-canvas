#!/usr/bin/env node

import { spawn } from "node:child_process";
import { writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function parseArgs(argv) {
  const args = {
    out: null,
    markdownOut: null,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) {
        throw new Error(`missing value for ${arg}`);
      }
      return argv[index];
    };
    switch (arg) {
      case "--out":
        args.out = path.resolve(next());
        break;
      case "--markdown-out":
        args.markdownOut = path.resolve(next());
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  await runOrThrow("bash", [path.join(ROOT_DIR, "scripts", "build_wasm_demo.sh")]);
  const server = await startDemoServer();
  try {
    const report = await runBenchmark(server.url);
    if (args.out) {
      await writeFile(args.out, `${JSON.stringify(report, null, 2)}\n`);
      console.log(args.out);
    } else {
      console.log(JSON.stringify(report, null, 2));
    }
    if (args.markdownOut) {
      await writeFile(args.markdownOut, benchmarkMarkdown(report));
    }
  } finally {
    server.process.kill("SIGTERM");
  }
}

async function runBenchmark(baseUrl) {
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({
      viewport: { width: 1200, height: 900 },
      deviceScaleFactor: 1,
    });
    try {
      await page.goto(baseUrl, { waitUntil: "load" });
      return await page.evaluate(async () => {
        async function createHeroDataUrl(width, height) {
          const canvas = document.createElement("canvas");
          canvas.width = width;
          canvas.height = height;
          const context = canvas.getContext("2d");
          const gradient = context.createLinearGradient(0, 0, width, height);
          gradient.addColorStop(0, "#2563eb");
          gradient.addColorStop(0.45, "#41c7a8");
          gradient.addColorStop(1, "#ffca58");
          context.fillStyle = gradient;
          context.fillRect(0, 0, width, height);
          context.fillStyle = "rgba(15, 23, 42, 0.26)";
          for (let x = -80; x < width; x += 220) {
            context.fillRect(x, 0, 96, height);
          }
          return canvas.toDataURL("image/png");
        }

        function thumbnailHtml(hero) {
          return `<!doctype html>
<html>
<body style="margin:0;background:#f4f7fb;font-family:Arial,Helvetica,sans-serif;color:#172033">
<table width="800" cellpadding="0" cellspacing="0" role="presentation" style="width:800px;height:1200px;background:#fff">
  <tr><td style="padding:0"><img src="${hero}" width="800" style="display:block;width:800px;height:auto" alt="hero"></td></tr>
  <tr><td style="padding:34px 60px">
    <div style="font-size:14px;letter-spacing:2px;text-transform:uppercase;color:#4577b9">WASM benchmark</div>
    <div style="font-size:42px;line-height:48px;font-weight:700;margin-top:14px">Browser worker thumbnail render</div>
    <p style="font-size:18px;line-height:28px;color:#536176;margin:18px 0 0">This fixed 800 by 1200 case exercises the public browser wrapper, font registration, diagnostics parsing, and WASM rendering path.</p>
  </td></tr>
  <tr><td style="padding:10px 60px 34px">
    <table width="680" cellpadding="0" cellspacing="0" role="presentation">
      <tr>
        <td width="320" style="padding:24px;background:#eef4fb;vertical-align:top"><h2 style="font-size:23px;line-height:30px;margin:0 0 12px">Wrapper API</h2><p style="font-size:16px;line-height:25px;margin:0;color:#536176">The demo calls createMailCanvasRenderer and renderThumbnail instead of raw wasm bindings.</p></td>
        <td width="40"></td>
        <td width="320" style="padding:24px;background:#f8efdc;vertical-align:top"><h2 style="font-size:23px;line-height:30px;margin:0 0 12px">Worker path</h2><p style="font-size:16px;line-height:25px;margin:0;color:#536176">The renderer runs off the main thread and returns a PNG buffer plus diagnostics.</p></td>
      </tr>
    </table>
  </td></tr>
  <tr><td style="height:184px;padding:28px 60px;background:#10233f;color:#c8d7e8;font-size:14px;line-height:22px;vertical-align:top">Footer text and preference links. Output target: 800 x 1200 CSS pixels.</td></tr>
</table>
</body>
</html>`;
        }

        const { createMailCanvasRenderer } = await import("/browser/mail-canvas-browser.js");
        const hero = await createHeroDataUrl(1400, 650);
        const renderer = await createMailCanvasRenderer({
          baseUrl: window.location.href,
          workerUrl: new URL("/worker.js", window.location.href),
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

function startDemoServer() {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(ROOT_DIR, "scripts", "serve_demo.mjs")], {
      cwd: ROOT_DIR,
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

function runOrThrow(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: ROOT_DIR,
      stdio: "inherit",
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${command} ${args.join(" ")} exited with ${code}`));
    });
  });
}

function benchmarkMarkdown(report) {
  return `# WASM Thumbnail Benchmark

| Case | Size | Total | Fetch | Render | PNG | Warnings | Assets |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ${report.case} | ${report.width}x${report.height} | ${formatMs(report.timing.totalMs)} | ${formatMs(report.timing.fetchMs)} | ${formatMs(report.timing.renderMs)} | ${formatBytes(report.pngBytes)} | ${report.diagnostics.warnings} | ${report.diagnostics.assets} |
`;
}

function formatMs(value) {
  return `${Number(value).toFixed(1)}ms`;
}

function formatBytes(value) {
  if (value > 1024 * 1024) {
    return `${(value / 1024 / 1024).toFixed(2)}MB`;
  }
  return `${Math.round(value / 1024)}KB`;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
