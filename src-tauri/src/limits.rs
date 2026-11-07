//! Plan usage limits of the signed-in Claude Code account (5-hour and weekly windows), read from the same
//! endpoint Claude Code's `/usage` uses. Read-only; the token is sent only to api.anthropic.com and never refreshed
//! here (refreshing would rotate the CLI's own refresh token).

use crate::orchestrator::AppState;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::State;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The account's current plan (the credentials file only keeps the plan from the last login).
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Do not ask more often than this, even if the tab refreshes.
const MIN_GAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// Window key from the API, e.g. five_hour, seven_day, seven_day_opus
    pub key: String,
    pub percent: f64,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLimits {
    pub plan: Option<String>,
    pub tier: Option<String>,
    pub windows: Vec<Window>,
    /// Pay-as-you-go credits beyond the plan, when the account has them
    pub extra_enabled: bool,
    pub extra_used: Option<f64>,
    pub extra_limit: Option<f64>,
    /// "login" (no credentials), "expired" (token out of date), or an error text
    pub error: Option<String>,
    pub checked_at: u64,
}

/// Claude Code's config folder the app's CLI runs use: CLAUDE_CONFIG_DIR if set, else ~/.claude.
fn credentials_path() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(d).join(".credentials.json");
        if p.is_file() {
            return Some(p);
        }
    }
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok()?;
    Some(PathBuf::from(home).join(".claude").join(".credentials.json")).filter(|p| p.is_file())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Windows from the response, in a stable order: 5-hour first, then weekly ones.
fn windows_of(v: &Value) -> Vec<Window> {
    const ORDER: [&str; 5] = ["five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet", "seven_day_oauth_apps"];
    ORDER
        .iter()
        .filter_map(|k| {
            let w = v.get(*k)?;
            let percent = w.get("utilization")?.as_f64()?;
            Some(Window { key: k.to_string(), percent, resets_at: w.get("resets_at").and_then(Value::as_str).map(str::to_string) })
        })
        .collect()
}

/// "Pro", "Max 5x", "Max 20x", "Team" … from the organization type ("claude_max") and rate limit tier ("default_claude_max_20x").
fn claude_plan_name(kind: &str, tier: &str) -> String {
    if let Some(x) = ["20x", "5x"].iter().find(|x| tier.ends_with(&format!("max_{x}"))) {
        return format!("Max {x}");
    }
    title_case(kind.strip_prefix("claude_").unwrap_or(kind))
}

/// "plus" → "Plus", "google_ai_pro" → "Google Ai Pro"
fn title_case(s: &str) -> String {
    s.split(['_', ' ', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cache() -> &'static Mutex<Option<(Instant, ClaudeLimits)>> {
    static C: OnceLock<Mutex<Option<(Instant, ClaudeLimits)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

#[tauri::command]
pub async fn limits_claude(st: State<'_, Arc<AppState>>) -> Result<ClaudeLimits, String> {
    if let Some((at, v)) = cache().lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        if at.elapsed() < MIN_GAP {
            return Ok(v.clone());
        }
    }
    let mut out = ClaudeLimits { checked_at: now_secs(), ..Default::default() };
    let Some(path) = credentials_path() else {
        out.error = Some("login".into());
        return Ok(out);
    };
    let cred: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let o = &cred["claudeAiOauth"];
    // Fallback when the profile lookup fails.
    out.plan = o["subscriptionType"].as_str().map(|t| claude_plan_name(t, o["rateLimitTier"].as_str().unwrap_or("")));
    out.tier = o["rateLimitTier"].as_str().map(str::to_string);
    let Some(token) = o["accessToken"].as_str() else {
        out.error = Some("login".into());
        return Ok(out);
    };
    if o["expiresAt"].as_u64().is_some_and(|ms| ms / 1000 < now_secs()) {
        out.error = Some("expired".into());
        return Ok(out);
    }
    let get = |url: &'static str| st.http.get(url).bearer_auth(token).header("anthropic-beta", "oauth-2025-04-20").header("User-Agent", "elysium").timeout(Duration::from_secs(15)).send();
    let (res, profile) = tokio::join!(get(USAGE_URL), get(PROFILE_URL));
    if let Ok(r) = profile.and_then(|r| r.error_for_status()) {
        let v: Value = r.json().await.unwrap_or_default();
        let org = &v["organization"];
        if let Some(name) = org["organization_type"].as_str().map(|t| claude_plan_name(t, org["rate_limit_tier"].as_str().unwrap_or(""))) {
            out.plan = Some(name);
        }
    }
    match res {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or_default();
            out.windows = windows_of(&v);
            let x = &v["extra_usage"];
            out.extra_enabled = x["is_enabled"].as_bool().unwrap_or(false);
            out.extra_used = x["used_credits"].as_f64();
            out.extra_limit = x["monthly_limit"].as_f64();
        }
        Ok(r) if r.status().as_u16() == 401 => out.error = Some("expired".into()),
        Ok(r) => out.error = Some(format!("HTTP {}", r.status().as_u16())),
        Err(e) => out.error = Some(e.to_string()),
    }
    if out.error.is_none() {
        *cache().lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), out.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_windows_in_order_and_skips_missing_ones() {
        let v = json!({
            "seven_day": { "utilization": 96.0, "resets_at": "2026-11-07T05:00:00Z" },
            "five_hour": { "utilization": 37.0, "resets_at": "2026-11-06T17:20:00Z" },
            "seven_day_opus": null,
            "seven_day_sonnet": { "utilization": 12.5, "resets_at": null }
        });
        let w = windows_of(&v);
        assert_eq!(w.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["five_hour", "seven_day", "seven_day_sonnet"]);
        assert_eq!((w[0].percent, w[1].resets_at.as_deref()), (37.0, Some("2026-11-07T05:00:00Z")));
    }

    #[test]
    fn plan_names_read_like_the_plans() {
        assert_eq!(claude_plan_name("claude_pro", "default_claude_ai"), "Pro");
        assert_eq!(claude_plan_name("claude_max", "default_claude_max_5x"), "Max 5x");
        assert_eq!(claude_plan_name("max", "default_claude_max_20x"), "Max 20x");
        assert_eq!(claude_plan_name("claude_team", ""), "Team");
        assert_eq!(title_case("plus"), "Plus");
    }
}

// ---------------------------------------------------------------------------
// Antigravity: the desktop app's own local language server knows the per-model quota.
// It writes its port and a local CSRF token to ~/.gemini/antigravity/daemon/hub_<port>.json while it runs.

/// Models that share one quota (same remaining share and reset time) shown as one bar.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgyPool {
    pub models: Vec<String>,
    /// Used share of the quota, 0-100
    pub percent: f64,
    pub resets_at: Option<String>,
}

/// One quota group from the app's summary (e.g. "Gemini Models"), with its 5-hour and weekly windows.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgyGroup {
    pub name: String,
    /// key: "5h" or "weekly"
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgyLimits {
    pub plan: Option<String>,
    /// From RetrieveUserQuotaSummary (5-hour and weekly per group). Empty if the app is too old to have it.
    pub groups: Vec<AgyGroup>,
    /// Fallback from the per-model quota when there is no summary
    pub pools: Vec<AgyPool>,
    /// "app_off" when the Antigravity app is not running, or an error text
    pub error: Option<String>,
    pub checked_at: u64,
}

/// Hub files of running (or recently running) Antigravity servers, newest first.
fn agy_hubs() -> Vec<Value> {
    let Some(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok() else { return vec![] };
    let dir = PathBuf::from(home).join(".gemini").join("antigravity").join("daemon");
    let mut files: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("hub_") && n.ends_with(".json")))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().take(4).filter_map(|(_, p)| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()).collect()
}

/// Groups models by shared quota. A model without `remainingFraction` has nothing left.
/// Each Gemini model family keeps its own quota (Argon and Flash are separate even when the numbers match);
/// other models are grouped by their numbers only.
fn agy_pools(status: &Value) -> Vec<AgyPool> {
    let mut pools: Vec<(String, AgyPool)> = vec![];
    for c in status["cascadeModelConfigData"]["clientModelConfigs"].as_array().into_iter().flatten() {
        let q = &c["quotaInfo"];
        if q.is_null() {
            continue;
        }
        let left = q["remainingFraction"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
        let reset = q["resetTime"].as_str().map(str::to_string);
        let label = c["label"].as_str().unwrap_or("?").to_string();
        let base = label.split(" (").next().unwrap_or(&label);
        let family = if base.to_lowercase().starts_with("gemini") { base } else { "" };
        let key = format!("{family}|{left:.4}|{}", reset.as_deref().unwrap_or(""));
        match pools.iter_mut().find(|(k, _)| *k == key) {
            Some((_, p)) => p.models.push(label),
            None => pools.push((key, AgyPool { models: vec![label], percent: ((1.0 - left) * 1000.0).round() / 10.0, resets_at: reset })),
        }
    }
    pools.into_iter().map(|(_, p)| p).collect()
}

fn agy_groups(v: &Value) -> Vec<AgyGroup> {
    let r = if v["response"].is_object() { &v["response"] } else { v };
    r["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            let mut windows: Vec<Window> = g["buckets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|b| Window {
                    key: b["window"].as_str().unwrap_or("").to_string(),
                    percent: ((1.0 - b["remainingFraction"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0)) * 1000.0).round() / 10.0,
                    resets_at: b["resetTime"].as_str().map(str::to_string),
                })
                .collect();
            // 5-hour first, then weekly
            windows.sort_by_key(|w| if w.key == "5h" { 0 } else { 1 });
            AgyGroup { name: g["displayName"].as_str().unwrap_or("").to_string(), windows }
        })
        .filter(|g| !g.windows.is_empty())
        .collect()
}

async fn agy_rpc(http: &reqwest::Client, port: u64, csrf: &str, method: &str) -> Result<Value, String> {
    let r = http
        .post(format!("http://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/{method}"))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("X-Codeium-Csrf-Token", csrf)
        .body("{}")
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status().as_u16()));
    }
    r.json().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn limits_antigravity(st: State<'_, Arc<AppState>>) -> Result<AgyLimits, String> {
    let mut out = AgyLimits { checked_at: now_secs(), ..Default::default() };
    for h in agy_hubs() {
        let (Some(port), Some(csrf)) = (h["httpPort"].as_u64(), h["csrfToken"].as_str()) else { continue };
        let status = match agy_rpc(&st.http, port, csrf, "GetUserStatus").await {
            Ok(v) => v,
            // Connection refused: this hub file belongs to a server that is gone.
            Err(e) if e.starts_with("HTTP") => {
                out.error = Some(e);
                continue;
            }
            Err(_) => continue,
        };
        let us = if status["userStatus"].is_object() { &status["userStatus"] } else { &status };
        out.plan = us["userTier"]["name"].as_str().or(us["planStatus"]["planInfo"]["planName"].as_str()).map(str::to_string);
        out.groups = agy_rpc(&st.http, port, csrf, "RetrieveUserQuotaSummary").await.map(|v| agy_groups(&v)).unwrap_or_default();
        if out.groups.is_empty() {
            out.pools = agy_pools(us);
        }
        out.error = None;
        return Ok(out);
    }
    out.error.get_or_insert_with(|| "app_off".into());
    Ok(out)
}

#[cfg(test)]
mod agy_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_gives_five_hour_and_weekly_per_group() {
        let v = json!({ "response": { "groups": [
            { "displayName": "Gemini Models", "buckets": [
                { "window": "weekly", "remainingFraction": 0.019038, "resetTime": "2026-11-08T13:48:33Z" },
                { "window": "5h", "remainingFraction": 0.98712575, "resetTime": "2026-11-06T17:15:28Z" } ] },
            { "displayName": "Gemini 4 Argon", "buckets": [ { "window": "weekly", "remainingFraction": 0, "resetTime": "2026-11-08T13:26:40Z" } ] },
            { "displayName": "Empty", "buckets": [] }
        ]}});
        let g = agy_groups(&v);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].windows.iter().map(|w| (w.key.as_str(), w.percent)).collect::<Vec<_>>(), [("5h", 1.3), ("weekly", 98.1)]);
        assert_eq!((g[1].name.as_str(), g[1].windows.len(), g[1].windows[0].percent), ("Gemini 4 Argon", 1, 100.0));
    }

    #[test]
    fn models_sharing_a_quota_become_one_pool() {
        let us = json!({ "cascadeModelConfigData": { "clientModelConfigs": [
            { "label": "Gemini 4 Argon (Medium)", "quotaInfo": { "resetTime": "2026-11-08T13:26:40Z" } },
            { "label": "Gemini 4 Argon (Low)", "quotaInfo": { "resetTime": "2026-11-08T13:26:40Z" } },
            { "label": "Gemini 4 Argon (High)", "quotaInfo": { "remainingFraction": 0.019038, "resetTime": "2026-11-08T13:48:33Z" } },
            { "label": "Gemini 3.8 Flash (High)", "quotaInfo": { "remainingFraction": 0.019038, "resetTime": "2026-11-08T13:48:33Z" } },
            { "label": "Claude Opus 5.5 (Low)", "quotaInfo": { "remainingFraction": 1, "resetTime": "2026-11-06T19:42:12Z" } },
            { "label": "No quota" }
        ]}});
        let p = agy_pools(&us);
        assert_eq!(p.len(), 4, "Argon and Flash stay apart even with the same numbers");
        assert_eq!((p[0].models.len(), p[0].percent), (2, 100.0), "no remainingFraction means used up");
        assert_eq!((p[1].models.as_slice(), p[1].percent), (&["Gemini 4 Argon (High)".to_string()][..], 98.1));
        assert_eq!(p[2].models, vec!["Gemini 3.8 Flash (High)".to_string()]);
        assert_eq!((p[3].percent, p[3].resets_at.as_deref()), (0.0, Some("2026-11-06T19:42:12Z")));
    }
}

// ---------------------------------------------------------------------------
// Codex: `codex app-server` (the JSON-RPC mode the editor extensions use) answers `account/rateLimits/read`.
// Nothing is sent to a model; the CLI reads its own account limits.

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexWindow {
    /// Window length in minutes (300 = 5 hours, 10080 = a week)
    pub minutes: u64,
    pub percent: f64,
    /// Unix seconds
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexGroup {
    pub name: Option<String>,
    pub windows: Vec<CodexWindow>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexLimits {
    pub plan: Option<String>,
    pub groups: Vec<CodexGroup>,
    /// "missing" when Codex is not installed, "login" when not signed in, or an error text
    pub error: Option<String>,
    pub checked_at: u64,
}

/// The Codex CLI: on PATH, or the standalone install (%LOCALAPPDATA%\Programs\OpenAI\Codex\bin, ~/.codex/packages/standalone).
fn codex_exe() -> Option<PathBuf> {
    if let Some(p) = crate::engines::locate("codex") {
        return Some(p);
    }
    let mut c = vec![];
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        c.push(PathBuf::from(l).join("Programs").join("OpenAI").join("Codex").join("bin").join("codex.exe"));
    }
    if let Ok(h) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        c.push(PathBuf::from(h).join(".codex").join("packages").join("standalone").join("current").join("bin").join("codex.exe"));
    }
    c.into_iter().find(|p| p.is_file())
}

fn codex_window(w: &Value) -> Option<CodexWindow> {
    Some(CodexWindow { minutes: w["windowDurationMins"].as_u64()?, percent: w["usedPercent"].as_f64().unwrap_or(0.0), resets_at: w["resetsAt"].as_u64() })
}

fn codex_groups(result: &Value) -> Vec<CodexGroup> {
    let by_id = result["rateLimitsByLimitId"].as_object();
    let all: Vec<&Value> = match by_id {
        Some(m) if !m.is_empty() => m.values().collect(),
        _ => vec![&result["rateLimits"]],
    };
    all.into_iter()
        .map(|r| {
            let mut windows: Vec<CodexWindow> = [&r["primary"], &r["secondary"]].into_iter().filter_map(codex_window).collect();
            windows.sort_by_key(|w| w.minutes);
            CodexGroup { name: r["limitName"].as_str().map(str::to_string), windows }
        })
        .filter(|g| !g.windows.is_empty())
        .collect()
}

async fn codex_read(exe: &std::path::Path) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut child = crate::engines::command(exe)
        .arg("app-server")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
    let msgs = [
        json!({ "id": 1, "method": "initialize", "params": { "clientInfo": { "name": "elysium", "title": "Elysium", "version": env!("CARGO_PKG_VERSION") } } }),
        json!({ "method": "initialized" }),
        json!({ "id": 2, "method": "account/rateLimits/read", "params": null }),
    ];
    for m in msgs {
        stdin.write_all(format!("{m}\n").as_bytes()).await.map_err(|e| e.to_string())?;
    }
    stdin.flush().await.map_err(|e| e.to_string())?;
    let read = async {
        while let Ok(Some(l)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
            if v["id"] == 2 {
                return if v["error"].is_object() { Err(v["error"]["message"].as_str().unwrap_or("error").to_string()) } else { Ok(v["result"].clone()) };
            }
        }
        Err("no answer".to_string())
    };
    let out = tokio::time::timeout(Duration::from_secs(20), read).await.map_err(|_| "timeout".to_string())?;
    let _ = child.kill().await;
    out
}

fn codex_cache() -> &'static Mutex<Option<(Instant, CodexLimits)>> {
    static C: OnceLock<Mutex<Option<(Instant, CodexLimits)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

#[tauri::command]
pub async fn limits_codex() -> Result<CodexLimits, String> {
    if let Some((at, v)) = codex_cache().lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        if at.elapsed() < MIN_GAP {
            return Ok(v.clone());
        }
    }
    let mut out = CodexLimits { checked_at: now_secs(), ..Default::default() };
    let Some(exe) = codex_exe() else {
        out.error = Some("missing".into());
        return Ok(out);
    };
    match codex_read(&exe).await {
        Ok(r) => {
            out.plan = r["rateLimits"]["planType"].as_str().map(title_case);
            out.groups = codex_groups(&r);
            if out.groups.is_empty() && out.plan.is_none() {
                out.error = Some("login".into());
            }
        }
        Err(e) if e.to_lowercase().contains("auth") || e.to_lowercase().contains("login") => out.error = Some("login".into()),
        Err(e) => out.error = Some(e),
    }
    if out.error.is_none() {
        *codex_cache().lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), out.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod codex_tests {
    use super::*;

    #[test]
    fn reads_primary_and_secondary_windows() {
        let r = json!({ "rateLimits": { "planType": "plus" }, "rateLimitsByLimitId": { "codex": {
            "limitName": null,
            "primary": { "usedPercent": 42.0, "windowDurationMins": 10080, "resetsAt": 1796569644 },
            "secondary": { "usedPercent": 7.5, "windowDurationMins": 300, "resetsAt": 1796500000 }
        }}});
        let g = codex_groups(&r);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].windows.iter().map(|w| (w.minutes, w.percent)).collect::<Vec<_>>(), [(300, 7.5), (10080, 42.0)], "shorter window first");
    }
}
