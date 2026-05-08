const DEFAULT_LIMITS = Object.freeze({
  maxAssetBytes: 10 * 1024 * 1024,
  maxTotalAssetBytes: 64 * 1024 * 1024,
  maxAssetCount: 128,
});

const DEFAULT_EMOJI_FONT_URL = new URL("../fixtures/fonts/NotoColorEmoji.ttf", import.meta.url);
const EMOJI_CODEPOINT_PATTERN = /[\u{1F000}-\u{1FAFF}\u{2600}-\u{27BF}\u{2300}-\u{23FF}]/u;
const NUMERIC_ENTITY_PATTERN = /&#(?:x([0-9a-fA-F]+)|([0-9]+));/g;
const assetPattern =
  /url\(\s*(['"]?)([^'")]+)\1\s*\)|@import\s+(?:url\(\s*)?(['"]?)([^'")\s]+)\3\s*\)?/gi;
const TEXT_DECODER = new TextDecoder();

export async function createMailCanvasRenderer(options = {}) {
  if (!options.workerUrl) {
    throw new Error("createMailCanvasRenderer requires workerUrl");
  }
  const workerUrl = options.workerUrl;
  const worker = new Worker(workerUrl, { type: "module" });
  const client = new WorkerClient(worker);
  const fontAssets = await fetchFontAssets(options.fonts ?? [], options.baseUrl);
  const fontPayload = fontAssets.map((asset) => ({
    url: asset.url,
    bytes: transferableBytes(asset.bytes),
  }));

  await client.call(
    {
      type: "init",
      fonts: fontPayload,
    },
    fontPayload.map((asset) => asset.bytes),
  );

  return new MailCanvasBrowserRenderer(client, {
    baseUrl: options.baseUrl,
    limits: { ...DEFAULT_LIMITS, ...(options.limits ?? {}) },
    defaultEmojiFont:
      options.defaultEmojiFont === false
        ? null
        : (options.defaultEmojiFont ?? DEFAULT_EMOJI_FONT_URL),
  });
}

export class MailCanvasBrowserRenderer {
  constructor(client, options) {
    this.client = client;
    this.defaultBaseUrl = options.baseUrl;
    this.limits = options.limits;
    this.defaultEmojiFont = options.defaultEmojiFont;
    this.defaultEmojiFontRegistered = false;
    this.defaultEmojiFontPromise = null;
    this.assetCache = new Map();
    this.cssDependencyCache = new Map();
    this.stylesheetRewriteCache = new Map();
    this.registeredAssetUrls = new Set();
    this.destroyed = false;
  }

  async renderThumbnail(options) {
    this.#assertAlive();
    const html = String(options.html ?? "");
    const width = positiveInteger(options.width ?? 800, "width");
    const height = positiveInteger(options.height ?? options.viewportHeight ?? 1200, "height");
    const scale = positiveNumber(options.scale ?? 1, "scale");
    const baseUrl = resolveBaseUrl(options.baseUrl ?? this.defaultBaseUrl);
    const started = performance.now();
    await this.#ensureDefaultEmojiFont(html, baseUrl);
    const prepared = await this.#inlineStylesheetLinks(html, baseUrl);
    const assets = await this.#fetchAssetGraph(prepared.assetUrls, baseUrl, prepared.assets);
    const fetchedAt = performance.now();
    let newAssets = assets.filter((asset) => !this.registeredAssetUrls.has(asset.url));
    if (this.registeredAssetUrls.size + newAssets.length > this.limits.maxAssetCount) {
      await this.client.call({ type: "clear" });
      this.registeredAssetUrls.clear();
      newAssets = assets;
    }
    const assetPayload = newAssets.map((asset) => ({
      url: asset.url,
      bytes: transferableBytes(asset.bytes),
    }));
    const response = await this.client.call(
      {
        type: "render",
        html: prepared.html,
        width,
        viewportHeight: height,
        maxHeight: height,
        scale,
        baseUrl,
        assets: assetPayload,
      },
      assetPayload.map((asset) => asset.bytes),
    );
    for (const asset of newAssets) {
      this.registeredAssetUrls.add(asset.url);
    }
    const renderedAt = performance.now();
    const png = new Uint8Array(response.png);
    return {
      png,
      blob: new Blob([png], { type: "image/png" }),
      width,
      height,
      scale,
      diagnostics: normalizeDiagnostics(response.diagnostics),
      assets: response.assetSummary,
      timing: {
        fetchMs: fetchedAt - started,
        renderMs: response.renderMs ?? renderedAt - fetchedAt,
        totalMs: renderedAt - started,
      },
    };
  }

  async clearCache() {
    this.assetCache.clear();
    this.cssDependencyCache.clear();
    this.stylesheetRewriteCache.clear();
    this.registeredAssetUrls.clear();
    if (!this.destroyed) {
      await this.client.call({ type: "clear" });
    }
  }

  destroy() {
    if (this.destroyed) {
      return;
    }
    this.assetCache.clear();
    this.cssDependencyCache.clear();
    this.stylesheetRewriteCache.clear();
    this.registeredAssetUrls.clear();
    this.client.destroy();
    this.destroyed = true;
  }

  async #fetchAssetGraph(assetUrls, baseUrl, initialAssets = []) {
    const visited = new Set(initialAssets.map((asset) => asset.url));
    const assets = [...initialAssets];
    const pending = [...assetUrls].map((url) => absoluteUrl(url, baseUrl));
    let totalBytes = initialAssets.reduce((sum, asset) => sum + asset.bytes.byteLength, 0);
    if (assets.length > this.limits.maxAssetCount) {
      throw new Error(`asset count exceeds maxAssetCount (${this.limits.maxAssetCount})`);
    }
    if (totalBytes > this.limits.maxTotalAssetBytes) {
      throw new Error(`assets exceed maxTotalAssetBytes (${this.limits.maxTotalAssetBytes})`);
    }

    while (pending.length > 0) {
      const url = pending.pop();
      if (!url || visited.has(url)) {
        continue;
      }
      visited.add(url);
      if (assets.length >= this.limits.maxAssetCount) {
        throw new Error(`asset count exceeds maxAssetCount (${this.limits.maxAssetCount})`);
      }

      const bytes = await this.#fetchBytes(url);
      if (bytes.byteLength > this.limits.maxAssetBytes) {
        throw new Error(`asset exceeds maxAssetBytes (${url})`);
      }
      totalBytes += bytes.byteLength;
      if (totalBytes > this.limits.maxTotalAssetBytes) {
        throw new Error(`assets exceed maxTotalAssetBytes (${this.limits.maxTotalAssetBytes})`);
      }
      assets.push({ url, bytes });

      if (url.endsWith(".css") || isStylesheetBytes(bytes)) {
        for (const nested of this.#cssDependencies(url, bytes)) {
          pending.push(absoluteUrl(nested, url));
        }
      }
    }

    return assets;
  }

  async #inlineStylesheetLinks(html, baseUrl) {
    const document = new DOMParser().parseFromString(html, "text/html");
    const stylesheetAssets = [];
    for (const link of document.querySelectorAll('link[rel~="stylesheet"][href]')) {
      if (/\balternate\b/i.test(link.getAttribute("rel") || "")) {
        continue;
      }
      const url = absoluteUrl(link.getAttribute("href") || "", baseUrl);
      const bytes = await this.#fetchBytes(url);
      if (bytes.byteLength > this.limits.maxAssetBytes) {
        throw new Error(`stylesheet exceeds maxAssetBytes (${url})`);
      }
      stylesheetAssets.push({ url, bytes });
      let cssText;
      if (this.stylesheetRewriteCache.has(url)) {
        cssText = this.stylesheetRewriteCache.get(url);
      } else {
        cssText = rewriteCssUrls(TEXT_DECODER.decode(bytes), url);
        this.stylesheetRewriteCache.set(url, cssText);
      }
      const style = document.createElement("style");
      style.textContent = cssText;
      link.replaceWith(style);
    }
    return {
      html: `<!doctype html>\n${document.documentElement.outerHTML}`,
      assets: stylesheetAssets,
      assetUrls: collectDocumentAssetUrls(document),
    };
  }

  async #fetchBytes(url) {
    const cached = this.assetCache.get(url);
    if (cached) {
      return cached;
    }
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`failed to fetch ${url}: ${response.status} ${response.statusText}`);
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    this.assetCache.set(url, bytes);
    return bytes;
  }

  #cssDependencies(url, bytes) {
    let dependencies = this.cssDependencyCache.get(url);
    if (!dependencies) {
      dependencies = Array.from(extractCssUrls(TEXT_DECODER.decode(bytes)));
      this.cssDependencyCache.set(url, dependencies);
    }
    return dependencies;
  }

  #assertAlive() {
    if (this.destroyed) {
      throw new Error("renderer has been destroyed");
    }
  }

  async #ensureDefaultEmojiFont(html, baseUrl) {
    if (
      this.defaultEmojiFontRegistered ||
      !this.defaultEmojiFont ||
      !htmlNeedsEmoji(html)
    ) {
      return;
    }
    if (!this.defaultEmojiFontPromise) {
      this.defaultEmojiFontPromise = (async () => {
        const fontAssets = await fetchFontAssets([this.defaultEmojiFont], baseUrl);
        const fontPayload = fontAssets.map((asset) => ({
          url: asset.url,
          bytes: transferableBytes(asset.bytes),
        }));
        await this.client.call(
          {
            type: "registerFonts",
            fonts: fontPayload,
          },
          fontPayload.map((asset) => asset.bytes),
        );
        this.defaultEmojiFontRegistered = true;
      })().finally(() => {
        this.defaultEmojiFontPromise = null;
      });
    }
    await this.defaultEmojiFontPromise;
  }
}

class WorkerClient {
  constructor(worker) {
    this.worker = worker;
    this.nextRequestId = 1;
  }

  call(message, transfer = []) {
    return new Promise((resolve, reject) => {
      const requestId = this.nextRequestId++;
      const onMessage = (event) => {
        if (event.data?.requestId !== requestId) {
          return;
        }
        this.worker.removeEventListener("message", onMessage);
        this.worker.removeEventListener("error", onError);
        if (event.data.ok) {
          resolve(event.data);
        } else {
          reject(new Error(event.data.error || "worker request failed"));
        }
      };
      const onError = (event) => {
        this.worker.removeEventListener("message", onMessage);
        this.worker.removeEventListener("error", onError);
        reject(event.error || new Error("worker crashed"));
      };
      this.worker.addEventListener("message", onMessage);
      this.worker.addEventListener("error", onError);
      this.worker.postMessage({ ...message, requestId }, transfer);
    });
  }

  destroy() {
    this.worker.terminate();
  }
}

function collectDocumentAssetUrls(document) {
  const urls = new Set();

  for (const image of document.querySelectorAll("[src]")) {
    uniquePush(urls, image.getAttribute("src") || "");
  }
  for (const link of document.querySelectorAll('link[rel="stylesheet"][href]')) {
    uniquePush(urls, link.getAttribute("href") || "");
  }
  for (const node of document.querySelectorAll("[background]")) {
    uniquePush(urls, node.getAttribute("background") || "");
  }
  for (const styleBlock of document.querySelectorAll("style")) {
    for (const url of extractCssUrls(styleBlock.textContent || "")) {
      uniquePush(urls, url);
    }
  }
  for (const styled of document.querySelectorAll("[style]")) {
    for (const url of extractCssUrls(styled.getAttribute("style") || "")) {
      uniquePush(urls, url);
    }
  }

  return urls;
}

function extractCssUrls(cssText) {
  const urls = new Set();
  let match;
  assetPattern.lastIndex = 0;
  while ((match = assetPattern.exec(cssText)) !== null) {
    uniquePush(urls, match[2] || match[4] || "");
  }
  return urls;
}

function rewriteCssUrls(cssText, baseUrl) {
  assetPattern.lastIndex = 0;
  const rewritten = cssText.replace(
    assetPattern,
    (match, _urlQuote, urlValue, _importQuote, importValue) => {
      const value = urlValue || importValue || "";
      if (!value || value.startsWith("data:") || value.startsWith("#")) {
        return match;
      }
      const resolved = absoluteUrl(value, baseUrl);
      if (urlValue) {
        return `url("${resolved}")`;
      }
      return `@import "${resolved}"`;
    },
  );
  assetPattern.lastIndex = 0;
  return rewritten;
}

function uniquePush(set, value) {
  const trimmed = value.trim();
  if (
    trimmed &&
    !trimmed.startsWith("data:") &&
    !trimmed.startsWith("#") &&
    !trimmed.startsWith("mailto:")
  ) {
    set.add(trimmed);
  }
}

async function fetchFontAssets(fonts, baseUrl) {
  const resolvedBase = resolveBaseUrl(baseUrl);
  const assets = [];
  for (const font of fonts) {
    if (font.bytes) {
      assets.push({
        url: font.url ?? "",
        bytes: asUint8Array(font.bytes),
      });
      continue;
    }
    const rawUrl = typeof font === "string" || font instanceof URL ? font : font.url;
    const url = absoluteUrl(rawUrl, resolvedBase);
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`failed to fetch font ${url}: ${response.status} ${response.statusText}`);
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    assets.push({ url, bytes });
  }
  return assets;
}

function normalizeDiagnostics(value) {
  return {
    warnings: Array.isArray(value?.warnings) ? value.warnings : [],
    assets: Array.isArray(value?.assets) ? value.assets : [],
    console_messages: Array.isArray(value?.console_messages) ? value.console_messages : [],
  };
}

function resolveBaseUrl(baseUrl) {
  return new URL(baseUrl || ".", window.location.href).toString();
}

function absoluteUrl(rawUrl, baseUrl) {
  return new URL(rawUrl, baseUrl).toString();
}

function isStylesheetBytes(bytes) {
  if (bytes.length === 0) {
    return false;
  }
  const head = TEXT_DECODER.decode(bytes.subarray(0, Math.min(bytes.length, 32))).trimStart();
  return head.startsWith("@") || head.includes("{");
}

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer`);
  }
  return parsed;
}

function positiveNumber(value, label) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a finite positive number`);
  }
  return parsed;
}

function htmlNeedsEmoji(html) {
  if (EMOJI_CODEPOINT_PATTERN.test(html)) {
    return true;
  }
  NUMERIC_ENTITY_PATTERN.lastIndex = 0;
  let match;
  while ((match = NUMERIC_ENTITY_PATTERN.exec(html)) !== null) {
    const radix = match[1] ? 16 : 10;
    const raw = match[1] || match[2];
    const codepoint = Number.parseInt(raw, radix);
    if (Number.isFinite(codepoint) && isEmojiScalar(codepoint)) {
      NUMERIC_ENTITY_PATTERN.lastIndex = 0;
      return true;
    }
  }
  NUMERIC_ENTITY_PATTERN.lastIndex = 0;
  return false;
}

function isEmojiScalar(codepoint) {
  return (
    (codepoint >= 0x1f000 && codepoint <= 0x1faff) ||
    (codepoint >= 0x2600 && codepoint <= 0x27bf) ||
    (codepoint >= 0x2300 && codepoint <= 0x23ff)
  );
}

function asUint8Array(bytes) {
  if (bytes instanceof Uint8Array) {
    return bytes;
  }
  if (bytes instanceof ArrayBuffer) {
    return new Uint8Array(bytes);
  }
  return new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}

function transferableBytes(bytes) {
  const source = asUint8Array(bytes);
  return source.slice().buffer;
}
