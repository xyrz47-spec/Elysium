import { useEffect, useState } from "react";
import { api, errorText, type Plugin, type Skill } from "../lib/api";
import { ask } from "../lib/confirm";
import { useApp } from "../store";
import { CloseIcon, FolderIcon, PlusIcon, PuzzleIcon, RefreshIcon, SparkIcon } from "./Icons";
import Segmented from "./Segmented";

const LABEL: Record<string, string> = { claude: "Claude Code", codex: "Codex", antigravity: "Antigravity", gemini: "Antigravity" };

function Toggle({ on, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label: string }) {
  return <input type="checkbox" className="switch" role="switch" checked={on} onChange={(e) => onChange(e.target.checked)} aria-label={label} />;
}

function SkillRow({ s, onToggle }: { s: Skill; onToggle: (v: boolean) => void }) {
  const { toast } = useApp();
  const locked = s.source !== "user" && !s.enabled;
  return (
    <div className={`item ${s.enabled ? "" : "off"}`}>
      <div className="item-icon">
        <SparkIcon size={16} />
      </div>
      <div className="item-main">
        <div className="item-title">
          <b>/{s.name}</b>
          {s.source !== "user" && <span className="tag">{s.source}</span>}
          {s.engine && <span className="tag">{LABEL[s.engine]}</span>}
          {s.review && <span className="tag">검토</span>}
          {s.mode === "work" && <span className="tag warn">작업 모드</span>}
        </div>
        <div className="item-desc">{s.description || "설명 없음"}</div>
        {s.triggers.length > 0 && <div className="item-meta">자동 적용 낱말: {s.triggers.join(", ")}</div>}
      </div>
      <button className="btn sm ghost" onClick={() => api.openPath(`skill:${s.path}`).catch((e) => toast(errorText(e), "error"))} title="SKILL.md 열기">
        편집
      </button>
      {s.source === "user" ? <Toggle on={s.enabled} onChange={onToggle} label={`${s.name} 켜기`} /> : <span className="item-note">{locked ? "플러그인 꺼짐" : "플러그인 스킬"}</span>}
    </div>
  );
}

function PluginRow({ p, onToggle }: { p: Plugin; onToggle: (v: boolean) => void }) {
  const runsCode = p.extras.some((x) => x === "hooks" || x === ".mcp.json");
  return (
    <div className={`item ${p.enabled ? "" : "off"}`}>
      <div className="item-icon">
        <PuzzleIcon />
      </div>
      <div className="item-main">
        <div className="item-title">
          <b>{p.name}</b>
          {p.version && <span className="tag">v{p.version}</span>}
          {p.builtin && <span className="tag">기본 제공</span>}
          <span className="tag">스킬 {p.skills}</span>
          {p.extras.map((x) => (
            <span key={x} className={`tag ${x === "hooks" || x === ".mcp.json" ? "warn" : ""}`}>
              {x}
            </span>
          ))}
        </div>
        <div className="item-desc">{p.error ?? (p.description || "설명 없음")}</div>
        {runsCode && <div className="item-meta warn">이 플러그인은 Claude Code에서 명령(훅·MCP 서버)을 실행할 수 있습니다.</div>}
      </div>
      <Toggle
        on={p.enabled}
        label={`${p.name} 켜기`}
        onChange={async (v) => {
          if (v && !p.builtin && !(await ask({ title: `'${p.name}' 플러그인을 켤까요?`, body: "플러그인은 Claude Code 안에서 명령(훅·MCP 서버)을 실행할 수 있습니다. 믿을 수 있는 출처의 것만 켜세요.", confirmLabel: "켜기" }))) return;
          onToggle(v);
        }}
      />
    </div>
  );
}

/** 스킬·플러그인 관리. 폴더 방식이라 파일을 복사해 넣고 새로고침하면 됩니다. */
export default function SkillsDialog() {
  const { skills, refreshSkills, setDialog, toast } = useApp();
  const [tab, setTab] = useState<"skills" | "plugins">("skills");
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);

  useEffect(() => {
    refreshSkills();
  }, [refreshSkills]);

  if (!skills) return null;

  async function toggle(kind: "skill" | "plugin", id: string, v: boolean) {
    try {
      await (kind === "skill" ? api.skillToggle(id, v) : api.pluginToggle(id, v));
      await refreshSkills();
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  async function create() {
    try {
      await api.skillCreate(name);
      setName("");
      setCreating(false);
      await refreshSkills();
      toast("새 스킬을 만들고 편집기로 열었습니다. 저장한 뒤 새로고침하세요.");
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  const userSkills = skills.skills.filter((s) => s.source === "user");
  const pluginSkills = skills.skills.filter((s) => s.source !== "user");
  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && setDialog(null)}>
      <div className="modal wide" role="dialog" aria-label="스킬과 플러그인">
        <div className="modal-scroll">
        <div className="modal-head">
          <Segmented
            as="tab"
            label="스킬과 플러그인"
            className="tabs"
            value={tab}
            options={[
              { value: "skills", label: <>스킬 <span>{skills.skills.length}</span></> },
              { value: "plugins", label: <>플러그인 <span>{skills.plugins.length}</span></> },
            ]}
            onChange={setTab}
          />
          <div className="row">
            <button className="btn sm" onClick={() => refreshSkills()} title="폴더를 다시 읽기">
              <RefreshIcon /> 새로고침
            </button>
            <button className="btn sm" onClick={() => api.openPath(tab).catch((e) => toast(errorText(e), "error"))}>
              <FolderIcon /> 폴더 열기
            </button>
            <button className="icon" onClick={() => setDialog(null)} aria-label="닫기">
              <CloseIcon />
            </button>
          </div>
        </div>

        {tab === "skills" ? (
          <>
            <p className="help">
              스킬은 AI에게 주는 지시문 묶음입니다. 입력창에서 <code>/이름</code>으로 부르거나, 지정한 낱말이 들어간 요청에 자동으로 적용됩니다. 형식은 Claude Code와 같은 <code>SKILL.md</code>입니다.
            </p>
            {creating ? (
              <div className="row create">
                <input type="text" autoFocus value={name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && create()} placeholder="스킬 이름 (예: meeting-notes)" />
                <button className="btn primary sm" onClick={create} disabled={!name.trim()}>
                  만들기
                </button>
                <button className="btn sm" onClick={() => setCreating(false)}>
                  취소
                </button>
              </div>
            ) : (
              <button className="btn sm add" onClick={() => setCreating(true)}>
                <PlusIcon /> 새 스킬
              </button>
            )}
            <div className="list">
              {userSkills.map((s) => (
                <SkillRow key={s.id} s={s} onToggle={(v) => toggle("skill", s.id, v)} />
              ))}
              {pluginSkills.length > 0 && <div className="list-group">플러그인이 제공하는 스킬</div>}
              {pluginSkills.map((s) => (
                <SkillRow key={s.id} s={s} onToggle={() => {}} />
              ))}
            </div>
          </>
        ) : (
          <>
            <p className="help">
              플러그인은 스킬·명령·에이전트를 묶은 폴더입니다(Claude Code 플러그인 형식: <code>.claude-plugin/plugin.json</code>). 폴더를 플러그인 폴더에 복사하고 새로고침하세요. 켠 플러그인은 Claude Code에 함께 넘겨지고, 그 안의 스킬은 다른 AI에서도 쓸 수 있습니다.
            </p>
            <div className="list">
              {skills.plugins.length === 0 && <div className="empty">플러그인이 없습니다.</div>}
              {skills.plugins.map((p) => (
                <PluginRow key={p.id} p={p} onToggle={(v) => toggle("plugin", p.id, v)} />
              ))}
            </div>
          </>
        )}
        </div>
      </div>
    </div>
  );
}
