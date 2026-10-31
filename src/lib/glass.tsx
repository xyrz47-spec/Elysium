// 리퀴드 글라스(애플 iOS 26 / macOS 26 방식).
// 1) 굴절: 판 가장자리를 볼록한 스퀴클 렌즈로 보고(높이 f(x) = (1-(1-x)^4)^(1/4)), 스넬 법칙(n=1.5)으로 구한 밀림을
//    SVG feDisplacementMap 지도로 만들어 backdrop-filter에 건다.
// 2) 지도는 9조각: 모서리 4장 + 변 4줄(늘려 씀) + 가운데 단색. 조각은 (둥근 정도, 렌즈 폭)에만 달려 있어 한 번만 만들고,
//    판 크기가 바뀌면 필터의 좌표만 바꾼다(픽셀 계산·PNG 인코딩 없음).
// 3) 반사광: 포인터 위치(--mx, --my)를 따라 움직이고, 누르면 판 안에서 빛난다(CSS).
import { useEffect } from "react";

/** 굴절을 거는 판: 늘 떠 있는 크롬만(애플 규칙: 유리는 내비게이션·컨트롤 층에만, 내용 층에는 쓰지 않는다). */
const LIQUID = ".dock, .composer, .head-folder, .welcome-cards button";
/** 포인터 반사광이 따라오는 판 */
const LIGHT = LIQUID;
const IOR = 1.5;
const LUT = 128;

/** 볼록 스퀴클 렌즈 단면: 가장자리(0)에서 안쪽(1)으로 갈 때 밀림의 세기(0~1). 스넬 법칙으로 미리 계산해 둔다. */
const PROFILE = (() => {
  const f = (x: number) => Math.pow(1 - Math.pow(1 - Math.min(Math.max(x, 0), 1), 4), 0.25);
  const out = new Float32Array(LUT);
  const dx = 1e-3;
  let max = 0;
  for (let i = 0; i < LUT; i++) {
    const x = i / (LUT - 1);
    const slope = (f(x + dx) - f(x - dx)) / (2 * dx);
    const t1 = Math.atan(slope); // 법선과 수직선 사이 각
    const t2 = Math.asin(Math.sin(t1) / IOR);
    // 판 두께(가장자리도 얇은 바닥이 있다) × 꺾인 각도
    const v = Math.tan(t1 - t2) * (0.35 + f(x));
    out[i] = v;
    if (v > max) max = v;
  }
  for (let i = 0; i < LUT; i++) out[i] /= max || 1;
  return out;
})();

function profile(t: number) {
  if (t <= 0) return 1;
  if (t >= 1) return 0;
  const p = t * (LUT - 1);
  const i = Math.floor(p);
  const k = p - i;
  return PROFILE[i] * (1 - k) + PROFILE[Math.min(i + 1, LUT - 1)] * k;
}

/** 둥근 사각형의 부호 있는 거리(안쪽이 음수) */
function sdf(x: number, y: number, w: number, h: number, r: number) {
  const qx = Math.abs(x - w / 2) - (w / 2 - r);
  const qy = Math.abs(y - h / 2) - (h / 2 - r);
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
}

// Little-endian RGBA: 128,128,128,255 = 밀림 없음
const NEUTRAL = 0xff808080;

/** (x, y)의 지도 값. 가상의 2m×2m 판의 왼쪽 위 모서리 기준으로 계산하고, 오른쪽·아래는 부호를 뒤집어 쓴다. */
function texel(x: number, y: number, m: number, r: number, bezel: number, fx: number, fy: number) {
  const s = 2 * m;
  const px = x + 0.5;
  const py = y + 0.5;
  const inside = -sdf(px, py, s, s, r);
  if (inside < 0 || inside >= bezel) return inside < 0 ? edgeTexel(px, py, s, r, fx, fy) : NEUTRAL;
  return dispTexel(px, py, s, r, profile(inside / bezel), fx, fy);
}

/** 판 밖(안티에일리어싱 1px)은 가장 센 밀림으로 둔다: 테두리에 빈 틈이 보이지 않게. */
function edgeTexel(px: number, py: number, s: number, r: number, fx: number, fy: number) {
  return dispTexel(px, py, s, r, 1, fx, fy);
}

function dispTexel(px: number, py: number, s: number, r: number, mag: number, fx: number, fy: number) {
  const nx = sdf(px + 0.5, py, s, s, r) - sdf(px - 0.5, py, s, s, r);
  const ny = sdf(px, py + 0.5, s, s, r) - sdf(px, py - 0.5, s, s, r);
  const len = Math.hypot(nx, ny) || 1;
  // 안쪽을 보게(렌즈 쪽으로) 민다
  return pack((-nx / len) * mag * fx, (-ny / len) * mag * fy);
}

function toURL(w: number, h: number, fill: (buf: Uint32Array) => void): string {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const ctx = c.getContext("2d")!;
  const img = ctx.createImageData(w, h);
  fill(new Uint32Array(img.data.buffer));
  ctx.putImageData(img, 0, 0);
  return c.toDataURL("image/png");
}

/** 변 조각을 늘려 쓸 때 가장자리 보간이 비지 않게 길이 방향으로 4px 만든다. */
const STRIP = 4;

interface Tiles {
  m: number;
  tl: string;
  tr: string;
  bl: string;
  br: string;
  top: string;
  bottom: string;
  left: string;
  right: string;
}

const tileCache = new Map<string, Tiles>();

/** (모서리 반지름, 렌즈 폭)마다 9조각을 한 번만 만든다. m은 모서리 조각의 한 변. */
function tiles(r: number, bezel: number): Tiles {
  const key = `${r}:${bezel}`;
  const hit = tileCache.get(key);
  if (hit) return hit;
  const m = Math.max(1, Math.ceil(Math.max(r, bezel)));
  const corner = (fx: number, fy: number, flipX: boolean, flipY: boolean) =>
    toURL(m, m, (b) => {
      for (let y = 0; y < m; y++)
        for (let x = 0; x < m; x++) {
          const v = texel(flipX ? m - 1 - x : x, flipY ? m - 1 - y : y, m, r, bezel, fx, fy);
          b[y * m + x] = v;
        }
    });
  // 변: 모서리 조각의 가운데 끝(m-1) 줄은 둥근 부분을 벗어나 곧은 변과 같다.
  const hStrip = (fy: number, flipY: boolean) =>
    toURL(STRIP, m, (b) => {
      for (let y = 0; y < m; y++) {
        const yy = flipY ? m - 1 - y : y;
        const inside = yy + 0.5;
        const v = inside >= bezel ? NEUTRAL : pack(0, profile(inside / bezel) * fy);
        for (let x = 0; x < STRIP; x++) b[y * STRIP + x] = v;
      }
    });
  const vStrip = (fx: number, flipX: boolean) =>
    toURL(m, STRIP, (b) => {
      for (let x = 0; x < m; x++) {
        const xx = flipX ? m - 1 - x : x;
        const inside = xx + 0.5;
        const v = inside >= bezel ? NEUTRAL : pack(profile(inside / bezel) * fx, 0);
        for (let y = 0; y < STRIP; y++) b[y * m + x] = v;
      }
    });
  const t: Tiles = {
    m,
    tl: corner(1, 1, false, false),
    tr: corner(-1, 1, true, false),
    bl: corner(1, -1, false, true),
    br: corner(-1, -1, true, true),
    top: hStrip(1, false),
    bottom: hStrip(-1, true),
    left: vStrip(1, false),
    right: vStrip(-1, true),
  };
  if (tileCache.size > 32) tileCache.delete(tileCache.keys().next().value as string);
  tileCache.set(key, t);
  return t;
}

/** dx, dy(-1~1) → 지도 값 */
function pack(dx: number, dy: number) {
  const R = Math.round(128 + dx * 127);
  const G = Math.round(128 + dy * 127);
  return (0xff000000 | (128 << 16) | (G << 8) | R) >>> 0;
}

const NS = "http://www.w3.org/2000/svg";

function node(name: string, attrs: Record<string, string | number>) {
  const n = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
  return n;
}

function image(href: string, x: number, y: number, w: number, h: number, result: string) {
  const n = node("feImage", { x, y, width: w, height: h, preserveAspectRatio: "none", result });
  n.setAttribute("href", href);
  return n;
}

let seq = 0;

/** w×h, 반지름 r 판의 굴절 필터. 조각을 제자리에 놓고 합친 지도로 뒤를 민다. */
function buildFilter(w: number, h: number, r: number) {
  const short = Math.min(w, h);
  // 렌즈 폭: 판이 작을수록 판 전체가 물방울처럼 휜다(애플 툴바 버튼). 큰 판은 가장자리만.
  const bezel = Math.max(6, Math.min(Math.round(short * 0.36), 24, Math.floor(short / 2)));
  const rr = Math.min(r, Math.floor(short / 2));
  const t = tiles(rr, bezel);
  const m = t.m;
  const id = `lg-${++seq}`;
  const f = node("filter", { id, x: 0, y: 0, width: w, height: h, filterUnits: "userSpaceOnUse", primitiveUnits: "userSpaceOnUse", "color-interpolation-filters": "sRGB" });
  f.appendChild(node("feFlood", { x: 0, y: 0, width: w, height: h, "flood-color": "rgb(128,128,128)", result: "c" }));
  const parts = ["c"];
  const add = (href: string, x: number, y: number, iw: number, ih: number) => {
    if (iw <= 0 || ih <= 0) return;
    const name = `p${parts.length}`;
    f.appendChild(image(href, x, y, iw, ih, name));
    parts.push(name);
  };
  const mw = w - 2 * m;
  const mh = h - 2 * m;
  add(t.top, m, 0, mw, m);
  add(t.bottom, m, h - m, mw, m);
  add(t.left, 0, m, m, mh);
  add(t.right, w - m, m, m, mh);
  add(t.tl, 0, 0, m, m);
  add(t.tr, w - m, 0, m, m);
  add(t.bl, 0, h - m, m, m);
  add(t.br, w - m, h - m, m, m);
  const merge = node("feMerge", { result: "map" });
  for (const p of parts) merge.appendChild(node("feMergeNode", { in: p }));
  f.appendChild(merge);
  // 밀림의 최대(px): 렌즈 폭에 비례
  f.appendChild(node("feDisplacementMap", { in: "SourceGraphic", in2: "map", scale: Math.round(bezel * 1.7), xChannelSelector: "R", yChannelSelector: "G" }));
  return f;
}

/** 포인터 반사광. 프레임마다 한 번, 포인터 아래의 유리 판에만 좌표를 쓴다. */
export function useGlassLight() {
  useEffect(() => {
    let raf = 0;
    let last: PointerEvent | null = null;
    let lastEl: HTMLElement | null = null;
    let rect: DOMRect | null = null;
    const paint = () => {
      raf = 0;
      const e = last;
      const el = e && e.target instanceof Element ? (e.target.closest(LIGHT) as HTMLElement | null) : null;
      if (!e || !el) {
        lastEl = null;
        return;
      }
      // 같은 판 위에서 움직이는 동안은 위치를 다시 재지 않는다(강제 레이아웃 방지).
      if (el !== lastEl || !rect) {
        lastEl = el;
        rect = el.getBoundingClientRect();
      }
      el.style.setProperty("--mx", `${Math.round(e.clientX - rect.left)}px`);
      el.style.setProperty("--my", `${Math.round(e.clientY - rect.top)}px`);
    };
    const onMove = (e: PointerEvent) => {
      last = e;
      if (!raf) raf = requestAnimationFrame(paint);
    };
    const reset = () => {
      rect = null;
    };
    window.addEventListener("pointermove", onMove, { passive: true });
    window.addEventListener("scroll", reset, { passive: true, capture: true });
    window.addEventListener("resize", reset, { passive: true });
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("scroll", reset, { capture: true });
      window.removeEventListener("resize", reset);
      cancelAnimationFrame(raf);
    };
  }, []);
}

/** 늘 보이는 유리 판마다 크기에 맞는 굴절 필터를 걸고, 크기가 바뀌면 좌표만 바꾼 새 필터로 갈아 끼운다. */
export function useLiquidRefraction() {
  useEffect(() => {
    const defs = document.getElementById("lg-defs");
    if (!defs) return;
    const live = new Map<HTMLElement, { key: string; f: SVGElement }>();
    const dirty = new Set<HTMLElement>();
    let raf = 0;
    const flush = () => {
      raf = 0;
      for (const e of dirty) {
        if (!e.isConnected) continue;
        const w = Math.round(e.offsetWidth);
        const h = Math.round(e.offsetHeight);
        if (w < 8 || h < 8) continue;
        const r = Math.round(Math.min(parseFloat(getComputedStyle(e).borderTopLeftRadius) || 0, w / 2, h / 2));
        const key = `${w}x${h}x${r}`;
        const cur = live.get(e);
        if (cur?.key === key) continue;
        const f = buildFilter(w, h, r);
        defs.appendChild(f);
        e.style.setProperty("--lg-filter", `url(#${f.id})`);
        cur?.f.remove();
        live.set(e, { key, f });
      }
      dirty.clear();
    };
    const mark = (e: HTMLElement) => {
      dirty.add(e);
      if (!raf) raf = requestAnimationFrame(flush);
    };
    const ro = new ResizeObserver((entries) => {
      for (const en of entries) mark(en.target as HTMLElement);
    });
    const scan = () => {
      // 사라진 판의 필터는 지운다.
      for (const [e, v] of live)
        if (!e.isConnected) {
          v.f.remove();
          live.delete(e);
          ro.unobserve(e);
        }
      document.querySelectorAll<HTMLElement>(LIQUID).forEach((e) => {
        if (live.has(e) || dirty.has(e)) return;
        ro.observe(e);
        mark(e);
      });
    };
    scan();
    // 세션을 바꾸거나 첫 화면이 다시 뜰 때 새로 생긴 판도 잡는다(스트리밍 중 잦은 변경은 프레임당 한 번으로 묶는다).
    let pending = 0;
    const mo = new MutationObserver(() => {
      if (!pending) pending = window.setTimeout(() => ((pending = 0), scan()), 120);
    });
    mo.observe(document.getElementById("root") ?? document.body, { childList: true, subtree: true });
    return () => {
      ro.disconnect();
      mo.disconnect();
      cancelAnimationFrame(raf);
      clearTimeout(pending);
      for (const v of live.values()) v.f.remove();
    };
  }, []);
}

/** 굴절 필터가 들어가는 자리 */
export function GlassDefs() {
  return (
    <svg className="glass-defs" aria-hidden focusable="false">
      <defs id="lg-defs" />
    </svg>
  );
}
