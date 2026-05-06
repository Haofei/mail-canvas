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
      renderer.clear_assets();
      for (const asset of message.assets ?? []) {
        renderer.register_asset(asset.url, new Uint8Array(asset.bytes));
      }
      const png = renderer.render_png_with_base_url(
        message.html,
        message.width,
        message.viewportHeight,
        message.scale,
        message.baseUrl
      );
      const diagnostics = JSON.parse(renderer.diagnostics_json());
      self.postMessage(
        {
          requestId,
          ok: true,
          png: png.buffer,
          diagnostics,
        },
        [png.buffer]
      );
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
