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
    Family { name: "DeepSeek", aliases: &[("deepseek", true), ("딥시크", true)], targets: &[(Nvidia, "deepseek-ai/deepseek-v4.1-flash"), (OpenCode, "opencode/deepseek-v4.1-flash-free")] },
    Family { name: "OpenCode", aliases: &[("opencode", true), ("open code", true), ("오픈코드", true), ("오픈 코드", true)], targets: &[(OpenCode, "")] },
    Family { name: "GLM", aliases: &[("glm", true)], targets: &[(OpenCode, "opencode/glm-5.3-flash-free")] },
    Family { name: "MiMo", aliases: &[("mimo", true), ("미모", false)], targets: &[(OpenCode, "opencode/mimo-v2.6-flash-free")] },
    Family { name: "Big Pickle", aliases: &[("big pickle", true), ("big-pickle", true), ("빅 피클", true)], targets: &[(OpenCode, "opencode/big-pickle")] },
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
    /// 시·화학 용어와 겹치지 않는 이름인지
    strong: bool,
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
        hits.push(Hit { family: fi, exact, strong_marker, joiner, strong, start, end: e });
    }
    hits
}

/// 요청 글에서 고른 모델들. installed: 쓸 수 있는 엔진, work: 작업 모드면 CLI를 API보다 먼저 고른다.
pub fn requested_models(text: &str, installed: &[EngineId], work: bool) -> Wanted {
    let hits = scan(text);
    let mut w = Wanted::default();
    for h in accepted(&hits) {
        match pick_of(h, installed, work) {
            Ok(p) => {
                if !w.picks.contains(&p) {
                    w.picks.push(p);
                }
            }
            Err(n) => {
                if !w.missing.contains(&n) {
                    w.missing.push(n);
                }
            }
        }
    }
    w
}

/// 이 언급을 맡을 수 있는 엔진들. 쓸 수 있는 엔진이 없으면 Err(모델 이름).
fn pick_of(h: &Hit, installed: &[EngineId], work: bool) -> Result<ModelPick, String> {
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
    let name = h.exact.clone().unwrap_or_else(|| f.name.to_string());
    match alts.first().cloned() {
        Some((engine, model)) => Ok(ModelPick { engine, model, name, alts }),
        None => Err(name),
    }
}

/// 요청 글이 직접 정한 팀: 같은 모델의 워커 여럿과 (적었다면) 총괄 AI.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamAsk {
    pub workers: ModelPick,
    pub effort: Option<&'static str>,
    pub count: usize,
    pub lead: Option<ModelPick>,
}

const WORKER_WORDS: &[&str] = &["서브 에이전트", "서브에이전트", "하위 에이전트", "에이전트", "워커", "sub-agents", "sub-agent", "subagents", "subagent", "agents", "agent", "workers", "worker"];
const LEAD_WORDS: &[&str] = &["총괄", "총관", "관리자", "오케스트레이터", "리드", "감독", "orchestrator", "lead", "manager", "supervisor"];
const EFFORTS: &[&str] = &["xhigh", "max", "high", "medium", "low"];

fn skip_marks(s: &str) -> &str {
    s.trim_start_matches([' ', '*'])
}

/// 모델 이름 뒤의 노력치("(high)", " high")와 그 길이
fn effort_after(rest: &str) -> (Option<&'static str>, usize) {
    let t = skip_marks(rest);
    let inner = t.strip_prefix('(').unwrap_or(t);
    for e in EFFORTS {
        if let Some(after) = inner.strip_prefix(e).filter(|a| !a.starts_with(is_word)) {
            let after = after.strip_prefix(')').unwrap_or(after);
            return (Some(e), rest.len() - after.len());
        }
    }
    (None, 0)
}

/// 수 앞의 빈칸·닫는 괄호·굵게 표시(**)를 걷어 낸다. 곱하기로 쓴 '*' 하나("high * 3")는 남긴다.
fn skip_to_count(s: &str) -> &str {
    let mut t = s;
    loop {
        let n = t.trim_start_matches([' ', ')', '`']);
        let n = n.strip_prefix("**").unwrap_or(n);
        if n.len() == t.len() {
            return t;
        }
        t = n;
    }
}

/// 모델 바로 뒤의 "× 4", "x4", "* 4", "4개"
fn bare_count(after: &str) -> Option<usize> {
    let (marked, t) = match after.strip_prefix('×').or_else(|| after.strip_prefix('x')).or_else(|| after.strip_prefix('*')) {
        Some(t) => (true, t.trim_start_matches(' ')),
        None => (false, after),
    };
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let tail = t[digits..].trim_start_matches(' ');
    (marked || tail.starts_with(['개', '명', '대', '기'])).then(|| t[..digits].parse().ok()).flatten()
}

/// 이 모델 언급이 지시하는 워커 수와 노력치
fn worker_count(low: &str, h: &Hit) -> Option<(usize, Option<&'static str>)> {
    let (effort, used) = effort_after(&low[h.end..]);
    let after = skip_marks(&low[h.end + used..]);
    let n = match WORKER_WORDS.iter().find(|w| after.starts_with(**w)) {
        Some(w) => count_after(&after[w.len()..]).map(|(n, _)| n).or_else(|| count_before(low[..h.start].trim_end_matches(['*', ' '])).map(|(n, _)| n)),
        None => bare_count(skip_to_count(&low[h.end + used..])),
    }?;
    (n >= 2).then_some((n.min(MAX_AGENTS), effort))
}

/// 이름에 버전 숫자가 붙어 있는지("sonnet 5.5", "haiku-5-5")
fn versioned(low: &str, h: &Hit) -> bool {
    low[h.start..h.end].contains(|c: char| c.is_ascii_digit())
}

/// 같은 줄에서 총괄·관리자로 지목된 모델인지
fn names_lead(low: &str, h: &Hit) -> bool {
    let line_start = low[..h.start].rfind('\n').map_or(0, |i| i + 1);
    let before = &low[line_start..h.start];
    let after: String = low[h.end..].lines().next().unwrap_or("").chars().take(12).collect();
    LEAD_WORDS.iter().any(|w| before.contains(w) || after.contains(w))
}

/// "Opus가 총괄하고 Gemini 3.8 Flash (High) 워커 4개를 돌려" 꼴의 지시. 수와 모델이 함께 있을 때만 팀으로 본다.
pub fn requested_team(text: &str, installed: &[EngineId], work: bool) -> Option<TeamAsk> {
    let low = text.to_ascii_lowercase();
    let hits = scan(text);
    // 약한 이름도 버전을 붙여 적었으면("Sonnet 5.5") 모델로 본다.
    let (worker, (count, effort)) = hits.iter().filter(|h| h.strong || versioned(&low, h)).find_map(|h| worker_count(&low, h).map(|c| (h, c)))?;
    let workers = pick_of(worker, installed, work).ok()?;
    let lead = hits.iter().filter(|h| h.family != worker.family && names_lead(&low, h)).find_map(|h| pick_of(h, installed, work).ok());
    Some(TeamAsk { workers, effort, count, lead })
}

/// 요청 글이 팀별로 적은 워커 한 자리
#[derive(Debug, Clone, PartialEq)]
pub struct RosterMember {
    pub pick: ModelPick,
    pub effort: Option<&'static str>,
}

/// 요청 글에 적힌 혼합 팀 구성. 적힌 그대로 돌리며 다른 AI로 채우지 않는다.
#[derive(Debug, Clone, PartialEq)]
pub struct Roster {
    pub teams: Vec<Vec<RosterMember>>,
    pub lead: Option<ModelPick>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RosterError {
    /// 적힌 모델을 돌릴 엔진이 없다
    Missing(String),
    /// 적힌 자리 수가 MAX_AGENTS를 넘는다
    TooMany(usize),
}

#[derive(Clone)]
struct RosterEntry {
    family: usize,
    pick: Result<ModelPick, String>,
    effort: Option<&'static str>,
    count: usize,
}

/// 팀 줄에서 모델 이름 말고 이만큼의 글자가 더 있으면 구성표가 아니라 할 일을 적은 줄로 본다.
const ROSTER_LINE_MAX_WORDS: usize = 6;
const EACH_TEAM_WORDS: &[&str] = &["각 팀", "각팀", "각 그룹", "한 팀", "한 그룹", "팀당", "팀마다", "그룹마다", "each team", "per team", "each group", "per group"];
/// "각각 4개씩" 뒤에 이 말이 있으면 모델 하나씩을 한 팀으로 묶는다.
const PAIR_WORDS: &[&str] = &["그룹", "팀", "페어", "짝", "묶", "group", "team", "pair"];

/// 이 언급이 워커가 아니라 총괄을 가리키는지("총관은 Opus", "Opus가 총괄")
fn lead_mention(low: &str, h: &Hit, after: &str) -> bool {
    let before = &low[low[..h.start].rfind('\n').map_or(0, |i| i + 1)..h.start];
    let named_before = LEAD_WORDS.iter().any(|w| before.rfind(w).is_some_and(|i| before.len() - (i + w.len()) <= 12 && !before[i..].contains([',', '.'])));
    let t = after.trim_start_matches(['가', '이', '는', '은', '를', '을', ' ', '*', ')']);
    named_before || LEAD_WORDS.iter().any(|w| t.starts_with(w))
}

/// 모델 이름·수 말고 남은 글자 수. 이음말과 조사 정도면 구성표로 본다.
fn word_chars(s: &str) -> usize {
    s.chars().filter(|c| c.is_alphabetic() && !['기', '개', '명', '대', 'x', '×'].contains(c)).count()
}

/// 이 글의 모델 언급들과 그 밖에 남은 글자 수. "Claude Code (Haiku 5.5)"는 괄호 안의 모델만 센다.
fn roster_entries(text: &str, installed: &[EngineId], work: bool) -> (Vec<RosterEntry>, usize) {
    let low = text.to_ascii_lowercase();
    let hits = scan(text);
    let (mut out, mut rest, mut pos) = (vec![], 0, 0);
    for (i, h) in hits.iter().enumerate() {
        rest += word_chars(&low[pos..h.start.max(pos)]);
        pos = h.end;
        let names_inner = hits.get(i + 1).is_some_and(|n| n.start >= h.end && ["", "(", "-", ":"].contains(&low[h.end..n.start].trim_matches([' ', '`', '*'])));
        if names_inner {
            continue;
        }
        let (effort, used) = effort_after(&low[h.end..]);
        pos = h.end + used;
        if lead_mention(&low, h, &low[pos..]) {
            continue;
        }
        out.push(RosterEntry { family: h.family, pick: pick_of(h, installed, work), effort, count: bare_count(skip_to_count(&low[pos..])).unwrap_or(1).max(1) });
    }
    (out, rest + word_chars(&low[pos.min(low.len())..]))
}

const TEAM_NOUNS: &[&str] = &["team", "group", "팀", "그룹"];
/// 이 말이 있는 팀은 쓰라는 것이 아니라 허락만 한 것으로 본다.
const OPTIONAL_WORDS: &[&str] = &["보조", "선택적", "필요한 경우", "필요시", "필요 시", "필요에 따라", "있어도", "추가 호출", "optional", "if needed"];
/// "구성: A, B"처럼 콜론 뒤에 모델을 적는 말
const COMPOSITION_WORDS: &[&str] = &["구성", "모델", "멤버", "composition", "members", "model"];
const LIST_MARKS: [char; 8] = [' ', '\t', '-', '*', '|', '>', '`', '['];

fn leading_number(s: &str) -> (Option<usize>, &str) {
    let n = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (s[..n].parse().ok(), &s[n..])
}

fn strip_team_noun(s: &str) -> Option<&str> {
    TEAM_NOUNS.iter().find_map(|w| s.strip_prefix(w))
}

/// " ~ 4팀", "~team 4"처럼 이름표를 범위로 잇는 꼬리의 끝 번호와 그 뒤의 글
fn range_end(s: &str) -> Option<(usize, &str)> {
    let t = s.trim_start().strip_prefix(['~', '–', '—'])?.trim_start();
    let t = strip_team_noun(t).unwrap_or(t).trim_start();
    let (n, rest) = leading_number(t);
    Some((n?, strip_team_noun(rest.trim_start()).unwrap_or(rest)))
}

/// "1팀", "2 그룹", "Team 3", "1~4팀", "Team 1 ~ Team 4"로 시작하는 줄이면 그 뒤의 글과 이 이름표가 뜻하는 팀 번호 범위
fn team_label(line: &str) -> Option<(&str, usize, usize)> {
    let t = line.trim_start_matches(LIST_MARKS).trim_start_matches(['#', ' ']).trim_start_matches(LIST_MARKS);
    let (first, r) = match leading_number(t) {
        (Some(n), rest) => {
            let rest = rest.trim_start();
            if let Some((end, after)) = range_end(rest) {
                return Some((after, n, end.max(n)));
            }
            let after = strip_team_noun(rest)?;
            if after.starts_with(is_word) {
                return None;
            }
            (n, after)
        }
        (None, _) => {
            let (n, rest) = leading_number(strip_team_noun(t)?.trim_start());
            (n?, rest)
        }
    };
    Some(match range_end(r) {
        Some((end, after)) => (after, first, end.max(first)),
        None => (r, first, first),
    })
}

fn is_optional(s: &str) -> bool {
    OPTIONAL_WORDS.iter().any(|w| s.contains(w))
}

/// 팀 이름표 아래 줄의 모델들. "Worker A (Haiku): 할 일"과 "구성: A, B"는 콜론 앞뒤로 나눠 할 일에 나온 이름을 세지 않는다.
fn body_entries(line: &str, installed: &[EngineId], work: bool) -> Vec<RosterEntry> {
    let body = line.trim_start_matches(LIST_MARKS);
    if let Some(i) = body.find([':', '：']) {
        let (head, tail) = body.split_at(i);
        let named = roster_entries(head, installed, work).0;
        if !named.is_empty() {
            return named;
        }
        return if COMPOSITION_WORDS.iter().any(|w| head.contains(w)) { roster_entries(tail, installed, work).0 } else { vec![] };
    }
    let (entries, rest) = roster_entries(body, installed, work);
    if rest <= ROSTER_LINE_MAX_WORDS {
        entries
    } else {
        vec![]
    }
}

/// 팀 이름표로 나뉜 구성표. 이름표 줄에 모델을 적었거나 그 아래 줄에 적은 꼴을 모두 받는다. 허락만 한 팀은 뺀다.
fn written_sections(text: &str, installed: &[EngineId], work: bool) -> Vec<Vec<RosterEntry>> {
    struct Section {
        entries: Vec<RosterEntry>,
        first: usize,
        last: usize,
        optional: bool,
    }
    // 같은 번호를 다시 적으면 나중 것이 이긴다. 한 글에 같은 팀을 두 번 적어도 늘어나지 않는다.
    fn close(open: &mut Option<Section>, teams: &mut std::collections::BTreeMap<usize, Vec<RosterEntry>>) {
        if let Some(s) = open.take().filter(|s| !s.optional && !s.entries.is_empty()) {
            for n in (s.first..=s.last).take(MAX_AGENTS + 1) {
                teams.insert(n, s.entries.clone());
            }
        }
    }
    let low = text.to_ascii_lowercase();
    let (mut teams, mut open, mut pending) = (std::collections::BTreeMap::new(), None, false);
    for line in low.lines() {
        let t = line.trim();
        if let Some((rest, first, last)) = team_label(line) {
            close(&mut open, &mut teams);
            let (entries, words) = roster_entries(rest, installed, work);
            open = Some(Section { entries: if words <= ROSTER_LINE_MAX_WORDS { entries } else { vec![] }, first, last, optional: pending || is_optional(rest) });
            pending = false;
        } else if t.starts_with('#') || t.starts_with("---") {
            close(&mut open, &mut teams);
            pending = is_optional(t);
        } else if let Some(s) = &mut open {
            s.entries.extend(body_entries(t, installed, work));
            pending |= !t.starts_with(['-', '*', '|']) && is_optional(t);
        } else {
            pending |= is_optional(t);
        }
    }
    close(&mut open, &mut teams);
    teams.into_values().collect()
}

/// "각 팀", "각 핵심 팀"처럼 팀 하나의 구성을 말하는 줄인지
fn describes_each_team(line: &str) -> bool {
    EACH_TEAM_WORDS.iter().any(|w| line.contains(w)) || line.match_indices("각 ").any(|(i, m)| line[i + m.len()..].split_whitespace().take(2).any(|w| strip_team_noun(w).is_some()))
}

/// "각각 4개씩"의 수
fn each_times(line: &str) -> Option<usize> {
    let (_, r) = line.split_once("각각")?;
    let t = r.trim_start();
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    t[digits..].trim_start().starts_with('개').then(|| t[..digits].parse().ok()).flatten()
}

/// "4개 전담 팀", "4개 팀", "팀이 4개", "4 teams"에서 팀 수. 팀 이름 "4팀"과 헷갈리지 않게 단위가 있는 말만 본다.
fn team_total(low: &str) -> Option<usize> {
    let digits_at_end = |s: &str| -> Option<usize> {
        let w = s.trim_end();
        let n = w.len() - w.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        w[w.len() - n..].parse().ok()
    };
    for (i, _) in low.match_indices('팀') {
        let pre = low[..i].trim_end();
        let pre = ["전담", "작업", "개발", "핵심", "의"].iter().find_map(|w| pre.strip_suffix(w)).unwrap_or(pre).trim_end();
        if let Some(n) = pre.strip_suffix('개').and_then(digits_at_end) {
            return Some(n);
        }
        let post = low[i + '팀'.len_utf8()..].trim_start_matches(['이', '가', '은', '는', '을', '를', ' ', ':']);
        let digits = post.len() - post.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && post[digits..].trim_start().starts_with('개') {
            return post[..digits].parse().ok();
        }
    }
    // "1팀 2팀 3팀 4팀"처럼 번호만 있으면 가장 큰 번호
    low.match_indices("teams").find_map(|(i, _)| digits_at_end(&low[..i])).or_else(|| low.match_indices('팀').filter_map(|(i, _)| digits_at_end(&low[..i])).max().filter(|n| *n >= 2))
}

/// "워커들은 Sonnet 5.5 high * 3, Gemini 3.8 Flash high * 2"처럼 워커 줄에 모델 여럿을 나열한 구성. 자리마다 한 팀이다.
/// 모델이 한 가지뿐이면 requested_team이 읽는다(나누는 AI가 하위 작업 수를 정할 수 있게).
fn listed_workers(low: &str, installed: &[EngineId], work: bool) -> Option<Vec<Vec<RosterEntry>>> {
    low.lines().filter(|l| WORKER_WORDS.iter().any(|w| l.contains(w))).find_map(|l| {
        let entries = roster_entries(l, installed, work).0;
        let mut families: Vec<usize> = entries.iter().map(|e| e.family).collect();
        families.dedup();
        families.sort_unstable();
        families.dedup();
        (families.len() >= 2).then(|| entries.iter().flat_map(|e| std::iter::repeat_n(vec![RosterEntry { count: 1, ..e.clone() }], e.count)).collect())
    })
}

/// 요청 글에 적힌 혼합 팀 구성("1팀 - Haiku high, Gemini flash high" 줄들, 또는 "각 팀은 Haiku 1기와 Gemini 1기" + "4개 팀").
pub fn requested_roster(text: &str, installed: &[EngineId], work: bool) -> Result<Option<Roster>, RosterError> {
    let mut teams = written_sections(text, installed, work);
    if teams.is_empty() {
        let low = text.to_ascii_lowercase();
        let each = low.lines().filter(|l| describes_each_team(l)).map(|l| roster_entries(l, installed, work).0).find(|e| !e.is_empty());
        if let (Some(each), Some(total)) = (each, team_total(&low)) {
            teams = vec![each; total];
        } else if let Some((times, entries)) = low.lines().filter_map(|l| Some((each_times(l)?, roster_entries(l, installed, work).0))).find(|(_, e)| e.len() >= 2) {
            teams = if PAIR_WORDS.iter().any(|w| low.contains(w)) {
                vec![entries; times]
            } else {
                (0..times).flat_map(|_| entries.iter().map(|e| vec![e.clone()])).collect()
            };
        } else if let Some(listed) = listed_workers(&low, installed, work) {
            teams = listed;
        }
    }
    build_roster(teams, text, installed, work)
}

/// AI가 뽑은 구성표를 읽는다. 이름표 모양과 상관없이 모델이 적힌 줄 하나가 한 팀이다. 답에 총괄이 없으면 요청 글에서 찾는다.
pub fn roster_from_answer(answer: &str, request: &str, installed: &[EngineId], work: bool) -> Result<Option<Roster>, RosterError> {
    let low = answer.to_ascii_lowercase();
    let teams: Vec<Vec<RosterEntry>> = low.lines().map(|l| roster_entries(l, installed, work).0).filter(|e| !e.is_empty()).collect();
    let mut roster = build_roster(teams.clone(), answer, installed, work)?;
    if let Some(r) = roster.as_mut().filter(|r| r.lead.is_none()) {
        r.lead = written_lead(request, &teams, installed, work);
    }
    Ok(roster)
}

/// 글에서 워커가 아닌 모델 중 총괄로 지목된 것
fn written_lead(text: &str, teams: &[Vec<RosterEntry>], installed: &[EngineId], work: bool) -> Option<ModelPick> {
    let families: Vec<usize> = teams.iter().flatten().map(|e| e.family).collect();
    let low = text.to_ascii_lowercase();
    scan(text).iter().filter(|h| !families.contains(&h.family) && names_lead(&low, h)).find_map(|h| pick_of(h, installed, work).ok())
}

fn build_roster(teams: Vec<Vec<RosterEntry>>, text: &str, installed: &[EngineId], work: bool) -> Result<Option<Roster>, RosterError> {
    let seats: usize = teams.iter().flatten().map(|e| e.count).sum();
    if seats < 2 {
        return Ok(None);
    }
    if seats > MAX_AGENTS {
        return Err(RosterError::TooMany(seats));
    }
    let mut out = vec![];
    for team in &teams {
        let mut members = vec![];
        for e in team {
            let pick = e.pick.clone().map_err(RosterError::Missing)?;
            members.extend(std::iter::repeat_n(RosterMember { pick, effort: e.effort }, e.count));
        }
        out.push(members);
    }
    Ok(Some(Roster { teams: out, lead: written_lead(text, &teams, installed, work) }))
}

/// 파서가 못 읽은 글에서 팀을 적은 듯한지 보는 말
const TEAM_HINTS: &[&str] = &["팀", "그룹", "페어", "짝", "쌍", "워커", "서브", "에이전트", "team", "group", "pair", "worker", "agent"];

/// 팀 구성을 적은 듯한데 파서가 못 읽은 글인지. 총괄 말고 모델이 둘 이상 나올 때만 AI에게 읽힌다.
pub fn needs_team_reading(text: &str, installed: &[EngineId], work: bool) -> bool {
    if requested_roster(text, installed, work) != Ok(None) || requested_team(text, installed, work).is_some() {
        return false;
    }
    let low = text.to_ascii_lowercase();
    let mut families: Vec<usize> = scan(text).iter().filter(|h| !lead_mention(&low, h, &low[h.end + effort_after(&low[h.end..]).1..])).map(|h| h.family).collect();
    families.sort_unstable();
    let mentions = families.len();
    families.dedup();
    // 팀을 뜻하는 말이 있거나, 표처럼 모델 이름이 줄마다 되풀이되는 글
    families.len() >= 2 && (TEAM_HINTS.iter().any(|w| low.contains(w)) || mentions > families.len())
}

/// 요청 글에서 팀 구성표만 뽑게 하는 지시. 답은 requested_roster가 그대로 읽는다.
pub fn team_reading_prompt(text: &str) -> String {
    let instruction = "사용자 요청을 직접 수행하지 말고 요청에서 언급된 팀 구성 정보만 한 줄에 한 팀씩 아래 형식에 맞춰 정확히 추출하십시오. 모델명과 작업 강도는 사용자가 기재한 표현을 그대로 유지하고 팀장을 명시했을 때만 팀장 줄을 추가하며 선택 사항으로 언급된 팀은 제외하고 임의로 지어내지 마십시오. 사용자가 지정한 팀 구성이 전혀 없다면 어떤 부가 설명도 없이 오직 없음 단어 하나만 출력하십시오.";
    format!("{instruction}\n\n총괄 - Claude Opus 5.5 high\n1팀 - Claude Haiku 5.5 high, Gemini 3.8 Flash high\n2팀 - Codex x2\n\n[요청]\n{text}")
}

const AGENT_WORDS: &[&str] = &["서브에이전트", "에이전트", "agents", "agent"];
const MANY_WORDS: &[&str] = &[
    "여러 에이전트", "에이전트 여러", "에이전트들", "멀티 에이전트", "멀티에이전트", "multi-agent", "multi agent", "multiple agents", "parallel agents", "in parallel", "병렬로", "병렬 에이전트", "동시에 여러",
];
const NUM_WORDS: &[(&str, usize)] = &[("다섯", 5), ("두", 2), ("둘", 2), ("세", 3), ("셋", 3), ("네", 4), ("넷", 4), ("two", 2), ("three", 3), ("four", 4), ("five", 5)];
pub const MAX_AGENTS: usize = 16;

/// 요청 글이 에이전트 여러 개를 동시에 돌려 달라고 하면 그 수(2~16). 숫자 없이 "여러·병렬"이면 3.
pub fn requested_agents(text: &str) -> Option<usize> {
    let low = text.to_ascii_lowercase();
    if let Some((n, _, _)) = agent_count(&low) {
        return (n >= 2).then_some(n.min(MAX_AGENTS));
    }
    MANY_WORDS.iter().any(|w| low.contains(w)).then_some(3)
}

/// 회의 지시: AI끼리 서로 대화하며 풀어 달라는 말. "회의록"처럼 주제로 쓰인 "회의"는 잡지 않게 행동을 뜻하는 말만 본다.
const COUNCIL_WORDS: &[&str] = &[
    "서로 대화", "서로 이야기", "서로 얘기", "서로 토론", "서로 논의", "서로 의논", "끼리 대화", "끼리 토론", "끼리 논의", "회의해서", "회의해 줘", "회의해줘", "회의로 풀", "회의를 열", "회의 열어", "토의해서", "토의해 줘", "의논해서", "의논해 줘",
    "talk to each other", "discuss with each other", "debate each other", "hold a meeting",
];

pub fn requested_council(text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    COUNCIL_WORDS.iter().any(|w| low.contains(w))
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

    const ALL: &[EngineId] = &[Claude, Codex, Antigravity, OpenCode, ClaudeApi, OpenAi, GeminiApi, Nvidia];

    const PAIR_LINES: &str = "지금 전달이 잘못되었어 팀은 총관인 Opus 5.5 high 중심으로 \n1팀 - Claude Haiku 5.5 high, Gemini flash 3.8 high\n2팀 - Claude Haiku 5.5 high, Gemini flash 3.8 high\n3팀 - Claude Haiku 5.5 high, Gemini flash 3.8 high\n4팀 - Claude Haiku 5.5 high, Gemini flash 3.8 high\n이렇게 구성되어야 해 필요에 따라 아주 간단한 임무를 맞는 \n5팀 - OpenCode, Nvidia API 이 있어도 괜찮아";
    const PAIR_PROSE: &str = "당신은 **총관리자(Lead Orchestrator, 모델: Claude Opus 5.5 High)**입니다.\n산하의 **8개 서브 에이전트(4개 전담 팀)**에게 분배하세요.\n\n## 1. 에이전트 팀 구성 (총 4개 팀 / 8개 에이전트 고정)\n각 팀은 반드시 **Claude Code (Haiku 5.5 High) 1기**와 **Gemini 3.8 Flash High 1기**가 1:1 페어(Pair)를 이룹니다.\n\n- **Team 1 ~ Team 4 공통 편성 (팀당 2기, 총 8기 가동)**:\n  - **Worker A (`Claude Code - Haiku 5.5 High`)**: 코드 수정 전담\n  - **Worker B (`Gemini 3.8 Flash High`)**: 검증 전담\n";

    fn seats(r: &Roster) -> Vec<Vec<(EngineId, Option<String>, Option<&'static str>)>> {
        r.teams.iter().map(|t| t.iter().map(|m| (m.pick.engine, m.pick.model.clone(), m.effort)).collect()).collect()
    }

    fn pair() -> Vec<(EngineId, Option<String>, Option<&'static str>)> {
        vec![(Claude, Some("haiku".into()), Some("high")), (Antigravity, Some("gemini-3.8-flash".into()), Some("high"))]
    }

    #[test]
    fn roster_reads_numbered_team_lines_and_the_lead() {
        let r = requested_roster(PAIR_LINES, ALL, true).unwrap().expect("팀 구성");
        assert_eq!(seats(&r), vec![pair(); 4], "허락만 한 5팀은 넣지 않는다");
        let lead = r.lead.expect("총괄");
        assert_eq!((lead.engine, lead.model.as_deref()), (Claude, Some("opus")));
    }

    #[test]
    fn roster_reads_a_pair_repeated_for_each_team() {
        let r = requested_roster(PAIR_PROSE, ALL, true).unwrap().expect("팀 구성");
        assert_eq!(seats(&r), vec![pair(); 4]);
        assert_eq!(r.lead.expect("총괄").model.as_deref(), Some("opus"));
    }

    #[test]
    fn roster_reads_a_team_described_in_a_sentence() {
        let text = "총관은 Opus 5.5 high야 Claude Code Haiku 5.5 high와 Gemini 3.8 flash high 각각 4개씩 한마디로 총 8개의 에이전트를 사용하여 작업을 진행해줘 각각 하나씩 그룹을 묶어 작업을 하게 해줘\n주의 사항으로 Haiku 5.5 high 1개와 Gemini 3.8 flash high 1개가 한 팀이고 이런 팀이 4개가 존재하면 좋겠어 그러니 총 에이전트는 8개가 돌아가야해";
        let r = requested_roster(text, ALL, true).unwrap().expect("팀 구성");
        assert_eq!(seats(&r), vec![pair(); 4]);
        assert_eq!(r.lead.expect("총괄").model.as_deref(), Some("opus"));
    }

    #[test]
    fn roster_reads_each_model_n_times_with_or_without_groups() {
        let grouped = "총관은 Opus 5.5 high야 Claude Code Haiku 5.5 high와 Gemini 3.8 flash high 각각 4개씩 사용하고 하나씩 그룹으로 묶어 줘";
        let r = requested_roster(grouped, ALL, true).unwrap().expect("팀 구성");
        assert_eq!(seats(&r), vec![pair(); 4], "총관은 워커 자리에 들어가지 않는다");
        assert_eq!(r.lead.expect("총괄").model.as_deref(), Some("opus"));
        let flat = requested_roster("Claude Haiku와 Gemini flash 각각 2개씩 돌려 줘", ALL, true).unwrap().expect("팀 구성");
        assert_eq!(flat.teams.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1, 1, 1], "묶으라는 말이 없으면 따로 돈다");
    }

    const SECTIONS: &str = "당신은 **총관리자(Lead Orchestrator, 모델: `Claude Opus 5.5 High`)**입니다.\n산하의 **핵심 4개 전담 팀(8개 서브 에이전트)**에게 분배하세요.\n\n---\n\n## 1. 에이전트 팀 구성 (절대 준수 사항)\n\n### [핵심 전담 팀: 1팀 2팀 3팀 4팀]\n각 핵심 팀은 반드시 **`Claude Haiku 5.5 High` 1기**와 **`Gemini 3.8 Flash High` 1기**가 1:1 페어로 구성되어야 합니다.\n\n- **1팀 (코드 리팩터링 및 버전 승격 전담)**\n  - **Worker A (`Claude Haiku 5.5 High`)**: 코드 수정 수행\n  - **Worker B (`Gemini 3.8 Flash High`)**: 교차 리뷰\n- **2팀 (런타임 구동 전담)**\n  - **Worker A (`Claude Haiku 5.5 High`)**: 테스트 실행\n  - **Worker B (`Gemini 3.8 Flash High`)**: 로그 분석\n- **3팀 (벤치마킹 전담)**\n  - **Worker A (`Claude Haiku 5.5 High`)**: 측정\n  - **Worker B (`Gemini 3.8 Flash High`)**: 분석\n- **4팀 (릴리즈 전담)**\n  - **Worker A (`Claude Haiku 5.5 High`)**: 빌드\n  - **Worker B (`Gemini 3.8 Flash High`)**: 문서 검증\n\n---\n## 2. 총관리자(`Claude Opus 5.5 High`) 지휘 원칙\n\n1. **실제 실행 강제**\n   - 서브 에이전트(특히 `Gemini 3.8 Flash High`)에게 경로를 전달하세요.\n3. **병렬 처리**\n   - 독립적인 작업은 1~4팀(및 필요시 5팀)에 병렬로 할당하세요.\n";

    #[test]
    fn roster_reads_team_sections_with_worker_lines() {
        let r = requested_roster(SECTIONS, ALL, true).unwrap().expect("팀 구성");
        assert_eq!(seats(&r), vec![pair(); 4]);
        assert_eq!(r.lead.expect("총괄").model.as_deref(), Some("opus"));
    }

    #[test]
    fn roster_leaves_out_a_team_that_was_only_allowed() {
        let text = format!("{}\n### [보조 지원 팀: 5팀]\n필요한 경우에 한하여 아래 5팀을 추가 호출할 수 있습니다.\n- **5팀 (경량 보조 전담)**\n  - **구성**: `OpenCode`, `Nvidia API` 기반 에이전트\n  - **역할**: 단순 포맷 변환\n", SECTIONS.split("---\n## 2.").next().unwrap());
        assert_eq!(seats(&requested_roster(&text, ALL, true).unwrap().expect("팀 구성")), vec![pair(); 4]);
        let required = "- **1팀**\n  - **구성**: `OpenCode`, `Nvidia API` 기반 에이전트\n- **2팀**\n  - Claude Haiku high\n";
        let r = requested_roster(required, ALL, true).unwrap().expect("팀 구성");
        assert_eq!(seats(&r)[0].iter().map(|m| m.0).collect::<Vec<_>>(), vec![OpenCode, Nvidia], "적어서 정한 구성은 그대로 쓴다");
    }

    #[test]
    fn roster_finds_each_team_with_a_word_between() {
        let r = requested_roster("각 핵심 팀은 Claude Haiku 1기와 Gemini flash 1기로 합니다. 1팀 2팀 3팀 4팀이 있습니다.", ALL, true).unwrap().expect("팀 구성");
        assert_eq!(r.teams.len(), 4);
        let range = requested_roster("- **1~3팀 공통**\n  - Worker A (Claude Haiku): 구현\n  - Worker B (Gemini flash): 검증\n", ALL, true).unwrap().expect("팀 구성");
        assert_eq!(range.teams.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 2, 2]);
    }

    #[test]
    fn team_reading_runs_only_for_a_team_the_parser_missed() {
        assert!(needs_team_reading("총관은 Opus로 하고 Haiku랑 Gemini flash를 짝지어서 네 쌍 만들어 일을 나눠 줘", ALL, true));
        assert!(needs_team_reading("이 표대로 돌려\n| 이름 | 구현 | 검증 |\n|---|---|---|\n| 알파조 | Claude Haiku 5.5 High | Gemini 3.8 Flash High |\n| 베타조 | Claude Haiku 5.5 High | Gemini 3.8 Flash High |\n", ALL, true), "표: 같은 모델이 여러 줄에 나온다");
        for text in [SECTIONS, PAIR_LINES, "Claude Code랑 Gemini 차이를 문서로 써 줘", "Gemini flash 워커 4개로 고쳐 줘", "에이전트 4개로 조사해 줘", "버그 고쳐 줘"] {
            assert!(!needs_team_reading(text, ALL, true), "{text}");
        }
    }

    #[test]
    fn team_reading_answer_is_read_by_the_same_parser() {
        let prompt = team_reading_prompt("요청 본문");
        assert!(prompt.contains("요청 본문") && prompt.contains("1팀 - ") && prompt.contains("없음"), "{prompt}");
        let r = roster_from_answer("총괄 - Claude Opus 5.5 high\n1팀 - Claude Haiku 5.5 high, Gemini 3.8 Flash high\n2팀 - Claude Haiku 5.5 high, Gemini 3.8 Flash high\n", "요청", ALL, true).unwrap().expect("구성표");
        assert_eq!(seats(&r), vec![pair(); 2]);
        assert_eq!(r.lead.expect("총괄").model.as_deref(), Some("opus"));
        assert_eq!(roster_from_answer("없음", "요청", ALL, true), Ok(None));
    }

    #[test]
    fn team_reading_answer_needs_no_team_label() {
        let opus = "총관은 Opus 5.5 high로 하고 Haiku 5.5 high랑 Gemini 3.8 Flash high를 짝지어서 네 쌍 만들어 일을 나눠 줘";
        let unlabeled = "총관 - Opus 5.5 high\nHaiku 5.5 high, Gemini 3.8 Flash high\nHaiku 5.5 high, Gemini 3.8 Flash high\nHaiku 5.5 high, Gemini 3.8 Flash high\nHaiku 5.5 high, Gemini 3.8 Flash high\n";
        let r = roster_from_answer(unlabeled, opus, ALL, true).unwrap().expect("구성표");
        assert_eq!(seats(&r), vec![pair(); 4]);
        assert_eq!(r.lead.expect("총괄").model.as_deref(), Some("opus"));
        let named = roster_from_answer("알파조 - Claude Haiku 5.5 High, Gemini 3.8 Flash High\n베타조 - Claude Haiku 5.5 High, Gemini 3.8 Flash High\n", "총괄은 Opus 5.5로 해 줘", ALL, true).unwrap().expect("구성표");
        assert_eq!(seats(&named), vec![pair(); 2]);
        assert_eq!(named.lead.expect("요청 글의 총괄").model.as_deref(), Some("opus"));
    }

    #[test]
    fn team_written_twice_in_one_message_is_not_doubled() {
        let text = format!("{PAIR_PROSE}\n{PAIR_LINES}");
        assert_eq!(seats(&requested_roster(&text, ALL, true).unwrap().expect("팀 구성")), vec![pair(); 4]);
        let later_wins = requested_roster("1팀 - Claude Haiku, Gemini flash\n2팀 - Claude Haiku\n1팀 - Codex", ALL, true).unwrap().expect("팀 구성");
        assert_eq!(later_wins.teams.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1]);
        assert_eq!(later_wins.teams[0][0].pick.engine, Codex);
    }

    #[test]
    fn roster_keeps_each_written_model_effort_and_count() {
        let r = requested_roster("Team 1: gemini-3.8-flash low x2 + Claude Sonnet\n**2팀**: Codex", ALL, true).unwrap().expect("팀 구성");
        assert_eq!(seats(&r)[0].len(), 3);
        assert_eq!(seats(&r)[0][1], (Antigravity, Some("gemini-3.8-flash".into()), Some("low")));
        assert_eq!(seats(&r)[0][2], (Claude, Some("sonnet".into()), None));
        assert_eq!(seats(&r)[1], vec![(Codex, None, None)]);
    }

    #[test]
    fn roster_ignores_text_that_only_mentions_teams_or_models() {
        for text in ["1팀: 실험 버전 승격\n2팀: 게임 창 테스트", "에이전트 4개로 조사해 줘", "1팀: Gemini API 연동 코드를 새로 구현하고 테스트를 추가\n2팀: Claude 호출 문서를 전부 다시 작성", "각 팀은 코드를 나눠 맡으세요. 4개 팀입니다.", "Claude Haiku로 고쳐 줘", "1팀 - Claude Haiku"] {
            assert_eq!(requested_roster(text, ALL, true), Ok(None), "{text}");
        }
    }

    #[test]
    fn roster_refuses_a_model_it_cannot_run_or_too_many_seats() {
        assert_eq!(requested_roster(PAIR_LINES, &[Antigravity], true), Err(RosterError::Missing("Claude Haiku".into())));
        let big = "1팀: Claude Haiku x9, Gemini flash\n2팀: Claude Haiku x9";
        assert_eq!(requested_roster(big, ALL, true), Err(RosterError::TooMany(19)));
    }

    const PLEXUS_REQUEST: &str ="당신은 본 프로젝트의 **총괄 분할자 및 관리자(Orchestrator & Lead Manager)**인 **Opus 5.5**입니다.\n아래 과제를 나눈 뒤, **4개의 Gemini 3.8 Flash (High) 하위 에이전트(Worker)**를 병렬 구동하여 작업을 지시·감독하십시오.\n- **병렬 실행 워커**: Gemini 3.8 Flash (High) × 4\n";

    #[test]
    fn team_request_names_lead_workers_effort_and_count() {
        let t = requested_team(PLEXUS_REQUEST, ALL, true).expect("팀 지시");
        assert_eq!(t.count, 4);
        assert_eq!(t.effort, Some("high"));
        assert_eq!((t.workers.engine, t.workers.model.as_deref()), (Antigravity, Some("gemini-3.8-flash")));
        let lead = t.lead.expect("총괄");
        assert_eq!((lead.engine, lead.model.as_deref()), (Claude, Some("opus")));
    }

    #[test]
    fn team_request_reads_other_wordings() {
        let t = requested_team("Claude가 총관리자를 맡고 gemini 3.8 flash high 4개에게 작업하라고 지시해", ALL, true).expect("수 뒤에 붙은 지시");
        assert_eq!((t.count, t.effort), (4, Some("high")));
        assert_eq!(t.lead.map(|l| l.engine), Some(Claude));
        let t = requested_team("워커: Gemini 3.8 Flash x 3 으로 고쳐", ALL, true).expect("x 3");
        assert_eq!((t.count, t.effort, t.lead.is_none()), (3, None, true));
        assert_eq!(requested_team("3개의 Gemini 에이전트로 나눠 줘", ALL, true).map(|t| t.count), Some(3));
        assert_eq!(requested_team("gemini 워커 99개", ALL, true).map(|t| t.count), Some(MAX_AGENTS), "최대 16개");
    }

    #[test]
    fn times_after_effort_counts_workers() {
        for text in ["총괄은 Opus 5.5 high, 워커는 Sonnet 5.5 high * 3으로 해 줘", "워커: **Sonnet 5.5** high *3", "워커 Sonnet 5.5 (high) × 3", "워커는 claude sonnet x3"] {
            let t = requested_team(text, ALL, true).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((t.count, t.workers.engine, t.workers.model.as_deref()), (3, Claude, Some("sonnet")), "{text}");
        }
        assert_eq!(requested_team("총괄은 Opus 5.5 high, 워커는 Sonnet 5.5 high * 3", ALL, true).unwrap().effort, Some("high"));
        let r = roster_from_answer("1팀 - Sonnet 5.5 high * 3
2팀 - Gemini 3.8 Flash high * 2", "요청", ALL, true).unwrap().expect("구성표");
        assert_eq!(r.teams.iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 2], "구성표의 * 수도 센다");
        assert!(requested_team("소네트 3개 써 줘", ALL, true).is_none(), "버전 없는 약한 이름은 여전히 받지 않는다");
    }

    #[test]
    fn workers_listed_on_one_line_are_each_a_seat() {
        let text = "총괄은 Opus 5.5 high를 사용해 주고 그 및 워커들은 Sonnet 5.5 high * 3, Gemini 3.8 Flash high * 2 이렇게 구성해줘";
        let r = requested_roster(text, ALL, true).unwrap().expect("나열한 워커");
        let s = seats(&r);
        assert_eq!(s.len(), 5, "자리마다 한 팀");
        assert_eq!(s[0], vec![(Claude, Some("sonnet".into()), Some("high"))]);
        assert_eq!(s[4], vec![(Antigravity, Some("gemini-3.8-flash".into()), Some("high"))]);
        assert_eq!(r.lead.expect("총괄").model.as_deref(), Some("opus"));
        let repeated = "총괄은 Opus 5.5 high를 사용해 주고 그 및 워커들은 Sonnet 5.5 high,Sonnet 5.5 high,Sonnet 5.5 high, Gemini 3.8 Flash high,Gemini 3.8 Flash high,Gemini 3.8 Flash high 이렇게 구성해줘";
        assert_eq!(seats(&requested_roster(repeated, ALL, true).unwrap().expect("되풀이한 워커")).len(), 6);
        assert_eq!(requested_roster("워커는 Sonnet 5.5 high * 3", ALL, true), Ok(None), "모델이 하나면 requested_team이 읽는다");
    }

    #[test]
    fn team_request_ignores_plain_mentions() {
        assert!(requested_team("Gemini 3.8 Flash 에이전트가 뭐야?", ALL, true).is_none(), "수가 없으면 아님");
        assert!(requested_team("gemini 워커 1개만", ALL, true).is_none(), "하나면 팀이 아님");
        assert!(requested_team("에이전트 4개로 조사해 줘", ALL, true).is_none(), "모델을 고르지 않음");
        assert!(requested_team("gemini 3.8 flash 버전이 4개 나왔다는데", ALL, true).is_none(), "수가 모델에 붙은 지시가 아님");
        assert!(requested_team(PLEXUS_REQUEST, &[Claude], true).is_none(), "워커를 돌릴 엔진이 없음");
    }

    #[test]
    fn opencode_is_asked_by_name_and_only_gets_free_models() {
        assert_eq!(one("opencode로 답해 줘", ALL), Some((OpenCode, None)));
        assert_eq!(one("오픈코드를 사용해서 정리해", ALL), Some((OpenCode, None)));
        assert_eq!(one("glm으로 답해", ALL), Some((OpenCode, Some("opencode/glm-5.3-flash-free".into()))));
        assert_eq!(one("딥시크로 답해", &[OpenCode]), Some((OpenCode, Some("opencode/deepseek-v4.1-flash-free".into()))));
        for f in FAMILIES {
            for (e, m) in f.targets {
                assert!(*e != OpenCode || m.is_empty() || crate::engines::is_free_model(m), "{}", f.name);
            }
        }
    }

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
        assert_eq!(requested_agents("에이전트 99개"), Some(MAX_AGENTS), "최대 16개");
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

    #[test]
    fn council_words_need_an_action_not_a_topic() {
        assert!(requested_council("세 AI가 서로 대화하며 이 버그 원인을 찾아 줘"));
        assert!(requested_council("회의해서 결정해 줘"));
        assert!(requested_council("let the agents talk to each other"));
        assert!(!requested_council("회의록을 정리해 줘"));
        assert!(!requested_council("회의실 예약 메일 써 줘"));
    }
}
