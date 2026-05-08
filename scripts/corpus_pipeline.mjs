#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { PNG } from 'pngjs';

import { TEMPLATE_CORPUS } from './templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_WIDTH = 800;
const DEFAULT_TIMEOUT_MS = 30000;
const DEFAULT_MAX_IMAGE_BYTES = 20 * 1024 * 1024;
const DEFAULT_MAX_DECODED_PIXELS = 50_000_000;
const DEFAULT_MAX_TOTAL_RESOURCE_BYTES = 128 * 1024 * 1024;
const DEFAULT_ISSUES_LOG = path.join(ROOT_DIR, 'corpus', 'issues.json');

function parseArgs(argv) {
  const args = {
    provider: 'reallygoodemails',
    category: 'saas',
    collection: null,
    limit: 10,
    width: DEFAULT_WIDTH,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    workDir: null,
    browserCacheDir: path.join(ROOT_DIR, '.cache', 'mail-canvas', 'browser-screenshots'),
    only: [],
    random: false,
    excludeExisting: true,
    excludeSeen: true,
    login: false,
    headful: false,
    skipVendor: false,
    skipAudit: false,
    skipCompare: false,
    noRemote: true,
    fixtureFonts: true,
    maxImageBytes: DEFAULT_MAX_IMAGE_BYTES,
    maxDecodedPixels: DEFAULT_MAX_DECODED_PIXELS,
    maxTotalResourceBytes: DEFAULT_MAX_TOTAL_RESOURCE_BYTES,
    registryPath: path.join(ROOT_DIR, 'corpus', 'registry.json'),
    updateRegistry: true,
    keepVendored: false,
    issuesLogPath: DEFAULT_ISSUES_LOG,
    updateIssues: true,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`missing value for ${arg}`);
      return argv[index];
    };
    switch (arg) {
      case '--provider':
        args.provider = next();
        break;
      case '--category':
        args.category = next();
        break;
      case '--collection':
        args.collection = next();
        break;
      case '--limit':
        args.limit = positiveInt(next(), '--limit');
        break;
      case '--width':
        args.width = positiveInt(next(), '--width');
        break;
      case '--timeout-ms':
        args.timeoutMs = positiveInt(next(), '--timeout-ms');
        break;
      case '--work-dir':
        args.workDir = path.resolve(next());
        break;
      case '--browser-cache-dir':
        args.browserCacheDir = path.resolve(next());
        break;
      case '--only':
        args.only.push(next());
        break;
      case '--random':
        args.random = true;
        break;
      case '--exclude-existing':
        args.excludeExisting = true;
        break;
      case '--include-existing':
        args.excludeExisting = false;
        break;
      case '--exclude-seen':
        args.excludeSeen = true;
        break;
      case '--include-seen':
        args.excludeSeen = false;
        break;
      case '--login':
        args.login = true;
        break;
      case '--headful':
        args.headful = true;
        break;
      case '--skip-vendor':
        args.skipVendor = true;
        break;
      case '--skip-audit':
        args.skipAudit = true;
        break;
      case '--skip-compare':
        args.skipCompare = true;
        break;
      case '--allow-remote':
        args.noRemote = false;
        break;
      case '--no-fixture-fonts':
        args.fixtureFonts = false;
        break;
      case '--max-image-bytes':
        args.maxImageBytes = positiveInt(next(), '--max-image-bytes');
        break;
      case '--max-decoded-pixels':
        args.maxDecodedPixels = positiveInt(next(), '--max-decoded-pixels');
        break;
      case '--max-total-resource-bytes':
        args.maxTotalResourceBytes = positiveInt(next(), '--max-total-resource-bytes');
        break;
      case '--registry':
        args.registryPath = path.resolve(next());
        break;
      case '--no-registry':
        args.updateRegistry = false;
        break;
      case '--keep-vendored':
        args.keepVendored = true;
        break;
      case '--issues-log':
        args.issuesLogPath = path.resolve(next());
        break;
      case '--no-issues-log':
        args.updateIssues = false;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  args.workDir ??= path.join(
    ROOT_DIR,
    'runs',
    `corpus-${new Date().toISOString().replaceAll(/[:.]/g, '-')}`,
  );
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  await mkdir(args.workDir, { recursive: true });

  const startedAt = Date.now();
  const steps = [];
  const beforeCatalog = await readCatalog();
  let vendoredNames = [];
  let collectAttempted = false;

  if (!args.skipVendor && args.provider === 'reallygoodemails' && args.only.length === 0) {
    collectAttempted = true;
    const vendorArgs = [
      'scripts/vendor_reallygoodemails.mjs',
      '--limit',
      String(args.limit),
      '--timeout-ms',
      String(args.timeoutMs),
    ];
    if (args.collection) {
      vendorArgs.push('--collection', args.collection);
    } else {
      vendorArgs.push('--category', args.category);
    }
    if (args.random) vendorArgs.push('--random');
    if (args.excludeExisting) vendorArgs.push('--exclude-existing');
    if (args.excludeSeen) vendorArgs.push('--exclude-seen', '--registry', args.registryPath);
    if (args.login) vendorArgs.push('--login');
    if (args.headful) vendorArgs.push('--headful');

    const collect = await runStep(steps, 'collect', 'node', vendorArgs, {
      captureStdout: true,
      teeStdout: true,
    });
    const afterCatalog = await readCatalog();
    const beforeNames = new Set(beforeCatalog.map((entry) => entry.name));
    vendoredNames = parseVendoredNames(collect.stdout);
    if (vendoredNames.length === 0) {
      vendoredNames = afterCatalog
        .map((entry) => entry.name)
        .filter((name) => !beforeNames.has(name) && name.startsWith(`${args.provider}-`));
    }
  } else if (!args.skipVendor && args.provider !== 'reallygoodemails') {
    console.log(`collect: no built-in collector for provider ${args.provider}; using existing corpus`);
  }

  const targets = selectTargets(args, vendoredNames);
  if (collectAttempted && vendoredNames.length === 0) {
    const pipelineJson = path.join(args.workDir, 'pipeline.json');
    const summary = {
      generatedAt: new Date().toISOString(),
      durationSeconds: Math.round((Date.now() - startedAt) / 1000),
      args: safeArgs(args),
      targets: [],
      steps,
      audit: { issues: [] },
      compare: null,
      triage: [],
      note: 'No new templates were selected after corpus registry filtering.',
    };
    await writeJson(pipelineJson, summary);
    await writeJson(path.join(args.workDir, 'manifest.json'), { targets: [] });
    console.log(`pipeline: ${pipelineJson}`);
    console.log(summary.note);
    return;
  }
  if (targets.length === 0) {
    throw new Error('no target templates selected');
  }

  const audit = args.skipAudit ? { issues: [] } : await runAudit(args, steps, targets);
  const compare = args.skipCompare ? null : await runCompare(args, steps, targets);
  const triage = compare ? await buildTriage(args, compare, audit) : [];
  if (compare) {
    await writeFirstBadCrops(compare, triage);
    await writeJson(path.join(args.workDir, 'triage.json'), { triage });
    await writeTriageMarkdown(args, compare, triage, audit);
  }

  const summary = {
    generatedAt: new Date().toISOString(),
    durationSeconds: Math.round((Date.now() - startedAt) / 1000),
    args: safeArgs(args),
    targets,
    steps,
    audit,
    compare: compare
      ? {
          workDir: compare.workDir,
          comparisonJson: compare.comparisonJson,
          reportJson: compare.reportJson,
          reportMd: compare.reportMd,
        }
      : null,
    triage,
  };
  const pipelineJson = path.join(args.workDir, 'pipeline.json');
  await writeJson(pipelineJson, summary);
  await writeManifest(args, targets);
  let issuesLog = null;
  if (compare && args.updateIssues) {
    issuesLog = await updateIssueLog(steps, args, triage, targets);
  }
  if (args.updateRegistry) {
    await runStep(steps, 'registry', 'node', [
      'scripts/corpus_registry.mjs',
      'record-run',
      '--registry',
      args.registryPath,
      '--pipeline',
      pipelineJson,
    ]);
  }
  if (vendoredNames.length > 0 && !args.keepVendored) {
    await cleanupVendoredTemplates(steps, vendoredNames);
    if (args.updateRegistry) {
      await runStep(steps, 'registry-refresh', 'node', [
        'scripts/corpus_registry.mjs',
        'refresh',
        '--registry',
        args.registryPath,
      ]);
    }
  }
  await writeJson(pipelineJson, {
    ...summary,
    steps,
    issuesLog,
  });

  console.log(`pipeline: ${pipelineJson}`);
  if (compare) {
    console.log(`triage: ${path.join(args.workDir, 'triage.md')}`);
  }
}

async function runAudit(args, steps, targets) {
  const output = await runStep(steps, 'audit', 'node', ['scripts/audit_corpus.mjs', '--json'], {
    captureStdout: true,
  });
  const parsed = JSON.parse(output.stdout || '{"issues":[]}');
  const targetNames = new Set(targets);
  const issues = (parsed.issues ?? []).filter((issue) => targetNames.has(issue.name));
  const audit = { issues };
  await writeJson(path.join(args.workDir, 'audit.json'), audit);
  return audit;
}

async function runCompare(args, steps, targets) {
  const compareDir = path.join(args.workDir, 'compare');
  const compareArgs = [
    'scripts/playwright_compare.mjs',
    '--work-dir',
    compareDir,
    '--timeout-ms',
    String(args.timeoutMs),
    '--width',
    String(args.width),
    '--browser-cache-dir',
    args.browserCacheDir,
    '--max-image-bytes',
    String(args.maxImageBytes),
    '--max-decoded-pixels',
    String(args.maxDecodedPixels),
    '--max-total-resource-bytes',
    String(args.maxTotalResourceBytes),
    '--continue-on-error',
  ];
  if (args.noRemote) compareArgs.push('--no-remote');
  if (args.fixtureFonts) compareArgs.push('--fixture-fonts');
  for (const target of targets) {
    compareArgs.push('--only', target);
  }
  await runStep(steps, 'render-compare', 'node', compareArgs);
  return {
    workDir: compareDir,
    comparisonJson: path.join(compareDir, 'comparison.json'),
    reportJson: path.join(compareDir, 'comparison.report.json'),
    reportMd: path.join(compareDir, 'report.md'),
    results: (await readJson(path.join(compareDir, 'comparison.json'))).results ?? [],
  };
}

async function buildTriage(args, compare, audit) {
  const issueByTemplate = new Map((audit.issues ?? []).map((issue) => [issue.name, issue]));
  const triage = compare.results
    .map((result) => {
      const heightDelta =
        result.status === 'render-failed' ? null : result.rust.height - result.browser.height;
      const classification = classifyResult(result, issueByTemplate.get(result.name));
      return {
        name: result.name,
        priority: classification.priority,
        reason: classification.reason,
        firstBadRegion: result.firstBadRegion ?? null,
        browser: result.browser,
        rust: result.rust,
        heightDelta,
        diffPercent: percent(result.diffRatio),
        nonMediaNonTextPercent: percent(result.nonMediaNonText?.diffRatio),
        mediaPercent: percent(result.media?.diffRatio),
        mediaRectDeltaPercent: percent(result.mediaRects?.deltaRatio),
        textCoverageDeltaPercent: percent(result.textCoverage?.coverageDeltaRatio),
        textRectDeltaPercent: percent(result.textRects?.positionDeltaRatio),
        warnings: result.warningCount,
        assets: result.assetSummary,
        corpusIssue: issueByTemplate.get(result.name) ?? null,
        files: {
          sideBySide: result.sideBySidePng,
          browser: result.browserPng,
          rust: result.rustPng,
          diff: result.diffPng,
          log: result.log,
          diagnostics: result.diagnosticsJson,
          layout: result.layoutJson,
        },
      };
    })
    .sort((left, right) => priorityRank(left.priority) - priorityRank(right.priority));
  await writeJson(path.join(args.workDir, 'triage.json'), { triage });
  return triage;
}

function classifyResult(result, corpusIssue) {
  if (result.status === 'render-failed') {
    return { priority: 'P0', reason: result.error ?? 'renderer failed' };
  }
  if ((result.assetSummary?.failed ?? 0) > 0) {
    const failedByKind = result.assetSummary?.failedByKind ?? {};
    const failedImages =
      (failedByKind.image ?? 0) +
      (failedByKind.background_image ?? 0) +
      (failedByKind.background ?? 0);
    if (failedImages > 0) {
      return { priority: 'P0', reason: 'image asset failed to load' };
    }
    if ((failedByKind.web_font ?? 0) === result.assetSummary.failed) {
      return { priority: 'P2', reason: 'web font failed; using fallback font' };
    }
    return { priority: 'P1', reason: 'non-image asset failed to load' };
  }
  const heightDelta = Math.abs(result.rust.height - result.browser.height);
  if (heightDelta > 300) {
    return { priority: 'P0', reason: `large height delta (${heightDelta}px)` };
  }
  if ((result.nonMediaNonText?.diffRatio ?? 0) > 0.05) {
    return { priority: 'P1', reason: 'layout/background structural diff' };
  }
  if ((result.mediaRects?.deltaRatio ?? 0) > 0.1) {
    return { priority: 'P1', reason: 'media rectangle mismatch' };
  }
  if ((result.media?.diffRatio ?? 0) > 0.25) {
    return { priority: 'P2', reason: 'media pixel mismatch' };
  }
  if ((result.textCoverage?.coverageDeltaRatio ?? 0) > 0.08) {
    return { priority: 'P2', reason: 'text coverage mismatch' };
  }
  if ((result.textRects?.positionDeltaRatio ?? 0) > 0.5) {
    return { priority: 'P2', reason: 'text position/wrap mismatch' };
  }
  if (corpusIssue) {
    return { priority: 'P3', reason: 'corpus issue only' };
  }
  return { priority: 'P3', reason: 'low-risk pixel/raster difference' };
}

async function writeFirstBadCrops(compare, triage) {
  const cropDir = path.join(path.dirname(compare.workDir), 'first-bad-crops');
  await mkdir(cropDir, { recursive: true });
  const resultByName = new Map(compare.results.map((result) => [result.name, result]));
  for (const item of triage) {
    if (!item.firstBadRegion) continue;
    const result = resultByName.get(item.name);
    if (!result) continue;
    const cropPath = path.join(cropDir, `${item.name}.png`);
    await writeCropTriptych(
      [result.browserPng, result.rustPng, result.diffPng],
      cropPath,
      item.firstBadRegion.y0,
      item.firstBadRegion.y1,
    );
    item.files.firstBadCrop = cropPath;
  }
}

async function writeCropTriptych(paths, outPath, y0, y1) {
  const padding = 80;
  const gap = 24;
  const crops = paths.map((filePath) => {
    const source = PNG.sync.read(readFileSync(filePath));
    const top = Math.min(Math.max(0, Math.floor(y0 - padding)), Math.max(0, source.height - 1));
    const bottom = Math.min(source.height, Math.ceil(y1 + padding));
    return cropPng(source, top, Math.max(top + 1, bottom));
  });
  const width = crops.reduce((sum, png) => sum + png.width, 0) + gap * (crops.length - 1);
  const height = Math.max(...crops.map((png) => png.height));
  const output = new PNG({ width, height, colorType: 6 });
  output.data.fill(255);
  let x = 0;
  for (const crop of crops) {
    blit(crop, output, x, 0);
    x += crop.width + gap;
  }
  await writeFile(outPath, PNG.sync.write(output));
}

function cropPng(source, top, bottom) {
  const height = bottom - top;
  const output = new PNG({ width: source.width, height, colorType: 6 });
  output.data.fill(255);
  for (let y = 0; y < height; y += 1) {
    const sourceStart = ((top + y) * source.width) * 4;
    const targetStart = y * source.width * 4;
    source.data.copy(output.data, targetStart, sourceStart, sourceStart + source.width * 4);
  }
  return output;
}

function blit(source, target, offsetX, offsetY) {
  for (let y = 0; y < source.height; y += 1) {
    const sourceStart = y * source.width * 4;
    const targetStart = ((offsetY + y) * target.width + offsetX) * 4;
    source.data.copy(target.data, targetStart, sourceStart, sourceStart + source.width * 4);
  }
}

async function writeTriageMarkdown(args, compare, triage, audit) {
  const counts = countTriagePriorities(triage);
  const lines = [
    '# Corpus Pipeline Report',
    '',
    `- Generated: ${new Date().toISOString()}`,
    `- Width: ${args.width}px`,
    `- Targets: ${triage.length}`,
    `- Priority counts: P0=${counts.P0}, P1=${counts.P1}, P2=${counts.P2}, P3=${counts.P3}`,
    `- Browser cache: \`${args.browserCacheDir}\``,
    `- Compare report: \`${compare.reportMd}\``,
    `- Audit issues: ${(audit.issues ?? []).length}`,
    '',
    '| Priority | Template | Reason | Diff | Height Δ | First Bad | Non-Media Non-Text | Media Rect Δ | Text Rect Δ | Files |',
    '|---|---|---|---:|---:|---|---:|---:|---:|---|',
  ];
  for (const item of triage) {
    const firstBad = item.firstBadRegion
      ? `${item.firstBadRegion.y0}-${item.firstBadRegion.y1}`
      : '';
    const files = [
      markdownLink('crop', item.files.firstBadCrop),
      markdownLink('side-by-side', item.files.sideBySide),
      markdownLink('browser', item.files.browser),
      markdownLink('log', item.files.log),
      markdownLink('layout', item.files.layout),
      markdownLink('diagnostics', item.files.diagnostics),
    ]
      .filter(Boolean)
      .join(' ');
    lines.push(
      `| ${item.priority} | ${item.name} | ${item.reason} | ${item.diffPercent} | ${item.heightDelta === null ? '' : `${item.heightDelta}px`} | ${firstBad} | ${item.nonMediaNonTextPercent} | ${item.mediaRectDeltaPercent} | ${item.textRectDeltaPercent} | ${files} |`,
    );
  }
  lines.push('');
  await writeFile(path.join(args.workDir, 'triage.md'), `${lines.join('\n')}\n`);
}

function countTriagePriorities(triage) {
  const counts = { P0: 0, P1: 0, P2: 0, P3: 0 };
  for (const item of triage) {
    if (Object.prototype.hasOwnProperty.call(counts, item.priority)) {
      counts[item.priority] += 1;
    }
  }
  return counts;
}

async function writeManifest(args, targets) {
  const catalog = await readCatalog();
  const byName = new Map(catalog.map((entry) => [entry.name, entry]));
  const entries = [];
  for (const name of targets) {
    const entry = byName.get(name);
    if (!entry?.sourcePath) {
      entries.push({ name, hash: null, sourcePath: entry?.sourcePath ?? null });
      continue;
    }
    const htmlPath = path.resolve(ROOT_DIR, entry.sourcePath);
    const html = await readFile(htmlPath);
    entries.push({
      name,
      sourcePath: entry.sourcePath,
      htmlHash: createHash('sha256').update(html).digest('hex'),
    });
  }
  await writeJson(path.join(args.workDir, 'manifest.json'), { targets: entries });
}

async function cleanupVendoredTemplates(steps, names) {
  const startedAt = new Date().toISOString();
  const catalogPath = path.join(ROOT_DIR, 'corpus', 'catalog.json');
  const catalog = await readCatalog();
  const removeNames = new Set(names);
  const removedEntries = catalog.filter((entry) => removeNames.has(entry.name));
  if (removedEntries.length === 0) {
    steps.push({
      name: 'cleanup-vendored',
      command: ['internal', 'cleanup-vendored', ...names],
      startedAt,
      finishedAt: new Date().toISOString(),
      exitCode: 0,
    });
    return;
  }
  const retainedCatalog = catalog.filter((entry) => !removeNames.has(entry.name));
  await writeJson(catalogPath, retainedCatalog);
  for (const entry of removedEntries) {
    if (!entry.sourcePath) continue;
    const htmlPath = path.resolve(ROOT_DIR, entry.sourcePath);
    const assetDir = htmlPath.replace(/\.html$/i, '.assets');
    await rm(htmlPath, { force: true });
    await rm(assetDir, { recursive: true, force: true });
  }
  steps.push({
    name: 'cleanup-vendored',
    command: ['internal', 'cleanup-vendored', ...names],
    startedAt,
    finishedAt: new Date().toISOString(),
    exitCode: 0,
  });
}

async function updateIssueLog(steps, args, triage, targets) {
  const startedAt = new Date().toISOString();
  const runAt = new Date().toISOString();
  const commit = await currentGitCommit();
  const workDir = relativePath(args.workDir);
  const targetNames = new Set(targets);
  const existing = await readJson(args.issuesLogPath).catch(() => ({
    schemaVersion: 1,
    updatedAt: null,
    issues: [],
  }));
  const byKey = new Map((existing.issues ?? []).map((issue) => [issue.key, issue]));
  const currentPendingKeys = new Set();
  const currentRunIssues = [];

  for (const item of triage) {
    if (!isPendingPriority(item.priority)) {
      continue;
    }
    const type = issueTypeForReason(item.reason);
    const key = `${item.name}:${type}`;
    currentPendingKeys.add(key);
    const previous = byKey.get(key);
    const issue = normalizeIssue({
      ...previous,
      key,
      name: item.name,
      type,
      status: 'pending',
      firstSeenAt: previous?.firstSeenAt ?? runAt,
      lastSeenAt: runAt,
      fixedAt: null,
      fixCommit: null,
      fixedInRun: null,
      occurrences: (previous?.occurrences ?? 0) + 1,
      latest: issueSnapshot(item, { runAt, commit, workDir }),
    });
    byKey.set(key, issue);
    currentRunIssues.push(issue);
  }

  for (const issue of byKey.values()) {
    if (
      issue.status === 'pending' &&
      targetNames.has(issue.name) &&
      !currentPendingKeys.has(issue.key)
    ) {
      issue.status = 'fixed';
      issue.fixedAt = runAt;
      issue.fixCommit = commit;
      issue.fixedInRun = workDir;
    }
  }

  const issues = [...byKey.values()].map(normalizeIssue).sort(compareIssues);
  const summaryByType = summarizeIssuesByType(issues, currentRunIssues);
  const log = {
    schemaVersion: existing.schemaVersion ?? 1,
    updatedAt: runAt,
    summary: {
      pendingCount: issues.filter((issue) => issue.status === 'pending').length,
      fixedCount: issues.filter((issue) => issue.status === 'fixed').length,
      byType: summaryByType,
    },
    issues,
  };
  await writeJson(args.issuesLogPath, log);
  const runSummary = {
    issuesLogPath: args.issuesLogPath,
    pendingCount: issues.filter((issue) => issue.status === 'pending').length,
    fixedCount: issues.filter((issue) => issue.status === 'fixed').length,
    currentRunPendingCount: currentRunIssues.length,
    currentRunIssues: currentRunIssues.map((issue) => issue.key),
    byType: summaryByType,
  };
  await writeJson(path.join(args.workDir, 'issues.json'), runSummary);
  steps.push({
    name: 'issues-log',
    command: ['internal', 'issues-log', args.issuesLogPath],
    startedAt,
    finishedAt: new Date().toISOString(),
    exitCode: 0,
  });
  return runSummary;
}

function isPendingPriority(priority) {
  return priority === 'P0' || priority === 'P1' || priority === 'P2';
}

function issueTypeForReason(reason = '') {
  if (reason.includes('renderer failed')) return 'render_failed';
  if (reason.includes('image asset failed')) return 'image_asset_failed';
  if (reason.includes('web font failed')) return 'web_font_failed';
  if (reason.includes('non-image asset failed')) return 'non_image_asset_failed';
  if (reason.includes('large height delta')) return 'large_height_delta';
  if (reason.includes('layout/background structural diff')) return 'structural_diff';
  if (reason.includes('media rectangle mismatch')) return 'media_rect_mismatch';
  if (reason.includes('media pixel mismatch')) return 'media_pixel_mismatch';
  if (reason.includes('text coverage mismatch')) return 'text_coverage_mismatch';
  if (reason.includes('text position/wrap mismatch')) return 'text_position_wrap_mismatch';
  if (reason.includes('corpus issue')) return 'corpus_issue';
  return reason
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '') || 'unknown';
}

function issueSnapshot(item, context) {
  return {
    runAt: context.runAt,
    commit: context.commit,
    workDir: context.workDir,
    priority: item.priority,
    reason: item.reason,
    diffPercent: item.diffPercent,
    heightDelta: item.heightDelta,
    firstBadRegion: item.firstBadRegion ?? null,
    nonMediaNonTextPercent: item.nonMediaNonTextPercent,
    mediaPercent: item.mediaPercent,
    mediaRectDeltaPercent: item.mediaRectDeltaPercent,
    textCoverageDeltaPercent: item.textCoverageDeltaPercent,
    textRectDeltaPercent: item.textRectDeltaPercent,
    warnings: item.warnings,
    assets: item.assets,
  };
}

function normalizeIssue(issue) {
  return {
    key: issue.key,
    name: issue.name,
    type: issue.type,
    status: issue.status ?? 'pending',
    firstSeenAt: issue.firstSeenAt ?? null,
    lastSeenAt: issue.lastSeenAt ?? null,
    fixedAt: issue.fixedAt ?? null,
    fixCommit: issue.fixCommit ?? null,
    fixedInRun: issue.fixedInRun ?? null,
    occurrences: issue.occurrences ?? 0,
    latest: issue.latest ?? null,
  };
}

function compareIssues(left, right) {
  const statusRank = { pending: 0, fixed: 1 };
  return (
    (statusRank[left.status] ?? 9) - (statusRank[right.status] ?? 9) ||
    left.name.localeCompare(right.name) ||
    left.type.localeCompare(right.type)
  );
}

function summarizeIssuesByType(issues, currentRunIssues = []) {
  const currentRunCounts = new Map();
  for (const issue of currentRunIssues) {
    currentRunCounts.set(issue.type, (currentRunCounts.get(issue.type) ?? 0) + 1);
  }

  const byType = new Map();
  for (const issue of issues) {
    const entry = byType.get(issue.type) ?? {
      type: issue.type,
      pendingTemplates: 0,
      fixedTemplates: 0,
      totalTemplates: 0,
      totalOccurrences: 0,
      currentRunOccurrences: 0,
      latestSeenAt: null,
    };
    entry.totalTemplates += 1;
    entry.totalOccurrences += issue.occurrences ?? 0;
    if (issue.status === 'pending') {
      entry.pendingTemplates += 1;
    } else if (issue.status === 'fixed') {
      entry.fixedTemplates += 1;
    }
    if (!entry.latestSeenAt || (issue.lastSeenAt && issue.lastSeenAt > entry.latestSeenAt)) {
      entry.latestSeenAt = issue.lastSeenAt;
    }
    byType.set(issue.type, entry);
  }

  for (const [type, count] of currentRunCounts) {
    const entry = byType.get(type);
    if (entry) {
      entry.currentRunOccurrences = count;
    }
  }

  return [...byType.values()].sort(
    (left, right) =>
      right.pendingTemplates - left.pendingTemplates ||
      right.totalOccurrences - left.totalOccurrences ||
      left.type.localeCompare(right.type),
  );
}

function selectTargets(args, vendoredNames) {
  if (args.only.length > 0) {
    return [...new Set(args.only)];
  }
  if (vendoredNames.length > 0) {
    return vendoredNames.slice(0, args.limit);
  }
  const pool = TEMPLATE_CORPUS.filter((template) => {
    if (args.provider && template.provider !== args.provider) return false;
    if (args.category && template.category !== args.category) return false;
    return true;
  });
  return pool.slice(0, args.limit).map((template) => template.name);
}

function parseVendoredNames(stdout) {
  const names = [];
  for (const line of stdout.split('\n')) {
    const match = line.match(/\bvendored\s+([a-z0-9_-]+)/i);
    if (match) {
      names.push(match[1]);
    }
  }
  return [...new Set(names)];
}

async function runStep(steps, name, command, args, options = {}) {
  const startedAt = new Date().toISOString();
  console.log(`\n[${name}] ${command} ${args.join(' ')}`);
  const result = await runCommand(command, args, options);
  steps.push({
    name,
    command: [command, ...args],
    startedAt,
    finishedAt: new Date().toISOString(),
    exitCode: result.exitCode,
  });
  return result;
}

function runCommand(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: ROOT_DIR,
      stdio: options.captureStdout ? ['ignore', 'pipe', 'pipe'] : 'inherit',
      env: process.env,
    });
    let stdout = '';
    let stderr = '';
    if (options.captureStdout) {
      child.stdout.on('data', (chunk) => {
        stdout += chunk;
        if (options.teeStdout) {
          process.stdout.write(chunk);
        }
      });
      child.stderr.on('data', (chunk) => {
        stderr += chunk;
        process.stderr.write(chunk);
      });
    }
    child.on('error', reject);
    child.on('close', (exitCode) => {
      if (exitCode === 0) {
        resolve({ exitCode, stdout, stderr });
      } else {
        reject(new Error(`${command} ${args.join(' ')} failed with exit code ${exitCode}`));
      }
    });
  });
}

async function readCatalog() {
  return readJson(path.join(ROOT_DIR, 'corpus', 'catalog.json')).catch(() => []);
}

async function readJson(filePath) {
  return JSON.parse(await readFile(filePath, 'utf8'));
}

async function writeJson(filePath, value) {
  await mkdir(path.dirname(filePath), { recursive: true });
  await writeFile(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function markdownLink(label, filePath) {
  return filePath ? `[${label}](${filePath})` : '';
}

function percent(value) {
  return `${(((Number.isFinite(value) ? value : 0) ?? 0) * 100).toFixed(2)}%`;
}

function priorityRank(priority) {
  return { P0: 0, P1: 1, P2: 2, P3: 3 }[priority] ?? 9;
}

function positiveInt(value, name) {
  const number = Number.parseInt(value, 10);
  if (!Number.isFinite(number) || number <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return number;
}

function safeArgs(args) {
  return {
    ...args,
    login: Boolean(args.login),
    headful: Boolean(args.headful),
  };
}

async function currentGitCommit() {
  const result = await runCommand('git', ['rev-parse', '--short', 'HEAD'], {
    captureStdout: true,
  });
  return result.stdout.trim();
}

function relativePath(filePath) {
  return path.relative(ROOT_DIR, filePath).replaceAll(path.sep, '/');
}

main().catch((error) => {
  console.error(`Error: ${error.message}`);
  process.exit(1);
});
