// 生成应用图标源图：512×512 PNG（圆角方块 + 白色放大镜），纯 Node 实现
// 产物：scripts/app-icon.png → 供 `npx tauri icon` 生成全套图标
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const W = 512;
const px = Buffer.alloc(W * W * 4);

// ---------- SDF 工具（d<0 在内部，返回值即带符号距离） ----------
function sdRoundRect(x, y, cx, cy, hw, hh, r) {
  const qx = Math.abs(x - cx) - (hw - r);
  const qy = Math.abs(y - cy) - (hh - r);
  const ax = Math.max(qx, 0), ay = Math.max(qy, 0);
  return Math.hypot(ax, ay) + Math.min(Math.max(qx, qy), 0) - r;
}
function sdRing(x, y, cx, cy, rMid, half) {
  return Math.hypot(x - cx, y - cy) - rMid - half; // abs 包络在调用处处理
}
function sdSegment(x, y, ax, ay, bx, by, half) {
  const vx = bx - ax, vy = by - ay;
  const len2 = vx * vx + vy * vy;
  let t = ((x - ax) * vx + (y - ay) * vy) / len2;
  t = Math.min(1, Math.max(0, t));
  return Math.hypot(x - (ax + vx * t), y - (ay + vy * t)) - half;
}
/** 带符号距离 → 抗锯齿覆盖度（1px 过渡带） */
function cover(d) {
  return Math.min(1, Math.max(0, 0.5 - d));
}
function blendOver(idx, color, alpha) {
  if (alpha <= 0) return;
  px[idx] = Math.round(color[0] * alpha + px[idx] * (1 - alpha));
  px[idx + 1] = Math.round(color[1] * alpha + px[idx + 1] * (1 - alpha));
  px[idx + 2] = Math.round(color[2] * alpha + px[idx + 2] * (1 - alpha));
  px[idx + 3] = Math.min(255, Math.round(Math.max(px[idx + 3] / 255, alpha) * 255));
}

// ---------- 绘制：渐变圆角底 + 白色放大镜（圆环 + 柄） ----------
const c1 = [0x4f, 0x8c, 0xff], c2 = [0x7b, 0x5c, 0xff];
const WHITE = [255, 255, 255];
for (let y = 0; y < W; y++) {
  for (let x = 0; x < W; x++) {
    const i = (y * W + x) * 4;
    const fx = x + 0.5, fy = y + 0.5;
    const dBox = sdRoundRect(fx, fy, W / 2, W / 2, 240, 240, 110);
    const aBg = cover(dBox);
    if (aBg <= 0) continue;
    const t = fy / W;
    const bg = [
      c1[0] + (c2[0] - c1[0]) * t,
      c1[1] + (c2[1] - c1[1]) * t,
      c1[2] + (c2[2] - c1[2]) * t,
    ];
    blendOver(i, bg, aBg);
    // 放大镜：圆环（rMid=110, half=18）与柄（half=21）取并集
    const ring = Math.abs(Math.hypot(fx - 225, fy - 225) - 110) - 18;
    const handle = sdSegment(fx, fy, 300, 300, 386, 386, 21);
    const aWhite = cover(Math.min(ring, handle));
    blendOver(i, WHITE, aWhite);
  }
}

// ---------- PNG 编码 ----------
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(W, 0);
ihdr.writeUInt32BE(W, 4);
ihdr[8] = 8;  // bit depth
ihdr[9] = 6;  // RGBA
// 每行前加 filter 字节 0
const raw = Buffer.alloc(W * (W * 4 + 1));
for (let y = 0; y < W; y++) {
  raw[y * (W * 4 + 1)] = 0;
  px.copy(raw, y * (W * 4 + 1) + 1, y * W * 4, (y + 1) * W * 4);
}
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "app-icon.png"), png);
console.log("OK:", join(here, "app-icon.png"));
