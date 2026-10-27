import { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import "katex/dist/katex.min.css";
import { normalizeMath } from "../../src/lib/math";

/** AI 답(마크다운·표·수식). HTML은 실행하지 않고, 링크는 앱 안에서 열지 않는다(주소만 보인다). */
function Markdown({ text }: { text: string }) {
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[[rehypeKatex, { throwOnError: false, strict: "ignore" }]]}
        components={{
          a: ({ href, children }) => (
            <span className="md-link" title={href}>
              {children}
            </span>
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
