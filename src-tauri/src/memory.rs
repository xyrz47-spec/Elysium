//! 세션 메모리: 같은 세션(대화) 안의 모든 AI(초안·검토·토론·판정, CLI·API)가 같은 기억을 쓴다.
//!
//! - 원본은 messages 표(사용자 요청과 최종 답, 누가 답했는지)다.
//! - 오래된 대화는 싼 AI가 "세션 요약"으로 접어 둔다(conversations.memory_summary, memory_upto까지).
//! - 매 실행마다 [세션 요약] + [최근 대화(요약 뒤)]를 요청 앞에 붙인다.
//! - Claude Code처럼 자기 세션을 이어 쓰는 엔진에는 그 엔진이 아직 못 본 부분(다른 AI가 답한 차례 등)만 붙인다.

use crate::store::Message;

/// 그대로 붙이는 최근 메시지 수(요청·답 6차례)
pub const RECENT: usize = 12;
/// 최근 대화에 쓰는 글자 수 한도
pub const RECENT_BUDGET: usize = 9000;
/// 메시지 하나에서 붙이는 최대 글자 수
const PER_MESSAGE: usize = 2500;
/// 요약 갱신 기준: 요약 밖으로 밀려난 메시지가 이만큼 쌓이면 접는다
const FOLD_MESSAGES: usize = 4;
const FOLD_CHARS: usize = 2500;
/// 요약 길이(글자)
pub const SUMMARY_CHARS: usize = 1400;

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

fn line(t: &Turn) -> String {
    let who = if t.role == "user" { "사용자".to_string() } else { format!("AI({})", t.by.as_deref().unwrap_or("?")) };
    let c: String = t.content.chars().take(PER_MESSAGE).collect();
    let cut = if t.content.chars().count() > PER_MESSAGE { " …(생략)" } else { "" };
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

/// 요청 앞에 붙일 세션 메모리.
/// - history: 이번 요청을 뺀 이 세션의 메시지(오래된 것부터)
/// - seen_at: 이 엔진이 자기 세션으로 마지막에 본 시각(이어 쓰는 엔진만). None이면 요약 + 최근 대화를 다 붙인다.
pub fn context(summary: Option<&str>, upto: Option<&str>, history: &[Turn], seen_at: Option<&str>, prompt: &str) -> String {
    let summary = summary.map(str::trim).filter(|s| !s.is_empty());
    let after_summary: Vec<&Turn> = history.iter().filter(|t| upto.is_none_or(|u| t.at.as_str() > u)).collect();
    let (show_summary, recent, head) = match seen_at {
        Some(seen) => {
            let unseen: Vec<&Turn> = history.iter().filter(|t| t.at.as_str() > seen).collect();
            // 요약된 부분도 못 봤다면(이 엔진이 오래 쉬었으면) 요약도 준다.
            let missed_summary = upto.is_some_and(|u| seen < u);
            (missed_summary, unseen, "[이 세션에서 당신이 아직 못 본 대화 — 다른 AI가 답한 차례 포함]")
        }
        None => {
            let skip = after_summary.len().saturating_sub(RECENT);
            (true, after_summary[skip..].to_vec(), "[최근 대화]")
        }
    };
    let lines = within_budget(&recent, RECENT_BUDGET);
    if (summary.is_none() || !show_summary) && lines.is_empty() {
        return prompt.to_string();
    }
    let mut s = String::from("이 세션의 모든 AI는 아래 세션 메모리를 같이 씁니다. 이어지는 대화로 여기고, 앞서 정한 내용과 사용자의 선호를 지키세요.\n\n");
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
        let c = context(Some("- 사용자는 존댓말을 싫어함"), None, &h, None, "내 이름이 뭐였지?");
        assert!(c.contains("[세션 요약]\n- 사용자는 존댓말을 싫어함"));
        assert!(c.contains("사용자: 이름은 호야야") && c.contains("AI(NVIDIA API): 알겠어요"));
        assert!(c.ends_with("[지금 요청]\n내 이름이 뭐였지?"));
        assert_eq!(context(None, None, &[], None, "안녕"), "안녕", "처음이면 요청만");
    }

    #[test]
    fn resumed_engine_gets_only_what_it_missed() {
        let h = vec![
            t("user", None, "A", "2026-10-17T01:00:00+00:00"),
            t("assistant", Some("Claude Code"), "a", "2026-10-17T01:00:05+00:00"),
            t("user", None, "B", "2026-10-17T02:00:00+00:00"),
            t("assistant", Some("NVIDIA API"), "b", "2026-10-17T02:00:05+00:00"),
        ];
        let c = context(Some("요약"), Some("2026-10-17T00:30:00+00:00"), &h, Some("2026-10-17T01:00:06+00:00"), "C");
        assert!(c.contains("사용자: B") && c.contains("AI(NVIDIA API): b"));
        assert!(!c.contains("사용자: A"), "이미 본 차례는 다시 안 줌");
        assert!(!c.contains("[세션 요약]"), "요약 이후에 본 엔진에는 요약 생략");
        assert_eq!(context(Some("요약"), None, &h, Some("2026-10-17T03:00:00+00:00"), "C"), "C", "다 봤으면 요청만");
        let c = context(Some("요약"), Some("2026-10-17T05:00:00+00:00"), &h, Some("2026-10-17T01:00:06+00:00"), "C");
        assert!(c.contains("[세션 요약]"), "요약된 부분을 못 봤으면 요약도");
    }

    #[test]
    fn summary_covers_old_turns_and_recent_stays_verbatim() {
        let mut h = vec![];
        for i in 0..20 {
            h.push(t(if i % 2 == 0 { "user" } else { "assistant" }, (i % 2 == 1).then_some("Claude Code"), &format!("m{i}"), &format!("2026-10-17T01:{i:02}:00+00:00")));
        }
        let fold = to_fold(&h, None).unwrap();
        assert_eq!(fold.len(), 8, "최근 12개 이전의 8개를 접는다");
        let upto = fold.last().unwrap().at.clone();
        assert!(to_fold(&h, Some(&upto)).is_none(), "접은 뒤에는 더 접을 것이 없다");
        let c = context(Some("요약"), Some(&upto), &h, None, "다음");
        assert!(c.contains("m8") && c.contains("m19") && !c.contains("m7\n"), "요약 뒤 12개만 그대로");
        assert!(summary_prompt(None, &fold).contains("AI(Claude Code): m1"));
    }

    #[test]
    fn long_history_respects_budget() {
        let big = "가".repeat(2400);
        let h: Vec<Turn> = (0..12).map(|i| t("user", None, &big, &format!("2026-10-17T01:{i:02}:00+00:00"))).collect();
        let c = context(None, None, &h, None, "x");
        assert!(c.chars().count() < RECENT_BUDGET + 600);
    }
}
