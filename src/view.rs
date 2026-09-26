use crate::context::{ContextId, ContextObject, ScopeId};
use crate::heap::ZoneKind;

mod builder;
pub(crate) use builder::ViewBuilder;
pub use builder::ViewError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenSpace(pub usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextViewItem {
    pub scope: ScopeId,
    pub zone: Option<ZoneKind>,
    pub object: ContextObject,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColdScopeSummaryView {
    pub content: String,
    pub references: Vec<ContextId>,
    pub covered_objects: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColdScopeView {
    pub scope: ScopeId,
    pub object_count: usize,
    pub summaries: Vec<ColdScopeSummaryView>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextView {
    pub items: Vec<ContextViewItem>,
    pub cold_scopes: Vec<ColdScopeView>,
    pub used_tokens: usize,
}
