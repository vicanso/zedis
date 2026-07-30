// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Minimal client for an OpenAI-compatible chat-completions endpoint.
//!
//! Used by the memory-analysis view to send a Markdown report to a
//! user-configured LLM and render its optimization advice. The request
//! is blocking (`ureq`) and **must** run on a background task, never on
//! the UI thread.

use super::proxy::system_proxy;
use crate::error::Error;
use serde_json::{Value, json};
use std::time::Duration;
use tracing::{debug, error};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Default model used when the user leaves the model field blank.
pub const DEFAULT_AI_MODEL: &str = "gpt-4o-mini";

/// How long to wait for the whole request/response before giving up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// System prompt steering the model toward concise, actionable Redis
/// optimization advice over the supplied analysis report. The reply
/// language is appended by [`build_system_prompt`].
const SYSTEM_PROMPT_BASE: &str = "You are a senior Redis performance engineer. You will receive a \
Markdown report summarizing a Redis database memory analysis (key-prefix groups, biggest keys, \
TTL distribution, eviction policy, sampling info). Produce concise, prioritized, actionable \
optimization advice in Markdown. Look specifically for: memory-leak risks (cache keys without \
TTL), oversized keys to split, large numbers of tiny string keys that could be merged into a \
hash, eviction-policy mismatches, and encoding/compression opportunities. Order findings by \
impact and keep each recommendation short.";

/// Human-readable language name for an app locale code, used to steer
/// the model's reply language. Falls back to English for unknown codes.
/// Mirrors the locales shipped under `locales/`.
fn language_name(locale: &str) -> &'static str {
    match locale {
        "zh" => "Chinese",
        "de" => "German",
        "es" => "Spanish",
        "fr" => "French",
        "ja" => "Japanese",
        "pt" => "Portuguese",
        "ru" => "Russian",
        _ => "English",
    }
}

/// Build the system prompt, instructing the model to reply in the
/// language matching the app's current `locale`.
fn build_system_prompt(locale: &str) -> String {
    format!(
        "{SYSTEM_PROMPT_BASE} Write your entire response in {}.",
        language_name(locale)
    )
}

/// Connection parameters for an OpenAI-compatible chat-completions endpoint.
pub struct AiEndpoint {
    /// Either an OpenAI-style base URL including the version path
    /// (e.g. `https://api.openai.com/v1`) or a complete endpoint URL
    /// ending in `/chat/completions`. A trailing slash is tolerated.
    pub base_url: String,
    /// Bearer API key (already decrypted).
    pub api_key: String,
    /// Model name; blank falls back to [`DEFAULT_AI_MODEL`].
    pub model: String,
}

impl AiEndpoint {
    /// Full chat-completions URL derived from `base_url`.
    ///
    /// Accepts either an OpenAI-style base URL (e.g. `…/v1`), to which
    /// `/chat/completions` is appended, or an already-complete endpoint
    /// (anything containing `/chat/completions`, including a query
    /// string such as Azure's `?api-version=…`), which is used verbatim.
    fn chat_completions_url(&self) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        if base.contains("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    fn effective_model(&self) -> &str {
        let model = self.model.trim();
        if model.is_empty() { DEFAULT_AI_MODEL } else { model }
    }
}

/// Submit a Markdown memory-analysis `report` to the configured
/// endpoint and return the model's Markdown reply. `locale` is the
/// app's UI locale code (e.g. `zh`, `ja`) — the model is asked to reply
/// in that language.
///
/// Blocking — run from a background task. Returns a descriptive error
/// (HTTP status + endpoint message when available) on failure.
pub fn analyze_report(endpoint: &AiEndpoint, report: &str, locale: &str) -> Result<String> {
    if endpoint.base_url.trim().is_empty() {
        return invalid("AI base URL is not configured");
    }
    if endpoint.api_key.trim().is_empty() {
        return invalid("AI API key is not configured");
    }

    let url = endpoint.chat_completions_url();
    let model = endpoint.effective_model().to_string();
    let system_prompt = build_system_prompt(locale);
    let body = json!({
        "model": model,
        "stream": false,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": report },
        ],
    });
    let body = serde_json::to_string(&body)?;
    // URL + model are safe to log; the API key never is.
    debug!(%url, %model, report_bytes = report.len(), "AI analysis: sending request");

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        // Env-var proxy plus the OS system proxy — AI endpoints are
        // frequently only reachable through one (see `helpers/proxy.rs`).
        .proxy(system_proxy())
        // Read the body ourselves on non-2xx so we can surface the
        // endpoint's `error.message` instead of a bare status code.
        .http_status_as_error(false)
        .build()
        .new_agent();

    let response = agent
        .post(&url)
        .header("Authorization", format!("Bearer {}", endpoint.api_key.trim()))
        .header("Content-Type", "application/json")
        .send(body.as_str())
        .map_err(|e| {
            error!(%url, error = %e, "AI analysis: request failed (network/TLS)");
            Error::Invalid {
                message: format!("AI request failed: {e}"),
            }
        })?;

    let status = response.status();
    let text = response.into_body().read_to_string().map_err(|e| {
        error!(%url, error = %e, "AI analysis: failed to read response body");
        Error::Invalid {
            message: format!("AI response read failed: {e}"),
        }
    })?;

    if !status.is_success() {
        let detail = extract_error_message(&text).unwrap_or_else(|| text.chars().take(300).collect());
        // Dump the full response body and the model we sent — a 400
        // "Param Incorrect" usually means an unknown model name or an
        // unsupported parameter, and the raw body names which one.
        let raw_body: String = text.chars().take(1000).collect();
        error!(
            %url,
            %model,
            status = status.as_u16(),
            detail = %detail,
            response_body = %raw_body,
            "AI analysis: endpoint returned an error status"
        );
        return Err(Error::Invalid {
            message: format!("AI endpoint error ({}): {detail}", status.as_u16()),
        });
    }

    let value: Value = serde_json::from_str(&text)?;
    let content = extract_content(&value).ok_or_else(|| {
        // Log a snippet of the raw body so an unexpected response shape
        // (e.g. a provider that nests content differently) is diagnosable.
        let snippet: String = text.chars().take(300).collect();
        error!(%url, body_snippet = %snippet, "AI analysis: response missing choices[0].message.content");
        Error::Invalid {
            message: "AI response did not contain choices[0].message.content".to_string(),
        }
    })?;
    debug!(%url, reply_bytes = content.len(), "AI analysis: received reply");
    Ok(content)
}

fn invalid(message: &str) -> Result<String> {
    Err(Error::Invalid {
        message: message.to_string(),
    })
}

/// Pull `choices[0].message.content` out of a chat-completions response.
fn extract_content(value: &Value) -> Option<String> {
    let content = value.get("choices")?.get(0)?.get("message")?.get("content")?.as_str()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Pull `error.message` out of an error response body, if present.
fn extract_error_message(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    value.get("error")?.get("message")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_url_appends_or_passes_through() {
        let url = |base: &str| {
            AiEndpoint {
                base_url: base.to_string(),
                api_key: "k".to_string(),
                model: String::new(),
            }
            .chat_completions_url()
        };
        // A base URL (with or without a trailing slash) gets the path appended.
        assert_eq!(
            url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
        // An already-complete endpoint is used verbatim.
        assert_eq!(
            url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        // Query strings (e.g. Azure deployments) are preserved.
        assert_eq!(
            url("https://x.openai.azure.com/openai/deployments/g/chat/completions?api-version=2024-02-01"),
            "https://x.openai.azure.com/openai/deployments/g/chat/completions?api-version=2024-02-01"
        );
    }

    #[test]
    fn effective_model_falls_back_when_blank() {
        let endpoint = AiEndpoint {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "k".to_string(),
            model: "  ".to_string(),
        };
        assert_eq!(endpoint.effective_model(), DEFAULT_AI_MODEL);
    }

    #[test]
    fn extract_content_reads_first_choice() {
        let value: Value =
            serde_json::from_str(r#"{"choices":[{"message":{"role":"assistant","content":"  advice  "}}]}"#)
                .expect("valid json");
        assert_eq!(extract_content(&value).as_deref(), Some("advice"));
    }

    #[test]
    fn extract_content_none_when_empty_or_missing() {
        let empty: Value = serde_json::from_str(r#"{"choices":[{"message":{"content":"   "}}]}"#).expect("valid json");
        assert_eq!(extract_content(&empty), None);
        let missing: Value = serde_json::from_str(r#"{"choices":[]}"#).expect("valid json");
        assert_eq!(extract_content(&missing), None);
    }

    #[test]
    fn system_prompt_targets_locale_language() {
        assert_eq!(language_name("zh"), "Chinese");
        assert_eq!(language_name("ja"), "Japanese");
        // Unknown / English codes fall back to English.
        assert_eq!(language_name("xx"), "English");
        assert!(build_system_prompt("zh").ends_with("Write your entire response in Chinese."));
        assert!(build_system_prompt("de").contains("German"));
    }

    #[test]
    fn extract_error_message_reads_openai_shape() {
        let body = r#"{"error":{"message":"Invalid API key","type":"auth"}}"#;
        assert_eq!(extract_error_message(body).as_deref(), Some("Invalid API key"));
        assert_eq!(extract_error_message("not json"), None);
    }
}
