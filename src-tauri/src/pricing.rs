//! 토큰 단가 및 비용 계산 모듈: CLI와 API의 모델별 토큰 단가표(1M 토큰당 USD)와 비용 추정.
//!
//! 출처(2026-10-31 확인, 표준 요금):
//! - Anthropic: https://platform.claude.com/docs/en/about-claude/pricing
//!   Fable 5.1 $10/$50(캐시 읽기 0.025배) · Opus 5.5 $4/$20(0.05배) · Sonnet 5.5 $2/$10(0.05배)
//!   Haiku 5.5 $0.10/$0.50(프롬프트 10만 토큰 초과 시 $0.50/$2.50, 0.1배) · 캐시 쓰기(5분) 1.25배
//! - OpenAI: https://developers.openai.com/api/docs/pricing (캐시 입력 0.1배, 272K 초과 장문 요금)
//! - Google: https://ai.google.dev/gemini-api/docs/pricing (3.8 Flash·4 Argon은 기간 한정 할인가)
//! - NVIDIA Build: 무료 체험 크레딧 · TypeSafe Jev: 입력 $0.042, 출력 무료

use crate::engines::EngineId;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// 한 모델의 단가. 캐시는 입력 단가에 곱하는 배수다.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    /// (프롬프트 토큰 기준선, 입력, 출력): 기준선을 넘는 요청은 이 단가로 계산
    pub long: Option<(u64, f64, f64)>,
}

impl Rate {
    const fn new(input: f64, output: f64, cache_read: f64) -> Rate {
        Rate { input, output, cache_read, cache_write: 1.0, long: None }
    }
    const fn anthropic(input: f64, output: f64, cache_read: f64) -> Rate {
        Rate { input, output, cache_read, cache_write: 1.25, long: None }
    }
    const fn long(mut self, over: u64, input: f64, output: f64) -> Rate {
        self.long = Some((over, input, output));
        self
    }
    /// 이 프롬프트 크기에 적용되는 (입력, 출력) 단가
    pub fn at(&self, prompt_tokens: u64) -> (f64, f64) {
        match self.long {
            Some((over, i, o)) if prompt_tokens > over => (i, o),
            _ => (self.input, self.output),
        }
    }
}

const FREE: Rate = Rate::new(0.0, 0.0, 0.0);

// Anthropic
const FABLE: Rate = Rate::anthropic(10.0, 50.0, 0.025);
const OPUS_55: Rate = Rate::anthropic(4.0, 20.0, 0.05);
const OPUS_OLD: Rate = Rate::anthropic(5.0, 25.0, 0.1);
const SONNET_55: Rate = Rate::anthropic(2.0, 10.0, 0.05);
const SONNET_5: Rate = Rate::anthropic(2.0, 10.0, 0.1);
const SONNET_4: Rate = Rate::anthropic(3.0, 15.0, 0.1);
const HAIKU_55: Rate = Rate::anthropic(0.10, 0.50, 0.1).long(100_000, 0.50, 2.50);
const HAIKU_45: Rate = Rate::anthropic(1.0, 5.0, 0.1);

// OpenAI
const GPT_56_SOL: Rate = Rate::new(4.0, 20.0, 0.1).long(272_000, 8.0, 30.0);
const GPT_56_TERRA: Rate = Rate::new(2.0, 12.0, 0.1).long(272_000, 4.0, 18.0);
const GPT_56_LUNA: Rate = Rate::new(0.20, 1.20, 0.1).long(272_000, 0.40, 1.80);
const GPT_56_CYBER: Rate = Rate::new(12.5, 75.0, 0.1);
const GPT_55: Rate = Rate::new(5.0, 30.0, 0.1).long(272_000, 10.0, 45.0);
const GPT_54: Rate = Rate::new(2.50, 15.0, 0.1).long(272_000, 5.0, 22.50);
const GPT_54_MINI: Rate = Rate::new(0.75, 4.50, 0.1);
const GPT_53_CODEX: Rate = Rate::new(1.75, 14.0, 0.1);
const GPT_5: Rate = Rate::new(1.25, 10.0, 0.1);
const GPT_5_MINI: Rate = Rate::new(0.25, 2.0, 0.1);
const GPT_5_NANO: Rate = Rate::new(0.05, 0.40, 0.1);
const GPT_4O: Rate = Rate::new(2.50, 10.0, 0.5);
const GPT_4O_MINI: Rate = Rate::new(0.15, 0.60, 0.5);
const O4_MINI: Rate = Rate::new(1.10, 4.40, 0.25);
const O3: Rate = Rate::new(2.0, 8.0, 0.25);
const O3_MINI: Rate = Rate::new(1.10, 4.40, 0.5);
const O1: Rate = Rate::new(15.0, 60.0, 0.5);
// 공식 표에 없어 호스팅 업체 평균가로 둔 값
const GPT_OSS: Rate = Rate::new(0.15, 0.60, 0.1);

// Google
const GEMINI_31_PRO: Rate = Rate::new(2.0, 12.0, 0.1).long(200_000, 4.0, 18.0);
const GEMINI_25_PRO: Rate = Rate::new(1.25, 10.0, 0.1).long(200_000, 2.50, 15.0);
const GEMINI_3_FLASH: Rate = Rate::new(0.50, 3.0, 0.1);
const GEMINI_25_FLASH: Rate = Rate::new(0.30, 2.50, 0.1);
const GEMINI_35_LITE: Rate = Rate::new(0.30, 2.50, 0.1);
const GEMINI_31_LITE: Rate = Rate::new(0.25, 1.50, 0.1);
const GEMINI_25_LITE: Rate = Rate::new(0.10, 0.40, 0.1);

/// 할인가가 끝나는 날(그 날까지 할인가, 다음 날부터 정가)
const ARGON_PROMO_LAST: (i32, u32, u32) = (2026, 11, 30);
const FLASH38_PROMO_LAST: (i32, u32, u32) = (2026, 12, 31);

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn until(last: (i32, u32, u32), on: NaiveDate) -> bool {
    NaiveDate::from_ymd_opt(last.0, last.1, last.2).is_some_and(|d| on <= d)
}

fn argon(on: NaiveDate) -> Rate {
    if until(ARGON_PROMO_LAST, on) { Rate::new(2.0, 10.0, 0.05) } else { Rate::new(4.0, 20.0, 0.05) }
}

fn flash38(on: NaiveDate) -> Rate {
    if until(FLASH38_PROMO_LAST, on) { Rate::new(0.75, 3.75, 0.1) } else { Rate::new(1.50, 7.50, 0.1) }
}

/// Claude 모델 이름 또는 Claude Code 별칭(haiku·sonnet·opus). 비어 있으면 Claude Code 기본 모델(Opus 5.5)로 본다.
fn claude(m: &str) -> Rate {
    if m.contains("fable") || m.contains("mythos") {
        FABLE
    } else if m.contains("haiku") {
        if m.contains("haiku-4") || m.contains("haiku-3") { HAIKU_45 } else { HAIKU_55 }
    } else if m.contains("sonnet") {
        if m.contains("sonnet-5-5") || m.contains("sonnet-5.5") || !m.contains('-') {
            SONNET_55
        } else if m.contains("sonnet-5") {
            SONNET_5
        } else {
            SONNET_4
        }
    } else if m.contains("opus") {
        if m.contains("opus-5-5") || m.contains("opus-5.5") || !m.contains('-') { OPUS_55 } else { OPUS_OLD }
    } else {
        OPUS_55
    }
}

/// 긴 이름부터 맞춘다(gpt-5.6-luna가 gpt-5에 걸리지 않게).
fn openai(m: &str) -> Rate {
    const TABLE: &[(&str, Rate)] = &[
        ("gpt-5.6-sol", GPT_56_SOL),
        ("gpt-5.6-terra", GPT_56_TERRA),
        ("gpt-5.6-luna", GPT_56_LUNA),
        ("gpt-5.6-cyber", GPT_56_CYBER),
        ("gpt-5.5", GPT_55),
        ("gpt-5.4-mini", GPT_54_MINI),
        ("gpt-5.4", GPT_54),
        ("gpt-5.3-codex", GPT_53_CODEX),
        ("gpt-5-nano", GPT_5_NANO),
        ("gpt-5-mini", GPT_5_MINI),
        ("gpt-4o-mini", GPT_4O_MINI),
        ("gpt-4o", GPT_4O),
        ("o4-mini", O4_MINI),
        ("o3-mini", O3_MINI),
        ("o3", O3),
        ("o1", O1),
        ("oss", GPT_OSS),
    ];
    TABLE.iter().find(|(k, _)| m.contains(k)).map(|(_, r)| *r).unwrap_or(GPT_5)
}

fn gemini(m: &str, on: NaiveDate) -> Rate {
    if m.contains("claude") || m.contains("opus") || m.contains("sonnet") {
        return claude(m);
    }
    if m.contains("oss") {
        return GPT_OSS;
    }
    if m.contains("argon") {
        argon(on)
    } else if m.contains("3.5-flash-lite") {
        GEMINI_35_LITE
    } else if m.contains("3.1-flash-lite") {
        GEMINI_31_LITE
    } else if m.contains("flash-lite") {
        GEMINI_25_LITE
    } else if m.contains("3.1-pro") || m.contains("3-pro") {
        GEMINI_31_PRO
    } else if m.contains("pro") {
        GEMINI_25_PRO
    } else if m.contains("2.5-flash") {
        GEMINI_25_FLASH
    } else if m.contains("gemini-3-flash") {
        GEMINI_3_FLASH
    } else {
        // gemini-3.8-flash·3.6-flash, 그리고 Antigravity 기본 모델
        flash38(on)
    }
}

pub fn rate_on(engine: EngineId, model: Option<&str>, on: NaiveDate) -> Rate {
    let m = model.unwrap_or("").to_ascii_lowercase();
    match engine {
        EngineId::Claude | EngineId::ClaudeApi => claude(&m),
        EngineId::Codex | EngineId::OpenAi => openai(&m),
        EngineId::Antigravity | EngineId::GeminiApi => gemini(&m, on),
        EngineId::Nvidia => FREE,
    }
}

pub fn rate_for(engine: EngineId, model: Option<&str>) -> Rate {
    rate_on(engine, model, today())
}

/// JEV 단가(USD / 1M 입력 토큰). 출력은 무료다.
pub const JEV_INPUT: f64 = 0.042;

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
    /// USD per 1M cached input tokens(캐시 읽기)
    pub cached_usd_per_m: f64,
    pub description: String,
}

/// 단가표에 보여 줄 모델(엔진, 표시 이름, 모델, 설명). 값은 rate_for에서 가져와 계산과 화면이 어긋나지 않는다.
const LISTED: &[(EngineId, &str, &str, &str)] = &[
    (EngineId::Claude, "Claude Haiku 5.5", "haiku", "가볍게 단계 · 10만 토큰 초과 프롬프트는 $0.50/$2.50"),
    (EngineId::Claude, "Claude Sonnet 5.5", "sonnet", "보통 단계"),
    (EngineId::Claude, "Claude Opus 5.5", "opus", "깊게 단계 · Claude Code 기본 모델"),
    (EngineId::Claude, "Claude Fable 5.1", "fable", "가장 강한 모델"),
    (EngineId::Codex, "GPT-5.6 Sol", "gpt-5.6-sol", "할인가(2026-11-21까지 유지 예정)"),
    (EngineId::Codex, "GPT-5.5", "gpt-5.5", "OpenAI 플래그십"),
    (EngineId::Codex, "GPT-5.3 Codex", "gpt-5.3-codex", "코딩 특화"),
    (EngineId::Codex, "GPT-5", "gpt-5", "Codex 모델을 모를 때의 기본 추정값"),
    (EngineId::Antigravity, "Gemini 3.8 Flash", "gemini-3.8-flash", "2026-12-31까지 할인가, 이후 $1.50/$7.50"),
    (EngineId::Antigravity, "Gemini 4 Argon", "gemini-4-argon", "2026-11-30까지 할인가, 이후 $4/$20"),
    (EngineId::Antigravity, "Claude Opus 5.5 (Antigravity)", "claude-opus-5-5", "Antigravity 경유 Claude"),
    (EngineId::ClaudeApi, "Claude Sonnet 5.5 (API)", "claude-sonnet-5-5", "Claude API 기본 모델"),
    (EngineId::ClaudeApi, "Claude Haiku 5.5 (API)", "claude-haiku-5-5", "가장 싼 Claude"),
    (EngineId::ClaudeApi, "Claude Opus 5.5 (API)", "claude-opus-5-5", "Opus 5.5"),
    (EngineId::OpenAi, "GPT-5 (API)", "gpt-5", "OpenAI API 기본 모델"),
    (EngineId::OpenAi, "GPT-5 nano (API)", "gpt-5-nano", "가장 싼 OpenAI 모델"),
    (EngineId::OpenAi, "GPT-5.6 Luna (API)", "gpt-5.6-luna", "저가 최신 모델"),
    (EngineId::GeminiApi, "Gemini 2.5 Flash (API)", "gemini-2.5-flash", "Gemini API 기본 모델"),
    (EngineId::GeminiApi, "Gemini 2.5 Flash-Lite (API)", "gemini-2.5-flash-lite", "가장 싼 Gemini"),
    (EngineId::GeminiApi, "Gemini 3.8 Flash (API)", "gemini-3.8-flash", "2026-12-31까지 할인가"),
    (EngineId::Nvidia, "Nemotron 3 Nano Omni", "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning", "NVIDIA Build 무료 체험 크레딧"),
    (EngineId::Nvidia, "DeepSeek v4.1 Flash", "deepseek-ai/deepseek-v4.1-flash", "NVIDIA Build 무료 체험 크레딧"),
];

pub fn all_prices() -> Vec<ModelPrice> {
    let mut out: Vec<ModelPrice> = LISTED
        .iter()
        .map(|&(engine, display_name, model, description)| {
            let r = rate_for(engine, Some(model));
            ModelPrice {
                engine: engine.as_str().into(),
                engine_label: engine.label().into(),
                kind: if engine.is_api() { "api" } else { "cli" }.into(),
                model: model.into(),
                display_name: display_name.into(),
                input_usd_per_m: r.input,
                output_usd_per_m: r.output,
                cached_usd_per_m: r.input * r.cache_read,
                description: description.into(),
            }
        })
        .collect();
    out.push(ModelPrice {
        engine: "jev".into(),
        engine_label: "TypeSafe JEV".into(),
        kind: "jev".into(),
        model: "jev-latest".into(),
        display_name: "TypeSafe System One".into(),
        input_usd_per_m: JEV_INPUT,
        output_usd_per_m: 0.0,
        cached_usd_per_m: JEV_INPUT,
        description: "요청 판단·기억 선별·답 비교·검토 생략 판단 (입력만 과금)".into(),
    });
    out
}

/// 엔진과 모델 이름을 바탕으로 (입력 단가, 출력 단가) USD/1M을 돌려준다.
pub fn price_for(engine: EngineId, model: Option<&str>) -> (f64, f64) {
    let r = rate_for(engine, model);
    (r.input, r.output)
}

/// 같은 일을 시킬 때의 대략적인 비용 순위용 값(입력 3 : 출력 1 비율).
pub fn blended(engine: EngineId, model: Option<&str>) -> f64 {
    let (i, o) = price_for(engine, model);
    (3.0 * i + o) / 4.0
}

/// 입력/출력 토큰 수와 모델로부터 추정 비용(USD)을 계산한다.
pub fn estimate_cost(engine: EngineId, model: Option<&str>, input_tokens: Option<u64>, output_tokens: Option<u64>) -> Option<f64> {
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    let inp = input_tokens.unwrap_or(0);
    let (ip, op) = rate_for(engine, model).at(inp);
    Some((inp as f64 * ip + output_tokens.unwrap_or(0) as f64 * op) / 1_000_000.0)
}

/// 문자열 기반 엔진 식별자로 비용을 계산한다(과거 사용량 집계용).
pub fn estimate_cost_by_str(engine: &str, model: Option<&str>, input_tokens: Option<u64>, output_tokens: Option<u64>) -> Option<f64> {
    if engine == "jev" {
        return input_tokens.map(|i| i as f64 * JEV_INPUT / 1_000_000.0);
    }
    let id = EngineId::parse(engine)?;
    estimate_cost(id, model, input_tokens, output_tokens)
}

/// 캐시를 따로 셈: 캐시 읽기·쓰기 배수는 모델마다 다르다(Opus·Sonnet 5.5는 읽기 0.05배, Fable 5.1은 0.025배).
/// 캐시에서 읽은 입력은 u.input에 넣지 않는다. 장문 요금은 이 호출의 프롬프트 전체(입력+캐시) 크기로 정한다.
pub fn estimate_cost_cached(engine: EngineId, model: Option<&str>, u: crate::apis::Usage) -> f64 {
    let r = rate_for(engine, model);
    let (ip, op) = r.at(u.input + u.cache_read + u.cache_write);
    let read = u.cache_read as f64 * ip * r.cache_read;
    let write = u.cache_write as f64 * ip * r.cache_write;
    (u.input as f64 * ip + read + write + u.output as f64 * op) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn cached_input_uses_per_model_ratio() {
        // Sonnet 5.5 캐시 읽기 100만 토큰 = $2 × 0.05 = $0.10
        let u = crate::apis::Usage { input: 0, output: 0, cache_read: 1_000_000, cache_write: 0 };
        assert!((estimate_cost_cached(EngineId::ClaudeApi, Some("claude-sonnet-5-5"), u) - 0.10).abs() < 1e-9);
        // Haiku 5.5는 0.1배지만 100만 토큰 프롬프트라 장문 단가($0.50)로 계산: $0.05
        assert!((estimate_cost_cached(EngineId::ClaudeApi, Some("claude-haiku-5-5"), u) - 0.05).abs() < 1e-9);
        // 캐시 쓰기는 1.25배: Sonnet 5.5 $2.50
        let w = crate::apis::Usage { input: 0, output: 0, cache_read: 0, cache_write: 1_000_000 };
        assert!((estimate_cost_cached(EngineId::ClaudeApi, Some("claude-sonnet-5-5"), w) - 2.50).abs() < 1e-9);
    }

    #[test]
    fn official_rates_2026_10() {
        assert_eq!(price_for(EngineId::Claude, Some("haiku")), (0.10, 0.50));
        assert_eq!(price_for(EngineId::Claude, Some("sonnet")), (2.0, 10.0));
        assert_eq!(price_for(EngineId::Claude, Some("opus")), (4.0, 20.0));
        assert_eq!(price_for(EngineId::Claude, None), (4.0, 20.0), "Claude Code 기본 = Opus 5.5");
        assert_eq!(price_for(EngineId::ClaudeApi, Some("claude-fable-5-1")), (10.0, 50.0));
        assert_eq!(price_for(EngineId::ClaudeApi, Some("claude-opus-5")), (5.0, 25.0));
        assert_eq!(price_for(EngineId::ClaudeApi, Some("claude-sonnet-4-6")), (3.0, 15.0));
        assert_eq!(price_for(EngineId::OpenAi, Some("gpt-5")), (1.25, 10.0));
        assert_eq!(price_for(EngineId::OpenAi, Some("gpt-5-mini")), (0.25, 2.0));
        assert_eq!(price_for(EngineId::OpenAi, Some("gpt-5.6-luna")), (0.20, 1.20));
        assert_eq!(price_for(EngineId::Codex, Some("gpt-5.4-mini")), (0.75, 4.50));
        assert_eq!(price_for(EngineId::OpenAi, Some("gpt-4o-mini")), (0.15, 0.60));
        assert_eq!(price_for(EngineId::GeminiApi, Some("gemini-2.5-flash")), (0.30, 2.50));
        assert_eq!(price_for(EngineId::GeminiApi, Some("gemini-2.5-flash-lite")), (0.10, 0.40));
        assert_eq!(price_for(EngineId::GeminiApi, Some("gemini-2.5-pro")), (1.25, 10.0));
        assert_eq!(price_for(EngineId::Antigravity, Some("claude-opus-5-5")), (4.0, 20.0));
        assert_eq!(price_for(EngineId::Nvidia, None), (0.0, 0.0));
    }

    #[test]
    fn promo_prices_end_on_their_dates() {
        let r = |m: &str, on| {
            let x = rate_on(EngineId::Antigravity, Some(m), on);
            (x.input, x.output)
        };
        assert_eq!(r("gemini-4-argon", d(2026, 11, 30)), (2.0, 10.0));
        assert_eq!(r("gemini-4-argon", d(2026, 12, 1)), (4.0, 20.0));
        assert_eq!(r("gemini-3.8-flash", d(2026, 12, 31)), (0.75, 3.75));
        assert_eq!(r("gemini-3.8-flash", d(2027, 1, 1)), (1.50, 7.50));
        assert_eq!(r("", d(2026, 10, 31)), (0.75, 3.75), "Antigravity 기본 = 3.8 Flash");
    }

    #[test]
    fn long_context_tier_applies_above_threshold() {
        let r = rate_for(EngineId::OpenAi, Some("gpt-5.5"));
        assert_eq!(r.at(272_000), (5.0, 30.0));
        assert_eq!(r.at(272_001), (10.0, 45.0));
        // Haiku 5.5: 10만 토큰 초과 요청은 5배
        let c = estimate_cost(EngineId::ClaudeApi, Some("claude-haiku-5-5"), Some(200_000), Some(0)).unwrap();
        assert!((c - 0.10).abs() < 1e-9);
    }

    #[test]
    fn cost_calculation() {
        // Sonnet 5.5: 100만 입력 + 10만 출력 = $2.00 + $1.00
        let cost = estimate_cost(EngineId::ClaudeApi, Some("sonnet"), Some(1_000_000), Some(100_000)).unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
        let jev = estimate_cost_by_str("jev", None, Some(10_000), None).unwrap();
        assert!((jev - 0.00042).abs() < 1e-9);
    }

    #[test]
    fn listed_prices_match_calculation() {
        for p in all_prices().iter().filter(|p| p.kind != "jev") {
            let e = EngineId::parse(&p.engine).unwrap();
            assert_eq!(price_for(e, Some(&p.model)), (p.input_usd_per_m, p.output_usd_per_m), "{}", p.display_name);
        }
    }
}
