#!/usr/bin/env node

import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { parseHTML } from 'linkedom';

const TEXT_ID_ATTR = 'data-mc-text-id';
const TARGET_TAGS = new Set([
  'DIV',
  'H1',
  'H2',
  'H3',
  'H4',
  'H5',
  'H6',
  'P',
]);

function parseArgs(argv) {
  const args = {
    html: null,
    out: null,
    reportJson: null,
    baseUrl: null,
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
      case '--html':
        args.html = path.resolve(next());
        break;
      case '--out':
        args.out = path.resolve(next());
        break;
      case '--report-json':
        args.reportJson = path.resolve(next());
        break;
      case '--base-url':
        args.baseUrl = next();
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!args.html || !args.out) {
    throw new Error('pass --html input.html --out annotated.html');
  }
  return args;
}

function normalizeText(text) {
  return (text || '').replace(/\s+/g, ' ').trim();
}

function collectPretextCandidates(document) {
  const body = document.body;
  if (!body) {
    return [];
  }
  const candidates = [];
  for (const element of body.querySelectorAll('*')) {
    if (!TARGET_TAGS.has(element.tagName)) {
      continue;
    }
    if (element.children.length > 0) {
      continue;
    }
    if (!normalizeText(element.textContent)) {
      continue;
    }
    if (element.querySelector('img,table,svg,ul,ol,li')) {
      continue;
    }
    candidates.push(element);
  }
  return candidates;
}

function ensureBaseHref(document, baseUrl) {
  if (!baseUrl) {
    return;
  }
  let head = document.head;
  if (!head) {
    head = document.createElement('head');
    document.documentElement.insertBefore(head, document.body || null);
  }
  let base = head.querySelector('base');
  if (!base) {
    base = document.createElement('base');
    head.prepend(base);
  }
  base.setAttribute('href', baseUrl);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const html = await readFile(args.html, 'utf8');
  const { document } = parseHTML(html);
  const defaultBaseUrl = pathToFileURL(path.dirname(args.html) + path.sep).toString();
  const baseUrl = args.baseUrl || defaultBaseUrl;
  ensureBaseHref(document, baseUrl);
  const candidates = collectPretextCandidates(document);

  let nextId = 1;
  for (const element of candidates) {
    element.setAttribute(TEXT_ID_ATTR, String(nextId));
    nextId += 1;
  }

  const annotated = `<!doctype html>\n${document.documentElement.outerHTML}\n`;
  await mkdir(path.dirname(args.out), { recursive: true });
  await writeFile(args.out, annotated, 'utf8');

  const report = {
    input: args.html,
    output: args.out,
    baseUrl,
    scope: 'plain_leaf_block_text_only',
    candidateCount: candidates.length,
  };
  if (args.reportJson) {
    await mkdir(path.dirname(args.reportJson), { recursive: true });
    await writeFile(args.reportJson, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
  }
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
