#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
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
const FIXTURE_FONT_MANIFEST = path.join(ROOT_DIR, 'fixtures', 'fonts', 'fixture-fonts.json');
const FIXTURE_FONT_FILES = [
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Arimo-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Arimo-Bold.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Tinos-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'Tinos-Bold.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Bold.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSansMath-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoColorEmoji.ttf'),
];

function parseArgs(argv) {
  const args = {
    width: DEFAULT_WIDTH,
    workDir: DEFAULT_WORK_DIR,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    limit: TEMPLATE_CORPUS.length,
    only: [],
    htmls: [],
    providers: [],
    categories: [],
    corpusGroups: [],
    all: false,
    allowRemote: true,
    keep: false,
    expectations: null,
    fixtureFonts: false,
    browserCacheDir: null,
    continueOnError: false,
    maxImageBytes: null,
    maxDecodedPixels: null,
    maxTotalResourceBytes: null,
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
      case '--html':
        args.htmls.push(path.resolve(next()));
        break;
      case '--name':
        if (args.htmls.length === 0) {
          throw new Error('--name must follow --html');
        }
        args.htmls[args.htmls.length - 1] = {
          path: args.htmls[args.htmls.length - 1],
          name: next(),
        };
        break;
      case '--provider':
        args.providers.push(next());
        break;
      case '--category':
        args.categories.push(next());
        break;
      case '--corpus-group':
        args.corpusGroups.push(next());
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
      case '--browser-cache-dir':
        args.browserCacheDir = path.resolve(next());
        break;
      case '--continue-on-error':
        args.continueOnError = true;
        break;
      case '--max-image-bytes':
        args.maxImageBytes = Number.parseInt(next(), 10);
        break;
      case '--max-decoded-pixels':
        args.maxDecodedPixels = Number.parseInt(next(), 10);
        break;
      case '--max-total-resource-bytes':
        args.maxTotalResourceBytes = Number.parseInt(next(), 10);
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
  for (const [name, value] of [
    ['--max-image-bytes', args.maxImageBytes],
    ['--max-decoded-pixels', args.maxDecodedPixels],
    ['--max-total-resource-bytes', args.maxTotalResourceBytes],
  ]) {
    if (value !== null && (!Number.isFinite(value) || value <= 0)) {
      throw new Error(`${name} must be a positive integer`);
    }
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.fixtureFonts) {
    args.fixtureFontFaces = await loadFixtureFontFaces();
  }
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
    throw new Error(
      args.htmls.length > 0
        ? 'no local --html templates were provided'
        : `no templates matched --only: ${args.only.join(', ')}`,
    );
  }
  const downloaded = [];
  for (const template of templates) {
    const source = template.localHtmlPath
      ? await loadLocalHtmlTemplate(template)
      : await loadTemplateSource(template, args.timeoutMs);
    const htmlPath = path.join(dirs.html, `${template.name}.html`);
    await writeFile(htmlPath, source.html);
    downloaded.push({
      ...template,
      htmlPath,
      sourceUrl: source.url,
      sourceBaseUrl: source.baseUrl,
      corpusIssues: await detectCorpusIssues(source.html, source.htmlPath),
    });
  }

  const browser = await chromium.launch({ headless: true });
  const results = [];
  try {
    for (const template of downloaded) {
      let result;
      try {
        result = await compareTemplate(template, args, dirs, renderer, browser);
      } catch (error) {
        if (!args.continueOnError) {
          throw error;
        }
        result = await failedComparisonResult(template, error);
      }
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
  if (args.htmls.length > 0) {
    return args.htmls.map((entry) => {
      const htmlPath = typeof entry === 'string' ? entry : entry.path;
      const name =
        typeof entry === 'string'
          ? path.basename(entry, path.extname(entry)).replaceAll(/[^a-z0-9_-]+/gi, '-')
          : entry.name;
      return {
        name,
        url: pathToFileURL(htmlPath).href,
        localHtmlPath: htmlPath,
        provider: 'local',
        corpusGroup: 'local',
        category: 'manual',
        supportTier: 'modern-supported',
        supportReason: '',
        status: 'active',
        expectedWarnings: 0,
        reason: '',
      };
    });
  }

  let pool = TEMPLATE_CORPUS;
  if (args.providers.length > 0) {
    const providers = new Set(args.providers);
    pool = pool.filter((template) => providers.has(template.provider));
  }
  if (args.categories.length > 0) {
    const categories = new Set(args.categories);
    pool = pool.filter((template) => categories.has(template.category));
  }
  if (args.corpusGroups.length > 0) {
    const corpusGroups = new Set(args.corpusGroups);
    pool = pool.filter((template) => corpusGroups.has(template.corpusGroup));
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

async function loadLocalHtmlTemplate(template) {
  const htmlPath = path.resolve(template.localHtmlPath);
  return {
    template,
    html: await readFile(htmlPath, 'utf8'),
    htmlPath,
    url: pathToFileURL(htmlPath).href,
    baseUrl: pathToFileURL(`${path.dirname(htmlPath)}${path.sep}`).href,
  };
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
  const fixtureFontFaces = args.fixtureFontFaces
    ? fixtureFontFacesForHtml(args.fixtureFontFaces, sourceHtml)
    : null;
  await writeFile(
    preparedPath,
    buildBrowserDocument(sourceHtml, baseUrl, args.width, fixtureFontFaces),
  );

  const browserMetrics = await browserScreenshotWithCache(
    browser,
    preparedPath,
    browserPath,
    args.width,
    args.timeoutMs,
    args.browserCacheDir,
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
  if (fixtureFontFaces) {
    for (const fontPath of fixtureFontFilesForFaces(fixtureFontFaces)) {
      renderArgs.push('--font-file', fontPath);
    }
  }
  if (args.allowRemote) {
    renderArgs.push('--allow-remote');
    renderArgs.push('--allow-http');
  }
  if (args.maxImageBytes !== null) {
    renderArgs.push('--max-image-bytes', String(args.maxImageBytes));
  }
  if (args.maxDecodedPixels !== null) {
    renderArgs.push('--max-decoded-pixels', String(args.maxDecodedPixels));
  }
  if (args.maxTotalResourceBytes !== null) {
    renderArgs.push('--max-total-resource-bytes', String(args.maxTotalResourceBytes));
  }
  const render = spawnSync(renderer, renderArgs, {
    cwd: ROOT_DIR,
    encoding: 'utf8',
    stdio: 'pipe',
  });
  await writeFile(logPath, `${render.stdout}${render.stderr}`);
  if (render.status !== 0) {
    return failedComparisonResult(template, new Error(rendererFailureMessage(render, logPath)), {
      browserPng: browserPath,
      log: logPath,
    });
  }
  const rustLayout = JSON.parse(await readFile(layoutPath, 'utf8'));
  const rustRects = collectRustRectsFromLayout(rustLayout);

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
    corpusGroup: template.corpusGroup,
    category: template.category,
    supportTier: template.supportTier,
    supportReason: template.supportReason,
    corpusStatus: template.status,
    corpusReason: template.reason,
    corpusIssues: template.corpusIssues,
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

async function failedComparisonResult(template, error, files = {}) {
  const message = error?.message ?? String(error);
  const browser = files.browserPng ? await pngSize(files.browserPng) : null;
  return {
    name: template.name,
    url: template.sourceUrl ?? template.url,
    provider: template.provider,
    corpusGroup: template.corpusGroup,
    category: template.category,
    supportTier: template.supportTier,
    supportReason: template.supportReason,
    corpusStatus: template.status,
    corpusReason: template.reason,
    corpusIssues: template.corpusIssues,
    expectedWarnings: template.expectedWarnings,
    status: 'render-failed',
    error: message,
    browserPng: files.browserPng ?? null,
    rustPng: files.rustPng ?? null,
    diffPng: files.diffPng ?? null,
    sideBySidePng: files.sideBySidePng ?? null,
    log: files.log ?? null,
    diagnosticsJson: await existingPath(files.diagnosticsJson),
    layoutJson: await existingPath(files.layoutJson),
    browser: browser ?? { width: 0, height: 0 },
    rust: { width: 0, height: 0 },
    compared: { width: 0, height: 0 },
    diffPixels: 0,
    diffRatio: 1,
    media: emptyMetric(),
    mediaRects: { deltaRatio: 0, browserAreaPixels: 0, rustAreaPixels: 0, overlapPixels: 0 },
    text: emptyMetric(),
    nonMedia: emptyMetric(),
    nonMediaText: emptyMetric(),
    nonMediaNonText: emptyMetric(),
    textCoverage: {
      areaPixels: 0,
      browserInk: 0,
      rustInk: 0,
      coverageDeltaRatio: 0,
      alphaDeltaRatio: 0,
    },
    textRects: { positionDeltaRatio: 0, browserRects: 0, rustRects: 0, matchedRects: 0 },
    bandDiffs: [],
    firstBadRegion: null,
    warningCount: 0,
    assetSummary: { total: 0, loaded: 0, blocked: 0, failed: 0 },
  };
}

async function pngSize(filePath) {
  try {
    const png = PNG.sync.read(await readFile(filePath));
    return { width: png.width, height: png.height };
  } catch {
    return null;
  }
}

async function existingPath(filePath) {
  if (!filePath) {
    return null;
  }
  try {
    await stat(filePath);
    return filePath;
  } catch {
    return null;
  }
}

function emptyMetric() {
  return { areaPixels: 0, diffPixels: 0, diffRatio: 0 };
}

function summarizeAssets(assets) {
  const summary = {
    total: assets.length,
    loaded: 0,
    blocked: 0,
    failed: 0,
    failedByKind: {},
  };
  for (const asset of assets) {
    if (asset.status === 'loaded') summary.loaded += 1;
    if (asset.status === 'blocked') summary.blocked += 1;
    if (asset.status === 'failed') {
      summary.failed += 1;
      const kind = asset.kind ?? 'unknown';
      summary.failedByKind[kind] = (summary.failedByKind[kind] ?? 0) + 1;
    }
  }
  return summary;
}

function rendererFailureMessage(render, logPath) {
  const lines = `${render.stdout ?? ''}\n${render.stderr ?? ''}`
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line && line !== 'Caused by:');
  const summary = lines.slice(0, 4).join(' / ');
  return summary ? `${summary}; see ${logPath}` : `renderer failed; see ${logPath}`;
}

async function detectCorpusIssues(html, sourcePath) {
  const issues = [];
  const invalidStyleUrlQuotes = html.match(/style\s*=\s*"[^"]*url\("/gi)?.length ?? 0;
  if (invalidStyleUrlQuotes > 0) {
    issues.push({
      code: 'invalid-style-url-quotes',
      count: invalidStyleUrlQuotes,
    });
  }

  if (sourcePath) {
    const emptyLinkedStylesheets = await emptyStylesheetLinks(html, sourcePath);
    if (emptyLinkedStylesheets.length > 0) {
      issues.push({
        code: 'empty-linked-css',
        count: emptyLinkedStylesheets.length,
        files: emptyLinkedStylesheets,
      });
    }
  }

  return issues;
}

async function emptyStylesheetLinks(html, htmlPath) {
  const links = [];
  const linkPattern = /<link\b[^>]*\bhref\s*=\s*["']([^"']+\.css(?:[?#][^"']*)?)["'][^>]*>/gi;
  for (const match of html.matchAll(linkPattern)) {
    if (!/\brel\s*=\s*["']?stylesheet\b/i.test(match[0])) {
      continue;
    }
    const href = match[1];
    if (/^[a-z][a-z0-9+.-]*:/i.test(href)) {
      continue;
    }
    const pathname = href.split(/[?#]/, 1)[0];
    const cssPath = path.resolve(path.dirname(htmlPath), pathname);
    try {
      const cssStat = await stat(cssPath);
      if (cssStat.size === 0) {
        links.push(path.relative(ROOT_DIR, cssPath));
      }
    } catch {
      links.push(path.relative(ROOT_DIR, cssPath));
    }
  }
  return links;
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
    await waitForStableBrowserAssets(page, timeoutMs);
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
    await waitForStableBrowserAssets(page, timeoutMs);
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

async function browserScreenshotWithCache(browser, htmlPath, outPath, width, timeoutMs, cacheDir) {
  if (!cacheDir) {
    return browserScreenshot(browser, htmlPath, outPath, width, timeoutMs);
  }

  const html = await readFile(htmlPath, 'utf8');
  const cacheKey = createHash('sha256')
    .update('mail-canvas-browser-v1\0')
    .update(String(width))
    .update('\0')
    .update(html)
    .digest('hex');
  const pngCachePath = path.join(cacheDir, `${cacheKey}.png`);
  const metricsCachePath = path.join(cacheDir, `${cacheKey}.json`);

  try {
    const metrics = JSON.parse(await readFile(metricsCachePath, 'utf8'));
    await copyFile(pngCachePath, outPath);
    return metrics;
  } catch {
    // Cache misses are expected for new or changed templates.
  }

  await mkdir(cacheDir, { recursive: true });
  const metrics = await browserScreenshot(browser, htmlPath, outPath, width, timeoutMs);
  await copyFile(outPath, pngCachePath);
  await writeFile(metricsCachePath, `${JSON.stringify(metrics, null, 2)}\n`);
  return metrics;
}

async function waitForStableBrowserAssets(page, timeoutMs) {
  const waitMs = Math.min(Math.max(timeoutMs, 1000), 5000);
  await page
    .evaluate(async (timeout) => {
      const wait = (promise) =>
        Promise.race([
          promise.catch(() => undefined),
          new Promise((resolve) => {
            setTimeout(resolve, timeout);
          }),
        ]);

      if (document.fonts?.ready) {
        await wait(document.fonts.ready);
      }

      const pendingImages = [...document.images].filter((image) => !image.complete);
      if (pendingImages.length > 0) {
        await wait(
          Promise.allSettled(
            pendingImages.map(
              (image) =>
                image.decode?.() ??
                new Promise((resolve) => {
                  image.addEventListener('load', resolve, { once: true });
                  image.addEventListener('error', resolve, { once: true });
                }),
            ),
          ),
        );
      }
    }, waitMs)
    .catch(() => undefined);
}

async function collectComparisonRects(page, width, height) {
  return page.evaluate(
    ({ width, height }) => {
      const mediaRects = [];
      for (const element of document.body.querySelectorAll('*')) {
        const style = window.getComputedStyle(element);
        const tag = element.tagName.toLowerCase();
        const hasMedia = tag === 'img' || hasRenderableBackgroundImage(style.backgroundImage);
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

      function hasRenderableBackgroundImage(value) {
        if (!value || value === 'none') {
          return false;
        }
        return !/url\((['"]?)\1\)/.test(value.trim());
      }
    },
    { width, height },
  );
}

function collectRustRectsFromLayout(layout) {
  const mediaRects = [];
  const textRects = (layout.text_rects ?? []).map((current) => rectToMaskRect(current.rect));
  visit(layout.tree);
  return { mediaRects, textRects };

  function visit(current) {
    if (!current || !current.rect) {
      return;
    }
    if (
      textRects.length === 0 &&
      current.tag === '#text' &&
      current.text &&
      current.rect.width > 0 &&
      current.rect.height > 0
    ) {
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

function buildBrowserDocument(sourceHtml, baseUrl, width, fixtureFontFaces = null) {
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
    fixtureFontFaces ? fixtureFontCss(fixtureFontFaces) : '',
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

async function loadFixtureFontFaces() {
  try {
    const manifest = JSON.parse(await readFile(FIXTURE_FONT_MANIFEST, 'utf8'));
    if (Array.isArray(manifest.fonts) && manifest.fonts.length > 0) {
      return manifest.fonts;
    }
  } catch {
    // Older checkouts can still use the built-in fixture list below.
  }
  return fallbackFixtureFontFaces();
}

function fallbackFixtureFontFaces() {
  const sansAliases = [
    'Arial',
    'Arial Nova',
    'Avenir',
    'Avenir Next',
    'Avenir Next LT Pro',
    'Corbel',
    'Helvetica',
    'Helvetica Neue',
    'Lucida Grande',
    'Lucida Sans',
    'Lucida Sans Unicode',
    'Nimbus Sans',
    'Segoe UI',
    'Tahoma',
    'Trebuchet MS',
    'Verdana',
  ];
  const serifAliases = ['Cambria', 'Georgia', 'Palatino', 'Palatino Linotype', 'Times', 'Times New Roman'];
  return [
    { family: 'Arimo', weight: 400, style: 'normal', path: 'Arimo-Regular.ttf', aliases: sansAliases },
    { family: 'Arimo', weight: 700, style: 'normal', path: 'Arimo-Bold.ttf', aliases: sansAliases },
    { family: 'Tinos', weight: 400, style: 'normal', path: 'Tinos-Regular.ttf', aliases: serifAliases },
    { family: 'Tinos', weight: 700, style: 'normal', path: 'Tinos-Bold.ttf', aliases: serifAliases },
    { family: 'Noto Sans', weight: 400, style: 'normal', path: 'NotoSans-Regular.ttf', aliases: [] },
    { family: 'Noto Sans', weight: 700, style: 'normal', path: 'NotoSans-Bold.ttf', aliases: [] },
    {
      family: 'Noto Sans Math',
      weight: 400,
      style: 'normal',
      path: 'NotoSansMath-Regular.ttf',
      aliases: ['Apple Symbols', 'Segoe UI Symbol'],
    },
    {
      family: 'Noto Color Emoji',
      weight: 400,
      style: 'normal',
      path: 'NotoColorEmoji.ttf',
      aliases: ['Apple Color Emoji', 'Segoe UI Emoji'],
    },
  ];
}

function fixtureFontFacesForHtml(fontFaces, html) {
  if (htmlNeedsEmojiFont(html)) {
    return fontFaces;
  }
  return fontFaces.filter((face) => !isEmojiFontFace(face));
}

function fixtureFontFilesForFaces(fontFaces) {
  return [
    ...new Set(fontFaces.map((face) => path.resolve(ROOT_DIR, 'fixtures', 'fonts', face.path))),
  ];
}

function isEmojiFontFace(face) {
  return (
    /emoji/i.test(face.family || '') ||
    /emoji/i.test(face.path || '') ||
    (face.aliases ?? []).some((alias) => /emoji/i.test(alias))
  );
}

function htmlNeedsEmojiFont(html) {
  return (
    /[\u{1F000}-\u{1FAFF}\u{2600}-\u{27BF}]/u.test(html) ||
    /&#x(?:1f[0-9a-f]{3,4}|2[6-7][0-9a-f]{2});/i.test(html) ||
    /&#(?:12[7-9]\d{3});/.test(html)
  );
}

function fixtureFontCss(fontFaces) {
  const css = ['<style id="email-render-fixture-fonts">'];
  for (const face of fontFaces) {
    const url = pathToFileURL(path.resolve(ROOT_DIR, 'fixtures', 'fonts', face.path)).href;
    for (const family of [face.family, ...(face.aliases ?? [])]) {
      css.push(fontFaceCss(family, face.weight, face.style ?? 'normal', url));
    }
  }
  css.push('</style>');
  return css.join('\n');
}

function fontFaceCss(family, weight, style, url) {
  const format = url.endsWith('.woff2') ? 'woff2' : 'truetype';
  return [
    '@font-face {',
    `font-family: "${family}";`,
    `src: url("${url}") format("${format}");`,
    `font-weight: ${weight};`,
    `font-style: ${style};`,
    'font-display: block;',
    '}',
  ].join(' ');
}

function escapeAttr(value) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('"', '&quot;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

function printResult(result) {
  if (result.status === 'render-failed') {
    const corpusIssues = formatCorpusIssues(result.corpusIssues);
    console.log(
      `${result.name}\tstatus render-failed\terror ${result.error}\tlog ${result.log ?? ''}\tcorpus ${corpusIssues}`,
    );
    return;
  }
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
  const corpusIssues = formatCorpusIssues(result.corpusIssues);
  console.log(
    `${result.name}\tbrowser ${result.browser.width}x${result.browser.height}\trust ${result.rust.width}x${result.rust.height}\theightΔ ${heightDelta}px\tdiff ${percent}%\ttext ${text}\ttext-coverage ${textCoverage}%\ttext-rect ${textRect}%\ttext-pixel ${textPixel}%\tmedia ${media}\tmedia-rect ${mediaRect}%\tnon-media ${nonMedia}%\tnon-media non-text ${nonMediaNonText}%\tfirst-bad ${firstBad}\twarnings ${result.warningCount}\tcorpus ${corpusIssues}`,
  );
}

function formatCorpusIssues(issues = []) {
  if (issues.length === 0) {
    return '';
  }
  return issues.map((issue) => `${issue.code}:${issue.count}`).join(',');
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
    '| Template | Provider | Group | Browser | Rust | Diff | Media Diff | Media Rect Δ | Text Diff | Text Coverage Δ | Text Rect Δ | Text Pixel Δ | First Bad Region | Non-Media Diff | Non-Media Non-Text Diff | Diff Target | Non-Media Target | Non-Media Non-Text Target | Warnings | Assets | Corpus Issues | Files |',
    '|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---|---|---|',
  ];

  for (const result of results) {
    if (result.status === 'render-failed') {
      const corpusIssues = formatCorpusIssues(result.corpusIssues);
      const logLink = result.log ? `[log](${result.log})` : '';
      lines.push(
        `| ${result.name} | ${result.provider ?? ''} | ${result.corpusGroup ?? ''} | failed | failed | 100.00% |  |  |  |  |  |  |  |  |  |  |  |  | 0 | 0/0/0 | ${corpusIssues} | ${logLink} |`,
      );
      continue;
    }
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
    const corpusIssues = formatCorpusIssues(result.corpusIssues);
    lines.push(
      `| ${result.name} | ${result.provider} | ${result.corpusGroup} | ${result.browser.width}x${result.browser.height} | ${result.rust.width}x${result.rust.height} | ${percent} | ${mediaPercent} | ${mediaRectPercent} | ${textPercent} | ${textCoveragePercent} | ${textRectPercent} | ${textPixelPercent} | ${firstBadRegion} | ${nonMediaPercent} | ${nonMediaNonTextPercent} | ${targetText} | ${nonMediaTargetText} | ${nonMediaNonTextTargetText} | ${result.warningCount} | ${assets} | ${corpusIssues} | [side-by-side](${result.sideBySidePng}) [browser](${result.browserPng}) [rust](${result.rustPng}) [diff](${result.diffPng}) [log](${result.log}) [diagnostics](${result.diagnosticsJson}) [layout](${result.layoutJson}) |`,
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
    '| Template | Provider | Group | Support | Status | Browser | Rust | Height Delta | Total Diff | Text Diff | Text Coverage Δ | Text Rect Δ | Text Pixel Δ | First Bad Region | Media Diff | Media Rect Δ | Non-Media Non-Text Diff | Semantic Limits | Warnings | Assets | Corpus Issues | Files |',
    '|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---|---:|---|---|---|',
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
    const corpusIssues = formatCorpusIssues(result.corpusIssues);
    lines.push(
      `| ${result.name} | ${result.provider} | ${result.corpusGroup} | ${result.supportTier} | ${status} | ${result.browser.width}x${result.browser.height} | ${result.rust.width}x${result.rust.height} | ${heightDeltaPx}px (${formatPercent(heightDeltaPercent)}) | ${formatPercent(result.diffRatio)} | ${textPercent} | ${textCoveragePercent} | ${textRectPercent} | ${textPixelPercent} | ${firstBadRegion} | ${mediaPercent} | ${mediaRectPercent} | ${formatPercent(result.nonMediaNonText.diffRatio)} | ${limits} | ${result.warningCount} | ${assets} | ${corpusIssues} | [side-by-side](${result.sideBySidePng}) [browser](${result.browserPng}) [rust](${result.rustPng}) [diff](${result.diffPng}) [log](${result.log}) [diagnostics](${result.diagnosticsJson}) [layout](${result.layoutJson}) |`,
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
      corpusGroup: result.corpusGroup,
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
      corpusIssues: result.corpusIssues,
      status: result.status ?? 'ok',
      error: result.error,
    }));
  return {
    generatedAt: new Date().toISOString(),
    resultCount: results.length,
    renderFailedCount: results.filter((result) => result.status === 'render-failed').length,
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
