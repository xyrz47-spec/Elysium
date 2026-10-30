//! TypeSafe Jev(System One) 어댑터.
//! 계약 출처: https://docs.typesafe.ai/api (2026-10-16 확인) — POST https://api.typesafe.ai/v1/systemone,
//! `Authorization: Bearer`, 바디 {model, state, questions}, 응답 {model, answers, usage{input_tokens, output_tokens}}.
//! Jev는 텍스트를 생성하지 않고 타입이 지정된 판단(choice/noul/score)과 확률을 돌려준다.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: HashMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        confidence: f64,
        #[serde(default)]
        probabilities: HashMap<String, f64>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Response {
    pub model: String,
    pub answers: HashMap<String, Answer>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JevErrorKind {
    Auth,
    RateLimit,
    Overloaded,
    Timeout,
    TooLarge,
    BadRequest,
    Network,
    Server,
    Decode,
}

#[derive(Debug, Clone, Serialize)]
pub struct JevError {
    pub kind: JevErrorKind,
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for JevError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Jev {:?}: {}", self.kind, self.message)
    }
}

pub struct CallResult {
    pub response: Response,
    pub latency_ms: u64,
}

pub fn choice(answers: &HashMap<String, Answer>, key: &str) -> Option<(String, f64)> {
    match answers.get(key)? {
        Answer::Choice { choice, confidence, .. } => Some((choice.clone(), *confidence)),
        _ => None,
    }
}

/// choice 답에서 특정 선택지의 확률. 확률표에 없으면 고른 선택지와 확신도로 어림한다(두 갈래 질문용).
pub fn prob(answers: &HashMap<String, Answer>, key: &str, label: &str) -> Option<f64> {
    match answers.get(key)? {
        Answer::Choice { choice, probabilities, confidence } => Some(match probabilities.get(label) {
            Some(p) => *p,
            None if choice == label => *confidence,
            None => 1.0 - *confidence,
        }),
        _ => None,
    }
}

pub fn score(answers: &HashMap<String, Answer>, key: &str) -> Option<(f64, f64)> {
    match answers.get(key)? {
        Answer::Score { score, confidence, .. } => Some((*score, *confidence)),
        _ => None,
    }
}

/// 질문 묶음을 한 번의 요청으로 보낸다(병렬 평가). 429/529/5xx·네트워크 오류만 제한된 횟수로 재시도한다.
pub async fn ask(
    http: &reqwest::Client,
    api_key: &str,
    model: &str,
    state: Value,
    questions: Map<String, Value>,
    max_retries: u32,
    timeout: Duration,
) -> Result<CallResult, JevError> {
    let body = serde_json::json!({ "model": model, "state": state, "questions": questions });
    let started = Instant::now();
    let mut attempt = 0u32;
    loop {
        let res = http
            .post(ENDPOINT)
            .bearer_auth(api_key)
            .json(&body)
            .timeout(timeout)
            .send()
            .await;
        let err = match res {
            Ok(r) => {
                let status = r.status().as_u16();
                if r.status().is_success() {
                    return match r.json::<Response>().await {
                        Ok(response) => Ok(CallResult { response, latency_ms: started.elapsed().as_millis() as u64 }),
                        Err(e) => Err(JevError { kind: JevErrorKind::Decode, status: Some(status), message: e.to_string() }),
                    };
                }
                let text = r.text().await.unwrap_or_default();
                let kind = match status {
                    401 | 403 => JevErrorKind::Auth,
                    413 => JevErrorKind::TooLarge,
                    429 => JevErrorKind::RateLimit,
                    529 => JevErrorKind::Overloaded,
                    400 | 415 | 422 => JevErrorKind::BadRequest,
                    _ => JevErrorKind::Server,
                };
                JevError { kind, status: Some(status), message: truncate(&text, 300) }
            }
            Err(e) if e.is_timeout() => JevError { kind: JevErrorKind::Timeout, status: None, message: "응답 시간 초과".into() },
            Err(e) => JevError { kind: JevErrorKind::Network, status: None, message: truncate(&e.to_string(), 300) },
        };
        let retryable = matches!(err.kind, JevErrorKind::RateLimit | JevErrorKind::Overloaded | JevErrorKind::Server | JevErrorKind::Network);
        if !retryable || attempt >= max_retries {
            return Err(err);
        }
        attempt += 1;
        tokio::time::sleep(Duration::from_millis(400 * 2u64.pow(attempt))).await;
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 실제 호출(2026-10-16)에서 받은 응답 형태로 파싱을 검증한다.
    #[test]
    fn parses_live_response_shape() {
        let raw = r#"{"model":"jev-1.13.0","answers":{"tool":{"type":"choice","choice":"file.search","confidence":1.0,"probabilities":{"time.now":0.0,"file.search":1.0}},"difficulty":{"type":"score","score":0.18,"confidence":0.84,"legend":{"0":"a","1":"b"},"probabilities":{"0":0.85,"1":0.14}},"sensitive":{"type":"noul","noul":0.18}},"usage":{"input_tokens":540,"output_tokens":96}}"#;
        let r: Response = serde_json::from_str(raw).unwrap();
        assert_eq!(r.model, "jev-1.13.0");
        assert_eq!(choice(&r.answers, "tool"), Some(("file.search".into(), 1.0)));
        assert_eq!(score(&r.answers, "difficulty"), Some((0.18, 0.84)));
        assert!(matches!(r.answers["sensitive"], Answer::Noul { noul } if (noul - 0.18).abs() < 1e-9));
        assert_eq!(r.usage.input_tokens, 540);
    }

    #[test]
    fn prob_reads_probabilities_or_falls_back_to_confidence() {
        let raw = r#"{"model":"jev","answers":{"a":{"type":"choice","choice":"skip","confidence":0.8,"probabilities":{"needed":0.2,"skip":0.8}},"b":{"type":"choice","choice":"needed","confidence":0.7,"probabilities":{}},"c":{"type":"score","score":1.0,"confidence":0.9}}}"#;
        let r: Response = serde_json::from_str(raw).unwrap();
        assert_eq!(prob(&r.answers, "a", "needed"), Some(0.2));
        assert_eq!(prob(&r.answers, "b", "needed"), Some(0.7));
        assert!((prob(&r.answers, "b", "skip").unwrap() - 0.3).abs() < 1e-9);
        assert_eq!(prob(&r.answers, "c", "needed"), None, "choice가 아니면 없음");
        assert_eq!(prob(&r.answers, "zz", "needed"), None);
    }

    #[test]
    fn unknown_answer_types_do_not_break_parsing() {
        let raw = r#"{"model":"jev-x","answers":{"b":{"type":"box","left":0.1}},"usage":{"input_tokens":1,"output_tokens":0}}"#;
        let r: Response = serde_json::from_str(raw).unwrap();
        assert!(matches!(r.answers["b"], Answer::Unknown));
    }
}
