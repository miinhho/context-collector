use crate::context::{ContextId, ContextObject, ScopeId};
use std::future::Future;
use std::pin::Pin;

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
    fn summarize<'a>(
        &'a self,
        scope: ScopeId,
        inputs: &'a [ScopeSummaryInput],
    ) -> Pin<Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, String>> + Send + 'a>>;
}

#[derive(Default)]
pub struct NoopScopeSummarizer;

impl ScopeSummarizer for NoopScopeSummarizer {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        _inputs: &'a [ScopeSummaryInput],
    ) -> Pin<Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, String>> + Send + 'a>>
    {
        Box::pin(async { Ok(None) })
    }
}
