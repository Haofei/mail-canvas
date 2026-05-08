#!/usr/bin/env node

import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const ROOT_DIR = path.resolve(new URL('..', import.meta.url).pathname);
const renderer = path.join(ROOT_DIR, 'target', 'debug', 'mail-canvas');

async function main() {
  const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mail-canvas-node-render-'));
  try {
    const htmlPath = path.join(tmpDir, 'template.html');
    const outputPath = path.join(tmpDir, 'output.png');
    await writeFile(
      htmlPath,
      '<table width="600" cellpadding="24" cellspacing="0"><tr><td style="font-family:Arial,sans-serif;font-size:18px;line-height:150%;background:#f5f7fb;color:#111827">Hello from Node.</td></tr></table>',
    );
    await execFileAsync(
      renderer,
      ['render', '--html', htmlPath, '--output', outputPath, '--width', '600'],
      { cwd: ROOT_DIR },
    );
    const png = await readFile(outputPath);
    console.log(`rendered ${png.length} bytes to ${outputPath}`);
  } finally {
    await rm(tmpDir, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
