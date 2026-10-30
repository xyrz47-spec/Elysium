//! 토큰 단가 및 비용 계산 모듈: CLI와 API의 모델별 토큰 단가표(1M 토큰당 USD)와 비용 추정.
//!
//! 단가 기준 (2026년 기준 공식/표준 단가, USD / 1M 토큰):
//! - Anthropic Claude:
//!   - Sonnet (3.5 / 3.7 / 5.5): 입력 $3.00, 출력 $15.00
//!   - Haiku (3.5 / 5.5): 입력 $0.80, 출력 $4.00
//!   - Opus (3.0 / 5.5): 입력 $15.00, 출력 $75.00
//! - OpenAI / Codex:
//!   - GPT-4o / GPT-5: 입력 $2.50, 출력 $10.00
//!   - GPT-4o-mini: 입력 $0.15, 출력 $0.60
//!   - o1 / o1-preview: 입력 $15.00, 출력 $60.00
//!   - o3-mini: 입력 $1.10, 출력 $4.40
//!   - GPT-OSS 120B: 입력 $0.15, 출력 $0.60
//! - Google Gemini / Antigravity:
//!   - Flash (2.0 / 2.5 / 3.8 / 1.5): 입력 $0.10, 출력 $0.40
//!   - Pro / Argon (1.5 / 2.5 / 4.0): 입력 $1.25, 출력 $5.00
//! - NVIDIA:
//!   - Nemotron / 무료 빌드 모델: $0.00 / $0.00
//!   - DeepSeek v4.1: 입력 $0.14, 출력 $0.28 (자체 API 기준, 빌드 체험 시 $0.00)
//! - TypeSafe JEV:
//!   - System One 판단: 입력 $0.042 / 출력 $0.00

use crate::engines::EngineId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    pub engine: String,
    pub engine_label: String,
    pub kind: String,
    pub model: String,
    pub display_name: String,
    /// USD per 1M input tokens
    pub input_usd_per_m: f64,
    /// USD per 1M output tokens
    pub output_usd_per_m: f64,
    pub description: String,
}

pub const ALL_PRICES: &[(&str, &str, &str, &str, &str, f64, f64, &str)] = &[
    // CLI 엔진
    ("claude", "Claude Code", "cli", "sonnet", "Claude Sonnet (3.5/3.7/5.5)", 3.00, 15.00, "Claude Code 기본 모델"),
    ("claude", "Claude Code", "cli", "haiku", "Claude Haiku (3.5/5.5)", 0.80, 4.00, "경량 고속 모델"),
    ("claude", "Claude Code", "cli", "opus", "Claude Opus (3/5.5)", 15.00, 75.00, "최고 성능 복잡 추론 모델"),
    ("codex", "Codex", "cli", "gpt-5", "GPT-5 / GPT-4o", 2.50, 10.00, "Codex 기본 플래그십 코딩 모델"),
    ("codex", "Codex", "cli", "gpt-4o-mini", "GPT-4o Mini", 0.15, 0.60, "경량 코딩 및 보조 모델"),
    ("codex", "Codex", "cli", "o1", "o1 / o1-preview", 15.00, 60.00, "고난도 추론 모델"),
    ("codex", "Codex", "cli", "o3-mini", "o3-mini", 1.10, 4.40, "경량 심층 추론 모델"),
    ("antigravity", "Antigravity", "cli", "gemini-3.8-flash", "Gemini 3.8 / 2.5 Flash", 0.10, 0.40, "Antigravity 기본 고속 추론 모델"),
    ("antigravity", "Antigravity", "cli", "gemini-4-argon", "Gemini 4 Argon / Pro", 1.25, 5.00, "고성능 심층 작업 모델"),
    ("antigravity", "Antigravity", "cli", "claude-sonnet-5-5", "Claude Sonnet (Antigravity)", 3.00, 15.00, "Antigravity 경유 Claude 모델"),
    ("antigravity", "Antigravity", "cli", "claude-opus-5-5", "Claude Opus (Antigravity)", 15.00, 75.00, "Antigravity 경유 Opus 모델"),
    ("antigravity", "Antigravity", "cli", "gpt-oss-120b", "GPT-OSS 120B", 0.15, 0.60, "오픈소스 고성능 모델"),
    // API 엔진
    ("claude-api", "Claude API", "api", "claude-sonnet-5-5", "Claude Sonnet (API)", 3.00, 15.00, "Claude API 기본 모델"),
    ("claude-api", "Claude API", "api", "claude-haiku-5-5", "Claude Haiku (API)", 0.80, 4.00, "빠르고 경제적인 API 모델"),
    ("claude-api", "Claude API", "api", "claude-opus-5-5", "Claude Opus (API)", 15.00, 75.00, "최고급 추론 API 모델"),
    ("openai", "OpenAI API", "api", "gpt-5", "GPT-5 / GPT-4o (API)", 2.50, 10.00, "OpenAI API 플래그십"),
    ("openai", "OpenAI API", "api", "gpt-4o-mini", "GPT-4o Mini (API)", 0.15, 0.60, "저비용 고효율 API 모델"),
    ("openai", "OpenAI API", "api", "o1", "o1 (API)", 15.00, 60.00, "OpenAI 추론 모델"),
    ("openai", "OpenAI API", "api", "o3-mini", "o3-mini (API)", 1.10, 4.40, "고성능 경량 추론 API 모델"),
    ("gemini-api", "Gemini API", "api", "gemini-2.5-flash", "Gemini 2.5 / 2.0 Flash (API)", 0.10, 0.40, "Google AI Studio 기본 고속 모델"),
    ("gemini-api", "Gemini API", "api", "gemini-2.5-pro", "Gemini 2.5 / 1.5 Pro (API)", 1.25, 5.00, "대규모 문맥 및 고급 추론 API"),
    ("nvidia", "NVIDIA API", "api", "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning", "Nemotron 3 Nano Omni", 0.00, 0.00, "NVIDIA Build 무료 체험 크레딧"),
    ("nvidia", "NVIDIA API", "api", "deepseek-ai/deepseek-v4.1-flash", "DeepSeek v4.1 Flash", 0.00, 0.00, "NVIDIA Build 경유 무료"),
    // JEV
    ("jev", "TypeSafe JEV", "jev", "jev-latest", "TypeSafe System One", 0.042, 0.00, "요청 분석·작업 분류·기억 선별 (입력만 과금)"),
];

pub fn all_prices() -> Vec<ModelPrice> {
    ALL_PRICES
        .iter()
        .map(|&(engine, engine_label, kind, model, display_name, input_usd_per_m, output_usd_per_m, description)| ModelPrice {
            engine: engine.into(),
            engine_label: engine_label.into(),
            kind: kind.into(),
            model: model.into(),
            display_name: display_name.into(),
            input_usd_per_m,
            output_usd_per_m,
            description: description.into(),
        })
        .collect()
}

/// 엔진과 모델 이름을 바탕으로 (입력 단가, 출력 단가) USD/1M을 돌려준다.
pub fn price_for(engine: EngineId, model: Option<&str>) -> (f64, f64) {
    let m = model.unwrap_or("").to_ascii_lowercase();
    match engine {
        EngineId::Claude | EngineId::ClaudeApi => {
            if m.contains("haiku") {
                (0.80, 4.00)
            } else if m.contains("opus") {
                (15.00, 75.00)
            } else {
                // Sonnet, Fable 등 기본 플래그십
                (3.00, 15.00)
            }
        }
        EngineId::Codex | EngineId::OpenAi => {
            if m.contains("mini") || m.contains("oss") {
                (0.15, 0.60)
            } else if m.contains("o1") {
                (15.00, 60.00)
            } else if m.contains("o3") {
                (1.10, 4.40)
            } else {
                // gpt-4o, gpt-5 등
                (2.50, 10.00)
            }
        }
        EngineId::Antigravity | EngineId::GeminiApi => {
            if m.contains("opus") {
                (15.00, 75.00)
            } else if m.contains("sonnet") {
                (3.00, 15.00)
            } else if m.contains("pro") || m.contains("argon") {
                (1.25, 5.00)
            } else {
                // gemini-3.8-flash, gemini-2.5-flash, gemini-2.0-flash 등
                (0.10, 0.40)
            }
        }
        EngineId::Nvidia => {
            if m.contains("deepseek") {
                (0.00, 0.00) // NVIDIA Build 체험
            } else {
                (0.00, 0.00)
            }
        }
    }
}

/// 문자열 기반 엔진 식별자로 단가를 구한다(과거 사용량 집계용).
pub fn price_for_str(engine: &str, model: Option<&str>) -> (f64, f64) {
    if engine == "jev" {
        return (0.042, 0.00);
    }
    match EngineId::parse(engine) {
        Some(id) => price_for(id, model),
        None => (0.0, 0.0),
    }
}

/// 입력/출력 토큰 수와 모델로부터 추정 비용(USD)을 계산한다.
pub fn estimate_cost(
    engine: EngineId,
    model: Option<&str>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Option<f64> {
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    let (inp_price, out_price) = price_for(engine, model);
    let inp = input_tokens.unwrap_or(0) as f64;
    let out = output_tokens.unwrap_or(0) as f64;
    Some((inp * inp_price + out * out_price) / 1_000_000.0)
}

/// 문자열 기반 엔진 식별자로 비용을 계산한다.
pub fn estimate_cost_by_str(
    engine: &str,
    model: Option<&str>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Option<f64> {
    if engine == "jev" {
        return input_tokens.map(|i| (i as f64 * 0.042) / 1_000_000.0);
    }
    let id = EngineId::parse(engine)?;
    estimate_cost(id, model, input_tokens, output_tokens)
}

/// 캐시를 따로 셈: 캐시 읽기는 입력 단가의 10%, 캐시 쓰기는 125%(Anthropic 5분 캐시 기준이며 다른 공급자는 근사치).
/// 캐시에서 읽은 입력은 u.input에 넣지 않는다.
pub fn estimate_cost_cached(engine: EngineId, model: Option<&str>, u: crate::apis::Usage) -> f64 {
    let (inp_price, out_price) = price_for(engine, model);
    let read = u.cache_read as f64 * inp_price * 0.10;
    let write = u.cache_write as f64 * inp_price * 1.25;
    (u.input as f64 * inp_price + read + write + u.output as f64 * out_price) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_input_is_billed_separately() {
        // 캐시 읽기 100만 토큰 = 입력 단가의 10%: Sonnet 기준 $0.30
        let u = crate::apis::Usage { input: 0, output: 0, cache_read: 1_000_000, cache_write: 0 };
        assert!((estimate_cost_cached(EngineId::ClaudeApi, Some("claude-sonnet-5-5"), u) - 0.30).abs() < 1e-6);
        let w = crate::apis::Usage { input: 0, output: 0, cache_read: 0, cache_write: 1_000_000 };
        assert!((estimate_cost_cached(EngineId::ClaudeApi, Some("claude-sonnet-5-5"), w) - 3.75).abs() < 1e-6);
    }

    #[test]
    fn test_pricing_lookup() {
        assert_eq!(price_for(EngineId::Claude, Some("haiku")), (0.80, 4.00));
        assert_eq!(price_for(EngineId::ClaudeApi, Some("claude-sonnet-5-5")), (3.00, 15.00));
        assert_eq!(price_for(EngineId::Claude, Some("opus")), (15.00, 75.00));
        assert_eq!(price_for(EngineId::Codex, Some("gpt-5")), (2.50, 10.00));
        assert_eq!(price_for(EngineId::OpenAi, Some("gpt-4o-mini")), (0.15, 0.60));
        assert_eq!(price_for(EngineId::Antigravity, Some("gemini-3.8-flash")), (0.10, 0.40));
        assert_eq!(price_for(EngineId::GeminiApi, Some("gemini-2.5-pro")), (1.25, 5.00));
        assert_eq!(price_for(EngineId::Nvidia, None), (0.0, 0.0));
    }

    #[test]
    fn test_cost_calculation() {
        // 100만 토큰 입력, 10만 토큰 출력 (Claude Sonnet: $3.00 + $1.50 = $4.50)
        let cost = estimate_cost(EngineId::ClaudeApi, Some("sonnet"), Some(1_000_000), Some(100_000)).unwrap();
        assert!((cost - 4.50).abs() < 1e-6);

        // Antigravity Flash: 10,000 입력, 2,000 출력 ($0.001 + $0.0008 = $0.0018)
        let cost_agy = estimate_cost(EngineId::Antigravity, Some("gemini-3.8-flash"), Some(10_000), Some(2_000)).unwrap();
        assert!((cost_agy - 0.0018).abs() < 1e-6);

        // JEV: 10,000 입력 -> $0.00042
        let cost_jev = estimate_cost_by_str("jev", None, Some(10_000), None).unwrap();
        assert!((cost_jev - 0.00042).abs() < 1e-6);
    }
}
