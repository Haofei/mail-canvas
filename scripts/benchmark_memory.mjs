#!/usr/bin/env node

import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { TEMPLATE_CORPUS, loadTemplateSource } from './templates.mjs';

const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FIXTURE_FONT_FILES = [
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Regular.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSans-Bold.ttf'),
  path.join(ROOT_DIR, 'fixtures', 'fonts', 'NotoSansMath-Regular.ttf'),
];

function parseArgs(argv) {
  const args = {
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
  const template = TEMPLATE_CORPUS.find((entry) => entry.name === args.template);
  if (!template) {
    throw new Error(`unknown template: ${args.template}`);
  }
  const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mail-canvas-benchmark-'));
  try {
    const source = await loadTemplateSource(template, args.timeoutMs);
    const htmlPath = path.join(tmpDir, `${template.name}.html`);
    const browserPng = path.join(tmpDir, `${template.name}.browser.png`);
    const rustPng = path.join(tmpDir, `${template.name}.rust.png`);
    await writeFile(htmlPath, source.html);
    const baseUrl = source.baseUrl;

    await runOrThrow('cargo', ['build'], ROOT_DIR);
    const renderer = path.join(ROOT_DIR, 'target', 'debug', 'mail-canvas');

    const rust = await measureCommand(
      renderer,
      [
        '--html',
        htmlPath,
        '--output',
        rustPng,
        '--width',
        String(args.width),
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
        String(args.width),
        '--timeout-ms',
        String(args.timeoutMs),
        '--base-url',
        baseUrl,
      ],
      ROOT_DIR,
    );

    const report = {
      generatedAt: new Date().toISOString(),
      template: template.name,
      url: source.url,
      width: args.width,
      mailCanvas: rust,
      chromium,
      delta: {
        rssKb: chromium.maxRssKb - rust.maxRssKb,
        elapsedMs: chromium.elapsedMs - rust.elapsedMs,
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
    const timer = setInterval(async () => {
      try {
        maxRssKb = Math.max(maxRssKb, await sampleProcessTreeRss(child.pid));
      } catch {}
    }, 50);

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
