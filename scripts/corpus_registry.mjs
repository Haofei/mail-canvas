#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { readdir, readFile, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_CATALOG = path.join(ROOT_DIR, 'corpus', 'catalog.json');
const DEFAULT_REGISTRY = path.join(ROOT_DIR, 'corpus', 'registry.json');

function parseArgs(argv) {
  const args = {
    command: argv[0] ?? 'refresh',
    catalogPath: DEFAULT_CATALOG,
    registryPath: DEFAULT_REGISTRY,
    pipelinePath: null,
    preserveMissing: true,
  };

  for (let index = 1; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`missing value for ${arg}`);
      return argv[index];
    };
    switch (arg) {
      case '--catalog':
        args.catalogPath = path.resolve(next());
        break;
      case '--registry':
        args.registryPath = path.resolve(next());
        break;
      case '--pipeline':
        args.pipelinePath = path.resolve(next());
        break;
      case '--drop-missing':
        args.preserveMissing = false;
        break;
      case '--preserve-missing':
        args.preserveMissing = true;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  switch (args.command) {
    case 'refresh':
      await refreshRegistry(args);
      break;
    case 'record-run':
      await recordRun(args);
      break;
    default:
      throw new Error(`unknown command: ${args.command}`);
  }
}

async function refreshRegistry(args) {
  const catalog = await readJson(args.catalogPath).catch(() => []);
  const existing = args.preserveMissing
    ? await readJson(args.registryPath).catch(() => emptyRegistry())
    : emptyRegistry();
  const existingByName = new Map((existing.templates ?? []).map((entry) => [entry.name, entry]));
  const templates = [];

  for (const entry of catalog) {
    const previous = existingByName.get(entry.name) ?? {};
    const fingerprint = entry.sourcePath
      ? await fingerprintTemplate(entry.sourcePath).catch((error) => ({
          fingerprintError: error.message,
        }))
      : {};
    templates.push(normalizeRegistryEntry({
      ...previous,
      ...entry,
      ...fingerprint,
      retainedInRepo: Boolean(entry.sourcePath),
    }));
    existingByName.delete(entry.name);
  }

  if (args.preserveMissing) {
    for (const previous of existingByName.values()) {
      templates.push(normalizeRegistryEntry({
        ...previous,
        retainedInRepo: false,
      }));
    }
  }

  await writeJson(args.registryPath, {
    schemaVersion: 1,
    updatedAt: new Date().toISOString(),
    templates: sortTemplates(templates),
  });
  console.log(args.registryPath);
}

async function recordRun(args) {
  if (!args.pipelinePath) {
    throw new Error('record-run requires --pipeline');
  }
  const registry = await readJson(args.registryPath).catch(() => emptyRegistry());
  const pipeline = await readJson(args.pipelinePath);
  const catalog = await readJson(args.catalogPath).catch(() => []);
  const catalogByName = new Map(catalog.map((entry) => [entry.name, entry]));
  const manifestPath = path.join(path.dirname(args.pipelinePath), 'manifest.json');
  const manifest = await readJson(manifestPath).catch(() => ({ targets: [] }));
  const triageByName = new Map((pipeline.triage ?? []).map((entry) => [entry.name, entry]));
  const manifestByName = new Map((manifest.targets ?? []).map((entry) => [entry.name, entry]));
  const byName = new Map((registry.templates ?? []).map((entry) => [entry.name, entry]));

  for (const target of pipeline.targets ?? []) {
    const name = typeof target === 'string' ? target : target.name;
    if (!name) continue;
    const previous = byName.get(name) ?? { name };
    const catalogEntry = catalogByName.get(name) ?? {};
    const triage = triageByName.get(name);
    const manifestEntry = manifestByName.get(name) ?? {};
    const fingerprint = catalogEntry.sourcePath
      ? await fingerprintTemplate(catalogEntry.sourcePath).catch((error) => ({
          fingerprintError: error.message,
        }))
      : {};
    byName.set(name, normalizeRegistryEntry({
      ...previous,
      ...catalogEntry,
      ...manifestEntry,
      ...fingerprint,
      name,
      lastRun: {
        at: pipeline.generatedAt,
        commit: await currentGitCommit(),
        workDir: path.relative(ROOT_DIR, path.dirname(args.pipelinePath)).replaceAll(path.sep, '/'),
        priority: triage?.priority ?? null,
        reason: triage?.reason ?? null,
        diffPercent: triage?.diffPercent ?? null,
        heightDelta: triage?.heightDelta ?? null,
      },
    }));
  }

  await writeJson(args.registryPath, {
    schemaVersion: registry.schemaVersion ?? 1,
    updatedAt: new Date().toISOString(),
    templates: sortTemplates([...byName.values()]),
  });
  console.log(args.registryPath);
}

async function fingerprintTemplate(sourcePath) {
  const absoluteHtmlPath = path.resolve(ROOT_DIR, sourcePath);
  const html = await readFile(absoluteHtmlPath);
  const assetDir = absoluteHtmlPath.replace(/\.html$/i, '.assets');
  const assets = await fingerprintAssets(assetDir);
  const assetManifest = assets
    .map((asset) => `${asset.path}\0${asset.bytes}\0${asset.md5}`)
    .join('\n');
  const contentMd5 = createHash('md5')
    .update(md5(html))
    .update('\0')
    .update(md5(assetManifest))
    .digest('hex');
  return {
    sourcePath: sourcePath.replaceAll(path.sep, '/'),
    htmlMd5: md5(html),
    htmlBytes: html.length,
    assetCount: assets.length,
    assetBytes: assets.reduce((sum, asset) => sum + asset.bytes, 0),
    assetManifestMd5: md5(assetManifest),
    contentMd5,
  };
}

async function fingerprintAssets(assetDir) {
  if (!(await exists(assetDir))) {
    return [];
  }
  const files = await walkFiles(assetDir);
  const assets = [];
  for (const filePath of files) {
    const bytes = await readFile(filePath);
    assets.push({
      path: path.relative(assetDir, filePath).replaceAll(path.sep, '/'),
      bytes: bytes.length,
      md5: md5(bytes),
    });
  }
  return assets.sort((left, right) => left.path.localeCompare(right.path));
}

async function walkFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...await walkFiles(entryPath));
    } else if (entry.isFile()) {
      files.push(entryPath);
    }
  }
  return files;
}

function normalizeRegistryEntry(entry) {
  const provider = entry.provider ?? providerFromName(entry.name);
  return {
    name: entry.name,
    provider,
    url: entry.url ?? null,
    sourcePath: entry.sourcePath ?? null,
    htmlMd5: entry.htmlMd5 ?? null,
    htmlBytes: entry.htmlBytes ?? null,
    assetCount: entry.assetCount ?? 0,
    assetBytes: entry.assetBytes ?? 0,
    assetManifestMd5: entry.assetManifestMd5 ?? null,
    contentMd5: entry.contentMd5 ?? null,
    retainedInRepo: Boolean(entry.retainedInRepo),
    lastRun: entry.lastRun ?? null,
  };
}

function providerFromName(name = '') {
  return name.split('-', 1)[0] || 'unknown';
}

function sortTemplates(templates) {
  return templates.sort((left, right) => left.name.localeCompare(right.name));
}

async function currentGitCommit() {
  const { spawnSync } = await import('node:child_process');
  const result = spawnSync('git', ['rev-parse', '--short', 'HEAD'], {
    cwd: ROOT_DIR,
    encoding: 'utf8',
  });
  return result.status === 0 ? result.stdout.trim() : null;
}

function emptyRegistry() {
  return { schemaVersion: 1, updatedAt: null, templates: [] };
}

async function exists(filePath) {
  try {
    await stat(filePath);
    return true;
  } catch {
    return false;
  }
}

function md5(value) {
  return createHash('md5').update(value).digest('hex');
}

async function readJson(filePath) {
  return JSON.parse(await readFile(filePath, 'utf8'));
}

async function writeJson(filePath, value) {
  await writeFile(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

main().catch((error) => {
  console.error(`Error: ${error.message}`);
  process.exit(1);
});
