#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import pixelmatch from 'pixelmatch';
import { chromium } from 'playwright';
import { PNG } from 'pngjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_WORK_DIR = '/tmp/email-render-playwright-compare';
const DEFAULT_WIDTH = 600;
const DEFAULT_TIMEOUT_MS = 15000;

const TEMPLATES = [
  [
    'leemunroe-inlined',
    'https://raw.githubusercontent.com/leemunroe/responsive-html-email-template/master/email-inlined.html',
  ],
  [
    'mailgun-action',
    'https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/action.html',
  ],
  [
    'mailgun-alert',
    'https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/alert.html',
  ],
  [
    'mailgun-billing',
    'https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/billing.html',
  ],
  [
    'waypoint-saas-otp',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-one-time-passcode-otp.html',
  ],
  [
    'waypoint-saas-receipt',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-subscription-receipt.html',
  ],
  [
    'waypoint-ecommerce-delivery',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-delivery-notification.html',
  ],
  [
    'waypoint-marketplace-qr',
    'https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-qr-tickets.html',
  ],
];

function parseArgs(argv) {
  const args = {
    width: DEFAULT_WIDTH,
    workDir: DEFAULT_WORK_DIR,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    limit: TEMPLATES.length,
    allowRemote: true,
    keep: false,
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
      case '--no-remote':
        args.allowRemote = false;
        break;
      case '--keep':
        args.keep = true;
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
  const templates = TEMPLATES.slice(0, args.limit);
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
  await writeFile(path.join(args.workDir, 'report.md'), renderMarkdownReport(results, args));
  console.log(`outputs: ${args.workDir}`);
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
  const baseUrl = pathToFileURL(path.dirname(template.htmlPath) + path.sep).href;
  await writeFile(preparedPath, buildBrowserDocument(sourceHtml, baseUrl, args.width));

  await browserScreenshot(browser, preparedPath, browserPath, args.width, args.timeoutMs);

  const renderArgs = [
    '--html',
    template.htmlPath,
    '--output',
    rustPath,
    '--width',
    String(args.width),
    '--timeout-ms',
    String(args.timeoutMs),
  ];
  if (args.allowRemote) {
    renderArgs.push('--allow-remote');
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

  const comparison = await comparePng(browserPath, rustPath, diffPath);
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
      return Math.max(1, Math.ceil(maxBottom || document.body.getBoundingClientRect().bottom));
    });
    await page.screenshot({
      path: outPath,
      clip: { x: 0, y: 0, width, height },
    });
  } finally {
    await page.close();
  }
}

async function comparePng(browserPath, rustPath, diffPath) {
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
  await writeFile(diffPath, PNG.sync.write(diff));
  return {
    browser: { width: browser.width, height: browser.height },
    rust: { width: rust.width, height: rust.height },
    compared: { width, height },
    diffPixels,
    diffRatio: diffPixels / (width * height),
  };
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
  console.log(
    `${result.name}\tbrowser ${result.browser.width}x${result.browser.height}\trust ${result.rust.width}x${result.rust.height}\tdiff ${percent}%\twarnings ${result.warningCount}`,
  );
}

function renderMarkdownReport(results, args) {
  const lines = [
    '# Playwright Comparison Report',
    '',
    `- Width: ${args.width}px`,
    `- Remote image loading in Rust renderer: ${args.allowRemote ? 'enabled' : 'disabled'}`,
    `- Output directory: \`${args.workDir}\``,
    '',
    '| Template | Browser | Rust | Diff | Warnings | Files |',
    '|---|---:|---:|---:|---:|---|',
  ];

  for (const result of results) {
    const percent = `${(result.diffRatio * 100).toFixed(2)}%`;
    lines.push(
      `| ${result.name} | ${result.browser.width}x${result.browser.height} | ${result.rust.width}x${result.rust.height} | ${percent} | ${result.warningCount} | [side-by-side](${result.sideBySidePng}) [browser](${result.browserPng}) [rust](${result.rustPng}) [diff](${result.diffPng}) [log](${result.log}) |`,
    );
  }
  lines.push('');
  lines.push('Notes: pixel comparison pads the shorter image with white before diffing.');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
