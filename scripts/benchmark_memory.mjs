#!/usr/bin/env node

import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { PNG } from 'pngjs';

import { TEMPLATE_CORPUS, loadTemplateSource } from './templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FIXTURE_FONT_FILES = [
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Bold.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSansMath-Regular.ttf'),
];

function parseArgs(argv) {
  const args = {
    caseName: 'corpus',
    template: 'colorlib-template-1',
    width: 600,
    timeoutMs: 15000,
    out: null,
    fixtureFonts: false,
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
      case '--case':
        args.caseName = next();
        break;
      case '--template':
        args.template = next();
        break;
      case '--width':
        args.width = Number.parseInt(next(), 10);
        break;
      case '--timeout-ms':
        args.timeoutMs = Number.parseInt(next(), 10);
        break;
      case '--out':
        args.out = path.resolve(next());
        break;
      case '--fixture-fonts':
        args.fixtureFonts = true;
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!['corpus', 'repeated-image', 'thumbnail-800x1200'].includes(args.caseName)) {
    throw new Error(`unknown --case: ${args.caseName}`);
  }
  const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mail-canvas-benchmark-'));
  try {
    const source = await loadBenchmarkSource(args, tmpDir);
    const htmlPath = path.join(tmpDir, `${source.name}.html`);
    const browserPng = path.join(tmpDir, `${source.name}.browser.png`);
    const rustPng = path.join(tmpDir, `${source.name}.rust.png`);
    await writeFile(htmlPath, source.html);
    const baseUrl = source.baseUrl;
    const renderWidth = source.width ?? args.width;

    await runOrThrow('cargo', ['build', '--release'], ROOT_DIR);
    const renderer = path.join(ROOT_DIR, 'target', 'release', 'mail-canvas');

    const rust = await measureNativeCommand(
      renderer,
      [
        '--html',
        htmlPath,
        '--output',
        rustPng,
        '--width',
        String(renderWidth),
        '--viewport-height',
        String(source.viewportHeight ?? 800),
        ...(source.maxHeight ? ['--max-height', String(source.maxHeight)] : []),
        '--timeout-ms',
        String(args.timeoutMs),
        '--base-url',
        baseUrl,
        '--allow-remote',
        '--allow-http',
        ...(args.fixtureFonts
          ? FIXTURE_FONT_FILES.flatMap((fontPath) => ['--font-file', fontPath])
          : []),
      ],
      ROOT_DIR,
    );

    const chromium = await measureCommand(
      process.execPath,
      [
        path.join(ROOT_DIR, 'scripts', 'playwright_capture.mjs'),
        '--html',
        htmlPath,
        '--output',
        browserPng,
        '--width',
        String(renderWidth),
        '--timeout-ms',
        String(args.timeoutMs),
        '--base-url',
        baseUrl,
      ],
      ROOT_DIR,
    );

    const report = {
      generatedAt: new Date().toISOString(),
      case: args.caseName,
      template: source.name,
      url: source.url,
      width: renderWidth,
      height: source.maxHeight ?? null,
      mailCanvas: rust,
      chromium,
      delta: {
        rssKb: chromium.maxRssKb - rust.maxRssKb,
        elapsedMs: chromium.elapsedMs - rust.elapsedMs,
      },
      ratio: {
        rss: rust.maxRssKb > 0 ? chromium.maxRssKb / rust.maxRssKb : null,
        elapsed: rust.elapsedMs > 0 ? chromium.elapsedMs / rust.elapsedMs : null,
      },
      files: {
        html: htmlPath,
        rustPng,
        browserPng,
      },
    };

    if (args.out) {
      await writeFile(args.out, `${JSON.stringify(report, null, 2)}\n`);
      console.log(args.out);
    } else {
      console.log(JSON.stringify(report, null, 2));
    }
  } finally {
    if (!args.out) {
      await rm(tmpDir, { recursive: true, force: true });
    }
  }
}

async function loadBenchmarkSource(args, tmpDir) {
  switch (args.caseName) {
    case 'corpus':
      return await loadCorpusBenchmarkSource(args.template, args.timeoutMs);
    case 'repeated-image':
      return await createRepeatedImageSource(tmpDir);
    case 'thumbnail-800x1200':
      return await createThumbnail800x1200Source(tmpDir);
    default:
      throw new Error(`unknown --case: ${args.caseName}`);
  }
}

async function loadCorpusBenchmarkSource(templateName, timeoutMs) {
  const template = TEMPLATE_CORPUS.find((entry) => entry.name === templateName);
  if (!template) {
    throw new Error(`unknown template: ${templateName}`);
  }
  const source = await loadTemplateSource(template, timeoutMs);
  return {
    ...source,
    name: template.name,
  };
}

async function createRepeatedImageSource(tmpDir) {
  const imageName = 'hero.png';
  const imagePath = path.join(tmpDir, imageName);
  const imageWidth = 1400;
  const imageHeight = 900;
  const repeatCount = 24;
  const png = new PNG({ width: imageWidth, height: imageHeight });
  for (let y = 0; y < imageHeight; y += 1) {
    for (let x = 0; x < imageWidth; x += 1) {
      const offset = (y * imageWidth + x) * 4;
      png.data[offset] = Math.floor((x * 255) / imageWidth);
      png.data[offset + 1] = Math.floor((y * 255) / imageHeight);
      png.data[offset + 2] = (x + y) % 256;
      png.data[offset + 3] = 255;
    }
  }
  await writeFile(imagePath, PNG.sync.write(png));

  const rows = Array.from(
    { length: repeatCount },
    (_, index) =>
      `<tr><td><img src="${imageName}" width="800" style="display:block;width:800px;height:auto" alt="hero ${index}"></td></tr>`,
  ).join('\n');
  return {
    name: 'repeated-image',
    url: null,
    baseUrl: pathToFileURL(`${tmpDir}${path.sep}`).href,
    html: `<!doctype html><html><body style="margin:0;background:#fff"><table width="800" cellpadding="0" cellspacing="0" style="margin:0 auto">${rows}</table></body></html>`,
  };
}

async function createThumbnail800x1200Source(tmpDir) {
  const imageName = 'hero.png';
  const imagePath = path.join(tmpDir, imageName);
  await writeFile(imagePath, PNG.sync.write(createGradientPng(1400, 650)));

  return {
    name: 'thumbnail-800x1200',
    url: null,
    width: 800,
    viewportHeight: 1200,
    maxHeight: 1200,
    baseUrl: pathToFileURL(`${tmpDir}${path.sep}`).href,
    html: `<!doctype html>
<html>
<body style="margin:0;background:#f4f7fb;font-family:Arial,Helvetica,sans-serif;color:#172033">
<table width="800" cellpadding="0" cellspacing="0" role="presentation" style="width:800px;height:1200px;background:#fff">
  <tr>
    <td style="padding:0">
      <img src="${imageName}" width="800" style="display:block;width:800px;height:auto" alt="hero">
    </td>
  </tr>
  <tr>
    <td style="padding:34px 60px">
      <div style="font-size:14px;letter-spacing:2px;text-transform:uppercase;color:#4577b9">Marketing campaign</div>
      <div style="font-size:42px;line-height:48px;font-weight:700;margin-top:14px">Spring launch snapshot benchmark</div>
      <p style="font-size:18px;line-height:28px;color:#536176;margin:18px 0 0">This fixed 800 by 1200 template includes one large decoded image, nested tables, rounded blocks, and enough copy to exercise text layout.</p>
    </td>
  </tr>
  <tr>
    <td style="padding:10px 60px 34px">
      <table width="680" cellpadding="0" cellspacing="0" role="presentation">
        <tr>
          <td width="320" style="padding:24px;background:#eef4fb;vertical-align:top">
            <h2 style="font-size:23px;line-height:30px;margin:0 0 12px">Lower memory</h2>
            <p style="font-size:16px;line-height:25px;margin:0;color:#536176">Keep thumbnail workers stable on small VPS machines without launching a browser process per render.</p>
          </td>
          <td width="40"></td>
          <td width="320" style="padding:24px;background:#f8efdc;vertical-align:top">
            <h2 style="font-size:23px;line-height:30px;margin:0 0 12px">Fast enough</h2>
            <p style="font-size:16px;line-height:25px;margin:0;color:#536176">Semantic layout stability matters more than pixel-perfect glyph matching for preview thumbnails.</p>
          </td>
        </tr>
      </table>
    </td>
  </tr>
  <tr>
    <td style="height:184px;padding:28px 60px;background:#10233f;color:#c8d7e8;font-size:14px;line-height:22px;vertical-align:top">Footer text and preference links. Output target: 800 x 1200 CSS pixels.</td>
  </tr>
</table>
</body>
</html>`,
  };
}

function createGradientPng(width, height) {
  const png = new PNG({ width, height });
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const offset = (y * width + x) * 4;
      png.data[offset] = Math.floor((x * 255) / width);
      png.data[offset + 1] = Math.floor((y * 255) / height);
      png.data[offset + 2] = 180;
      png.data[offset + 3] = 255;
    }
  }
  return png;
}

async function runOrThrow(command, args, cwd) {
  const result = await measureCommand(command, args, cwd);
  if (result.exitCode !== 0) {
    throw new Error(`${command} ${args.join(' ')} failed`);
  }
}

function measureCommand(command, args, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const stdout = [];
    const stderr = [];
    child.stdout.on('data', (chunk) => stdout.push(chunk));
    child.stderr.on('data', (chunk) => stderr.push(chunk));

    let maxRssKb = 0;
    const started = Date.now();
    const sampleRss = async () => {
      try {
        maxRssKb = Math.max(maxRssKb, await sampleProcessTreeRss(child.pid));
      } catch {}
    };
    void sampleRss();
    const timer = setInterval(sampleRss, 10);

    child.on('error', (error) => {
      clearInterval(timer);
      reject(error);
    });
    child.on('close', async (exitCode) => {
      clearInterval(timer);
      maxRssKb = Math.max(maxRssKb, await sampleProcessTreeRss(child.pid).catch(() => 0));
      resolve({
        command,
        args,
        exitCode,
        elapsedMs: Date.now() - started,
        maxRssKb,
        stdout: Buffer.concat(stdout).toString('utf8'),
        stderr: Buffer.concat(stderr).toString('utf8'),
      });
    });
  });
}

function measureNativeCommand(command, args, cwd) {
  const timeArgs =
    process.platform === 'darwin' ? ['-l', command, ...args] : ['-v', command, ...args];
  return new Promise((resolve, reject) => {
    const child = spawn('/usr/bin/time', timeArgs, {
      cwd,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const stdout = [];
    const stderr = [];
    child.stdout.on('data', (chunk) => stdout.push(chunk));
    child.stderr.on('data', (chunk) => stderr.push(chunk));

    const started = Date.now();
    child.on('error', (error) => {
      reject(error);
    });
    child.on('close', (exitCode) => {
      const stderrText = Buffer.concat(stderr).toString('utf8');
      resolve({
        command,
        args,
        exitCode,
        elapsedMs: Date.now() - started,
        maxRssKb: parseTimeMaxRssKb(stderrText),
        stdout: Buffer.concat(stdout).toString('utf8'),
        stderr: stderrText,
      });
    });
  });
}

function parseTimeMaxRssKb(stderr) {
  const darwin = stderr.match(/^\s*(\d+)\s+maximum resident set size$/m);
  if (darwin) {
    return Math.round(Number.parseInt(darwin[1], 10) / 1024);
  }
  const linux = stderr.match(/Maximum resident set size \(kbytes\):\s*(\d+)/);
  if (linux) {
    return Number.parseInt(linux[1], 10);
  }
  return 0;
}

async function sampleProcessTreeRss(rootPid) {
  if (!rootPid) {
    return 0;
  }
  const output = await new Promise((resolve, reject) => {
    const child = spawn('ps', ['-axo', 'pid=,ppid=,rss='], {
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    const chunks = [];
    child.stdout.on('data', (chunk) => chunks.push(chunk));
    child.on('error', reject);
    child.on('close', (code) => {
      if (code !== 0) {
        reject(new Error(`ps exited ${code}`));
        return;
      }
      resolve(Buffer.concat(chunks).toString('utf8'));
    });
  });

  const rows = `${output}`.trim().split('\n').map((line) => {
    const [pidText, ppidText, rssText] = line.trim().split(/\s+/, 3);
    return {
      pid: Number.parseInt(pidText, 10),
      ppid: Number.parseInt(ppidText, 10),
      rssKb: Number.parseInt(rssText, 10),
    };
  });
  const childrenByParent = new Map();
  for (const row of rows) {
    if (!childrenByParent.has(row.ppid)) {
      childrenByParent.set(row.ppid, []);
    }
    childrenByParent.get(row.ppid).push(row.pid);
  }

  const queue = [rootPid];
  const seen = new Set(queue);
  let total = 0;
  const rssByPid = new Map(rows.map((row) => [row.pid, row.rssKb]));
  while (queue.length > 0) {
    const pid = queue.pop();
    total += rssByPid.get(pid) ?? 0;
    for (const childPid of childrenByParent.get(pid) ?? []) {
      if (!seen.has(childPid)) {
        seen.add(childPid);
        queue.push(childPid);
      }
    }
  }
  return total;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
