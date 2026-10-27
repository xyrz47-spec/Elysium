// Elysium 아이콘 원본(1024px PNG)을 만든다. 회색 계열 + 청록 한 방울.
// 먹색 둥근 사각형 위에 은회색 구체, 옆에 청록 작은 구(두 AI가 이어서 일한다는 뜻).
// 사용: node scripts/gen-icon.mjs && npx tauri icon app-icon.png
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const N = 1024;
const c = N / 2;
const NIGHT = [20, 21, 24];
const NAVY = [44, 46, 52];
const SLATE = [93, 97, 104];
const TEAL = [214, 218, 223]; // 큰 구: 은회색
const AQUA = [121, 184, 179]; // 작은 구·반사광: 청록
const WHITE = [250, 251, 252];

const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const mix = (a, b, t) => a.map((v, i) => v + (b[i] - v) * t);
// 경계를 1px 폭으로 부드럽게 만든 덮임 정도(안티에일리어싱)
const cover = (dist) => clamp(0.5 - dist);

/** 모서리가 둥근 사각형까지의 부호 있는 거리 */
function roundRect(x, y, half, radius) {
  const qx = Math.abs(x) - (half - radius);
  const qy = Math.abs(y) - (half - radius);
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - radius;
}

/** 빛이 왼쪽 위에서 오는 구체의 색 */
function sphere(nx, ny, base, light, shadow) {
  const nz = Math.sqrt(Math.max(0, 1 - nx * nx - ny * ny));
  const L = [-0.45, -0.55, 0.7];
  const len = Math.hypot(...L);
  const diffuse = clamp((nx * L[0] + ny * L[1] + nz * L[2]) / len);
  const spec = Math.pow(clamp((nx * -0.3 + ny * -0.4 + nz * 0.86)), 40);
  let col = mix(shadow, base, clamp(diffuse * 1.15));
  col = mix(col, light, spec * 0.85);
  // 가장자리 반사광(아래 오른쪽)
  const rim = Math.pow(1 - nz, 3) * clamp(nx * 0.6 + ny * 0.8);
  return mix(col, AQUA, rim * 0.45);
}

const raw = Buffer.alloc((N * 4 + 1) * N);
for (let y = 0; y < N; y++) {
  raw[y * (N * 4 + 1)] = 0;
  for (let x = 0; x < N; x++) {
    const px = x - c + 0.5;
    const py = y - c + 0.5;
    let rgb = [0, 0, 0];
    let a = 0;
    const over = (col, alpha) => {
      rgb = mix(rgb, col, alpha / Math.max(a + alpha * (1 - a), 1e-6) * (a === 0 ? 1 : 1));
      a = a + alpha * (1 - a);
    };

    // 1) 바탕: 연어색 → 파우더 블러시 대각선, 아래쪽은 살짝 깊게
    const bg = roundRect(px, py, 470, 210);
    const t = clamp((px + py) / 1300 + 0.5);
    let bgc = mix(NAVY, NIGHT, t);
    // 구체 뒤의 은은한 청록 빛
    bgc = mix(bgc, SLATE, 0.55 * Math.exp(-Math.pow(Math.hypot(px + 30, py + 10) / 420, 2)));
    over(bgc, cover(bg));

    // 2) 구체 그림자
    const sd = Math.hypot(px - 10, py - 40);
    over(NIGHT, 0.5 * Math.exp(-Math.pow(Math.max(0, sd - 200) / 95, 2)));

    // 3) 스노 구체
    const R = 270;
    const sx = px + 30;
    const sy = py + 10;
    const d = Math.hypot(sx, sy) - R;
    if (d < 1) over(sphere(sx / R, sy / R, TEAL, WHITE, mix(SLATE, NAVY, 0.4)), cover(d));

    // 4) 아쿠아마린 작은 구(오른쪽 위)
    const r2 = 92;
    const ax = px - 250;
    const ay = py + 250;
    const d2 = Math.hypot(ax, ay) - r2;
    // 작은 구 둘레의 스노 테두리(구체와 떨어져 보이게)
    const ring = Math.hypot(ax, ay) - (r2 + 16);
    if (ring < 1) over(mix(NAVY, NIGHT, 0.5), cover(ring));
    // 작은 구의 빛 번짐
    over(AQUA, 0.35 * Math.exp(-Math.pow(Math.max(0, Math.hypot(ax, ay) - r2) / 40, 2)) * (ring >= 1 ? 1 : 0));
    if (d2 < 1) over(sphere(ax / r2, ay / r2, AQUA, WHITE, mix(TEAL, SLATE, 0.5)), cover(d2));

    raw.set([Math.round(rgb[0]), Math.round(rgb[1]), Math.round(rgb[2]), Math.round(a * 255)], y * (N * 4 + 1) + 1 + x * 4);
  }
}

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let k = n;
  for (let i = 0; i < 8; i++) k = k & 1 ? 0xedb88320 ^ (k >>> 1) : k >>> 1;
  return k >>> 0;
});
function crc32(buf) {
  let crc = 0xffffffff;
  for (const b of buf) crc = crcTable[(crc ^ b) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(N, 0);
ihdr.writeUInt32BE(N, 4);
ihdr[8] = 8;
ihdr[9] = 6;
const png = Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw, { level: 9 })), chunk("IEND", Buffer.alloc(0))]);
writeFileSync("app-icon.png", png);
console.log("app-icon.png", png.length, "bytes");
