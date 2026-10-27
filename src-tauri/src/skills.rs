//! 스킬·플러그인(폴더 방식). 형식은 Claude Code와 같다.
//! - 스킬: `<데이터>/skills/<이름>/SKILL.md` (머리말 name·description + 본문 지시문)
//! - 플러그인: `<데이터>/plugins/<이름>/.claude-plugin/plugin.json` + `skills/<이름>/SKILL.md` (+ commands·agents 등)
//!
//! Elysium이 쓰는 추가 머리말(선택): `engine`(claude·codex·gemini), `review`(true/false),
//! `mode`(chat·work), `triggers`(이 낱말이 들어간 요청에 자동 적용, 쉼표 구분).

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 처음 실행할 때 넣어 두는 플러그인. 직접 넣은 플러그인과 달리 처음부터 켜져 있다.
pub const BUILTIN_PLUGINS: [&str; 1] = ["elysium-starter"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// 스킬 폴더 이름. 플러그인 스킬은 "플러그인:스킬".
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(skip)]
    pub body: String,
    pub engine: Option<String>,
    pub review: Option<bool>,
    /// true면 정확도가 중요하다고 보고 토론으로 답한다
    pub debate: Option<bool>,
    /// 노력 단계를 고정한다: light · standard · deep
    pub tier: Option<String>,
    pub mode: Option<String>,
    pub triggers: Vec<String>,
    /// "user"(직접 만든 스킬) 또는 플러그인 이름
    pub source: String,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub path: String,
    pub skills: usize,
    /// commands·agents·hooks·.mcp.json 등 Claude Code가 직접 쓰는 구성이 있는지
    pub extras: Vec<String>,
    pub builtin: bool,
    pub enabled: bool,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct Catalog {
    pub skills: Vec<Skill>,
    pub plugins: Vec<Plugin>,
}

impl Catalog {
    pub fn enabled_skills(&self) -> impl Iterator<Item = &Skill> {
        self.skills.iter().filter(|s| s.enabled)
    }

    /// `/이름 나머지` 형식이면 그 스킬과 나머지 요청을 돌려준다.
    pub fn explicit<'a>(&self, text: &'a str) -> Option<(&Skill, &'a str)> {
        let t = text.trim_start();
        let rest = t.strip_prefix('/')?;
        let (name, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let name = name.to_lowercase();
        let s = self.enabled_skills().find(|s| s.name.to_lowercase() == name || s.id.to_lowercase() == name)?;
        Some((s, tail.trim()))
    }

    /// 머리말 triggers의 낱말이 요청에 들어 있으면 그 스킬을 고른다(가장 긴 낱말 우선).
    pub fn triggered(&self, text: &str) -> Option<&Skill> {
        let low = text.to_lowercase();
        self.enabled_skills()
            .filter_map(|s| s.triggers.iter().filter(|t| !t.is_empty() && low.contains(&t.to_lowercase())).map(|t| t.chars().count()).max().map(|n| (n, s)))
            .max_by_key(|(n, _)| *n)
            .map(|(_, s)| s)
    }

    pub fn enabled_plugin_dirs(&self) -> Vec<String> {
        self.plugins.iter().filter(|p| p.enabled && p.error.is_none()).map(|p| p.path.clone()).collect()
    }
}

/// 아주 작은 머리말 해석기(`key: value` 줄만). 값의 따옴표와 대괄호 목록을 벗긴다.
pub fn parse_skill_md(text: &str) -> (HashMap<String, String>, String) {
    let text = text.trim_start_matches('\u{feff}');
    let mut meta = HashMap::new();
    let Some(rest) = text.strip_prefix("---") else {
        return (meta, text.trim().to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (meta, text.trim().to_string());
    };
    for line in rest[..end].lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim().to_lowercase();
        if k.is_empty() || k.starts_with('#') {
            continue;
        }
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        meta.insert(k, v);
    }
    let body = rest[end + 4..].trim_start_matches(|c| c == '-').trim().to_string();
    (meta, body)
}

fn list_value(v: Option<&String>) -> Vec<String> {
    v.map(|v| v.trim_matches(|c| c == '[' || c == ']').split(',').map(|x| x.trim().trim_matches(|c| c == '"' || c == '\'').to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default()
}

fn read_skill(dir: &Path, id: String, source: &str, toggles: &HashMap<String, bool>) -> Option<Skill> {
    let path = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&path).ok()?;
    let (meta, body) = parse_skill_md(&text);
    let folder = dir.file_name()?.to_string_lossy().to_string();
    let engine = meta.get("engine").map(|e| e.to_lowercase()).filter(|e| matches!(e.as_str(), "claude" | "codex" | "gemini"));
    Some(Skill {
        enabled: toggles.get(&format!("skill:{id}")).copied().unwrap_or(true),
        name: meta.get("name").cloned().filter(|n| !n.is_empty()).unwrap_or(folder),
        description: meta.get("description").cloned().unwrap_or_default(),
        body,
        engine,
        review: meta.get("review").map(|v| v.eq_ignore_ascii_case("true")),
        debate: meta.get("debate").map(|v| v.eq_ignore_ascii_case("true")),
        tier: meta.get("tier").map(|t| t.to_lowercase()).filter(|t| matches!(t.as_str(), "light" | "standard" | "deep")),
        mode: meta.get("mode").map(|m| m.to_lowercase()).filter(|m| m == "chat" || m == "work"),
        triggers: list_value(meta.get("triggers")),
        source: source.to_string(),
        path: path.to_string_lossy().to_string(),
        id,
    })
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    v.sort();
    v
}

pub fn load(root: &Path, toggles: &HashMap<String, bool>) -> Catalog {
    let mut cat = Catalog::default();
    for d in subdirs(&root.join("skills")) {
        let id = d.file_name().unwrap_or_default().to_string_lossy().to_string();
        if let Some(s) = read_skill(&d, id, "user", toggles) {
            cat.skills.push(s);
        }
    }
    for d in subdirs(&root.join("plugins")) {
        let id = d.file_name().unwrap_or_default().to_string_lossy().to_string();
        let manifest = d.join(".claude-plugin").join("plugin.json");
        let builtin = BUILTIN_PLUGINS.contains(&id.as_str());
        let enabled = toggles.get(&format!("plugin:{id}")).copied().unwrap_or(builtin);
        let (name, description, version, error) = match std::fs::read_to_string(&manifest).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| e.to_string())) {
            Ok(v) => (
                v["name"].as_str().unwrap_or(&id).to_string(),
                v["description"].as_str().unwrap_or("").to_string(),
                v["version"].as_str().unwrap_or("").to_string(),
                None,
            ),
            Err(e) => (id.clone(), String::new(), String::new(), Some(format!(".claude-plugin/plugin.json을 읽을 수 없음: {e}"))),
        };
        let mut count = 0;
        for sd in subdirs(&d.join("skills")) {
            let sid = sd.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Some(mut s) = read_skill(&sd, format!("{id}:{sid}"), &id, toggles) {
                // 꺼진 플러그인의 스킬은 쓰지 않는다.
                s.enabled &= enabled && error.is_none();
                cat.skills.push(s);
                count += 1;
            }
        }
        let extras = ["commands", "agents", "hooks", ".mcp.json"].iter().filter(|x| d.join(x).exists()).map(|x| x.to_string()).collect();
        cat.plugins.push(Plugin { id, name, description, version, path: d.to_string_lossy().to_string(), skills: count, extras, builtin, enabled, error });
    }
    cat
}

/// 처음 실행할 때 예시 스킬과 플러그인을 넣는다. 이미 폴더가 있으면 건드리지 않는다.
pub fn seed(root: &Path) -> std::io::Result<()> {
    let skills = root.join("skills");
    if !skills.exists() {
        for (dir, text) in SEED_SKILLS {
            let d = skills.join(dir);
            std::fs::create_dir_all(&d)?;
            std::fs::write(d.join("SKILL.md"), text)?;
        }
    }
    // 기본 스킬(처음 한 번, 새 판이 나오면 고치지 않은 것만 바꿈)
    seed_builtins(&skills)?;
    let plugin = root.join("plugins").join("elysium-starter");
    if !root.join("plugins").exists() {
        std::fs::create_dir_all(plugin.join(".claude-plugin"))?;
        std::fs::write(plugin.join(".claude-plugin").join("plugin.json"), STARTER_MANIFEST)?;
        let s = plugin.join("skills").join("brainstorm");
        std::fs::create_dir_all(&s)?;
        std::fs::write(s.join("SKILL.md"), STARTER_SKILL)?;
    }
    Ok(())
}

/// 새 스킬 틀을 만들고 SKILL.md 경로를 돌려준다.
pub fn create_skill(root: &Path, name: &str) -> Result<PathBuf, String> {
    let id: String = name.trim().chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let id = id.trim_matches('-').to_lowercase();
    if id.is_empty() {
        return Err("이름을 입력하세요".into());
    }
    let d = root.join("skills").join(&id);
    if d.exists() {
        return Err(format!("같은 이름의 스킬이 있습니다: {id}"));
    }
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let path = d.join("SKILL.md");
    let text = format!(
        "---\nname: {id}\ndescription: 이 스킬이 하는 일을 한 줄로 적으세요\n# engine: claude\n# review: true\n# triggers: 낱말1, 낱말2\n---\n\n여기에 AI에게 줄 지시문을 적으세요.\n"
    );
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// 엘리시움 기본 스킬(src-tauri/seed-skills). 여러 공개 스킬에서 필요한 부분만 골라 다시 정리했다.
/// 판(version)을 올리면, 사용자가 고치지 않은 이전 판은 새 판으로 바꾼다(고친 것은 건드리지 않음).
struct Builtin {
    dir: &'static str,
    version: u32,
    /// (스킬 폴더 안의 경로, 내용). 첫 항목이 SKILL.md
    files: &'static [(&'static str, &'static str)],
}

const BUILTIN_SKILLS: &[Builtin] = &[
    Builtin {
        dir: "ui-craft",
        version: 2,
        files: &[
            ("SKILL.md", include_str!("../seed-skills/ui-craft/SKILL.md")),
            ("references/accessibility.md", include_str!("../seed-skills/ui-craft/references/accessibility.md")),
            ("references/components-libraries.md", include_str!("../seed-skills/ui-craft/references/components-libraries.md")),
            ("references/content-forms-states.md", include_str!("../seed-skills/ui-craft/references/content-forms-states.md")),
            ("references/data-display.md", include_str!("../seed-skills/ui-craft/references/data-display.md")),
            ("references/direction-and-tokens.md", include_str!("../seed-skills/ui-craft/references/direction-and-tokens.md")),
            ("references/interaction-motion.md", include_str!("../seed-skills/ui-craft/references/interaction-motion.md")),
            ("references/responsive-performance.md", include_str!("../seed-skills/ui-craft/references/responsive-performance.md")),
            ("references/review-and-testing.md", include_str!("../seed-skills/ui-craft/references/review-and-testing.md")),
        ],
    },
    Builtin {
        dir: "perf-tune",
        version: 2,
        files: &[
            ("SKILL.md", include_str!("../seed-skills/perf-tune/SKILL.md")),
            ("references/backend-db.md", include_str!("../seed-skills/perf-tune/references/backend-db.md")),
            ("references/languages.md", include_str!("../seed-skills/perf-tune/references/languages.md")),
            ("references/measure.md", include_str!("../seed-skills/perf-tune/references/measure.md")),
            ("references/report.md", include_str!("../seed-skills/perf-tune/references/report.md")),
            ("references/web.md", include_str!("../seed-skills/perf-tune/references/web.md")),
        ],
    },
    Builtin {
        dir: "fact-check",
        version: 2,
        files: &[
            ("SKILL.md", include_str!("../seed-skills/fact-check/SKILL.md")),
            ("references/claims.md", include_str!("../seed-skills/fact-check/references/claims.md")),
            ("references/debate.md", include_str!("../seed-skills/fact-check/references/debate.md")),
            ("references/evidence.md", include_str!("../seed-skills/fact-check/references/evidence.md")),
            ("references/report.md", include_str!("../seed-skills/fact-check/references/report.md")),
        ],
    },
    Builtin {
        dir: "token-saver",
        version: 2,
        files: &[
            ("SKILL.md", include_str!("../seed-skills/token-saver/SKILL.md")),
        ],
    },
    Builtin {
        dir: "root-cause",
        version: 2,
        files: &[
            ("SKILL.md", include_str!("../seed-skills/root-cause/SKILL.md")),
        ],
    },
];

/// 이전 판들의 SKILL.md 해시(SHA-256): 이 값과 같으면 사용자가 고치지 않은 것으로 본다.
const BUILTIN_OLD: &[(&str, &str)] = &[
    ("ui-craft", "82750bc5e3a0a189812119856915fd6977017976990ef7450211ddd34f68ecd1"),
    ("perf-tune", "b48ee41dd19bffceaa55cd8e93829aaf0acf171377ffef9ae94ccf00572c56de"),
    ("fact-check", "080f2a85c8581b4054bb331797904b663dc611cdb24d3d6ecf9142a2cf458832"),
    ("token-saver", "5726894977f5088650d07b4fa14e47d9dff565c3eb325f371b092e6bfbcc1eb0"),
];

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn write_builtin(dir: &Path, b: &Builtin) -> std::io::Result<()> {
    for (rel, text) in b.files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, text)?;
    }
    Ok(())
}

/// 기본 스킬을 넣거나 새 판으로 바꾼다. 기록: skills/.seeded ("이름@판" 한 줄씩, 판이 없으면 1)
fn seed_builtins(skills: &Path) -> std::io::Result<()> {
    let mark = skills.join(".seeded");
    let mut done: Vec<(String, u32)> = std::fs::read_to_string(&mark)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match l.split_once('@') {
            Some((n, v)) => (n.to_string(), v.parse().unwrap_or(1)),
            None => (l.to_string(), 1),
        })
        .collect();
    let mut changed = false;
    for b in BUILTIN_SKILLS {
        let dir = skills.join(b.dir);
        match done.iter().position(|(n, _)| n == b.dir) {
            None => {
                // 처음: 같은 이름의 스킬이 이미 있으면(사용자가 만든 것) 건드리지 않는다.
                if !dir.exists() {
                    write_builtin(&dir, b)?;
                }
                done.push((b.dir.to_string(), b.version));
                changed = true;
            }
            Some(i) if done[i].1 < b.version => {
                // 새 판: 지웠으면 다시 만들지 않고, 고쳤으면 그대로 둔다.
                if let Ok(cur) = std::fs::read(dir.join("SKILL.md")) {
                    let h = sha256_hex(&cur);
                    if BUILTIN_OLD.iter().any(|(n, old)| *n == b.dir && *old == h) {
                        write_builtin(&dir, b)?;
                    }
                }
                done[i].1 = b.version;
                changed = true;
            }
            Some(_) => {}
        }
    }
    if changed {
        let text: String = done.iter().map(|(n, v)| format!("{n}@{v}\n")).collect();
        std::fs::write(&mark, text)?;
    }
    Ok(())
}

/// 스킬 폴더의 참고 파일(references/*.md): (절대 경로, 첫 제목)
pub fn references(skill_md: &str) -> Vec<(String, String)> {
    let Some(dir) = Path::new(skill_md).parent() else { return vec![] };
    let mut v: Vec<(String, String)> = std::fs::read_dir(dir.join("references"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .map(|p| {
                    let title = std::fs::read_to_string(&p).ok().and_then(|t| t.lines().find(|l| l.starts_with('#')).map(|l| l.trim_start_matches('#').trim().to_string())).unwrap_or_default();
                    (p.to_string_lossy().to_string(), title)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}


const SEED_SKILLS: [(&str, &str); 4] = [
    (
        "summarize",
        "---\nname: summarize\ndescription: 긴 글·문서·대화를 핵심만 짧게 요약\ntriggers: 요약해, 요약 좀, 정리해 줘\n---\n\n사용자가 준 내용을 한국어로 요약하세요.\n- 맨 위에 한 문장 결론\n- 그 아래 핵심 3~5개를 글머리표로\n- 숫자·날짜·이름은 원문 그대로 유지\n- 원문에 없는 내용은 덧붙이지 않기\n",
    ),
    (
        "code-review",
        "---\nname: code-review\ndescription: 코드나 변경 사항을 버그·보안·가독성 관점에서 검토\nengine: claude\nreview: false\ntriggers: 코드 리뷰, 코드리뷰\n---\n\n코드를 검토하세요. 작업 폴더의 파일을 읽어도 되지만 고치지는 마세요.\n1. 실제로 문제를 일으킬 버그와 보안 문제를 심각한 순서로\n2. 각 항목마다 파일·줄, 왜 문제인지, 고치는 방법\n3. 마지막에 사소한 개선점은 짧게\n확실하지 않은 것은 추측이라고 밝히세요.\n",
    ),
    (
        "report",
        "---\nname: report\ndescription: 주제를 조사해 구조가 잡힌 보고서로 작성\nreview: true\ntriggers: 보고서\n---\n\n요청한 주제로 한국어 보고서를 쓰세요.\n구성: 요약 → 배경 → 본문(소제목) → 결론과 다음 단계.\n근거가 필요한 주장은 출처를 밝히고, 모르는 것은 모른다고 쓰세요.\n",
    ),
    (
        "translate",
        "---\nname: translate\ndescription: 자연스러운 번역(한↔영 등)\ntriggers: 번역해, 번역 좀\n---\n\n뜻을 지키면서 목표 언어로 자연스럽게 번역하세요. 목표 언어를 말하지 않았으면 한국어는 영어로, 다른 언어는 한국어로 옮기세요. 번역문만 출력하세요.\n",
    ),
];

const STARTER_MANIFEST: &str = r#"{
  "name": "elysium-starter",
  "description": "Elysium 예시 플러그인: 플러그인 구조를 보여 주는 아이디어 스킬 1개",
  "version": "0.1.0"
}
"#;

const STARTER_SKILL: &str = "---\nname: brainstorm\ndescription: 아이디어를 여러 방향으로 넓게 낸 뒤 좋은 것을 골라 줌\ntriggers: 아이디어, 브레인스토밍\n---\n\n서로 다른 방향의 아이디어를 8개 내고, 그중 실행하기 쉽고 효과가 큰 3개를 골라 이유와 첫 단계를 적으세요.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter_and_body() {
        let (m, body) = parse_skill_md("\u{feff}---\nname: \"My Skill\"\ndescription: 설명: 콜론 포함\ntriggers: [요약, '정리']\nreview: true\n---\n\n본문\n둘째 줄");
        assert_eq!(m["name"], "My Skill");
        assert_eq!(m["description"], "설명: 콜론 포함");
        assert_eq!(list_value(m.get("triggers")), vec!["요약", "정리"]);
        assert_eq!(body, "본문\n둘째 줄");
        let (m2, b2) = parse_skill_md("머리말 없음");
        assert!(m2.is_empty() && b2 == "머리말 없음");
    }

    #[test]
    fn seed_load_match_and_toggle() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path()).unwrap();
        let cat = load(tmp.path(), &HashMap::new());
        assert_eq!(cat.skills.len(), 10, "예시 스킬 4 + 기본 스킬 5 + 플러그인 스킬 1");
        assert_eq!(cat.triggered("로그인하면 크래시가 나는데 원인 찾아줘").unwrap().id, "root-cause");
        let ui = cat.skills.iter().find(|s| s.id == "ui-craft").unwrap();
        let refs = references(&ui.path);
        assert_eq!(refs.len(), 8, "ui-craft 참고 파일 8개");
        assert!(refs.iter().any(|(p, t)| p.ends_with("accessibility.md") && t.contains("접근성")));
        assert!(std::fs::read_to_string(tmp.path().join("skills/.seeded")).unwrap().contains("ui-craft@2"));
        assert_eq!(cat.triggered("설정 화면 디자인 시스템 좀 잡아줘").unwrap().id, "ui-craft");
        let fc = cat.triggered("이 통계가 정확한지 팩트체크 해줘").unwrap();
        assert_eq!((fc.id.as_str(), fc.debate), ("fact-check", Some(true)));
        assert_eq!(cat.triggered("핵심만 짧게 알려줘").unwrap().tier.as_deref(), Some("light"));
        assert_eq!(cat.triggered("API가 너무 느려 최적화 해줘").unwrap().tier.as_deref(), Some("deep"));
        // 새 판: 고친 스킬은 그대로 둔다
        std::fs::write(tmp.path().join("skills/perf-tune/SKILL.md"), "---\nname: perf-tune\n---\n내가 고침").unwrap();
        std::fs::write(tmp.path().join("skills/.seeded"), "ui-craft\nperf-tune\nfact-check\ntoken-saver\nroot-cause@2\n").unwrap();
        seed(tmp.path()).unwrap();
        assert!(std::fs::read_to_string(tmp.path().join("skills/perf-tune/SKILL.md")).unwrap().contains("내가 고침"), "고친 것은 덮어쓰지 않음");
        assert!(std::fs::read_to_string(tmp.path().join("skills/.seeded")).unwrap().contains("perf-tune@2"));
        // 지운 기본 스킬은 다시 만들지 않는다
        std::fs::remove_dir_all(tmp.path().join("skills/token-saver")).unwrap();
        seed(tmp.path()).unwrap();
        assert!(!tmp.path().join("skills/token-saver").exists());
        let p = &cat.plugins[0];
        assert!(p.builtin && p.enabled && p.error.is_none() && p.skills == 1);
        // 명시 호출
        let (s, rest) = cat.explicit("/summarize 이 글을 줄여줘").unwrap();
        assert_eq!((s.name.as_str(), rest), ("summarize", "이 글을 줄여줘"));
        assert!(cat.explicit("/없는스킬 hi").is_none());
        // 낱말로 자동 적용
        assert_eq!(cat.triggered("이 코드 리뷰 부탁해").unwrap().name, "code-review");
        assert_eq!(cat.triggered("새 앱 아이디어 줘").unwrap().id, "elysium-starter:brainstorm");
        assert!(cat.triggered("안녕").is_none());
        // 스킬 끄기, 플러그인 끄면 그 스킬도 꺼짐
        let mut t = HashMap::new();
        t.insert("skill:summarize".to_string(), false);
        t.insert("plugin:elysium-starter".to_string(), false);
        let cat = load(tmp.path(), &t);
        assert!(cat.explicit("/summarize x").is_none());
        assert!(cat.triggered("아이디어").is_none());
        assert!(cat.enabled_plugin_dirs().is_empty());
        // 다시 seed해도 덮어쓰지 않음
        std::fs::write(tmp.path().join("skills/summarize/SKILL.md"), "---\nname: summarize\n---\n바꿈").unwrap();
        seed(tmp.path()).unwrap();
        assert!(std::fs::read_to_string(tmp.path().join("skills/summarize/SKILL.md")).unwrap().contains("바꿈"));
    }

    #[test]
    fn user_plugins_start_disabled_and_broken_manifest_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("plugins/thirdparty");
        std::fs::create_dir_all(d.join(".claude-plugin")).unwrap();
        std::fs::write(d.join(".claude-plugin/plugin.json"), r#"{"name":"thirdparty"}"#).unwrap();
        std::fs::create_dir_all(d.join("commands")).unwrap();
        let bad = tmp.path().join("plugins/broken");
        std::fs::create_dir_all(&bad).unwrap();
        let cat = load(tmp.path(), &HashMap::new());
        let third = cat.plugins.iter().find(|p| p.id == "thirdparty").unwrap();
        assert!(!third.enabled, "직접 넣은 플러그인은 꺼진 채 시작");
        assert_eq!(third.extras, vec!["commands"]);
        assert!(cat.plugins.iter().find(|p| p.id == "broken").unwrap().error.is_some());
    }

    #[test]
    fn creates_skill_template_with_safe_name() {
        let tmp = tempfile::tempdir().unwrap();
        let p = create_skill(tmp.path(), " My Skill/../x ").unwrap();
        assert!(p.starts_with(tmp.path().join("skills")));
        assert!(p.to_string_lossy().contains("my-skill"));
        assert!(create_skill(tmp.path(), " My Skill/../x ").is_err());
        assert!(create_skill(tmp.path(), "   ").is_err());
    }
}
