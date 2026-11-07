// A <details> that renders its body only while open: closed sections in long sessions cost nothing.
import { useState, type ReactNode } from "react";

export default function LazyDetails({ className, summary, children, defaultOpen = false }: { className?: string; summary: ReactNode; children: ReactNode; defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <details className={className} open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary>{summary}</summary>
      {open && children}
    </details>
  );
}
