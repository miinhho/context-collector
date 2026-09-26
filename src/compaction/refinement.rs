use crate::context::{ContextId, ContextItem, ScopeId, SourceSpan};
use crate::error::TaskFuture;
use crate::heap::{ContextHeap, ZoneKind};
use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RefinementValidationError {
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
    #[error("Info draft requires content and RawInfo sources")]
    EmptyProposal,
    #[error("Info source is outside the prepared cohort")]
    SourceOutsideCohort,
    #[error("invalid Raw source span")]
    InvalidSourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawInfoInput {
    pub id: ContextId,
    pub revision: u64,
    pub content: String,
    pub born_turn: u64,
    pub last_used_turn: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InfoDraft<Data = ()> {
    pub content: String,
    pub sources: Vec<SourceSpan>,
    pub data: Data,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefinementResult<Data = ()> {
    pub infos: Vec<InfoDraft<Data>>,
    /// RawInfo inputs for which this pass needs no further refinement in this Zone.
    pub settled: Vec<ContextId>,
}

impl<Data> From<Vec<InfoDraft<Data>>> for RefinementResult<Data> {
    fn from(infos: Vec<InfoDraft<Data>>) -> Self {
        Self {
            infos,
            settled: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RefinementInput<'a> {
    pub scope: ScopeId,
    pub zone: ZoneKind,
    pub raw: &'a [RawInfoInput],
}

pub trait InfoRefiner<Data = ()>: Send + Sync {
    fn refine<'a>(&'a self, input: RefinementInput<'a>) -> TaskFuture<'a, RefinementResult<Data>>;
}

#[derive(Default)]
pub struct NoopInfoRefiner;

impl<Data: Send + Sync> InfoRefiner<Data> for NoopInfoRefiner {
    fn refine<'a>(&'a self, input: RefinementInput<'a>) -> TaskFuture<'a, RefinementResult<Data>> {
        Box::pin(async move {
            Ok(RefinementResult {
                infos: Vec::new(),
                settled: input.raw.iter().map(|raw| raw.id).collect(),
            })
        })
    }
}

#[derive(Clone, Debug)]
pub struct Refinement {
    zone: ZoneKind,
    scope: ScopeId,
    inputs: Vec<RawInfoInput>,
}

impl Refinement {
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
            let item = &entry.item;
            let crate::context::InfoKind::Raw(raw) = &item.kind else {
                return None;
            };
            inputs.push(RawInfoInput {
                id: *id,
                revision: item.revision,
                content: raw.content.clone(),
                born_turn: entry.born_turn,
                last_used_turn: entry.last_used_turn,
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

    pub fn inputs(&self) -> &[RawInfoInput] {
        &self.inputs
    }

    pub fn validate<Data>(
        &self,
        heap: &ContextHeap<Data>,
        result: &RefinementResult<Data>,
    ) -> Result<(), RefinementValidationError> {
        for input in &self.inputs {
            let entry = heap
                .zone(self.zone)
                .get(input.id)
                .ok_or(RefinementValidationError::SourceMoved)?;
            if entry.scope() != self.scope {
                return Err(RefinementValidationError::ScopeChanged);
            }
            if entry.protected {
                return Err(RefinementValidationError::SourceProtected);
            }
            let item = &entry.item;
            if item.revision != input.revision {
                return Err(RefinementValidationError::RevisionChanged);
            }
            let crate::context::InfoKind::Raw(raw) = &item.kind else {
                return Err(RefinementValidationError::SourceNoLongerRaw);
            };
            if raw.content != input.content {
                return Err(RefinementValidationError::ContentChanged);
            }
        }
        for id in &result.settled {
            if !self.inputs.iter().any(|input| input.id == *id) {
                return Err(RefinementValidationError::SourceOutsideCohort);
            }
        }
        for proposal in &result.infos {
            if proposal.content.is_empty() || proposal.sources.is_empty() {
                return Err(RefinementValidationError::EmptyProposal);
            }
            for span in &proposal.sources {
                let input = self
                    .inputs
                    .iter()
                    .find(|input| input.id == span.raw)
                    .ok_or(RefinementValidationError::SourceOutsideCohort)?;
                if span.revision != input.revision
                    || span.start >= span.end
                    || span.end > input.content.len()
                    || !input.content.is_char_boundary(span.start)
                    || !input.content.is_char_boundary(span.end)
                {
                    return Err(RefinementValidationError::InvalidSourceSpan);
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn make_info<Data>(id: ContextId, proposal: InfoDraft<Data>) -> ContextItem<Data> {
    ContextItem::info(id, proposal.content, proposal.sources, proposal.data)
}
