import { useEffect, useRef } from "react";
import { isLight } from "../lib/theme";
import { isLowSpec, PERF_EVENT } from "../lib/perf";

interface Star {
  x: number;
  y: number;
  r: number;
  a: number;
  layer: 0 | 1 | 2;
  tw: number; // twinkle speed (rad/s)
  ph: number;
  /** index into TINTS; stars are sorted by it so the fill color changes only a few times per frame */
  ti: number;
}

interface Meteor {
  x: number;
  y: number;
  vx: number;
  vy: number;
  life: number;
  ttl: number;
}

const FPS = 30;
/** Drift speed per layer (px/s): far stars move slowest, which reads as depth. */
const DRIFT = [1.2, 2.6, 4.6];
const TINTS: [number, number, number][] = [
  [235, 245, 255],
  [210, 228, 255],
  [255, 244, 225],
  [200, 245, 236],
];

function makeStars(w: number, h: number): Star[] {
  const n = Math.round(Math.min(700, Math.max(200, (w * h) / 3000)));
  const out: Star[] = [];
  for (let i = 0; i < n; i++) {
    const roll = Math.random();
    const layer: 0 | 1 | 2 = roll < 0.72 ? 0 : roll < 0.95 ? 1 : 2;
    out.push({
      x: Math.random() * w,
      y: Math.random() * h,
      r: layer === 0 ? 0.35 + Math.random() * 0.5 : layer === 1 ? 0.7 + Math.random() * 0.6 : 1.1 + Math.random() * 0.8,
      a: layer === 0 ? 0.3 + Math.random() * 0.4 : layer === 1 ? 0.5 + Math.random() * 0.35 : 0.75 + Math.random() * 0.25,
      layer,
      tw: 0.4 + Math.random() * 1.4,
      ph: Math.random() * Math.PI * 2,
      ti: (Math.random() * TINTS.length) | 0,
    });
  }
  return out.sort((a, b) => a.ti - b.ti);
}

/** Night sky behind the whole window: three layers of slowly drifting, softly twinkling stars and a rare shooting star. */
export default function Starfield() {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current!;
    const ctx = canvas.getContext("2d")!;
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    let w = 0;
    let h = 0;
    let dpr = 1;
    let stars: Star[] = [];
    let meteor: Meteor | null = null;
    let nextMeteor = performance.now() + 8000 + Math.random() * 10000;
    let raf = 0;
    let last = performance.now();
    let acc = 0;

    const resize = () => {
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      w = window.innerWidth;
      h = window.innerHeight;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      stars = makeStars(w, h);
    };

    // Glow for the nearest stars, rendered once and reused (no gradient objects per frame).
    const glow = document.createElement("canvas");
    glow.width = glow.height = 32;
    {
      const g = glow.getContext("2d")!;
      const grad = g.createRadialGradient(16, 16, 0, 16, 16, 16);
      grad.addColorStop(0, "rgba(255,255,255,0.55)");
      grad.addColorStop(1, "rgba(255,255,255,0)");
      g.fillStyle = grad;
      g.fillRect(0, 0, 32, 32);
    }

    const draw = (t: number, dt: number) => {
      const light = isLight();
      ctx.clearRect(0, 0, w, h);
      const sec = t / 1000;
      let tint = -1;
      for (const s of stars) {
        s.x -= DRIFT[s.layer] * dt;
        if (s.x < -4) s.x += w + 8;
        // Gentle twinkle: brightness swings within ±22%, never blinks off.
        const k = reduce ? 1 : 0.78 + 0.22 * Math.sin(sec * s.tw + s.ph);
        const a = s.a * k * (light ? 0.42 : 1);
        if (s.ti !== tint) {
          tint = s.ti;
          const [r, g, b] = light ? [70, 92, 110] : TINTS[tint];
          ctx.fillStyle = `rgb(${r},${g},${b})`;
        }
        ctx.globalAlpha = a;
        if (s.layer === 0) {
          const d = s.r * 2;
          ctx.fillRect(s.x - s.r, s.y - s.r, d, d);
          continue;
        }
        if (s.layer === 2 && !light) {
          const gs = s.r * 10;
          ctx.globalAlpha = a * 0.6;
          ctx.drawImage(glow, s.x - gs / 2, s.y - gs / 2, gs, gs);
          ctx.globalAlpha = a;
        }
        ctx.beginPath();
        ctx.arc(s.x, s.y, s.r, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.globalAlpha = 1;
      if (light || reduce) return;
      if (!meteor && t > nextMeteor) {
        const ang = (200 + Math.random() * 25) * (Math.PI / 180);
        const speed = 520 + Math.random() * 260;
        meteor = { x: w * (0.35 + Math.random() * 0.6), y: h * Math.random() * 0.35, vx: Math.cos(ang) * speed, vy: -Math.sin(ang) * speed, life: 0, ttl: 0.9 + Math.random() * 0.5 };
      }
      if (meteor) {
        meteor.life += dt;
        meteor.x += meteor.vx * dt;
        meteor.y += meteor.vy * dt;
        const p = meteor.life / meteor.ttl;
        const fade = p < 0.2 ? p / 0.2 : 1 - (p - 0.2) / 0.8;
        const len = 0.16;
        const tx = meteor.x - meteor.vx * len;
        const ty = meteor.y - meteor.vy * len;
        const grad = ctx.createLinearGradient(meteor.x, meteor.y, tx, ty);
        grad.addColorStop(0, `rgba(230,255,250,${0.75 * fade})`);
        grad.addColorStop(1, "rgba(158,234,217,0)");
        ctx.strokeStyle = grad;
        ctx.lineWidth = 1.4;
        ctx.lineCap = "round";
        ctx.beginPath();
        ctx.moveTo(meteor.x, meteor.y);
        ctx.lineTo(tx, ty);
        ctx.stroke();
        if (p >= 1) {
          meteor = null;
          nextMeteor = t + 14000 + Math.random() * 18000;
        }
      }
    };

    const frame = (t: number) => {
      raf = requestAnimationFrame(frame);
      const dt = Math.min(0.1, (t - last) / 1000);
      last = t;
      // Paused while hidden or in sleep mode; capped at 30 fps to keep the GPU quiet.
      if (document.hidden || document.documentElement.dataset.sleep != null || isLowSpec()) return;
      acc += dt;
      if (acc < 1 / FPS) return;
      draw(t, acc);
      acc = 0;
    };

    resize();
    draw(performance.now(), 0);
    if (!reduce) raf = requestAnimationFrame(frame);
    const onResize = () => {
      resize();
      draw(performance.now(), 0);
    };
    window.addEventListener("resize", onResize);
    const mo = new MutationObserver(() => draw(performance.now(), 0));
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    // Low-spec mode keeps one still frame; the loop above skips drawing while it is on.
    const onPerf = () => draw(performance.now(), 0);
    window.addEventListener(PERF_EVENT, onPerf);
    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("resize", onResize);
      window.removeEventListener(PERF_EVENT, onPerf);
      mo.disconnect();
    };
  }, []);
  return <canvas ref={ref} className="starfield" aria-hidden />;
}
