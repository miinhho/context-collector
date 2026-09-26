use std::collections::BTreeMap;

use crate::context::{ContextId, ScopeId};
use crate::heap::ZoneEntry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogLocation {
    ColdZone,
    Backing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColdCatalogEntry {
    pub id: ContextId,
    pub scope: ScopeId,
    pub revision: u64,
    pub tokens: usize,
    pub location: CatalogLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeSummary {
    pub content: String,
    pub references: Vec<ContextId>,
    pub coverage: Vec<(ContextId, u64)>,
}

#[derive(Default)]
pub struct ColdCatalog {
    entries: BTreeMap<ContextId, ColdCatalogEntry>,
    summaries: BTreeMap<ScopeId, Vec<ScopeSummary>>,
}

impl ColdCatalog {
    pub fn get(&self, id: ContextId) -> Option<&ColdCatalogEntry> {
        self.entries.get(&id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &ColdCatalogEntry> {
        self.entries.values()
    }

    pub fn entries_for_scope(&self, scope: ScopeId) -> impl Iterator<Item = &ColdCatalogEntry> {
        self.entries
            .values()
            .filter(move |entry| entry.scope == scope)
    }

    pub fn summaries(&self, scope: ScopeId) -> &[ScopeSummary] {
        self.summaries.get(&scope).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn record_cold(&mut self, entry: &ZoneEntry) {
        self.entries.insert(
            entry.id,
            ColdCatalogEntry {
                id: entry.id,
                scope: entry.scope,
                revision: entry.object.revision,
                tokens: entry.tokens,
                location: CatalogLocation::ColdZone,
            },
        );
    }

    pub(crate) fn record_backing(&mut self, id: ContextId) {
        self.entries
            .get_mut(&id)
            .expect("Cold entry was registered")
            .location = CatalogLocation::Backing;
    }

    pub(crate) fn add_summary(&mut self, scope: ScopeId, summary: ScopeSummary) {
        self.summaries.entry(scope).or_default().push(summary);
    }
}
