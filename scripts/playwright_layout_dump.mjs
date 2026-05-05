#!/usr/bin/env node

import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { chromium } from 'playwright';
import { TEMPLATES } from './templates.mjs';

const DEFAULT_WIDTH = 600;
const DEFAULT_TIMEOUT_MS = 30000;
const DEFAULT_SELECTOR = 'body, table, tr, td, div, p, h1, h2, h3, span, a, img';

function parseArgs(argv) {
  const args = {
    template: null,
    html: null,
    baseUrl: null,
    width: DEFAULT_WIDTH,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    selector: DEFAULT_SELECTOR,
    y: [],
    out: null,
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
      case '--template':
        args.template = next();
        break;
      case '--html':
        args.html = path.resolve(next());
        break;
      case '--base-url':
        args.baseUrl = next();
        break;
      case '--width':
        args.width = Number.parseInt(next(), 10);
        break;
      case '--timeout-ms':
        args.timeoutMs = Number.parseInt(next(), 10);
        break;
      case '--selector':
        args.selector = next();
        break;
      case '--y':
        args.y.push(Number.parseFloat(next()));
        break;
      case '--out':
        args.out = path.resolve(next());
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!args.template && !args.html) {
    throw new Error('pass --template NAME or --html FILE');
  }
  if (args.template && args.html) {
    throw new Error('pass only one of --template or --html');
  }
  if (!Number.isFinite(args.width) || args.width <= 0) {
    throw new Error('--width must be a positive integer');
  }
  if (!Number.isFinite(args.timeoutMs) || args.timeoutMs <= 0) {
    throw new Error('--timeout-ms must be a positive integer');
  }
  if (args.y.some((value) => !Number.isFinite(value))) {
    throw new Error('--y must be a number');
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const source = await loadSource(args);
  const preparedHtml = buildBrowserDocument(source.html, source.baseUrl, args.width);
  const preparedPath = path.join(
    '/tmp/mail-canvas-layout-dump',
    `${source.name}-${args.width}.html`,
  );
  await mkdir(path.dirname(preparedPath), { recursive: true });
  await writeFile(preparedPath, preparedHtml);

  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({
      viewport: { width: args.width, height: 900 },
      deviceScaleFactor: 1,
    });
    await page.goto(pathToFileURL(preparedPath).href, {
      waitUntil: 'load',
      timeout: args.timeoutMs,
    });
    await page.waitForTimeout(500);
    const height = await page.evaluate(() => {
      let maxBottom = 0;
      for (const element of document.body.querySelectorAll('*')) {
        const rect = element.getBoundingClientRect();
        if (rect.width > 0 || rect.height > 0) {
          maxBottom = Math.max(maxBottom, rect.bottom);
        }
      }
      return Math.max(1, Math.ceil(maxBottom), document.body.scrollHeight);
    });
    await page.setViewportSize({ width: args.width, height });
    await page.waitForTimeout(100);

    const dump = await page.evaluate(
      ({ selector, yValues }) => {
        const selected = [...document.body.querySelectorAll(selector)].map((element) =>
          elementSnapshot(element),
        );
        const hitTests = yValues.map((y) => {
          const hits = [...document.body.querySelectorAll('*')]
            .map((element) => elementSnapshot(element))
            .filter((element) => {
              return (
                element.rect.width > 0 &&
                element.rect.height > 0 &&
                element.rect.top <= y &&
                element.rect.bottom >= y
              );
            })
            .sort((a, b) => a.rect.height - b.rect.height);
          return { y, hits };
        });

        return {
          url: location.href,
          viewport: { width: window.innerWidth, height: window.innerHeight },
          document: {
            bodyScrollHeight: document.body.scrollHeight,
            documentScrollHeight: document.documentElement.scrollHeight,
          },
          selected,
          hitTests,
          textRects: collectTextRects(),
        };

        function elementSnapshot(element) {
          const rect = element.getBoundingClientRect();
          const style = window.getComputedStyle(element);
          return {
            tag: element.tagName.toLowerCase(),
            id: element.id || '',
            className: typeof element.className === 'string' ? element.className : '',
            text: element.textContent.trim().replace(/\s+/g, ' ').slice(0, 120),
            rect: rectSnapshot(rect),
            style: {
              display: style.display,
              position: style.position,
              boxSizing: style.boxSizing,
              float: style.cssFloat,
              clear: style.clear,
              width: style.width,
              height: style.height,
              minWidth: style.minWidth,
              maxWidth: style.maxWidth,
              margin: style.margin,
              padding: style.padding,
              borderWidth: style.borderWidth,
              borderStyle: style.borderStyle,
              borderColor: style.borderColor,
              verticalAlign: style.verticalAlign,
              textAlign: style.textAlign,
              fontFamily: style.fontFamily,
              fontSize: style.fontSize,
              fontWeight: style.fontWeight,
              lineHeight: style.lineHeight,
              letterSpacing: style.letterSpacing,
              color: style.color,
              backgroundColor: style.backgroundColor,
              backgroundImage: style.backgroundImage,
              backgroundSize: style.backgroundSize,
              backgroundPosition: style.backgroundPosition,
              backgroundRepeat: style.backgroundRepeat,
              tableLayout: style.tableLayout,
            },
          };
        }

        function collectTextRects() {
          const textRects = [];
          const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
          const range = document.createRange();
          while (walker.nextNode()) {
            const node = walker.currentNode;
            if (!node.nodeValue || !node.nodeValue.trim()) {
              continue;
            }
            range.selectNodeContents(node);
            for (const rect of range.getClientRects()) {
              if (rect.width > 0 && rect.height > 0) {
                textRects.push({
                  text: node.nodeValue.trim().replace(/\s+/g, ' ').slice(0, 120),
                  rect: rectSnapshot(rect),
                });
              }
            }
          }
          range.detach();
          return textRects;
        }

        function rectSnapshot(rect) {
          return {
            left: round(rect.left),
            top: round(rect.top),
            right: round(rect.right),
            bottom: round(rect.bottom),
            width: round(rect.width),
            height: round(rect.height),
          };
        }

        function round(value) {
          return Math.round(value * 1000) / 1000;
        }
      },
      { selector: args.selector, yValues: args.y },
    );

    const output = {
      template: source.name,
      sourceUrl: source.url,
      preparedHtml: preparedPath,
      width: args.width,
      selector: args.selector,
      y: args.y,
      ...dump,
    };
    const json = `${JSON.stringify(output, null, 2)}\n`;
    if (args.out) {
      await mkdir(path.dirname(args.out), { recursive: true });
      await writeFile(args.out, json);
      console.log(args.out);
    } else {
      process.stdout.write(json);
    }
    await page.close();
  } finally {
    await browser.close();
  }
}

async function loadSource(args) {
  if (args.html) {
    const html = await readFile(args.html, 'utf8');
    const baseUrl =
      args.baseUrl ?? pathToFileURL(`${path.dirname(args.html)}${path.sep}`).href;
    return {
      name: path.basename(args.html, path.extname(args.html)),
      url: args.baseUrl ?? pathToFileURL(args.html).href,
      baseUrl,
      html,
    };
  }

  const template = TEMPLATES.find(([name]) => name === args.template);
  if (!template) {
    throw new Error(`unknown template: ${args.template}`);
  }
  const [name, url] = template;
  const response = await fetch(url, { signal: AbortSignal.timeout(args.timeoutMs) });
  if (!response.ok) {
    throw new Error(`${url}: ${response.status} ${response.statusText}`);
  }
  return {
    name,
    url,
    baseUrl: new URL('.', url).href,
    html: await response.text(),
  };
}

function buildBrowserDocument(sourceHtml, baseUrl, width) {
  const head = [
    '<meta charset="utf-8">',
    `<base href="${escapeAttr(baseUrl)}">`,
    '<style id="email-render-defaults">',
    'html, body { margin: 0; padding: 0; }',
    `body { width: ${width}px; min-width: ${width}px; overflow: visible; background: #fff; }`,
    '#email-render-root { width: 100%; }',
    'table { border-collapse: separate; border-spacing: 0; }',
    'img { display: block; }',
    '</style>',
  ].join('\n');
  const lower = sourceHtml.toLowerCase();
  const looksLikeDocument =
    lower.includes('<!doctype') ||
    lower.includes('<html') ||
    lower.includes('<body') ||
    lower.includes('<head');
  if (!looksLikeDocument) {
    return `<!doctype html><html><head>${head}</head><body><div id="email-render-root">${sourceHtml}</div></body></html>`;
  }
  const headEnd = lower.indexOf('</head>');
  if (headEnd >= 0) {
    return `${sourceHtml.slice(0, headEnd)}${head}${sourceHtml.slice(headEnd)}`;
  }
  const htmlStart = lower.indexOf('<html');
  if (htmlStart >= 0) {
    const closeOffset = sourceHtml.slice(htmlStart).indexOf('>');
    if (closeOffset >= 0) {
      const insertAt = htmlStart + closeOffset + 1;
      return `${sourceHtml.slice(0, insertAt)}<head>${head}</head>${sourceHtml.slice(insertAt)}`;
    }
  }
  return `<!doctype html><html><head>${head}</head>${sourceHtml}</html>`;
}

function escapeAttr(value) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('"', '&quot;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
