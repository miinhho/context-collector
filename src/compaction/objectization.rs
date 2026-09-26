use crate::context::{ContextId, ContextObject, ScopeId, SourceSpan};
use crate::heap::{ContextHeap, ZoneKind};
use std::future::Future;
use std::pin::Pin;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawInput {
    pub id: ContextId,
    pub revision: u64,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredProposal {
    pub content: String,
    pub sources: Vec<SourceSpan>,
}

pub trait Objectizer: Send + Sync {
    fn extract<'a>(
        &'a self,
        inputs: &'a [RawInput],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<StructuredProposal>, String>> + Send + 'a>>;
}

#[derive(Default)]
pub struct NoopObjectizer;

impl Objectizer for NoopObjectizer {
    fn extract<'a>(
        &'a self,
        _inputs: &'a [RawInput],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<StructuredProposal>, String>> + Send + 'a>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[derive(Clone, Debug)]
pub struct Objectization {
    zone: ZoneKind,
    scope: ScopeId,
    inputs: Vec<RawInput>,
}

impl Objectization {
    pub fn prepare(
        heap: &ContextHeap,
        zone: ZoneKind,
        scope: ScopeId,
        ids: &[ContextId],
    ) -> Option<Self> {
        let mut inputs = Vec::with_capacity(ids.len());
        for id in ids {
            let entry = heap.zone(zone).get(*id)?;
            if entry.scope() != scope {
                return None;
            }
            let object = &entry.object;
            let crate::context::Representation::Raw(content) = &object.representation else {
                return None;
            };
            inputs.push(RawInput {
                id: *id,
                revision: object.revision,
                content: content.clone(),
            });
        }
        if inputs.is_empty() {
            return None;
        }
        Some(Self {
            zone,
            scope,
            inputs,
        })
    }

    pub fn zone(&self) -> ZoneKind {
        self.zone
    }

    pub fn scope(&self) -> ScopeId {
        self.scope
    }

    pub fn inputs(&self) -> &[RawInput] {
        &self.inputs
    }

    pub fn validate(
        &self,
        heap: &ContextHeap,
        proposals: &[StructuredProposal],
    ) -> Result<(), String> {
        for input in &self.inputs {
            let entry = heap
                .zone(self.zone)
                .get(input.id)
                .ok_or("source moved out of zone")?;
            if entry.scope() != self.scope {
                return Err("source scope changed".into());
            }
            let object = &entry.object;
            if object.revision != input.revision {
                return Err("source revision changed".into());
            }
            let crate::context::Representation::Raw(content) = &object.representation else {
                return Err("source is no longer Raw".into());
            };
            if content != &input.content {
                return Err("source content changed".into());
            }
        }
        for proposal in proposals {
            if proposal.content.is_empty() || proposal.sources.is_empty() {
                return Err("Structured requires content and Raw sources".into());
            }
            for span in &proposal.sources {
                let input = self
                    .inputs
                    .iter()
                    .find(|input| input.id == span.raw)
                    .ok_or("source is outside the prepared cohort")?;
                if span.revision != input.revision
                    || span.start >= span.end
                    || span.end > input.content.len()
                    || !input.content.is_char_boundary(span.start)
                    || !input.content.is_char_boundary(span.end)
                {
                    return Err("invalid Raw source span".into());
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn make_structured(id: ContextId, proposal: StructuredProposal) -> ContextObject {
    ContextObject::structured(id, proposal.content, proposal.sources)
}
