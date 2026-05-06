#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import pixelmatch from 'pixelmatch';
import { chromium } from 'playwright';
import { PNG } from 'pngjs';
import { TEMPLATE_CORPUS, loadTemplateSource } from './templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_WORK_DIR = '/tmp/mail-canvas-playwright-compare';
const DEFAULT_WIDTH = 600;
const DEFAULT_TIMEOUT_MS = 15000;
const FIXTURE_FONT_FILES = [
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Arimo-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Arimo-Bold.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Tinos-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Tinos-Bold.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Bold.ttf'),
];

function parseArgs(argv) {
  const args = {
    width: DEFAULT_WIDTH,
    workDir: DEFAULT_WORK_DIR,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    limit: TEMPLATE_CORPUS.length,
    only: [],
    providers: [],
    categories: [],
    all: false,
    allowRemote: true,
    keep: false,
    expectations: null,
    fixtureFonts: false,
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
      case '--provider':
        args.providers.push(next());
        break;
      case '--category':
        args.categories.push(next());
        break;
      case '--all':
        args.all = true;
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
      case '--fixture-fonts':
        args.fixtureFonts = true;
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
    layout: path.join(args.workDir, 'layout'),
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
  for (const template of templates) {
    const source = await loadTemplateSource(template, args.timeoutMs);
    const htmlPath = path.join(dirs.html, `${template.name}.html`);
    await writeFile(htmlPath, source.html);
    downloaded.push({
      ...template,
      htmlPath,
      sourceUrl: source.url,
      sourceBaseUrl: source.baseUrl,
    });
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
  await writeFile(
    path.join(args.workDir, 'comparison.report.json'),
    `${JSON.stringify(buildComparisonSummary(results, failures, expectations), null, 2)}\n`,
  );
  await writeFile(path.join(args.workDir, 'report.md'), renderMarkdownReport(results, args, expectations));
  if (expectations) {
    console.log(validationSummary(results, failures, expectations));
  }
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
  let pool = TEMPLATE_CORPUS;
  if (args.providers.length > 0) {
    const providers = new Set(args.providers);
    pool = pool.filter((template) => providers.has(template.provider));
  }
  if (args.categories.length > 0) {
    const categories = new Set(args.categories);
    pool = pool.filter((template) => categories.has(template.category));
  }

  const expectedNames = expectations ? Object.keys(expectations.templates ?? {}) : [];
  const wanted =
    args.only.length > 0
      ? args.only
      : args.all
        ? pool
            .filter((template) => template.supportTier === 'modern-supported')
            .slice(0, args.limit)
            .map((template) => template.name)
      : expectedNames.length > 0
        ? expectedNames
        : pool
            .filter((template) => template.supportTier === 'modern-supported')
            .slice(0, args.limit)
            .map((template) => template.name);
  const wantedSet = new Set(wanted);
  const templates = TEMPLATE_CORPUS.filter((template) => wantedSet.has(template.name));
  const found = new Set(templates.map((template) => template.name));
  const missing = wanted.filter((name) => !found.has(name));
  if (missing.length > 0) {
    throw new Error(`unknown template names: ${missing.join(', ')}`);
  }
  return templates;
}

async function ensureRenderer() {
  const renderer = path.join(ROOT_DIR, 'target', 'debug', 'mail-canvas');
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

async function compareTemplate(template, args, dirs, renderer, browser) {
  const browserPath = path.join(dirs.browser, `${template.name}.png`);
  const rustPath = path.join(dirs.rust, `${template.name}.png`);
  const diffPath = path.join(dirs.diff, `${template.name}.png`);
  const sideBySidePath = path.join(dirs.sideBySide, `${template.name}.png`);
  const logPath = path.join(dirs.logs, `${template.name}.log`);
  const diagnosticsPath = path.join(dirs.logs, `${template.name}.diagnostics.json`);
  const layoutPath = path.join(dirs.layout, `${template.name}.layout.json`);
  const preparedPath = path.join(dirs.prepared, `${template.name}.html`);

  const sourceHtml = await readFile(template.htmlPath, 'utf8');
  const baseUrl = template.sourceBaseUrl ?? template.baseUrl ?? new URL('.', template.url).href;
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
    '--warnings-json',
    diagnosticsPath,
    '--layout-json',
    layoutPath,
  ];
  if (args.fixtureFonts) {
    for (const fontPath of FIXTURE_FONT_FILES) {
      renderArgs.push('--font-file', fontPath);
    }
  }
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
  const rustLayout = JSON.parse(await readFile(layoutPath, 'utf8'));
  const rustRects = collectRustRectsFromLayout(rustLayout.tree);

  const comparison = await comparePng(
    browserPath,
    rustPath,
    diffPath,
    browserMetrics.mediaRects,
    browserMetrics.textRects,
    rustRects.mediaRects,
    rustRects.textRects,
  );
  await writeTriptych([browserPath, rustPath, diffPath], sideBySidePath);
  const diagnostics = JSON.parse(await readFile(diagnosticsPath, 'utf8'));
  const warningCount = diagnostics.warnings?.length ?? 0;
  return {
    name: template.name,
    url: template.sourceUrl ?? template.url,
    provider: template.provider,
    category: template.category,
    supportTier: template.supportTier,
    supportReason: template.supportReason,
    corpusStatus: template.status,
    corpusReason: template.reason,
    expectedWarnings: template.expectedWarnings,
    browserPng: browserPath,
    rustPng: rustPath,
    diffPng: diffPath,
    sideBySidePng: sideBySidePath,
    log: logPath,
    diagnosticsJson: diagnosticsPath,
    layoutJson: layoutPath,
    browser: comparison.browser,
    rust: comparison.rust,
    compared: comparison.compared,
    diffPixels: comparison.diffPixels,
    diffRatio: comparison.diffRatio,
    media: comparison.media,
    mediaRects: comparison.mediaRects,
    text: comparison.text,
    nonMedia: comparison.nonMedia,
    nonMediaText: comparison.nonMediaText,
    nonMediaNonText: comparison.nonMediaNonText,
    textCoverage: comparison.textCoverage,
    textRects: comparison.textRects,
    bandDiffs: comparison.bandDiffs,
    firstBadRegion: comparison.firstBadRegion,
    warningCount,
    assetSummary: summarizeAssets(diagnostics.assets ?? []),
  };
}

function summarizeAssets(assets) {
  const summary = { total: assets.length, loaded: 0, blocked: 0, failed: 0 };
  for (const asset of assets) {
    if (asset.status === 'loaded') summary.loaded += 1;
    if (asset.status === 'blocked') summary.blocked += 1;
    if (asset.status === 'failed') summary.failed += 1;
  }
  return summary;
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
      ...(await collectComparisonRects(page, width, height)),
    };
  } finally {
    await page.close();
  }
}

async function collectComparisonRects(page, width, height) {
  return page.evaluate(
    ({ width, height }) => {
      const mediaRects = [];
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
          mediaRects.push({ x: x0, y: y0, width: x1 - x0, height: y1 - y0 });
        }
      }

      const textRects = [];
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      const range = document.createRange();
      while (walker.nextNode()) {
        const node = walker.currentNode;
        if (!node.nodeValue || !node.nodeValue.trim()) {
          continue;
        }
        range.selectNodeContents(node);
        for (const rect of range.getClientRects()) {
          if (rect.width <= 0 || rect.height <= 0) {
            continue;
          }
          const x0 = Math.max(0, Math.floor(rect.left));
          const y0 = Math.max(0, Math.floor(rect.top));
          const x1 = Math.min(width, Math.ceil(rect.right));
          const y1 = Math.min(height, Math.ceil(rect.bottom));
          if (x1 > x0 && y1 > y0) {
            textRects.push({ x: x0, y: y0, width: x1 - x0, height: y1 - y0 });
          }
        }
      }
      range.detach();

      return { mediaRects, textRects };
    },
    { width, height },
  );
}

function collectRustRectsFromLayout(node) {
  const mediaRects = [];
  const textRects = [];
  visit(node);
  return { mediaRects, textRects };

  function visit(current) {
    if (!current || !current.rect) {
      return;
    }
    if (current.tag === '#text' && current.text && current.rect.width > 0 && current.rect.height > 0) {
      textRects.push(rectToMaskRect(current.rect));
    }
    if (current.tag === 'img' || current.style?.background_image) {
      if (current.rect.width > 0 && current.rect.height > 0) {
        mediaRects.push(rectToMaskRect(current.rect));
      }
    }
    for (const child of current.children ?? []) {
      visit(child);
    }
  }
}

function rectToMaskRect(rect) {
  return {
    x: Math.max(0, Math.floor(rect.x)),
    y: Math.max(0, Math.floor(rect.y)),
    width: Math.max(0, Math.ceil(rect.width)),
    height: Math.max(0, Math.ceil(rect.height)),
  };
}

async function comparePng(
  browserPath,
  rustPath,
  diffPath,
  browserMediaRects = [],
  browserTextRects = [],
  rustMediaRects = [],
  rustTextRects = [],
) {
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
  const mediaMask = buildUnionRectMask(width, height, browserMediaRects, rustMediaRects);
  const textMask = buildUnionRectMask(width, height, browserTextRects, rustTextRects);
  const mediaRects = compareRectMasks(width, height, browserMediaRects, rustMediaRects);
  const nonMediaTextMask = combineMasks(width, height, (pixel) => {
    return textMask[pixel] && !mediaMask[pixel];
  });
  const nonMediaNonTextMask = combineMasks(width, height, (pixel) => {
    return !textMask[pixel] && !mediaMask[pixel];
  });
  const media = compareMaskedPng(browserCanvas, rustCanvas, width, height, mediaMask, true);
  const text = compareMaskedPng(browserCanvas, rustCanvas, width, height, textMask, true);
  const textCoverage = computeCoverageMetrics(browserCanvas, rustCanvas, width, height, textMask);
  const nonMedia = compareMaskedPng(browserCanvas, rustCanvas, width, height, mediaMask, false);
  const nonMediaText = compareMaskedPng(
    browserCanvas,
    rustCanvas,
    width,
    height,
    nonMediaTextMask,
    true,
  );
  const nonMediaNonText = compareMaskedPng(
    browserCanvas,
    rustCanvas,
    width,
    height,
    nonMediaNonTextMask,
    true,
  );
  const bandDiffs = computeBandDiffs(browserCanvas, rustCanvas, width, height);
  const firstBadRegion = findFirstBadRegion(bandDiffs);
  const textRectMetrics = compareTextRects(browserTextRects, rustTextRects, height);
  await writeFile(diffPath, PNG.sync.write(diff));
  return {
    browser: { width: browser.width, height: browser.height },
    rust: { width: rust.width, height: rust.height },
    compared: { width, height },
    diffPixels,
    diffRatio: diffPixels / (width * height),
    media,
    mediaRects,
    text,
    textCoverage,
    textRects: textRectMetrics,
    nonMedia,
    nonMediaText,
    nonMediaNonText,
    bandDiffs,
    firstBadRegion,
  };
}

function computeCoverageMetrics(browserCanvas, rustCanvas, width, height, mask) {
  let areaPixels = 0;
  let browserInk = 0;
  let rustInk = 0;
  let alphaDelta = 0;
  for (let pixel = 0; pixel < width * height; pixel += 1) {
    if (!mask[pixel]) {
      continue;
    }
    areaPixels += 1;
    const index = pixel * 4;
    const browserAlpha = 255 - luminance(browserCanvas.data[index], browserCanvas.data[index + 1], browserCanvas.data[index + 2]);
    const rustAlpha = 255 - luminance(rustCanvas.data[index], rustCanvas.data[index + 1], rustCanvas.data[index + 2]);
    browserInk += browserAlpha;
    rustInk += rustAlpha;
    alphaDelta += Math.abs(browserAlpha - rustAlpha);
  }
  const maxInk = Math.max(browserInk, rustInk, 1);
  return {
    areaPixels,
    browserInk,
    rustInk,
    coverageDeltaRatio: Math.abs(browserInk - rustInk) / maxInk,
    alphaDeltaRatio: alphaDelta / (areaPixels * 255 || 1),
  };
}

function luminance(r, g, b) {
  return Math.round(0.299 * r + 0.587 * g + 0.114 * b);
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

function buildUnionRectMask(width, height, leftRects, rightRects) {
  const mask = buildRectMask(width, height, leftRects);
  for (const rect of rightRects) {
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

function combineMasks(width, height, predicate) {
  const mask = new Uint8Array(width * height);
  for (let pixel = 0; pixel < width * height; pixel += 1) {
    if (predicate(pixel)) {
      mask[pixel] = 1;
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

function compareRectMasks(width, height, browserRects, rustRects) {
  const browserMask = buildRectMask(width, height, browserRects);
  const rustMask = buildRectMask(width, height, rustRects);
  let browserArea = 0;
  let rustArea = 0;
  let overlapPixels = 0;
  let deltaPixels = 0;
  for (let pixel = 0; pixel < width * height; pixel += 1) {
    const browserCovered = browserMask[pixel] === 1;
    const rustCovered = rustMask[pixel] === 1;
    if (browserCovered) {
      browserArea += 1;
    }
    if (rustCovered) {
      rustArea += 1;
    }
    if (browserCovered && rustCovered) {
      overlapPixels += 1;
    }
    if (browserCovered !== rustCovered) {
      deltaPixels += 1;
    }
  }
  const unionPixels = browserArea + rustArea - overlapPixels;
  return {
    browserCount: browserRects.length,
    rustCount: rustRects.length,
    browserArea,
    rustArea,
    overlapPixels,
    deltaPixels,
    deltaRatio: deltaPixels / Math.max(1, unionPixels),
    coverageDeltaRatio: Math.abs(browserArea - rustArea) / Math.max(1, browserArea, rustArea),
  };
}

function computeBandDiffs(browserCanvas, rustCanvas, width, height, bandHeight = 100) {
  const bands = [];
  for (let y = 0; y < height; y += bandHeight) {
    const currentHeight = Math.min(bandHeight, height - y);
    const browserBand = new PNG({ width, height: currentHeight });
    const rustBand = new PNG({ width, height: currentHeight });
    for (let row = 0; row < currentHeight; row += 1) {
      const srcStart = ((y + row) * width) * 4;
      const srcEnd = srcStart + width * 4;
      browserCanvas.data.copy(browserBand.data, row * width * 4, srcStart, srcEnd);
      rustCanvas.data.copy(rustBand.data, row * width * 4, srcStart, srcEnd);
    }
    const scratch = new PNG({ width, height: currentHeight });
    const diffPixels = pixelmatch(
      browserBand.data,
      rustBand.data,
      scratch.data,
      width,
      currentHeight,
      { threshold: 0.1, includeAA: true },
    );
    const areaPixels = width * currentHeight;
    bands.push({
      y0: y,
      y1: y + currentHeight,
      diffPixels,
      diffRatio: diffPixels / Math.max(1, areaPixels),
    });
  }
  return bands;
}

function findFirstBadRegion(bands, minRatio = 0.1, minPixels = 500) {
  const first = bands.find((band) => band.diffRatio >= minRatio && band.diffPixels >= minPixels);
  if (first) {
    return first;
  }
  return bands.reduce((best, band) => {
    if (!best || band.diffRatio > best.diffRatio) {
      return band;
    }
    return best;
  }, null);
}

function compareTextRects(browserTextRects, rustTextRects, browserHeight) {
  const browserArea = rectAreaSum(browserTextRects);
  const rustArea = rectAreaSum(rustTextRects);
  const coverageDeltaRatio =
    Math.abs(browserArea - rustArea) / Math.max(1, browserArea, rustArea);
  const paired = Math.min(browserTextRects.length, rustTextRects.length);
  let rectDelta = 0;
  let maxDimension = 0;
  for (let index = 0; index < paired; index += 1) {
    const left = browserTextRects[index];
    const right = rustTextRects[index];
    rectDelta +=
      Math.abs(left.x - right.x) +
      Math.abs(left.y - right.y) +
      Math.abs(left.width - right.width) +
      Math.abs(left.height - right.height);
    maxDimension += left.width + left.height + right.width + right.height;
  }
  const positionDeltaRatio = paired > 0 ? rectDelta / Math.max(1, maxDimension) : 0;
  const countDeltaRatio =
    Math.abs(browserTextRects.length - rustTextRects.length) /
    Math.max(1, browserTextRects.length, rustTextRects.length);
  return {
    browserCount: browserTextRects.length,
    rustCount: rustTextRects.length,
    browserArea,
    rustArea,
    coverageDeltaRatio,
    positionDeltaRatio,
    countDeltaRatio,
    bandAnchors: buildTextBandAnchors(browserTextRects, rustTextRects, browserHeight),
  };
}

function rectAreaSum(rects) {
  return rects.reduce((sum, rect) => sum + rect.width * rect.height, 0);
}

function buildTextBandAnchors(browserTextRects, rustTextRects, height, bandHeight = 100) {
  const bands = [];
  for (let y = 0; y < height; y += bandHeight) {
    const y1 = Math.min(height, y + bandHeight);
    const browserCount = browserTextRects.filter((rect) => overlapsBand(rect, y, y1)).length;
    const rustCount = rustTextRects.filter((rect) => overlapsBand(rect, y, y1)).length;
    bands.push({
      y0: y,
      y1,
      browserCount,
      rustCount,
      countDelta: Math.abs(browserCount - rustCount),
    });
  }
  return bands;
}

function overlapsBand(rect, y0, y1) {
  return rect.y < y1 && rect.y + rect.height > y0;
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
  const media = result.media.areaPixels > 0 ? `${(result.media.diffRatio * 100).toFixed(2)}%` : '';
  const mediaRect = (result.mediaRects.deltaRatio * 100).toFixed(2);
  const text = result.text.areaPixels > 0 ? `${(result.text.diffRatio * 100).toFixed(2)}%` : '';
  const nonMedia = (result.nonMedia.diffRatio * 100).toFixed(2);
  const nonMediaNonText = (result.nonMediaNonText.diffRatio * 100).toFixed(2);
  const textCoverage = (result.textCoverage.coverageDeltaRatio * 100).toFixed(2);
  const textRect = (result.textRects.positionDeltaRatio * 100).toFixed(2);
  const textPixel = (result.textCoverage.alphaDeltaRatio * 100).toFixed(2);
  const heightDelta = result.rust.height - result.browser.height;
  const firstBad =
    result.firstBadRegion ? `${result.firstBadRegion.y0}-${result.firstBadRegion.y1}` : '';
  console.log(
    `${result.name}\tbrowser ${result.browser.width}x${result.browser.height}\trust ${result.rust.width}x${result.rust.height}\theightΔ ${heightDelta}px\tdiff ${percent}%\ttext ${text}\ttext-coverage ${textCoverage}%\ttext-rect ${textRect}%\ttext-pixel ${textPixel}%\tmedia ${media}\tmedia-rect ${mediaRect}%\tnon-media ${nonMedia}%\tnon-media non-text ${nonMediaNonText}%\tfirst-bad ${firstBad}\twarnings ${result.warningCount}`,
  );
}

function checkExpectations(results, expectations) {
  const failures = [];
  const semanticMode = expectations.validationMode === 'semantic';
  for (const result of results) {
    const expected = expectations.templates?.[result.name];
    if (semanticMode) {
      if (semanticResultSkipped(result, expectations)) {
        continue;
      }
      failures.push(...checkSemanticExpectations(result, expectations, expected ?? {}));
      continue;
    }
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
    const maxNonMediaNonTextDiffPercent =
      expected.maxNonMediaNonTextDiffPercent ?? expectations.maxNonMediaNonTextDiffPercent;
    if (
      Number.isFinite(maxNonMediaNonTextDiffPercent) &&
      result.nonMediaNonText.diffRatio * 100 > maxNonMediaNonTextDiffPercent
    ) {
      failures.push(
        `${result.name}: non-media non-text diff ${formatPercent(result.nonMediaNonText.diffRatio)} exceeded ${maxNonMediaNonTextDiffPercent.toFixed(2)}%`,
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

function validationSummary(results, failures, expectations) {
  const failedNames = new Set();
  for (const failure of failures) {
    const separator = failure.indexOf(':');
    if (separator > 0) {
      failedNames.add(failure.slice(0, separator));
    }
  }
  const skippedNames =
    expectations.validationMode === 'semantic'
      ? new Set(
          results
            .filter((result) => semanticResultSkipped(result, expectations))
            .map((result) => result.name),
        )
      : new Set();
  const checked = results.length - skippedNames.size;
  const passed = checked - failedNames.size;
  const mode =
    expectations.validationMode === 'semantic' ? 'semantic validation' : 'pixel validation';
  const skipped =
    skippedNames.size > 0 ? ` (${skippedNames.size} skipped due corpus scope/issues)` : '';
  return `${mode}: ${passed}/${checked} templates passed${skipped}`;
}

function semanticResultSkipped(result, expectations) {
  if (Object.prototype.hasOwnProperty.call(expectations.templates ?? {}, result.name)) {
    return false;
  }
  if (result.supportTier !== 'modern-supported') {
    return true;
  }
  if (expectations.skipKnownWarningTemplates && result.corpusStatus === 'known-warning') {
    return true;
  }
  if (result.warningCount === 0) {
    return false;
  }
  return Boolean(expectations.skipTemplatesWithWarnings);
}

function checkSemanticExpectations(result, expectations, expected) {
  const failures = [];
  const maxWarnings = expected.maxWarnings ?? expectations.maxWarnings;
  if (Number.isFinite(maxWarnings) && result.warningCount > maxWarnings) {
    failures.push(`${result.name}: warnings ${result.warningCount} exceeded ${maxWarnings}`);
  }

  const maxWidthDeltaPx = expected.maxWidthDeltaPx ?? expectations.maxWidthDeltaPx;
  const widthDeltaPx = Math.abs(result.rust.width - result.browser.width);
  if (Number.isFinite(maxWidthDeltaPx) && widthDeltaPx > maxWidthDeltaPx) {
    failures.push(`${result.name}: width delta ${widthDeltaPx}px exceeded ${maxWidthDeltaPx}px`);
  }

  const maxHeightDeltaPx = expected.maxHeightDeltaPx ?? expectations.maxHeightDeltaPx;
  const maxHeightDeltaPercent =
    expected.maxHeightDeltaPercent ?? expectations.maxHeightDeltaPercent;
  const heightDeltaPx = Math.abs(result.rust.height - result.browser.height);
  const heightDeltaPercent = (heightDeltaPx / Math.max(1, result.browser.height)) * 100;
  if (
    semanticDeltaExceeded(
      heightDeltaPx,
      heightDeltaPercent,
      maxHeightDeltaPx,
      maxHeightDeltaPercent,
    )
  ) {
    failures.push(
      `${result.name}: height delta ${heightDeltaPx}px (${heightDeltaPercent.toFixed(2)}%) exceeded ${formatSemanticDeltaLimit(maxHeightDeltaPx, maxHeightDeltaPercent)}`,
    );
  }

  const maxMediaDiffPercent = expected.maxMediaDiffPercent ?? expectations.maxMediaDiffPercent;
  const maxMediaDiffPixels = expected.maxMediaDiffPixels ?? expectations.maxMediaDiffPixels;
  if (
    result.media.areaPixels > 0 &&
    semanticMetricExceeded(
      result.media.diffPixels,
      result.media.diffRatio * 100,
      maxMediaDiffPixels,
      maxMediaDiffPercent,
    )
  ) {
    failures.push(
      `${result.name}: media diff ${formatSemanticMetric(result.media.diffPixels, result.media.diffRatio * 100)} exceeded ${formatSemanticMetricLimit(maxMediaDiffPixels, maxMediaDiffPercent)}`,
    );
  }

  const maxTextDiffPercent = expected.maxTextDiffPercent ?? expectations.maxTextDiffPercent;
  if (
    result.text.areaPixels > 0 &&
    Number.isFinite(maxTextDiffPercent) &&
    result.text.diffRatio * 100 > maxTextDiffPercent
  ) {
    failures.push(
      `${result.name}: text diff ${formatPercent(result.text.diffRatio)} exceeded ${maxTextDiffPercent.toFixed(2)}%`,
    );
  }

  const maxNonMediaNonTextDiffPercent =
    expected.maxNonMediaNonTextDiffPercent ?? expectations.maxNonMediaNonTextDiffPercent;
  if (
    Number.isFinite(maxNonMediaNonTextDiffPercent) &&
    result.nonMediaNonText.diffRatio * 100 > maxNonMediaNonTextDiffPercent
  ) {
    failures.push(
      `${result.name}: non-media non-text diff ${formatPercent(result.nonMediaNonText.diffRatio)} exceeded ${maxNonMediaNonTextDiffPercent.toFixed(2)}%`,
    );
  }

  const maxTotalDiffPercent = expected.maxTotalDiffPercent ?? expectations.maxTotalDiffPercent;
  if (Number.isFinite(maxTotalDiffPercent) && result.diffRatio * 100 > maxTotalDiffPercent) {
    failures.push(
      `${result.name}: total diff ${formatPercent(result.diffRatio)} exceeded ${maxTotalDiffPercent.toFixed(2)}%`,
    );
  }

  return failures;
}

function semanticDeltaExceeded(deltaPx, deltaPercent, maxPx, maxPercent) {
  const hasPx = Number.isFinite(maxPx);
  const hasPercent = Number.isFinite(maxPercent);
  if (hasPx && hasPercent) {
    return deltaPx > maxPx && deltaPercent > maxPercent;
  }
  if (hasPx) {
    return deltaPx > maxPx;
  }
  if (hasPercent) {
    return deltaPercent > maxPercent;
  }
  return false;
}

function semanticMetricExceeded(diffPixels, diffPercent, maxPixels, maxPercent) {
  const hasPixels = Number.isFinite(maxPixels);
  const hasPercent = Number.isFinite(maxPercent);
  if (hasPixels && hasPercent) {
    return diffPixels > maxPixels && diffPercent > maxPercent;
  }
  if (hasPixels) {
    return diffPixels > maxPixels;
  }
  if (hasPercent) {
    return diffPercent > maxPercent;
  }
  return false;
}

function formatSemanticDeltaLimit(maxPx, maxPercent) {
  const limits = [];
  if (Number.isFinite(maxPx)) {
    limits.push(`${maxPx}px`);
  }
  if (Number.isFinite(maxPercent)) {
    limits.push(`${maxPercent.toFixed(2)}%`);
  }
  return limits.length > 0 ? limits.join(' or ') : 'no limit';
}

function formatSemanticMetric(diffPixels, diffPercent) {
  return `${diffPixels}px / ${diffPercent.toFixed(2)}%`;
}

function formatSemanticMetricLimit(maxPixels, maxPercent) {
  const limits = [];
  if (Number.isFinite(maxPixels)) {
    limits.push(`${maxPixels}px`);
  }
  if (Number.isFinite(maxPercent)) {
    limits.push(`${maxPercent.toFixed(2)}%`);
  }
  return limits.length > 0 ? limits.join(' or ') : 'no limit';
}

function renderMarkdownReport(results, args, expectations) {
  if (expectations?.validationMode === 'semantic') {
    return renderSemanticMarkdownReport(results, args, expectations);
  }

  const lines = [
    '# Playwright Comparison Report',
    '',
    `- Width: ${args.width}px`,
    `- Remote image loading in Rust renderer: ${args.allowRemote ? 'enabled' : 'disabled'}`,
    `- Output directory: \`${args.workDir}\``,
    '',
    '| Template | Browser | Rust | Diff | Media Diff | Media Rect Δ | Text Diff | Text Coverage Δ | Text Rect Δ | Text Pixel Δ | First Bad Region | Non-Media Diff | Non-Media Non-Text Diff | Diff Target | Non-Media Target | Non-Media Non-Text Target | Warnings | Assets | Files |',
    '|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---|---|',
  ];

  for (const result of results) {
    const percent = formatPercent(result.diffRatio);
    const mediaPercent =
      result.media.areaPixels > 0 ? formatPercent(result.media.diffRatio) : '';
    const mediaRectPercent = formatPercent(result.mediaRects.deltaRatio);
    const textPercent = result.text.areaPixels > 0 ? formatPercent(result.text.diffRatio) : '';
    const textCoveragePercent =
      result.textCoverage.areaPixels > 0 ? formatPercent(result.textCoverage.coverageDeltaRatio) : '';
    const textRectPercent = formatPercent(result.textRects.positionDeltaRatio);
    const textPixelPercent =
      result.textCoverage.areaPixels > 0 ? formatPercent(result.textCoverage.alphaDeltaRatio) : '';
    const nonMediaPercent = formatPercent(result.nonMedia.diffRatio);
    const nonMediaNonTextPercent = formatPercent(result.nonMediaNonText.diffRatio);
    const firstBadRegion = result.firstBadRegion
      ? `${result.firstBadRegion.y0}-${result.firstBadRegion.y1}`
      : '';
    const expected = expectations?.templates?.[result.name];
    const target = expected?.targetDiffPercent ?? expectations?.targetDiffPercent;
    const max = expected?.maxDiffPercent ?? expectations?.maxDiffPercent;
    const nonMediaTarget =
      expected?.targetNonMediaDiffPercent ?? expectations?.targetNonMediaDiffPercent;
    const nonMediaMax =
      expected?.maxNonMediaDiffPercent ?? expectations?.maxNonMediaDiffPercent;
    const nonMediaNonTextTarget =
      expected?.targetNonMediaNonTextDiffPercent ??
      expectations?.targetNonMediaNonTextDiffPercent;
    const nonMediaNonTextMax =
      expected?.maxNonMediaNonTextDiffPercent ??
      expectations?.maxNonMediaNonTextDiffPercent;
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
    const nonMediaNonTextTargetText = Number.isFinite(nonMediaNonTextTarget)
      ? `${nonMediaNonTextTarget.toFixed(2)}%`
      : Number.isFinite(nonMediaNonTextMax)
        ? `<= ${nonMediaNonTextMax.toFixed(2)}%`
        : '';
    const assets = `${result.assetSummary.loaded}/${result.assetSummary.blocked}/${result.assetSummary.failed}`;
    lines.push(
      `| ${result.name} | ${result.browser.width}x${result.browser.height} | ${result.rust.width}x${result.rust.height} | ${percent} | ${mediaPercent} | ${mediaRectPercent} | ${textPercent} | ${textCoveragePercent} | ${textRectPercent} | ${textPixelPercent} | ${firstBadRegion} | ${nonMediaPercent} | ${nonMediaNonTextPercent} | ${targetText} | ${nonMediaTargetText} | ${nonMediaNonTextTargetText} | ${result.warningCount} | ${assets} | [side-by-side](${result.sideBySidePng}) [browser](${result.browserPng}) [rust](${result.rustPng}) [diff](${result.diffPng}) [log](${result.log}) [diagnostics](${result.diagnosticsJson}) [layout](${result.layoutJson}) |`,
    );
  }
  lines.push('');
  lines.push('Notes: pixel comparison pads the shorter image with white before diffing.');
  return `${lines.join('\n')}\n`;
}

function renderSemanticMarkdownReport(results, args, expectations) {
  const lines = [
    '# Playwright Semantic Visual Validation Report',
    '',
    `- Width: ${args.width}px`,
    `- Remote image loading in Rust renderer: ${args.allowRemote ? 'enabled' : 'disabled'}`,
    `- Output directory: \`${args.workDir}\``,
    '- Total pixel diff is reported as an observation only unless `maxTotalDiffPercent` is set.',
    '',
    '| Template | Provider | Support | Status | Browser | Rust | Height Delta | Total Diff | Text Diff | Text Coverage Δ | Text Rect Δ | Text Pixel Δ | First Bad Region | Media Diff | Media Rect Δ | Non-Media Non-Text Diff | Semantic Limits | Warnings | Assets | Files |',
    '|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---|---:|---|---|',
  ];

  for (const result of results) {
    const expected = expectations?.templates?.[result.name] ?? {};
    const status = semanticResultSkipped(result, expectations)
      ? `skipped: ${result.supportTier !== 'modern-supported' ? result.supportReason : result.corpusReason || 'known corpus warning'}`
      : semanticResultStatus(result, expectations, expected);
    const heightDeltaPx = Math.abs(result.rust.height - result.browser.height);
    const heightDeltaPercent = heightDeltaPx / Math.max(1, result.browser.height);
    const mediaPercent =
      result.media.areaPixels > 0 ? formatPercent(result.media.diffRatio) : '';
    const mediaRectPercent = formatPercent(result.mediaRects.deltaRatio);
    const textPercent = result.text.areaPixels > 0 ? formatPercent(result.text.diffRatio) : '';
    const textCoveragePercent =
      result.textCoverage.areaPixels > 0 ? formatPercent(result.textCoverage.coverageDeltaRatio) : '';
    const textRectPercent = formatPercent(result.textRects.positionDeltaRatio);
    const textPixelPercent =
      result.textCoverage.areaPixels > 0 ? formatPercent(result.textCoverage.alphaDeltaRatio) : '';
    const firstBadRegion = result.firstBadRegion
      ? `${result.firstBadRegion.y0}-${result.firstBadRegion.y1}`
      : '';
    const limits = semanticLimitSummary(expectations, expected);
    const assets = `${result.assetSummary.loaded}/${result.assetSummary.blocked}/${result.assetSummary.failed}`;
    lines.push(
      `| ${result.name} | ${result.provider} | ${result.supportTier} | ${status} | ${result.browser.width}x${result.browser.height} | ${result.rust.width}x${result.rust.height} | ${heightDeltaPx}px (${formatPercent(heightDeltaPercent)}) | ${formatPercent(result.diffRatio)} | ${textPercent} | ${textCoveragePercent} | ${textRectPercent} | ${textPixelPercent} | ${firstBadRegion} | ${mediaPercent} | ${mediaRectPercent} | ${formatPercent(result.nonMediaNonText.diffRatio)} | ${limits} | ${result.warningCount} | ${assets} | [side-by-side](${result.sideBySidePng}) [browser](${result.browserPng}) [rust](${result.rustPng}) [diff](${result.diffPng}) [log](${result.log}) [diagnostics](${result.diagnosticsJson}) [layout](${result.layoutJson}) |`,
    );
  }
  lines.push('');
  lines.push(
    'Notes: text and media diffs are tolerant coarse checks. They are intended to catch missing content or major placement failures, not exact font rasterization. By default the semantic corpus only treats `modern-supported` templates as in scope. Legacy hacks and malformed HTML are skipped unless explicitly listed.',
  );
  return `${lines.join('\n')}\n`;
}

function semanticResultStatus(result, expectations, expected) {
  const failures = checkSemanticExpectations(result, expectations, expected);
  return failures.length > 0 ? 'failed' : 'passed';
}

function semanticLimitSummary(expectations, expected) {
  const limits = [];
  const maxWidthDeltaPx = expected.maxWidthDeltaPx ?? expectations.maxWidthDeltaPx;
  if (Number.isFinite(maxWidthDeltaPx)) {
    limits.push(`width <= ${maxWidthDeltaPx}px`);
  }
  const maxHeightDeltaPx = expected.maxHeightDeltaPx ?? expectations.maxHeightDeltaPx;
  const maxHeightDeltaPercent =
    expected.maxHeightDeltaPercent ?? expectations.maxHeightDeltaPercent;
  if (Number.isFinite(maxHeightDeltaPx) || Number.isFinite(maxHeightDeltaPercent)) {
    limits.push(`height <= ${formatSemanticDeltaLimit(maxHeightDeltaPx, maxHeightDeltaPercent)}`);
  }
  const maxTextDiffPercent = expected.maxTextDiffPercent ?? expectations.maxTextDiffPercent;
  if (Number.isFinite(maxTextDiffPercent)) {
    limits.push(`text <= ${maxTextDiffPercent.toFixed(2)}%`);
  }
  const maxMediaDiffPercent = expected.maxMediaDiffPercent ?? expectations.maxMediaDiffPercent;
  const maxMediaDiffPixels = expected.maxMediaDiffPixels ?? expectations.maxMediaDiffPixels;
  if (Number.isFinite(maxMediaDiffPixels) || Number.isFinite(maxMediaDiffPercent)) {
    limits.push(`media <= ${formatSemanticMetricLimit(maxMediaDiffPixels, maxMediaDiffPercent)}`);
  }
  const maxNonMediaNonTextDiffPercent =
    expected.maxNonMediaNonTextDiffPercent ?? expectations.maxNonMediaNonTextDiffPercent;
  if (Number.isFinite(maxNonMediaNonTextDiffPercent)) {
    limits.push(`box <= ${maxNonMediaNonTextDiffPercent.toFixed(2)}%`);
  }
  return limits.join('<br>');
}

function formatPercent(ratio) {
  return `${(ratio * 100).toFixed(2)}%`;
}

function buildComparisonSummary(results, failures, expectations) {
  const ranked = [...results]
    .sort((left, right) => right.diffRatio - left.diffRatio)
    .slice(0, 10)
    .map((result) => ({
      name: result.name,
      provider: result.provider,
      supportTier: result.supportTier,
      diffPercent: Number((result.diffRatio * 100).toFixed(2)),
      textPercent: Number((result.text.diffRatio * 100).toFixed(2)),
      textCoverageDeltaPercent: Number((result.textCoverage.coverageDeltaRatio * 100).toFixed(2)),
      textRectDeltaPercent: Number((result.textRects.positionDeltaRatio * 100).toFixed(2)),
      textPixelDeltaPercent: Number((result.textCoverage.alphaDeltaRatio * 100).toFixed(2)),
      mediaPercent: Number((result.media.diffRatio * 100).toFixed(2)),
      mediaRectDeltaPercent: Number((result.mediaRects.deltaRatio * 100).toFixed(2)),
      firstBadRegion: result.firstBadRegion,
      warnings: result.warningCount,
    }));
  return {
    generatedAt: new Date().toISOString(),
    resultCount: results.length,
    failureCount: failures.length,
    failedTemplates: [...new Set(failures.map((failure) => failure.split(':', 1)[0]))],
    validationMode: expectations?.validationMode ?? 'none',
    worstTemplates: ranked,
  };
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
