use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;

use crate::compaction::objectization::{Objectizer, RawInput, StructuredProposal};
use crate::compaction::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use crate::context::{ContextId, ScopeId, SourceSpan};

use super::{LlmClient, LlmRequest, LlmTaskConfig};

pub struct LlmObjectizer {
    client: Arc<dyn LlmClient>,
    task: LlmTaskConfig,
}

impl LlmObjectizer {
    pub fn new(client: Arc<dyn LlmClient>, task: LlmTaskConfig) -> Result<Self, String> {
        if !task.valid() {
            return Err("invalid objectization LLM configuration".into());
        }
        Ok(Self { client, task })
    }
}

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

impl Objectizer for LlmObjectizer {
    fn extract<'a>(
        &'a self,
        inputs: &'a [RawInput],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<StructuredProposal>, String>> + Send + 'a>> {
        Box::pin(async move {
            let input = serde_json::to_string(
                &inputs
                    .iter()
                    .map(|raw| {
                        json!({
                            "id": raw.id.0,
                            "revision": raw.revision,
                            "content": raw.content,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| e.to_string())?;
            let schema = json!({
                "type": "object", "additionalProperties": false,
                "properties": {"objects": {"type": "array", "items": {
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "content": {"type": "string"},
                        "sources": {"type": "array", "items": {
                            "type": "object", "additionalProperties": false,
                            "properties": {
                                "raw": {"type": "integer"},
                                "revision": {"type": "integer"},
                                "start": {"type": "integer"},
                                "end": {"type": "integer"}
                            },
                            "required": ["raw", "revision", "start", "end"]
                        }}
                    },
                    "required": ["content", "sources"]
                }}},
                "required": ["objects"]
            });
            let text = self.client.complete(LlmRequest {
                task: self.task.clone(),
                instructions: "Extract useful standalone work objects from the supplied Raw context. Do not paraphrase away evidence. Return zero objects when no reliable extraction is possible. Source spans are UTF-8 byte offsets in the supplied Raw content.".into(),
                input,
                schema_name: "context_objects",
                schema,
            }).await?;
            let parsed: ObjectsJson = serde_json::from_str(&text).map_err(|e| e.to_string())?;
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
        })
    }
}

pub struct LlmScopeSummarizer {
    client: Arc<dyn LlmClient>,
    task: LlmTaskConfig,
}

impl LlmScopeSummarizer {
    pub fn new(client: Arc<dyn LlmClient>, task: LlmTaskConfig) -> Result<Self, String> {
        if !task.valid() {
            return Err("invalid scope summary LLM configuration".into());
        }
        Ok(Self { client, task })
    }
}

#[derive(Deserialize)]
struct SummaryJson {
    content: String,
    references: Vec<u64>,
}

impl ScopeSummarizer for LlmScopeSummarizer {
    fn summarize<'a>(
        &'a self,
        scope: ScopeId,
        inputs: &'a [ScopeSummaryInput],
    ) -> Pin<Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let input = serde_json::to_string(&json!({
                "scope": scope.0,
                "objects": inputs.iter().map(|item| json!({
                    "id": item.object.id.0,
                    "revision": item.object.revision,
                    "content": item.object.representation.content(),
                })).collect::<Vec<_>>()
            }))
            .map_err(|e| e.to_string())?;
            let schema = json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "content": {"type": "string"},
                    "references": {"type": "array", "items": {"type": "integer"}}
                },
                "required": ["content", "references"]
            });
            let text = self.client.complete(LlmRequest {
                task: self.task.clone(),
                instructions: "Summarize only the supplied part of this Scope for later navigation. Cite object IDs from the supplied set. Return empty content and references if there is no useful summary. Do not claim coverage beyond these objects.".into(),
                input,
                schema_name: "scope_summary",
                schema,
            }).await?;
            let parsed: SummaryJson = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            if parsed.content.is_empty() && parsed.references.is_empty() {
                return Ok(None);
            }
            Ok(Some(ScopeSummaryProposal {
                content: parsed.content,
                references: parsed.references.into_iter().map(ContextId).collect(),
            }))
        })
    }
}
