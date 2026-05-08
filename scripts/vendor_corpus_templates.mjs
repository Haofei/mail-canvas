#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const CORPUS_DIR = path.join(ROOT_DIR, 'corpus');
const MANIFEST_PATH = path.join(CORPUS_DIR, 'manifest.json');
const CATALOG_PATH = path.join(CORPUS_DIR, 'catalog.json');
const FALLBACK_FONT_PATH = path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Regular.ttf');
const IGNORED_PROVIDERS = new Set(['README.md', 'manifest.json', 'catalog.json']);
let fallbackFontBytes = null;

function unavailableStylesheetPlaceholder() {
  return '/* mail-canvas: external stylesheet unavailable or empty during corpus vendoring. */\n';
}

async function main() {
  const manifest = JSON.parse(await readFile(MANIFEST_PATH, 'utf8'));
  const templates = manifest.templates ?? [];
  const localSources = await snapshotLocalSources(templates.filter(shouldPreserveLocalFixture));

  const providerDirs = new Set(templates.map((template) => template.provider));
  for (const provider of providerDirs) {
    if ([...templates].some((template) => template.provider === provider && shouldPreserveLocalFixture(template))) {
      continue;
    }
    await rm(path.join(CORPUS_DIR, provider), { recursive: true, force: true });
  }
  for (const entry of await readDirSafe(CORPUS_DIR)) {
    if (!providerDirs.has(entry) && !IGNORED_PROVIDERS.has(entry)) {
      await rm(path.join(CORPUS_DIR, entry), { recursive: true, force: true });
    }
  }

  const catalog = [];
  for (const template of templates) {
    if (shouldPreserveLocalFixture(template)) {
      const entry = {
        name: template.name,
        url: template.url,
        sourcePath: template.sourcePath,
      };
      if (template.preserveLocal) {
        entry.preserveLocal = true;
      }
      catalog.push(entry);
      console.log(`preserved ${template.name}`);
      continue;
    }

    const providerDir = path.join(CORPUS_DIR, template.provider);
    const htmlTarget = path.join(providerDir, `${template.name}.html`);
    const assetDir = path.join(providerDir, `${template.name}.assets`);
    await mkdir(providerDir, { recursive: true });
    await rm(htmlTarget, { force: true });
    await rm(assetDir, { recursive: true, force: true });
    await mkdir(assetDir, { recursive: true });

    const source = await loadTemplateSource(template, localSources.get(template.name) ?? null);
    const rewritten = await mirrorHtml(source.html, source.baseUrl, assetDir);
    await writeFile(htmlTarget, rewritten, 'utf8');

    catalog.push({
      name: template.name,
      url: template.url,
      sourcePath: path.relative(ROOT_DIR, htmlTarget).replaceAll(path.sep, '/'),
    });
    console.log(`vendored ${template.name}`);
  }

  await writeFile(CATALOG_PATH, `${JSON.stringify(catalog, null, 2)}\n`);
  console.log(CATALOG_PATH);
}

async function snapshotLocalSources(templates) {
  const sources = new Map();
  for (const template of templates) {
    if (!template.sourcePath) {
      continue;
    }
    const htmlPath = path.join(ROOT_DIR, template.sourcePath);
    try {
      const html = await readFile(htmlPath, 'utf8');
      const baseUrl =
        template.baseUrl ?? pathToFileURL(`${path.dirname(htmlPath)}${path.sep}`).href;
      sources.set(template.name, { html, baseUrl });
    } catch (error) {
      if (error?.code !== 'ENOENT') {
        throw error;
      }
    }
  }
  return sources;
}

async function loadTemplateSource(template, localSource = null) {
  if (localSource) {
    return localSource;
  }

  const response = await fetch(template.url);
  if (!response.ok) {
    throw new Error(`${template.url}: ${response.status} ${response.statusText}`);
  }
  return {
    html: await response.text(),
    baseUrl: new URL('.', template.url).href,
  };
}

function shouldPreserveLocalFixture(template) {
  if (template.preserveLocal && template.sourcePath) {
    return true;
  }
  return template.provider === 'beefree' && Boolean(template.sourcePath);
}

export async function mirrorHtml(html, baseUrl, assetDir, options = {}) {
  const cache = new Map();
  const strictAssets = Boolean(options.requireCompleteAssets ?? options.strictAssets);
  let rewritten = html;

  rewritten = await replaceAsync(
    rewritten,
    /<style\b[^>]*>([\s\S]*?)<\/style>/gi,
    async (match, cssText) =>
      match.replace(
        cssText,
        await mirrorCssText(cssText, baseUrl, assetDir, cache, {
          relativeMode: 'html',
          strictAssets,
        }),
      ),
  );

  rewritten = await replaceAsync(
    rewritten,
    /<link\b([^>]*?)href=(["'])([^"']+)\2([^>]*)>/gi,
    async (match, before, quote, href, after) => {
      const whole = `${before}${after}`;
      if (!/\brel=(["'])?stylesheet\1?/i.test(whole)) {
        return match;
      }
      const local = await mirrorUrlAsset(href, baseUrl, assetDir, cache, {
        css: true,
        strictAssets,
      });
      return match.replace(href, local.htmlPath);
    },
  );

  rewritten = await replaceAsync(
    rewritten,
    /\b(src|background|poster)=((["'])(.*?)\3|([^\s>]+))/gi,
    async (match, attr, _value, _quote, quotedValue, bareValue) => {
      const value = quotedValue ?? bareValue;
      const local = await mirrorUrlAsset(value, baseUrl, assetDir, cache, { strictAssets });
      return local ? match.replace(value, local.htmlPath) : match;
    },
  );

  rewritten = await replaceAsync(
    rewritten,
    /\bsrcset=(["'])(.*?)\1/gi,
    async (match, quote, srcset) => {
      const candidates = srcset
        .split(',')
        .map((entry) => entry.trim())
        .filter(Boolean);
      const rewrittenCandidates = [];
      for (const candidate of candidates) {
        const [urlPart, descriptor] = candidate.split(/\s+/, 2);
        const local = await mirrorUrlAsset(urlPart, baseUrl, assetDir, cache, { strictAssets });
        rewrittenCandidates.push(local ? `${local.htmlPath}${descriptor ? ` ${descriptor}` : ''}` : candidate);
      }
      return `srcset=${quote}${rewrittenCandidates.join(', ')}${quote}`;
    },
  );

  rewritten = await replaceAsync(
    rewritten,
    /\bstyle=(["'])([\s\S]*?)\1/gi,
    async (match, quote, styleText) => {
      const next = await mirrorCssText(styleText, baseUrl, assetDir, cache, {
        declarationsOnly: true,
        relativeMode: 'html',
        cssUrlQuote: quote === '"' ? "'" : '"',
        strictAssets,
      });
      return `style=${quote}${next}${quote}`;
    },
  );

  return rewritten;
}

async function mirrorCssText(cssText, sourceUrl, assetDir, cache, options = {}) {
  let rewritten = cssText;
  const pathKey = options.relativeMode === 'html' ? 'htmlPath' : 'cssPath';

  if (!options.declarationsOnly) {
    rewritten = await replaceAsync(
      rewritten,
      /@import\s+(?:url\(\s*)?(["']?)([^"'()\s;]+)\1\s*\)?([^;]*);/gi,
      async (match, _quote, importUrl, trailer) => {
        const local = await mirrorUrlAsset(importUrl, sourceUrl, assetDir, cache, {
          css: true,
          strictAssets: options.strictAssets,
        });
        return local ? `@import "${local[pathKey]}"${trailer};` : match;
      },
    );
  }

  rewritten = await replaceAsync(
    rewritten,
    /url\(\s*(["']?)(.*?)\1\s*\)/gi,
    async (match, _quote, rawUrl) => {
      const local = await mirrorUrlAsset(rawUrl, sourceUrl, assetDir, cache, {
        strictAssets: options.strictAssets,
      });
      const quote = options.cssUrlQuote ?? '"';
      return local ? `url(${quote}${local[pathKey]}${quote})` : match;
    },
  );

  return rewritten;
}

async function mirrorUrlAsset(rawUrl, sourceUrl, assetDir, cache, options = {}) {
  const trimmed = decodeHtmlEntities(rawUrl.trim());
  if (!shouldMirror(trimmed)) {
    return null;
  }

  const resolved = resolveUrl(trimmed, sourceUrl);
  if (!resolved) {
    return null;
  }
  if (cache.has(resolved)) {
    return cache.get(resolved);
  }

  const ext = extensionFor(resolved, '', options.css);
  const fileName = `${hashId(resolved)}${ext}`;
  const assetPath = path.join(assetDir, fileName);
  const relativeForHtml = `${path.basename(assetDir)}/${fileName}`.replaceAll(path.sep, '/');
  const relativeForCss = fileName;
  const entry = { htmlPath: relativeForHtml, cssPath: relativeForCss };
  cache.set(resolved, entry);

  if (resolved.startsWith('file://')) {
    try {
      const localPath = fileURLToPath(resolved);
      if (options.css || ext === '.css') {
        const cssText = await readFile(localPath, 'utf8');
        await writeMirroredCssAsset(assetPath, cssText, resolved, assetDir, cache, {
          strictAssets: options.strictAssets,
        });
      } else {
        const bytes = await readFile(localPath);
        await writeFile(assetPath, bytes);
      }
      return entry;
    } catch (error) {
      const message = `${resolved}: ${error.message}`;
      if (options.strictAssets) {
        throw new Error(message);
      }
      console.warn(`warning: ${message}`);
      await writePlaceholderAsset(assetPath, ext, options);
      return entry;
    }
  }

  let response;
  try {
    response = await fetch(resolved);
  } catch (error) {
    const message = `${resolved}: ${error.message}`;
    if (options.strictAssets) {
      throw new Error(message);
    }
    console.warn(`warning: ${message}`);
    await writePlaceholderAsset(assetPath, ext, options);
    return entry;
  }

  if (!response.ok) {
    const message = `${resolved}: ${response.status} ${response.statusText}`;
    if (options.strictAssets) {
      throw new Error(message);
    }
    console.warn(`warning: ${message}`);
    await writePlaceholderAsset(assetPath, ext, options);
    return entry;
  }

  const contentType = response.headers.get('content-type') ?? '';

  if (options.css || contentType.includes('text/css')) {
    const cssText = await response.text();
    await writeMirroredCssAsset(assetPath, cssText, resolved, assetDir, cache, {
      strictAssets: options.strictAssets,
    });
  } else {
    const bytes = Buffer.from(await response.arrayBuffer());
    await writeFile(assetPath, bytes);
  }

  return entry;
}

async function writeMirroredCssAsset(assetPath, cssText, sourceUrl, assetDir, cache, options = {}) {
  if (cssText.trim().length === 0) {
    await writeFile(assetPath, unavailableStylesheetPlaceholder(), 'utf8');
    return;
  }
  const rewrittenCss = await mirrorCssText(cssText, sourceUrl, assetDir, cache, {
    strictAssets: options.strictAssets,
  });
  await writeFile(
    assetPath,
    rewrittenCss.trim().length === 0 ? unavailableStylesheetPlaceholder() : rewrittenCss,
    'utf8',
  );
}

async function writePlaceholderAsset(assetPath, ext, options = {}) {
  if (options.css || ext === '.css') {
    await writeFile(assetPath, unavailableStylesheetPlaceholder(), 'utf8');
    return;
  }
  if (ext === '.svg') {
    await writeFile(
      assetPath,
      '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>',
      'utf8',
    );
    return;
  }
  if (['.woff', '.woff2', '.ttf', '.otf'].includes(ext)) {
    if (!fallbackFontBytes) {
      fallbackFontBytes = await readFile(FALLBACK_FONT_PATH);
    }
    await writeFile(assetPath, fallbackFontBytes);
    return;
  }
  await writeFile(assetPath, tinyTransparentPng());
}

function tinyTransparentPng() {
  return Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+XkO8AAAAASUVORK5CYII=',
    'base64',
  );
}

function decodeHtmlEntities(value) {
  return value
    .replaceAll('&amp;', '&')
    .replaceAll('&quot;', '"')
    .replaceAll('&#39;', "'")
    .replaceAll('&lt;', '<')
    .replaceAll('&gt;', '>');
}

function shouldMirror(value) {
  if (!value) return false;
  if (value.startsWith('data:')) return false;
  if (value.startsWith('mailto:')) return false;
  if (value.startsWith('tel:')) return false;
  if (value.startsWith('javascript:')) return false;
  if (value.startsWith('#')) return false;
  return true;
}

function resolveUrl(value, baseUrl) {
  try {
    if (value.startsWith('//')) {
      return new URL(`https:${value}`).href;
    }
    return new URL(value, baseUrl).href;
  } catch {
    return null;
  }
}

function extensionFor(resolvedUrl, contentType, forceCss = false) {
  if (forceCss || contentType.includes('text/css')) {
    return '.css';
  }
  const urlPath = new URL(resolvedUrl).pathname;
  const parsed = path.extname(urlPath);
  if (parsed) {
    return parsed.toLowerCase();
  }
  if (contentType.includes('svg')) return '.svg';
  if (contentType.includes('png')) return '.png';
  if (contentType.includes('jpeg')) return '.jpg';
  if (contentType.includes('gif')) return '.gif';
  if (contentType.includes('woff2')) return '.woff2';
  if (contentType.includes('woff')) return '.woff';
  if (contentType.includes('ttf')) return '.ttf';
  if (contentType.includes('otf')) return '.otf';
  return '.bin';
}

function hashId(value) {
  return createHash('sha1').update(value).digest('hex').slice(0, 16);
}

async function replaceAsync(text, pattern, replacer) {
  const matches = [...text.matchAll(pattern)];
  if (matches.length === 0) {
    return text;
  }
  let result = '';
  let lastIndex = 0;
  for (const match of matches) {
    const replacement = await replacer(...match);
    result += text.slice(lastIndex, match.index) + replacement;
    lastIndex = match.index + match[0].length;
  }
  result += text.slice(lastIndex);
  return result;
}

async function readDirSafe(dir) {
  try {
    return await (await import('node:fs/promises')).readdir(dir);
  } catch {
    return [];
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
