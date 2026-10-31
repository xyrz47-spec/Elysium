import { useEffect, useRef } from "react";
import { isLight } from "../lib/theme";

export type OrbMode = "off" | "idle" | "thinking" | "approval" | "error";

type Rgb = [number, number, number];

interface Look {
  a: Rgb; // 위쪽 색
  b: Rgb; // 아래쪽 색
  energy: number; // 표면 일렁임
  spin: number; // 회전 속도
  swirl: number; // 위도별로 엇갈려 도는 정도(처리 중)
  spread: number; // 입자가 퍼지는 정도
  glitch: number;
}

// 회백색(.86,.88,.9) 구에 청록(.47,.72,.70)을 살짝 섞는다.
const LOOK: Record<OrbMode, Look> = {
  off: { a: [0.6, 0.62, 0.66], b: [0.4, 0.42, 0.46], energy: 0.03, spin: 0.05, swirl: 0, spread: 0, glitch: 0 },
  idle: { a: [0.88, 0.9, 0.92], b: [0.47, 0.72, 0.7], energy: 0.07, spin: 0.16, swirl: 0, spread: 0, glitch: 0 },
  thinking: { a: [0.88, 0.9, 0.92], b: [0.47, 0.72, 0.7], energy: 0.12, spin: 0.9, swirl: 1.0, spread: 0.04, glitch: 0 },
  approval: { a: [0.88, 0.7, 0.42], b: [0.6, 0.62, 0.66], energy: 0.1, spin: 0.2, swirl: 0, spread: 0, glitch: 0 },
  error: { a: [0.91, 0.47, 0.44], b: [0.4, 0.42, 0.46], energy: 0.08, spin: 0.08, swirl: 0, spread: 0.05, glitch: 1 },
};

// 구(0) + 둘레 먼지(1) + 창 전체로 퍼지는 성운(2). 성운이 바탕과 구를 하나로 잇는다.
const SPHERE = 5200;
const HALO = 2600;
const FIELD = 4200;

const VS = `#version 300 es
in vec3 aPos;
in float aSeed;
in float aKind;
uniform float uTime, uRot, uSwirl, uEnergy, uSpread, uAssemble, uGlitch, uPx, uRadius, uDim;
uniform vec2 uCenter, uRes;
uniform vec3 uA, uB;
out vec3 vColor;
out float vAlpha;

mat3 rotY(float a) { float c = cos(a), s = sin(a); return mat3(c, 0., -s, 0., 1., 0., s, 0., c); }
mat3 rotX(float a) { float c = cos(a), s = sin(a); return mat3(1., 0., 0., 0., c, s, 0., -s, c); }

void main() {
  vec3 p = aPos;
  float n = 0.0;
  float r0 = length(aPos);
  if (aKind < 0.5) {
    // 위도마다 다른 속도로 돌면 소용돌이처럼 보인다.
    float ang = uRot + uSwirl * p.y * 2.2 * sin(uTime * 0.7 + p.y * 3.0);
    p = rotY(ang) * p;
    n = sin(p.x * 3.1 + uTime * 1.3 + aSeed * 6.0) * sin(p.y * 4.3 - uTime * 1.1) * sin(p.z * 3.7 + uTime * 0.9);
    p *= 1.0 + uEnergy * n * 1.6 + uSpread * aSeed;
  } else if (aKind < 1.5) {
    // 둘레 먼지: 구보다 느리게 돌며 천천히 숨 쉰다.
    p = rotY(uRot * 0.35 + aSeed * 0.2) * p;
    p *= 1.0 + 0.05 * sin(uTime * 0.5 + aSeed * 30.0);
    p.z *= 0.55;
  } else {
    // 성운: 아주 느리게 돌고 깜박인다. 앞뒤로 납작해서 화면 가득 고르게 퍼진다.
    p = rotY(uRot * 0.12 + aSeed * 0.05) * p;
    p.y += 0.06 * sin(uTime * 0.15 + aSeed * 20.0);
    p.z *= 0.3;
  }
  p = rotX(0.32) * p;
  // 처음 켤 때 흩어진 입자가 모여든다.
  vec3 start = normalize(aPos + vec3(aSeed - 0.5, fract(aSeed * 7.3) - 0.5, fract(aSeed * 3.1) - 0.5)) * (3.0 + aSeed * 3.0);
  float e = 1.0 - pow(1.0 - clamp(uAssemble * (1.2 - aSeed * 0.4), 0.0, 1.0), 3.0);
  if (aKind > 1.5) e = clamp(uAssemble * 1.5 - aSeed * 0.5, 0.0, 1.0);
  else p = mix(start, p, e);
  p.x += uGlitch * step(0.92, fract(sin(floor(uTime * 12.0) + floor(p.y * 9.0)) * 43758.5)) * 0.12;

  float persp = 2.0 / (3.2 - p.z);
  // 화면 픽셀 좌표: 구 중심(uCenter)에서 반지름(uRadius)만큼
  vec2 s = uCenter + vec2(p.x, -p.y) * persp * 1.35 * uRadius;
  gl_Position = vec4(s.x / uRes.x * 2.0 - 1.0, 1.0 - s.y / uRes.y * 2.0, 0.0, 1.0);
  float front = clamp((p.z + 1.2) / 2.4, 0.0, 1.0);
  float size = aKind < 0.5 ? 1.6 + aSeed * 2.2 : aKind < 1.5 ? 0.9 + aSeed * 1.6 : 0.7 + aSeed * 1.5;
  gl_PointSize = size * persp * uPx * (0.7 + front * 0.6);
  vColor = aKind < 0.5 ? mix(uB, uA, clamp(aPos.y * 0.5 + 0.5 + n * 0.25, 0.0, 1.0)) : mix(uB, uA, aSeed);
  float a;
  if (aKind < 0.5) a = uDim;
  else if (aKind < 1.5) a = 0.45 * (1.0 - smoothstep(1.05, 1.72, r0)) * mix(1.0, uDim, 0.5);
  else a = (0.16 + 0.22 * (1.0 - smoothstep(1.6, 7.5, r0))) * (0.55 + 0.45 * sin(uTime * 0.7 + aSeed * 40.0));
  vAlpha = (0.3 + 0.7 * front) * e * a;
}`;

const FS = `#version 300 es
precision mediump float;
in vec3 vColor;
in float vAlpha;
out vec4 o;
void main() {
  float d = length(gl_PointCoord - 0.5);
  float a = smoothstep(0.5, 0.0, d);
  a *= a;
  o = vec4(vColor * a * vAlpha, a * vAlpha);
}`;

function compile(gl: WebGL2RenderingContext, type: number, src: string) {
  const s = gl.createShader(type)!;
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
  return s;
}

function buildGeometry() {
  const n = SPHERE + HALO + FIELD;
  const pos = new Float32Array(n * 3);
  const seed = new Float32Array(n);
  const kind = new Float32Array(n);
  const golden = Math.PI * (3 - Math.sqrt(5));
  for (let i = 0; i < SPHERE; i++) {
    // 피보나치 구면 배치 + 약간의 흔들림
    const y = 1 - (i / (SPHERE - 1)) * 2;
    const r = Math.sqrt(1 - y * y);
    const th = golden * i;
    const j = 1 + (Math.random() - 0.5) * 0.04;
    pos.set([Math.cos(th) * r * j, y * j, Math.sin(th) * r * j], i * 3);
    seed[i] = Math.random();
  }
  const shell = (i: number, rr: number, k: number) => {
    const u = Math.random() * 2 - 1;
    const th = Math.random() * Math.PI * 2;
    const r0 = Math.sqrt(1 - u * u);
    pos.set([Math.cos(th) * r0 * rr, u * rr, Math.sin(th) * r0 * rr], i * 3);
    seed[i] = Math.random();
    kind[i] = k;
  };
  // 둘레 먼지: 1.08~1.7(안쪽에 더 많이)
  for (let k = 0; k < HALO; k++) shell(SPHERE + k, 1.08 + Math.pow(Math.random(), 1.8) * 0.62, 1);
  // 성운: 1.6~9, 화면 넓이에 고르게 보이도록 바깥에도 충분히
  for (let k = 0; k < FIELD; k++) shell(SPHERE + HALO + k, Math.sqrt(1.6 * 1.6 + Math.random() * (81 - 2.56)), 2);
  return { pos, seed, kind, n };
}

/** 구가 자리 잡을 곳: data-orb-anchor 요소가 있으면 그 가운데, 없으면 본문 가운데에 크고 옅게. */
function target(dpr: number) {
  const anchor = document.querySelector<HTMLElement>("[data-orb-anchor]");
  if (anchor) {
    const r = anchor.getBoundingClientRect();
    if (r.width > 0) return { x: (r.left + r.width / 2) * dpr, y: (r.top + r.height / 2) * dpr, radius: (r.width / 2) * dpr, dim: 1 };
  }
  const main = document.querySelector<HTMLElement>(".main")?.getBoundingClientRect();
  const w = main?.width ?? innerWidth;
  const h = main?.height ?? innerHeight;
  return { x: ((main?.left ?? 0) + w / 2) * dpr, y: ((main?.top ?? 0) + h * 0.46) * dpr, radius: Math.min(w, h) * 0.34 * dpr, dim: 0.22 };
}

/**
 * 창 전체를 덮는 빛 입자 배경. 가운데 구는 첫 화면의 빈 자리(data-orb-anchor)에 놓이고,
 * 대화를 열면 본문 뒤로 미끄러지듯 커지며 옅어진다. 구 둘레 먼지와 성운이 창 끝까지 이어진다.
 * WebGL2를 쓸 수 없으면 아무것도 그리지 않는다(바탕색과 CSS 빛만 남는다).
 */
export default function ParticleOrb({ mode }: { mode: OrbMode }) {
  const ref = useRef<HTMLCanvasElement>(null);
  const glowRef = useRef<HTMLDivElement>(null);
  const modeRef = useRef(mode);
  modeRef.current = mode;

  useEffect(() => {
    const canvas = ref.current;
    const gl = canvas?.getContext("webgl2", { premultipliedAlpha: true, antialias: false, alpha: true });
    if (!canvas || !gl) return;
    let prog: WebGLProgram;
    try {
      prog = gl.createProgram()!;
      gl.attachShader(prog, compile(gl, gl.VERTEX_SHADER, VS));
      gl.attachShader(prog, compile(gl, gl.FRAGMENT_SHADER, FS));
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
    } catch (e) {
      console.warn("입자 구를 그릴 수 없습니다:", e);
      return;
    }
    gl.useProgram(prog);
    const geo = buildGeometry();
    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    const attr = (name: string, data: Float32Array, size: number) => {
      const buf = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW);
      const loc = gl.getAttribLocation(prog, name);
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, size, gl.FLOAT, false, 0, 0);
    };
    attr("aPos", geo.pos, 3);
    attr("aSeed", geo.seed, 1);
    attr("aKind", geo.kind, 1);
    const U = (n: string) => gl.getUniformLocation(prog, n);
    const u = {
      time: U("uTime"), rot: U("uRot"), swirl: U("uSwirl"), energy: U("uEnergy"), spread: U("uSpread"), assemble: U("uAssemble"),
      glitch: U("uGlitch"), px: U("uPx"), radius: U("uRadius"), dim: U("uDim"), center: U("uCenter"), res: U("uRes"), a: U("uA"), b: U("uB"),
    };
    gl.enable(gl.BLEND);
    gl.clearColor(0, 0, 0, 0);

    let dpr = 1;
    const fit = () => {
      dpr = Math.min(window.devicePixelRatio || 1, 1.5);
      canvas.width = Math.round(canvas.clientWidth * dpr);
      canvas.height = Math.round(canvas.clientHeight * dpr);
      gl.viewport(0, 0, canvas.width, canvas.height);
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(canvas);

    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)");
    const first = LOOK[modeRef.current];
    const cur = { ...first, a: [...first.a] as Rgb, b: [...first.b] as Rgb };
    const at = target(dpr);
    let rot = 0;
    const t0 = performance.now();
    let last = t0;
    let raf = 0;
    let frame = 0;
    let to = { ...at };
    let steps = 1;
    let tick = t0;
    let acc = 0;
    let glowKey = "";
    // 60fps 기준 계수를 실제 프레임 간격에 맞춘다(30fps로 그려도 같은 속도로 따라간다).
    const ease = (x: number, y: number, k: number) => x + (y - x) * (1 - Math.pow(1 - k, steps));

    const draw = (now: number) => {
      raf = requestAnimationFrame(draw);
      // 고주사율 화면에서도 60fps, 대화 화면(구가 옅을 때)에서는 30fps면 충분하다. 뒤가 덜 바뀌면 유리 판의 backdrop-filter도 덜 다시 계산된다.
      const budget = at.dim < 0.5 && modeRef.current !== "thinking" ? 1000 / 30 : 1000 / 60;
      acc += now - tick;
      tick = now;
      if (acc < budget - 1) return;
      acc = Math.min(acc - budget, budget);
      const dt = Math.min(0.05, (now - last) / 1000);
      steps = dt * 60;
      last = now;
      const t = (now - t0) / 1000;
      const m = modeRef.current;
      const want = LOOK[m];
      for (let i = 0; i < 3; i++) {
        cur.a[i] = ease(cur.a[i], want.a[i], 0.05);
        cur.b[i] = ease(cur.b[i], want.b[i], 0.05);
      }
      cur.energy = ease(cur.energy, want.energy + (m === "idle" ? 0.025 * Math.sin(t * 1.3) : 0), 0.06);
      cur.spin = ease(cur.spin, want.spin, 0.04);
      cur.swirl = ease(cur.swirl, want.swirl, 0.04);
      cur.spread = ease(cur.spread, want.spread, 0.05);
      cur.glitch = ease(cur.glitch, want.glitch, 0.1);
      rot += cur.spin * dt * (reduce.matches ? 0.25 : 1);

      // 첫 화면 ↔ 대화 화면 사이를 미끄러지듯 옮겨 간다.
      // 위치는 여섯 프레임에 한 번만 잰다(매 프레임 강제 레이아웃 방지). 사이는 보간이 메운다.
      if (frame++ % 6 === 0) to = target(dpr);
      at.x = ease(at.x, to.x, 0.07);
      at.y = ease(at.y, to.y, 0.07);
      at.radius = ease(at.radius, to.radius, 0.07);
      at.dim = ease(at.dim, to.dim, 0.05);
      const glow = glowRef.current;
      if (glow) {
        const tf = `translate(${(at.x / dpr).toFixed(1)}px, ${(at.y / dpr).toFixed(1)}px) translate(-50%, -50%) scale(${Math.max(0.2, at.radius / dpr / 160).toFixed(3)})`;
        const op = (0.35 + at.dim * 0.65).toFixed(2);
        // 자리 잡은 뒤에는 스타일을 다시 쓰지 않는다.
        if (tf + op !== glowKey) {
          glowKey = tf + op;
          glow.style.transform = tf;
          glow.style.opacity = op;
        }
      }

      // 다크는 빛이 겹치며 밝아지는 가산 합성, 화이트는 일반 합성(가산은 흰 바탕에서 사라진다)
      const light = isLight();
      if (light) gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      else gl.blendFunc(gl.ONE, gl.ONE);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.uniform1f(u.time, t);
      gl.uniform1f(u.rot, rot);
      gl.uniform1f(u.swirl, cur.swirl);
      gl.uniform1f(u.energy, cur.energy);
      gl.uniform1f(u.spread, cur.spread);
      gl.uniform1f(u.assemble, Math.min(1, t / 2.2));
      gl.uniform1f(u.glitch, cur.glitch);
      gl.uniform2f(u.center, at.x, at.y);
      gl.uniform2f(u.res, canvas.width, canvas.height);
      gl.uniform1f(u.radius, at.radius);
      gl.uniform1f(u.dim, at.dim);
      gl.uniform1f(u.px, dpr * Math.max(0.8, Math.min(1.6, at.radius / dpr / 170)));
      // 화이트 모드에서는 밝은 입자가 바탕에 묻히므로 진한 회색 쪽으로 내린다.
      gl.uniform3fv(u.a, cur.a.map((v) => v * (light ? 0.34 : 1)));
      gl.uniform3fv(u.b, cur.b.map((v) => v * (light ? 0.62 : 1)));
      gl.drawArrays(gl.POINTS, 0, geo.n);
    };
    raf = requestAnimationFrame(draw);
    // 창을 내리거나 가리면 멈춘다.
    const onVis = () => {
      cancelAnimationFrame(raf);
      raf = 0;
      if (!document.hidden) {
        last = tick = performance.now();
        acc = 0;
        raf = requestAnimationFrame(draw);
      }
    };
    document.addEventListener("visibilitychange", onVis);
    return () => {
      cancelAnimationFrame(raf);
      document.removeEventListener("visibilitychange", onVis);
      // 개발 모드(StrictMode)는 같은 캔버스로 다시 마운트하므로 컨텍스트를 버리지 않는다.
      ro.disconnect();
    };
  }, []);

  return (
    <div className="orb-field" aria-hidden>
      <div ref={glowRef} className="orb-glow" />
      <canvas ref={ref} className="orb-canvas" />
    </div>
  );
}
