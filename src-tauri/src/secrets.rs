//! API 키는 Windows 자격 증명 관리자에 보관한다(없으면 환경 변수를 읽는다).
//! 값은 화면·로그·프롬프트로 절대 돌려주지 않고 있는지 없는지만 알려 준다.

use crate::error::{AppError, AppResult};
use serde::Serialize;

const SERVICE: &str = "com.hoya.elysium";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretName {
    TypeSafe,
    Anthropic,
    OpenAi,
    Gemini,
    Nvidia,
}

pub const ALL: [SecretName; 5] = [SecretName::TypeSafe, SecretName::Anthropic, SecretName::OpenAi, SecretName::Gemini, SecretName::Nvidia];

impl SecretName {
    pub fn parse(s: &str) -> AppResult<Self> {
        ALL.into_iter().find(|n| n.account() == s).ok_or_else(|| AppError::msg(format!("알 수 없는 키 이름: {s}")))
    }
    pub fn account(self) -> &'static str {
        match self {
            Self::TypeSafe => "typesafe",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Nvidia => "nvidia",
        }
    }
    fn env_vars(self) -> &'static [&'static str] {
        match self {
            Self::TypeSafe => &["TYPESAFE_API_KEY"],
            Self::Anthropic => &["ANTHROPIC_API_KEY"],
            Self::OpenAi => &["OPENAI_API_KEY"],
            Self::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            Self::Nvidia => &["NVIDIA_API_KEY"],
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub name: String,
    pub present: bool,
    /// "keyring" | "env" | "none"
    pub source: String,
}

fn entry(name: SecretName) -> AppResult<keyring::Entry> {
    keyring::Entry::new(SERVICE, name.account()).map_err(|e| AppError::msg(format!("자격 증명 저장소 오류: {e}")))
}

fn from_keyring(name: SecretName) -> Option<String> {
    entry(name).ok()?.get_password().ok().filter(|v| !v.is_empty())
}

fn from_env(name: SecretName) -> Option<String> {
    name.env_vars().iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
}

/// 자격 증명 관리자 → 환경 변수 순서로 찾는다.
pub fn get(name: SecretName) -> Option<String> {
    from_keyring(name).or_else(|| from_env(name))
}

/// 붙여 넣을 때 딸려 오는 따옴표·"Bearer "·공백을 떼어 낸다.
pub fn clean(value: &str) -> &str {
    let v = value.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`').trim();
    let v = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")).unwrap_or(v);
    v.trim()
}

pub fn set(name: SecretName, value: &str) -> AppResult<()> {
    let value = clean(value);
    if value.is_empty() {
        return Err(AppError::msg("빈 값은 저장할 수 없습니다"));
    }
    entry(name)?.set_password(value).map_err(|e| AppError::msg(format!("자격 증명 저장 실패: {e}")))
}

pub fn clear(name: SecretName) -> AppResult<()> {
    match entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::msg(format!("자격 증명 삭제 실패: {e}"))),
    }
}

/// 키 값을 드러내지 않고 형식만 점검한다(길이·접두어·공백·ASCII 여부).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretShape {
    pub present: bool,
    pub source: String,
    pub length: usize,
    pub prefix_ok: bool,
    pub has_whitespace: bool,
    pub non_ascii: bool,
}

pub fn shape(name: SecretName) -> SecretShape {
    let (v, source) = match from_keyring(name) {
        Some(v) => (Some(v), "keyring"),
        None => match from_env(name) {
            Some(v) => (Some(v), "env"),
            None => (None, "none"),
        },
    };
    let v = v.unwrap_or_default();
    let prefix = match name {
        SecretName::Nvidia => "nvapi-",
        SecretName::Anthropic => "sk-ant-",
        SecretName::OpenAi => "sk-",
        _ => "",
    };
    SecretShape {
        present: !v.is_empty(),
        source: source.into(),
        length: v.chars().count(),
        prefix_ok: v.starts_with(prefix),
        has_whitespace: v.chars().any(char::is_whitespace),
        non_ascii: !v.is_ascii(),
    }
}

pub fn status() -> Vec<SecretStatus> {
    ALL.into_iter()
        .map(|n| {
            let k = from_keyring(n).is_some();
            let e = from_env(n).is_some();
            SecretStatus { name: n.account().into(), present: k || e, source: if k { "keyring" } else if e { "env" } else { "none" }.into() }
        })
        .collect()
}

/// 오류 메시지에 키 값이 섞여 나가지 않도록 가린다.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for n in ALL {
        if let Some(v) = get(n).filter(|v| v.len() >= 8) {
            out = out.replace(&v, "[REDACTED]");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_keys_are_cleaned() {
        assert_eq!(clean("  \"nvapi-abc\" "), "nvapi-abc");
        assert_eq!(clean("Bearer nvapi-abc\n"), "nvapi-abc");
        assert_eq!(clean("sk-xyz"), "sk-xyz");
    }

    #[test]
    fn names_round_trip() {
        for n in ALL {
            assert_eq!(SecretName::parse(n.account()).unwrap(), n);
        }
        assert!(SecretName::parse("x").is_err());
    }
}
