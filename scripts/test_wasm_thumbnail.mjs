#!/usr/bin/env node

import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { runWasmThumbnail } from "./wasm_thumbnail_runner.mjs";

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

async function main() {
  const report = await runWasmThumbnail(ROOT_DIR);
  assert.equal(report.case, "wasm-thumbnail-800x1200");
  assert.equal(report.width, 800);
  assert.equal(report.height, 1200);
  assert.ok(report.pngBytes > 100_000, `expected non-empty PNG, got ${report.pngBytes}`);
  assert.ok(report.timing.totalMs > 0, "expected positive totalMs");
  assert.ok(report.timing.renderMs > 0, "expected positive renderMs");
  assert.equal(report.diagnostics.warnings, 0);
  assert.ok(report.diagnostics.assets >= 1, "expected at least one asset diagnostic");
  console.log(JSON.stringify(report, null, 2));
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
