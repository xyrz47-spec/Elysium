//! 병렬 작업의 로드맵: OpenCode가 하위 작업마다 맡을 AI와 노력 단계를 정하고, 통합자는 다음 단계를 덧붙여 남은 일을 채운다. 순수 함수만 둔다.

use crate::engines::EngineId;
use crate::orchestrator::cut;
use crate::teamwork::Part;
use serde::Deserialize;

const NEXT_FENCE: &str = "elysium-roadmap";
/// 통합자가 첫 단계 뒤에 덧붙일 수 있는 단계 수
pub const MAX_EXTRA_STAGES: usize = 2;
/// 로드맵을 짜는 AI에게 보여 주는 글자 수(요청, 하위 작업 하나)
const REQUEST_CHARS: usize = 4000;
const TASK_CHARS: usize = 1500;

/// 하위 작업 하나에 OpenCode가 고른 AI와 노력 단계(못 고른 칸은 None)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pick {
    pub engine: Option<EngineId>,
    pub tier: Option<&'static str>,
}

fn tier(t: &str) -> Option<&'static str> {
    match t.trim().to_ascii_lowercase().as_str() {
        "standard" => Some("standard"),
        "deep" => Some("deep"),
        _ => None,
    }
}

fn engine_in(id: &str, engines: &[EngineId]) -> Option<EngineId> {
    EngineId::parse(id.trim()).filter(|e| engines.contains(e))
}

/// OpenCode에게 로드맵(하위 작업별 AI·노력 단계)을 묻는 글. briefs는 (AI, 설명).
pub fn assign_prompt(request: &str, parts: &[Part], briefs: &[(EngineId, &str)]) -> String {
    let list: String = parts
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{}. {}: {}{}\n", i + 1, p.title, cut(&p.task, TASK_CHARS), if p.files.is_empty() { String::new() } else { format!(" (파일: {})", p.files.join(", ")) }))
        .collect();
    let engines: String = briefs.iter().map(|(e, b)| format!("- {}: {b}\n", e.as_str())).collect();
    format!(
        "[요청]\n{request}\n\n[하위 작업]\n{list}\n[쓸 수 있는 AI]\n{engines}\n\
         각 하위 작업을 어느 AI가 어떤 노력 단계로 맡을지 로드맵을 정하세요. 잘 해낼 수 있는 AI 중 비용과 시간이 가장 적은 쪽을 고르고, 설계가 필요하거나 여러 파일이 얽힌 어려운 작업에만 강한 AI와 deep을 쓰세요.\n\
         파일을 열거나 고치지 말고 도구도 쓰지 말고, 설명 없이 아래 JSON 하나만 답하세요.\n\
         {{\"roadmap\":[{{\"part\":1,\"engine\":\"AI id\",\"tier\":\"standard\"}}]}}\n\
         - part는 하위 작업 번호, engine은 위 목록의 id, tier는 standard(보통) 또는 deep(깊게)입니다.",
        request = cut(request, REQUEST_CHARS),
    )
}

/// 첫 { 부터 마지막 } 까지(앞뒤 글이나 코드 펜스가 붙어 있어도 읽는다)
fn json_object(text: &str) -> Option<&str> {
    let (a, b) = (text.find('{')?, text.rfind('}')?);
    (b > a).then(|| &text[a..=b])
}

/// OpenCode의 답에서 하위 작업 n개의 배정을 읽는다. 읽은 칸이 하나도 없으면 None.
pub fn parse_assign(text: &str, n: usize, engines: &[EngineId]) -> Option<Vec<Pick>> {
    #[derive(Deserialize)]
    struct Item {
        part: usize,
        #[serde(default)]
        engine: Option<String>,
        #[serde(default)]
        tier: Option<String>,
    }
    #[derive(Deserialize)]
    struct Body {
        roadmap: Vec<Item>,
    }
    let body: Body = serde_json::from_str(json_object(text)?).ok()?;
    let mut picks = vec![Pick::default(); n];
    for it in body.roadmap {
        let Some(slot) = it.part.checked_sub(1).and_then(|i| picks.get_mut(i)) else { continue };
        slot.engine = it.engine.as_deref().and_then(|e| engine_in(e, engines));
        slot.tier = it.tier.as_deref().and_then(tier);
    }
    picks.iter().any(|p| p.engine.is_some() || p.tier.is_some()).then_some(picks)
}

/// 통합자에게 알리는 다음 단계 덧붙이기 방법. 남은 단계가 없으면 빈 글.
pub fn next_system(stages_left: usize, max_parts: usize, engines: &[EngineId]) -> String {
    if stages_left == 0 || max_parts == 0 {
        return String::new();
    }
    let ids = engines.iter().map(|e| e.as_str()).collect::<Vec<_>>().join(", ");
    format!(
        "\n[로드맵 다음 단계] 통합하며 보니 남은 일이 크고 서로 독립된 부분으로 나눌 수 있으면, 혼자 다 하지 말고 로드맵에 다음 단계를 덧붙이세요. 답의 맨 끝에 아래 블록을 쓰고 차례를 끝내면 Elysium이 워커들에게 동시에 맡기고, 바뀐 파일을 합친 뒤 결과를 당신에게 돌려줍니다.\n\
         ```{NEXT_FENCE}\n{{\"next\":[{{\"title\":\"짧은 이름\",\"task\":\"이 워커가 할 일(구체적으로)\",\"files\":[\"맡는 파일·폴더\"],\"engine\":\"AI id(선택)\"}}]}}\n```\n\
         - 하위 작업은 최대 {max_parts}개이고, 다음 단계는 {stages_left}번 더 덧붙일 수 있습니다. 두 하위 작업이 같은 파일을 고치지 않게 나누세요.\n\
         - engine은 고르지 않아도 됩니다({ids}).\n\
         - 작은 수정은 직접 하세요. 남은 일이 없으면 블록을 쓰지 말고 최종 결과 요약을 쓰세요. 블록을 쓸 때는 지금까지 한 일만 짧게 적으세요.\n"
    )
}

/// 통합자가 덧붙인 다음 단계의 하위 작업 하나
#[derive(Debug, Clone, PartialEq)]
pub struct NextPart {
    pub part: Part,
    pub engine: Option<EngineId>,
}

/// 통합자의 답에서 다음 단계(마지막 블록)를 읽고, 블록을 뺀 본문과 함께 돌려준다. 읽을 수 없는 블록은 없는 것으로 본다.
pub fn parse_next(text: &str, max_parts: usize, engines: &[EngineId]) -> (Vec<NextPart>, String) {
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        title: String,
        #[serde(default)]
        task: String,
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        engine: Option<String>,
    }
    #[derive(Deserialize)]
    struct Body {
        next: Vec<Item>,
    }
    let (bodies, _, clean) = crate::lead::take_blocks(text, NEXT_FENCE);
    let Some(body) = bodies.iter().rev().find_map(|b| serde_json::from_str::<Body>(b).ok()) else { return (vec![], clean) };
    let next = body
        .next
        .into_iter()
        .filter(|it| !it.task.trim().is_empty())
        .take(max_parts)
        .enumerate()
        .map(|(i, it)| NextPart {
            engine: it.engine.as_deref().and_then(|e| engine_in(e, engines)),
            part: Part { title: if it.title.trim().is_empty() { format!("#{}", i + 1) } else { it.title.trim().to_string() }, task: it.task, files: it.files, members: 1, org: vec![] },
        })
        .collect();
    (next, clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use EngineId::*;

    fn part(title: &str) -> Part {
        Part { title: title.into(), task: format!("{title} 만들기"), files: vec![], members: 1, org: vec![] }
    }

    #[test]
    fn assign_prompt_lists_parts_and_engine_ids() {
        let p = assign_prompt("앱", &[part("화면"), part("서버")], &[(Claude, "강함"), (OpenCode, "무료")]);
        assert!(p.contains("1. 화면") && p.contains("2. 서버") && p.contains("- claude: 강함") && p.contains("- opencode: 무료") && p.contains("\"roadmap\""), "{p}");
    }

    #[test]
    fn assign_reads_fenced_json_and_drops_unknown_values() {
        let t = "로드맵입니다.\n```json\n{\"roadmap\":[{\"part\":2,\"engine\":\"codex\",\"tier\":\"DEEP\"},{\"part\":1,\"engine\":\"nvidia\",\"tier\":\"light\"},{\"part\":9,\"engine\":\"claude\"}]}\n```";
        let picks = parse_assign(t, 2, &[Claude, Codex]).unwrap();
        assert_eq!(picks[0], Pick::default(), "목록에 없는 AI와 가벼운 단계는 버린다");
        assert_eq!(picks[1], Pick { engine: Some(Codex), tier: Some("deep") });
        assert_eq!(parse_assign("모르겠습니다", 2, &[Claude]), None);
        assert_eq!(parse_assign("{\"roadmap\":[{\"part\":1,\"engine\":\"x\"}]}", 1, &[Claude]), None, "읽은 칸이 없으면 None");
    }

    #[test]
    fn next_system_is_empty_when_no_stage_is_left() {
        assert!(next_system(0, 4, &[Claude]).is_empty());
        let s = next_system(2, 4, &[Claude, Codex]);
        assert!(s.contains("```elysium-roadmap") && s.contains("최대 4개") && s.contains("2번") && s.contains("claude, codex"), "{s}");
    }

    #[test]
    fn next_stage_is_read_capped_and_removed_from_the_answer() {
        let t = "통합했습니다.\n```elysium-roadmap\n{\"next\":[{\"title\":\"테스트\",\"task\":\"테스트 추가\",\"files\":[\"tests/\"],\"engine\":\"codex\"},{\"task\":\"  \"},{\"task\":\"문서\",\"engine\":\"nvidia\"},{\"title\":\"c\",\"task\":\"셋\"}]}\n```\n";
        let (next, clean) = parse_next(t, 2, &[Claude, Codex]);
        assert_eq!(clean.trim(), "통합했습니다.");
        assert_eq!(next.len(), 2, "빈 작업은 빼고 최대 수만 받는다");
        assert_eq!((next[0].part.title.as_str(), next[0].engine, next[0].part.files.clone()), ("테스트", Some(Codex), vec!["tests/".to_string()]));
        assert_eq!((next[1].part.title.as_str(), next[1].engine, next[1].part.members), ("#2", None, 1));
        let (none, same) = parse_next("끝났습니다.", 4, &[Claude]);
        assert!(none.is_empty() && same == "끝났습니다.");
        let (bad, clean) = parse_next("요약\n```elysium-roadmap\n{깨짐}\n```", 4, &[Claude]);
        assert!(bad.is_empty() && clean.trim() == "요약", "읽을 수 없는 블록도 본문에서 뺀다");
    }
}
