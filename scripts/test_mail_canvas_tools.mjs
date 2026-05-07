#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { readFile, rm, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const TOOL = path.join(ROOT_DIR, 'scripts', 'mail_canvas_tools.mjs');
const WORK_DIR = '/tmp/mail-canvas-tools-smoke';
const BASIC_HTML = path.join(ROOT_DIR, 'examples', 'basic.html');

async function main() {
  await rm(WORK_DIR, { recursive: true, force: true });

  run(['check', BASIC_HTML, '--out-dir', path.join(WORK_DIR, 'check'), '--warnings-json', path.join(WORK_DIR, 'check', 'warnings.json')]);
  await assertFile(path.join(WORK_DIR, 'check', 'warnings.json'));

  run(['preview', BASIC_HTML, '--out-dir', path.join(WORK_DIR, 'preview')]);
  await assertFile(path.join(WORK_DIR, 'preview', 'preview.png'));
  await assertFile(path.join(WORK_DIR, 'preview', 'warnings.json'));

  run(['diff', BASIC_HTML, BASIC_HTML, '--out', path.join(WORK_DIR, 'diff')]);
  await assertFile(path.join(WORK_DIR, 'diff', 'side-by-side.png'));
  const diffReport = JSON.parse(await readFile(path.join(WORK_DIR, 'diff', 'report.json'), 'utf8'));
  if (diffReport.diffPixels !== 0) {
    throw new Error(`expected identical diff to be zero, got ${diffReport.diffPixels}`);
  }

  const missing = run(
    ['snapshot', path.join(ROOT_DIR, 'examples', '*.html'), '--baseline', path.join(WORK_DIR, 'snapshots')],
    { expectFailure: true },
  );
  if (!missing.stderr.includes('missing') && !missing.stdout.includes('missing')) {
    throw new Error('expected missing snapshot baseline to be reported');
  }

  run(['snapshot', path.join(ROOT_DIR, 'examples', '*.html'), '--baseline', path.join(WORK_DIR, 'snapshots'), '--update']);
  run(['snapshot', path.join(ROOT_DIR, 'examples', '*.html'), '--baseline', path.join(WORK_DIR, 'snapshots')]);
  await assertFile(path.join(WORK_DIR, 'snapshots', 'manifest.json'));

  console.log('mail-canvas tools smoke test passed');
}

function run(args, options = {}) {
  const result = spawnSync(process.execPath, [TOOL, ...args], {
    cwd: ROOT_DIR,
    encoding: 'utf8',
    stdio: 'pipe',
  });
  const failed = result.status !== 0;
  if (options.expectFailure) {
    if (!failed) {
      throw new Error(`expected command to fail: ${args.join(' ')}`);
    }
    return result;
  }
  if (failed) {
    throw new Error(`command failed: ${args.join(' ')}\n${result.stdout}\n${result.stderr}`);
  }
  return result;
}

async function assertFile(file) {
  const info = await stat(file).catch(() => null);
  if (!info?.isFile() || info.size === 0) {
    throw new Error(`expected non-empty file: ${file}`);
  }
}

main().catch((error) => {
  console.error(error?.stack ?? error?.message ?? String(error));
  process.exitCode = 1;
});
