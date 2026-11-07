// Work setup set before sending: who works on the request (automatic, a number of agents, or a team built here)
// and how the work runs (effort, review, lead commits, OpenCode assignment, JEV, worker turns).
import { useEffect, useState } from "react";
import { api, type ReviewMode, type SetupGroup, type SetupSeat, type SetupTeam, type WorkSetup } from "../lib/api";
import { CloseIcon, PlusIcon, RedoIcon, TrashIcon } from "./Icons";
import Segmented from "./Segmented";

export const EMPTY_SETUP: WorkSetup = { team: null, agents: 0, tier: null, review: null, leadCommit: null, roadmap: null, jev: null, maxTurns: null };

const KEY = "elysium.workSetup";
const EFFORTS = ["low", "medium", "high", "xhigh", "max"];
const MAX_AGENTS = 64;
const MAX_COUNT = 16;
/** Turn limit the backend allows (SETUP_TURNS) and its default (MAX_TURNS) */
const TURNS = { min: 5, max: 200, def: 40 };

export const T = {
  title: "작업 설정",
  open: "작업 설정 열기/닫기",
  custom: "사용자 지정",
  reset: "기본값 초기화",
  close: "닫기",
  keep: "초기화하기 전까지 이후 모든 메시지에 이 설정이 적용됩니다.",
  teamHead: "팀 구성",
  modeAuto: "자동",
  modeCount: "인원 지정",
  modeCustom: "직접 구성",
  autoHelp: "메시지에 명시된 팀 구성을 우선 따르며, 없으면 판별 모델과 기획자가 인원과 AI를 자동 결정합니다.",
  countLabel: "에이전트 수",
  countHelp: "1명은 단독 처리하며, 2명 이상은 작업을 분할하여 독립된 사본에서 병렬 수행한 뒤 병합합니다.",
  lead: "총괄",
  leadAuto: "자동 선택",
  team: "{n}팀",
  dept: "부서명 (선택, 예: 개발/프론트)",
  model: "모델",
  effort: "추론 강도",
  effortDefault: "기본값",
  count: "워커 수",
  addSeat: "워커 추가",
  addTeam: "팀 추가",
  removeSeat: "워커 삭제",
  removeTeam: "팀 삭제",
  seats: "워커 {n}명",
  tooFew: "워커는 최소 2명 이상이어야 합니다.",
  tooMany: "워커는 최대 {max}명까지 설정할 수 있습니다.",
  workOnly: "직접 구성한 팀은 '작업' 모드나 '만들기' 요청 시 적용되며, '대화' 모드에서는 무시됩니다.",
  flowHead: "작업 진행 방식",
  inherit: "기본 설정 따름",
  on: "켜짐",
  off: "꺼짐",
  tier: "작업 강도",
  tierLight: "경량",
  tierStandard: "표준",
  tierDeep: "심층",
  review: "AI 검토",
  reviewAuto: "자동",
  reviewAlways: "항상",
  reviewDebate: "토론",
  commit: "총괄 커밋",
  roadmap: "태스크별 AI 배정",
  jev: "JEV 사용",
  turns: "워커 최대 턴 수",
  turnsHelp: "기본 {n}",
};

const TIERS = [
  { value: "auto", label: T.modeAuto },
  { value: "light", label: T.tierLight },
  { value: "standard", label: T.tierStandard },
  { value: "deep", label: T.tierDeep },
] as const;

/** Every field back at its automatic value: nothing to send. */
export function isDefault(s: WorkSetup): boolean {
  return JSON.stringify(s) === JSON.stringify(EMPTY_SETUP);
}

export function loadSetup(): WorkSetup {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "null") as Partial<WorkSetup> | null;
    return v ? { ...EMPTY_SETUP, ...v } : EMPTY_SETUP;
  } catch {
    return EMPTY_SETUP;
  }
}

export function saveSetup(s: WorkSetup) {
  try {
    if (isDefault(s)) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, JSON.stringify(s));
  } catch {
    /* not kept: this run only */
  }
}

const seatsOf = (t: SetupTeam) => t.groups.reduce((n, g) => n + g.members.reduce((m, s) => m + Math.max(1, s.count), 0), 0);

/** Three-way toggle: settings value · on · off */
function Tri({ label, value, onChange }: { label: string; value: boolean | null; onChange: (v: boolean | null) => void }) {
  return (
    <Segmented
      label={label}
      className="ws-seg"
      value={value === null ? "inherit" : value ? "on" : "off"}
      options={[
        { value: "inherit", label: T.inherit },
        { value: "on", label: T.on },
        { value: "off", label: T.off },
      ]}
      onChange={(v) => onChange(v === "inherit" ? null : v === "on")}
    />
  );
}

function Seat({ seat, models, onChange, onRemove, withCount = true }: { seat: SetupSeat; models: string[]; onChange: (s: SetupSeat) => void; onRemove?: () => void; withCount?: boolean }) {
  return (
    <div className="ws-seat">
      <select aria-label={T.model} value={seat.model} onChange={(e) => onChange({ ...seat, model: e.target.value })}>
        {!models.includes(seat.model) && seat.model && <option value={seat.model}>{seat.model}</option>}
        {models.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </select>
      <select aria-label={T.effort} value={seat.effort ?? ""} onChange={(e) => onChange({ ...seat, effort: e.target.value || null })}>
        <option value="">{T.effortDefault}</option>
        {EFFORTS.map((x) => (
          <option key={x} value={x}>
            {x}
          </option>
        ))}
      </select>
      {withCount && (
        <input
          type="number"
          aria-label={T.count}
          min={1}
          max={MAX_COUNT}
          value={seat.count}
          onChange={(e) => onChange({ ...seat, count: Math.min(MAX_COUNT, Math.max(1, Number(e.target.value) || 1)) })}
        />
      )}
      {onRemove && (
        <button type="button" className="icon" onClick={onRemove} aria-label={T.removeSeat} title={T.removeSeat}>
          <CloseIcon />
        </button>
      )}
    </div>
  );
}

export default function WorkSetupPanel({ value, onChange, onClose, work }: { value: WorkSetup; onChange: (s: WorkSetup) => void; onClose: () => void; work: boolean }) {
  const [models, setModels] = useState<string[]>([]);
  useEffect(() => {
    api.workModels().then(setModels).catch(() => setModels([]));
  }, []);
  const set = (patch: Partial<WorkSetup>) => onChange({ ...value, ...patch });
  const mode = value.team ? "custom" : value.agents > 0 ? "count" : "auto";
  const firstModel = models.find((m) => m === "Claude Sonnet") ?? models[0] ?? "";
  const newSeat = (): SetupSeat => ({ model: firstModel, effort: null, count: 1 });
  const team = value.team;
  const setTeam = (t: SetupTeam) => set({ team: t, agents: 0 });
  const setGroup = (i: number, g: SetupGroup) => team && setTeam({ ...team, groups: team.groups.map((x, j) => (j === i ? g : x)) });
  const seats = team ? seatsOf(team) : 0;

  return (
    <section className="ws-panel" aria-label={T.title}>
      <header className="ws-head">
        <b>{T.title}</b>
        <span className="ws-note">{T.keep}</span>
        <div className="spacer" />
        <button type="button" className="btn sm ghost" onClick={() => onChange(EMPTY_SETUP)} disabled={isDefault(value)}>
          <RedoIcon /> {T.reset}
        </button>
        <button type="button" className="icon" onClick={onClose} aria-label={T.close} title={T.close}>
          <CloseIcon />
        </button>
      </header>

      <div className="ws-body">
        <div className="ws-col">
          <h5>{T.teamHead}</h5>
          <Segmented
            label={T.teamHead}
            className="ws-seg"
            value={mode}
            options={[
              { value: "auto", label: T.modeAuto },
              { value: "count", label: T.modeCount },
              { value: "custom", label: T.modeCustom },
            ]}
            onChange={(m) => {
              if (m === "auto") set({ team: null, agents: 0 });
              else if (m === "count") set({ team: null, agents: value.agents || 3 });
              else setTeam(team ?? { lead: null, groups: [{ dept: "", members: [newSeat(), { ...newSeat(), count: 1 }] }] });
            }}
          />
          {mode === "auto" && <p className="ws-help">{T.autoHelp}</p>}
          {mode === "count" && (
            <>
              <label className="ws-row">
                <span>{T.countLabel}</span>
                <input type="number" min={1} max={MAX_AGENTS} value={value.agents} onChange={(e) => set({ agents: Math.min(MAX_AGENTS, Math.max(1, Number(e.target.value) || 1)) })} />
              </label>
              <p className="ws-help">{T.countHelp}</p>
            </>
          )}
          {team && (
            <div className="ws-team">
              <div className="ws-lead">
                <span className="ws-tag">{T.lead}</span>
                <select
                  aria-label={T.lead}
                  value={team.lead?.model ?? ""}
                  onChange={(e) => setTeam({ ...team, lead: e.target.value ? { model: e.target.value, effort: team.lead?.effort ?? null, count: 1 } : null })}
                >
                  <option value="">{T.leadAuto}</option>
                  {models.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
                {team.lead && (
                  <select aria-label={T.effort} value={team.lead.effort ?? ""} onChange={(e) => setTeam({ ...team, lead: team.lead && { ...team.lead, effort: e.target.value || null } })}>
                    <option value="">{T.effortDefault}</option>
                    {EFFORTS.map((x) => (
                      <option key={x} value={x}>
                        {x}
                      </option>
                    ))}
                  </select>
                )}
              </div>
              {team.groups.map((g, i) => (
                <div key={i} className="ws-group">
                  <div className="ws-group-head">
                    <b>{T.team.replace("{n}", String(i + 1))}</b>
                    <input type="text" value={g.dept} placeholder={T.dept} aria-label={T.dept} onChange={(e) => setGroup(i, { ...g, dept: e.target.value })} />
                    <button type="button" className="icon" onClick={() => setTeam({ ...team, groups: team.groups.filter((_, j) => j !== i) })} aria-label={T.removeTeam} title={T.removeTeam}>
                      <TrashIcon />
                    </button>
                  </div>
                  {g.members.map((m, k) => (
                    <Seat
                      key={k}
                      seat={m}
                      models={models}
                      onChange={(s) => setGroup(i, { ...g, members: g.members.map((x, j) => (j === k ? s : x)) })}
                      onRemove={() => setGroup(i, { ...g, members: g.members.filter((_, j) => j !== k) })}
                    />
                  ))}
                  <button type="button" className="btn sm ghost" onClick={() => setGroup(i, { ...g, members: [...g.members, newSeat()] })}>
                    <PlusIcon /> {T.addSeat}
                  </button>
                </div>
              ))}
              <div className="ws-team-foot">
                <button type="button" className="btn sm" onClick={() => setTeam({ ...team, groups: [...team.groups, { dept: "", members: [newSeat()] }] })}>
                  <PlusIcon /> {T.addTeam}
                </button>
                <span className={seats < 2 || seats > MAX_AGENTS ? "agent-err" : "ws-note"}>
                  {seats < 2 ? T.tooFew : seats > MAX_AGENTS ? T.tooMany.replace("{max}", String(MAX_AGENTS)) : T.seats.replace("{n}", String(seats))}
                </span>
              </div>
              {!work && <p className="ws-help warn">{T.workOnly}</p>}
            </div>
          )}
        </div>

        <div className="ws-col">
          <h5>{T.flowHead}</h5>
          <div className="ws-row">
            <span>{T.tier}</span>
            <Segmented label={T.tier} className="ws-seg" value={value.tier ?? "auto"} options={TIERS.map((t) => ({ ...t }))} onChange={(v) => set({ tier: v === "auto" ? null : v })} />
          </div>
          <label className="ws-row">
            <span>{T.review}</span>
            <select value={value.review ?? ""} onChange={(e) => set({ review: (e.target.value || null) as ReviewMode | null })}>
              <option value="">{T.inherit}</option>
              <option value="auto">{T.reviewAuto}</option>
              <option value="always">{T.reviewAlways}</option>
              <option value="off">{T.off}</option>
              <option value="debate">{T.reviewDebate}</option>
            </select>
          </label>
          <div className="ws-row">
            <span>{T.commit}</span>
            <Tri label={T.commit} value={value.leadCommit} onChange={(v) => set({ leadCommit: v })} />
          </div>
          <div className="ws-row">
            <span>{T.roadmap}</span>
            <Segmented
              label={T.roadmap}
              className="ws-seg"
              value={value.roadmap === false ? "off" : "auto"}
              options={[
                { value: "auto", label: T.modeAuto },
                { value: "off", label: T.off },
              ]}
              onChange={(v) => set({ roadmap: v === "off" ? false : null })}
            />
          </div>
          <div className="ws-row">
            <span>{T.jev}</span>
            <Segmented
              label={T.jev}
              className="ws-seg"
              value={value.jev === false ? "off" : "inherit"}
              options={[
                { value: "inherit", label: T.inherit },
                { value: "off", label: T.off },
              ]}
              onChange={(v) => set({ jev: v === "off" ? false : null })}
            />
          </div>
          <label className="ws-row">
            <span>{T.turns}</span>
            <input
              type="number"
              min={TURNS.min}
              max={TURNS.max}
              value={value.maxTurns ?? ""}
              placeholder={T.turnsHelp.replace("{n}", String(TURNS.def))}
              onChange={(e) => set({ maxTurns: e.target.value === "" ? null : Math.min(TURNS.max, Math.max(1, Math.round(Number(e.target.value)) || 1)) })}
              onBlur={() => value.maxTurns !== null && value.maxTurns < TURNS.min && set({ maxTurns: TURNS.min })}
            />
          </label>
        </div>
      </div>
    </section>
  );
}
