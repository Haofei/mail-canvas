#!/usr/bin/env node

import { mkdir, readdir, readFile, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';

import { createCanvas, GlobalFonts } from '@napi-rs/canvas';
import { clearCache, layoutWithLines, prepareWithSegments } from '@chenglou/pretext';

const FONT_EXTENSIONS = new Set(['.ttf', '.otf', '.woff', '.woff2']);

if (typeof globalThis.OffscreenCanvas === 'undefined') {
  globalThis.OffscreenCanvas = class OffscreenCanvasPolyfill {
    constructor(width, height) {
      this._canvas = createCanvas(width, height);
    }

    getContext(kind) {
      return this._canvas.getContext(kind);
    }
  };
}

function parseArgs(argv) {
  const args = {
    layoutJson: null,
    out: null,
    reportJson: null,
    fontFiles: [],
    fontDirs: [],
    minWidth: 220,
    minTextLength: 40,
    minHeadingLength: 20,
    strict: false,
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
      case '--layout-json':
        args.layoutJson = path.resolve(next());
        break;
      case '--out':
        args.out = path.resolve(next());
        break;
      case '--report-json':
        args.reportJson = path.resolve(next());
        break;
      case '--font-file':
        args.fontFiles.push(path.resolve(next()));
        break;
      case '--font-dir':
        args.fontDirs.push(path.resolve(next()));
        break;
      case '--min-width':
        args.minWidth = Number.parseFloat(next());
        break;
      case '--min-text-length':
        args.minTextLength = Number.parseInt(next(), 10);
        break;
      case '--min-heading-length':
        args.minHeadingLength = Number.parseInt(next(), 10);
        break;
      case '--strict':
        args.strict = true;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!args.layoutJson || !args.out) {
    throw new Error('pass --layout-json pass1.layout.json --out text-hints.json');
  }
  if (!Number.isFinite(args.minWidth) || args.minWidth <= 0) {
    throw new Error('--min-width must be a positive number');
  }
  if (!Number.isInteger(args.minTextLength) || args.minTextLength < 0) {
    throw new Error('--min-text-length must be a non-negative integer');
  }
  if (!Number.isInteger(args.minHeadingLength) || args.minHeadingLength < 0) {
    throw new Error('--min-heading-length must be a non-negative integer');
  }
  return args;
}

function normalizeText(text) {
  return (text || '').replace(/\s+/g, ' ').trim();
}

function isAllCapsText(text) {
  const letters = text.replace(/[^A-Za-z]+/g, '');
  return letters.length > 0 && letters === letters.toUpperCase();
}

function pretextOptionsFromRust(style) {
  return {
    whiteSpace: style.wrap === 'none' ? 'pre' : 'normal',
    wordBreak: 'normal',
    letterSpacing: Number.isFinite(style.letter_spacing) ? style.letter_spacing : 0,
  };
}

function canvasFontShorthandFromRust(style) {
  const fontStyle = style.font_style || 'normal';
  const fontWeight = style.font_weight || 400;
  const fontSize = `${style.font_size || 16}px`;
  const fontFamily = style.font_family || 'sans-serif';
  return `${fontStyle} ${fontWeight} ${fontSize} ${fontFamily}`;
}

function shouldApplyHint(layout, options) {
  const text = normalizeText(layout.text);
  const width = Number(layout.rect?.width || 0);
  const fontSize = Number(layout.style?.font_size || 0);
  if (!layout.text_id) {
    return { ok: false, reason: 'missingTextId' };
  }
  if (!text) {
    return { ok: false, reason: 'emptyText' };
  }
  if ((layout.style?.wrap || '').toLowerCase() === 'none') {
    return { ok: false, reason: 'wrapNone' };
  }
  if (width < options.minWidth) {
    return { ok: false, reason: 'tooNarrow' };
  }
  if (isAllCapsText(text)) {
    return { ok: false, reason: 'allCaps' };
  }
  const minLength = fontSize >= 24 ? options.minHeadingLength : options.minTextLength;
  if (text.length < minLength) {
    return { ok: false, reason: 'notEligible' };
  }
  return { ok: true, text, width, fontSize };
}

async function collectFontPaths(fontFiles, fontDirs) {
  const files = [...fontFiles];
  for (const dir of fontDirs) {
    for (const file of await walkFontFiles(dir)) {
      files.push(file);
    }
  }
  return [...new Set(files)];
}

async function walkFontFiles(root) {
  const results = [];
  const rootStat = await stat(root);
  if (!rootStat.isDirectory()) {
    return results;
  }
  async function visit(dir) {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        await visit(fullPath);
        continue;
      }
      if (!entry.isFile()) {
        continue;
      }
      if (FONT_EXTENSIONS.has(path.extname(entry.name).toLowerCase())) {
        results.push(fullPath);
      }
    }
  }
  await visit(root);
  return results;
}

function registerFonts(fontPaths) {
  const registered = [];
  for (const fontPath of fontPaths) {
    const key = GlobalFonts.registerFromPath(fontPath);
    if (key) {
      registered.push(fontPath);
    }
  }
  return registered;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const layoutJson = JSON.parse(await readFile(args.layoutJson, 'utf8'));
  const textLayouts = Array.isArray(layoutJson.text_layouts) ? layoutJson.text_layouts : [];
  const matchedCount = textLayouts.filter((layout) => Boolean(layout?.text_id)).length;
  if (textLayouts.length > 0 && matchedCount === 0) {
    const message = 'No text_id found in layout JSON. Run scripts/annotate_text_candidates.mjs before pass 1.';
    if (args.strict) {
      throw new Error(message);
    }
    console.error(`warning: ${message}`);
  }
  const fontPaths = await collectFontPaths(args.fontFiles, args.fontDirs);
  const registeredFonts = registerFonts(fontPaths);

  clearCache();

  const textHints = [];
  const skipped = {
    missingTextId: 0,
    emptyText: 0,
    wrapNone: 0,
    tooNarrow: 0,
    allCaps: 0,
    notEligible: 0,
    singleLine: 0,
  };
  const changed = [];
  let eligible = 0;

  for (const layout of textLayouts) {
    const verdict = shouldApplyHint(layout, args);
    if (!verdict.ok) {
      skipped[verdict.reason] = (skipped[verdict.reason] ?? 0) + 1;
      continue;
    }
    eligible += 1;
    const style = layout.style || {};
    const lineHeight =
      Number(style.line_height) ||
      Number(style.font_size || 16) * 1.2;
    const prepared = prepareWithSegments(
      verdict.text,
      canvasFontShorthandFromRust(style),
      pretextOptionsFromRust(style),
    );
    const result = layoutWithLines(prepared, verdict.width, lineHeight);
    if (result.lineCount <= 1) {
      skipped.singleLine += 1;
      continue;
    }

    textHints.push({
      text_id: String(layout.text_id),
      text: verdict.text,
      lines: result.lines.map((line) => line.text),
      measured_height: Math.round(result.height * 1000) / 1000,
    });
    changed.push({
      textId: String(layout.text_id),
      text: verdict.text.slice(0, 80),
      width: Math.round(verdict.width * 100) / 100,
      fontSize: Math.round((Number(style.font_size || 0)) * 100) / 100,
      lineHeight: Math.round(lineHeight * 100) / 100,
      lines: result.lineCount,
      height: Math.round(result.height * 100) / 100,
      rustHeight: Math.round((Number(layout.rect?.height || 0)) * 100) / 100,
    });
  }

  await mkdir(path.dirname(args.out), { recursive: true });
  await writeFile(args.out, `${JSON.stringify(textHints, null, 2)}\n`, 'utf8');

  const report = {
    input: args.layoutJson,
    output: args.out,
    scope: 'plain_leaf_block_text_only',
    candidates: textLayouts.length,
    matched: matchedCount,
    eligible,
    applied: textHints.length,
    skipped,
    registeredFonts,
    changed: changed.slice(0, 20),
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
