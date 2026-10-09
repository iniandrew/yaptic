//! Anthropic Messages API provider.

use std::time::Duration;

use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use super::{
    http_error, system_prompt, tidy_output, user_message, BoxFuture, Cleaner, CleanupContext,
    CleanupError,
};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";
const API_VERSION: &str = "2023-06-01";
/// Leaves room for thinking on models that think by default.
const MAX_TOKENS: u32 = 4096;

pub struct AnthropicCleaner {
    pub client: reqwest::Client,
    pub endpoint: Url,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
}

#[derive(Deserialize)]
struct MessageResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    /// Thinking and any other block types.
    #[serde(other)]
    Other,
}

impl Cleaner for AnthropicCleaner {
    fn clean<'a>(
        &'a self,
        transcript: &'a str,
        ctx: &'a CleanupContext<'a>,
    ) -> BoxFuture<'a, Result<String, CleanupError>> {
        Box::pin(async move {
            // Minimal body: the model name is user-editable, so anything
            // model-specific could be rejected.
            let body = json!({
                "model": self.model,
                "max_tokens": MAX_TOKENS,
                "system": system_prompt(ctx),
                "messages": [{ "role": "user", "content": user_message(transcript) }],
            });
            let response = self
                .client
                .post(self.endpoint.clone())
                .timeout(self.timeout)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .json(&body)
                .send()
                .await
                .map_err(|e| CleanupError::from_reqwest(e, self.timeout))?;
            if !response.status().is_success() {
                return Err(http_error(response).await);
            }
            let message: MessageResponse = response
                .json()
                .await
                .map_err(|e| CleanupError::from_reqwest(e, self.timeout))?;
            parse(message)
        })
    }
}

fn parse(message: MessageResponse) -> Result<String, CleanupError> {
    match message.stop_reason.as_deref() {
        Some("end_turn") => {}
        other => {
            return Err(CleanupError::BadResponse(format!(
                "stopped early ({})",
                other.unwrap_or("unknown")
            )))
        }
    }
    let text: String = message
        .content
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text),
            ContentBlock::Other => None,
        })
        .collect();
    Ok(tidy_output(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(body: &str) -> Result<String, CleanupError> {
        parse(serde_json::from_str(body).unwrap())
    }

    #[test]
    fn joins_text_blocks_and_skips_thinking() {
        let body = r#"{
            "content": [
                {"type": "thinking", "thinking": "", "signature": "abc"},
                {"type": "text", "text": "Jam 4 ya. "}
            ],
            "stop_reason": "end_turn"
        }"#;
        assert_eq!(parse_str(body).unwrap(), "Jam 4 ya.");
    }

    #[test]
    fn refusal_and_truncation_are_errors() {
        for reason in ["refusal", "max_tokens"] {
            let body = format!(
                r#"{{"content": [{{"type": "text", "text": "partial"}}], "stop_reason": "{reason}"}}"#
            );
            assert!(parse_str(&body).unwrap_err().to_string().contains(reason));
        }
    }
}
