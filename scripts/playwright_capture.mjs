#!/usr/bin/env node

import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { chromium } from 'playwright';
import { pathToFileURL } from 'node:url';

function parseArgs(argv) {
  const args = {
    width: 600,
    timeoutMs: 15000,
    html: null,
    output: null,
    baseUrl: '',
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
      case '--html':
        args.html = path.resolve(next());
        break;
      case '--output':
        args.output = path.resolve(next());
        break;
      case '--width':
        args.width = Number.parseInt(next(), 10);
        break;
      case '--timeout-ms':
        args.timeoutMs = Number.parseInt(next(), 10);
        break;
      case '--base-url':
        args.baseUrl = next();
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  if (!args.html || !args.output) {
    throw new Error('--html and --output are required');
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const rawHtml = await readFile(args.html, 'utf8');
  const baseUrl = args.baseUrl || new URL('.', pathToFileURL(args.html).href).href;
  const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mail-canvas-browser-capture-'));
  const preparedPath = path.join(tmpDir, 'prepared.html');
  await writeFile(preparedPath, buildBrowserDocument(rawHtml, baseUrl, args.width));

  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({
      viewport: { width: args.width, height: 900 },
      deviceScaleFactor: 1,
    });
    try {
      await page.goto(pathToFileURL(preparedPath).href, {
        waitUntil: 'load',
        timeout: args.timeoutMs,
      });
      await page.waitForTimeout(500);
      const height = await page.evaluate(() => {
        let maxBottom = 0;
        for (const element of document.body.querySelectorAll('*')) {
          const rect = element.getBoundingClientRect();
          if (rect.width > 0 || rect.height > 0) {
            maxBottom = Math.max(maxBottom, rect.bottom);
          }
        }
        return Math.max(
          1,
          Math.ceil(maxBottom || document.body.getBoundingClientRect().bottom),
          document.documentElement.scrollHeight,
          document.body.scrollHeight,
        );
      });
      await page.setViewportSize({ width: args.width, height });
      await page.waitForTimeout(100);
      await page.screenshot({
        path: args.output,
        clip: { x: 0, y: 0, width: args.width, height },
      });
    } finally {
      await page.close();
    }
  } finally {
    await browser.close();
    await rm(tmpDir, { recursive: true, force: true });
  }
}

function buildBrowserDocument(sourceHtml, baseUrl, width) {
  const head = [
    '<meta charset="utf-8">',
    `<base href="${escapeAttr(baseUrl)}">`,
    '<style>',
    'html, body { margin: 0; padding: 0; }',
    `body { width: ${width}px; min-width: ${width}px; overflow: visible; background: #fff; }`,
    '#email-render-root { width: 100%; }',
    'table { border-collapse: separate; border-spacing: 0; }',
    'img { display: block; }',
    '</style>',
  ].join('\n');
  const lower = sourceHtml.toLowerCase();
  const looksLikeDocument =
    lower.includes('<!doctype') ||
    lower.includes('<html') ||
    lower.includes('<body') ||
    lower.includes('<head');
  if (!looksLikeDocument) {
    return `<!doctype html><html><head>${head}</head><body><div id="email-render-root">${sourceHtml}</div></body></html>`;
  }
  const headStart = lower.indexOf('<head');
  if (headStart >= 0) {
    const closeOffset = sourceHtml.slice(headStart).indexOf('>');
    if (closeOffset >= 0) {
      const insertAt = headStart + closeOffset + 1;
      return `${sourceHtml.slice(0, insertAt)}${head}${sourceHtml.slice(insertAt)}`;
    }
  }
  const htmlStart = lower.indexOf('<html');
  if (htmlStart >= 0) {
    const closeOffset = sourceHtml.slice(htmlStart).indexOf('>');
    if (closeOffset >= 0) {
      const insertAt = htmlStart + closeOffset + 1;
      return `${sourceHtml.slice(0, insertAt)}<head>${head}</head>${sourceHtml.slice(insertAt)}`;
    }
  }
  return `<!doctype html><html><head>${head}</head>${sourceHtml}</html>`;
}

function escapeAttr(value) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('"', '&quot;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
