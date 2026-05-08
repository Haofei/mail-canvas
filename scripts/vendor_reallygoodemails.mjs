#!/usr/bin/env node

import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

import { chromium } from 'playwright';

import { mirrorHtml } from './vendor_corpus_templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const RGE_ORIGIN = 'https://reallygoodemails.com';
const PROVIDER = 'reallygoodemails';
const CORPUS_DIR = path.join(ROOT_DIR, 'corpus', PROVIDER);
const CATALOG_PATH = path.join(ROOT_DIR, 'corpus', 'catalog.json');
const REGISTRY_PATH = path.join(ROOT_DIR, 'corpus', 'registry.json');
const DEFAULT_STORAGE_STATE = path.join(ROOT_DIR, '.rge-auth', 'storage-state.json');

function parseArgs(argv) {
  const args = {
    category: 'promotional',
    collection: null,
    limit: 12,
    outDir: CORPUS_DIR,
    catalogPath: CATALOG_PATH,
    storageState: DEFAULT_STORAGE_STATE,
    replaceProvider: false,
    headful: false,
    login: Boolean(process.env.RGE_EMAIL && process.env.RGE_PASSWORD),
    timeoutMs: 30000,
    random: false,
    excludeExisting: false,
    excludeSeen: false,
    registryPath: REGISTRY_PATH,
    requireCompleteAssets: true,
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
        args.category = next();
        break;
      case '--collection':
        args.collection = next();
        break;
      case '--limit':
        args.limit = positiveInt(next(), '--limit');
        break;
      case '--out-dir':
        args.outDir = path.resolve(next());
        break;
      case '--catalog':
        args.catalogPath = path.resolve(next());
        break;
      case '--storage-state':
        args.storageState = path.resolve(next());
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
      case '--random':
        args.random = true;
        break;
      case '--exclude-existing':
        args.excludeExisting = true;
        break;
      case '--exclude-seen':
        args.excludeSeen = true;
        break;
      case '--registry':
        args.registryPath = path.resolve(next());
        break;
      case '--allow-incomplete-assets':
        args.requireCompleteAssets = false;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  await mkdir(args.outDir, { recursive: true });
  if (args.replaceProvider) {
    await rm(args.outDir, { recursive: true, force: true });
    await mkdir(args.outDir, { recursive: true });
  }

  const browser = await chromium.launch({ headless: !args.headful });
  const context = await newContext(browser, args);
  try {
    if (args.login) {
      await loginIfNeeded(context, args);
    }
    const page = await context.newPage();
    let slugs = await collectSlugs(page, args);
    if (args.excludeExisting) {
      const existing = await existingReallyGoodEmailsSlugs(args.catalogPath);
      slugs = slugs.filter((slug) => !existing.has(slug));
    }
    if (args.excludeSeen) {
      const seen = await seenReallyGoodEmailsSlugs(args.registryPath);
      slugs = slugs.filter((slug) => !seen.has(slug));
    }
    if (args.random) {
      shuffle(slugs);
    }
    console.log(`found ${slugs.length} Really Good Emails slugs`);

    const catalogEntries = [];
    for (const slug of slugs) {
      if (catalogEntries.length >= args.limit) {
        break;
      }
      try {
        const entry = await vendorSlug(context, slug, args);
        catalogEntries.push(entry);
        console.log(`vendored ${entry.name}`);
      } catch (error) {
        console.warn(`skipped ${slug}: ${error.message}`);
      }
      await wait(500);
    }
    if (catalogEntries.length === 0 && args.excludeSeen) {
      console.log('no new Really Good Emails templates');
    } else if (catalogEntries.length < args.limit && !args.excludeSeen) {
      throw new Error(`vendored ${catalogEntries.length} templates, expected ${args.limit}`);
    } else if (catalogEntries.length < args.limit) {
      console.warn(`vendored ${catalogEntries.length} new templates, requested ${args.limit}`);
    }
    await updateCatalog(args.catalogPath, catalogEntries, args.replaceProvider);
    console.log(args.catalogPath);
  } finally {
    await context.close();
    await browser.close();
  }
}

async function newContext(browser, args) {
  const contextOptions = {
    viewport: { width: 1280, height: 900 },
    userAgent:
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124 Safari/537.36 MailCanvasRegressionBot/1.0',
  };
  if (await fileExists(args.storageState)) {
    contextOptions.storageState = args.storageState;
  }
  return browser.newContext(contextOptions);
}

async function loginIfNeeded(context, args) {
  const email = process.env.RGE_EMAIL;
  const password = process.env.RGE_PASSWORD;
  if (!email || !password) {
    throw new Error('RGE_EMAIL and RGE_PASSWORD are required when --login is used');
  }
  const page = await context.newPage();
  try {
    await page.goto(`${RGE_ORIGIN}/login`, {
      waitUntil: 'domcontentloaded',
      timeout: args.timeoutMs,
    });
    await page.waitForTimeout(1000);
    if (!(await page.locator('input[type="email"], input[name="email"]').count())) {
      return;
    }
    await page.locator('input[type="email"], input[name="email"]').first().fill(email);
    await page.locator('input[type="password"], input[name="password"]').first().fill(password);
    await Promise.all([
      page.waitForLoadState('networkidle', { timeout: args.timeoutMs }).catch(() => undefined),
      page.locator('button[type="submit"], button:has-text("Log In"), button:has-text("Login")').first().click(),
    ]);
    await mkdir(path.dirname(args.storageState), { recursive: true });
    await context.storageState({ path: args.storageState });
  } finally {
    await page.close();
  }
}

async function collectSlugs(page, args) {
  const url =
    args.collection === 'latest'
      ? `${RGE_ORIGIN}/latest`
      : `${RGE_ORIGIN}/categories/${encodeURIComponent(args.category)}`;
  await page.goto(url, { waitUntil: 'domcontentloaded', timeout: args.timeoutMs });
  await page.waitForTimeout(1500);
  const slugs = await page.evaluate(() => {
    const found = new Set();
    for (const link of document.querySelectorAll('a[href*="/emails/"]')) {
      const match = link.href.match(/\/emails\/([^/?#]+)/);
      if (match) found.add(match[1].replace(/\.png$/i, ''));
    }
    for (const image of document.querySelectorAll('img[src*="/emails/"]')) {
      const match = image.src.match(/\/emails\/(?:mobile\/)?([^/?#]+?)\.png/i);
      if (match) found.add(match[1]);
    }
    const bodyHtml = document.documentElement.innerHTML;
    for (const match of bodyHtml.matchAll(/\/emails\/(?:mobile\/)?([^"'<>?#]+?)\.png/gi)) {
      found.add(match[1]);
    }
    return [...found].filter((slug) => slug && !slug.includes('/'));
  });
  return [...new Set(slugs)];
}

async function existingReallyGoodEmailsSlugs(catalogPath) {
  const entries = await readJson(catalogPath).catch(() => []);
  const slugs = new Set();
  for (const entry of entries) {
    if (!entry.name?.startsWith(`${PROVIDER}-`)) {
      continue;
    }
    slugs.add(entry.name.slice(`${PROVIDER}-`.length));
  }
  return slugs;
}

async function seenReallyGoodEmailsSlugs(registryPath) {
  const registry = await readJson(registryPath).catch(() => ({ templates: [] }));
  const slugs = new Set();
  for (const entry of registry.templates ?? []) {
    if (entry.provider !== PROVIDER && !entry.name?.startsWith(`${PROVIDER}-`)) {
      continue;
    }
    if (entry.name?.startsWith(`${PROVIDER}-`)) {
      slugs.add(entry.name.slice(`${PROVIDER}-`.length));
    }
    const match = entry.url?.match(/\/emails\/([^/?#]+)/);
    if (match) {
      slugs.add(match[1].replace(/\.png$/i, ''));
    }
  }
  return slugs;
}

function shuffle(values) {
  for (let index = values.length - 1; index > 0; index -= 1) {
    const swapIndex = Math.floor(Math.random() * (index + 1));
    [values[index], values[swapIndex]] = [values[swapIndex], values[index]];
  }
  return values;
}

async function vendorSlug(context, slug, args) {
  const page = await context.newPage();
  const detailUrl = `${RGE_ORIGIN}/emails/${slug}`;
  const name = `${PROVIDER}-${slugToTemplateName(slug)}`;
  const providerDir = args.outDir;
  const htmlPath = path.join(providerDir, `${name}.html`);
  const assetDir = path.join(providerDir, `${name}.assets`);
  try {
    await page.goto(detailUrl, { waitUntil: 'domcontentloaded', timeout: args.timeoutMs });
    await page.waitForTimeout(1000);
    const html = await extractEmailHtml(page);
    await rm(htmlPath, { force: true });
    await rm(assetDir, { recursive: true, force: true });
    await mkdir(assetDir, { recursive: true });
    const rewritten = await mirrorHtml(html, detailUrl, assetDir, {
      requireCompleteAssets: args.requireCompleteAssets,
    });
    await writeFile(htmlPath, rewritten, 'utf8');
    return {
      name,
      url: detailUrl,
      sourcePath: path.relative(ROOT_DIR, htmlPath).replaceAll(path.sep, '/'),
      preserveLocal: true,
    };
  } catch (error) {
    if (args.requireCompleteAssets) {
      await rm(htmlPath, { force: true });
      await rm(assetDir, { recursive: true, force: true });
    }
    throw error;
  } finally {
    await page.close();
  }
}

function slugToTemplateName(slug) {
  return slug
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .replace(/-+/g, '-');
}

async function extractEmailHtml(page) {
  const scripts = await page.locator('script').evaluateAll((nodes) =>
    nodes.map((node) => node.textContent ?? '').filter((text) => text.includes('self.__next_f.push')),
  );
  const chunks = [];
  for (const script of scripts) {
    try {
      vm.runInNewContext(
        script,
        {
          self: {
            __next_f: {
              push(value) {
                chunks.push(value);
              },
            },
          },
        },
        { timeout: 1000 },
      );
    } catch {
      // Ignore non-flight scripts or changed chunks; extraction below will fail if no HTML is found.
    }
  }
  const joined = chunks
    .map((chunk) => (Array.isArray(chunk) && typeof chunk[1] === 'string' ? chunk[1] : ''))
    .join('');
  const start = firstPresentIndex(joined, ['<!DOCTYPE', '<!doctype', '<html']);
  if (start < 0) {
    throw new Error('could not find raw email HTML in Really Good Emails flight payload');
  }
  const endHtml = joined.lastIndexOf('</html>');
  const end = endHtml > start ? endHtml + '</html>'.length : joined.length;
  const html = joined.slice(start, end).trim();
  if (!/<body[\s>]/i.test(html) && !/<table[\s>]/i.test(html)) {
    throw new Error('extracted payload does not look like email HTML');
  }
  return html;
}

function firstPresentIndex(text, needles) {
  let best = -1;
  for (const needle of needles) {
    const index = text.indexOf(needle);
    if (index >= 0 && (best < 0 || index < best)) best = index;
  }
  return best;
}

async function updateCatalog(catalogPath, entries, replaceProvider) {
  const current = (await readJson(catalogPath).catch(() => []))
    .filter((entry) => !(replaceProvider && entry.name?.startsWith(`${PROVIDER}-`)));
  const byName = new Map(current.map((entry) => [entry.name, entry]));
  for (const entry of entries) {
    byName.set(entry.name, entry);
  }
  const next = [...byName.values()].sort((left, right) => left.name.localeCompare(right.name));
  await mkdir(path.dirname(catalogPath), { recursive: true });
  await writeFile(catalogPath, `${JSON.stringify(next, null, 2)}\n`);
}

async function readJson(file) {
  return JSON.parse(await readFile(file, 'utf8'));
}

async function fileExists(file) {
  try {
    await readFile(file);
    return true;
  } catch {
    return false;
  }
}

function positiveInt(value, name) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${name} must be positive`);
  return parsed;
}

function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

main().catch((error) => {
  console.error(error?.stack ?? error?.message ?? String(error));
  process.exitCode = 1;
});
