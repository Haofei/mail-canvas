#!/usr/bin/env node

import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const ROOT_DIR = path.resolve(new URL('..', import.meta.url).pathname);
const renderer =
  process.env.MAIL_CANVAS_RENDERER ?? path.join(ROOT_DIR, 'target', 'debug', 'mail-canvas');
const host = process.env.MAIL_CANVAS_HOST ?? '127.0.0.1';
const port = Number.parseInt(process.env.MAIL_CANVAS_PORT ?? '8787', 10);
const fontDir = process.env.MAIL_CANVAS_FONT_DIR ?? path.join(ROOT_DIR, 'fixtures', 'fonts');
const maxBodyBytes = Number.parseInt(process.env.MAIL_CANVAS_MAX_BODY_BYTES ?? '1048576', 10);

const server = http.createServer(async (request, response) => {
  if (request.method === 'GET' && request.url === '/healthz') {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ ok: true }));
    return;
  }

  if (request.method !== 'POST' || request.url !== '/render') {
    response.writeHead(404).end('not found');
    return;
  }

  try {
    const body = await readBody(request, maxBodyBytes);
    const payload = JSON.parse(body);
    const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mail-canvas-http-'));
    try {
      const htmlPath = path.join(tmpDir, 'template.html');
      const outputPath = path.join(tmpDir, 'output.png');
      const warningsPath = path.join(tmpDir, 'warnings.json');
      await writeFile(htmlPath, payload.html ?? '');
      await execFileAsync(
        renderer,
        [
          '--html',
          htmlPath,
          '--output',
          outputPath,
          '--warnings-json',
          warningsPath,
          '--width',
          String(payload.width ?? 600),
          '--viewport-height',
          String(payload.viewportHeight ?? 800),
          '--scale',
          String(payload.scale ?? 1),
          '--font-dir',
          fontDir,
          ...(payload.maxHeight ? ['--max-height', String(payload.maxHeight)] : []),
          ...(payload.baseUrl ? ['--base-url', String(payload.baseUrl)] : []),
          ...(payload.allowRemote ? ['--allow-remote'] : []),
          ...(payload.allowHttp ? ['--allow-http'] : []),
        ],
        { cwd: ROOT_DIR },
      );
      const png = await readFile(outputPath);
      const diagnostics = JSON.parse(await readFile(warningsPath, 'utf8'));
      const wantsJson =
        payload.output === 'json' || request.headers.accept?.includes('application/json');
      if (wantsJson) {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(
          JSON.stringify({
            pngBase64: png.toString('base64'),
            diagnostics,
          }),
        );
      } else {
        response
          .writeHead(200, {
            'content-type': 'image/png',
            'x-mail-canvas-warnings': String(diagnostics.warnings?.length ?? 0),
            'x-mail-canvas-assets': String(diagnostics.assets?.length ?? 0),
          })
          .end(png);
      }
    } finally {
      await rm(tmpDir, { recursive: true, force: true });
    }
  } catch (error) {
    response.writeHead(400, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ error: String(error) }));
  }
});

server.listen(port, host, () => {
  console.log(`listening on http://${host}:${port}/render`);
});

function readBody(request, limitBytes) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let total = 0;
    request.on('data', (chunk) => {
      total += chunk.length;
      if (total > limitBytes) {
        reject(new Error(`request body exceeds ${limitBytes} bytes`));
        request.destroy();
        return;
      }
      chunks.push(chunk);
    });
    request.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    request.on('error', reject);
  });
}
