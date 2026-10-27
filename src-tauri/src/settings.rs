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
    /// TypeSafe Jev로 요청 종류·난이도를 판단해 AI와 검토 여부를 고른다(요청 글이 TypeSafe로 전송됨).
    pub jev_enabled: bool,
    /// API 엔진별 모델 이름(engine id → model). 없으면 기본 모델.
    pub models: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { workspace_dir: None, review: ReviewMode::Auto, jev_enabled: false, models: BTreeMap::new() }
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
    }
}
