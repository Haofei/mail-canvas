#!/usr/bin/env node
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';

const DEFAULT_LIMITS = {
  maxImageBytes: '20971520',
  maxDecodedPixels: '50000000',
  maxTotalResourceBytes: '134217728',
};

function parseArgs(argv) {
  const args = {
    dir: 'corpus/reallygoodemails',
    workDir: '/tmp/mail-canvas-rge-local',
    width: '800',
    timeoutMs: '30000',
    batchSize: 50,
    onlyMissingFrom: [],
    seenRegistry: 'corpus/reallygoodemails/run-registry.json',
    excludeSeen: false,
    updateSeenRegistry: false,
    clearSeenRegistry: false,
    dryRun: false,
    importReport: null,
    limit: null,
    noRemote: true,
    fixtureFonts: true,
    continueOnError: true,
    ...DEFAULT_LIMITS,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      i += 1;
      if (i >= argv.length) throw new Error(`missing value for ${arg}`);
      return argv[i];
    };

    switch (arg) {
      case '--dir':
        args.dir = next();
        break;
      case '--work-dir':
        args.workDir = next();
        break;
      case '--width':
        args.width = next();
        break;
      case '--timeout-ms':
        args.timeoutMs = next();
        break;
      case '--batch-size':
        args.batchSize = Number.parseInt(next(), 10);
        break;
      case '--limit':
        args.limit = Number.parseInt(next(), 10);
        break;
      case '--only-missing-from':
        args.onlyMissingFrom.push(next());
        break;
      case '--seen-registry':
        args.seenRegistry = next();
        break;
      case '--exclude-seen':
        args.excludeSeen = true;
        break;
      case '--update-seen-registry':
        args.updateSeenRegistry = true;
        break;
      case '--clear-seen-registry':
        args.clearSeenRegistry = true;
        break;
      case '--dry-run':
        args.dryRun = true;
        break;
      case '--import-report':
        args.importReport = next();
        args.updateSeenRegistry = true;
        break;
      case '--allow-remote':
        args.noRemote = false;
        break;
      case '--no-fixture-fonts':
        args.fixtureFonts = false;
        break;
      case '--stop-on-error':
        args.continueOnError = false;
        break;
      case '--max-image-bytes':
        args.maxImageBytes = next();
        break;
      case '--max-decoded-pixels':
        args.maxDecodedPixels = next();
        break;
      case '--max-total-resource-bytes':
        args.maxTotalResourceBytes = next();
        break;
      case '--help':
        printHelp();
        process.exit(0);
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!Number.isFinite(args.batchSize) || args.batchSize < 1) {
    throw new Error('--batch-size must be a positive integer');
  }
  if (args.limit !== null && (!Number.isFinite(args.limit) || args.limit < 1)) {
    throw new Error('--limit must be a positive integer');
  }
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/compare_local_rge_corpus.mjs [options]

Scans a local, gitignored Really Good Emails corpus directory and compares every
HTML file against Chromium using scripts/playwright_compare.mjs.

Options:
  --dir <path>                  Local RGE template directory (default: corpus/reallygoodemails)
  --work-dir <path>             Output directory (default: /tmp/mail-canvas-rge-local)
  --width <px>                  Render width (default: 800)
  --timeout-ms <ms>             Per-template timeout (default: 30000)
  --batch-size <n>              Templates per Playwright batch (default: 50)
  --limit <n>                   Compare only the first n scanned templates
  --only-missing-from <json>    Skip names already present in a comparison.json; repeatable
  --seen-registry <json>        Local content-MD5 run registry (default: corpus/reallygoodemails/run-registry.json)
  --exclude-seen                Skip templates whose HTML/assets content MD5 was already recorded
  --update-seen-registry        Record selected template content MD5s and latest compare status
  --clear-seen-registry         Delete the local run registry before scanning
  --dry-run                     Print scan/skip counts without rendering
  --import-report <json>        Mark matching local templates as seen from an existing comparison.json
  --allow-remote                Allow remote resources during compare
  --no-fixture-fonts            Do not force fixture fonts
  --stop-on-error               Stop the batch on first compare error
`);
}

function readCompletedNames(reportPaths) {
  const names = new Set();
  for (const reportPath of reportPaths) {
    if (!fs.existsSync(reportPath)) continue;
    const report = JSON.parse(fs.readFileSync(reportPath, 'utf8'));
    for (const result of report.results || []) {
      if (result?.name) names.add(result.name);
    }
  }
  return names;
}

function readSeenRegistry(registryPath) {
  if (!registryPath || !fs.existsSync(registryPath)) {
    return emptySeenRegistry();
  }
  const parsed = JSON.parse(fs.readFileSync(registryPath, 'utf8'));
  const entries = Array.isArray(parsed.templates) ? parsed.templates : [];
  return {
    schemaVersion: parsed.schemaVersion || 1,
    updatedAt: parsed.updatedAt || null,
    templates: entries,
  };
}

function emptySeenRegistry() {
  return { schemaVersion: 1, updatedAt: null, templates: [] };
}

function scanTemplates(dir, completedNames, seenRegistry, options) {
  if (!fs.existsSync(dir)) {
    throw new Error(`template directory does not exist: ${dir}`);
  }
  const seenByName = new Map((seenRegistry.templates || []).map((entry) => [entry.name, entry]));
  const all = fs
    .readdirSync(dir)
    .filter((file) => file.endsWith('.html'))
    .sort()
    .map((file) => {
      const htmlPath = path.join(dir, file);
      return {
        file,
        name: path.basename(file, '.html'),
        htmlPath,
        fingerprint: fingerprintTemplate(htmlPath),
      };
    });
  const stats = {
    total: all.length,
    skippedCompleted: 0,
    skippedSeen: 0,
    selectedBeforeLimit: 0,
  };
  let files = all.filter((entry) => {
    if (completedNames.has(entry.name)) {
      stats.skippedCompleted += 1;
      return false;
    }
    const seen = seenByName.get(entry.name);
    if (options.excludeSeen && seen?.contentMd5 === entry.fingerprint.contentMd5) {
      stats.skippedSeen += 1;
      return false;
    }
    return true;
  });
  stats.selectedBeforeLimit = files.length;
  if (options.limit !== null) files = files.slice(0, options.limit);
  stats.selected = files.length;
  return { templates: files, stats };
}

function fingerprintTemplate(htmlPath) {
  const html = fs.readFileSync(htmlPath);
  const assetDir = htmlPath.replace(/\.html$/i, '.assets');
  const assets = fingerprintAssets(assetDir);
  const assetManifest = assets
    .map((asset) => `${asset.path}\0${asset.bytes}\0${asset.md5}`)
    .join('\n');
  return {
    htmlMd5: md5(html),
    htmlBytes: html.length,
    assetCount: assets.length,
    assetBytes: assets.reduce((sum, asset) => sum + asset.bytes, 0),
    assetManifestMd5: md5(assetManifest),
    contentMd5: createHash('md5')
      .update(md5(html))
      .update('\0')
      .update(md5(assetManifest))
      .digest('hex'),
  };
}

function fingerprintAssets(assetDir) {
  if (!fs.existsSync(assetDir)) {
    return [];
  }
  return walkFiles(assetDir)
    .map((filePath) => {
      const bytes = fs.readFileSync(filePath);
      return {
        path: path.relative(assetDir, filePath).replaceAll(path.sep, '/'),
        bytes: bytes.length,
        md5: md5(bytes),
      };
    })
    .sort((left, right) => left.path.localeCompare(right.path));
}

function walkFiles(dir) {
  const files = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...walkFiles(entryPath));
    } else if (entry.isFile()) {
      files.push(entryPath);
    }
  }
  return files;
}

function md5(value) {
  return createHash('md5').update(value).digest('hex');
}

function chunks(items, size) {
  const out = [];
  for (let i = 0; i < items.length; i += size) {
    out.push(items.slice(i, i + size));
  }
  return out;
}

function runNode(args) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, args, { stdio: 'inherit' });
    child.on('error', reject);
    child.on('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`command failed with exit code ${code}`));
    });
  });
}

function compareArgs(batch, options, batchDir) {
  const args = ['scripts/playwright_compare.mjs'];
  for (const entry of batch) {
    args.push('--html', entry.htmlPath, '--name', entry.name);
  }
  args.push(
    '--width',
    String(options.width),
    '--work-dir',
    batchDir,
    '--timeout-ms',
    String(options.timeoutMs),
    '--max-image-bytes',
    String(options.maxImageBytes),
    '--max-decoded-pixels',
    String(options.maxDecodedPixels),
    '--max-total-resource-bytes',
    String(options.maxTotalResourceBytes),
  );
  if (options.noRemote) args.push('--no-remote');
  if (options.fixtureFonts) args.push('--fixture-fonts');
  if (options.continueOnError) args.push('--continue-on-error');
  return args;
}

function mergeReports(batchDirs, outPath) {
  const results = [];
  const failures = [];
  for (const batchDir of batchDirs) {
    const reportPath = path.join(batchDir, 'comparison.json');
    if (!fs.existsSync(reportPath)) continue;
    const report = JSON.parse(fs.readFileSync(reportPath, 'utf8'));
    results.push(...(report.results || []));
    failures.push(...(report.failures || []));
  }
  results.sort((a, b) => a.name.localeCompare(b.name));
  const merged = {
    generatedAt: new Date().toISOString(),
    results,
    failures,
  };
  fs.writeFileSync(outPath, `${JSON.stringify(merged, null, 2)}\n`);
  return merged;
}

function updateSeenRegistry(registryPath, dir, templates, merged, workDir) {
  const registry = readSeenRegistry(registryPath);
  const byName = new Map((registry.templates || []).map((entry) => [entry.name, entry]));
  const resultsByName = new Map((merged.results || []).map((result) => [result.name, result]));
  const failuresByName = new Map((merged.failures || []).map((failure) => [failure.name, failure]));
  const runAt = merged.generatedAt || new Date().toISOString();

  for (const template of templates) {
    const result = resultsByName.get(template.name);
    const failure = failuresByName.get(template.name);
    byName.set(template.name, {
      name: template.name,
      file: path.relative(dir, template.htmlPath).replaceAll(path.sep, '/'),
      ...template.fingerprint,
      lastRun: {
        at: runAt,
        workDir,
        status: result?.status || (failure ? 'compare-failed' : 'missing-result'),
        diffPercent: result ? Number(((result.diffRatio || 0) * 100).toFixed(2)) : null,
        heightDelta: result ? (result.rust?.height || 0) - (result.browser?.height || 0) : null,
        error: failure?.error || result?.error || null,
      },
    });
  }

  const next = {
    schemaVersion: registry.schemaVersion || 1,
    updatedAt: new Date().toISOString(),
    sourceDir: dir,
    templates: [...byName.values()].sort((left, right) => left.name.localeCompare(right.name)),
  };
  fs.mkdirSync(path.dirname(registryPath), { recursive: true });
  fs.writeFileSync(registryPath, `${JSON.stringify(next, null, 2)}\n`);
  return next;
}

function importReportIntoSeenRegistry(registryPath, dir, templates, reportPath) {
  const report = JSON.parse(fs.readFileSync(reportPath, 'utf8'));
  const reportedNames = new Set([
    ...(report.results || []).map((result) => result.name).filter(Boolean),
    ...(report.failures || []).map((failure) => failure.name).filter(Boolean),
  ]);
  const matchingTemplates = templates.filter((template) => reportedNames.has(template.name));
  const imported = updateSeenRegistry(
    registryPath,
    dir,
    matchingTemplates,
    {
      generatedAt: report.generatedAt || new Date().toISOString(),
      results: report.results || [],
      failures: report.failures || [],
    },
    path.dirname(reportPath),
  );
  return { imported, count: matchingTemplates.length, reported: reportedNames.size };
}

function printTop(results, count = 25) {
  const rows = [...results].sort((a, b) => (b.diffRatio || 0) - (a.diffRatio || 0));
  for (const result of rows.slice(0, count)) {
    const first = result.firstBadRegion
      ? `${result.firstBadRegion.y0}-${result.firstBadRegion.y1}`
      : '';
    const heightDelta = (result.rust?.height || 0) - (result.browser?.height || 0);
    console.log(
      `${((result.diffRatio || 0) * 100).toFixed(2).padStart(6)}% ` +
        `media ${((result.media?.diffRatio || 0) * 100).toFixed(2).padStart(6)}% ` +
        `nonText ${((result.nonMediaNonText?.diffRatio || 0) * 100).toFixed(2).padStart(6)}% ` +
        `h ${String(heightDelta).padStart(5)} first ${first.padEnd(9)} ${result.name}`,
    );
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.clearSeenRegistry && fs.existsSync(options.seenRegistry)) {
    fs.rmSync(options.seenRegistry, { force: true });
    console.error(`cleared seen registry: ${options.seenRegistry}`);
  }
  const completedNames = readCompletedNames(options.onlyMissingFrom);
  const seenRegistry = readSeenRegistry(options.seenRegistry);
  const { templates, stats } = scanTemplates(options.dir, completedNames, seenRegistry, options);
  console.error(
    `found ${stats.total} template(s) in ${options.dir}; selected ${stats.selected}` +
      (stats.selectedBeforeLimit !== stats.selected ? ` of ${stats.selectedBeforeLimit} before --limit` : '') +
      (stats.skippedCompleted ? `, skipped ${stats.skippedCompleted} completed name(s)` : '') +
      (stats.skippedSeen ? `, skipped ${stats.skippedSeen} unchanged seen template(s)` : ''),
  );
  if (options.importReport) {
    const { imported, count, reported } = importReportIntoSeenRegistry(
      options.seenRegistry,
      options.dir,
      templates,
      options.importReport,
    );
    console.error(
      `imported ${count} local template record(s) from ${reported} reported name(s) into ${options.seenRegistry}; registry now has ${imported.templates.length}`,
    );
    return;
  }
  if (templates.length === 0) return;
  if (options.dryRun) return;

  fs.mkdirSync(options.workDir, { recursive: true });
  const batchDirs = [];
  const batches = chunks(templates, options.batchSize);
  for (let index = 0; index < batches.length; index += 1) {
    const batchDir = path.join(options.workDir, `batch-${String(index + 1).padStart(3, '0')}`);
    batchDirs.push(batchDir);
    console.error(`batch ${index + 1}/${batches.length}: ${batches[index].length} template(s)`);
    await runNode(compareArgs(batches[index], options, batchDir));
  }

  const merged = mergeReports(batchDirs, path.join(options.workDir, 'comparison.json'));
  console.error(`merged ${merged.results.length} result(s) into ${path.join(options.workDir, 'comparison.json')}`);
  if (options.updateSeenRegistry) {
    const updated = updateSeenRegistry(options.seenRegistry, options.dir, templates, merged, options.workDir);
    console.error(`updated ${updated.templates.length} seen template record(s) in ${options.seenRegistry}`);
  }
  printTop(merged.results);
}

main().catch((error) => {
  console.error(error?.stack || error?.message || String(error));
  process.exit(1);
});
