use crate::context::{ContextId, ContextObject, ScopeId};
use crate::error::TaskFuture;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeSummaryInput<Data = ()> {
    pub object: ContextObject<Data>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeSummaryProposal<SummaryData = ()> {
    pub content: String,
    pub references: Vec<ContextId>,
    pub covered: Vec<ContextId>,
    pub data: SummaryData,
}

pub trait ScopeSummarizer<Data = (), SummaryData = ()>: Send + Sync {
    fn summarize<'a>(
        &'a self,
        scope: ScopeId,
        inputs: &'a [ScopeSummaryInput<Data>],
    ) -> TaskFuture<'a, Option<ScopeSummaryProposal<SummaryData>>>;
}

#[derive(Default)]
pub struct NoopScopeSummarizer;

impl<Data: Send + Sync, SummaryData: Send + Sync> ScopeSummarizer<Data, SummaryData>
    for NoopScopeSummarizer
{
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        _inputs: &'a [ScopeSummaryInput<Data>],
    ) -> TaskFuture<'a, Option<ScopeSummaryProposal<SummaryData>>> {
        Box::pin(async { Ok(None) })
    }
}
