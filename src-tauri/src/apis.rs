//! API 키로 쓰는 AI(Claude API·OpenAI·Gemini API·NVIDIA). 스트리밍으로 받아 CLI 엔진과 같은 이벤트로 넘긴다.
//! 작업 모드에서는 api_agent가 도구 호출(파일·PowerShell)을 이 PC에서 실행해 준다.

use crate::engines::{EngineId, Event, Outcome, RunSpec};
use crate::secrets::{self, SecretName};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    OpenAiCompat,
    Anthropic,
}

pub struct Api {
    pub base_url: &'static str,
    pub wire: Wire,
    pub default_model: &'static str,
    pub secret: SecretName,
    pub key_hint: &'static str,
    /// OpenAI 호환 `reasoning_effort`를 받는지
    pub reasoning: bool,
}

/// 기본 모델은 설정에서 바꿀 수 있다.
pub fn api(e: EngineId) -> Option<Api> {
    Some(match e {
        EngineId::ClaudeApi => Api {
            base_url: "https://api.anthropic.com",
            wire: Wire::Anthropic,
            default_model: "claude-sonnet-5-5",
            secret: SecretName::Anthropic,
            key_hint: "console.anthropic.com에서 발급(sk-ant-…)",
            reasoning: false,
        },
        EngineId::OpenAi => Api {
            base_url: "https://api.openai.com/v1",
            wire: Wire::OpenAiCompat,
            default_model: "gpt-5",
            secret: SecretName::OpenAi,
            key_hint: "platform.openai.com에서 발급(sk-…)",
            reasoning: true,
        },
        EngineId::GeminiApi => Api {
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            wire: Wire::OpenAiCompat,
            default_model: "gemini-2.5-flash",
            secret: SecretName::Gemini,
            key_hint: "aistudio.google.com에서 발급",
            reasoning: true,
        },
        EngineId::Nvidia => Api {
            base_url: "https://integrate.api.nvidia.com/v1",
            wire: Wire::OpenAiCompat,
            // 2026-10-17 사용자 요청·사용자 키로 확인(첫 호출 5~20초, 가끔 503 과부하). 대안: deepseek-ai/deepseek-v4.1-flash
            default_model: "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
            secret: SecretName::Nvidia,
            key_hint: "build.nvidia.com에서 무료 발급(nvapi-…, 체험 크레딧·분당 약 40회)",
            reasoning: false,
        },
        _ => return None,
    })
}

/// 토큰 사용량. input은 캐시에서 읽지 않은 입력 토큰이고, 캐시 읽기·쓰기는 따로 센다.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// SSE 바이트 스트림을 이벤트 단위로 자른다. 빈 줄까지 바이트로 모은 뒤 디코딩하므로 청크 경계에서 한글이 깨지지 않는다.
#[derive(Default)]
struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend(chunk.iter().copied().filter(|b| *b != b'\r'));
        let mut out = Vec::new();
        while let Some(pos) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let event: Vec<u8> = self.buf.drain(..pos + 2).collect();
            for line in String::from_utf8_lossy(&event).lines() {
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data != "[DONE]" && !data.is_empty() {
                    out.push(data.to_string());
                }
            }
        }
        out
    }
}

#[derive(Default, Debug, PartialEq)]
struct Acc {
    /// 캐시에서 읽지 않은 입력 토큰
    input: Option<u64>,
    output: Option<u64>,
    /// 캐시 읽기·쓰기 토큰(따로 싸게·비싸게 계산한다)
    cache_read: u64,
    cache_write: u64,
    stop: Option<String>,
    /// 생각 단계(reasoning_content)로만 온 글자 수: 본문이 비었을 때 원인 진단용
    reasoning: usize,
    /// 첫 응답 조각(본문이 비었을 때 원인 진단용, 키는 가려서)
    first: Option<String>,
}

fn parse_openai(v: &Value, acc: &mut Acc) -> Option<String> {
    if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
        // prompt_tokens에는 캐시에서 읽은 입력도 들어 있다(prompt_tokens_details.cached_tokens)
        acc.cache_read = u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(acc.cache_read);
        acc.input = u["prompt_tokens"].as_u64().map(|p| p.saturating_sub(acc.cache_read)).or(acc.input);
        acc.output = u["completion_tokens"].as_u64().or(acc.output);
    }
    if acc.first.is_none() {
        acc.first = Some(v.to_string().chars().take(160).collect());
    }
    let c = v.get("choices")?.get(0)?;
    if let Some(r) = c.get("delta").and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning"))).and_then(|r| r.as_str()) {
        acc.reasoning += r.chars().count();
    }
    if let Some(r) = c["finish_reason"].as_str() {
        acc.stop = Some(r.to_string());
    }
    c.get("delta")?.get("content")?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn parse_anthropic(v: &Value, acc: &mut Acc) -> Result<Option<String>, String> {
    match v["type"].as_str() {
        Some("message_start") => {
            acc.input = v["message"]["usage"]["input_tokens"].as_u64();
            acc.cache_read = v["message"]["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
            acc.cache_write = v["message"]["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            Ok(None)
        }
        Some("content_block_delta") if v["delta"]["type"] == "text_delta" => Ok(v["delta"]["text"].as_str().map(str::to_string)),
        Some("message_delta") => {
            acc.stop = v["delta"]["stop_reason"].as_str().map(str::to_string).or(acc.stop.take());
            acc.output = v["usage"]["output_tokens"].as_u64().or(acc.output);
            Ok(None)
        }
        Some("error") => Err(v["error"]["message"].as_str().unwrap_or("API 오류").to_string()),
        _ => Ok(None),
    }
}

pub(crate) fn status_message(code: u16, body: &str) -> String {
    let what = match code {
        401 | 403 => "키가 맞지 않거나 권한이 없습니다",
        404 => "모델 또는 주소를 찾을 수 없습니다(설정의 모델 이름 확인)",
        429 => "요청 한도를 넘었습니다(429)",
        529 | 503 => "공급자가 과부하 상태입니다",
        400..=499 => "잘못된 요청입니다",
        _ => "공급자 서버 오류",
    };
    let short: String = body.chars().take(200).collect();
    secrets::redact(&format!("{what} ({code}) {short}"))
}

/// 다시 시도하면 될 만한 오류(과부하·요청 한도·일시적인 서버 오류)
pub(crate) fn transient(e: &str) -> bool {
    ["과부하", "바쁩니다", "(429)", "(500)", "(502)", "(503)", "(504)"].iter().any(|w| e.contains(w))
}

/// 스트림 안에 온 오류를 사람이 읽을 말로
pub(crate) fn stream_error(e: &Value) -> String {
    let msg = e["message"].as_str().map(str::to_string).unwrap_or_else(|| e.to_string());
    let code = e["code"].as_u64().map(|c| format!(" ({c})")).unwrap_or_default();
    let busy = ["ResourceExhausted", "rate limit", "overloaded", "capacity", "Too Many"].iter().any(|w| msg.to_lowercase().contains(&w.to_lowercase()));
    let short: String = msg.chars().take(160).collect();
    secrets::redact(&if busy { format!("공급자 서버가 바쁩니다(과부하){code} {short}") } else { format!("공급자 오류{code} {short}") })
}

/// 요청 본문. effort: OpenAI 호환은 `reasoning_effort`(OpenAI·Gemini가 지원, NVIDIA는 보내지 않음), Anthropic은 `output_config.effort`.
pub(crate) fn body(a: &Api, model: &str, system: &str, prompt: &str, max_tokens: u32, stream: bool, effort: Option<&str>) -> Value {
    match a.wire {
        Wire::OpenAiCompat => {
            let mut b = json!({
                "model": model,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}],
                "max_tokens": max_tokens,
                "stream": stream,
            });
            if stream {
                b["stream_options"] = json!({"include_usage": true});
            }
            if let (Some(e), true) = (effort, a.reasoning) {
                b["reasoning_effort"] = json!(e);
            }
            // NVIDIA Nemotron 추론 모델은 생각 단계를 켜고 끌 수 있다. 가벼운 요청은 꺼서 토큰을 아낀다.
            if let (Some(e), true) = (effort, a.secret == SecretName::Nvidia && model.contains("nemotron")) {
                b["chat_template_kwargs"] = json!({ "enable_thinking": e != "low" });
            }
            b
        }
        Wire::Anthropic => {
            let mut b = json!({ "model": model, "max_tokens": max_tokens, "messages": [{"role": "user", "content": prompt}], "stream": stream });
            // 시스템 지시는 캐시한다: 같은 세션의 다음 요청에서 다시 읽는 값이 기본 입력의 약 1/10이다.
            if !system.is_empty() {
                b["system"] = json!([{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }]);
            }
            if let Some(e) = effort {
                b["output_config"] = json!({ "effort": e });
            }
            b
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn request(http: &reqwest::Client, a: &Api, key: &str, model: &str, system: &str, prompt: &str, max_tokens: u32, stream: bool, effort: Option<&str>) -> reqwest::RequestBuilder {
    let b = body(a, model, system, prompt, max_tokens, stream, effort);
    match a.wire {
        Wire::OpenAiCompat => http.post(format!("{}/chat/completions", a.base_url)).bearer_auth(key).json(&b),
        Wire::Anthropic => http.post(format!("{}/v1/messages", a.base_url)).header("x-api-key", key).header("anthropic-version", "2023-06-01").json(&b),
    }
}

pub async fn run(http: &reqwest::Client, spec: &RunSpec, model: &str, timeout: Duration, cancel: &CancellationToken, on_event: &mut impl FnMut(&Event)) -> Outcome {
    let started = Instant::now();
    let mut out = Outcome::default();
    let done = |mut o: Outcome, err: Option<String>| {
        o.ok = err.is_none();
        o.error = err;
        o.latency_ms = started.elapsed().as_millis() as u64;
        o
    };
    let Some(a) = api(spec.engine) else { return done(out, Some("API 엔진이 아닙니다".into())) };
    let Some(key) = secrets::get(a.secret) else { return done(out, Some(format!("{} 키가 없습니다. 설정 → API 키에서 넣으세요", spec.engine.label()))) };
    // 작업 모드: 모델이 부른 도구(파일·PowerShell)를 이 PC에서 실행하며 끝날 때까지 주고받는다.
    if spec.work {
        let mut usage = Usage::default();
        let r = tokio::select! {
            r = tokio::time::timeout(timeout, crate::api_agent::run(http, spec, &a, &key, model, &mut out, &mut usage, on_event)) => r.unwrap_or_else(|_| Err(format!("{}초 안에 끝나지 않았습니다", timeout.as_secs()))),
            _ = cancel.cancelled() => Err("사용자가 중지했습니다".into()),
        };
        if let Ok(t) = &r {
            out.text = t.clone();
        }
        out.input_tokens = Some(usage.input);
        out.output_tokens = Some(usage.output);
        out.cost_usd = Some(crate::pricing::estimate_cost_cached(spec.engine, Some(model), usage));
        return done(out, r.err());
    }
    let mut acc = Acc::default();
    let send = async {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let one = async {
                let resp = request(http, &a, &key, model, &spec.system, &spec.prompt, spec.max_tokens.unwrap_or(8192), true, spec.effort.as_deref())
                    .send()
                    .await
                    .map_err(|e| if e.is_timeout() { "응답 시간 초과".to_string() } else { format!("{}에 연결할 수 없습니다", spec.engine.label()) })?;
                let code = resp.status().as_u16();
                if !resp.status().is_success() {
                    return Err(status_message(code, &resp.text().await.unwrap_or_default()));
                }
                let mut stream = resp.bytes_stream();
                let mut sse = SseBuffer::default();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| format!("스트림이 끊겼습니다: {e}"))?;
                    for data in sse.push(&chunk) {
                        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
                        // 일부 공급자(NVIDIA 무료 서버 등)는 200 응답의 스트림 안에 오류를 보낸다.
                        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                            return Err(stream_error(e));
                        }
                        let delta = match a.wire {
                            Wire::OpenAiCompat => parse_openai(&v, &mut acc),
                            Wire::Anthropic => parse_anthropic(&v, &mut acc)?,
                        };
                        if let Some(d) = delta {
                            out.text.push_str(&d);
                            on_event(&Event::Text(d));
                        }
                    }
                }
                if acc.stop.as_deref() == Some("refusal") {
                    return Err("모델이 안전 정책에 따라 답을 거절했습니다".into());
                }
                Ok::<(), String>(())
            };
            match one.await {
                // 잠깐의 과부하는 글자를 받기 전이면 조금 기다렸다 다시 시도한다(최대 3번).
                Err(e) if attempt < 3 && out.text.is_empty() && transient(&e) => {
                    on_event(&Event::Activity(format!("{} 서버가 바빠 {}초 뒤 다시 시도합니다({}/3)", spec.engine.label(), attempt * 2, attempt + 1)));
                    acc = Acc::default();
                    tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                }
                other => break other,
            }
        }
    };
    let r = tokio::select! {
        r = tokio::time::timeout(timeout, send) => r.unwrap_or_else(|_| Err(format!("{}초 안에 끝나지 않았습니다", timeout.as_secs()))),
        _ = cancel.cancelled() => Err("사용자가 중지했습니다".into()),
    };
    out.input_tokens = acc.input;
    out.output_tokens = acc.output;
    out.cost_usd = acc.input.map(|input| {
        let usage = Usage { input, output: acc.output.unwrap_or(0), cache_read: acc.cache_read, cache_write: acc.cache_write };
        crate::pricing::estimate_cost_cached(spec.engine, Some(model), usage)
    });
    let err = r.err().or_else(|| {
        out.text.trim().is_empty().then(|| {
            let why = match (acc.stop.as_deref(), acc.reasoning) {
                (Some("length"), r) if r > 0 => format!("생각 단계에서 출력 한도를 다 써 본문이 비었습니다(생각 {r}자)"),
                (Some(s), _) => format!("끝난 이유: {s}"),
                (None, _) => "응답이 바로 끝났습니다".into(),
            };
            secrets::redact(&format!("빈 답을 받았습니다 — {why}; 첫 응답: {}", acc.first.clone().unwrap_or_else(|| "(없음)".into())))
        })
    });
    done(out, err)
}

/// 키·모델 확인: 1토큰짜리 요청을 보낸다(요청 1회를 쓴다).
pub async fn test(http: &reqwest::Client, engine: EngineId, model: &str) -> Result<String, String> {
    let a = api(engine).ok_or("API 엔진이 아닙니다")?;
    let key = secrets::get(a.secret).ok_or("키가 없습니다")?;
    let started = Instant::now();
    let r = request(http, &a, &key, model, "Reply with one word.", "안녕", 1, false, None)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| format!("{}에 연결할 수 없습니다", engine.label()))?;
    let code = r.status().as_u16();
    if !r.status().is_success() {
        return Err(status_message(code, &r.text().await.unwrap_or_default()));
    }
    Ok(format!("키·모델 확인됨({}ms): {model}", started.elapsed().as_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_errors_are_readable_and_retried() {
        let e = serde_json::json!({"code": 500, "message": "ResourceExhausted: Worker local total request limit reached (16/16)", "type": "internal_server_error"});
        let m = stream_error(&e);
        assert!(m.starts_with("공급자 서버가 바쁩니다(과부하) (500)"), "{m}");
        assert!(transient(&m));
        assert!(!transient("키가 거절됐습니다 (401)"));
        assert!(stream_error(&serde_json::json!({"message": "bad model"})).starts_with("공급자 오류"));
    }

    #[test]
    fn sse_keeps_korean_split_across_chunks() {
        let raw = "data: {\"t\":\"안녕하세요\"}\r\n\r\ndata: [DONE]\n\n".as_bytes();
        let cut = raw.iter().position(|b| *b >= 0x80).unwrap() + 1;
        let mut sse = SseBuffer::default();
        assert!(sse.push(&raw[..cut]).is_empty());
        assert_eq!(sse.push(&raw[cut..]), vec!["{\"t\":\"안녕하세요\"}".to_string()]);
    }

    #[test]
    fn parses_openai_and_anthropic_streams() {
        let mut acc = Acc::default();
        assert_eq!(parse_openai(&json!({"choices":[{"delta":{"content":"hi"},"finish_reason":null}]}), &mut acc).as_deref(), Some("hi"));
        assert_eq!(parse_openai(&json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3}}), &mut acc), None);
        assert_eq!((acc.input, acc.output), (Some(10), Some(3)));
        let mut acc = Acc::default();
        parse_anthropic(&json!({"type":"message_start","message":{"usage":{"input_tokens":42}}}), &mut acc).unwrap();
        assert_eq!(parse_anthropic(&json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"안녕"}}), &mut acc).unwrap().as_deref(), Some("안녕"));
        assert_eq!(parse_anthropic(&json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"…"}}), &mut acc).unwrap(), None);
        parse_anthropic(&json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}), &mut acc).unwrap();
        assert_eq!(acc, Acc { input: Some(42), output: Some(12), stop: Some("end_turn".into()), ..Default::default() });
        assert!(parse_anthropic(&json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}), &mut acc).is_err());
    }

    #[test]
    fn effort_goes_where_each_api_accepts_it() {
        let o = api(EngineId::OpenAi).unwrap();
        let b = body(&o, "m", "s", "p", 1500, true, Some("low"));
        assert_eq!((b["reasoning_effort"].as_str(), b["max_tokens"].as_u64()), (Some("low"), Some(1500)));
        assert!(body(&api(EngineId::Nvidia).unwrap(), "m", "s", "p", 10, true, Some("low")).get("reasoning_effort").is_none(), "NVIDIA에는 안 보냄");
        let n = api(EngineId::Nvidia).unwrap();
        let think = |m: &str, e: Option<&str>| body(&n, m, "s", "p", 10, true, e).get("chat_template_kwargs").map(|k| k["enable_thinking"].clone());
        assert_eq!(think(n.default_model, Some("low")), Some(json!(false)), "가벼운 요청은 생각 끔");
        assert_eq!(think(n.default_model, Some("high")), Some(json!(true)));
        assert_eq!(think("deepseek-ai/deepseek-v4.1-flash", Some("high")), None, "Nemotron 아닌 모델엔 안 보냄");
        let c = body(&api(EngineId::ClaudeApi).unwrap(), "m", "s", "p", 10, true, Some("high"));
        assert_eq!(c["output_config"]["effort"], "high");
        assert!(body(&o, "m", "s", "p", 10, false, None).get("reasoning_effort").is_none());
    }

    #[test]
    fn cache_tokens_are_counted_apart_from_input() {
        // Anthropic: 캐시 읽기는 input_tokens와 따로 온다
        let mut acc = Acc::default();
        parse_anthropic(&json!({"type":"message_start","message":{"usage":{"input_tokens":5,"cache_read_input_tokens":900,"cache_creation_input_tokens":0}}}), &mut acc).unwrap();
        assert_eq!((acc.input, acc.cache_read), (Some(5), 900));
        // OpenAI 호환: prompt_tokens에 캐시 적중분이 포함된다 → 뺀 나머지를 입력으로
        let mut acc = Acc::default();
        parse_openai(&json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":80}}}), &mut acc);
        assert_eq!((acc.input, acc.cache_read), (Some(20), 80));
        // 시스템 지시는 캐시 표시를 붙여 보낸다(비어 있으면 아예 안 보냄)
        let b = body(&api(EngineId::ClaudeApi).unwrap(), "m", "지시", "p", 10, true, None);
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body(&api(EngineId::ClaudeApi).unwrap(), "m", "", "p", 10, true, None).get("system").is_none());
    }

    #[test]
    fn every_api_engine_has_a_config_and_cli_engines_do_not() {
        for e in crate::engines::ALL {
            assert_eq!(api(e).is_some(), e.is_api(), "{e:?}");
        }
        assert!(api(EngineId::Nvidia).unwrap().base_url.starts_with("https://"));
    }
}
