import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, errorText, type BgJob } from "../lib/api";
import { useApp, type Live } from "../store";
import { LOG_LINE_STEP, LT } from "./LeadPanel";
import { LoadEarlier, TAIL_TEXT, useKeepScroll, useTail } from "../lib/tail";

/** 실행 중인 명령의 출력을 다시 읽는 간격(ms) */
const POLL_MS = 1000;
/** 서버는 출력 끝 96KB만 돌려준다. 이만큼 오면 앞이 잘린 것이다. */
const TAIL_CHARS = 90_000;

const T = {
  title: "백그라운드 명령",
  count: "{r}/{n} 실행 중",
  ok: "성공",
  fail: "실패",
  stopped: "중단",
  stop: "중지",
  output: "출력 보기",
  hide: "출력 닫기",
  tail: "최근 출력만 표시됩니다",
  fold: "목록 접기",
  unfold: "목록 펼치기",
};

const TAG: Record<BgJob["state"], [string, string]> = {
  run: [LT.running, "live"],
  ok: [T.ok, "ok"],
  fail: [T.fail, "bad"],
  stopped: [T.stopped, ""],
};

/** 지금까지의 출력. 실행 중이면 계속 다시 읽는다. */
function BgOutput({ job }: { job: BgJob }) {
  const [text, setText] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [follow, setFollow] = useState(true);
  const box = useRef<HTMLPreElement>(null);
  const live = job.state === "run";
  useEffect(() => {
    let off = false;
    const load = () =>
      api
        .bgRead(job.id)
        .then((t) => !off && (setText(t), setErr(null)))
        .catch((e) => !off && setErr(errorText(e)));
    load();
    if (!live) return () => void (off = true);
    const t = setInterval(load, POLL_MS);
    return () => {
      off = true;
      clearInterval(t);
    };
  }, [job.id, live]);
  const all = useMemo(() => (text ? text.split("\n") : []), [text]);
  const tail = useTail(all.length, LOG_LINE_STEP);
  const keep = useKeepScroll(box, tail.shown);
  useLayoutEffect(() => {
    if (follow && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [text, follow]);
  const onScroll = () => {
    const b = box.current;
    if (b) setFollow(b.scrollHeight - b.scrollTop - b.clientHeight < 24);
  };
  const lines = all.length;
  return (
    <div className="test-log">
      <div className="test-log-head">
        <span>{LT.logLines.replace("{n}", String(lines))}</span>
        {(text?.length ?? 0) >= TAIL_CHARS && <span>{T.tail}</span>}
        <div className="spacer" />
        {!follow && (
          <button className="link" onClick={() => setFollow(true)}>
            {LT.logFollow}
          </button>
        )}
      </div>
      <pre ref={box} onScroll={onScroll} tabIndex={0}>
        {!err && <LoadEarlier hidden={tail.hidden} label={TAIL_TEXT.lines} onMore={() => (keep(), setFollow(false), tail.more())} />}
        {err ?? (text?.trim() ? all.slice(all.length - tail.shown).join("\n") : LT.logEmpty)}
      </pre>
    </div>
  );
}

function BgRow({ job, who }: { job: BgJob; who: string }) {
  const { toast } = useApp();
  const [open, setOpen] = useState(false);
  const [tag, cls] = TAG[job.state];
  const stop = () => api.bgStop(job.id).catch((e) => toast(errorText(e), "error"));
  return (
    <li className={open ? "open" : ""}>
      <div className="lead-queue-row">
        <span className={`tag ${cls}`}>{tag}</span>
        <code title={job.command}>{job.command}</code>
        <button className="btn sm ghost" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          {open ? T.hide : T.output}
        </button>
        {job.stoppable && job.state === "run" && (
          <button className="btn sm danger" onClick={stop}>
            {T.stop}
          </button>
        )}
      </div>
      <div className="agent-sub">{who}</div>
      {open && <BgOutput job={job} />}
    </li>
  );
}

const BG_OPEN_KEY = "elysium.bgJobsOpen";
/** Folded by default: the list can be long and polls output while open. */
function loadBgOpen(): boolean {
  try {
    return localStorage.getItem(BG_OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

/** 이 작업에서 에이전트가 뒤에서 돌리는 명령. 실행 중인 것을 먼저 보인다. */
export function BgJobs({ l }: { l: Live }) {
  const [loaded, setLoaded] = useState<Record<string, BgJob>>({});
  const [open, setOpen] = useState(loadBgOpen);
  useEffect(() => {
    try {
      localStorage.setItem(BG_OPEN_KEY, open ? "1" : "0");
    } catch {
      /* not saved: this run only */
    }
  }, [open]);
  // 앱을 다시 연 경우처럼 이벤트를 놓쳤을 때를 위해 한 번 읽어 둔다.
  useEffect(() => {
    api
      .bgJobs(l.taskId)
      .then((js) => setLoaded(Object.fromEntries(js.map((j) => [j.id, j]))))
      .catch(() => {});
  }, [l.taskId]);
  const jobs = Object.values({ ...loaded, ...l.bg }).sort((a, b) => Number(b.state === "run") - Number(a.state === "run") || b.startedAt.localeCompare(a.startedAt));
  if (jobs.length === 0) return null;
  const running = jobs.filter((j) => j.state === "run").length;
  const who = (key: string) => l.parts[key]?.title ?? l.agents[key]?.label ?? key;
  return (
    <div className={`bg-jobs ${open ? "open" : ""}`}>
      <h5 className="lead-queue-sub">
        <button type="button" className="bg-fold" onClick={() => setOpen((o) => !o)} aria-expanded={open} title={open ? T.fold : T.unfold}>
          <i className="fold-chev" aria-hidden />
          {T.title} <span>{T.count.replace("{r}", String(running)).replace("{n}", String(jobs.length))}</span>
        </button>
      </h5>
      {open && (
        <ul className="lead-queue">
          {jobs.map((j) => (
            <BgRow key={j.id} job={j} who={who(j.key)} />
          ))}
        </ul>
      )}
    </div>
  );
}
