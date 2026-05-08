#!/usr/bin/env node

import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_LIMITS = {
  maxImageBytes: '20971520',
  maxDecodedPixels: '50000000',
  maxTotalResourceBytes: '134217728',
};
const IMAGE_ARTIFACT_DIRS = ['browser', 'rust', 'diff', 'side-by-side'];
const DEBUG_ARTIFACT_DIRS = ['layout', 'logs'];

function parseArgs(argv) {
  const command = argv[0] && !argv[0].startsWith('-') ? argv[0] : 'help';
  const args = {
    command,
    dir: defaultRgeDir(),
    store: null,
    runsDir: null,
    width: 800,
    scale: 1,
    profile: 'desktop',
    timeoutMs: 30000,
    batchSize: 20,
    limit: null,
    sample: null,
    topDiff: null,
    templates: [],
    onlyNew: false,
    missingForCurrentSha: false,
    includeUnchangedVersion: false,
    artifactTop: 50,
    keepDebugTop: 10,
    keepAllArtifacts: false,
    dryRun: false,
    noRemote: true,
    fixtureFonts: true,
    continueOnError: true,
    runName: null,
    compareWorkDir: null,
    ...DEFAULT_LIMITS,
  };

  for (let i = command === 'help' ? 0 : 1; i < argv.length; i += 1) {
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
      case '--store':
        args.store = next();
        break;
      case '--runs-dir':
        args.runsDir = next();
        break;
      case '--width':
        args.width = Number.parseInt(next(), 10);
        break;
      case '--scale':
        args.scale = Number.parseFloat(next());
        break;
      case '--profile':
        args.profile = next();
        break;
      case '--timeout-ms':
        args.timeoutMs = Number.parseInt(next(), 10);
        break;
      case '--batch-size':
        args.batchSize = Number.parseInt(next(), 10);
        break;
      case '--limit':
        args.limit = Number.parseInt(next(), 10);
        break;
      case '--sample':
        args.sample = Number.parseInt(next(), 10);
        break;
      case '--top-diff':
        args.topDiff = Number.parseInt(next(), 10);
        break;
      case '--template':
        args.templates.push(next());
        break;
      case '--only-new':
        args.onlyNew = true;
        break;
      case '--missing-for-current-sha':
        args.missingForCurrentSha = true;
        break;
      case '--include-unchanged-version':
        args.includeUnchangedVersion = true;
        break;
      case '--artifact-top':
        args.artifactTop = Number.parseInt(next(), 10);
        break;
      case '--keep-debug-top':
        args.keepDebugTop = Number.parseInt(next(), 10);
        break;
      case '--keep-all-artifacts':
        args.keepAllArtifacts = true;
        break;
      case '--dry-run':
        args.dryRun = true;
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
      case '--name':
        args.runName = next();
        break;
      case '--work-dir':
        args.compareWorkDir = next();
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
        args.command = 'help';
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  args.dir = path.resolve(args.dir);
  args.store = path.resolve(args.store || path.join(args.dir, '.mail-canvas', 'corpus-store.json'));
  args.runsDir = path.resolve(args.runsDir || path.join(args.dir, '.mail-canvas', 'runs'));
  validatePositiveInteger('--width', args.width);
  validatePositiveInteger('--timeout-ms', args.timeoutMs);
  validatePositiveInteger('--batch-size', args.batchSize);
  validateOptionalPositiveInteger('--limit', args.limit);
  validateOptionalPositiveInteger('--sample', args.sample);
  validateOptionalPositiveInteger('--top-diff', args.topDiff);
  validatePositiveInteger('--artifact-top', args.artifactTop);
  validatePositiveInteger('--keep-debug-top', args.keepDebugTop);
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/rge_corpus_store.mjs <command> [options]

Commands:
  index                    Scan local RGE HTML files and update the local store.
  run                      Create a repeatable comparison run and store results.
  report                   Print/write reports from the latest or selected run.
  list                     Show indexed template/run counts.

Options:
  --dir <path>             Local RGE folder (default: ./rge, fallback: corpus/reallygoodemails)
  --store <path>           Store JSON path (default: <dir>/.mail-canvas/corpus-store.json)
  --runs-dir <path>        Run output directory (default: <dir>/.mail-canvas/runs)
  --width <px>             Render width (default: 800)
  --profile <name>         Logical profile label stored with the run (default: desktop)
  --timeout-ms <ms>        Per-template timeout (default: 30000)
  --batch-size <n>         Templates per Playwright batch (default: 20)
  --limit <n>              Limit selected templates
  --sample <n>             Deterministic sample from the selected set
  --template <name>        Run one template; repeatable
  --only-new               Select templates with no stored result history
  --missing-for-current-sha Select templates missing for current git sha/config/version
  --top-diff <n>           Select templates with the highest latest diff
  --artifact-top <n>       Keep image artifacts for top diff/regression templates (default: 50)
  --keep-debug-top <n>     Keep layout/log artifacts for top diff/regression templates (default: 10)
  --keep-all-artifacts     Do not prune per-template artifacts
  --dry-run                Print what would run without rendering
  --allow-remote           Allow remote resources during compare
  --no-fixture-fonts       Do not force fixture fonts
  --name <label>           Human-readable run name

Examples:
  npm run corpus:index -- --dir rge
  npm run corpus:run -- --dir rge --missing-for-current-sha --limit 500
  npm run corpus:run -- --dir rge --top-diff 200 --artifact-top 40
  npm run corpus:report -- --dir rge
`);
}

function defaultRgeDir() {
  const rootRge = path.join(ROOT_DIR, 'rge');
  if (fs.existsSync(rootRge)) return rootRge;
  return path.join(ROOT_DIR, 'corpus', 'reallygoodemails');
}

function validatePositiveInteger(name, value) {
  if (!Number.isFinite(value) || value < 1) throw new Error(`${name} must be a positive integer`);
}

function validateOptionalPositiveInteger(name, value) {
  if (value !== null && (!Number.isFinite(value) || value < 1)) {
    throw new Error(`${name} must be a positive integer`);
  }
}

function emptyStore(sourceDir) {
  return {
    schemaVersion: 1,
    sourceDir,
    createdAt: new Date().toISOString(),
    updatedAt: null,
    templates: {},
    versions: {},
    runs: {},
    results: {},
    issues: {},
  };
}

function readStore(args) {
  if (!fs.existsSync(args.store)) return emptyStore(args.dir);
  const parsed = JSON.parse(fs.readFileSync(args.store, 'utf8'));
  return {
    ...emptyStore(args.dir),
    ...parsed,
    sourceDir: parsed.sourceDir || args.dir,
    templates: parsed.templates || {},
    versions: parsed.versions || {},
    runs: parsed.runs || {},
    results: parsed.results || {},
    issues: parsed.issues || {},
  };
}

function writeStore(args, store) {
  store.updatedAt = new Date().toISOString();
  fs.mkdirSync(path.dirname(args.store), { recursive: true });
  fs.writeFileSync(args.store, `${JSON.stringify(store, null, 2)}\n`);
}

function scanLocalTemplates(dir) {
  if (!fs.existsSync(dir)) throw new Error(`RGE directory does not exist: ${dir}`);
  return fs
    .readdirSync(dir)
    .filter((file) => file.endsWith('.html'))
    .sort()
    .map((file) => {
      const htmlPath = path.join(dir, file);
      const name = path.basename(file, '.html');
      const assetDir = htmlPath.replace(/\.html$/i, '.assets');
      const fingerprint = fingerprintTemplate(htmlPath, assetDir);
      return { name, htmlPath, assetDir, ...fingerprint };
    });
}

function fingerprintTemplate(htmlPath, assetDir) {
  const html = fs.readFileSync(htmlPath);
  const assets = fingerprintAssets(assetDir);
  const assetManifest = assets.map((asset) => `${asset.path}\0${asset.bytes}\0${asset.sha256}`).join('\n');
  const htmlHash = sha256(html);
  const assetHash = sha256(assetManifest);
  return {
    htmlHash,
    htmlBytes: html.length,
    assetHash,
    assetCount: assets.length,
    assetBytes: assets.reduce((sum, asset) => sum + asset.bytes, 0),
    contentHash: sha256(`${htmlHash}\0${assetHash}`),
  };
}

function fingerprintAssets(assetDir) {
  if (!fs.existsSync(assetDir)) return [];
  return walkFiles(assetDir)
    .map((filePath) => {
      const bytes = fs.readFileSync(filePath);
      return {
        path: path.relative(assetDir, filePath).replaceAll(path.sep, '/'),
        bytes: bytes.length,
        sha256: sha256(bytes),
      };
    })
    .sort((left, right) => left.path.localeCompare(right.path));
}

function walkFiles(dir) {
  const files = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === '.mail-canvas') continue;
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...walkFiles(entryPath));
    else if (entry.isFile()) files.push(entryPath);
  }
  return files;
}

function sha256(value) {
  return createHash('sha256').update(value).digest('hex');
}

function indexTemplates(args, store) {
  const scanned = scanLocalTemplates(args.dir);
  let added = 0;
  let changed = 0;
  let unchanged = 0;
  const seenNames = new Set();
  const now = new Date().toISOString();

  for (const template of scanned) {
    seenNames.add(template.name);
    const existing = store.templates[template.name];
    const versionId = versionIdFor(template);
    if (!store.versions[versionId]) {
      store.versions[versionId] = {
        id: versionId,
        templateName: template.name,
        htmlPath: relativeToRoot(template.htmlPath),
        assetDir: fs.existsSync(template.assetDir) ? relativeToRoot(template.assetDir) : null,
        htmlHash: template.htmlHash,
        assetHash: template.assetHash,
        contentHash: template.contentHash,
        htmlBytes: template.htmlBytes,
        assetCount: template.assetCount,
        assetBytes: template.assetBytes,
        firstSeenAt: now,
      };
    }
    if (!existing) {
      added += 1;
      store.templates[template.name] = {
        name: template.name,
        htmlPath: relativeToRoot(template.htmlPath),
        assetDir: fs.existsSync(template.assetDir) ? relativeToRoot(template.assetDir) : null,
        currentVersionId: versionId,
        currentContentHash: template.contentHash,
        firstSeenAt: now,
        lastSeenAt: now,
        versions: [versionId],
        status: 'active',
      };
      continue;
    }
    existing.htmlPath = relativeToRoot(template.htmlPath);
    existing.assetDir = fs.existsSync(template.assetDir) ? relativeToRoot(template.assetDir) : null;
    existing.lastSeenAt = now;
    existing.status = 'active';
    if (existing.currentContentHash === template.contentHash) {
      unchanged += 1;
    } else {
      changed += 1;
      existing.currentVersionId = versionId;
      existing.currentContentHash = template.contentHash;
    }
    existing.versions = [...new Set([...(existing.versions || []), versionId])];
  }

  let missing = 0;
  for (const template of Object.values(store.templates)) {
    if (!seenNames.has(template.name) && template.status !== 'missing') {
      missing += 1;
      template.status = 'missing';
      template.missingAt = now;
    }
  }
  store.sourceDir = args.dir;
  return { scanned: scanned.length, added, changed, unchanged, missing };
}

function versionIdFor(template) {
  return `${template.name}:${template.contentHash.slice(0, 16)}`;
}

function relativeToRoot(filePath) {
  return path.relative(ROOT_DIR, filePath).replaceAll(path.sep, '/');
}

function absoluteFromRoot(filePath) {
  return path.resolve(ROOT_DIR, filePath);
}

function currentGitSha() {
  const result = spawnSync('git', ['rev-parse', '--short=12', 'HEAD'], {
    cwd: ROOT_DIR,
    encoding: 'utf8',
  });
  return result.status === 0 ? result.stdout.trim() : 'unknown';
}

function runConfig(args, gitSha = currentGitSha()) {
  return {
    rendererGitSha: gitSha,
    width: args.width,
    scale: args.scale,
    profile: args.profile,
    allowRemote: !args.noRemote,
    fixtureFonts: args.fixtureFonts,
  };
}

function selectTemplates(args, store, config) {
  const active = Object.values(store.templates)
    .filter((template) => template.status === 'active')
    .sort((left, right) => left.name.localeCompare(right.name));
  let selected = active;
  const notes = [];

  if (args.templates.length > 0) {
    const wanted = new Set(args.templates);
    selected = active.filter((template) => wanted.has(template.name));
    const found = new Set(selected.map((template) => template.name));
    const missing = args.templates.filter((name) => !found.has(name));
    if (missing.length > 0) throw new Error(`unknown template(s): ${missing.join(', ')}`);
    notes.push(`template=${args.templates.join(',')}`);
  }

  if (args.onlyNew) {
    selected = selected.filter((template) => latestResultsForTemplate(store, template.name).length === 0);
    notes.push('only-new');
  }

  if (args.missingForCurrentSha) {
    selected = selected.filter((template) => {
      const versionId = template.currentVersionId;
      return !Object.values(store.results).some(
        (result) =>
          result.templateName === template.name &&
          result.templateVersionId === versionId &&
          sameConfig(result.config, config),
      );
    });
    notes.push('missing-for-current-sha');
  }

  if (args.topDiff !== null) {
    selected = selected
      .map((template) => ({ template, latest: latestComparableResult(store, template.name) }))
      .filter((entry) => entry.latest)
      .sort((left, right) => (right.latest.diffRatio || 0) - (left.latest.diffRatio || 0))
      .slice(0, args.topDiff)
      .map((entry) => entry.template);
    notes.push(`top-diff=${args.topDiff}`);
  }

  if (!args.includeUnchangedVersion && !args.onlyNew && !args.missingForCurrentSha && args.topDiff === null) {
    notes.push('all-active');
  }

  if (args.sample !== null && selected.length > args.sample) {
    selected = deterministicSample(selected, args.sample, `${config.rendererGitSha}:${config.width}:${config.profile}`);
    notes.push(`sample=${args.sample}`);
  }

  if (args.limit !== null && selected.length > args.limit) {
    selected = selected.slice(0, args.limit);
    notes.push(`limit=${args.limit}`);
  }

  return { selected, notes };
}

function sameConfig(left, right) {
  return (
    left?.rendererGitSha === right?.rendererGitSha &&
    Number(left?.width) === Number(right?.width) &&
    String(left?.profile || 'desktop') === String(right?.profile || 'desktop') &&
    Boolean(left?.allowRemote) === Boolean(right?.allowRemote) &&
    Boolean(left?.fixtureFonts) === Boolean(right?.fixtureFonts)
  );
}

function deterministicSample(items, count, seed) {
  return [...items]
    .map((item) => ({ item, key: sha256(`${seed}:${item.name}`) }))
    .sort((left, right) => left.key.localeCompare(right.key))
    .slice(0, count)
    .map((entry) => entry.item)
    .sort((left, right) => left.name.localeCompare(right.name));
}

function latestResultsForTemplate(store, templateName) {
  return Object.values(store.results)
    .filter((result) => result.templateName === templateName)
    .sort((left, right) => String(right.createdAt).localeCompare(String(left.createdAt)));
}

function latestComparableResult(store, templateName) {
  return latestResultsForTemplate(store, templateName).find((result) => result.status === 'ok');
}

function chunks(items, size) {
  const out = [];
  for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
  return out;
}

function runNode(args) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, args, { cwd: ROOT_DIR, stdio: 'inherit' });
    child.on('error', reject);
    child.on('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`command failed with exit code ${code}: node ${args.join(' ')}`));
    });
  });
}

function compareArgs(batch, args, batchDir) {
  const cliArgs = ['scripts/playwright_compare.mjs'];
  for (const template of batch) {
    cliArgs.push('--html', absoluteFromRoot(template.htmlPath), '--name', template.name);
  }
  cliArgs.push(
    '--width',
    String(args.width),
    '--work-dir',
    batchDir,
    '--timeout-ms',
    String(args.timeoutMs),
    '--max-image-bytes',
    String(args.maxImageBytes),
    '--max-decoded-pixels',
    String(args.maxDecodedPixels),
    '--max-total-resource-bytes',
    String(args.maxTotalResourceBytes),
  );
  if (args.noRemote) cliArgs.push('--no-remote');
  if (args.fixtureFonts) cliArgs.push('--fixture-fonts');
  if (args.continueOnError) cliArgs.push('--continue-on-error');
  return cliArgs;
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
  results.sort((left, right) => left.name.localeCompare(right.name));
  const merged = { generatedAt: new Date().toISOString(), results, failures };
  fs.writeFileSync(outPath, `${JSON.stringify(merged, null, 2)}\n`);
  return merged;
}

async function runComparison(args, store) {
  const indexStats = indexTemplates(args, store);
  const gitSha = currentGitSha();
  const config = runConfig(args, gitSha);
  const { selected, notes } = selectTemplates(args, store, config);
  const runId = runIdFor(gitSha);
  const runDir = path.resolve(args.compareWorkDir || path.join(args.runsDir, runId));
  const startedAt = new Date().toISOString();
  const run = {
    id: runId,
    name: args.runName || runId,
    sourceDir: args.dir,
    startedAt,
    finishedAt: null,
    selection: notes,
    selectedCount: selected.length,
    indexStats,
    config,
    runDir: relativeToRoot(runDir),
    status: args.dryRun ? 'dry-run' : 'running',
  };

  console.error(
    `indexed ${indexStats.scanned} template(s), selected ${selected.length}` +
      (notes.length ? ` (${notes.join(', ')})` : ''),
  );
  if (args.dryRun) {
    for (const template of selected.slice(0, 25)) {
      console.log(template.name);
    }
    if (selected.length > 25) console.log(`... ${selected.length - 25} more`);
    return { run, merged: { results: [], failures: [] } };
  }

  store.runs[runId] = run;
  if (selected.length === 0 || args.dryRun) {
    run.finishedAt = new Date().toISOString();
    run.status = 'empty';
    writeStore(args, store);
    return { run, merged: { results: [], failures: [] } };
  }

  fs.mkdirSync(runDir, { recursive: true });
  const batchDirs = [];
  const batches = chunks(selected, args.batchSize);
  for (let index = 0; index < batches.length; index += 1) {
    const batchDir = path.join(runDir, 'work', `batch-${String(index + 1).padStart(4, '0')}`);
    batchDirs.push(batchDir);
    console.error(`batch ${index + 1}/${batches.length}: ${batches[index].length} template(s)`);
    await runNode(compareArgs(batches[index], args, batchDir));
  }

  const merged = mergeReports(batchDirs, path.join(runDir, 'comparison.json'));
  ingestRunResults(args, store, run, selected, merged);
  const keep = copyAndPruneArtifacts(args, runDir, batchDirs, merged, store, runId);
  run.artifacts = keep;
  run.finishedAt = new Date().toISOString();
  run.status = merged.failures?.length ? 'completed-with-failures' : 'completed';
  run.resultCount = merged.results.length;
  run.failureCount = merged.failures?.length || 0;
  writeRunReports(runDir, run, merged, store);
  writeStore(args, store);
  printRunSummary(merged, store, runId);
  console.error(`run: ${runId}`);
  console.error(`outputs: ${runDir}`);
  return { run, merged };
}

function runIdFor(gitSha) {
  const stamp = new Date().toISOString().replaceAll(/[-:.]/g, '').replace('T', '-').slice(0, 15);
  return `${stamp}-${gitSha}`;
}

function ingestRunResults(args, store, run, selected, merged) {
  const byTemplate = new Map(selected.map((template) => [template.name, template]));
  const failuresByName = new Map((merged.failures || []).map((failure) => [failure.name, failure]));
  const resultNames = new Set();

  for (const result of merged.results || []) {
    resultNames.add(result.name);
    const template = byTemplate.get(result.name) || store.templates[result.name];
    const previous = latestComparableResult(store, result.name);
    const record = resultRecord(args, run, template, result, previous);
    store.results[record.id] = record;
    for (const issue of classifyIssues(record, result)) {
      store.issues[issue.id] = issue;
    }
  }

  for (const template of selected) {
    if (resultNames.has(template.name)) continue;
    const failure = failuresByName.get(template.name);
    const record = {
      id: `${run.id}:${template.name}`,
      runId: run.id,
      templateName: template.name,
      templateVersionId: template.currentVersionId,
      status: 'failed',
      config: run.config,
      createdAt: run.startedAt,
      error: failure?.error || 'missing comparison result',
    };
    store.results[record.id] = record;
    store.issues[`${record.id}:render-failed`] = {
      id: `${record.id}:render-failed`,
      runId: run.id,
      templateName: template.name,
      issueKey: 'render-failed',
      severity: 'high',
      evidence: { error: record.error },
      createdAt: run.startedAt,
    };
  }
}

function resultRecord(args, run, template, result, previous) {
  const heightDelta = (result.rust?.height || 0) - (result.browser?.height || 0);
  const diffDelta = previous?.diffRatio == null ? null : (result.diffRatio || 0) - previous.diffRatio;
  return {
    id: `${run.id}:${result.name}`,
    runId: run.id,
    templateName: result.name,
    templateVersionId: template?.currentVersionId || null,
    status: result.status || 'ok',
    config: run.config,
    createdAt: run.startedAt,
    diffRatio: result.diffRatio ?? null,
    mediaDiffRatio: result.media?.diffRatio ?? null,
    mediaRectDeltaRatio: result.mediaRects?.deltaRatio ?? null,
    textDiffRatio: result.text?.diffRatio ?? null,
    textRectDeltaRatio: result.textRects?.positionDeltaRatio ?? null,
    textCoverageDeltaRatio: result.textCoverage?.coverageDeltaRatio ?? null,
    nonMediaNonTextDiffRatio: result.nonMediaNonText?.diffRatio ?? null,
    heightDelta,
    browserHeight: result.browser?.height ?? null,
    rustHeight: result.rust?.height ?? null,
    firstBadRegion: result.firstBadRegion || null,
    warningCount: result.warningCount || 0,
    assetSummary: result.assetSummary || null,
    previousResultId: previous?.id || null,
    diffDelta,
    paths: {
      browserPng: result.browserPng || null,
      rustPng: result.rustPng || null,
      diffPng: result.diffPng || null,
      sideBySidePng: result.sideBySidePng || null,
      diagnosticsJson: result.diagnosticsJson || null,
      layoutJson: result.layoutJson || null,
    },
  };
}

function classifyIssues(record, result) {
  const issues = [];
  const push = (issueKey, severity, evidence) => {
    issues.push({
      id: `${record.id}:${issueKey}`,
      runId: record.runId,
      templateName: record.templateName,
      issueKey,
      severity,
      evidence,
      createdAt: record.createdAt,
    });
  };
  if (record.status !== 'ok') {
    push('render-failed', 'high', { status: record.status });
  }
  if ((result.assetSummary?.failed || 0) > 0) {
    push('asset-load-failed', 'high', result.assetSummary);
  }
  if (Math.abs(record.heightDelta || 0) >= 120) {
    push('height-delta', 'medium', { heightDelta: record.heightDelta });
  }
  if ((record.mediaRectDeltaRatio || 0) >= 0.15) {
    push('image-position-or-size-delta', 'high', {
      mediaRectDeltaRatio: record.mediaRectDeltaRatio,
      mediaDiffRatio: record.mediaDiffRatio,
    });
  } else if ((record.mediaDiffRatio || 0) >= 0.25) {
    push('image-raster-or-color-delta', 'medium', {
      mediaDiffRatio: record.mediaDiffRatio,
      mediaRectDeltaRatio: record.mediaRectDeltaRatio,
    });
  }
  if ((record.nonMediaNonTextDiffRatio || 0) >= 0.05) {
    push('box-or-background-delta', 'medium', {
      nonMediaNonTextDiffRatio: record.nonMediaNonTextDiffRatio,
    });
  }
  if ((record.textRectDeltaRatio || 0) >= 0.5) {
    push('text-wrap-or-position-delta', 'medium', {
      textRectDeltaRatio: record.textRectDeltaRatio,
      textDiffRatio: record.textDiffRatio,
    });
  } else if ((record.textCoverageDeltaRatio || 0) >= 0.12) {
    push('text-coverage-delta', 'low', {
      textCoverageDeltaRatio: record.textCoverageDeltaRatio,
      textDiffRatio: record.textDiffRatio,
    });
  }
  if ((record.diffDelta || 0) >= 0.02) {
    push('regression-vs-previous', 'high', {
      diffDelta: record.diffDelta,
      previousResultId: record.previousResultId,
    });
  }
  return issues;
}

function copyAndPruneArtifacts(args, runDir, batchDirs, merged, store, runId) {
  const records = Object.values(store.results).filter((result) => result.runId === runId);
  const topDiffNames = new Set(
    records
      .filter((result) => result.status === 'ok')
      .sort((left, right) => (right.diffRatio || 0) - (left.diffRatio || 0))
      .slice(0, args.artifactTop)
      .map((result) => result.templateName),
  );
  const regressionNames = new Set(
    records
      .filter((result) => (result.diffDelta || 0) >= 0.02)
      .sort((left, right) => (right.diffDelta || 0) - (left.diffDelta || 0))
      .slice(0, args.artifactTop)
      .map((result) => result.templateName),
  );
  const failureNames = new Set((merged.failures || []).map((failure) => failure.name));
  const keepImageNames = new Set([...topDiffNames, ...regressionNames, ...failureNames]);
  const keepDebugNames = new Set(
    records
      .filter((result) => keepImageNames.has(result.templateName))
      .sort((left, right) => (right.diffRatio || 0) - (left.diffRatio || 0))
      .slice(0, args.keepDebugTop)
      .map((result) => result.templateName),
  );

  if (!args.keepAllArtifacts) {
    for (const batchDir of batchDirs) {
      pruneArtifactsInBatch(batchDir, keepImageNames, keepDebugNames);
    }
  }

  return {
    policy: args.keepAllArtifacts ? 'keep-all' : 'top-and-failures',
    topDiff: [...topDiffNames],
    regressions: [...regressionNames],
    failures: [...failureNames],
    imageArtifactCount: keepImageNames.size,
    debugArtifactCount: keepDebugNames.size,
  };
}

function pruneArtifactsInBatch(batchDir, keepImageNames, keepDebugNames) {
  for (const dirName of IMAGE_ARTIFACT_DIRS) {
    pruneNamedFiles(path.join(batchDir, dirName), keepImageNames);
  }
  for (const dirName of DEBUG_ARTIFACT_DIRS) {
    pruneNamedFiles(path.join(batchDir, dirName), keepDebugNames);
  }
  for (const dirName of ['html', 'prepared']) {
    fs.rmSync(path.join(batchDir, dirName), { recursive: true, force: true });
  }
}

function pruneNamedFiles(dir, keepNames) {
  if (!fs.existsSync(dir)) return;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (!entry.isFile()) continue;
    const fullPath = path.join(dir, entry.name);
    const keep = [...keepNames].some((name) => entry.name === `${name}${path.extname(entry.name)}` || entry.name.startsWith(`${name}.`));
    if (!keep) fs.rmSync(fullPath, { force: true });
  }
}

function writeRunReports(runDir, run, merged, store) {
  const records = Object.values(store.results).filter((result) => result.runId === run.id);
  const issues = Object.values(store.issues).filter((issue) => issue.runId === run.id);
  const summary = buildSummary(run, records, issues, merged.failures || []);
  fs.writeFileSync(path.join(runDir, 'summary.json'), `${JSON.stringify(summary, null, 2)}\n`);
  fs.writeFileSync(path.join(runDir, 'summary.md'), renderSummaryMarkdown(summary));
  fs.writeFileSync(path.join(runDir, 'issues.json'), `${JSON.stringify(issues, null, 2)}\n`);
  fs.writeFileSync(path.join(runDir, 'results.csv'), renderResultsCsv(records));
}

function buildSummary(run, records, issues, failures) {
  const ok = records.filter((record) => record.status === 'ok');
  const issueCounts = new Map();
  for (const issue of issues) {
    issueCounts.set(issue.issueKey, (issueCounts.get(issue.issueKey) || 0) + 1);
  }
  return {
    run,
    totals: {
      selected: run.selectedCount,
      results: records.length,
      ok: ok.length,
      failures: failures.length + records.filter((record) => record.status !== 'ok').length,
    },
    metrics: summarizeMetrics(ok),
    topDiffs: topRecords(ok, 'diffRatio', 25),
    topRegressions: ok
      .filter((record) => (record.diffDelta || 0) > 0)
      .sort((left, right) => (right.diffDelta || 0) - (left.diffDelta || 0))
      .slice(0, 25),
    issueCounts: [...issueCounts.entries()]
      .map(([issueKey, count]) => ({ issueKey, count }))
      .sort((left, right) => right.count - left.count || left.issueKey.localeCompare(right.issueKey)),
  };
}

function summarizeMetrics(records) {
  return {
    diff: percentiles(records.map((record) => record.diffRatio).filter(isNumber)),
    media: percentiles(records.map((record) => record.mediaDiffRatio).filter(isNumber)),
    text: percentiles(records.map((record) => record.textDiffRatio).filter(isNumber)),
    nonMediaNonText: percentiles(records.map((record) => record.nonMediaNonTextDiffRatio).filter(isNumber)),
    absoluteHeightDelta: percentiles(records.map((record) => Math.abs(record.heightDelta || 0)).filter(isNumber), false),
  };
}

function isNumber(value) {
  return Number.isFinite(value);
}

function percentiles(values, ratio = true) {
  if (values.length === 0) return { count: 0, p50: null, p90: null, p99: null, max: null, mean: null };
  const sorted = [...values].sort((left, right) => left - right);
  const at = (p) => sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * p))];
  const mean = sorted.reduce((sum, value) => sum + value, 0) / sorted.length;
  const convert = (value) => (ratio ? Number((value * 100).toFixed(2)) : Number(value.toFixed(2)));
  return {
    count: sorted.length,
    p50: convert(at(0.5)),
    p90: convert(at(0.9)),
    p99: convert(at(0.99)),
    max: convert(sorted[sorted.length - 1]),
    mean: convert(mean),
  };
}

function topRecords(records, key, count) {
  return records
    .filter((record) => Number.isFinite(record[key]))
    .sort((left, right) => (right[key] || 0) - (left[key] || 0))
    .slice(0, count);
}

function renderSummaryMarkdown(summary) {
  const lines = [];
  lines.push(`# RGE Corpus Run ${summary.run.id}`);
  lines.push('');
  lines.push(`- status: ${summary.run.status}`);
  lines.push(`- renderer: ${summary.run.config.rendererGitSha}`);
  lines.push(`- width/profile: ${summary.run.config.width} / ${summary.run.config.profile}`);
  lines.push(`- selected/results/ok/failures: ${summary.totals.selected}/${summary.totals.results}/${summary.totals.ok}/${summary.totals.failures}`);
  lines.push('');
  lines.push('## Metrics');
  lines.push('');
  lines.push('| metric | count | p50 | p90 | p99 | max | mean |');
  lines.push('| --- | ---: | ---: | ---: | ---: | ---: | ---: |');
  for (const [name, value] of Object.entries(summary.metrics)) {
    lines.push(`| ${name} | ${value.count} | ${empty(value.p50)} | ${empty(value.p90)} | ${empty(value.p99)} | ${empty(value.max)} | ${empty(value.mean)} |`);
  }
  lines.push('');
  lines.push('## Issue Counts');
  lines.push('');
  for (const issue of summary.issueCounts) {
    lines.push(`- ${issue.issueKey}: ${issue.count}`);
  }
  lines.push('');
  lines.push('## Top Diffs');
  lines.push('');
  lines.push('| diff | media | text | nonText | hΔ | template |');
  lines.push('| ---: | ---: | ---: | ---: | ---: | --- |');
  for (const record of summary.topDiffs.slice(0, 25)) {
    lines.push(
      `| ${pct(record.diffRatio)} | ${pct(record.mediaDiffRatio)} | ${pct(record.textDiffRatio)} | ${pct(record.nonMediaNonTextDiffRatio)} | ${record.heightDelta ?? ''} | ${record.templateName} |`,
    );
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

function empty(value) {
  return value === null || value === undefined ? '' : value;
}

function pct(value) {
  return Number.isFinite(value) ? `${(value * 100).toFixed(2)}%` : '';
}

function renderResultsCsv(records) {
  const header = [
    'template',
    'status',
    'diff_pct',
    'media_diff_pct',
    'text_diff_pct',
    'non_media_non_text_pct',
    'height_delta',
    'first_bad_y0',
    'first_bad_y1',
    'diff_delta_pct',
  ];
  const rows = records
    .sort((left, right) => left.templateName.localeCompare(right.templateName))
    .map((record) =>
      [
        record.templateName,
        record.status,
        csvNumber(record.diffRatio, true),
        csvNumber(record.mediaDiffRatio, true),
        csvNumber(record.textDiffRatio, true),
        csvNumber(record.nonMediaNonTextDiffRatio, true),
        record.heightDelta ?? '',
        record.firstBadRegion?.y0 ?? '',
        record.firstBadRegion?.y1 ?? '',
        csvNumber(record.diffDelta, true),
      ]
        .map(csvEscape)
        .join(','),
    );
  return `${header.join(',')}\n${rows.join('\n')}\n`;
}

function csvNumber(value, ratio = false) {
  if (!Number.isFinite(value)) return '';
  return ratio ? (value * 100).toFixed(4) : value.toFixed(4);
}

function csvEscape(value) {
  const text = String(value);
  return /[",\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

function printRunSummary(merged, store, runId) {
  const records = Object.values(store.results).filter((result) => result.runId === runId && result.status === 'ok');
  for (const record of topRecords(records, 'diffRatio', 15)) {
    console.log(
      `${pct(record.diffRatio).padStart(7)} media ${pct(record.mediaDiffRatio).padStart(7)} ` +
        `nonText ${pct(record.nonMediaNonTextDiffRatio).padStart(7)} h ${String(record.heightDelta).padStart(5)} ${record.templateName}`,
    );
  }
  if ((merged.failures || []).length > 0) {
    console.log(`failures: ${merged.failures.length}`);
  }
}

function latestRun(store) {
  return (
    Object.values(store.runs)
      .filter((run) => run.status !== 'dry-run')
      .sort((left, right) => String(right.startedAt).localeCompare(String(left.startedAt)))[0] || null
  );
}

function report(args, store) {
  const run = latestRun(store);
  if (!run) {
    console.log('no runs recorded');
    return;
  }
  const records = Object.values(store.results).filter((result) => result.runId === run.id);
  const issues = Object.values(store.issues).filter((issue) => issue.runId === run.id);
  const summary = buildSummary(run, records, issues, []);
  console.log(renderSummaryMarkdown(summary));
}

function listStore(store) {
  const templates = Object.values(store.templates);
  const active = templates.filter((template) => template.status === 'active').length;
  const versions = Object.keys(store.versions).length;
  const runs = Object.values(store.runs).sort((left, right) => String(right.startedAt).localeCompare(String(left.startedAt)));
  const results = Object.keys(store.results).length;
  console.log(`templates: ${templates.length} (${active} active)`);
  console.log(`versions: ${versions}`);
  console.log(`runs: ${runs.length}`);
  console.log(`results: ${results}`);
  for (const run of runs.slice(0, 10)) {
    console.log(`- ${run.id} ${run.status} selected=${run.selectedCount} results=${run.resultCount || 0} ${run.name || ''}`);
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.command === 'help') {
    printHelp();
    return;
  }
  const store = readStore(args);
  switch (args.command) {
    case 'index': {
      const stats = indexTemplates(args, store);
      writeStore(args, store);
      console.log(
        `indexed ${stats.scanned} template(s): added ${stats.added}, changed ${stats.changed}, unchanged ${stats.unchanged}, missing ${stats.missing}`,
      );
      console.log(`store: ${args.store}`);
      break;
    }
    case 'run':
      await runComparison(args, store);
      break;
    case 'report':
      report(args, store);
      break;
    case 'list':
      listStore(store);
      break;
    default:
      throw new Error(`unknown command: ${args.command}`);
  }
}

main().catch((error) => {
  console.error(error?.stack || error?.message || String(error));
  process.exit(1);
});
