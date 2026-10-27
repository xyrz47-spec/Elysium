import { memo, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import "katex/dist/katex.min.css";
import { CheckIcon, CopyIcon } from "./Icons";
import { api } from "../lib/api";
import { normalizeMath } from "../lib/math";

function textOf(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (node && typeof node === "object" && "props" in node) return textOf((node as { props: { children?: ReactNode } }).props.children);
  return "";
}

function CodeBlock({ children }: { children?: ReactNode }) {
  const [copied, setCopied] = useState(false);
  const child = Array.isArray(children) ? children[0] : children;
  const cls = child && typeof child === "object" && "props" in child ? ((child as { props: { className?: string } }).props.className ?? "") : "";
  const lang = /language-([\w+-]+)/.exec(cls)?.[1];
  const code = textOf(children).replace(/\n$/, "");
  return (
    <div className="code-block">
      <div className="code-head">
        <span>{lang ?? "코드"}</span>
        <button
          onClick={() =>
            navigator.clipboard.writeText(code).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1400);
            })
          }
        >
          {copied ? <CheckIcon /> : <CopyIcon />} {copied ? "복사됨" : "복사"}
        </button>
      </div>
      <pre>{children}</pre>
    </div>
  );
}

/**
 * AI 답을 마크다운으로 그린다. react-markdown은 기본값에서 HTML을 실행하지 않으므로
 * 답에 <script> 등이 섞여 있어도 글자로만 보인다. 수식은 KaTeX로 그린다(trust 꺼짐: \href 등 링크·HTML 명령 무시). 링크는 앱 안에서 열지 않고 기본 브라우저로 연다(http·https만).
 */
function Markdown({ text }: { text: string }) {
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[[rehypeKatex, { throwOnError: false, strict: "ignore" }]]}
        components={{
          pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
          a: ({ href, children }) => (
            <a
              href={href}
              title={href}
              onClick={(e) => {
                e.preventDefault();
                if (href) api.openUrl(href).catch(() => {});
              }}
            >
              {children}
            </a>
          ),
          table: ({ children }) => (
            <div className="table-wrap">
              <table>{children}</table>
            </div>
          ),
        }}
      >
        {normalizeMath(text)}
      </ReactMarkdown>
    </div>
  );
}

export default memo(Markdown);
