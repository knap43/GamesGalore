/*
 * Builds the app icon set from the hand-drawn gamepad in
 * `icon-source.png`.
 *
 *   npm install --no-save playwright && npx playwright install chromium
 *   node make-icons.js [source.png] [output-dir]
 *
 * Set CHROMIUM_PATH to use a browser you already have instead of the one
 * Playwright downloads.
 *
 * Chromium does the raster work, so no image library is needed: the black
 * paper is keyed out to transparency, the drawing is trimmed to its ink,
 * re-inked in the app's violet, thickened enough to survive at 32px, and
 * laid on the app's dark rounded tile. `icon.ico` and `icon.icns` are then
 * assembled by hand — both containers take PNG payloads directly.
 *
 * Run it after changing the drawing; the rendered files are committed, so
 * a build never needs a browser.
 */
const fs = require('fs');
const path = require('path');

const SRC = process.argv[2] || path.join(__dirname, 'icon-source.png');
const OUT = process.argv[3] || path.join(__dirname, '..', 'src-tauri', 'icons');
const SIZES = [16, 32, 48, 64, 128, 256, 512, 1024];

/* The named files Tauri's `bundle.icon` and the PKGBUILD expect. */
const NAMED = {
  32: '32x32.png',
  64: '64x64.png',
  128: '128x128.png',
  256: '128x128@2x.png',
  512: 'icon.png',
};

/* ICO: a directory of entries, each pointing at a whole PNG. 256px is
 * written as 0, the format's way of saying "not 255". */
function ico(png, sizes) {
  const head = Buffer.alloc(6);
  head.writeUInt16LE(0, 0); head.writeUInt16LE(1, 2); head.writeUInt16LE(sizes.length, 4);
  const entries = [];
  const blobs = [];
  let offset = 6 + 16 * sizes.length;
  for (const size of sizes) {
    const data = png(size);
    const e = Buffer.alloc(16);
    e.writeUInt8(size < 256 ? size : 0, 0);
    e.writeUInt8(size < 256 ? size : 0, 1);
    e.writeUInt16LE(1, 4);   // colour planes
    e.writeUInt16LE(32, 6);  // bits per pixel
    e.writeUInt32LE(data.length, 8);
    e.writeUInt32LE(offset, 12);
    entries.push(e);
    blobs.push(data);
    offset += data.length;
  }
  return Buffer.concat([head, ...entries, ...blobs]);
}

/* ICNS: a length-prefixed container of four-character-code chunks. */
function icns(png, pairs) {
  const chunks = [];
  for (const [kind, size] of pairs) {
    const data = png(size);
    const head = Buffer.alloc(8);
    head.write(kind, 0, 'ascii');
    head.writeUInt32BE(data.length + 8, 4);
    chunks.push(head, data);
  }
  const body = Buffer.concat(chunks);
  const head = Buffer.alloc(8);
  head.write('icns', 0, 'ascii');
  head.writeUInt32BE(body.length + 8, 4);
  return Buffer.concat([head, body]);
}

(async () => {
  let chromium;
  try {
    ({ chromium } = require('playwright'));
  } catch {
    console.error('playwright is not installed here — see the header of this file');
    process.exit(2);
  }

  const browser = await chromium.launch(
    process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {});
  const page = await browser.newPage();
  const data = 'data:image/png;base64,' + fs.readFileSync(SRC).toString('base64');

  const rendered = await page.evaluate(async ({ data, sizes }) => {
    const img = new Image();
    img.src = data;
    await img.decode();

    // 1. Separate ink from paper. The paper is black and the ink is a
    //    bright violet, so brightness alone is a clean mask.
    const src = document.createElement('canvas');
    src.width = img.width; src.height = img.height;
    const sctx = src.getContext('2d');
    sctx.drawImage(img, 0, 0);
    const px = sctx.getImageData(0, 0, src.width, src.height);
    const d = px.data;
    let minX = src.width, minY = src.height, maxX = -1, maxY = -1;
    for (let y = 0; y < src.height; y++) {
      for (let x = 0; x < src.width; x++) {
        const i = (y * src.width + x) * 4;
        const v = Math.max(d[i], d[i + 1], d[i + 2]) * (d[i + 3] / 255);
        // Anything under 24 is paper and anything over 110 is solid ink;
        // the band between the two keeps the drawing's soft edges.
        const a = Math.max(0, Math.min(1, (v - 24) / 86));
        d[i] = 255; d[i + 1] = 255; d[i + 2] = 255;
        d[i + 3] = Math.round(a * 255);
        if (a > 0.08) {
          if (x < minX) minX = x;
          if (x > maxX) maxX = x;
          if (y < minY) minY = y;
          if (y > maxY) maxY = y;
        }
      }
    }
    sctx.putImageData(px, 0, 0);

    // 2. Trim to the ink, so the padding below is padding around the
    //    drawing rather than around whatever margin it was drawn with.
    const w = maxX - minX + 1, h = maxY - minY + 1;
    const art = document.createElement('canvas');
    art.width = w; art.height = h;
    art.getContext('2d').drawImage(src, minX, minY, w, h, 0, 0, w, h);

    const files = {};
    for (const size of sizes) {
      const c = document.createElement('canvas');
      c.width = size; c.height = size;
      const ctx = c.getContext('2d');

      // 3. The tile: the app's panel colour, flat rather than graded so
      //    the PNGs stay small, with the corner radius desktops expect.
      const r = size * 0.22;
      ctx.beginPath();
      ctx.moveTo(r, 0);
      ctx.arcTo(size, 0, size, size, r);
      ctx.arcTo(size, size, 0, size, r);
      ctx.arcTo(0, size, 0, 0, r);
      ctx.arcTo(0, 0, size, 0, r);
      ctx.closePath();
      ctx.fillStyle = '#171222';
      ctx.fill();

      // 4. Fit the drawing inside the tile with room to breathe.
      const pad = size * 0.11;
      const box = size - pad * 2;
      const scale = Math.min(box / w, box / h);
      const dw = w * scale, dh = h * scale;
      const dx = (size - dw) / 2, dy = (size - dh) / 2;

      // 5. Thicken. Scaled down, a stroke drawn six pixels wide at 500px
      //    is a third of a pixel at 32px and fades to nothing, so stamp
      //    the art repeatedly around a small circle. The radius is in
      //    device pixels: generous where the stroke needs the help, and
      //    nothing at all where it is already wide enough.
      const grow = size <= 16 ? 0.45 : size <= 32 ? 0.6 : size <= 48 ? 0.55
        : size <= 64 ? 0.5 : size <= 128 ? 0.35 : size <= 256 ? 0.25 : 0;
      const ink = document.createElement('canvas');
      ink.width = size; ink.height = size;
      const ictx = ink.getContext('2d');
      ictx.imageSmoothingQuality = 'high';
      const stamps = grow > 0 ? 12 : 1;
      for (let s = 0; s < stamps; s++) {
        const t = (s / stamps) * Math.PI * 2;
        const ox = grow > 0 ? Math.cos(t) * grow : 0;
        const oy = grow > 0 ? Math.sin(t) * grow : 0;
        ictx.drawImage(art, dx + ox, dy + oy, dw, dh);
      }
      if (grow > 0) ictx.drawImage(art, dx, dy, dw, dh);

      // 6. Re-ink in the app's own violet, through the mask built above.
      ictx.globalCompositeOperation = 'source-in';
      const g = ictx.createLinearGradient(0, size, size, 0);
      g.addColorStop(0, '#9b5cf6');
      g.addColorStop(1, '#c86bf0');
      ictx.fillStyle = g;
      ictx.fillRect(0, 0, size, size);

      ctx.drawImage(ink, 0, 0);
      files[size] = c.toDataURL('image/png').split(',')[1];
    }
    return files;
  }, { data, sizes: SIZES });

  await browser.close();

  const png = (size) => Buffer.from(rendered[size], 'base64');
  fs.mkdirSync(OUT, { recursive: true });
  for (const [size, name] of Object.entries(NAMED)) {
    fs.writeFileSync(path.join(OUT, name), png(size));
  }
  fs.writeFileSync(path.join(OUT, 'icon.ico'), ico(png, [16, 32, 48, 64, 128, 256]));
  fs.writeFileSync(path.join(OUT, 'icon.icns'), icns(png, [
    ['icp4', 16], ['icp5', 32], ['icp6', 64], ['ic07', 128],
    ['ic08', 256], ['ic09', 512], ['ic10', 1024],
    ['ic11', 32], ['ic12', 64], ['ic13', 256], ['ic14', 512],
  ]));
  console.log('wrote', Object.values(NAMED).concat(['icon.ico', 'icon.icns']).join(', '), 'to', OUT);
})();
