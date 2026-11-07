type Pos = { start: { offset?: number }; end: { offset?: number } };
type MdNode = { type: string; value?: string; position?: Pos; children?: MdNode[] };

// Unwraps GFM `delete` nodes back into text, restoring the exact source tildes (`~` or `~~`).
export default function remarkNoStrike() {
  return (tree: MdNode, file: { value?: unknown }) => {
    const src = String(file.value ?? "");
    const walk = (node: MdNode) => {
      if (!node.children) return;
      const out: MdNode[] = [];
      for (const child of node.children) {
        walk(child);
        if (child.type !== "delete") {
          out.push(child);
          continue;
        }
        const kids = child.children ?? [];
        const s = child.position?.start.offset;
        const e = child.position?.end.offset;
        const a = kids[0]?.position?.start.offset;
        const b = kids[kids.length - 1]?.position?.end.offset;
        const open = s != null && a != null ? src.slice(s, a) : "~~";
        const close = e != null && b != null ? src.slice(b, e) : "~~";
        out.push({ type: "text", value: open }, ...kids, { type: "text", value: close });
      }
      node.children = out;
    };
    walk(tree);
  };
}
