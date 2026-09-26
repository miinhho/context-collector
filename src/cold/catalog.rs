use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::context::{ContextId, ProcessingState, ScopeId};
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
    pub processing: ProcessingState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeSummary<SummaryData = ()> {
    pub content: String,
    pub references: Vec<ContextId>,
    pub coverage: Vec<(ContextId, u64)>,
    pub data: SummaryData,
}

pub struct ColdCatalog<SummaryData = ()> {
    entries: BTreeMap<ContextId, ColdCatalogEntry>,
    summaries: BTreeMap<ScopeId, Vec<ScopeSummary<SummaryData>>>,
}

impl<SummaryData> Default for ColdCatalog<SummaryData> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            summaries: BTreeMap::new(),
        }
    }
}

impl<SummaryData> ColdCatalog<SummaryData> {
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

    pub fn summaries(&self, scope: ScopeId) -> &[ScopeSummary<SummaryData>] {
        self.summaries.get(&scope).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn record_cold<Data>(&mut self, entry: &ZoneEntry<Data>) {
        self.entries.insert(
            entry.id,
            ColdCatalogEntry {
                id: entry.id,
                scope: entry.scope,
                revision: entry.item.revision,
                tokens: entry.tokens,
                location: CatalogLocation::ColdZone,
                processing: entry.item.processing.clone(),
            },
        );
    }

    pub(crate) fn record_backing(&mut self, id: ContextId) {
        self.entries
            .get_mut(&id)
            .expect("Cold entry was registered")
            .location = CatalogLocation::Backing;
    }

    pub(crate) fn update_processing(&mut self, id: ContextId, processing: ProcessingState) {
        self.entries
            .get_mut(&id)
            .expect("Cold entry was registered")
            .processing = processing;
    }

    pub(crate) fn add_summary(&mut self, scope: ScopeId, summary: ScopeSummary<SummaryData>) {
        self.summaries.entry(scope).or_default().push(summary);
    }
}
