#!/usr/bin/env node
import http from "node:http";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..");
const demoRoot = path.join(repoRoot, "demo");
const browserRoot = path.join(repoRoot, "browser");
const scriptsRoot = path.join(repoRoot, "scripts");
const host = "127.0.0.1";
const startPort = Number(process.env.PORT || 4173);

const mimeTypes = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".wasm", "application/wasm"],
  [".json", "application/json; charset=utf-8"],
  [".png", "image/png"],
  [".gif", "image/gif"],
  [".jpg", "image/jpeg"],
  [".jpeg", "image/jpeg"],
  [".svg", "image/svg+xml"],
  [".ttf", "font/ttf"],
]);

function safePath(urlPath) {
  const decoded = decodeURIComponent(urlPath.split("?")[0]);
  if (decoded.startsWith("/browser/")) {
    const resolved = path.resolve(browserRoot, `.${decoded.slice("/browser".length)}`);
    if (!resolved.startsWith(browserRoot)) {
      return null;
    }
    return resolved;
  }
  if (decoded.startsWith("/scripts/")) {
    const resolved = path.resolve(scriptsRoot, `.${decoded.slice("/scripts".length)}`);
    if (!resolved.startsWith(scriptsRoot)) {
      return null;
    }
    return resolved;
  }
  const relative = decoded === "/" ? "/index.html" : decoded;
  const resolved = path.resolve(demoRoot, `.${relative}`);
  if (!resolved.startsWith(demoRoot)) {
    return null;
  }
  return resolved;
}

async function handler(req, res) {
  try {
    const filePath = safePath(req.url || "/");
    if (!filePath) {
      res.writeHead(403);
      res.end("Forbidden");
      return;
    }
    const fileStat = await stat(filePath);
    if (fileStat.isDirectory()) {
      res.writeHead(302, { Location: `${req.url?.replace(/\/?$/, "/") || "/"}index.html` });
      res.end();
      return;
    }
    const body = await readFile(filePath);
    const ext = path.extname(filePath);
    res.writeHead(200, {
      "Content-Type": mimeTypes.get(ext) || "application/octet-stream",
      "Cache-Control": "no-store",
      "Cross-Origin-Opener-Policy": "same-origin",
      "Cross-Origin-Embedder-Policy": "require-corp",
    });
    res.end(body);
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "ENOENT") {
      res.writeHead(404);
      res.end("Not found");
      return;
    }
    res.writeHead(500);
    res.end(String(error));
  }
}

function listen(port) {
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      handler(req, res);
    });
    server.once("error", reject);
    server.listen(port, host, () => resolve(server));
  });
}

let port = startPort;
let server;
for (;;) {
  try {
    server = await listen(port);
    break;
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "EADDRINUSE") {
      port += 1;
      continue;
    }
    throw error;
  }
}

console.log(`Demo server running at http://${host}:${port}`);
console.log(`Serving ${demoRoot}`);

process.on("SIGINT", () => {
  server.close(() => process.exit(0));
});
