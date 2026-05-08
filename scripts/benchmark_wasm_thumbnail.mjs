#!/usr/bin/env node

import { writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { runWasmThumbnail } from "./wasm_thumbnail_runner.mjs";

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
  const report = await runWasmThumbnail(ROOT_DIR);
  if (args.out) {
    await writeFile(args.out, `${JSON.stringify(report, null, 2)}\n`);
    console.log(args.out);
  } else {
    console.log(JSON.stringify(report, null, 2));
  }
  if (args.markdownOut) {
    await writeFile(args.markdownOut, benchmarkMarkdown(report));
  }
}

function benchmarkMarkdown(report) {
  return `# WASM Thumbnail Benchmark

| Case | Size | Total | Fetch | Render | PNG | Warnings | Assets |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ${report.case} | ${report.width}x${report.height} | ${formatMs(report.timing.totalMs)} | ${formatMs(report.timing.fetchMs)} | ${formatMs(report.timing.renderMs)} | ${formatBytes(report.pngBytes)} | ${report.diagnostics.warnings} | ${report.diagnostics.assets} |

| Extra Case | Total | Fetch | Render | PNG | Assets | Transferred |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| repeated-image | ${formatMs(report.repeatedImage.totalMs)} | ${formatMs(report.repeatedImage.fetchMs)} | ${formatMs(report.repeatedImage.renderMs)} | ${formatBytes(report.repeatedImage.pngBytes)} | ${report.repeatedImage.diagnosticsAssets} | ${report.repeatedImage.transferredAssets} |
| repeated-image cached | ${formatMs(report.repeatedImageCached.totalMs)} | ${formatMs(report.repeatedImageCached.fetchMs)} | ${formatMs(report.repeatedImageCached.renderMs)} | ${formatBytes(report.repeatedImageCached.pngBytes)} | ${report.repeatedImageCached.diagnosticsAssets} | ${report.repeatedImageCached.transferredAssets} |
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
