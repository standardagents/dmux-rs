//! Provider-neutral text inference: a port of the TS `InferenceService` /
//! `inferenceProviders.ts` essentials. Reads the same settings shape
//! (`inferencePrimary`/`inferenceBackup` targets) and the same credential
//! sources (env vars, `~/.dmux/inference-credentials.json`), speaks the
//! openai-compatible, openai-responses, and anthropic protocols, and fails
//! over primary → backup. ChatGPT/Grok subscription and Cohere protocols are
//! not yet ported.

mod chatgpt;

use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum InferError {
    #[error("no inference provider configured")]
    NotConfigured,
    #[error("provider '{0}' is not supported yet")]
    Unsupported(String),
    #[error("no API key for provider '{0}' (set {1})")]
    NoKey(String, String),
    #[error("request failed: {0}")]
    Request(String),
    #[error("bad response: {0}")]
    BadResponse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    OpenAiCompatible,
    OpenAiResponses,
    Anthropic,
    Cohere,
}

struct ProviderDef {
    id: &'static str,
    env_keys: &'static [&'static str],
    base_url: &'static str,
    protocol: Protocol,
}

/// Port of INFERENCE_PROVIDERS (the HTTP-key providers; subscription
/// providers are handled elsewhere or unsupported).
const PROVIDERS: &[ProviderDef] = &[
    ProviderDef { id: "openrouter", env_keys: &["OPENROUTER_API_KEY"], base_url: "https://openrouter.ai/api/v1", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "openai", env_keys: &["OPENAI_API_KEY"], base_url: "https://api.openai.com/v1", protocol: Protocol::OpenAiResponses },
    ProviderDef { id: "anthropic", env_keys: &["ANTHROPIC_API_KEY"], base_url: "https://api.anthropic.com/v1", protocol: Protocol::Anthropic },
    ProviderDef { id: "google", env_keys: &["GOOGLE_GENERATIVE_AI_API_KEY", "GOOGLE_API_KEY", "GEMINI_API_KEY"], base_url: "https://generativelanguage.googleapis.com/v1beta/openai", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "xai", env_keys: &["XAI_API_KEY"], base_url: "https://api.x.ai/v1", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "groq", env_keys: &["GROQ_API_KEY"], base_url: "https://api.groq.com/openai/v1", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "cerebras", env_keys: &["CEREBRAS_API_KEY"], base_url: "https://api.cerebras.ai/v1", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "deepseek", env_keys: &["DEEPSEEK_API_KEY"], base_url: "https://api.deepseek.com", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "mistral", env_keys: &["MISTRAL_API_KEY"], base_url: "https://api.mistral.ai/v1", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "together", env_keys: &["TOGETHER_API_KEY", "TOGETHER_AI_API_KEY"], base_url: "https://api.together.xyz/v1", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "fireworks", env_keys: &["FIREWORKS_API_KEY"], base_url: "https://api.fireworks.ai/inference/v1", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "perplexity", env_keys: &["PERPLEXITY_API_KEY"], base_url: "https://api.perplexity.ai", protocol: Protocol::OpenAiCompatible },
    ProviderDef { id: "cohere", env_keys: &["COHERE_API_KEY", "CO_API_KEY"], base_url: "https://api.cohere.ai/v1", protocol: Protocol::Cohere },
];

/// Settings-shaped inference target (`inferencePrimary` / `inferenceBackup`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub env_key: Option<String>,
}

impl Target {
    pub fn from_value(v: &Value) -> Option<Target> {
        serde_json::from_value(v.clone()).ok()
    }
}

#[derive(Debug)]
struct Resolved {
    base_url: String,
    protocol: Protocol,
    api_key: String,
}

fn stored_credentials(home: &std::path::Path) -> serde_json::Map<String, Value> {
    std::fs::read(home.join(".dmux").join("inference-credentials.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn lookup_key(home: &std::path::Path, provider_id: &str, env_keys: &[String]) -> Option<String> {
    for key in env_keys {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    let stored = stored_credentials(home);
    for key in env_keys {
        if let Some(v) = stored.get(key.as_str()).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    stored.get(provider_id).and_then(|v| v.as_str()).map(String::from)
}

fn resolve(home: &std::path::Path, target: &Target) -> Result<Resolved, InferError> {
    if target.provider_id == "custom" {
        let base = target.base_url.clone().ok_or_else(|| InferError::Unsupported("custom (no baseUrl)".into()))?;
        let env_key = target.env_key.clone().unwrap_or_default();
        let key = lookup_key(home, "custom", &[env_key.clone()])
            .ok_or_else(|| InferError::NoKey("custom".into(), env_key))?;
        return Ok(Resolved { base_url: base.trim_end_matches('/').to_string(), protocol: Protocol::OpenAiCompatible, api_key: key });
    }
    let def = PROVIDERS
        .iter()
        .find(|p| p.id == target.provider_id)
        .ok_or_else(|| InferError::Unsupported(target.provider_id.clone()))?;
    let env_keys: Vec<String> = def.env_keys.iter().map(|s| s.to_string()).collect();
    let key = lookup_key(home, def.id, &env_keys)
        .ok_or_else(|| InferError::NoKey(def.id.into(), def.env_keys.join("/")))?;
    Ok(Resolved { base_url: def.base_url.into(), protocol: def.protocol, api_key: key })
}

async fn generate_one(
    home: &std::path::Path,
    target: &Target,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String, InferError> {
    // ChatGPT subscription rides the codex app-server, not HTTP.
    if target.provider_id == "chatgpt" {
        return chatgpt::generate(&target.model_id, system, user).await;
    }
    let resolved = resolve(home, target)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| InferError::Request(e.to_string()))?;

    let (url, body, headers): (String, Value, Vec<(String, String)>) = match resolved.protocol {
        Protocol::OpenAiCompatible => (
            format!("{}/chat/completions", resolved.base_url),
            json!({
                "model": target.model_id,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user},
                ],
                "max_tokens": max_tokens,
                "temperature": 0,
            }),
            vec![("authorization".into(), format!("Bearer {}", resolved.api_key))],
        ),
        Protocol::OpenAiResponses => (
            format!("{}/responses", resolved.base_url),
            json!({
                "model": target.model_id,
                "instructions": system,
                "input": user,
                "max_output_tokens": max_tokens.max(16),
            }),
            vec![("authorization".into(), format!("Bearer {}", resolved.api_key))],
        ),
        Protocol::Anthropic => (
            format!("{}/messages", resolved.base_url),
            json!({
                "model": target.model_id,
                "system": system,
                "messages": [{"role": "user", "content": user}],
                "max_tokens": max_tokens,
            }),
            vec![
                ("x-api-key".into(), resolved.api_key.clone()),
                ("anthropic-version".into(), "2023-06-01".into()),
            ],
        ),
        Protocol::Cohere => (
            format!("{}/chat", resolved.base_url),
            json!({
                "model": target.model_id,
                "preamble": system,
                "message": user,
                "max_tokens": max_tokens,
            }),
            vec![("authorization".into(), format!("Bearer {}", resolved.api_key))],
        ),
    };

    let mut req = client.post(&url).json(&body);
    for (name, value) in headers {
        req = req.header(name, value);
    }
    let resp = req.send().await.map_err(|e| InferError::Request(e.to_string()))?;
    let status = resp.status();
    let payload: Value = resp.json().await.map_err(|e| InferError::BadResponse(e.to_string()))?;
    if !status.is_success() {
        let msg = payload["error"]["message"].as_str().unwrap_or("unknown error");
        return Err(InferError::Request(format!("{status}: {msg}")));
    }

    let text = match resolved.protocol {
        Protocol::OpenAiCompatible => payload["choices"][0]["message"]["content"].as_str().map(String::from),
        Protocol::OpenAiResponses => payload["output"]
            .as_array()
            .and_then(|items| {
                items.iter().find_map(|item| {
                    item["content"].as_array().and_then(|parts| {
                        parts.iter().find_map(|p| p["text"].as_str().map(String::from))
                    })
                })
            })
            .or_else(|| payload["output_text"].as_str().map(String::from)),
        Protocol::Anthropic => payload["content"][0]["text"].as_str().map(String::from),
        Protocol::Cohere => payload["text"].as_str().map(String::from),
    };
    text.filter(|t| !t.is_empty())
        .ok_or_else(|| InferError::BadResponse("no text in response".into()))
}

/// One row for the providers settings view: id, primary env var, and whether
/// a credential was found (env or the stored credentials file).
pub struct ProviderStatus {
    pub id: &'static str,
    pub env_key: &'static str,
    pub has_key: bool,
}

pub fn provider_statuses(home: &std::path::Path) -> Vec<ProviderStatus> {
    PROVIDERS
        .iter()
        .map(|d| {
            let keys: Vec<String> = d.env_keys.iter().map(|k| k.to_string()).collect();
            ProviderStatus { id: d.id, env_key: d.env_keys[0], has_key: lookup_key(home, d.id, &keys).is_some() }
        })
        .collect()
}

/// Generate with primary → backup failover (TS `callInference` semantics).
pub async fn generate(
    home: &std::path::Path,
    primary: Option<&Target>,
    backup: Option<&Target>,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String, InferError> {
    let primary = primary.ok_or(InferError::NotConfigured)?;
    match generate_one(home, primary, system, user, max_tokens).await {
        Ok(text) => Ok(text),
        Err(primary_err) => {
            if let Some(backup) = backup {
                tracing::warn!(%primary_err, "primary inference failed; trying backup");
                generate_one(home, backup, system, user, max_tokens).await
            } else {
                Err(primary_err)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pane analysis (PaneAnalyzer stage-1 port)

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneVerdict {
    OptionDialog,
    OpenPrompt,
    InProgress,
}

/// The TS PaneAnalyzer stage-1 system prompt, verbatim in substance.
pub const STATE_PROMPT: &str = r#"You are analyzing terminal output to determine its current state.
IMPORTANT: Focus primarily on the LAST 10 LINES of the output, as that's where the current state is shown.

Return a JSON object with a "state" field containing exactly one of these three values:
- "option_dialog": ONLY when specific options/choices are clearly presented
- "in_progress": When there are progress indicators showing active work
- "open_prompt": DEFAULT state - use this unless you're certain it's one of the above

OPTION DIALOG - Must have clear choices presented:
- "Continue? [y/n]"
- "Select: 1) Create 2) Edit 3) Cancel"
- Menu with numbered/lettered options

IN PROGRESS - Look for these in the BOTTOM 10 LINES:
- KEY INDICATOR: "(esc to interrupt)" or "esc to cancel" = ALWAYS in_progress
- Progress symbols with ANY action word ending in "ing..."
- Active progress bars or percentages

OPEN PROMPT - The DEFAULT state:
- Empty prompts: "> "
- Questions waiting for input without specific options

CRITICAL:
1. Check the BOTTOM 10 lines first
2. If you see "(esc to interrupt)" ANYWHERE = it's in_progress
3. When uncertain, default to "open_prompt""#;

/// Parse the model's state JSON (tolerates code fences and prose).
pub fn parse_state(text: &str) -> PaneVerdict {
    let cleaned = text.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```");
    let value: Option<Value> = serde_json::from_str(cleaned.trim()).ok().or_else(|| {
        // Find the first {...} block.
        let start = cleaned.find('{')?;
        let end = cleaned.rfind('}')?;
        serde_json::from_str(&cleaned[start..=end]).ok()
    });
    match value.and_then(|v| v["state"].as_str().map(String::from)).as_deref() {
        Some("option_dialog") => PaneVerdict::OptionDialog,
        Some("in_progress") => PaneVerdict::InProgress,
        Some("open_prompt") => PaneVerdict::OpenPrompt,
        // TS defaults invalid states to in_progress (safe: no false attention).
        _ => PaneVerdict::InProgress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parses_settings_shape() {
        let v = json!({"providerId": "openrouter", "modelId": "openai/gpt-5-mini"});
        let t = Target::from_value(&v).unwrap();
        assert_eq!(t.provider_id, "openrouter");
        assert_eq!(t.model_id, "openai/gpt-5-mini");
    }

    #[test]
    fn state_parsing_variants() {
        assert_eq!(parse_state(r#"{"state":"option_dialog"}"#), PaneVerdict::OptionDialog);
        assert_eq!(parse_state("```json\n{\"state\": \"open_prompt\"}\n```"), PaneVerdict::OpenPrompt);
        assert_eq!(parse_state("The state is {\"state\":\"in_progress\"} here"), PaneVerdict::InProgress);
        assert_eq!(parse_state("garbage"), PaneVerdict::InProgress);
    }

    #[test]
    fn unknown_provider_unsupported() {
        let t = Target { provider_id: "chatgpt".into(), model_id: "x".into(), base_url: None, env_key: None };
        let err = resolve(std::path::Path::new("/nonexistent"), &t).unwrap_err();
        assert!(matches!(err, InferError::Unsupported(_)));
    }
}
