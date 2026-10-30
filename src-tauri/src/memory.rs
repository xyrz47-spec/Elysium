//! 세션 메모리: 같은 세션(대화) 안의 모든 AI(초안·검토·토론·판정, CLI·API)가 같은 기억을 쓴다.
//!
//! - 원본은 messages 표(사용자 요청과 최종 답, 누가 답했는지)다.
//! - 오래된 대화는 싼 AI가 "세션 요약"으로 접어 둔다(conversations.memory_summary, memory_upto까지).
//! - 매 실행마다 [세션 요약] + [최근 대화(요약 뒤)]를 요청 앞에 붙인다.
//! - Claude Code처럼 자기 세션을 이어 쓰는 엔진에는 그 엔진이 아직 못 본 부분(다른 AI가 답한 차례 등)만 붙인다.
//!
//! 토큰 절약(2026-10-30): 이전 대화는 매 호출마다 다시 보내므로 짧게 둔다.
//! - 그대로 붙이는 최근 대화는 3차례(6개)까지, 나머지는 요약으로 넘긴다.
//! - AI 답은 앞부분만, 코드 블록은 "(코드 N줄 생략)"으로 줄인다(파일에 이미 있거나 다시 물으면 된다).
//!
//! JEV 거르기(2026-10-31): JEV가 켜져 있으면 요약 줄과 최근 대화 하나하나를 "지금 요청에 필요한가"로 판단해
//! 필요한 것만 붙인다(`candidates` → `filter_questions` → `select` → `context(.., Some(&sel))`).

use crate::store::Message;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// 그대로 붙이는 최근 메시지 수(요청·답 3차례)
pub const RECENT: usize = 6;
/// 최근 대화에 쓰는 글자 수 한도
pub const RECENT_BUDGET: usize = 4000;
/// 메시지 하나에서 붙이는 최대 글자 수(사용자 요청 / AI 답)
const PER_USER: usize = 1500;
const PER_ANSWER: usize = 700;
/// 요약 갱신 기준: 요약 밖으로 밀려난 메시지가 이만큼 쌓이면 접는다
const FOLD_MESSAGES: usize = 4;
const FOLD_CHARS: usize = 2500;
/// 요약 길이(글자)
pub const SUMMARY_CHARS: usize = 1400;

/// JEV에게 물어볼 요약 줄 수 상한(넘는 줄은 거르지 않고 그대로 둔다)
const FILTER_MAX_LINES: usize = 24;
/// 거를 기억이 이보다 짧으면 JEV를 부르지 않는다(기다리는 시간이 아끼는 토큰보다 크다).
pub const FILTER_MIN_CHARS: usize = 600;
/// JEV가 "필요"라고 본 확률이 이 값 이상이면 남긴다.
const KEEP: f64 = 0.5;
/// 바로 앞 차례(가장 최근 요청·답)는 이어지는 요청이 많아 더 너그럽게 남긴다.
const KEEP_LATEST: f64 = 0.25;
/// JEV에 보내는 지금 요청의 최대 글자 수
const FILTER_REQUEST_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub role: String,
    /// 답한 AI 이름(최종 답을 만든 쪽: 검토·판정 > 초안)
    pub by: Option<String>,
    pub content: String,
    pub at: String,
}

fn label(id: &str) -> String {
    crate::engines::EngineId::parse(id).map(|e| e.label().to_string()).unwrap_or_else(|| id.to_string())
}

pub fn turns(msgs: &[Message]) -> Vec<Turn> {
    msgs.iter()
        .map(|m| {
            let d = m.data.as_ref();
            let pick = |k: &str| d.and_then(|d| d.get(k)).and_then(|v| v.as_str()).map(label);
            Turn {
                role: m.role.clone(),
                by: if m.role == "assistant" { pick("judgedBy").or_else(|| pick("reviewedBy")).or_else(|| pick("draftedBy")) } else { None },
                content: m.content.clone(),
                at: m.created_at.clone(),
            }
        })
        .collect()
}

/// AI 답의 코드 블록 본문을 "(코드 N줄 생략)"으로 바꾼다. 코드 언어 표시는 남겨 무엇을 다뤘는지는 알 수 있게 한다.
fn compact_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(4096));
    let mut in_code = false;
    let mut n = 0;
    for l in text.lines() {
        if l.trim_start().starts_with("```") {
            if in_code {
                out.push_str(&format!("(코드 {n}줄 생략)\n```\n"));
                in_code = false;
            } else {
                out.push_str(l);
                out.push('\n');
                in_code = true;
                n = 0;
            }
        } else if in_code {
            n += 1;
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    if in_code {
        out.push_str(&format!("(코드 {n}줄 생략)\n"));
    }
    out.trim_end().to_string()
}

fn line(t: &Turn) -> String {
    let (who, body, max) = if t.role == "user" {
        ("사용자".to_string(), t.content.clone(), PER_USER)
    } else {
        (format!("AI({})", t.by.as_deref().unwrap_or("?")), compact_code(&t.content), PER_ANSWER)
    };
    let c: String = body.chars().take(max).collect();
    let cut = if body.chars().count() > max { " …(생략)" } else { "" };
    format!("{who}: {c}{cut}")
}

/// 글자 한도 안에서 최신부터 거꾸로 담고, 원래 순서로 돌려준다.
fn within_budget(ts: &[&Turn], budget: usize) -> Vec<String> {
    let mut out = vec![];
    let mut used = 0;
    for t in ts.iter().rev() {
        let l = line(t);
        let n = l.chars().count();
        if used + n > budget && !out.is_empty() {
            break;
        }
        used += n;
        out.push(l);
    }
    out.reverse();
    out
}

// ---------------------------------------------------------------------------
// JEV 거르기

/// 거르기 후보: 요약 항목과 최근 대화(history 인덱스).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Candidates {
    /// JEV에게 물어볼 요약 항목(불릿 하나 = 항목 하나, 이어지는 줄은 같은 항목)
    pub lines: Vec<String>,
    /// 묻지 않고 그대로 두는 요약 항목(상한을 넘은 것)
    pub rest: Vec<String>,
    /// 최근 대화의 history 인덱스(오래된 것부터). context가 붙일 수 있는 범위(마지막 RECENT개)와 같다.
    pub turns: Vec<usize>,
}

impl Candidates {
    fn chars(&self, history: &[Turn]) -> usize {
        self.lines.iter().map(|l| l.chars().count()).sum::<usize>() + self.turns.iter().map(|&i| line(&history[i]).chars().count()).sum::<usize>()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.turns.is_empty()
    }
}

fn starts_item(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with('-') || t.starts_with('*') || t.starts_with('•') || t.starts_with('#') || {
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        digits > 0 && t[digits..].starts_with(['.', ')'])
    }
}

/// 요약을 항목으로 나눈다: 불릿·번호로 시작하는 줄이 새 항목, 들여 쓴 이어지는 줄은 앞 항목에 붙인다.
fn summary_items(summary: &str) -> Vec<String> {
    let mut items: Vec<String> = vec![];
    for l in summary.lines().filter(|l| !l.trim().is_empty()) {
        match items.last_mut() {
            Some(last) if !starts_item(l) && l.starts_with([' ', '\t']) => {
                last.push('\n');
                last.push_str(l);
            }
            _ => items.push(l.trim_end().to_string()),
        }
    }
    items
}

/// 거르기 후보를 만든다. 짧아서 거를 가치가 없으면 None.
pub fn candidates(summary: Option<&str>, history: &[Turn]) -> Option<Candidates> {
    let mut items = summary.map(summary_items).unwrap_or_default();
    let rest = if items.len() > FILTER_MAX_LINES { items.split_off(FILTER_MAX_LINES) } else { vec![] };
    let c = Candidates { lines: items, rest, turns: (history.len().saturating_sub(RECENT)..history.len()).collect() };
    (!c.is_empty() && c.chars(history) >= FILTER_MIN_CHARS).then_some(c)
}

/// JEV에 보낼 state와 질문 묶음. 요약 항목은 s0…, 최근 대화는 t0…(오래된 것부터) 키로 묻는다.
pub fn filter_questions(prompt: &str, c: &Candidates, history: &[Turn]) -> (Value, Map<String, Value>) {
    let mut state = Map::new();
    state.insert("request".into(), Value::String(prompt.chars().take(FILTER_REQUEST_CHARS).collect()));
    let mut q = Map::new();
    let criteria = |what: &str| {
        json!({
            "needed": format!("`request` refers to, continues or depends on this {what}: facts, decisions, user preferences or rules (language, tone, format, things to avoid), file or code names, or unfinished work in it are needed to answer `request` correctly"),
            "skip": format!("This {what} is unrelated to `request` or already settled, and the answer to `request` would be just as good without it")
        })
    };
    for (i, l) in c.lines.iter().enumerate() {
        let k = format!("s{i}");
        state.insert(k.clone(), Value::String(l.clone()));
        q.insert(k.clone(), json!({ "type": "choice", "instructions": format!("`{k}` is one item of the summary of this chat session so far. Does the assistant need to see `{k}` to answer `request`?"), "criteria": criteria("summary item") }));
    }
    for (n, &i) in c.turns.iter().enumerate() {
        let k = format!("t{n}");
        state.insert(k.clone(), Value::String(line(&history[i])));
        q.insert(k.clone(), json!({ "type": "choice", "instructions": format!("`{k}` is an earlier message in this chat session (higher number = more recent). Does the assistant need to see `{k}` to answer `request`?"), "criteria": criteria("earlier message") }));
    }
    (Value::Object(state), q)
}

/// JEV 거르기 결과: 지금 요청에 필요하다고 본 요약 항목·최근 대화만 남긴다.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    /// 남긴 요약. None이면 요약을 붙이지 않는다.
    pub summary: Option<String>,
    /// 남긴 최근 대화(history 인덱스). 후보가 아니었던 차례는 원래도 붙지 않는다.
    pub keep: HashSet<usize>,
    pub lines_total: usize,
    pub lines_kept: usize,
    pub turns_total: usize,
    pub turns_kept: usize,
}

impl Selection {
    /// 화면의 판단 근거에 보여 줄 한 줄
    pub fn reason(&self) -> String {
        let mut parts = vec![];
        if self.turns_total > 0 {
            parts.push(format!("최근 대화 {}/{}개", self.turns_kept, self.turns_total));
        }
        if self.lines_total > 0 {
            parts.push(format!("요약 {}/{}줄", self.lines_kept, self.lines_total));
        }
        format!("JEV: 이전 기억 중 지금 요청에 필요한 것만 전달({})", parts.join(", "))
    }
}

/// need(키) = JEV가 "필요"라고 본 확률. 답이 없는 항목은 남긴다(모르면 버리지 않는다).
pub fn select(c: &Candidates, need: impl Fn(&str) -> Option<f64>) -> Selection {
    let keep_if = |k: &str, min: f64| need(k).is_none_or(|p| p >= min);
    let mut kept: Vec<String> = c.lines.iter().enumerate().filter(|(i, _)| keep_if(&format!("s{i}"), KEEP)).map(|(_, l)| l.clone()).collect();
    let lines_kept = kept.len();
    kept.extend(c.rest.iter().cloned());
    let n = c.turns.len();
    let keep: HashSet<usize> = c
        .turns
        .iter()
        .enumerate()
        .filter(|(pos, _)| keep_if(&format!("t{pos}"), if pos + 2 >= n { KEEP_LATEST } else { KEEP }))
        .map(|(_, &i)| i)
        .collect();
    Selection {
        summary: (!kept.is_empty()).then(|| kept.join("\n")),
        lines_total: c.lines.len(),
        lines_kept,
        turns_total: n,
        turns_kept: keep.len(),
        keep,
    }
}

// ---------------------------------------------------------------------------

/// 요청 앞에 붙일 세션 메모리.
/// - history: 이번 요청을 뺀 이 세션의 메시지(오래된 것부터)
/// - seen_at: 이 엔진이 자기 세션으로 마지막에 본 시각(이어 쓰는 엔진만). None이면 요약 + 최근 대화를 다 붙인다.
/// - sel: JEV가 거른 결과(있으면 남긴 요약·대화만 붙인다)
pub fn context(summary: Option<&str>, upto: Option<&str>, history: &[Turn], seen_at: Option<&str>, prompt: &str, sel: Option<&Selection>) -> String {
    let summary = match sel {
        Some(s) => s.summary.as_deref(),
        None => summary,
    }
    .map(str::trim)
    .filter(|s| !s.is_empty());
    let indexed: Vec<(usize, &Turn)> = history.iter().enumerate().collect();
    let after_summary: Vec<(usize, &Turn)> = indexed.iter().copied().filter(|(_, t)| upto.is_none_or(|u| t.at.as_str() > u)).collect();
    let (show_summary, recent, head) = match seen_at {
        Some(seen) => {
            let unseen: Vec<(usize, &Turn)> = indexed.iter().copied().filter(|(_, t)| t.at.as_str() > seen).collect();
            // 요약된 부분도 못 봤다면(이 엔진이 오래 쉬었으면) 요약도 준다.
            let missed_summary = upto.is_some_and(|u| seen < u);
            // 오래 쉬었어도 최근 몇 차례만 준다(나머지는 요약에 있다).
            let skip = unseen.len().saturating_sub(RECENT);
            (missed_summary, unseen[skip..].to_vec(), "[이 세션에서 당신이 아직 못 본 대화 — 다른 AI가 답한 차례 포함]")
        }
        None => {
            let skip = after_summary.len().saturating_sub(RECENT);
            (true, after_summary[skip..].to_vec(), "[최근 대화]")
        }
    };
    let recent: Vec<&Turn> = recent.into_iter().filter(|(i, _)| sel.is_none_or(|s| s.keep.contains(i))).map(|(_, t)| t).collect();
    let lines = within_budget(&recent, RECENT_BUDGET);
    if (summary.is_none() || !show_summary) && lines.is_empty() {
        return prompt.to_string();
    }
    let mut s = String::from(if sel.is_some() {
        "[세션 메모리: 이어지는 대화입니다. JEV가 지금 요청에 필요한 부분만 골라 붙였습니다. 앞서 정한 내용과 사용자의 선호를 지키세요. AI 답의 코드는 줄여 두었으니 필요하면 파일을 읽으세요.]\n\n"
    } else {
        "[세션 메모리: 이어지는 대화입니다. 앞서 정한 내용과 사용자의 선호를 지키세요. AI 답의 코드는 줄여 두었으니 필요하면 파일을 읽으세요.]\n\n"
    });
    if let (true, Some(sum)) = (show_summary, summary) {
        s.push_str(&format!("[세션 요약]\n{sum}\n\n"));
    }
    if !lines.is_empty() {
        s.push_str(head);
        s.push('\n');
        s.push_str(&lines.join("\n\n"));
        s.push_str("\n\n");
    }
    s.push_str(&format!("[지금 요청]\n{prompt}"));
    s
}

/// 요약 밖으로 밀려나 아직 접지 않은 메시지(최근 RECENT개 이전, upto 이후). 충분히 쌓였을 때만 돌려준다.
pub fn to_fold<'a>(history: &'a [Turn], upto: Option<&str>) -> Option<Vec<&'a Turn>> {
    let aged = history.len().saturating_sub(RECENT);
    let pending: Vec<&Turn> = history[..aged].iter().filter(|t| upto.is_none_or(|u| t.at.as_str() > u)).collect();
    let chars: usize = pending.iter().map(|t| t.content.chars().count()).sum();
    (pending.len() >= FOLD_MESSAGES || chars >= FOLD_CHARS).then_some(pending).filter(|p| !p.is_empty())
}

pub fn summary_prompt(prev: Option<&str>, fold: &[&Turn]) -> String {
    let mut s = format!(
        "아래는 한 작업 세션의 [기존 요약]과 그 뒤에 이어진 [대화]입니다. 이 세션을 다른 AI가 이어받아도 같은 맥락으로 일할 수 있도록 요약을 갱신하세요.\n\
         - 한국어 불릿 목록, {SUMMARY_CHARS}자 이내. 인사·잡담은 빼세요.\n\
         - 불릿 하나에는 한 가지 내용만 적으세요(필요한 항목만 골라 전달할 수 있게).\n\
         - 남길 것: 사용자의 목표와 선호(말투·형식·금지 사항), 정한 결정과 그 이유, 확인된 사실·숫자, 다룬 파일·폴더·코드 이름, 아직 남은 일·약속.\n\
         - 어느 AI가 무엇을 했는지는 결정에 영향이 있을 때만 적으세요. 추측은 넣지 마세요.\n\
         - 요약만 출력하세요(머리말·설명 없이).\n\n[기존 요약]\n{}\n\n[대화]\n",
        prev.map(str::trim).filter(|p| !p.is_empty()).unwrap_or("(없음)")
    );
    for t in fold {
        s.push_str(&line(t));
        s.push_str("\n\n");
    }
    s
}

/// 요약 결과 다듬기: 너무 길면 자른다.
pub fn clean_summary(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= SUMMARY_CHARS + 200 {
        t.to_string()
    } else {
        t.chars().take(SUMMARY_CHARS + 200).collect::<String>() + " …"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(role: &str, by: Option<&str>, c: &str, at: &str) -> Turn {
        Turn { role: role.into(), by: by.map(str::to_string), content: c.into(), at: at.into() }
    }

    #[test]
    fn everyone_gets_summary_and_recent_turns_with_who_answered() {
        let h = vec![t("user", None, "이름은 호야야", "2026-10-17T01:00:00+00:00"), t("assistant", Some("NVIDIA API"), "알겠어요", "2026-10-17T01:00:05+00:00")];
        let c = context(Some("- 사용자는 존댓말을 싫어함"), None, &h, None, "내 이름이 뭐였지?", None);
        assert!(c.contains("[세션 요약]\n- 사용자는 존댓말을 싫어함"));
        assert!(c.contains("사용자: 이름은 호야야") && c.contains("AI(NVIDIA API): 알겠어요"));
        assert!(c.ends_with("[지금 요청]\n내 이름이 뭐였지?"));
        assert_eq!(context(None, None, &[], None, "안녕", None), "안녕", "처음이면 요청만");
    }

    #[test]
    fn resumed_engine_gets_only_what_it_missed() {
        let h = vec![
            t("user", None, "A", "2026-10-17T01:00:00+00:00"),
            t("assistant", Some("Claude Code"), "a", "2026-10-17T01:00:05+00:00"),
            t("user", None, "B", "2026-10-17T02:00:00+00:00"),
            t("assistant", Some("NVIDIA API"), "b", "2026-10-17T02:00:05+00:00"),
        ];
        let c = context(Some("요약"), Some("2026-10-17T00:30:00+00:00"), &h, Some("2026-10-17T01:00:06+00:00"), "C", None);
        assert!(c.contains("사용자: B") && c.contains("AI(NVIDIA API): b"));
        assert!(!c.contains("사용자: A"), "이미 본 차례는 다시 안 줌");
        assert!(!c.contains("[세션 요약]"), "요약 이후에 본 엔진에는 요약 생략");
        assert_eq!(context(Some("요약"), None, &h, Some("2026-10-17T03:00:00+00:00"), "C", None), "C", "다 봤으면 요청만");
        let c = context(Some("요약"), Some("2026-10-17T05:00:00+00:00"), &h, Some("2026-10-17T01:00:06+00:00"), "C", None);
        assert!(c.contains("[세션 요약]"), "요약된 부분을 못 봤으면 요약도");
    }

    #[test]
    fn summary_covers_old_turns_and_recent_stays_verbatim() {
        let mut h = vec![];
        for i in 0..20 {
            h.push(t(if i % 2 == 0 { "user" } else { "assistant" }, (i % 2 == 1).then_some("Claude Code"), &format!("m{i}"), &format!("2026-10-17T01:{i:02}:00+00:00")));
        }
        let fold = to_fold(&h, None).unwrap();
        assert_eq!(fold.len(), 14, "최근 6개 이전의 14개를 접는다");
        let upto = fold.last().unwrap().at.clone();
        assert!(to_fold(&h, Some(&upto)).is_none(), "접은 뒤에는 더 접을 것이 없다");
        let c = context(Some("요약"), Some(&upto), &h, None, "다음", None);
        assert!(c.contains("m14") && c.contains("m19") && !c.contains("m13\n"), "요약 뒤 6개만 그대로");
        assert!(summary_prompt(None, &fold).contains("AI(Claude Code): m1"));
    }

    #[test]
    fn long_history_respects_budget() {
        let big = "가".repeat(2400);
        let h: Vec<Turn> = (0..12).map(|i| t("user", None, &big, &format!("2026-10-17T01:{i:02}:00+00:00"))).collect();
        let c = context(None, None, &h, None, "x", None);
        assert!(c.chars().count() < RECENT_BUDGET + 600);
    }

    #[test]
    fn answers_are_short_and_code_is_folded() {
        let code = format!("설명입니다\n```rust\n{}```\n끝", "let x = 1;\n".repeat(40));
        let l = line(&t("assistant", Some("Claude Code"), &code, "x"));
        assert!(l.contains("```rust") && l.contains("(코드 40줄 생략)") && l.contains("끝") && !l.contains("let x"));
        let long = "나".repeat(3000);
        assert!(line(&t("assistant", None, &long, "x")).chars().count() < PER_ANSWER + 30);
        assert!(line(&t("user", None, &long, "x")).contains(&"나".repeat(PER_USER)), "사용자 요청은 더 길게 남긴다");
    }

    fn long_session() -> (String, Vec<Turn>) {
        let summary = "- 사용자는 한국어 답을 원함\n- 프로젝트는 Tauri 앱\n  (Rust + React)\n- 색상 테마는 보라색으로 정함\n1. 남은 일: 로그인 화면".to_string();
        let h = (0..10)
            .map(|i| t(if i % 2 == 0 { "user" } else { "assistant" }, (i % 2 == 1).then_some("Claude Code"), &format!("m{i} {}", "내용".repeat(60)), &format!("2026-10-17T01:{i:02}:00+00:00")))
            .collect();
        (summary, h)
    }

    #[test]
    fn candidates_split_summary_items_and_take_recent_turns() {
        let (summary, h) = long_session();
        let c = candidates(Some(&summary), &h).unwrap();
        assert_eq!(c.lines.len(), 4, "들여 쓴 이어지는 줄은 앞 항목에 붙는다");
        assert!(c.lines[1].contains("(Rust + React)"));
        assert_eq!(c.turns, vec![4, 5, 6, 7, 8, 9], "context가 붙일 수 있는 마지막 RECENT개");
        assert!(candidates(Some("- 짧음"), &h[..1]).is_none(), "짧으면 거르지 않는다");
        assert!(candidates(None, &[]).is_none());
        let many: String = (0..30).map(|i| format!("- 항목 {i} {}\n", "가".repeat(30))).collect();
        let c = candidates(Some(&many), &[]).unwrap();
        assert_eq!((c.lines.len(), c.rest.len()), (FILTER_MAX_LINES, 30 - FILTER_MAX_LINES));
    }

    #[test]
    fn filter_questions_use_state_keys() {
        let (summary, h) = long_session();
        let c = candidates(Some(&summary), &h).unwrap();
        let (state, q) = filter_questions("로그인 화면 만들어줘", &c, &h);
        assert_eq!(q.len(), c.lines.len() + c.turns.len());
        assert_eq!(state["request"], "로그인 화면 만들어줘");
        assert!(state["s0"].as_str().unwrap().contains("한국어"));
        assert!(state["t5"].as_str().unwrap().starts_with("AI(Claude Code): m9"));
        assert_eq!(q["t0"]["type"], "choice");
        assert!(q["s3"]["instructions"].as_str().unwrap().contains("`s3`"));
        assert!(q["t2"]["criteria"]["needed"].is_string() && q["t2"]["criteria"]["skip"].is_string());
    }

    #[test]
    fn selection_keeps_only_needed_and_context_follows_it() {
        let (summary, h) = long_session();
        let c = candidates(Some(&summary), &h).unwrap();
        // s0(선호)·s3(남은 일)과 t0(m4)만 필요, 바로 앞 차례 t4(m8)는 애매(0.3), t5(m9)는 불필요(0.1)
        let p = |k: &str| -> Option<f64> {
            Some(match k {
                "s0" | "s3" | "t0" => 0.9,
                "t4" => 0.3,
                "t5" => 0.1,
                _ => 0.05,
            })
        };
        let sel = select(&c, p);
        assert_eq!((sel.lines_kept, sel.lines_total, sel.turns_kept, sel.turns_total), (2, 4, 2, 6));
        assert!(sel.keep.contains(&4) && sel.keep.contains(&8), "바로 앞 차례는 낮은 기준으로 남긴다");
        assert!(!sel.keep.contains(&9));
        let ctx = context(Some(&summary), None, &h, None, "로그인 화면 만들어줘", Some(&sel));
        assert!(ctx.contains("JEV가 지금 요청에 필요한 부분만"));
        assert!(ctx.contains("한국어 답") && ctx.contains("로그인 화면") && !ctx.contains("보라색") && !ctx.contains("Tauri"));
        assert!(ctx.contains("m4 ") && ctx.contains("m8 ") && !ctx.contains("m9 ") && !ctx.contains("m5 "));
        assert!(!ctx.contains("m3 "), "버린 자리로 오래된 대화가 밀려 들어오지 않는다");
        assert!(sel.reason().contains("최근 대화 2/6개") && sel.reason().contains("요약 2/4줄"));
    }

    #[test]
    fn nothing_needed_sends_only_the_request_and_missing_answers_are_kept() {
        let (summary, h) = long_session();
        let c = candidates(Some(&summary), &h).unwrap();
        let none = select(&c, |_| Some(0.0));
        assert_eq!(none.summary, None);
        assert_eq!(context(Some(&summary), None, &h, None, "안녕", Some(&none)), "안녕", "필요한 기억이 없으면 요청만");
        let unknown = select(&c, |_| None);
        assert_eq!((unknown.lines_kept, unknown.turns_kept), (4, 6), "JEV 답이 없는 항목은 버리지 않는다");
        // 이어 쓰는 엔진도 거른 결과를 따른다
        let ctx = context(Some(&summary), None, &h, Some("2026-10-17T01:07:30+00:00"), "다음", Some(&none));
        assert_eq!(ctx, "다음");
    }
}
