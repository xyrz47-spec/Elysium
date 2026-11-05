//! 지금 돌고 있는 작업의 에이전트 상태. 화면 이벤트를 그대로 따라가 늦게 붙은 폰에 한 번에 준다(순수 상태 기계).

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// 에이전트마다 남기는 출력 끝부분(글자)
const TAIL_CHARS: usize = 6000;
/// 작업마다 남기는 총괄 활동 기록 수
const TIMELINE_KEEP: usize = 40;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Tok {
    pub input: u64,
    pub output: u64,
    pub cost: f64,
}

impl Tok {
    fn add(self, o: Tok) -> Tok {
        Tok { input: self.input + o.input, output: self.output + o.output, cost: self.cost + o.cost }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnap {
    pub key: String,
    pub engine: Value,
    pub label: String,
    pub role: String,
    pub state: String,
    pub wait: Option<String>,
    pub last: Option<String>,
    pub stopped: bool,
    /// 끝난 차례의 합계와 지금 차례의 값
    pub done: Tok,
    pub cur: Tok,
    pub tail: String,
    /// 도구를 쓴 횟수
    pub tools: u32,
    /// 지금 백그라운드에서 도는 명령 수
    pub bg: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnap {
    pub task_id: String,
    pub conversation_id: String,
    pub work: bool,
    pub lead: bool,
    pub parts: BTreeMap<String, String>,
    pub agents: BTreeMap<String, AgentSnap>,
    /// 총괄이 한 일(최근 몇 개)
    pub timeline: Vec<Value>,
    pub commits: u32,
}

#[derive(Default)]
pub struct Snap {
    tasks: HashMap<String, TaskSnap>,
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn tok(v: &Value, i: &str, o: &str, c: &str) -> Option<Tok> {
    (v[i].is_u64() || v[o].is_u64()).then(|| Tok { input: v[i].as_u64().unwrap_or(0), output: v[o].as_u64().unwrap_or(0), cost: v[c].as_f64().unwrap_or(0.0) })
}

impl Snap {
    fn task(&mut self, p: &Value) -> Option<&mut TaskSnap> {
        let id = p["taskId"].as_str()?.to_string();
        let t = self.tasks.entry(id.clone()).or_insert_with(|| TaskSnap { task_id: id, ..Default::default() });
        if t.conversation_id.is_empty() {
            t.conversation_id = s(p, "conversationId");
        }
        Some(t)
    }

    /// 화면 이벤트 하나를 반영한다. 모르는 이벤트는 무시한다.
    pub fn apply(&mut self, name: &str, p: &Value) {
        match name {
            "task:done" => {
                if let Some(id) = p["taskId"].as_str() {
                    self.tasks.remove(id);
                }
            }
            "task:started" => {
                if let Some(t) = self.task(p) {
                    t.work = p["work"].as_bool().unwrap_or(false);
                }
            }
            "lead:started" => {
                if let Some(t) = self.task(p) {
                    t.lead = true;
                }
            }
            "lead:activity" => {
                if let Some(t) = self.task(p) {
                    t.timeline.push(serde_json::json!({ "kind": p["kind"], "text": p["text"], "at": p["at"] }));
                    let over = t.timeline.len().saturating_sub(TIMELINE_KEEP);
                    t.timeline.drain(..over);
                }
            }
            "lead:commit" => {
                if let Some(t) = self.task(p) {
                    t.commits += 1;
                }
            }
            "task:parts" => {
                if let Some(t) = self.task(p) {
                    t.parts = p["parts"].as_array().into_iter().flatten().map(|x| (s(x, "key"), s(x, "title"))).collect();
                }
            }
            "task:agent" => {
                let Some(t) = self.task(p) else { return };
                let key = s(p, "key");
                let a = t.agents.entry(key.clone()).or_insert_with(|| AgentSnap { key, ..Default::default() });
                a.engine = p["engine"].clone();
                a.label = s(p, "label");
                a.role = s(p, "role");
                a.state = s(p, "state");
                a.wait = None;
                a.stopped = p["stopped"].as_bool().unwrap_or(false);
                if a.state == "run" {
                    a.cur = Tok::default();
                } else {
                    let final_ = tok(p, "inputTokens", "outputTokens", "costUsd").unwrap_or(a.cur);
                    a.done = a.done.add(final_);
                    a.cur = Tok::default();
                    a.last = None;
                }
            }
            _ => {
                let Some(t) = p["taskId"].as_str().and_then(|id| self.tasks.get_mut(id)) else { return };
                let Some(a) = p["key"].as_str().and_then(|k| t.agents.get_mut(k)) else { return };
                match name {
                    "task:agent-activity" => {
                        a.last = p["text"].as_str().map(str::to_string);
                        a.tools += 1;
                    }
                    "task:agent-wait" => {
                        a.state = "run".into();
                        a.wait = p["note"].as_str().map(str::to_string);
                    }
                    "task:agent-usage" => a.cur = tok(p, "input", "output", "costUsd").unwrap_or_default(),
                    "task:agent-bg" => a.bg = p["count"].as_u64().unwrap_or(0) as u32,
                    "task:agent-delta" => {
                        a.tail.push_str(p["delta"].as_str().unwrap_or(""));
                        let n = a.tail.chars().count();
                        if n > TAIL_CHARS * 2 {
                            a.tail = a.tail.chars().skip(n - TAIL_CHARS).collect();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn list(&self) -> Vec<TaskSnap> {
        let mut v: Vec<TaskSnap> = self.tasks.values().cloned().collect();
        for t in &mut v {
            for a in t.agents.values_mut() {
                let n = a.tail.chars().count();
                if n > TAIL_CHARS {
                    a.tail = a.tail.chars().skip(n - TAIL_CHARS).collect();
                }
            }
        }
        v.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn follows_agent_events_and_forgets_finished_tasks() {
        let mut s = Snap::default();
        s.apply("task:started", &json!({ "taskId": "t", "conversationId": "c", "work": true }));
        s.apply("lead:started", &json!({ "taskId": "t" }));
        s.apply("task:parts", &json!({ "taskId": "t", "parts": [{ "key": "agent:0", "title": "화면" }] }));
        s.apply("task:agent", &json!({ "taskId": "t", "key": "agent:0", "engine": "claude", "label": "Claude Code", "role": "agent", "state": "run" }));
        s.apply("task:agent-activity", &json!({ "taskId": "t", "key": "agent:0", "text": "Edit · a.ts" }));
        s.apply("task:agent-usage", &json!({ "taskId": "t", "key": "agent:0", "input": 100, "output": 7, "costUsd": 0.01 }));
        s.apply("task:agent-delta", &json!({ "taskId": "t", "key": "agent:0", "delta": "고치는 중" }));
        let l = s.list();
        let a = &l[0].agents["agent:0"];
        assert!(l[0].lead && l[0].work && l[0].conversation_id == "c" && l[0].parts["agent:0"] == "화면");
        assert_eq!((a.state.as_str(), a.last.as_deref(), a.cur.input, a.tail.as_str()), ("run", Some("Edit · a.ts"), 100, "고치는 중"));
        s.apply("task:agent", &json!({ "taskId": "t", "key": "agent:0", "state": "ok", "inputTokens": 150, "outputTokens": 9, "costUsd": 0.02 }));
        s.apply("task:agent-wait", &json!({ "taskId": "t", "key": "agent:0", "note": "idle" }));
        let a = &s.list()[0].agents["agent:0"];
        assert_eq!((a.done.input, a.cur.input, a.state.as_str(), a.wait.as_deref()), (150, 0, "run", Some("idle")), "끝난 차례는 합계로, 쉬는 워커는 메시지 대기");
        s.apply("lead:activity", &json!({ "taskId": "t", "kind": "commit", "text": "abc feat", "at": "x" }));
        s.apply("lead:commit", &json!({ "taskId": "t", "sha": "abc" }));
        let t = &s.list()[0];
        assert_eq!((t.timeline.len(), t.commits, t.agents["agent:0"].tools), (1, 1, 1));
        s.apply("task:done", &json!({ "taskId": "t" }));
        assert!(s.list().is_empty());
        s.apply("task:agent-delta", &json!({ "taskId": "x", "key": "k", "delta": "?" }));
        assert!(s.list().is_empty(), "모르는 작업의 글은 무시한다");
    }
}
