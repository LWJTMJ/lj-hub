//! Generate the LJ Hub app icon source (1024x1024 PNG) without external
//! dependencies: a BMP is written by hand and converted with macOS `sips`.
//!
//! Run: node scripts/make-icon.mjs
//! Produces: src-tauri/icons/source.png which `npx tauri icon` expands into the
//! full icon set.

import { writeFileSync, mkdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = resolve(here, "../src-tauri/icons");
mkdirSync(outDir, { recursive: true });

const S = 1024;
const px = Buffer.alloc(S * S * 4);

const clamp = (v) => Math.max(0, Math.min(255, Math.round(v)));

function setPx(x, y, r, g, b, a = 255) {
  if (x < 0 || y < 0 || x >= S || y >= S) return;
  const i = (y * S + x) * 4;
  // Simple source-over blend onto the existing pixel.
  const sa = a / 255;
  const dr = px[i], dg = px[i + 1], db = px[i + 2], da = px[i + 3] / 255;
  const oa = sa + da * (1 - sa);
  if (oa === 0) return;
  px[i] = clamp((r * sa + dr * da * (1 - sa)) / oa);
  px[i + 1] = clamp((g * sa + dg * da * (1 - sa)) / oa);
  px[i + 2] = clamp((b * sa + db * da * (1 - sa)) / oa);
  px[i + 3] = clamp(oa * 255);
}

// Rounded-square background with a vertical gradient (deep indigo -> violet).
const radius = 224;
const inRounded = (x, y) => {
  const cx = Math.min(Math.max(x, radius), S - radius);
  const cy = Math.min(Math.max(y, radius), S - radius);
  return (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2;
};

for (let y = 0; y < S; y++) {
  for (let x = 0; x < S; x++) {
    if (!inRounded(x, y)) continue;
    const t = y / S;
    setPx(x, y, 32 + 40 * t, 30 + 18 * t, 92 + 90 * t, 255);
  }
}

// "Hub" mark: a centred ring plus three spokes to orbiting nodes.
const cx = S / 2;
const cy = S / 2;
const ringR = 236;
const ringW = 40;
const nodeR = 54;

function drawDisc(ox, oy, r, rC, gC, bC, alpha = 255) {
  for (let y = -r - 1; y <= r + 1; y++) {
    for (let x = -r - 1; x <= r + 1; x++) {
      const d = Math.hypot(x, y);
      const edge = Math.min(1, r - d + 0.5);
      if (edge <= 0) continue;
      setPx(Math.round(ox + x), Math.round(oy + y), rC, gC, bC, alpha * edge);
    }
  }
}

function drawRing(ox, oy, r, w, rC, gC, bC, alpha = 255) {
  for (let y = -r - w; y <= r + w; y++) {
    for (let x = -r - w; x <= r + w; x++) {
      const d = Math.hypot(x, y);
      const edge = Math.min(1, w / 2 - Math.abs(d - r) + 0.5);
      if (edge <= 0) continue;
      setPx(Math.round(ox + x), Math.round(oy + y), rC, gC, bC, alpha * edge);
    }
  }
}

function drawSpoke(angle, length, width, rC, gC, bC) {
  const dx = Math.cos(angle);
  const dy = Math.sin(angle);
  for (let t = ringR - 10; t <= length; t += 1) {
    drawDisc(cx + dx * t, cy + dy * t, width, rC, gC, bC);
  }
}

const spokeAngles = [-Math.PI / 2, Math.PI / 6, (5 * Math.PI) / 6];
for (const angle of spokeAngles) {
  drawSpoke(angle, 348, 18, 235, 240, 255);
}
for (const angle of spokeAngles) {
  drawDisc(cx + Math.cos(angle) * 348, cy + Math.sin(angle) * 348, nodeR, 168, 224, 255);
}
drawRing(cx, cy, ringR, ringW, 245, 247, 255);
drawDisc(cx, cy, 96, 168, 224, 255);

// Encode as a 32-bit BMP (bottom-up) so `sips` can convert it to PNG.
const rowSize = S * 4;
const pixelDataSize = rowSize * S;
const fileSize = 54 + pixelDataSize;
const bmp = Buffer.alloc(fileSize);
bmp.write("BM", 0, "ascii");
bmp.writeUInt32LE(fileSize, 2);
bmp.writeUInt32LE(54, 10);
bmp.writeUInt32LE(40, 14);
bmp.writeInt32LE(S, 18);
bmp.writeInt32LE(S, 22);
bmp.writeUInt16LE(1, 26);
bmp.writeUInt16LE(32, 28);
bmp.writeUInt32LE(pixelDataSize, 34);
for (let y = 0; y < S; y++) {
  const srcRow = (S - 1 - y) * S * 4;
  const dstRow = 54 + y * rowSize;
  for (let x = 0; x < S; x++) {
    const s = srcRow + x * 4;
    const d = dstRow + x * 4;
    // BGRA with premultiplied-free straight alpha.
    bmp[d] = px[s + 2];
    bmp[d + 1] = px[s + 1];
    bmp[d + 2] = px[s];
    bmp[d + 3] = px[s + 3];
  }
}

const bmpPath = resolve(outDir, "source.bmp");
const pngPath = resolve(outDir, "source.png");
writeFileSync(bmpPath, bmp);
execFileSync("sips", ["-s", "format", "png", bmpPath, "--out", pngPath], {
  stdio: "inherit",
});
console.log(`wrote ${pngPath}`);
