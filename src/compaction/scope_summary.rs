use crate::context::{ContextId, ContextObject, ScopeId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeSummaryInput {
    pub object: ContextObject,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeSummaryProposal {
    pub content: String,
    pub references: Vec<ContextId>,
}

pub trait ScopeSummarizer: Send + Sync {
    fn summarize(
        &self,
        scope: ScopeId,
        inputs: &[ScopeSummaryInput],
    ) -> Result<Option<ScopeSummaryProposal>, String>;
}

#[derive(Default)]
pub struct NoopScopeSummarizer;

impl ScopeSummarizer for NoopScopeSummarizer {
    fn summarize(
        &self,
        _scope: ScopeId,
        _inputs: &[ScopeSummaryInput],
    ) -> Result<Option<ScopeSummaryProposal>, String> {
        Ok(None)
    }
}
