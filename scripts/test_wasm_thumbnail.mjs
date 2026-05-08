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
  assert.ok(report.repeatedImage.pngBytes > 100_000, "expected repeated-image PNG output");
  assert.ok(report.repeatedImage.renderMs > 0, "expected positive repeated-image renderMs");
  assert.ok(
    report.repeatedImage.diagnosticsAssets >= 1,
    "expected repeated-image asset diagnostics",
  );
  assert.equal(report.repeatedImage.transferredAssets, 1);
  assert.ok(report.repeatedImageCached.pngBytes > 100_000, "expected cached PNG output");
  assert.equal(report.repeatedImageCached.transferredAssets, 0);
  assert.equal(report.wrapperChecks.destroyRejects, true);
  assert.equal(report.wrapperChecks.limitRejects, true);
  assert.equal(report.wrapperChecks.defaultEmojiLoads, true);
  console.log(JSON.stringify(report, null, 2));
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
