import { useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

export interface SegOption<T extends string> {
  value: T;
  label: ReactNode;
  title?: string;
}

/**
 * 라디오(또는 탭) 묶음. 고른 칸 뒤의 밝은 판(thumb)이 다른 칸을 누르면 그쪽으로 미끄러져 간다.
 * 왼쪽·오른쪽 화살표로도 고를 수 있다.
 */
export default function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
  className = "",
  as = "radio",
}: {
  value: T;
  options: SegOption<T>[];
  onChange: (v: T) => void;
  label: string;
  className?: string;
  as?: "radio" | "tab";
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [thumb, setThumb] = useState<{ x: number; w: number } | null>(null);
  const [ready, setReady] = useState(false);

  useLayoutEffect(() => {
    const box = ref.current;
    if (!box) return;
    const measure = () => {
      const on = box.querySelector<HTMLElement>('[data-on="true"]');
      if (on) setThumb({ x: on.offsetLeft, w: on.offsetWidth });
    };
    measure();
    // 처음 자리를 잡은 뒤부터만 움직임을 켠다(열자마자 왼쪽에서 날아오지 않도록).
    const id = requestAnimationFrame(() => setReady(true));
    const ro = new ResizeObserver(measure);
    ro.observe(box);
    return () => {
      cancelAnimationFrame(id);
      ro.disconnect();
    };
  }, [value, options.length]);

  const onKey = (e: KeyboardEvent) => {
    const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const i = options.findIndex((o) => o.value === value);
    const next = options[(i + step + options.length) % options.length];
    onChange(next.value);
    requestAnimationFrame(() => ref.current?.querySelector<HTMLElement>('[data-on="true"]')?.focus());
  };

  return (
    <div ref={ref} className={`seg ${ready ? "ready" : ""} ${className}`} role={as === "tab" ? "tablist" : "radiogroup"} aria-label={label} onKeyDown={onKey}>
      {thumb && <span className="seg-thumb" style={{ transform: `translateX(${thumb.x}px)`, width: thumb.w }} aria-hidden />}
      {options.map((o) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role={as}
            {...(as === "tab" ? { "aria-selected": on } : { "aria-checked": on })}
            tabIndex={on ? 0 : -1}
            data-on={on}
            className={on ? "on" : ""}
            title={o.title}
            onClick={() => onChange(o.value)}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
