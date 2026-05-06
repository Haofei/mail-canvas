#!/usr/bin/env node

import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

function parseArgs(argv) {
  const args = { browser: null, rust: null, out: null };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`missing value for ${arg}`);
      return argv[index];
    };
    switch (arg) {
      case '--browser':
        args.browser = path.resolve(next());
        break;
      case '--rust':
        args.rust = path.resolve(next());
        break;
      case '--out':
        args.out = path.resolve(next());
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  if (!args.browser || !args.rust) {
    throw new Error('pass --browser chrome-layout.json --rust mail-canvas-layout.json');
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const browser = JSON.parse(await readFile(args.browser, 'utf8'));
  const rust = JSON.parse(await readFile(args.rust, 'utf8'));
  const browserNodes = normalizeBrowserNodes(browser);
  const rustNodes = normalizeRustNodes(rust.tree ?? rust);
  const comparisons = pairNodes(browserNodes, rustNodes);
  const ranked = comparisons
    .map(scoreNodePair)
    .sort((left, right) => right.score - left.score);
  const firstBad = ranked.find((item) => item.score > 20) ?? ranked[0] ?? null;
  const summary = {
    browserCount: browserNodes.length,
    rustCount: rustNodes.length,
    matchedCount: ranked.length,
    firstBad,
    worst: ranked.slice(0, 20),
  };
  const json = `${JSON.stringify(summary, null, 2)}\n`;
  if (args.out) {
    await writeFile(args.out, json);
    console.log(args.out);
  } else {
    process.stdout.write(json);
  }
}

function normalizeBrowserNodes(browser) {
  return (browser.selected ?? [])
    .filter((node) => node.rect?.width > 0 || node.rect?.height > 0)
    .map((node) => ({
      key: stableKey(node.tag, node.id, node.className, node.text),
      tag: node.tag,
      id: node.id || null,
      className: node.className || null,
      text: node.text || '',
      rect: {
        x: node.rect.left,
        y: node.rect.top,
        width: node.rect.width,
        height: node.rect.height,
      },
    }));
}

function normalizeRustNodes(node) {
  const items = [];
  visit(node);
  return items.filter((item) => item.rect.width > 0 || item.rect.height > 0);

  function visit(current) {
    if (!current) return;
    items.push({
      key: stableKey(current.tag, current.id, current.class_name, current.text),
      tag: current.tag,
      id: current.id ?? null,
      className: current.class_name ?? null,
      text: current.text ?? '',
      rect: current.rect,
    });
    for (const child of current.children ?? []) {
      visit(child);
    }
  }
}

function stableKey(tag, id, className, text) {
  return [
    tag || '',
    id || '',
    (className || '').trim().replace(/\s+/g, '.'),
    (text || '').trim().replace(/\s+/g, ' ').slice(0, 40),
  ].join('|');
}

function pairNodes(browserNodes, rustNodes) {
  const rustBuckets = new Map();
  for (const node of rustNodes) {
    const bucket = rustBuckets.get(node.key) ?? [];
    bucket.push(node);
    rustBuckets.set(node.key, bucket);
  }
  const pairs = [];
  for (const browserNode of browserNodes) {
    const bucket = rustBuckets.get(browserNode.key);
    if (!bucket || bucket.length === 0) continue;
    pairs.push({ browser: browserNode, rust: bucket.shift() });
  }
  return pairs;
}

function scoreNodePair(pair) {
  const dx = Math.abs(pair.browser.rect.x - pair.rust.rect.x);
  const dy = Math.abs(pair.browser.rect.y - pair.rust.rect.y);
  const dw = Math.abs(pair.browser.rect.width - pair.rust.rect.width);
  const dh = Math.abs(pair.browser.rect.height - pair.rust.rect.height);
  return {
    tag: pair.browser.tag,
    id: pair.browser.id,
    className: pair.browser.className,
    text: pair.browser.text,
    browserRect: pair.browser.rect,
    rustRect: pair.rust.rect,
    delta: { dx, dy, dw, dh },
    score: round(dx + dy + dw + dh),
  };
}

function round(value) {
  return Math.round(value * 1000) / 1000;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
