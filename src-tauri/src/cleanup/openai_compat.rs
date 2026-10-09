//! OpenAI-style `/chat/completions` provider. Covers OpenAI, Groq,
//! OpenRouter, Together, and local servers such as Ollama and LM Studio.

use std::time::Duration;

use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use super::{
    http_error, system_prompt, tidy_output, user_message, BoxFuture, Cleaner, CleanupContext,
    CleanupError,
};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Generous so local reasoning models have room to think before answering.
const MAX_TOKENS: u32 = 4096;

pub struct OpenAiCompatCleaner {
    pub client: reqwest::Client,
    pub endpoint: Url,
    /// Optional: local servers usually don't need one.
    pub api_key: Option<String>,
    pub model: String,
    pub timeout: Duration,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

impl Cleaner for OpenAiCompatCleaner {
    fn clean<'a>(
        &'a self,
        transcript: &'a str,
        ctx: &'a CleanupContext<'a>,
    ) -> BoxFuture<'a, Result<String, CleanupError>> {
        Box::pin(async move {
            // Minimal body: servers differ in which extra fields they accept.
            let body = json!({
                "model": self.model,
                "messages": [
                    { "role": "system", "content": system_prompt(ctx) },
                    { "role": "user", "content": user_message(transcript) },
                ],
                "temperature": 0,
                "max_tokens": MAX_TOKENS,
            });
            let mut request = self
                .client
                .post(self.endpoint.clone())
                .timeout(self.timeout)
                .json(&body);
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key);
            }
            let response = request
                .send()
                .await
                .map_err(|e| CleanupError::from_reqwest(e, self.timeout))?;
            if !response.status().is_success() {
                return Err(http_error(response).await);
            }
            let chat: ChatResponse = response
                .json()
                .await
                .map_err(|e| CleanupError::from_reqwest(e, self.timeout))?;
            parse(chat)
        })
    }
}

fn parse(chat: ChatResponse) -> Result<String, CleanupError> {
    let choice = chat
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| CleanupError::BadResponse("no choices".into()))?;
    if let Some(reason) = choice.finish_reason.as_deref() {
        if reason != "stop" {
            return Err(CleanupError::BadResponse(format!("stopped early ({reason})")));
        }
    }
    let content = choice
        .message
        .content
        .ok_or_else(|| CleanupError::BadResponse("empty message".into()))?;
    Ok(tidy_output(&content))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(body: &str) -> Result<String, CleanupError> {
        parse(serde_json::from_str(body).unwrap())
    }

    #[test]
    fn reads_first_choice_and_strips_think_block() {
        let body = r#"{"choices":[{"index":0,"message":{"role":"assistant",
            "content":"<think>ok</think> Jam 4 ya."},"finish_reason":"stop"}]}"#;
        assert_eq!(parse_str(body).unwrap(), "Jam 4 ya.");
    }

    #[test]
    fn truncation_and_empty_are_errors() {
        let cut = r#"{"choices":[{"message":{"content":"Jam"},"finish_reason":"length"}]}"#;
        assert!(parse_str(cut).unwrap_err().to_string().contains("length"));
        let empty = r#"{"choices":[{"message":{"content":null},"finish_reason":"stop"}]}"#;
        assert!(parse_str(empty).is_err());
        assert!(parse_str(r#"{"choices":[]}"#).is_err());
    }
}
