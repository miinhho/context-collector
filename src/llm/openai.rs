use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{LlmClient, LlmRequest};
use crate::token::{TiktokenCounter, TokenCounter};

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    instructions: &'a str,
    input: &'a str,
    max_output_tokens: u32,
    store: bool,
    text: TextFormat<'a>,
}

#[derive(Serialize)]
struct TextFormat<'a> {
    format: JsonSchemaFormat<'a>,
}

#[derive(Serialize)]
struct JsonSchemaFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    strict: bool,
    schema: &'a Value,
}

#[derive(Deserialize)]
struct ResponsesResult {
    status: String,
    output: Vec<OutputItem>,
}

#[derive(Deserialize)]
struct OutputItem {
    content: Vec<OutputContent>,
}

#[derive(Deserialize)]
struct OutputContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

pub struct OpenAiClientConfig {
    pub api_key: String,
    pub endpoint: String,
    pub timeout: Duration,
}

pub struct OpenAiResponsesClient {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    api_key: String,
}

impl OpenAiResponsesClient {
    pub fn new(config: OpenAiClientConfig) -> Result<Self, String> {
        if config.api_key.trim().is_empty() || config.timeout.is_zero() {
            return Err("API key and positive timeout are required".into());
        }
        let endpoint = reqwest::Url::parse(&config.endpoint).map_err(|e| e.to_string())?;
        let loopback = matches!(
            endpoint.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback) {
            return Err("LLM endpoint must use HTTPS (or localhost HTTP)".into());
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
        {
            return Err("LLM endpoint must not contain credentials or query parameters".into());
        }
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            endpoint,
            api_key: config.api_key,
        })
    }
}

impl LlmClient for OpenAiResponsesClient {
    fn complete<'a>(
        &'a self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            if !request.task.valid() {
                return Err("invalid LLM task configuration".into());
            }
            let schema_text = request.schema.to_string();
            let input_tokens = TiktokenCounter.count(&request.instructions)
                + TiktokenCounter.count(&request.input)
                + TiktokenCounter.count(&schema_text);
            if input_tokens > request.task.max_input_tokens {
                return Err("LLM request exceeds configured input token limit".into());
            }
            let body = ResponsesRequest {
                model: &request.task.model,
                instructions: &request.instructions,
                input: &request.input,
                max_output_tokens: request.task.max_output_tokens,
                store: false,
                text: TextFormat {
                    format: JsonSchemaFormat {
                        kind: "json_schema",
                        name: &request.schema_name,
                        strict: true,
                        schema: &request.schema,
                    },
                },
            };
            let response = self
                .http
                .post(self.endpoint.clone())
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !response.status().is_success() {
                return Err(format!("LLM HTTP status {}", response.status()));
            }
            let mut response = response;
            let mut bytes = Vec::new();
            const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
            while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
                if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err("LLM response exceeded size limit".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            let body: ResponsesResult =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if body.status != "completed" {
                return Err("LLM response was not completed".into());
            }
            body.output
                .into_iter()
                .flat_map(|item| item.content)
                .find(|content| content.kind == "output_text")
                .and_then(|content| content.text)
                .ok_or("LLM response has no output text".into())
        })
    }
}
