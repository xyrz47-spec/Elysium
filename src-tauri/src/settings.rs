//! 사용자 설정. 항목은 일부러 최소한만 둔다.

use crate::db::Db;
use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMode {
    /// 길거나 어려운 요청만 다른 AI가 검토한다.
    Auto,
    Always,
    Off,
    /// 모든 대화 요청을 두 AI가 토론하고 판정 AI가 정리한다(토큰을 가장 많이 쓴다).
    Debate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// AI CLI가 일하는 폴더. 작업 모드에서는 이 안에서만 파일을 고친다. None이면 문서\Elysium.
    pub workspace_dir: Option<String>,
    pub review: ReviewMode,
    /// TypeSafe Jev로 요청 종류·난이도를 판단해 AI와 검토 여부를 고르고, 세션 기억 중 필요한 부분만 골라 전달한다(요청 글·세션 기억이 TypeSafe로 전송됨).
    pub jev_enabled: bool,
    /// API 엔진별 모델 이름(engine id → model). 없으면 기본 모델.
    pub models: BTreeMap<String, String>,
    /// 입력하는 동안 JEV가 모드·스킬을 추천한다(보내기 전의 글이 TypeSafe로 전송됨). JEV가 켜져 있을 때만 돈다.
    pub jev_suggest: bool,
    /// 창을 띄우는 실행(창 테스트)를 동시에 몇 개까지 돌릴지(앱 전체, 1~8)
    pub test_slots: u8,
    /// 병렬 작업에서 워커가 끝낼 때마다 총괄이 변경을 검토·보정한 뒤 그 파일만 커밋한다(git 저장소일 때만).
    pub lead_commit: bool,
    /// 모든 AI에게 미리 알려 두는 말(나에 대해·답변 방식). 시스템 지시 끝에 붙는다.
    pub instructions: String,
    /// 답변 언어: auto(요청 글의 언어, 기본은 한국어)·ko·en·ja·zh
    pub answer_lang: String,
    /// 답변 길이: auto·brief·detailed
    pub answer_length: String,
    /// 자주 쓰는 프롬프트(입력창에서 골라 넣는다)
    pub prompts: Vec<SavedPrompt>,
    /// 작업이 끝났을 때 창이 앞에 없으면 작업 표시줄에서 알린다
    pub notify_done: bool,
    /// Allow shutting down / restarting / logging off after a task, when the user asked for it in that task.
    pub power_on_done: bool,
}

/// 자주 쓰는 프롬프트 하나
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedPrompt {
    pub title: String,
    pub text: String,
}

/// 사용자 지시의 최대 글자 수(매 요청에 붙으므로 길면 토큰이 든다)
pub const INSTRUCTIONS_MAX: usize = 2000;
const PROMPTS_MAX: usize = 50;
const PROMPT_TEXT_MAX: usize = 8000;
const LANGS: &[&str] = &["auto", "ko", "en", "ja", "zh"];
const LENGTHS: &[&str] = &["auto", "brief", "detailed"];

/// 지금 설정의 답변 방식. 시스템 지시를 만드는 곳은 설정을 받지 않는 곳이 많아 여기서 읽는다.
static PREFS: std::sync::RwLock<Option<Prefs>> = std::sync::RwLock::new(None);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prefs {
    pub lang: String,
    pub length: String,
    pub instructions: String,
}

/// 설정을 읽거나 바꿀 때마다 부른다.
pub fn set_prefs(s: &Settings) {
    *PREFS.write().unwrap_or_else(|p| p.into_inner()) = Some(Prefs { lang: s.answer_lang.clone(), length: s.answer_length.clone(), instructions: s.instructions.clone() });
}

pub fn prefs() -> Prefs {
    PREFS.read().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_default()
}

impl Prefs {
    /// 시스템 지시 첫 줄의 언어 안내
    pub fn lang_line(&self) -> &'static str {
        match self.lang.as_str() {
            "en" => "Answer in English unless the user asks for another language.",
            "ja" => "ユーザーが別の言語を求めない限り、日本語で答えてください。",
            "zh" => "除非用户要求其他语言，请用简体中文回答。",
            "ko" => "사용자가 다른 언어를 요청하지 않으면 한국어로 답하세요.",
            _ => "사용자는 한국어를 씁니다. 사용자가 다른 언어로 쓰거나 요청하지 않으면 한국어로 답하세요.",
        }
    }

    /// 시스템 지시 끝에 붙는 답변 길이와 사용자 지시(고른 것이 없으면 빈 글)
    pub fn tail(&self) -> String {
        let mut s = String::new();
        match self.length.as_str() {
            "brief" => s.push_str("\n[답변 길이] 결론부터 짧게 답하세요. 꼭 필요한 설명만 쓰고, 코드·숫자·경고는 줄이지 마세요.\n"),
            "detailed" => s.push_str("\n[답변 길이] 배경, 이유, 예시, 주의할 점까지 자세히 설명하세요.\n"),
            _ => {}
        }
        let t = self.instructions.trim();
        if !t.is_empty() {
            s.push_str(&format!("\n[사용자가 미리 알려 둔 것] 사용자가 모든 요청에 적용해 달라고 적어 둔 정보와 선호입니다. 이번 요청과 어긋나면 이번 요청을 따르세요.\n{t}\n"));
        }
        s
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            workspace_dir: None,
            review: ReviewMode::Auto,
            jev_enabled: false,
            models: BTreeMap::new(),
            jev_suggest: true,
            test_slots: 1,
            lead_commit: true,
            instructions: String::new(),
            answer_lang: "auto".into(),
            answer_length: "auto".into(),
            prompts: vec![],
            notify_done: true,
            power_on_done: false,
        }
    }
}

impl Settings {
    pub fn workspace(&self) -> PathBuf {
        match self.workspace_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => default_workspace(),
        }
    }

    pub fn sanitized(mut self) -> Self {
        self.workspace_dir = self.workspace_dir.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
        self.models = self.models.into_iter().map(|(k, v)| (k, v.trim().to_string())).filter(|(_, v)| !v.is_empty()).collect();
        self.test_slots = self.test_slots.clamp(1, 8);
        self.instructions = self.instructions.trim().chars().take(INSTRUCTIONS_MAX).collect();
        if !LANGS.contains(&self.answer_lang.as_str()) {
            self.answer_lang = "auto".into();
        }
        if !LENGTHS.contains(&self.answer_length.as_str()) {
            self.answer_length = "auto".into();
        }
        self.prompts = self
            .prompts
            .into_iter()
            .map(|p| SavedPrompt { title: p.title.trim().chars().take(60).collect(), text: p.text.trim().chars().take(PROMPT_TEXT_MAX).collect() })
            .filter(|p| !p.text.is_empty())
            .map(|p| if p.title.is_empty() { SavedPrompt { title: p.text.chars().take(24).collect(), ..p } } else { p })
            .take(PROMPTS_MAX)
            .collect();
        // OpenCode는 무료 모델만: 다른 값은 저장하지 않는다.
        if self.models.get("opencode").is_some_and(|m| !crate::engines::is_free_model(m)) {
            self.models.remove("opencode");
        }
        self
    }
}

pub fn default_workspace() -> PathBuf {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Documents").join("Elysium")
}

pub fn load(db: &Db) -> AppResult<Settings> {
    let raw: Option<String> = db.with(|c| {
        c.query_row("SELECT value FROM settings WHERE key = 'app'", [], |r| r.get(0))
            .map(Some)
            .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
    })?;
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default())
}

pub fn save(db: &Db, s: &Settings) -> AppResult<()> {
    let json = serde_json::to_string(s)?;
    db.with(|c| c.execute("INSERT INTO settings(key, value) VALUES ('app', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [json]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_default_workspace() {
        let db = Db::open_in_memory().unwrap();
        let s = load(&db).unwrap();
        assert_eq!(s.review, ReviewMode::Auto);
        assert!(s.workspace().ends_with("Elysium"));
        let s2 = Settings { workspace_dir: Some("  C:\\work  ".into()), review: ReviewMode::Off, ..Default::default() }.sanitized();
        save(&db, &s2).unwrap();
        let back = load(&db).unwrap();
        assert_eq!(back.workspace_dir.as_deref(), Some("C:\\work"));
        assert_eq!(back.review, ReviewMode::Off);
        // 빈 문자열은 기본 폴더로
        assert!(Settings { workspace_dir: Some("   ".into()), ..Default::default() }.sanitized().workspace_dir.is_none());
        // 예전 설정의 동시 에이전트 수(agents·agentsAuto)는 읽기만 하고 버린다: 이제 AI가 요청마다 정한다.
        let old: Settings = serde_json::from_str(r#"{"review":"auto","jevEnabled":false,"models":{},"agents":4,"agentsAuto":false}"#).unwrap();
        assert_eq!(old.test_slots, 1, "예전 설정의 테스트 동시 실행은 1개");
        assert!(old.lead_commit, "예전 설정도 총괄 커밋을 켠 채로 시작");
        assert_eq!(Settings { test_slots: 0, ..Default::default() }.sanitized().test_slots, 1);
        assert_eq!(Settings { test_slots: 99, ..Default::default() }.sanitized().test_slots, 8);
        let mut models = BTreeMap::new();
        models.insert("opencode".to_string(), "anthropic/claude-opus-5-5".to_string());
        assert!(Settings { models, ..Default::default() }.sanitized().models.is_empty(), "유료 모델은 저장하지 않는다");
        assert!(old.notify_done && old.prompts.is_empty() && old.answer_lang == "auto", "예전 설정은 새 항목의 기본값으로");
    }

    #[test]
    fn answer_prefs_are_cleaned_and_rendered() {
        let s = Settings {
            answer_lang: "xx".into(),
            answer_length: "brief".into(),
            instructions: format!("  저는 백엔드 개발자입니다 {}", "가".repeat(INSTRUCTIONS_MAX)),
            prompts: vec![SavedPrompt { title: " ".into(), text: " 이 코드를 리뷰해 줘 ".into() }, SavedPrompt { title: "빈 글".into(), text: "  ".into() }],
            ..Default::default()
        }
        .sanitized();
        assert_eq!(s.answer_lang, "auto", "모르는 언어는 자동");
        assert_eq!(s.instructions.chars().count(), INSTRUCTIONS_MAX);
        assert_eq!(s.prompts, vec![SavedPrompt { title: "이 코드를 리뷰해 줘".into(), text: "이 코드를 리뷰해 줘".into() }], "빈 프롬프트는 버리고 제목이 없으면 본문 앞부분");
        let p = Prefs { lang: "en".into(), length: "brief".into(), instructions: "백엔드 개발자".into() };
        assert!(p.lang_line().contains("English"));
        assert!(p.tail().contains("짧게") && p.tail().contains("백엔드 개발자"));
        assert!(Prefs { lang: "auto".into(), length: "auto".into(), instructions: " ".into() }.tail().is_empty(), "고른 것이 없으면 아무것도 붙이지 않는다");
    }
}
