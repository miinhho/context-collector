mod openai;

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

pub use openai::{OpenAiClientConfig, OpenAiResponsesClient};

#[derive(Clone, Debug)]
pub struct LlmTaskConfig {
    pub model: String,
    pub max_input_tokens: usize,
    pub max_output_tokens: u32,
}

impl LlmTaskConfig {
    pub fn valid(&self) -> bool {
        !self.model.trim().is_empty() && self.max_input_tokens > 0 && self.max_output_tokens > 0
    }
}

#[derive(Clone, Debug)]
pub struct LlmRequest {
    pub task: LlmTaskConfig,
    pub instructions: String,
    pub input: String,
    pub schema_name: String,
    pub schema: Value,
}

pub trait LlmClient: Send + Sync {
    fn complete<'a>(
        &'a self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}
