#!/usr/bin/env node
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

function scanTemplates(dir, completedNames, limit) {
  if (!fs.existsSync(dir)) {
    throw new Error(`template directory does not exist: ${dir}`);
  }
  let files = fs
    .readdirSync(dir)
    .filter((file) => file.endsWith('.html'))
    .sort()
    .map((file) => ({
      file,
      name: path.basename(file, '.html'),
      htmlPath: path.join(dir, file),
    }))
    .filter((entry) => !completedNames.has(entry.name));
  if (limit !== null) files = files.slice(0, limit);
  return files;
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
  const completedNames = readCompletedNames(options.onlyMissingFrom);
  const templates = scanTemplates(options.dir, completedNames, options.limit);
  console.error(
    `scanned ${templates.length} template(s) from ${options.dir}` +
      (completedNames.size ? `, skipped ${completedNames.size} completed name(s)` : ''),
  );
  if (templates.length === 0) return;

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
  printTop(merged.results);
}

main().catch((error) => {
  console.error(error?.stack || error?.message || String(error));
  process.exit(1);
});
