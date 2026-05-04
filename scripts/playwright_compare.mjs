#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import pixelmatch from 'pixelmatch';
import { chromium } from 'playwright';
import { PNG } from 'pngjs';
import { TEMPLATES } from './templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_WORK_DIR = '/tmp/email-render-playwright-compare';
const DEFAULT_WIDTH = 600;
const DEFAULT_TIMEOUT_MS = 15000;

function parseArgs(argv) {
  const args = {
    width: DEFAULT_WIDTH,
    workDir: DEFAULT_WORK_DIR,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    limit: TEMPLATES.length,
    only: [],
    allowRemote: true,
    keep: false,
    expectations: null,
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
      case '--width':
        args.width = Number.parseInt(next(), 10);
        break;
      case '--work-dir':
        args.workDir = path.resolve(next());
        break;
      case '--timeout-ms':
        args.timeoutMs = Number.parseInt(next(), 10);
        break;
      case '--limit':
        args.limit = Number.parseInt(next(), 10);
        break;
      case '--only':
        args.only.push(next());
        break;
      case '--no-remote':
        args.allowRemote = false;
        break;
      case '--keep':
        args.keep = true;
        break;
      case '--expectations':
        args.expectations = path.resolve(next());
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!Number.isFinite(args.width) || args.width <= 0) {
    throw new Error('--width must be a positive integer');
  }
  if (!Number.isFinite(args.timeoutMs) || args.timeoutMs <= 0) {
    throw new Error('--timeout-ms must be a positive integer');
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const expectations = args.expectations ? await loadExpectations(args.expectations) : null;
  if (expectations?.width && args.width === DEFAULT_WIDTH) {
    args.width = expectations.width;
  }
  if (!args.keep) {
    await rm(args.workDir, { recursive: true, force: true });
  }

  const dirs = {
    html: path.join(args.workDir, 'html'),
    prepared: path.join(args.workDir, 'prepared'),
    browser: path.join(args.workDir, 'browser'),
    rust: path.join(args.workDir, 'rust'),
    diff: path.join(args.workDir, 'diff'),
    sideBySide: path.join(args.workDir, 'side-by-side'),
    logs: path.join(args.workDir, 'logs'),
  };
  await Promise.all(Object.values(dirs).map((dir) => mkdir(dir, { recursive: true })));

  const renderer = await ensureRenderer();
  const templates = selectTemplates(args, expectations);
  if (templates.length === 0) {
    throw new Error(`no templates matched --only: ${args.only.join(', ')}`);
  }
  const downloaded = [];
  for (const [name, url] of templates) {
    const html = await download(url, args.timeoutMs);
    const htmlPath = path.join(dirs.html, `${name}.html`);
    await writeFile(htmlPath, html);
    downloaded.push({ name, url, htmlPath });
  }

  const browser = await chromium.launch({ headless: true });
  const results = [];
  try {
    for (const template of downloaded) {
      const result = await compareTemplate(template, args, dirs, renderer, browser);
      results.push(result);
      printResult(result);
    }
  } finally {
    await browser.close();
  }

  await writeFile(
    path.join(args.workDir, 'comparison.json'),
    `${JSON.stringify({ generatedAt: new Date().toISOString(), args, results }, null, 2)}\n`,
  );
  const failures = expectations ? checkExpectations(results, expectations) : [];
  await writeFile(path.join(args.workDir, 'report.md'), renderMarkdownReport(results, args, expectations));
  console.log(`outputs: ${args.workDir}`);
  if (failures.length > 0) {
    for (const failure of failures) {
      console.error(failure);
    }
    process.exitCode = 1;
  }
}

async function loadExpectations(expectationsPath) {
  const raw = await readFile(expectationsPath, 'utf8');
  return JSON.parse(raw);
}

function selectTemplates(args, expectations) {
  const expectedNames = expectations ? Object.keys(expectations.templates ?? {}) : [];
  const wanted =
    args.only.length > 0
      ? args.only
      : expectedNames.length > 0
        ? expectedNames
        : TEMPLATES.slice(0, args.limit).map(([name]) => name);
  const wantedSet = new Set(wanted);
  const templates = TEMPLATES.filter(([name]) => wantedSet.has(name));
  const found = new Set(templates.map(([name]) => name));
  const missing = wanted.filter((name) => !found.has(name));
  if (missing.length > 0) {
    throw new Error(`unknown template names: ${missing.join(', ')}`);
  }
  return templates;
}

async function ensureRenderer() {
  const renderer = path.join(ROOT_DIR, 'target', 'debug', 'email-render');
  const build = spawnSync('cargo', ['build'], {
    cwd: ROOT_DIR,
    encoding: 'utf8',
    stdio: 'pipe',
  });
  if (build.status !== 0) {
    throw new Error(`cargo build failed:\n${build.stdout}\n${build.stderr}`);
  }
  return renderer;
}

async function download(url, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(url, { signal: controller.signal });
    if (!response.ok) {
      throw new Error(`${response.status} ${response.statusText}`);
    }
    return await response.text();
  } finally {
    clearTimeout(timer);
  }
}

async function compareTemplate(template, args, dirs, renderer, browser) {
  const browserPath = path.join(dirs.browser, `${template.name}.png`);
  const rustPath = path.join(dirs.rust, `${template.name}.png`);
  const diffPath = path.join(dirs.diff, `${template.name}.png`);
  const sideBySidePath = path.join(dirs.sideBySide, `${template.name}.png`);
  const logPath = path.join(dirs.logs, `${template.name}.log`);
  const preparedPath = path.join(dirs.prepared, `${template.name}.html`);

  const sourceHtml = await readFile(template.htmlPath, 'utf8');
  const baseUrl = new URL('.', template.url).href;
  await writeFile(preparedPath, buildBrowserDocument(sourceHtml, baseUrl, args.width));

  const browserMetrics = await browserScreenshot(
    browser,
    preparedPath,
    browserPath,
    args.width,
    args.timeoutMs,
  );

  const renderArgs = [
    '--html',
    template.htmlPath,
    '--output',
    rustPath,
    '--width',
    String(args.width),
    '--timeout-ms',
    String(args.timeoutMs),
    '--base-url',
    baseUrl,
  ];
  if (args.allowRemote) {
    renderArgs.push('--allow-remote');
    renderArgs.push('--allow-http');
  }
  const render = spawnSync(renderer, renderArgs, {
    cwd: ROOT_DIR,
    encoding: 'utf8',
    stdio: 'pipe',
  });
  await writeFile(logPath, `${render.stdout}${render.stderr}`);
  if (render.status !== 0) {
    throw new Error(`renderer failed for ${template.name}; see ${logPath}`);
  }

  const comparison = await comparePng(browserPath, rustPath, diffPath, browserMetrics.mediaRects);
  await writeTriptych([browserPath, rustPath, diffPath], sideBySidePath);
  const warningCount = (await readFile(logPath, 'utf8'))
    .split('\n')
    .filter((line) => line.startsWith('console.warn:')).length;
  return {
    name: template.name,
    url: template.url,
    browserPng: browserPath,
    rustPng: rustPath,
    diffPng: diffPath,
    sideBySidePng: sideBySidePath,
    log: logPath,
    browser: comparison.browser,
    rust: comparison.rust,
    compared: comparison.compared,
    diffPixels: comparison.diffPixels,
    diffRatio: comparison.diffRatio,
    media: comparison.media,
    nonMedia: comparison.nonMedia,
    warningCount,
  };
}

async function writeTriptych(paths, outPath) {
  const images = await Promise.all(paths.map(async (file) => PNG.sync.read(await readFile(file))));
  const gutter = 12;
  const width =
    images.reduce((sum, image) => sum + image.width, 0) + gutter * (images.length - 1);
  const height = Math.max(...images.map((image) => image.height));
  const sheet = new PNG({ width, height });
  sheet.data.fill(255);

  let x = 0;
  for (const image of images) {
    copyPng(image, sheet, x, 0);
    x += image.width + gutter;
  }
  await writeFile(outPath, PNG.sync.write(sheet));
}

function copyPng(source, target, offsetX, offsetY) {
  for (let y = 0; y < source.height; y += 1) {
    const sourceStart = y * source.width * 4;
    const targetStart = ((offsetY + y) * target.width + offsetX) * 4;
    source.data.copy(target.data, targetStart, sourceStart, sourceStart + source.width * 4);
  }
}

async function browserScreenshot(browser, htmlPath, outPath, width, timeoutMs) {
  const page = await browser.newPage({
    viewport: { width, height: 900 },
    deviceScaleFactor: 1,
  });
  try {
    await page.goto(pathToFileURL(htmlPath).href, { waitUntil: 'load', timeout: timeoutMs });
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
        document.documentElement.scrollHeight > window.innerHeight
          ? document.documentElement.scrollHeight
          : 0,
        document.body.scrollHeight > window.innerHeight ? document.body.scrollHeight : 0,
      );
    });
    await page.setViewportSize({ width, height });
    await page.waitForTimeout(100);
    await page.screenshot({
      path: outPath,
      clip: { x: 0, y: 0, width, height },
    });
    return {
      mediaRects: await collectMediaRects(page, width, height),
    };
  } finally {
    await page.close();
  }
}

async function collectMediaRects(page, width, height) {
  return page.evaluate(
    ({ width, height }) => {
      const rects = [];
      for (const element of document.body.querySelectorAll('*')) {
        const style = window.getComputedStyle(element);
        const tag = element.tagName.toLowerCase();
        const hasMedia = tag === 'img' || style.backgroundImage !== 'none';
        if (!hasMedia) {
          continue;
        }
        const rect = element.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) {
          continue;
        }
        const x0 = Math.max(0, Math.floor(rect.left));
        const y0 = Math.max(0, Math.floor(rect.top));
        const x1 = Math.min(width, Math.ceil(rect.right));
        const y1 = Math.min(height, Math.ceil(rect.bottom));
        if (x1 > x0 && y1 > y0) {
          rects.push({ x: x0, y: y0, width: x1 - x0, height: y1 - y0 });
        }
      }
      return rects;
    },
    { width, height },
  );
}

async function comparePng(browserPath, rustPath, diffPath, mediaRects = []) {
  const browser = PNG.sync.read(await readFile(browserPath));
  const rust = PNG.sync.read(await readFile(rustPath));
  const width = Math.max(browser.width, rust.width);
  const height = Math.max(browser.height, rust.height);
  const browserCanvas = padPng(browser, width, height);
  const rustCanvas = padPng(rust, width, height);
  const diff = new PNG({ width, height });
  const diffPixels = pixelmatch(
    browserCanvas.data,
    rustCanvas.data,
    diff.data,
    width,
    height,
    { threshold: 0.1, includeAA: true },
  );
  const mediaMask = buildRectMask(width, height, mediaRects);
  const media = compareMaskedPng(browserCanvas, rustCanvas, width, height, mediaMask, true);
  const nonMedia = compareMaskedPng(browserCanvas, rustCanvas, width, height, mediaMask, false);
  await writeFile(diffPath, PNG.sync.write(diff));
  return {
    browser: { width: browser.width, height: browser.height },
    rust: { width: rust.width, height: rust.height },
    compared: { width, height },
    diffPixels,
    diffRatio: diffPixels / (width * height),
    media,
    nonMedia,
  };
}

function buildRectMask(width, height, rects) {
  const mask = new Uint8Array(width * height);
  for (const rect of rects) {
    const x0 = Math.max(0, rect.x);
    const y0 = Math.max(0, rect.y);
    const x1 = Math.min(width, rect.x + rect.width);
    const y1 = Math.min(height, rect.y + rect.height);
    for (let y = y0; y < y1; y += 1) {
      mask.fill(1, y * width + x0, y * width + x1);
    }
  }
  return mask;
}

function compareMaskedPng(browserCanvas, rustCanvas, width, height, mask, keepMasked) {
  const browserMasked = new PNG({ width, height });
  const rustMasked = new PNG({ width, height });
  let areaPixels = 0;
  for (let pixel = 0; pixel < width * height; pixel += 1) {
    const keep = Boolean(mask[pixel]) === keepMasked;
    const index = pixel * 4;
    if (keep) {
      areaPixels += 1;
      browserCanvas.data.copy(browserMasked.data, index, index, index + 4);
      rustCanvas.data.copy(rustMasked.data, index, index, index + 4);
    } else {
      browserCanvas.data.copy(browserMasked.data, index, index, index + 4);
      browserCanvas.data.copy(rustMasked.data, index, index, index + 4);
    }
  }
  if (areaPixels === 0) {
    return { areaPixels: 0, diffPixels: 0, diffRatio: 0 };
  }
  const scratch = new PNG({ width, height });
  const diffPixels = pixelmatch(
    browserMasked.data,
    rustMasked.data,
    scratch.data,
    width,
    height,
    { threshold: 0.1, includeAA: true },
  );
  return { areaPixels, diffPixels, diffRatio: diffPixels / areaPixels };
}

function padPng(source, width, height) {
  if (source.width === width && source.height === height) {
    return source;
  }
  const target = new PNG({ width, height });
  target.data.fill(255);
  for (let y = 0; y < source.height; y += 1) {
    const sourceStart = y * source.width * 4;
    const targetStart = y * width * 4;
    source.data.copy(target.data, targetStart, sourceStart, sourceStart + source.width * 4);
  }
  return target;
}

function buildBrowserDocument(sourceHtml, baseUrl, width) {
  const head = [
    '<meta charset="utf-8">',
    `<base href="${escapeAttr(baseUrl)}">`,
    '<style id="email-render-defaults">',
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
  const headEnd = lower.indexOf('</head>');
  if (headEnd >= 0) {
    return `${sourceHtml.slice(0, headEnd)}${head}${sourceHtml.slice(headEnd)}`;
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

function printResult(result) {
  const percent = (result.diffRatio * 100).toFixed(2);
  const nonMedia = (result.nonMedia.diffRatio * 100).toFixed(2);
  console.log(
    `${result.name}\tbrowser ${result.browser.width}x${result.browser.height}\trust ${result.rust.width}x${result.rust.height}\tdiff ${percent}%\tnon-media ${nonMedia}%\twarnings ${result.warningCount}`,
  );
}

function checkExpectations(results, expectations) {
  const failures = [];
  for (const result of results) {
    const expected = expectations.templates?.[result.name];
    if (!expected) {
      continue;
    }
    const maxDiffPercent = expected.maxDiffPercent ?? expectations.maxDiffPercent;
    if (Number.isFinite(maxDiffPercent) && result.diffRatio * 100 > maxDiffPercent) {
      failures.push(
        `${result.name}: diff ${formatPercent(result.diffRatio)} exceeded ${maxDiffPercent.toFixed(2)}%`,
      );
    }
    const maxNonMediaDiffPercent =
      expected.maxNonMediaDiffPercent ?? expectations.maxNonMediaDiffPercent;
    if (
      Number.isFinite(maxNonMediaDiffPercent) &&
      result.nonMedia.diffRatio * 100 > maxNonMediaDiffPercent
    ) {
      failures.push(
        `${result.name}: non-media diff ${formatPercent(result.nonMedia.diffRatio)} exceeded ${maxNonMediaDiffPercent.toFixed(2)}%`,
      );
    }
    const maxWarnings = expected.maxWarnings ?? expectations.maxWarnings;
    if (Number.isFinite(maxWarnings) && result.warningCount > maxWarnings) {
      failures.push(
        `${result.name}: warnings ${result.warningCount} exceeded ${maxWarnings}`,
      );
    }
  }
  return failures;
}

function renderMarkdownReport(results, args, expectations) {
  const lines = [
    '# Playwright Comparison Report',
    '',
    `- Width: ${args.width}px`,
    `- Remote image loading in Rust renderer: ${args.allowRemote ? 'enabled' : 'disabled'}`,
    `- Output directory: \`${args.workDir}\``,
    '',
    '| Template | Browser | Rust | Diff | Media Diff | Non-Media Diff | Diff Target | Non-Media Target | Warnings | Files |',
    '|---|---:|---:|---:|---:|---:|---:|---:|---:|---|',
  ];

  for (const result of results) {
    const percent = formatPercent(result.diffRatio);
    const mediaPercent =
      result.media.areaPixels > 0 ? formatPercent(result.media.diffRatio) : '';
    const nonMediaPercent = formatPercent(result.nonMedia.diffRatio);
    const expected = expectations?.templates?.[result.name];
    const target = expected?.targetDiffPercent ?? expectations?.targetDiffPercent;
    const max = expected?.maxDiffPercent ?? expectations?.maxDiffPercent;
    const nonMediaTarget =
      expected?.targetNonMediaDiffPercent ?? expectations?.targetNonMediaDiffPercent;
    const nonMediaMax =
      expected?.maxNonMediaDiffPercent ?? expectations?.maxNonMediaDiffPercent;
    const targetText = Number.isFinite(target)
      ? `${target.toFixed(2)}%`
      : Number.isFinite(max)
        ? `<= ${max.toFixed(2)}%`
        : '';
    const nonMediaTargetText = Number.isFinite(nonMediaTarget)
      ? `${nonMediaTarget.toFixed(2)}%`
      : Number.isFinite(nonMediaMax)
        ? `<= ${nonMediaMax.toFixed(2)}%`
        : '';
    lines.push(
      `| ${result.name} | ${result.browser.width}x${result.browser.height} | ${result.rust.width}x${result.rust.height} | ${percent} | ${mediaPercent} | ${nonMediaPercent} | ${targetText} | ${nonMediaTargetText} | ${result.warningCount} | [side-by-side](${result.sideBySidePng}) [browser](${result.browserPng}) [rust](${result.rustPng}) [diff](${result.diffPng}) [log](${result.log}) |`,
    );
  }
  lines.push('');
  lines.push('Notes: pixel comparison pads the shorter image with white before diffing.');
  return `${lines.join('\n')}\n`;
}

function formatPercent(ratio) {
  return `${(ratio * 100).toFixed(2)}%`;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
