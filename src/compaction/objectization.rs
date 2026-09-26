use crate::context::{ContextId, ContextObject, ScopeId, SourceSpan};
use crate::error::TaskFuture;
use crate::heap::{ContextHeap, ZoneKind};
use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ObjectizationValidationError {
    #[error("Raw source moved out of its Zone")]
    SourceMoved,
    #[error("Raw source Scope changed")]
    ScopeChanged,
    #[error("Raw source became protected")]
    SourceProtected,
    #[error("Raw source revision changed")]
    RevisionChanged,
    #[error("Raw source is no longer Raw")]
    SourceNoLongerRaw,
    #[error("Raw source content changed")]
    ContentChanged,
    #[error("Structured proposal requires content and Raw sources")]
    EmptyProposal,
    #[error("Structured source is outside the prepared cohort")]
    SourceOutsideCohort,
    #[error("invalid Raw source span")]
    InvalidSourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawInput {
    pub id: ContextId,
    pub revision: u64,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredProposal<Data = ()> {
    pub content: String,
    pub sources: Vec<SourceSpan>,
    pub data: Data,
}

#[derive(Clone, Copy, Debug)]
pub struct ObjectizationInput<'a> {
    pub scope: ScopeId,
    pub zone: ZoneKind,
    pub raw: &'a [RawInput],
}

pub trait Objectizer<Data = ()>: Send + Sync {
    fn extract<'a>(
        &'a self,
        input: ObjectizationInput<'a>,
    ) -> TaskFuture<'a, Vec<StructuredProposal<Data>>>;
}

#[derive(Default)]
pub struct NoopObjectizer;

impl<Data: Send + Sync> Objectizer<Data> for NoopObjectizer {
    fn extract<'a>(
        &'a self,
        _input: ObjectizationInput<'a>,
    ) -> TaskFuture<'a, Vec<StructuredProposal<Data>>> {
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
    pub fn prepare<Data>(
        heap: &ContextHeap<Data>,
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

    pub fn validate<Data>(
        &self,
        heap: &ContextHeap<Data>,
        proposals: &[StructuredProposal<Data>],
    ) -> Result<(), ObjectizationValidationError> {
        for input in &self.inputs {
            let entry = heap
                .zone(self.zone)
                .get(input.id)
                .ok_or(ObjectizationValidationError::SourceMoved)?;
            if entry.scope() != self.scope {
                return Err(ObjectizationValidationError::ScopeChanged);
            }
            if entry.protected {
                return Err(ObjectizationValidationError::SourceProtected);
            }
            let object = &entry.object;
            if object.revision != input.revision {
                return Err(ObjectizationValidationError::RevisionChanged);
            }
            let crate::context::Representation::Raw(content) = &object.representation else {
                return Err(ObjectizationValidationError::SourceNoLongerRaw);
            };
            if content != &input.content {
                return Err(ObjectizationValidationError::ContentChanged);
            }
        }
        for proposal in proposals {
            if proposal.content.is_empty() || proposal.sources.is_empty() {
                return Err(ObjectizationValidationError::EmptyProposal);
            }
            for span in &proposal.sources {
                let input = self
                    .inputs
                    .iter()
                    .find(|input| input.id == span.raw)
                    .ok_or(ObjectizationValidationError::SourceOutsideCohort)?;
                if span.revision != input.revision
                    || span.start >= span.end
                    || span.end > input.content.len()
                    || !input.content.is_char_boundary(span.start)
                    || !input.content.is_char_boundary(span.end)
                {
                    return Err(ObjectizationValidationError::InvalidSourceSpan);
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn make_structured<Data>(
    id: ContextId,
    proposal: StructuredProposal<Data>,
) -> ContextObject<Data> {
    ContextObject::structured(id, proposal.content, proposal.sources, proposal.data)
}
