import { useEffect, useRef, useState, type CSSProperties } from "react";
import { ENGINE_LABEL, TIER_LABEL, type Approval, type Budget, type Judgment, type Message, type MessageData, type Step } from "../lib/api";
import { useApp, type Live } from "../store";
import Markdown from "./Markdown";
import { ChevronIcon, CopyIcon, FolderIcon, SparkIcon, WrenchIcon } from "./Icons";

const SUGGESTIONS: { title: string; text: string; tone: string }[] = [
  { title: "보고서", text: "/report 2026년 개인용 AI 비서 시장 동향", tone: "salmon" },
  { title: "코드 리뷰", text: "/code-review 이 세션 폴더의 코드를 검토해 줘", tone: "aqua" },
  { title: "아이디어", text: "주말 사이드 프로젝트 아이디어를 내 줘", tone: "blush" },
  { title: "번역", text: "/translate ", tone: "rose" },
];

const TASK_LABEL: Record<string, string> = { code: "코드", writing: "글쓰기", research: "조사", analysis: "분석", chat: "대화", translate: "번역" };

function secs(ms: number) {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}초`;
}

type NodeState = "wait" | "run" | "ok" | "fail";
interface RibbonNode {
  key: string;
  title: string;
  sub: string;
  state: NodeState;
  kind: "jev" | "draft" | "review" | "debate" | "rebuttal" | "judge";
}

const ROLE_LABEL: Record<Step["role"], string> = { draft: "초안", review: "검토", debate: "토론", rebuttal: "반론", judge: "판정" };

/** 릴레이 리본: JEV 판단 → 초안 → 검토가 어떤 AI로 이어졌는지 한 줄로 보여 준다. 진행 중이면 흐르는 선으로 움직인다. */
function Ribbon({ nodes }: { nodes: RibbonNode[] }) {
  if (nodes.length === 0) return null;
  return (
    <div className="ribbon" role="list" aria-label="처리 과정">
      {nodes.map((n, i) => (
        <div key={n.key} className="ribbon-seg" role="listitem" style={{ "--i": i } as CSSProperties}>
          {i > 0 && <span className={`ribbon-link ${n.state === "wait" ? "" : n.state === "run" ? "flow" : "done"}`} aria-hidden />}
          <div className={`ribbon-node ${n.kind} ${n.state}`}>
            <i className="ribbon-dot" aria-hidden />
            <div>
              <b>{n.title}</b>
              <span>{n.sub}</span>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

function jevNode(j: Judgment | null | undefined, b: Budget | null | undefined): RibbonNode | null {
  if (!j) return null;
  const task = j.task ? (TASK_LABEL[j.task] ?? j.task) : "판단";
  const diff = j.difficulty != null ? ` · 난이도 ${j.difficulty.toFixed(1)}` : "";
  const tier = b ? ` · ${TIER_LABEL[b.tier]}` : "";
  const stakes = j.stakes != null && j.stakes >= 2 ? ` · 정확도 ${j.stakes.toFixed(1)}` : "";
  return { key: "jev", kind: "jev", title: "JEV", sub: `${task}${diff}${stakes}${tier}`, state: "ok" };
}

/** "haiku · low" 처럼 모델·노력치를 짧게 */
function tune(model: string | null | undefined, effort: string | null | undefined): string {
  return [model, effort].filter(Boolean).join(" · ");
}

function stepNodes(steps: Step[]): RibbonNode[] {
  // 토론은 같은 라운드의 두 AI를 한 칸에 나란히 묶는다.
  const out: RibbonNode[] = [];
  steps.forEach((s, i) => {
    const t = tune(s.engine === "claude" ? s.model : null, s.effort);
    const prev = out[out.length - 1];
    if (prev && (s.role === "debate" || s.role === "rebuttal") && prev.kind === s.role && !prev.title.includes(" · ")) {
      prev.title = `${prev.title} · ${s.label}`;
      prev.sub = `${ROLE_LABEL[s.role]} · ${steps.filter((x) => x.role === s.role).every((x) => x.ok) ? "둘 다 답함" : "일부 실패"}`;
      if (!s.ok) prev.state = "fail";
      return;
    }
    out.push({ key: `s${i}`, kind: s.role, title: s.label, sub: `${ROLE_LABEL[s.role]} · ${s.ok ? secs(s.ms) : "실패"}${t ? ` · ${t}` : ""}`, state: s.ok ? "ok" : "fail" });
  });
  return out;
}

function debateNodes(l: Live): RibbonNode[] {
  const d = l.debate!;
  const st = (e: string, role: string) => l.debateState[`${e}:${role}`];
  const pair = (role: "debate" | "rebuttal"): NodeState => {
    const a = st(d.a, role);
    const b = st(d.b, role);
    if (!a && !b) return "wait";
    if (a === "run" || b === "run") return "run";
    return a === "fail" && b === "fail" ? "fail" : "ok";
  };
  const one = pair("debate");
  const out: RibbonNode[] = [{ key: "d1", kind: "debate", title: `${ENGINE_LABEL[d.a]} · ${ENGINE_LABEL[d.b]}`, sub: one === "run" ? "따로 답하는 중" : one === "wait" ? "토론 대기" : "따로 답함", state: one }];
  if (d.rebuttal) {
    const two = pair("rebuttal");
    out.push({ key: "d2", kind: "rebuttal", title: "반론", sub: two === "run" ? "서로 반론하는 중" : two === "wait" ? "반론 대기" : "반론 끝", state: two });
  }
  out.push({ key: "j", kind: "judge", title: ENGINE_LABEL[d.judge], sub: l.phase === "judge" ? "판정하는 중" : "판정 대기", state: l.phase === "judge" ? "run" : "wait" });
  return out;
}

function liveNodes(l: Live): RibbonNode[] {
  const out: RibbonNode[] = [];
  const j = jevNode(l.jev, l.budget);
  const t = tune(l.primary === "claude" ? l.model : null, l.effort);
  if (j) out.push(j);
  if (l.debate) return [...out, ...debateNodes(l)];
  if (l.primary) {
    out.push({ key: "d", kind: "draft", title: ENGINE_LABEL[l.primary], sub: (l.phase === "review" ? "초안 완료" : "초안 쓰는 중") + (t && l.phase === "draft" ? ` · ${t}` : ""), state: l.phase === "review" ? "ok" : l.phase === "draft" ? "run" : "wait" });
  }
  if (l.reviewer) out.push({ key: "r", kind: "review", title: ENGINE_LABEL[l.reviewer], sub: l.phase === "review" ? "검토 중" : "검토 대기", state: l.phase === "review" ? "run" : "wait" });
  return out;
}

function Details({ data }: { data: MessageData }) {
  const [open, setOpen] = useState(false);
  const steps = data.steps ?? [];
  const cost = steps.reduce((s, x) => s + (x.costUsd ?? 0), 0) + (data.jev?.costUsd ?? 0);
  const tokens = steps.reduce((s, x) => s + (x.inputTokens ?? 0) + (x.outputTokens ?? 0), 0);
  return (
    <div className="details">
      <button className="details-line" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        과정 보기
        {tokens > 0 && <span>{tokens.toLocaleString()} 토큰</span>}
        {cost > 0 && <span title="추정 비용(구독 요금제에서는 실제 청구액이 아님)">≈${cost.toFixed(3)}</span>}
        <ChevronIcon open={open} />
      </button>
      {open && (
        <div className="details-body">
          {data.reasons?.map((r, i) => (
            <div key={i} className="reason" style={{ "--i": i } as CSSProperties}>
              · {r}
            </div>
          ))}
          {steps.map((s, i) => (
            <div key={i} className="step">
              <div className="step-head">
                <b>
                  {s.label} · {ROLE_LABEL[s.role]}
                </b>
                <span>
                  {secs(s.ms)}
                  {s.inputTokens != null && ` · 입력 ${s.inputTokens.toLocaleString()} / 출력 ${(s.outputTokens ?? 0).toLocaleString()}`}
                </span>
              </div>
              {s.activities.map((a, j) => (
                <div key={j} className="act">
                  {a}
                </div>
              ))}
              {s.error && <div className="step-err">{s.error}</div>}
            </div>
          ))}
          {data.debate?.rounds.filter((r) => r.text).map((r, i) => (
            <details key={i} className="draft">
              <summary>
                {r.round === 1 ? "토론 1차 답" : "반론과 고친 답"} ({ENGINE_LABEL[r.engine]})
              </summary>
              <Markdown text={r.text} />
            </details>
          ))}
          {data.draft && (
            <details className="draft">
              <summary>검토 전 초안 ({data.draftedBy ? ENGINE_LABEL[data.draftedBy] : ""})</summary>
              <Markdown text={data.draft} />
            </details>
          )}
        </div>
      )}
    </div>
  );
}

function ApprovalView({ a }: { a: Approval }) {
  const { decide } = useApp();
  return (
    <div className="approval">
      <div className="approval-head">
        <WrenchIcon /> 파일을 고쳐도 될까요?
      </div>
      <p>
        <b>{ENGINE_LABEL[a.engine]}</b>가 아래 폴더 안의 파일을 만들거나 고치려고 합니다. 명령 실행은 허용하지 않습니다.
      </p>
      <div className="approval-ws">
        <FolderIcon /> {a.workspace}
      </div>
      <div className="approval-actions">
        <button className="btn primary" onClick={() => decide(a.id, true)}>
          허락하고 진행
        </button>
        <button className="btn" onClick={() => decide(a.id, false)}>
          거절
        </button>
      </div>
    </div>
  );
}

export interface Exchange {
  key: string;
  user: Message | null;
  answer: Message | null;
}

export function pair(messages: Message[]): Exchange[] {
  const out: Exchange[] = [];
  for (const m of messages) {
    if (m.role === "user") out.push({ key: m.id, user: m, answer: null });
    else {
      const last = out[out.length - 1];
      if (last && !last.answer) last.answer = m;
      else out.push({ key: m.id, user: null, answer: m });
    }
  }
  return out;
}

/** 결과 카드: 요청(머리) → 릴레이 리본 → 결과(본문) → 과정·복사(꼬리) */
function Card({ ex, live, approvals, index }: { ex: Exchange; live?: Live; approvals: Approval[]; index: number }) {
  const { toast } = useApp();
  const a = ex.answer;
  const data = a?.data ?? null;
  const failed = !!a && a.content.startsWith("⚠");
  const nodes = live ? liveNodes(live) : data ? [jevNode(data.jev, data.budget), ...stepNodes(data.steps ?? [])].filter((n): n is RibbonNode => !!n) : [];
  const work = live?.work ?? data?.work ?? ex.user?.data?.work;
  const skill = live?.skill ?? data?.skill;
  return (
    <article id={`ex-${ex.key}`} className={`card ${live ? "live" : ""} ${failed ? "failed" : ""}`} style={{ "--i": index } as CSSProperties}>
      {ex.user && (
        <header className="card-ask">
          <div className="ask-text">{ex.user.content}</div>
          <div className="ask-tags">
            {skill && (
              <span className="tag skill">
                <SparkIcon size={12} /> {skill.name}
              </span>
            )}
            {work && (
              <span className="tag work">
                <WrenchIcon /> 작업
              </span>
            )}
          </div>
        </header>
      )}
      <Ribbon nodes={nodes} />
      {approvals.map((ap) => (
        <ApprovalView key={ap.id} a={ap} />
      ))}
      <div className="card-body">
        {live ? (
          <>
            {live.text ? <Markdown text={live.text} /> : <div className="typing" aria-label="준비 중"><i /><i /><i /></div>}
            {live.activities.length > 0 && (
              <div className="live-acts">
                {live.activities.slice(-4).map((t, i) => (
                  <div key={`${i}-${t}`}>{t}</div>
                ))}
              </div>
            )}
          </>
        ) : a ? (
          <Markdown text={a.content} />
        ) : null}
      </div>
      {a && !live && (
        <footer className="card-foot">
          {data && <Details data={data} />}
          <button className="foot-btn" onClick={() => navigator.clipboard.writeText(a.content).then(() => toast("복사했습니다."))}>
            <CopyIcon /> 복사
          </button>
        </footer>
      )}
    </article>
  );
}

function Welcome({ onPick }: { onPick: (t: string) => void }) {
  const { engines, setDialog } = useApp();
  const ready = engines.filter((e) => e.installed);
  return (
    <div className="welcome">
      {/* 배경의 구가 이 자리로 와서 머문다(ParticleOrb가 data-orb-anchor를 따라간다) */}
      <div className="orb-anchor" data-orb-anchor role="img" aria-label="Elysium" />
      <h1>
        여러 AI가 <em>이어서</em> 완성합니다
      </h1>
      <p className="welcome-sub">JEV가 요청을 읽고 맞는 AI와 노력치를 고릅니다. 한 AI가 쓰고 다른 AI가 검토하며, 정확도가 중요하면 두 AI가 토론합니다. 세션 안의 모든 AI는 같은 기억을 씁니다.</p>
      <div className="demo-relay" aria-hidden>
        <span className="demo-node" style={{ "--i": 0 } as CSSProperties}>
          <i /> <b>JEV</b> 판단
        </span>
        <span className="demo-link" />
        <span className="demo-node" style={{ "--i": 1 } as CSSProperties}>
          <i /> <b>초안</b> AI
        </span>
        <span className="demo-link" />
        <span className="demo-node" style={{ "--i": 2 } as CSSProperties}>
          <i /> <b>검토</b> AI
        </span>
      </div>
      <div className="welcome-engines">
        {engines.map((e, i) => (
          <span key={e.id} className={`eng ${e.installed ? "on" : ""}`} style={{ "--i": i } as CSSProperties} title={e.installed ? (e.path ?? e.version ?? "") : e.installHint}>
            <i /> {e.label}
          </span>
        ))}
        {ready.length === 0 && engines.length > 0 && (
          <button className="link" onClick={() => setDialog("settings")}>
            연결하기
          </button>
        )}
      </div>
      <div className="welcome-cards">
        {SUGGESTIONS.map((s, i) => (
          <button key={s.title} className={`tone-${s.tone}`} style={{ "--i": i } as CSSProperties} onClick={() => onPick(s.text)}>
            <b>{s.title}</b>
            <span>{s.text}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

export default function Thread({ onPick }: { onPick: (t: string) => void }) {
  const { messages, live, conversationId, approvals } = useApp();
  const ref = useRef<HTMLDivElement>(null);
  const mine = live.filter((l) => l.conversationId === conversationId);
  const pending = approvals.filter((a) => a.conversationId === conversationId);
  const stick = useRef(true);
  const exchanges = pair(messages);

  useEffect(() => {
    const el = ref.current;
    if (el && stick.current) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [messages.length, mine.map((l) => l.text.length + l.activities.length).join(), pending.length]);

  if (!conversationId) {
    return (
      <div className="thread empty">
        <Welcome onPick={onPick} />
      </div>
    );
  }
  // 진행 중인 요청은 답이 아직 없는 마지막 카드에 붙인다.
  const lastOpen = exchanges.length && !exchanges[exchanges.length - 1].answer ? exchanges.length - 1 : -1;
  return (
    <div
      className="thread"
      ref={ref}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
      }}
      aria-live="polite"
    >
      <div className="thread-inner">
        {exchanges.map((ex, i) => (
          <Card key={ex.key} ex={ex} index={i} live={i === lastOpen ? mine[0] : undefined} approvals={i === lastOpen ? pending : []} />
        ))}
      </div>
    </div>
  );
}

