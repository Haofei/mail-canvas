#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { mirrorHtml } from './vendor_corpus_templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const MJML_DIR = path.join(ROOT_DIR, 'corpus', 'mjml');
const MJML_VERSION = '4.16.1';

const TEMPLATES = [
  'alert',
  'arturia',
  'austin',
  'basic',
  'black-friday',
  'card',
  'christmas',
  'delivery',
  'happy-new-year',
  'loyalty-client',
  'newsletter',
  'onepage',
  'proof',
  'reactivation-email',
  'real-estate',
  'referral-email',
  'tech-geek',
  'welcome-email',
];

async function main() {
  await mkdir(MJML_DIR, { recursive: true });

  for (const name of TEMPLATES) {
    const rawUrl = `https://raw.githubusercontent.com/mjmlio/email-templates/master/templates/${name}.mjml`;
    const response = await fetch(rawUrl);
    if (!response.ok) {
      throw new Error(`${rawUrl}: ${response.status} ${response.statusText}`);
    }

    const mjml = await response.text();
    const compiled = compileMjml(mjml, rawUrl);
    const htmlPath = path.join(MJML_DIR, `mjml-${name}.html`);
    const assetDir = path.join(MJML_DIR, `mjml-${name}.assets`);
    await rm(assetDir, { recursive: true, force: true });
    await mkdir(assetDir, { recursive: true });
    const mirrored = await mirrorHtml(compiled, rawUrl, assetDir);
    await writeFile(htmlPath, `${mirrored.trimEnd()}\n`, 'utf8');
    console.log(`vendored mjml-${name}`);
  }
}

function compileMjml(mjml, rawUrl) {
  const command = process.platform === 'win32' ? 'npx.cmd' : 'npx';
  const result = spawnSync(
    command,
    ['--yes', `mjml@${MJML_VERSION}`, '--stdin', '--stdout', '--noStdoutFileComment'],
    {
      cwd: ROOT_DIR,
      input: mjml,
      encoding: 'utf8',
      maxBuffer: 20 * 1024 * 1024,
    },
  );
  if (result.status !== 0) {
    throw new Error(`mjml compile failed for ${rawUrl}:\n${result.stdout}\n${result.stderr}`);
  }
  return result.stdout;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
