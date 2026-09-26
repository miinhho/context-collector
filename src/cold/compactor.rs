use crate::error::ExternalError;
use std::collections::BTreeSet;
use std::sync::Arc;
use thiserror::Error;

use crate::cold::{
    BackingRecord, CatalogLocation, ColdBacking, ColdCompactionBatch, ScopeSummary,
    VerifiedColdCompactionBatch,
};
use crate::compaction::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use crate::context::{ContextId, ScopeId};
use crate::heap::{ContextHeap, ZoneKind};

use super::ColdCatalog;

#[derive(Clone, Debug, Error)]
pub enum ColdCompactorError {
    #[error("scope summarizer failed")]
    Summary(#[source] ExternalError),
    #[error(transparent)]
    InvalidSummary(#[from] ColdSummaryValidationError),
    #[error("Cold backing failed")]
    Backing(#[source] ExternalError),
    #[error("Cold compaction worker failed")]
    Worker(#[source] ExternalError),
    #[error("Cold compaction invariant failed: {0}")]
    Invariant(&'static str),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ColdSummaryValidationError {
    #[error("scope summary requires content and references")]
    EmptySummary,
    #[error("scope summary references must belong to the selected Scope cohort")]
    ReferenceOutsideCohort,
    #[error("scope summary coverage must belong to the selected Scope cohort")]
    CoverageOutsideCohort,
    #[error("scope summary references must be covered by the summary")]
    ReferenceOutsideCoverage,
}

pub(crate) struct ColdCompactor<Data = (), SummaryData = ()> {
    summarizer: Arc<dyn ScopeSummarizer<Data, SummaryData>>,
    backing: Arc<dyn ColdBacking<Data>>,
}

impl<Data, SummaryData> ColdCompactor<Data, SummaryData>
where
    Data: Clone + PartialEq + Send + Sync + 'static,
    SummaryData: Send + Sync + 'static,
{
    pub fn new(
        summarizer: Arc<dyn ScopeSummarizer<Data, SummaryData>>,
        backing: Arc<dyn ColdBacking<Data>>,
    ) -> Self {
        Self {
            summarizer,
            backing,
        }
    }

    pub fn prepare(heap: &ContextHeap<Data>, scope: ScopeId) -> ColdCompactionBatch<Data> {
        if !heap.zone(ZoneKind::Cold).above_high() {
            return ColdCompactionBatch {
                scope,
                records: Vec::new(),
            };
        }
        let mut remaining = heap.zone(ZoneKind::Cold).usage().total();
        let low = heap.zone(ZoneKind::Cold).watermark().low;
        let mut records = Vec::new();
        for id in heap.zone(ZoneKind::Cold).ids_for_scope(scope) {
            if remaining <= low {
                break;
            }
            let entry = heap
                .zone(ZoneKind::Cold)
                .get(id)
                .expect("listed entry exists");
            if entry.protected {
                continue;
            }
            records.push(BackingRecord {
                object: entry.object.clone(),
                scope: entry.scope,
                tokens: entry.tokens,
            });
            remaining = remaining.saturating_sub(entry.tokens);
        }
        ColdCompactionBatch { scope, records }
    }

    pub async fn offload(
        &self,
        batch: ColdCompactionBatch<Data>,
    ) -> Result<
        (
            VerifiedColdCompactionBatch<Data>,
            Option<ScopeSummaryProposal<SummaryData>>,
        ),
        ColdCompactorError,
    > {
        let inputs: Vec<_> = batch
            .records()
            .iter()
            .map(|record| ScopeSummaryInput {
                object: record.object.clone(),
            })
            .collect();
        let proposal = self
            .summarizer
            .summarize(batch.scope, &inputs)
            .await
            .map_err(ColdCompactorError::Summary)?;
        let backing = Arc::clone(&self.backing);
        let verified = tokio::task::spawn_blocking(move || {
            for record in batch.records() {
                backing
                    .store(&record.object)
                    .map_err(ColdCompactorError::Backing)?;
            }
            batch
                .verify(backing.as_ref())
                .map_err(ColdCompactorError::Backing)
        })
        .await
        .map_err(|error| ColdCompactorError::Worker(Arc::new(error)))??;
        Ok((verified, proposal))
    }

    pub fn commit(
        heap: &mut ContextHeap<Data>,
        catalog: &mut ColdCatalog<SummaryData>,
        verified: VerifiedColdCompactionBatch<Data>,
        proposal: Option<ScopeSummaryProposal<SummaryData>>,
    ) -> Result<Vec<ContextId>, ColdCompactorError> {
        let batch = verified.into_batch();
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        if let Some(proposal) = &proposal {
            if proposal.content.trim().is_empty()
                || proposal.references.is_empty()
                || proposal.covered.is_empty()
            {
                return Err(ColdSummaryValidationError::EmptySummary.into());
            }
            let covered: BTreeSet<_> = batch
                .records()
                .iter()
                .map(|record| record.object.id)
                .collect();
            if proposal.references.iter().any(|id| !covered.contains(id)) {
                return Err(ColdSummaryValidationError::ReferenceOutsideCohort.into());
            }
            let declared: BTreeSet<_> = proposal.covered.iter().copied().collect();
            if declared.iter().any(|id| !covered.contains(id)) {
                return Err(ColdSummaryValidationError::CoverageOutsideCohort.into());
            }
            if proposal.references.iter().any(|id| !declared.contains(id)) {
                return Err(ColdSummaryValidationError::ReferenceOutsideCoverage.into());
            }
        }
        for record in batch.records() {
            if record.scope != batch.scope {
                return Err(ColdCompactorError::Invariant("Cold batch mixed scopes"));
            }
            let current = heap
                .zone(ZoneKind::Cold)
                .get(record.object.id)
                .ok_or(ColdCompactorError::Invariant("Cold candidate moved"))?;
            if current.protected
                || current.object != record.object
                || current.scope != record.scope
                || current.tokens != record.tokens
            {
                return Err(ColdCompactorError::Invariant("Cold candidate changed"));
            }
            let catalog = catalog
                .get(record.object.id)
                .ok_or(ColdCompactorError::Invariant(
                    "Cold candidate missing from catalog",
                ))?;
            if catalog.location != CatalogLocation::ColdZone
                || catalog.scope != record.scope
                || catalog.revision != record.object.revision
            {
                return Err(ColdCompactorError::Invariant("Cold catalog entry changed"));
            }
        }
        let coverage = proposal.as_ref().map(|proposal| {
            let declared: BTreeSet<_> = proposal.covered.iter().copied().collect();
            batch
                .records()
                .iter()
                .filter(|record| declared.contains(&record.object.id))
                .map(|record| (record.object.id, record.object.revision))
                .collect()
        });
        let mut stored = Vec::new();
        for record in batch.records {
            heap.zone_mut(ZoneKind::Cold)
                .remove(record.object.id)
                .ok_or(ColdCompactorError::Invariant("Cold removal failed"))?;
            catalog.record_backing(record.object.id);
            stored.push(record.object.id);
        }
        if let Some(proposal) = proposal {
            catalog.add_summary(
                batch.scope,
                ScopeSummary {
                    content: proposal.content,
                    references: proposal.references,
                    coverage: coverage.expect("coverage exists for summary"),
                    data: proposal.data,
                },
            );
        }
        Ok(stored)
    }
}
