#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import pixelmatch from 'pixelmatch';
import { PNG } from 'pngjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_RENDERER = path.join(ROOT_DIR, 'target', 'debug', 'mail-canvas');
const DEFAULT_WIDTH = 800;
const DEFAULT_VIEWPORT_HEIGHT = 1200;
const DEFAULT_SCALE = 1;
const DEFAULT_FONT_DIR = path.join(ROOT_DIR, 'fixtures', 'fonts');

function parseArgs(argv) {
  const command = argv[0];
  if (!command || command === '--help' || command === '-h') {
    printUsage();
    process.exit(command ? 0 : 1);
  }
  return { command, rest: argv.slice(1) };
}

async function main() {
  const { command, rest } = parseArgs(process.argv.slice(2));
  switch (command) {
    case 'preview':
      await preview(rest);
      break;
    case 'diff':
      await diff(rest);
      break;
    case 'snapshot':
      await snapshot(rest);
      break;
    case 'check':
      await check(rest);
      break;
    default:
      throw new Error(`unknown command: ${command}`);
  }
}

function printUsage() {
  console.error(`mail-canvas-tools

Usage:
  node scripts/mail_canvas_tools.mjs preview <email.html> [--watch] [--port 4177] [--out-dir .mail-canvas-preview]
  node scripts/mail_canvas_tools.mjs diff <before.html> <after.html> --out report-dir
  node scripts/mail_canvas_tools.mjs snapshot <templates/**/*.html> --baseline snapshots [--update]
  node scripts/mail_canvas_tools.mjs check <email.html> [--warnings-json warnings.json]

Common options:
  --width <px>              CSS viewport width, default ${DEFAULT_WIDTH}
  --viewport-height <px>    initial CSS viewport height, default ${DEFAULT_VIEWPORT_HEIGHT}
  --scale <n>               output device scale, default ${DEFAULT_SCALE}
  --profile <name>          generic, desktop-800, mobile-375, or thumbnail
  --font-dir <dir>          deterministic font directory, default fixtures/fonts
  --renderer <path>         mail-canvas binary path, default target/debug/mail-canvas
  --allow-remote            allow remote resources
  --allow-http              allow HTTP remote resources with --allow-remote
`);
}

function parseCommon(rest, positionalCount) {
  const options = {
    renderer: DEFAULT_RENDERER,
    width: DEFAULT_WIDTH,
    viewportHeight: DEFAULT_VIEWPORT_HEIGHT,
    scale: DEFAULT_SCALE,
    fontDir: DEFAULT_FONT_DIR,
    allowRemote: false,
    allowHttp: false,
    maxHeight: null,
    profile: 'thumbnail',
  };
  const explicit = new Set();
  const positional = [];
  for (let index = 0; index < rest.length; index += 1) {
    const arg = rest[index];
    const next = () => {
      index += 1;
      if (index >= rest.length) throw new Error(`missing value for ${arg}`);
      return rest[index];
    };
    switch (arg) {
      case '--renderer':
        options.renderer = path.resolve(next());
        break;
      case '--width':
        options.width = positiveInt(next(), '--width');
        explicit.add('width');
        break;
      case '--viewport-height':
        options.viewportHeight = positiveInt(next(), '--viewport-height');
        explicit.add('viewportHeight');
        break;
      case '--scale':
        options.scale = positiveNumber(next(), '--scale');
        explicit.add('scale');
        break;
      case '--profile':
        applyProfile(options, next(), explicit);
        break;
      case '--font-dir':
        options.fontDir = path.resolve(next());
        break;
      case '--allow-remote':
        options.allowRemote = true;
        break;
      case '--allow-http':
        options.allowHttp = true;
        break;
      case '--max-height':
        options.maxHeight = positiveInt(next(), '--max-height');
        break;
      default:
        if (arg.startsWith('--')) {
          positional.push({ option: arg, value: nextMaybe(rest, index + 1) });
          if (positional.at(-1).value !== null) index += 1;
        } else {
          positional.push(arg);
        }
    }
  }
  const args = [];
  const commandOptions = [];
  for (const entry of positional) {
    if (typeof entry === 'string' && args.length < positionalCount) {
      args.push(entry);
    } else {
      commandOptions.push(entry);
    }
  }
  return { options, args, commandOptions };
}

function applyProfile(options, profile, explicit) {
  const profiles = {
    generic: { width: 600, viewportHeight: 800, scale: 1 },
    'desktop-800': { width: 800, viewportHeight: 1200, scale: 1 },
    'mobile-375': { width: 375, viewportHeight: 900, scale: 1 },
    thumbnail: { width: 800, viewportHeight: 1200, scale: 1 },
  };
  const preset = profiles[profile];
  if (!preset) {
    throw new Error(`unknown --profile ${profile}; expected ${Object.keys(profiles).join(', ')}`);
  }
  options.profile = profile;
  if (!explicit.has('width')) options.width = preset.width;
  if (!explicit.has('viewportHeight')) options.viewportHeight = preset.viewportHeight;
  if (!explicit.has('scale')) options.scale = preset.scale;
}

function nextMaybe(rest, index) {
  if (index >= rest.length || rest[index].startsWith('--')) return null;
  return rest[index];
}

function positiveInt(value, name) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${name} must be positive`);
  return parsed;
}

function positiveNumber(value, name) {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${name} must be positive`);
  return parsed;
}

async function preview(rest) {
  const { options, args, commandOptions } = parseCommon(rest, 1);
  const htmlPath = args[0] ? path.resolve(args[0]) : null;
  if (!htmlPath) throw new Error('preview requires <email.html>');
  const previewOptions = {
    outDir: path.resolve('.mail-canvas-preview'),
    watch: false,
    port: 4177,
  };
  applyCommandOptions(commandOptions, {
    '--out-dir': (value) => {
      previewOptions.outDir = path.resolve(requireValue('--out-dir', value));
    },
    '--watch': () => {
      previewOptions.watch = true;
    },
    '--port': (value) => {
      previewOptions.port = positiveInt(requireValue('--port', value), '--port');
    },
  });

  await mkdir(previewOptions.outDir, { recursive: true });
  const state = {
    htmlPath,
    outputPng: path.join(previewOptions.outDir, 'preview.png'),
    warningsJson: path.join(previewOptions.outDir, 'warnings.json'),
    reportJson: path.join(previewOptions.outDir, 'preview.json'),
    revision: 0,
    lastRender: null,
  };

  await ensureRenderer(options.renderer);
  await renderPreview(state, options);
  console.log(`preview image: ${state.outputPng}`);
  console.log(`diagnostics: ${state.warningsJson}`);
  if (!previewOptions.watch) return;

  const server = startPreviewServer(state, previewOptions.port);
  console.log(`preview server: http://127.0.0.1:${previewOptions.port}`);
  await watchHtmlAndAssets(htmlPath, async () => {
    await renderPreview(state, options).catch((error) => {
      state.lastRender = { ok: false, error: String(error?.message ?? error) };
      state.revision += 1;
      console.error(state.lastRender.error);
    });
  });
  server.close();
}

async function renderPreview(state, options) {
  const startedAt = Date.now();
  renderHtml(state.htmlPath, state.outputPng, state.warningsJson, options);
  const diagnostics = JSON.parse(await readFile(state.warningsJson, 'utf8'));
  state.revision += 1;
  state.lastRender = {
    ok: true,
    revision: state.revision,
    renderedAt: new Date().toISOString(),
    elapsedMs: Date.now() - startedAt,
    warnings: diagnostics.warnings?.length ?? 0,
    assets: diagnostics.assets?.length ?? 0,
    outputPng: state.outputPng,
    warningsJson: state.warningsJson,
  };
  await writeFile(state.reportJson, `${JSON.stringify(state.lastRender, null, 2)}\n`);
}

function startPreviewServer(state, port) {
  const server = http.createServer(async (request, response) => {
    try {
      if (request.url === '/' || request.url?.startsWith('/?')) {
        response.setHeader('content-type', 'text/html; charset=utf-8');
        response.end(previewHtml());
        return;
      }
      if (request.url?.startsWith('/state.json')) {
        response.setHeader('content-type', 'application/json; charset=utf-8');
        response.end(`${JSON.stringify(state.lastRender ?? {}, null, 2)}\n`);
        return;
      }
      if (request.url?.startsWith('/preview.png')) {
        response.setHeader('content-type', 'image/png');
        createReadStream(state.outputPng).pipe(response);
        return;
      }
      response.statusCode = 404;
      response.end('not found');
    } catch (error) {
      response.statusCode = 500;
      response.end(String(error?.message ?? error));
    }
  });
  server.listen(port, '127.0.0.1');
  return server;
}

function previewHtml() {
  return `<!doctype html>
<meta charset="utf-8">
<title>MailCanvas Preview</title>
<style>
body { margin: 0; font: 14px system-ui, sans-serif; background: #f6f7f9; color: #111827; }
header { position: sticky; top: 0; padding: 10px 14px; background: white; border-bottom: 1px solid #d9dee8; display: flex; gap: 16px; align-items: center; }
img { display: block; margin: 20px auto; background: white; box-shadow: 0 1px 8px rgb(0 0 0 / 12%); max-width: calc(100vw - 40px); height: auto; }
code { color: #4b5563; }
</style>
<header><strong>MailCanvas Preview</strong><code id="status">loading</code></header>
<img id="preview" src="/preview.png">
<script>
let revision = -1;
async function poll() {
  const state = await fetch('/state.json', { cache: 'no-store' }).then((r) => r.json()).catch(() => null);
  if (!state) return;
  document.getElementById('status').textContent = state.ok ? \`rev \${state.revision} · \${state.elapsedMs}ms · warnings \${state.warnings}\` : state.error;
  if (state.ok && state.revision !== revision) {
    revision = state.revision;
    document.getElementById('preview').src = \`/preview.png?rev=\${revision}\`;
  }
}
setInterval(poll, 750);
poll();
</script>`;
}

async function watchHtmlAndAssets(htmlPath, onChange) {
  let lastSignature = '';
  const root = path.dirname(htmlPath);
  setInterval(async () => {
    const signature = await directorySignature(root);
    if (signature !== lastSignature) {
      lastSignature = signature;
      await onChange();
    }
  }, 750).unref?.();
  lastSignature = await directorySignature(root);
  await new Promise(() => {});
}

async function directorySignature(root) {
  const files = await collectFiles(root, (file) => /\.(html?|css|png|jpe?g|gif|webp|svg|woff2?|ttf|otf)$/i.test(file));
  const hash = createHash('sha256');
  for (const file of files.sort()) {
    const info = await stat(file);
    hash.update(file);
    hash.update(String(info.mtimeMs));
    hash.update(String(info.size));
  }
  return hash.digest('hex');
}

async function diff(rest) {
  const { options, args, commandOptions } = parseCommon(rest, 2);
  if (args.length !== 2) throw new Error('diff requires <before.html> <after.html>');
  const diffOptions = { outDir: null };
  applyCommandOptions(commandOptions, {
    '--out': (value) => {
      diffOptions.outDir = path.resolve(requireValue('--out', value));
    },
  });
  if (!diffOptions.outDir) throw new Error('diff requires --out <report-dir>');
  await ensureRenderer(options.renderer);
  await mkdir(diffOptions.outDir, { recursive: true });
  const paths = {
    before: path.join(diffOptions.outDir, 'before.png'),
    after: path.join(diffOptions.outDir, 'after.png'),
    beforeWarnings: path.join(diffOptions.outDir, 'before.warnings.json'),
    afterWarnings: path.join(diffOptions.outDir, 'after.warnings.json'),
    diff: path.join(diffOptions.outDir, 'diff.png'),
    sideBySide: path.join(diffOptions.outDir, 'side-by-side.png'),
    reportJson: path.join(diffOptions.outDir, 'report.json'),
    reportMd: path.join(diffOptions.outDir, 'report.md'),
  };
  renderHtml(path.resolve(args[0]), paths.before, paths.beforeWarnings, options);
  renderHtml(path.resolve(args[1]), paths.after, paths.afterWarnings, options);
  const result = await comparePngFiles(paths.before, paths.after, paths.diff);
  await writeTriptych([paths.before, paths.after, paths.diff], paths.sideBySide);
  const report = {
    generatedAt: new Date().toISOString(),
    profile: options.profile,
    width: options.width,
    scale: options.scale,
    before: path.resolve(args[0]),
    after: path.resolve(args[1]),
    diffRatio: result.diffRatio,
    diffPixels: result.diffPixels,
    compared: result.compared,
    artifacts: paths,
  };
  await writeFile(paths.reportJson, `${JSON.stringify(report, null, 2)}\n`);
  await writeFile(paths.reportMd, renderDiffMarkdown(report));
  console.log(`diff ${(result.diffRatio * 100).toFixed(2)}% -> ${paths.sideBySide}`);
}

async function snapshot(rest) {
  const { options, args, commandOptions } = parseCommon(rest, Number.POSITIVE_INFINITY);
  const snapshotOptions = {
    baselineDir: null,
    update: false,
    actualDir: null,
  };
  applyCommandOptions(commandOptions, {
    '--baseline': (value) => {
      snapshotOptions.baselineDir = path.resolve(requireValue('--baseline', value));
    },
    '--actual-dir': (value) => {
      snapshotOptions.actualDir = path.resolve(requireValue('--actual-dir', value));
    },
    '--update': () => {
      snapshotOptions.update = true;
    },
  });
  if (args.length === 0) throw new Error('snapshot requires at least one HTML file, directory, or glob');
  if (!snapshotOptions.baselineDir) throw new Error('snapshot requires --baseline <dir>');
  snapshotOptions.actualDir ??= path.join(snapshotOptions.baselineDir, '.actual');
  await ensureRenderer(options.renderer);
  await mkdir(snapshotOptions.baselineDir, { recursive: true });
  await rm(snapshotOptions.actualDir, { recursive: true, force: true });
  await mkdir(snapshotOptions.actualDir, { recursive: true });

  const htmlFiles = await expandInputs(args);
  const results = [];
  for (const htmlPath of htmlFiles) {
    const id = snapshotId(htmlPath);
    const actualPng = path.join(snapshotOptions.actualDir, `${id}.png`);
    const warningsJson = path.join(snapshotOptions.actualDir, `${id}.warnings.json`);
    const baselinePng = path.join(snapshotOptions.baselineDir, `${id}.png`);
    const diffPng = path.join(snapshotOptions.actualDir, `${id}.diff.png`);
    renderHtml(htmlPath, actualPng, warningsJson, options);
    let status = 'missing';
    let comparison = null;
    if (await exists(baselinePng)) {
      comparison = await comparePngFiles(baselinePng, actualPng, diffPng);
      status = comparison.diffPixels === 0 ? 'passed' : 'changed';
    }
    if (snapshotOptions.update) {
      const image = await readFile(actualPng);
      await writeFile(baselinePng, image);
      status = snapshotOptions.update ? 'updated' : 'created';
    }
    results.push({
      id,
      htmlPath,
      baselinePng,
      actualPng,
      warningsJson,
      diffPng: comparison ? diffPng : null,
      status,
      diffRatio: comparison?.diffRatio ?? null,
      diffPixels: comparison?.diffPixels ?? null,
    });
    console.log(`${id}\t${status}\t${comparison ? `${(comparison.diffRatio * 100).toFixed(2)}%` : ''}`);
  }
  const manifest = {
    generatedAt: new Date().toISOString(),
    profile: options.profile,
    width: options.width,
    scale: options.scale,
    results,
  };
  await writeFile(path.join(snapshotOptions.baselineDir, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  const failures = results.filter((result) => result.status === 'changed' || result.status === 'missing');
  if (failures.length > 0 && !snapshotOptions.update) {
    process.exitCode = 1;
  }
}

async function check(rest) {
  const { options, args, commandOptions } = parseCommon(rest, 1);
  const htmlPath = args[0] ? path.resolve(args[0]) : null;
  if (!htmlPath) throw new Error('check requires <email.html>');
  const checkOptions = {
    warningsJson: path.resolve('warnings.json'),
    outDir: path.resolve('.mail-canvas-check'),
  };
  applyCommandOptions(commandOptions, {
    '--warnings-json': (value) => {
      checkOptions.warningsJson = path.resolve(requireValue('--warnings-json', value));
    },
    '--out-dir': (value) => {
      checkOptions.outDir = path.resolve(requireValue('--out-dir', value));
    },
  });
  await ensureRenderer(options.renderer);
  await mkdir(checkOptions.outDir, { recursive: true });
  const pngPath = path.join(checkOptions.outDir, `${snapshotId(htmlPath)}.png`);
  renderHtml(htmlPath, pngPath, checkOptions.warningsJson, options);
  const diagnostics = JSON.parse(await readFile(checkOptions.warningsJson, 'utf8'));
  const warnings = diagnostics.warnings ?? [];
  const assets = diagnostics.assets ?? [];
  const failedAssets = assets.filter((asset) => asset.status === 'failed');
  const blockedAssets = assets.filter((asset) => asset.status === 'blocked');
  console.log(JSON.stringify({
    html: htmlPath,
    png: pngPath,
    warningsJson: checkOptions.warningsJson,
    warnings: warnings.length,
    assets: assets.length,
    failedAssets: failedAssets.length,
    blockedAssets: blockedAssets.length,
  }, null, 2));
  if (warnings.length > 0 || failedAssets.length > 0) {
    process.exitCode = 1;
  }
}

function applyCommandOptions(entries, handlers) {
  for (const entry of entries) {
    if (typeof entry === 'string') {
      throw new Error(`unexpected argument: ${entry}`);
    }
    const handler = handlers[entry.option];
    if (!handler) throw new Error(`unknown option: ${entry.option}`);
    handler(entry.value);
  }
}

function requireValue(option, value) {
  if (value === null || value === undefined) throw new Error(`${option} requires a value`);
  return value;
}

async function ensureRenderer(renderer) {
  if (await exists(renderer)) return;
  const build = spawnSync('cargo', ['build'], { cwd: ROOT_DIR, encoding: 'utf8', stdio: 'pipe' });
  if (build.status !== 0) {
    throw new Error(`cargo build failed:\n${build.stdout}\n${build.stderr}`);
  }
}

function renderHtml(htmlPath, outputPath, warningsJsonPath, options) {
  const args = [
    '--html',
    htmlPath,
    '--output',
    outputPath,
    '--warnings-json',
    warningsJsonPath,
    '--width',
    String(options.width),
    '--viewport-height',
    String(options.viewportHeight),
    '--scale',
    String(options.scale),
    '--base-url',
    pathToFileURL(`${path.dirname(htmlPath)}${path.sep}`).href,
    '--font-dir',
    options.fontDir,
  ];
  if (options.maxHeight !== null) args.push('--max-height', String(options.maxHeight));
  if (options.allowRemote) args.push('--allow-remote');
  if (options.allowHttp) args.push('--allow-http');

  const render = spawnSync(options.renderer, args, { cwd: ROOT_DIR, encoding: 'utf8', stdio: 'pipe' });
  if (render.status !== 0) {
    throw new Error(`mail-canvas failed for ${htmlPath}:\n${render.stdout}\n${render.stderr}`);
  }
}

async function comparePngFiles(leftPath, rightPath, diffPath) {
  const left = PNG.sync.read(await readFile(leftPath));
  const right = PNG.sync.read(await readFile(rightPath));
  const width = Math.max(left.width, right.width);
  const height = Math.max(left.height, right.height);
  const leftCanvas = padPng(left, width, height);
  const rightCanvas = padPng(right, width, height);
  const diff = new PNG({ width, height });
  const diffPixels = pixelmatch(leftCanvas.data, rightCanvas.data, diff.data, width, height, {
    threshold: 0.1,
    includeAA: true,
  });
  await mkdir(path.dirname(diffPath), { recursive: true });
  await writeFile(diffPath, PNG.sync.write(diff));
  return {
    compared: { width, height },
    diffPixels,
    diffRatio: diffPixels / Math.max(1, width * height),
  };
}

function padPng(source, width, height) {
  if (source.width === width && source.height === height) return source;
  const canvas = new PNG({ width, height });
  canvas.data.fill(255);
  copyPng(source, canvas, 0, 0);
  return canvas;
}

async function writeTriptych(paths, outPath) {
  const images = await Promise.all(paths.map(async (file) => PNG.sync.read(await readFile(file))));
  const gutter = 12;
  const width = images.reduce((sum, image) => sum + image.width, 0) + gutter * (images.length - 1);
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

function renderDiffMarkdown(report) {
  return `# MailCanvas Diff

- Before: \`${report.before}\`
- After: \`${report.after}\`
- Compared: ${report.compared.width}x${report.compared.height}px
- Diff: ${(report.diffRatio * 100).toFixed(2)}% (${report.diffPixels} px)

Artifacts:

- \`${path.basename(report.artifacts.before)}\`
- \`${path.basename(report.artifacts.after)}\`
- \`${path.basename(report.artifacts.diff)}\`
- \`${path.basename(report.artifacts.sideBySide)}\`
`;
}

async function expandInputs(inputs) {
  const files = [];
  for (const input of inputs) {
    if (input.includes('*')) {
      files.push(...(await expandGlob(input)));
      continue;
    }
    const resolved = path.resolve(input);
    const info = await stat(resolved);
    if (info.isDirectory()) {
      files.push(...(await collectFiles(resolved, (file) => /\.html?$/i.test(file))));
    } else {
      files.push(resolved);
    }
  }
  return [...new Set(files)].sort();
}

async function expandGlob(pattern) {
  const absolute = path.resolve(pattern).replaceAll('\\', '/');
  const starIndex = absolute.indexOf('*');
  const prefix = absolute.slice(0, starIndex);
  const root = prefix.slice(0, prefix.lastIndexOf('/')) || '/';
  const relativePattern = absolute.slice(root.length + (root.endsWith('/') ? 0 : 1));
  const matcher = globMatcher(relativePattern);
  const candidates = await collectFiles(root, (file) => /\.html?$/i.test(file));
  return candidates.filter((file) => matcher(path.relative(root, file).replaceAll('\\', '/')));
}

function globMatcher(pattern) {
  let source = '';
  for (let index = 0; index < pattern.length; index += 1) {
    const char = pattern[index];
    const next = pattern[index + 1];
    const afterNext = pattern[index + 2];
    if (char === '*' && next === '*' && afterNext === '/') {
      source += '(?:.*/)?';
      index += 2;
    } else if (char === '*' && next === '*') {
      source += '.*';
      index += 1;
    } else if (char === '*') {
      source += '[^/]*';
    } else {
      source += char.replace(/[.+^${}()|[\]\\]/g, '\\$&');
    }
  }
  const regexp = new RegExp(`^${source}$`);
  return (value) => regexp.test(value);
}

async function collectFiles(root, predicate) {
  const files = [];
  const entries = await readdir(root, { withFileTypes: true }).catch(() => []);
  for (const entry of entries) {
    const fullPath = path.join(root, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectFiles(fullPath, predicate)));
    } else if (entry.isFile() && predicate(fullPath)) {
      files.push(fullPath);
    }
  }
  return files;
}

function snapshotId(htmlPath) {
  const relative = path.relative(ROOT_DIR, htmlPath).replaceAll(path.sep, '-').replace(/\.html?$/i, '');
  return relative.replace(/[^a-z0-9_-]+/gi, '-').replace(/^-+|-+$/g, '') || 'email';
}

async function exists(file) {
  return stat(file).then(() => true, () => false);
}

main().catch((error) => {
  console.error(error?.stack ?? error?.message ?? String(error));
  process.exitCode = 1;
});
