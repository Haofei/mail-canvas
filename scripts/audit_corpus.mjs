#!/usr/bin/env node

import { stat, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { TEMPLATE_CORPUS } from './templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function parseArgs(argv) {
  const args = {
    json: false,
    providers: [],
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
      case '--json':
        args.json = true;
        break;
      case '--provider':
        args.providers.push(next());
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const providerFilter = args.providers.length > 0 ? new Set(args.providers) : null;
  const results = [];

  for (const template of TEMPLATE_CORPUS) {
    if (providerFilter && !providerFilter.has(template.provider)) {
      continue;
    }
    if (!template.sourcePath) {
      continue;
    }
    const htmlPath = path.resolve(ROOT_DIR, template.sourcePath);
    const html = await readFile(htmlPath, 'utf8');
    const invalidStyleUrlQuotes = countInvalidStyleUrlQuotes(html);
    const emptyLinkedStylesheets = await emptyStylesheetLinks(html, htmlPath);
    if (invalidStyleUrlQuotes === 0 && emptyLinkedStylesheets.length === 0) {
      continue;
    }
    results.push({
      name: template.name,
      provider: template.provider,
      supportTier: template.supportTier,
      invalidStyleUrlQuotes,
      emptyLinkedStylesheets,
    });
  }

  if (args.json) {
    console.log(JSON.stringify({ issues: results }, null, 2));
    return;
  }

  if (results.length === 0) {
    console.log('No corpus issues found.');
    return;
  }

  for (const result of results) {
    const parts = [];
    if (result.invalidStyleUrlQuotes > 0) {
      parts.push(`invalid style url quotes: ${result.invalidStyleUrlQuotes}`);
    }
    if (result.emptyLinkedStylesheets.length > 0) {
      parts.push(`empty linked CSS: ${result.emptyLinkedStylesheets.length}`);
    }
    console.log(`${result.name}\t${result.provider}\t${parts.join('; ')}`);
  }
}

function countInvalidStyleUrlQuotes(html) {
  const matches = html.match(/style\s*=\s*"[^"]*url\("/gi);
  return matches?.length ?? 0;
}

async function emptyStylesheetLinks(html, htmlPath) {
  const links = [];
  const linkPattern = /<link\b[^>]*\bhref\s*=\s*["']([^"']+\.css(?:[?#][^"']*)?)["'][^>]*>/gi;
  for (const match of html.matchAll(linkPattern)) {
    if (!/\brel\s*=\s*["']?stylesheet\b/i.test(match[0])) {
      continue;
    }
    const href = match[1];
    if (/^[a-z][a-z0-9+.-]*:/i.test(href)) {
      continue;
    }
    const pathname = href.split(/[?#]/, 1)[0];
    const cssPath = path.resolve(path.dirname(htmlPath), pathname);
    try {
      const cssStat = await stat(cssPath);
      if (cssStat.size === 0) {
        links.push(path.relative(process.cwd(), cssPath));
      }
    } catch {
      links.push(path.relative(process.cwd(), cssPath));
    }
  }
  return links;
}

main().catch((error) => {
  console.error(`Error: ${error.message}`);
  process.exit(1);
});
