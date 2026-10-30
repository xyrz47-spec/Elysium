//! 요청 글에서 사용자가 바라는 모델과 동시에 돌릴 에이전트 수를 읽는다(순수 함수: 테스트 대상).
//!
//! 오탐을 줄이는 원칙
//! - 모델 이름 뒤에 "로·으로·모델·한테·를 써서" 같은 지시 표지가 있거나, 앞에 use·with가 있을 때만 요청으로 본다.
//! - 시·화학 용어와 겹치는 이름(haiku·sonnet·flash·argon 한글 표기 등)은 "모델" 표지나 회사 이름이 붙을 때만 받는다.

use crate::engines::EngineId;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelPick {
    pub engine: EngineId,
    /// None이면 그 엔진의 기본 모델
    pub model: Option<String>,
    /// 화면에 보일 이름(예: "Claude Opus", "gpt-5.1")
    pub name: String,
    /// 같은 모델을 쓸 수 있는 모든 엔진(engine이 첫째). 입력창에서 고른 AI가 여기 있으면 그 AI로 쓰고, 실패하면 다음 엔진으로 넘어간다.
    pub alts: Vec<(EngineId, Option<String>)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Wanted {
    pub picks: Vec<ModelPick>,
    /// 요청했지만 쓸 수 있는 엔진이 없는 모델 이름
    pub missing: Vec<String>,
}

struct Family {
    name: &'static str,
    /// (별칭, 강한 이름인지). 약한 이름은 "모델" 표지가 있을 때만 받는다.
    aliases: &'static [(&'static str, bool)],
    /// 먼저 시도할 엔진과 그 엔진에 줄 모델(""면 기본 모델)
    targets: &'static [(EngineId, &'static str)],
}

use EngineId::*;

/// 같은 위치에서는 긴 별칭이 이긴다. 순서는 상관없다.
const FAMILIES: &[Family] = &[
    Family { name: "Claude Opus", aliases: &[("claude opus", true), ("claude-opus", true), ("opus", true), ("오퍼스", true)], targets: &[(Claude, "opus"), (ClaudeApi, "claude-opus-5-5"), (Antigravity, "claude-opus-5-5")] },
    Family { name: "Claude Sonnet", aliases: &[("claude sonnet", true), ("claude-sonnet", true), ("sonnet", false), ("소네트", false), ("소넷", false)], targets: &[(Claude, "sonnet"), (ClaudeApi, "claude-sonnet-5-5"), (Antigravity, "claude-sonnet-5-5")] },
    Family { name: "Claude Haiku", aliases: &[("claude haiku", true), ("claude-haiku", true), ("haiku", false), ("하이쿠", false)], targets: &[(Claude, "haiku"), (ClaudeApi, "claude-haiku-5-5")] },
    Family { name: "Claude Fable", aliases: &[("claude fable", true), ("claude-fable", true), ("fable", false), ("페이블", false)], targets: &[(Claude, "claude-fable-5-1"), (ClaudeApi, "claude-fable-5-1")] },
    Family { name: "Claude Code", aliases: &[("claude code", true), ("클로드 코드", true), ("클로드코드", true)], targets: &[(Claude, "")] },
    Family { name: "Claude", aliases: &[("claude", true), ("클로드", true)], targets: &[(Claude, ""), (ClaudeApi, ""), (Antigravity, "claude-sonnet-5-5")] },
    Family { name: "Gemini 4 Argon", aliases: &[("gemini 4 argon", true), ("gemini-4-argon", true), ("4 argon", true), ("argon", true)], targets: &[(Antigravity, "gemini-4-argon"), (GeminiApi, "gemini-4-argon")] },
    Family { name: "Gemini 3.8 Flash", aliases: &[("gemini 3.8 flash", true), ("gemini-3.8-flash", true), ("3.8 flash", true), ("gemini flash", true), ("flash", false), ("플래시", false)], targets: &[(Antigravity, "gemini-3.8-flash"), (GeminiApi, "gemini-3.8-flash")] },
    Family { name: "Gemini", aliases: &[("gemini", true), ("제미나이", true), ("제미니", true)], targets: &[(Antigravity, ""), (GeminiApi, "")] },
    Family { name: "Antigravity", aliases: &[("antigravity", true), ("안티그래비티", true), ("agy", true)], targets: &[(Antigravity, "")] },
    Family { name: "GPT-OSS", aliases: &[("gpt-oss", true), ("gpt oss", true)], targets: &[(Antigravity, "gpt-oss-120b"), (Nvidia, "openai/gpt-oss-120b")] },
    Family { name: "GPT", aliases: &[("gpt", true), ("지피티", true)], targets: &[(Codex, ""), (OpenAi, "")] },
    Family { name: "Codex", aliases: &[("codex", true), ("코덱스", true)], targets: &[(Codex, "")] },
    Family { name: "Nemotron", aliases: &[("nemotron", true), ("네모트론", true), ("nvidia", true), ("엔비디아", true)], targets: &[(Nvidia, "")] },
    Family { name: "DeepSeek", aliases: &[("deepseek", true), ("딥시크", true)], targets: &[(Nvidia, "deepseek-ai/deepseek-v4.1-flash")] },
];

/// 이 이름 뒤에 오면 "이 모델로 해 달라"는 뜻으로 본다.
const MARKERS: &[&str] = &[
    "으로", "로", "모델", "한테", "에게", "를 써서", "을 써서", "를 사용", "을 사용", "를 이용", "을 이용", "써서", "사용해", "이용해", "가 답", "이 답", "가 맡", "이 맡", "가 해", "이 해",
];
/// 여러 모델을 나열할 때의 이음말("opus랑 gpt-5로"): 다른 모델에 지시 표지가 있을 때만 받는다.
const JOINERS: &[&str] = &["랑", "이랑", "하고", "와 ", "과 ", ",", "·", "/", "and "];
const EN_BEFORE: &[&str] = &["use ", "using ", "with ", "via "];

/// 지시 바로 뒤에 붙는 "~로 해 달라" 꼴의 말. 이름과 함께 요청 글에서 뺀다.
const USE_TAILS: &[&str] = &[
    "을 사용하여", "를 사용하여", "을 사용해서", "를 사용해서", "을 사용해", "를 사용해", "을 사용하고", "를 사용하고",
    "을 이용하여", "를 이용하여", "을 이용해서", "를 이용해서", "을 이용해", "를 이용해", "을 써서", "를 써서",
    "을 돌려서", "를 돌려서", "을 돌려", "를 돌려", "을 띄워서", "를 띄워서", "으로", "로", "한테", "에게",
];
/// 이름에 붙은 조사·이음말("opus랑", "codex가"). 이름이 지시로 확인됐을 때만 함께 뺀다.
const LINK_TAILS: &[&str] = &["이랑", "랑", "하고", "와", "과", "가", "이", "을", "를", ",", "·", "/"];
/// 띄어 쓴 동사("에이전트 4개 돌려서")
const VERBS: &[&str] = &["사용하여", "사용해서", "사용해", "이용하여", "이용해서", "이용해", "써서", "돌려서", "띄워서"];

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

#[derive(Debug)]
struct Hit {
    family: usize,
    /// 사용자가 적은 정확한 모델 이름(예: claude-opus-5-5, gpt-5.1-codex)
    exact: Option<String>,
    strong_marker: bool,
    joiner: bool,
    /// 지시가 시작하는 곳(앞의 use·with 포함)과 모델 이름이 끝나는 곳. 바이트 위치(ASCII 소문자 글 = 원래 글).
    start: usize,
    end: usize,
}

/// 앞에 "use ·with " 같은 영어 지시어가 있으면 그 시작까지 넓힌다.
fn with_en_before(low: &str, s: usize) -> usize {
    EN_BEFORE.iter().find(|w| low[..s].ends_with(**w)).map_or(s, |w| s - w.len())
}

/// e 뒤에 붙은 지시 꼬리의 끝. strict면 "~로·~를 사용해" 꼴만 받는다(조사 하나만 붙은 일반 낱말은 그대로 둔다).
fn tail_end(low: &str, e: usize, strict: bool) -> usize {
    let t = &low[e..];
    let longest = |list: &[&str], t: &str| list.iter().filter(|x| t.starts_with(**x)).map(|x| x.len()).max();
    let near = if strict { longest(USE_TAILS, t) } else { longest(USE_TAILS, t).max(longest(LINK_TAILS, t)) };
    if let Some(n) = near {
        return e + n;
    }
    let t2 = t.trim_start_matches(' ');
    longest(VERBS, t2).map_or(e, |n| e + (t.len() - t2.len()) + n)
}

/// 요청으로 받아들이는 모델 언급: 지시 표지가 있거나, 지시된 다른 모델과 이음말로 이어진 강한 이름
fn accepted(hits: &[Hit]) -> impl Iterator<Item = &Hit> + '_ {
    let any_strong = hits.iter().any(|h| h.strong_marker);
    hits.iter().filter(move |h| h.strong_marker || (any_strong && h.joiner && FAMILIES[h.family].aliases.iter().any(|a| a.1)))
}

fn scan(text: &str) -> Vec<Hit> {
    // ASCII만 소문자로 바꿔 바이트 위치가 원래 글과 같게 둔다(별칭은 모두 ASCII 소문자이거나 한글).
    let low = text.to_ascii_lowercase();
    let mut cands: Vec<(usize, usize, usize, bool)> = vec![];
    for (fi, f) in FAMILIES.iter().enumerate() {
        for &(alias, strong) in f.aliases {
            let mut from = 0;
            while let Some(i) = low[from..].find(alias) {
                let s = from + i;
                let e = s + alias.len();
                from = e;
                let before_ok = low[..s].chars().next_back().is_none_or(|c| !is_word(c));
                if before_ok {
                    cands.push((s, e, fi, strong));
                }
            }
        }
    }
    // 앞선 것, 같은 위치면 긴 것이 이긴다.
    cands.sort_by(|a, b| a.0.cmp(&b.0).then((b.1 - b.0).cmp(&(a.1 - a.0))));
    let mut hits: Vec<Hit> = vec![];
    let mut taken = 0;
    for (s, mut e, fi, strong) in cands {
        if s < taken {
            continue;
        }
        // 이어진 버전·이름("opus-5-5", "gpt-5.1-codex", "opus 5.5")까지 한 토큰으로 본다.
        let rest = &low[e..];
        let tail: usize = rest.chars().take_while(|c| is_word(*c) || *c == '.').map(char::len_utf8).sum();
        e += tail;
        let rest = &low[e..];
        if rest.starts_with(' ') && rest[1..].starts_with(|c: char| c.is_ascii_digit()) {
            e += 1 + rest[1..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').map(char::len_utf8).sum::<usize>();
        }
        let token = low[s..e].trim_end_matches(['.', '-']).to_string();
        e = s + token.len();
        let exact = (token.contains('-') && ["claude-", "gpt-", "gemini-", "deepseek", "o3", "o4"].iter().any(|p| token.starts_with(p))).then(|| token.clone());
        let after = low[e..].trim_start();
        let marker = MARKERS.iter().any(|m| after.starts_with(m)) || after.starts_with("model");
        let model_word = after.starts_with("모델") || after.starts_with("model");
        let start = with_en_before(&low, s);
        let en_before = start < s;
        let strong_marker = if strong { marker || en_before } else { model_word };
        let joiner = JOINERS.iter().any(|j| after.starts_with(j) || low[e..].starts_with(j));
        taken = e;
        hits.push(Hit { family: fi, exact, strong_marker, joiner, start, end: e });
    }
    hits
}

/// 요청 글에서 고른 모델들. installed: 쓸 수 있는 엔진, work: 작업 모드면 CLI를 API보다 먼저 고른다.
pub fn requested_models(text: &str, installed: &[EngineId], work: bool) -> Wanted {
    let hits = scan(text);
    let mut w = Wanted::default();
    for h in accepted(&hits) {
        let f = &FAMILIES[h.family];
        let mut alts: Vec<(EngineId, Option<String>)> = f
            .targets
            .iter()
            .filter(|(e, _)| installed.contains(e))
            .map(|&(e, m)| (e, h.exact.clone().or_else(|| (!m.is_empty()).then(|| m.to_string()))))
            .collect();
        // 작업 모드는 API도 쓸 수 있지만, 같은 모델을 CLI로 쓸 수 있으면 CLI가 먼저다.
        if work {
            alts.sort_by_key(|(e, _)| e.is_api());
        }
        match alts.first().cloned() {
            Some((engine, model)) => {
                let name = h.exact.clone().unwrap_or_else(|| f.name.to_string());
                let p = ModelPick { engine, model, name, alts };
                if !w.picks.contains(&p) {
                    w.picks.push(p);
                }
            }
            None => {
                let n = h.exact.clone().unwrap_or_else(|| f.name.to_string());
                if !w.missing.contains(&n) {
                    w.missing.push(n);
                }
            }
        }
    }
    w
}

const AGENT_WORDS: &[&str] = &["서브에이전트", "에이전트", "agents", "agent"];
const MANY_WORDS: &[&str] = &[
    "여러 에이전트", "에이전트 여러", "에이전트들", "멀티 에이전트", "멀티에이전트", "multi-agent", "multi agent", "multiple agents", "parallel agents", "in parallel", "병렬로", "병렬 에이전트", "동시에 여러",
];
const NUM_WORDS: &[(&str, usize)] = &[("다섯", 5), ("두", 2), ("둘", 2), ("세", 3), ("셋", 3), ("네", 4), ("넷", 4), ("two", 2), ("three", 3), ("four", 4), ("five", 5)];
pub const MAX_AGENTS: usize = 5;

/// 요청 글이 에이전트 여러 개를 동시에 돌려 달라고 하면 그 수(2~5). 숫자 없이 "여러·병렬"이면 3.
pub fn requested_agents(text: &str) -> Option<usize> {
    let low = text.to_ascii_lowercase();
    if let Some((n, _, _)) = agent_count(&low) {
        return (n >= 2).then_some(n.min(MAX_AGENTS));
    }
    MANY_WORDS.iter().any(|w| low.contains(w)).then_some(3)
}

/// 에이전트 키워드에 바로 붙은 수와 그 지시의 범위(수, 시작, 끝). "3개·3명·세 개·3 agents·에이전트 3"
fn agent_count(low: &str) -> Option<(usize, usize, usize)> {
    for kw in AGENT_WORDS {
        let mut from = 0;
        while let Some(i) = low[from..].find(kw) {
            let at = from + i;
            let ke = at + kw.len();
            from = ke;
            if let Some((n, s)) = count_before(&low[..at]) {
                return Some((n, s, ke));
            }
            if let Some((n, e)) = count_after(&low[ke..]) {
                return Some((n, at, ke + e));
            }
        }
    }
    None
}

/// 키워드 바로 앞의 수(“3개의 ”, “세 개의 ”, “2 ”)와 그 시작 위치
fn count_before(pre: &str) -> Option<(usize, usize)> {
    let w = pre.trim_end();
    let w = w.strip_suffix("의").unwrap_or(w).trim_end();
    let w = w.trim_end_matches(['개', '명', '대']).trim_end();
    let digits = w.len() - w.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        let s = w.len() - digits;
        return w[s..].parse().ok().map(|n| (n, s));
    }
    let s = w.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
    let last = &w[s..];
    NUM_WORDS.iter().find(|(word, _)| last == *word).map(|(_, n)| (*n, s))
}

/// 키워드 바로 뒤의 수(“ 3개”, “를 3개”, “ two”)와 그 끝 위치(post 기준)
fn count_after(post: &str) -> Option<(usize, usize)> {
    let t = post.trim_start();
    let t = t.trim_start_matches(['를', '을', '가', '이', ':']).trim_start();
    let off = post.len() - t.len();
    // 수 뒤의 "개·명·대"까지
    let unit = |rest: &str| -> usize {
        let r = rest.trim_start_matches(' ');
        ['개', '명', '대'].iter().find(|u| r.starts_with(**u)).map_or(0, |u| rest.len() - r.len() + u.len_utf8())
    };
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        let n = t[..digits].parse().ok()?;
        return Some((n, off + digits + unit(&t[digits..])));
    }
    if let Some((word, n)) = NUM_WORDS.iter().find(|(word, _)| t.starts_with(word) && t[word.len()..].trim_start().starts_with(['개', '명', '대'])) {
        return Some((*n, off + word.len() + unit(&t[word.len()..])));
    }
    NUM_WORDS
        .iter()
        .find(|(word, _)| (t == *word || t.starts_with(&format!("{word} "))) && (word.is_ascii() || ["둘", "셋", "넷", "다섯"].contains(word)))
        .map(|(word, n)| (*n, off + word.len()))
}

/// 요청 글에서 모델·에이전트 수 지시("opus를 사용하여", "에이전트 3개로")를 뺀 글. 이 지시는 엔진을 고르는 데만 쓰고 AI에게는 할 일만 넘긴다.
/// 빼고 나면 남는 것이 거의 없으면(지시만 적은 글) 원래 글을 그대로 둔다.
pub fn strip_directives(text: &str) -> String {
    let low = text.to_ascii_lowercase();
    let hits = scan(text);
    let mut cuts: Vec<(usize, usize)> = accepted(&hits)
        .map(|h| {
            // "sonnet 모델로", "opus model" 처럼 붙은 "모델"까지
            let mut e = h.end;
            let rest = &low[e..];
            let t = rest.trim_start();
            if let Some(m) = ["모델", "models", "model"].iter().find(|m| t.starts_with(**m)) {
                e += rest.len() - t.len() + m.len();
            }
            (h.start, tail_end(&low, e, false))
        })
        .collect();
    if let Some((_, s, e)) = agent_count(&low).filter(|a| a.0 >= 2) {
        cuts.push((with_en_before(&low, s), tail_end(&low, e, false)));
    }
    // "여러 에이전트로"처럼 수 없는 지시는 "~로·~를 사용해"가 붙을 때만 뺀다("멀티 에이전트 시스템을 설계해"는 주제라 둔다).
    for w in MANY_WORDS.iter().filter(|w| w.contains("에이전트") || w.contains("agent")) {
        let mut from = 0;
        while let Some(i) = low[from..].find(w) {
            let s = from + i;
            from = s + w.len();
            let end = tail_end(&low, from, true);
            if end > from {
                cuts.push((s, end));
            }
        }
    }
    if cuts.is_empty() {
        return text.trim().to_string();
    }
    cuts.sort();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (s, e) in cuts {
        if e <= last {
            continue;
        }
        out.push_str(&text[last..s.max(last)]);
        last = e;
        // 자른 자리에 남는 빈칸·쉼표를 정리한다(줄바꿈과 코드 블록은 그대로 둔다).
        let rest = &text[last..];
        if out.is_empty() || out.ends_with([' ', '\t', '\n']) {
            last += rest.len() - rest.trim_start_matches([' ', '\t', ',']).len();
        }
    }
    out.push_str(&text[last..]);
    let out = out.trim();
    if out.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
        text.trim().to_string()
    } else {
        out.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[EngineId] = &[Claude, Codex, Antigravity, ClaudeApi, OpenAi, GeminiApi, Nvidia];

    fn one(text: &str, installed: &[EngineId]) -> Option<(EngineId, Option<String>)> {
        let w = requested_models(text, installed, false);
        w.picks.first().map(|p| (p.engine, p.model.clone()))
    }

    #[test]
    fn reads_requested_model() {
        assert_eq!(one("opus로 이 코드 리뷰해 줘", ALL), Some((Claude, Some("opus".into()))));
        assert_eq!(one("Claude Opus 5.5로 답해", ALL), Some((Claude, Some("opus".into()))));
        assert_eq!(one("claude-opus-5-5 모델로 해 줘", ALL), Some((Claude, Some("claude-opus-5-5".into()))), "정확한 이름은 그대로");
        assert_eq!(one("gpt-5.1-codex로 고쳐 줘", ALL), Some((Codex, Some("gpt-5.1-codex".into()))));
        assert_eq!(one("gemini 4 argon으로 정리해", ALL), Some((Antigravity, Some("gemini-4-argon".into()))));
        assert_eq!(one("제미나이로 번역해 줘", ALL), Some((Antigravity, None)));
        assert_eq!(one("use opus for this", ALL), Some((Claude, Some("opus".into()))));
        assert_eq!(one("sonnet 모델로 요약", ALL), Some((Claude, Some("sonnet".into()))));
        // 설치된 엔진으로 대신한다: Claude Code가 없으면 Claude API, 그것도 없으면 Antigravity의 Claude 모델
        assert_eq!(one("opus로 답해", &[ClaudeApi, Antigravity]), Some((ClaudeApi, Some("claude-opus-5-5".into()))));
        assert_eq!(one("opus로 답해", &[Antigravity]), Some((Antigravity, Some("claude-opus-5-5".into()))));
        // 작업 모드에서는 CLI를 먼저, CLI가 없으면 API로
        assert_eq!(requested_models("opus로 고쳐", &[ClaudeApi, Antigravity], true).picks[0].engine, Antigravity);
        assert_eq!(requested_models("opus로 고쳐", &[ClaudeApi], true).picks[0].engine, ClaudeApi);
        // 쓸 수 있는 엔진이 없으면 따로 알린다
        let w = requested_models("codex로 해 줘", &[Claude], false);
        assert!(w.picks.is_empty() && w.missing == vec!["Codex".to_string()]);
    }

    #[test]
    fn ignores_words_that_are_not_model_requests() {
        assert_eq!(one("haiku로 시를 써 줘", ALL), None, "하이쿠 형식의 시");
        assert_eq!(one("소네트 한 편 써 줘", ALL), None);
        assert_eq!(one("플래시로 사진 찍는 법", ALL), None);
        assert_eq!(one("Claude Code의 단점은?", ALL), None, "지시 표지가 없으면 언급일 뿐");
        assert_eq!(one("gemini와 gpt를 비교해 줘", ALL), None, "비교 대상");
        assert_eq!(one("magnum opus의 뜻", ALL), None);
        assert_eq!(one("오늘 날씨", ALL), None);
    }

    #[test]
    fn reads_several_models_for_parallel_agents() {
        let w = requested_models("opus랑 gpt-5로 동시에 풀어 줘", ALL, false);
        assert_eq!(w.picks.iter().map(|p| (p.engine, p.model.clone())).collect::<Vec<_>>(), vec![(Claude, Some("opus".into())), (Codex, Some("gpt-5".into()))]);
        let w = requested_models("claude, gemini, codex로 각각 답해 줘", ALL, false);
        assert_eq!(w.picks.iter().map(|p| p.engine).collect::<Vec<_>>(), vec![Claude, Antigravity, Codex]);
    }

    #[test]
    fn reads_requested_agent_count() {
        assert_eq!(requested_agents("에이전트 3개로 조사해 줘"), Some(3));
        assert_eq!(requested_agents("3개의 에이전트가 동시에"), Some(3));
        assert_eq!(requested_agents("에이전트를 4개 돌려서"), Some(4));
        assert_eq!(requested_agents("세 개의 에이전트로"), Some(3));
        assert_eq!(requested_agents("use 2 agents"), Some(2));
        assert_eq!(requested_agents("에이전트 9개"), Some(MAX_AGENTS), "최대 5개");
        assert_eq!(requested_agents("여러 에이전트로 동시에 분석해"), Some(3));
        assert_eq!(requested_agents("병렬로 처리해 줘"), Some(3));
        assert_eq!(requested_agents("에이전트 1개만"), None);
        assert_eq!(requested_agents("에이전트가 뭐야?"), None);
        assert_eq!(requested_agents("오늘 3개 할 일"), None);
    }

    #[test]
    fn strips_model_and_agent_directives_from_the_prompt() {
        assert_eq!(strip_directives("opus를 사용하여 로그인 페이지를 만들어 줘"), "로그인 페이지를 만들어 줘");
        assert_eq!(strip_directives("Claude Code를 사용하여 작업을 해 줘"), "작업을 해 줘");
        assert_eq!(strip_directives("이 함수를 opus로 리팩터링해 줘"), "이 함수를 리팩터링해 줘");
        assert_eq!(strip_directives("sonnet 모델로 이 글을 요약해 줘"), "이 글을 요약해 줘");
        assert_eq!(strip_directives("claude, gemini, codex로 각각 답해 줘"), "각각 답해 줘");
        assert_eq!(strip_directives("gemini 4 argon을 이용해서 번역해 줘"), "번역해 줘");
        assert_eq!(strip_directives("에이전트 3개를 사용하여 시장을 조사해 줘"), "시장을 조사해 줘");
        assert_eq!(strip_directives("3개의 에이전트로 동시에 분석해"), "동시에 분석해");
        assert_eq!(strip_directives("에이전트를 4개 돌려서 비교해 줘"), "비교해 줘");
        assert_eq!(strip_directives("여러 에이전트로 동시에 분석해"), "동시에 분석해");
        // 줄바꿈·코드 블록은 그대로
        assert_eq!(strip_directives("opus로 고쳐 줘\n```\nfn a() {}\n```"), "고쳐 줘\n```\nfn a() {}\n```");
    }

    #[test]
    fn keeps_words_that_are_not_directives() {
        for t in ["haiku로 시를 써 줘", "Claude Code의 단점은?", "멀티 에이전트 시스템을 설계해 줘", "에이전트가 뭐야?", "gemini와 gpt를 비교해 줘"] {
            assert_eq!(strip_directives(t), t);
        }
        // 지시만 있는 글은 지우지 않는다
        assert_eq!(strip_directives("opus를 사용해 줘"), "opus를 사용해 줘");
    }
}
