export async function createHeroDataUrl(width, height) {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext("2d");
  const gradient = context.createLinearGradient(0, 0, width, height);
  gradient.addColorStop(0, "#2563eb");
  gradient.addColorStop(0.45, "#41c7a8");
  gradient.addColorStop(1, "#ffca58");
  context.fillStyle = gradient;
  context.fillRect(0, 0, width, height);
  context.fillStyle = "rgba(15, 23, 42, 0.26)";
  for (let x = -80; x < width; x += 220) {
    context.fillRect(x, 0, 96, height);
  }
  return canvas.toDataURL("image/png");
}

export function thumbnailHtml(hero) {
  return `<!doctype html>
<html>
<body style="margin:0;background:#f4f7fb;font-family:Arial,Helvetica,sans-serif;color:#172033">
<table width="800" cellpadding="0" cellspacing="0" role="presentation" style="width:800px;height:1200px;background:#fff">
  <tr><td style="padding:0"><img src="${hero}" width="800" style="display:block;width:800px;height:auto" alt="hero"></td></tr>
  <tr><td style="padding:34px 60px">
    <div style="font-size:14px;letter-spacing:2px;text-transform:uppercase;color:#4577b9">WASM benchmark</div>
    <div style="font-size:42px;line-height:48px;font-weight:700;margin-top:14px">Browser worker thumbnail render</div>
    <p style="font-size:18px;line-height:28px;color:#536176;margin:18px 0 0">This fixed 800 by 1200 case exercises the public browser wrapper, font registration, diagnostics parsing, emoji fallback 🚀, and WASM rendering path.</p>
  </td></tr>
  <tr><td style="padding:10px 60px 34px">
    <table width="680" cellpadding="0" cellspacing="0" role="presentation">
      <tr>
        <td width="320" style="padding:24px;background:#eef4fb;vertical-align:top"><h2 style="font-size:23px;line-height:30px;margin:0 0 12px">Wrapper API</h2><p style="font-size:16px;line-height:25px;margin:0;color:#536176">The demo calls createMailCanvasRenderer and renderThumbnail instead of raw wasm bindings.</p></td>
        <td width="40"></td>
        <td width="320" style="padding:24px;background:#f8efdc;vertical-align:top"><h2 style="font-size:23px;line-height:30px;margin:0 0 12px">Worker path</h2><p style="font-size:16px;line-height:25px;margin:0;color:#536176">The renderer runs off the main thread and returns a PNG buffer plus diagnostics.</p></td>
      </tr>
    </table>
  </td></tr>
  <tr><td style="height:184px;padding:28px 60px;background:#10233f;color:#c8d7e8;font-size:14px;line-height:22px;vertical-align:top">Footer text and preference links. Output target: 800 x 1200 CSS pixels.</td></tr>
</table>
</body>
</html>`;
}

export function repeatedImageHtml(src, repeatCount = 8) {
  const rows = Array.from(
    { length: repeatCount },
    (_, index) =>
      `<tr><td><img src="${src}" width="800" style="display:block;width:800px;height:150px;object-fit:cover" alt="hero ${index}"></td></tr>`,
  ).join("\n");
  return `<!doctype html>
<html>
<body style="margin:0;background:#fff">
<table width="800" cellpadding="0" cellspacing="0" role="presentation" style="width:800px;background:#fff">
${rows}
</table>
</body>
</html>`;
}
