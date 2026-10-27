/**
 * 수식 표기를 remark-math가 읽는 $ 꼴로 맞춘다. 코드 안은 건드리지 않는다.
 * - \[ … \] → $$ … $$, \( … \) → $ … $ (여러 AI가 쓰는 꼴)
 * - 숫자 앞 $는 금액($0.0007)으로 보고 글자 그대로 둔다.
 * - 줄 하나가 $$식$$뿐이면 블록 수식으로 나눈다.
 */
export function normalizeMath(text: string): string {
  return text
    .split(/(```[\s\S]*?(?:```|$)|`[^`\n]*`)/)
    .map((part, i) =>
      i % 2
        ? part
        : part
            .replace(/\\\[([\s\S]+?)\\\]/g, (_, m: string) => `$$${m}$$`)
            .replace(/\\\(([\s\S]+?)\\\)/g, (_, m: string) => `$${m}$`)
            .replace(/(^|[^$\\])\$(?=\d)/g, "$1\\$")
            // 한 줄에 $$식$$만 있으면 가운데 블록 수식으로(remark-math는 줄을 나눠야 블록으로 본다)
            .replace(/^([ \t]*)\$\$([^\n$]+?)\$\$[ \t]*$/gm, "$1$$$$\n$1$2\n$1$$$$"),
    )
    .join("");
}
