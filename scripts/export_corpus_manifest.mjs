#!/usr/bin/env node

import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { TEMPLATE_CORPUS } from './templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const CORPUS_DIR = path.join(ROOT_DIR, 'corpus');
const MANIFEST_PATH = path.join(CORPUS_DIR, 'manifest.json');

async function main() {
  await mkdir(CORPUS_DIR, { recursive: true });
  await writeFile(
    MANIFEST_PATH,
    `${JSON.stringify(
      {
        generatedAt: new Date().toISOString(),
        templates: TEMPLATE_CORPUS,
      },
      null,
      2,
    )}\n`,
  );
  console.log(MANIFEST_PATH);
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
