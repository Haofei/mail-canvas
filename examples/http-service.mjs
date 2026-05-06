#!/usr/bin/env node

import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const ROOT_DIR = path.resolve(new URL('..', import.meta.url).pathname);
const renderer = path.join(ROOT_DIR, 'target', 'debug', 'mail-canvas');

const server = http.createServer(async (request, response) => {
  if (request.method !== 'POST' || request.url !== '/render') {
    response.writeHead(404).end('not found');
    return;
  }

  try {
    const body = await readBody(request);
    const payload = JSON.parse(body);
    const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'mail-canvas-http-'));
    try {
      const htmlPath = path.join(tmpDir, 'template.html');
      const outputPath = path.join(tmpDir, 'output.png');
      await writeFile(htmlPath, payload.html ?? '');
      await execFileAsync(
        renderer,
        [
          '--html',
          htmlPath,
          '--output',
          outputPath,
          '--width',
          String(payload.width ?? 600),
          ...(payload.baseUrl ? ['--base-url', String(payload.baseUrl)] : []),
        ],
        { cwd: ROOT_DIR },
      );
      const png = await readFile(outputPath);
      response.writeHead(200, { 'content-type': 'image/png' }).end(png);
    } finally {
      await rm(tmpDir, { recursive: true, force: true });
    }
  } catch (error) {
    response.writeHead(400, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ error: String(error) }));
  }
});

server.listen(8787, '127.0.0.1', () => {
  console.log('listening on http://127.0.0.1:8787/render');
});

function readBody(request) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    request.on('data', (chunk) => chunks.push(chunk));
    request.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    request.on('error', reject);
  });
}
