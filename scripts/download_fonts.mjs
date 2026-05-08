#!/usr/bin/env node

import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FONT_ROOT = path.join(ROOT_DIR, 'fixtures', 'fonts');
const GOOGLE_FONT_DIR = path.join(FONT_ROOT, 'google');
const MANIFEST_PATH = path.join(FONT_ROOT, 'fixture-fonts.json');

const GOOGLE_FONTS = [
  'Roboto',
  'Open Sans',
  'Lato',
  'Montserrat',
  'Poppins',
  'Inter',
  'Source Sans 3',
  'Merriweather',
  'Nunito Sans',
];

const EXISTING_FIXTURES = [
  {
    family: 'Arimo',
    weight: 400,
    style: 'normal',
    path: 'Arimo-Regular.ttf',
    aliases: [
      'Arial',
      'Arial Nova',
      'Avenir',
      'Avenir Next',
      'Avenir Next LT Pro',
      'Corbel',
      'Helvetica',
      'Helvetica Neue',
      'Lucida Grande',
      'Lucida Sans',
      'Lucida Sans Unicode',
      'Nimbus Sans',
      'Segoe UI',
      'Tahoma',
      'Trebuchet MS',
      'Verdana',
    ],
    license: 'Apache-2.0',
  },
  {
    family: 'Arimo',
    weight: 700,
    style: 'normal',
    path: 'Arimo-Bold.ttf',
    aliases: [
      'Arial',
      'Arial Nova',
      'Avenir',
      'Avenir Next',
      'Avenir Next LT Pro',
      'Corbel',
      'Helvetica',
      'Helvetica Neue',
      'Lucida Grande',
      'Lucida Sans',
      'Lucida Sans Unicode',
      'Nimbus Sans',
      'Segoe UI',
      'Tahoma',
      'Trebuchet MS',
      'Verdana',
    ],
    license: 'Apache-2.0',
  },
  {
    family: 'Tinos',
    weight: 400,
    style: 'normal',
    path: 'Tinos-Regular.ttf',
    aliases: ['Cambria', 'Georgia', 'Palatino', 'Palatino Linotype', 'Times', 'Times New Roman'],
    license: 'Apache-2.0',
  },
  {
    family: 'Tinos',
    weight: 700,
    style: 'normal',
    path: 'Tinos-Bold.ttf',
    aliases: ['Cambria', 'Georgia', 'Palatino', 'Palatino Linotype', 'Times', 'Times New Roman'],
    license: 'Apache-2.0',
  },
  {
    family: 'Noto Sans',
    weight: 400,
    style: 'normal',
    path: 'NotoSans-Regular.ttf',
    aliases: [],
    license: 'OFL-1.1',
  },
  {
    family: 'Noto Sans',
    weight: 700,
    style: 'normal',
    path: 'NotoSans-Bold.ttf',
    aliases: [],
    license: 'OFL-1.1',
  },
  {
    family: 'Noto Sans Math',
    weight: 400,
    style: 'normal',
    path: 'NotoSansMath-Regular.ttf',
    aliases: ['Apple Symbols', 'Segoe UI Symbol'],
    license: 'OFL-1.1',
  },
  {
    family: 'Noto Color Emoji',
    weight: 400,
    style: 'normal',
    path: 'NotoColorEmoji.ttf',
    aliases: ['Apple Color Emoji', 'Segoe UI Emoji'],
    license: 'OFL-1.1',
    source: 'googlefonts/noto-emoji default color emoji font',
  },
];

async function main() {
  await mkdir(GOOGLE_FONT_DIR, { recursive: true });
  const fonts = [...EXISTING_FIXTURES];

  for (const family of GOOGLE_FONTS) {
    const css = await fetchGoogleFontCss(family);
    const faces = parseFontFaces(css);
    if (faces.length === 0) {
      throw new Error(`no @font-face blocks found for ${family}`);
    }
    for (const face of selectLatinFaces(faces)) {
      const extension = extensionFromUrl(face.url);
      const fileName = `${slug(family)}-${face.weight}.${extension}`;
      const outputPath = path.join(GOOGLE_FONT_DIR, fileName);
      const bytes = await downloadBytes(face.url);
      await writeFile(outputPath, bytes);
      fonts.push({
        family: face.family,
        weight: Number.parseInt(face.weight, 10),
        style: face.style,
        path: path.relative(FONT_ROOT, outputPath).replaceAll(path.sep, '/'),
        aliases: [],
        license: family === 'Roboto' ? 'Apache-2.0' : 'OFL-1.1',
        unicodeRange: face.unicodeRange,
        source: 'Google Fonts CSS2 API latin subset',
      });
      console.log(`downloaded ${face.family} ${face.weight} -> ${outputPath}`);
    }
  }

  await writeFile(
    MANIFEST_PATH,
    `${JSON.stringify(
      {
        generatedAt: new Date().toISOString(),
        note: 'Deterministic fixture fonts for CI/reference rendering. Do not add template-specific font hacks here.',
        fonts,
      },
      null,
      2,
    )}\n`,
  );
  await writeLicenseNote();
  console.log(MANIFEST_PATH);
}

async function fetchGoogleFontCss(family) {
  const familyQuery = encodeURIComponent(family).replaceAll('%20', '+');
  const url = `https://fonts.googleapis.com/css2?family=${familyQuery}:wght@400;700&display=swap`;
  const response = await fetch(url, {
    headers: {
      'user-agent':
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/124 MailCanvasFontVendor/1.0',
    },
  });
  if (!response.ok) {
    throw new Error(`${url}: ${response.status} ${response.statusText}`);
  }
  return response.text();
}

function parseFontFaces(css) {
  const faces = [];
  for (const match of css.matchAll(/@font-face\s*\{([\s\S]*?)\}/g)) {
    const block = match[1];
    const family = cssValue(block, 'font-family')?.replaceAll(/^['"]|['"]$/g, '');
    const style = cssValue(block, 'font-style') ?? 'normal';
    const weight = cssValue(block, 'font-weight') ?? '400';
    const unicodeRange = cssValue(block, 'unicode-range') ?? '';
    const url = block.match(/url\(([^)]+)\)/)?.[1]?.replaceAll(/^['"]|['"]$/g, '');
    if (family && weight && url) {
      faces.push({ family, style, weight, unicodeRange, url });
    }
  }
  return faces;
}

function selectLatinFaces(faces) {
  const selected = new Map();
  for (const face of faces) {
    const key = `${face.family}:${face.weight}:${face.style}`;
    const previous = selected.get(key);
    if (!previous || latinScore(face.unicodeRange) > latinScore(previous.unicodeRange)) {
      selected.set(key, face);
    }
  }
  return [...selected.values()];
}

function latinScore(unicodeRange) {
  const value = unicodeRange.toUpperCase();
  if (value.includes('U+0000-00FF')) return 100;
  if (value.includes('U+0100-02BA')) return 50;
  return 0;
}

function cssValue(block, property) {
  return block.match(new RegExp(`${property}\\s*:\\s*([^;]+)`, 'i'))?.[1]?.trim() ?? null;
}

async function downloadBytes(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url}: ${response.status} ${response.statusText}`);
  }
  return Buffer.from(await response.arrayBuffer());
}

function extensionFromUrl(url) {
  const pathname = new URL(url).pathname;
  return path.extname(pathname).slice(1) || 'ttf';
}

function slug(value) {
  return value.replaceAll(/[^a-z0-9]+/gi, '-').replaceAll(/^-|-$/g, '');
}

async function writeLicenseNote() {
  const existing = await readFile(path.join(FONT_ROOT, 'LICENSE-fixture-fonts.txt'), 'utf8').catch(
    () => null,
  );
  if (existing) {
    return;
  }
  await writeFile(
    path.join(FONT_ROOT, 'LICENSE-fixture-fonts.txt'),
    [
      'Fixture font license note',
      '',
      'These fonts are included only to make MailCanvas regression tests deterministic.',
      'Arimo and Tinos are Apache-2.0 licensed; Noto fonts are OFL-1.1 licensed.',
      'Additional Google Fonts downloaded by scripts/download_fonts.mjs use the license',
      'recorded in fixture-fonts.json. Keep this bundle limited to broadly used',
      'open-source email/web fonts, not template-specific overrides.',
      '',
    ].join('\n'),
  );
}

main().catch((error) => {
  console.error(`Error: ${error.message}`);
  process.exit(1);
});
