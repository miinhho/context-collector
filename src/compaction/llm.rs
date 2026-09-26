//! Optional LLM implementations of the compaction contracts.
//! Call strategies own task-specific requests and response parsing.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::compaction::objectization::{Objectizer, RawInput, StructuredProposal};
use crate::compaction::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use crate::context::{ContextId, ScopeId, SourceSpan};
use crate::llm::{LlmClient, LlmRequest, LlmTaskConfig};

pub trait ObjectizationCall: Send + Sync {
    fn request(&self, inputs: &[RawInput]) -> Result<LlmRequest, String>;
    fn parse(&self, response: &str) -> Result<Vec<StructuredProposal>, String>;
}

pub trait ScopeSummaryCall: Send + Sync {
    fn request(&self, scope: ScopeId, inputs: &[ScopeSummaryInput]) -> Result<LlmRequest, String>;
    fn parse(&self, response: &str) -> Result<Option<ScopeSummaryProposal>, String>;
}

pub struct LlmObjectizer {
    client: Arc<dyn LlmClient>,
    call: Arc<dyn ObjectizationCall>,
}

impl LlmObjectizer {
    pub fn new(client: Arc<dyn LlmClient>, call: Arc<dyn ObjectizationCall>) -> Self {
        Self { client, call }
    }
}

impl Objectizer for LlmObjectizer {
    fn extract<'a>(
        &'a self,
        inputs: &'a [RawInput],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<StructuredProposal>, String>> + Send + 'a>> {
        Box::pin(async move {
            let request = self.call.request(inputs)?;
            let response = self.client.complete(request).await?;
            self.call.parse(&response)
        })
    }
}

pub struct LlmScopeSummarizer {
    client: Arc<dyn LlmClient>,
    call: Arc<dyn ScopeSummaryCall>,
}

impl LlmScopeSummarizer {
    pub fn new(client: Arc<dyn LlmClient>, call: Arc<dyn ScopeSummaryCall>) -> Self {
        Self { client, call }
    }
}

impl ScopeSummarizer for LlmScopeSummarizer {
    fn summarize<'a>(
        &'a self,
        scope: ScopeId,
        inputs: &'a [ScopeSummaryInput],
    ) -> Pin<Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let request = self.call.request(scope, inputs)?;
            let response = self.client.complete(request).await?;
            self.call.parse(&response)
        })
    }
}

/// Ready-to-use JSON contract. Replace it with `ObjectizationCall` for a different request,
/// parser, schema, or per-call model selection.
pub struct JsonObjectizationCall {
    pub task: LlmTaskConfig,
    pub instructions: String,
}

impl JsonObjectizationCall {
    pub fn new(task: LlmTaskConfig) -> Result<Self, String> {
        if !task.valid() {
            return Err("invalid objectization LLM configuration".into());
        }
        Ok(Self {
            task,
            instructions: "Extract useful standalone work objects from the supplied Raw context. Do not paraphrase away evidence. Return zero objects when no reliable extraction is possible. Source spans are UTF-8 byte offsets in the supplied Raw content.".into(),
        })
    }
}

#[derive(Serialize)]
struct RawPayload<'a> {
    id: u64,
    revision: u64,
    content: &'a str,
}

const OBJECT_SCHEMA: &str = r#"{
  "type":"object","additionalProperties":false,
  "properties":{"objects":{"type":"array","items":{
    "type":"object","additionalProperties":false,
    "properties":{
      "content":{"type":"string"},
      "sources":{"type":"array","items":{
        "type":"object","additionalProperties":false,
        "properties":{
          "raw":{"type":"integer"},"revision":{"type":"integer"},
          "start":{"type":"integer"},"end":{"type":"integer"}
        },"required":["raw","revision","start","end"]
      }}
    },"required":["content","sources"]
  }}},"required":["objects"]
}"#;

#[derive(Serialize)]
struct ScopePayload<'a> {
    scope: u64,
    objects: Vec<ScopeObjectPayload<'a>>,
}

#[derive(Serialize)]
struct ScopeObjectPayload<'a> {
    id: u64,
    revision: u64,
    content: &'a str,
}

const SUMMARY_SCHEMA: &str = r#"{
  "type":"object","additionalProperties":false,
  "properties":{
    "content":{"type":"string"},
    "references":{"type":"array","items":{"type":"integer"}}
  },"required":["content","references"]
}"#;

#[derive(Deserialize)]
struct SourceJson {
    raw: u64,
    revision: u64,
    start: usize,
    end: usize,
}

#[derive(Deserialize)]
struct ObjectJson {
    content: String,
    sources: Vec<SourceJson>,
}

#[derive(Deserialize)]
struct ObjectsJson {
    objects: Vec<ObjectJson>,
}

impl ObjectizationCall for JsonObjectizationCall {
    fn request(&self, inputs: &[RawInput]) -> Result<LlmRequest, String> {
        let payload: Vec<_> = inputs
            .iter()
            .map(|raw| RawPayload {
                id: raw.id.0,
                revision: raw.revision,
                content: &raw.content,
            })
            .collect();
        let input = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
        let schema = serde_json::from_str(OBJECT_SCHEMA).map_err(|error| error.to_string())?;
        Ok(LlmRequest {
            task: self.task.clone(),
            instructions: self.instructions.clone(),
            input,
            schema_name: "context_objects".into(),
            schema,
        })
    }

    fn parse(&self, response: &str) -> Result<Vec<StructuredProposal>, String> {
        let parsed: ObjectsJson =
            serde_json::from_str(response).map_err(|error| error.to_string())?;
        Ok(parsed
            .objects
            .into_iter()
            .map(|object| StructuredProposal {
                content: object.content,
                sources: object
                    .sources
                    .into_iter()
                    .map(|span| SourceSpan {
                        raw: ContextId(span.raw),
                        revision: span.revision,
                        start: span.start,
                        end: span.end,
                    })
                    .collect(),
            })
            .collect())
    }
}

/// Ready-to-use JSON contract. The task and instructions are user-configurable.
pub struct JsonScopeSummaryCall {
    pub task: LlmTaskConfig,
    pub instructions: String,
}

impl JsonScopeSummaryCall {
    pub fn new(task: LlmTaskConfig) -> Result<Self, String> {
        if !task.valid() {
            return Err("invalid scope summary LLM configuration".into());
        }
        Ok(Self {
            task,
            instructions: "Summarize only the supplied part of this Scope for later navigation. Cite object IDs from the supplied set. Return empty content and references if there is no useful summary. Do not claim coverage beyond these objects.".into(),
        })
    }
}

#[derive(Deserialize)]
struct SummaryJson {
    content: String,
    references: Vec<u64>,
}

impl ScopeSummaryCall for JsonScopeSummaryCall {
    fn request(&self, scope: ScopeId, inputs: &[ScopeSummaryInput]) -> Result<LlmRequest, String> {
        let payload = ScopePayload {
            scope: scope.0,
            objects: inputs
                .iter()
                .map(|item| ScopeObjectPayload {
                    id: item.object.id.0,
                    revision: item.object.revision,
                    content: item.object.representation.content(),
                })
                .collect(),
        };
        let input = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
        let schema = serde_json::from_str(SUMMARY_SCHEMA).map_err(|error| error.to_string())?;
        Ok(LlmRequest {
            task: self.task.clone(),
            instructions: self.instructions.clone(),
            input,
            schema_name: "scope_summary".into(),
            schema,
        })
    }

    fn parse(&self, response: &str) -> Result<Option<ScopeSummaryProposal>, String> {
        let parsed: SummaryJson =
            serde_json::from_str(response).map_err(|error| error.to_string())?;
        if parsed.content.is_empty() && parsed.references.is_empty() {
            return Ok(None);
        }
        Ok(Some(ScopeSummaryProposal {
            content: parsed.content,
            references: parsed.references.into_iter().map(ContextId).collect(),
        }))
    }
}
