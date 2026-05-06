#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function parseArgs(argv) {
  const args = {
    sourceDir: null,
    provider: null,
    prefix: '',
    only: [],
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
      case '--source-dir':
        args.sourceDir = path.resolve(next());
        break;
      case '--provider':
        args.provider = next();
        break;
      case '--prefix':
        args.prefix = next();
        break;
      case '--only':
        args.only.push(next());
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!args.sourceDir) {
    throw new Error('missing required --source-dir');
  }
  if (!args.provider) {
    throw new Error('missing required --provider');
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const corpusDir = path.join(ROOT_DIR, 'corpus', args.provider);
  const entries = (await readdir(args.sourceDir))
    .filter((entry) => entry.endsWith('.html'))
    .filter((entry) => {
      if (args.only.length === 0) {
        return true;
      }
      return args.only.includes(entry.replace(/\.html$/i, ''));
    })
    .sort();

  for (const entry of entries) {
    const sourcePath = path.join(args.sourceDir, entry);
    const sourceHtml = await readFile(sourcePath, 'utf8');
    const slug = entry.replace(/\.html$/i, '');
    const targetName = `${args.prefix}${slug}`;
    const targetHtmlPath = path.join(corpusDir, `${targetName}.html`);
    const assetDir = path.join(corpusDir, `${targetName}.assets`);
    const baseUrl = `file://${path.dirname(sourcePath)}/`;

    await mkdir(corpusDir, { recursive: true });
    await rm(assetDir, { recursive: true, force: true });
    await mkdir(assetDir, { recursive: true });

    const rewritten = await mirrorHtml(sourceHtml, baseUrl, assetDir);
    await writeFile(targetHtmlPath, rewritten, 'utf8');
    console.log(`refreshed ${targetName}`);
  }
}

async function mirrorHtml(html, baseUrl, assetDir) {
  const cache = new Map();
  let rewritten = html;

  rewritten = await replaceAsync(
    rewritten,
    /<style\b[^>]*>([\s\S]*?)<\/style>/gi,
    async (match, cssText) =>
      match.replace(
        cssText,
        await mirrorCssText(cssText, baseUrl, assetDir, cache, { relativeMode: 'html' }),
      ),
  );

  rewritten = await replaceAsync(
    rewritten,
    /<link\b([^>]*?)href=(["'])([^"']+)\2([^>]*)>/gi,
    async (match, before, _quote, href, after) => {
      const whole = `${before}${after}`;
      if (!/\brel=(["'])?stylesheet\1?/i.test(whole)) {
        return match;
      }
      const local = await mirrorUrlAsset(href, baseUrl, assetDir, cache, { css: true });
      return local ? match.replace(href, local.htmlPath) : match;
    },
  );

  rewritten = await replaceAsync(
    rewritten,
    /\b(src|background|poster)=((["'])(.*?)\3|([^\s>]+))/gi,
    async (match, _attr, _value, _quote, quotedValue, bareValue) => {
      const value = quotedValue ?? bareValue;
      const local = await mirrorUrlAsset(value, baseUrl, assetDir, cache);
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
        const local = await mirrorUrlAsset(urlPart, baseUrl, assetDir, cache);
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
        const local = await mirrorUrlAsset(importUrl, sourceUrl, assetDir, cache, { css: true });
        return local ? `@import url("${local[pathKey]}")${trailer};` : match;
      },
    );
  }

  rewritten = await replaceAsync(
    rewritten,
    /url\(\s*(["']?)(.*?)\1\s*\)/gi,
    async (match, _quote, rawUrl) => {
      const local = await mirrorUrlAsset(rawUrl, sourceUrl, assetDir, cache);
      return local ? `url("${local[pathKey]}")` : match;
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
  const entry = { htmlPath: relativeForHtml, cssPath: fileName };
  cache.set(resolved, entry);

  const response = await fetch(resolved);
  if (!response.ok) {
    throw new Error(`${resolved}: ${response.status} ${response.statusText}`);
  }

  const contentType = response.headers.get('content-type') ?? '';
  if (options.css || contentType.includes('text/css')) {
    const cssText = await response.text();
    const rewrittenCss = await mirrorCssText(cssText, resolved, assetDir, cache);
    await writeFile(assetPath, rewrittenCss, 'utf8');
  } else {
    const bytes = Buffer.from(await response.arrayBuffer());
    await writeFile(assetPath, bytes);
  }

  return entry;
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

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
