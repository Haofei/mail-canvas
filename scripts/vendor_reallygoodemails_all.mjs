#!/usr/bin/env node

import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const RGE_ORIGIN = 'https://reallygoodemails.com';
const PROVIDER = 'reallygoodemails';
const DEFAULT_OUT_DIR = path.join(ROOT_DIR, 'corpus', PROVIDER);
const DEFAULT_CATALOG = path.join(DEFAULT_OUT_DIR, 'catalog.json');
const DEFAULT_REGISTRY = path.join(DEFAULT_OUT_DIR, 'registry.json');
const DEFAULT_STORAGE_STATE = path.join(ROOT_DIR, '.rge-auth', 'storage-state.json');
const DEFAULT_REPORT = path.join(DEFAULT_OUT_DIR, 'download-report.json');

function parseArgs(argv) {
  const args = {
    categories: [],
    includeLatest: false,
    maxCategories: null,
    limit: null,
    perCategoryLimit: 500,
    outDir: DEFAULT_OUT_DIR,
    catalogPath: DEFAULT_CATALOG,
    registryPath: DEFAULT_REGISTRY,
    storageState: DEFAULT_STORAGE_STATE,
    reportPath: DEFAULT_REPORT,
    replaceProvider: false,
    headful: false,
    login: Boolean(process.env.RGE_EMAIL && process.env.RGE_PASSWORD),
    timeoutMs: 45000,
    scrollAttempts: 80,
    stableScrolls: 4,
    allowIncompleteAssets: false,
    dryRun: false,
    stopOnError: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`missing value for ${arg}`);
      return argv[index];
    };
    switch (arg) {
      case '--category':
        args.categories.push(next());
        break;
      case '--include-latest':
        args.includeLatest = true;
        break;
      case '--max-categories':
        args.maxCategories = positiveInt(next(), '--max-categories');
        break;
      case '--limit':
        args.limit = positiveInt(next(), '--limit');
        break;
      case '--per-category-limit':
        args.perCategoryLimit = positiveInt(next(), '--per-category-limit');
        break;
      case '--out-dir':
        args.outDir = path.resolve(next());
        break;
      case '--catalog':
        args.catalogPath = path.resolve(next());
        break;
      case '--registry':
        args.registryPath = path.resolve(next());
        break;
      case '--storage-state':
        args.storageState = path.resolve(next());
        break;
      case '--report':
        args.reportPath = path.resolve(next());
        break;
      case '--replace-provider':
        args.replaceProvider = true;
        break;
      case '--headful':
        args.headful = true;
        break;
      case '--login':
        args.login = true;
        break;
      case '--timeout-ms':
        args.timeoutMs = positiveInt(next(), '--timeout-ms');
        break;
      case '--scroll-attempts':
        args.scrollAttempts = positiveInt(next(), '--scroll-attempts');
        break;
      case '--stable-scrolls':
        args.stableScrolls = positiveInt(next(), '--stable-scrolls');
        break;
      case '--allow-incomplete-assets':
        args.allowIncompleteAssets = true;
        break;
      case '--dry-run':
        args.dryRun = true;
        break;
      case '--stop-on-error':
        args.stopOnError = true;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const startedAt = new Date();
  let categories = args.categories.length > 0 ? args.categories : await fetchCategorySlugs();
  categories = [...new Set(categories.map(normalizeCategory).filter(Boolean))];
  if (args.maxCategories != null) {
    categories = categories.slice(0, args.maxCategories);
  }

  const jobs = [
    ...(args.includeLatest ? [{ kind: 'latest', label: 'latest' }] : []),
    ...categories.map((category) => ({ kind: 'category', label: category })),
  ];
  console.log(`Really Good Emails jobs: ${jobs.length}`);
  for (const job of jobs) {
    console.log(`- ${job.label}`);
  }

  if (args.dryRun) {
    return;
  }

  await mkdir(args.outDir, { recursive: true });
  if (args.replaceProvider) {
    await rm(args.outDir, { recursive: true, force: true });
    await mkdir(args.outDir, { recursive: true });
  }

  const results = [];
  let totalVendored = 0;
  for (const [index, job] of jobs.entries()) {
    if (args.limit != null && totalVendored >= args.limit) {
      break;
    }
    const remaining = args.limit == null ? args.perCategoryLimit : args.limit - totalVendored;
    const limit = Math.min(args.perCategoryLimit, remaining);
    const childArgs = vendorArgs(args, job, limit, index === 0 && args.replaceProvider);
    console.log(`\n[${index + 1}/${jobs.length}] ${job.label}: limit ${limit}`);
    const started = Date.now();
    const result = await runNodeScript('scripts/vendor_reallygoodemails.mjs', childArgs);
    const durationMs = Date.now() - started;
    const vendored = countVendored(result.stdout);
    totalVendored += vendored;
    const entry = {
      job,
      status: result.status,
      durationMs,
      vendored,
      skipped: countSkipped(result.stdout, result.stderr),
    };
    results.push(entry);
    if (result.status !== 0 && args.stopOnError) {
      await writeReport(args.reportPath, args, startedAt, results);
      process.exitCode = result.status;
      return;
    }
    await writeReport(args.reportPath, args, startedAt, results);
  }

  await writeReport(args.reportPath, args, startedAt, results);
  console.log(`\nvendored ${totalVendored} templates`);
  console.log(args.reportPath);
}

async function fetchCategorySlugs() {
  const response = await fetch(`${RGE_ORIGIN}/sitemap-categories.xml`);
  if (!response.ok) {
    throw new Error(`failed to fetch sitemap-categories.xml: HTTP ${response.status}`);
  }
  const xml = await response.text();
  const categories = [];
  for (const match of xml.matchAll(/https:\/\/reallygoodemails\.com\/categories\/([^<]+)/g)) {
    categories.push(decodeURIComponent(match[1].trim()));
  }
  if (categories.length === 0) {
    throw new Error('no categories found in Really Good Emails category sitemap');
  }
  return categories;
}

function vendorArgs(args, job, limit, replaceProvider) {
  const childArgs = [
    '--limit',
    String(limit),
    '--out-dir',
    args.outDir,
    '--catalog',
    args.catalogPath,
    '--registry',
    args.registryPath,
    '--storage-state',
    args.storageState,
    '--timeout-ms',
    String(args.timeoutMs),
    '--scroll-attempts',
    String(args.scrollAttempts),
    '--stable-scrolls',
    String(args.stableScrolls),
    '--exclude-existing',
    '--exclude-seen',
  ];
  if (job.kind === 'latest') {
    childArgs.push('--collection', 'latest');
  } else {
    childArgs.push('--category', job.label);
  }
  if (args.login) childArgs.push('--login');
  if (args.headful) childArgs.push('--headful');
  if (args.allowIncompleteAssets) childArgs.push('--allow-incomplete-assets');
  if (replaceProvider) childArgs.push('--replace-provider');
  return childArgs;
}

function runNodeScript(script, args) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [script, ...args], {
      cwd: ROOT_DIR,
      env: process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      const text = chunk.toString();
      stdout += text;
      process.stdout.write(text);
    });
    child.stderr.on('data', (chunk) => {
      const text = chunk.toString();
      stderr += text;
      process.stderr.write(text);
    });
    child.on('close', (status) => resolve({ status, stdout, stderr }));
  });
}

async function writeReport(reportPath, args, startedAt, results) {
  const report = {
    schemaVersion: 1,
    startedAt: startedAt.toISOString(),
    updatedAt: new Date().toISOString(),
    provider: PROVIDER,
    outDir: path.relative(ROOT_DIR, args.outDir).replaceAll(path.sep, '/'),
    catalogPath: path.relative(ROOT_DIR, args.catalogPath).replaceAll(path.sep, '/'),
    registryPath: path.relative(ROOT_DIR, args.registryPath).replaceAll(path.sep, '/'),
    totalVendored: results.reduce((sum, result) => sum + result.vendored, 0),
    totalSkipped: results.reduce((sum, result) => sum + result.skipped, 0),
    failedJobs: results.filter((result) => result.status !== 0).length,
    results,
  };
  await mkdir(path.dirname(reportPath), { recursive: true });
  await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`);
}

function countVendored(output) {
  return (output.match(/^vendored /gm) ?? []).length;
}

function countSkipped(stdout, stderr) {
  return (`${stdout}\n${stderr}`.match(/^skipped /gm) ?? []).length;
}

function normalizeCategory(category) {
  return category
    .trim()
    .replace(/^https?:\/\/(?:www\.)?reallygoodemails\.com\/categories\//, '')
    .replace(/^\/?categories\//, '')
    .replace(/^\/+|\/+$/g, '');
}

function positiveInt(value, name) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${name} must be positive`);
  return parsed;
}

main().catch((error) => {
  console.error(error?.stack ?? error?.message ?? String(error));
  process.exitCode = 1;
});
