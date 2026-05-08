import init, { WasmRenderer } from "./pkg/mail_canvas_wasm.js";

let renderer;

self.addEventListener("message", async (event) => {
  const message = event.data ?? {};
  const requestId = message.requestId;
  try {
    if (message.type === "init") {
      await init();
      renderer = new WasmRenderer();
      for (const font of message.fonts ?? []) {
        renderer.register_font(new Uint8Array(font.bytes));
      }
      self.postMessage({ requestId, ok: true });
      return;
    }

    if (message.type === "render") {
      if (!renderer) {
        throw new Error("worker not initialized");
      }
      const started = performance.now();
      renderer.clear_assets();
      for (const asset of message.assets ?? []) {
        renderer.register_asset(asset.url, new Uint8Array(asset.bytes));
      }
      const png = renderer.render_png_with_base_url_and_max_height(
        message.html,
        message.width,
        message.viewportHeight,
        message.scale,
        message.baseUrl,
        message.maxHeight || 0
      );
      const diagnostics = JSON.parse(renderer.diagnostics_json());
      self.postMessage(
        {
          requestId,
          ok: true,
          png: png.buffer,
          diagnostics,
          assetSummary: {
            registered: renderer.asset_count(),
          },
          renderMs: performance.now() - started,
        },
        [png.buffer]
      );
      return;
    }

    if (message.type === "registerFonts") {
      if (!renderer) {
        throw new Error("worker not initialized");
      }
      for (const font of message.fonts ?? []) {
        renderer.register_font(new Uint8Array(font.bytes));
      }
      self.postMessage({ requestId, ok: true });
      return;
    }

    if (message.type === "clear") {
      if (renderer) {
        renderer.clear_assets();
      }
      self.postMessage({ requestId, ok: true });
      return;
    }

    throw new Error(`unknown worker message type: ${message.type}`);
  } catch (error) {
    self.postMessage({
      requestId,
      ok: false,
      error: String(error),
    });
  }
});
