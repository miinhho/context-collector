use std::collections::BTreeSet;
use std::sync::Arc;

use thiserror::Error;

use crate::compaction::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use crate::context::{ContextId, ContextItem, ProcessingFailure, ScopeId};
use crate::error::ExternalError;
use crate::heap::{ContextHeap, ZoneKind};

use super::{ColdCatalog, ScopeSummary};

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
    #[error("Cold summary candidate changed during processing")]
    CandidateChanged,
}

#[derive(Clone, Debug, Error)]
pub enum ColdSummaryError {
    #[error("scope summarizer failed")]
    Summarizer(#[source] ExternalError),
    #[error("scope summarizer returned no summary for a nonempty Cold cohort")]
    NoSummaryReturned,
    #[error(transparent)]
    Invalid(#[from] ColdSummaryValidationError),
}

#[derive(Clone)]
pub(crate) struct ColdSummaryBatch<Data> {
    pub scope: ScopeId,
    pub infos: Vec<ContextItem<Data>>,
}

impl<Data> ColdSummaryBatch<Data> {
    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }
}

pub(crate) struct ColdSummaryManager<Data, SummaryData> {
    summarizer: Arc<dyn ScopeSummarizer<Data, SummaryData>>,
    batch_tokens: usize,
}

impl<Data, SummaryData> ColdSummaryManager<Data, SummaryData>
where
    Data: Clone + PartialEq + Send + Sync + 'static,
    SummaryData: Send + Sync + 'static,
{
    pub fn new(
        summarizer: Arc<dyn ScopeSummarizer<Data, SummaryData>>,
        batch_tokens: usize,
    ) -> Self {
        Self {
            summarizer,
            batch_tokens,
        }
    }

    pub fn prepare(
        &self,
        heap: &ContextHeap<Data>,
        scope: ScopeId,
        turn: u64,
    ) -> ColdSummaryBatch<Data> {
        let mut infos = Vec::new();
        let mut tokens = 0usize;
        for id in heap.zone(ZoneKind::Cold).ids_for_scope(scope) {
            let entry = heap
                .zone(ZoneKind::Cold)
                .get(id)
                .expect("listed Cold entry");
            let attempt = &entry.item.processing.cold_summary;
            if entry.protected
                || attempt.completed
                || attempt.exhausted
                || (attempt.last_failure.is_none() && attempt.last_attempt_turn == Some(turn))
            {
                continue;
            }
            if !infos.is_empty() && tokens.saturating_add(entry.tokens) > self.batch_tokens {
                break;
            }
            tokens = tokens.saturating_add(entry.tokens);
            infos.push(entry.item.clone());
        }
        ColdSummaryBatch { scope, infos }
    }

    pub async fn summarize(
        &self,
        batch: &ColdSummaryBatch<Data>,
    ) -> Result<Option<ScopeSummaryProposal<SummaryData>>, ColdSummaryError> {
        let inputs: Vec<_> = batch
            .infos
            .iter()
            .cloned()
            .map(|info| ScopeSummaryInput { info })
            .collect();
        self.summarizer
            .summarize(batch.scope, &inputs)
            .await
            .map_err(ColdSummaryError::Summarizer)
    }

    pub fn commit(
        heap: &mut ContextHeap<Data>,
        catalog: &mut ColdCatalog<SummaryData>,
        batch: ColdSummaryBatch<Data>,
        proposal: ScopeSummaryProposal<SummaryData>,
        turn: u64,
    ) -> Result<Vec<ContextId>, ColdSummaryError> {
        if proposal.content.trim().is_empty()
            || proposal.references.is_empty()
            || proposal.covered.is_empty()
        {
            return Err(ColdSummaryValidationError::EmptySummary.into());
        }
        let cohort: BTreeSet<_> = batch.infos.iter().map(|info| info.id).collect();
        if proposal.references.iter().any(|id| !cohort.contains(id)) {
            return Err(ColdSummaryValidationError::ReferenceOutsideCohort.into());
        }
        let covered: BTreeSet<_> = proposal.covered.iter().copied().collect();
        if covered.iter().any(|id| !cohort.contains(id)) {
            return Err(ColdSummaryValidationError::CoverageOutsideCohort.into());
        }
        if proposal.references.iter().any(|id| !covered.contains(id)) {
            return Err(ColdSummaryValidationError::ReferenceOutsideCoverage.into());
        }
        for info in &batch.infos {
            let Some(entry) = heap.zone(ZoneKind::Cold).get(info.id) else {
                return Err(ColdSummaryValidationError::CandidateChanged.into());
            };
            if entry.protected
                || entry.scope != batch.scope
                || entry.item.kind != info.kind
                || entry.item.revision != info.revision
            {
                return Err(ColdSummaryValidationError::CandidateChanged.into());
            }
        }
        let coverage = batch
            .infos
            .iter()
            .filter(|info| covered.contains(&info.id))
            .map(|info| (info.id, info.revision))
            .collect();
        catalog.add_summary(
            batch.scope,
            ScopeSummary {
                content: proposal.content,
                references: proposal.references,
                coverage,
                data: proposal.data,
            },
        );
        for info in &batch.infos {
            let entry = heap
                .zone_mut(ZoneKind::Cold)
                .get_mut(info.id)
                .expect("verified");
            if covered.contains(&info.id) {
                entry.item.processing.cold_summary.succeed(turn);
            } else {
                entry.item.processing.cold_summary.defer(turn);
            }
            catalog.update_processing(info.id, entry.item.processing.clone());
        }
        Ok(covered.into_iter().collect())
    }

    pub fn record_failure(
        heap: &mut ContextHeap<Data>,
        catalog: &mut ColdCatalog<SummaryData>,
        batch: &ColdSummaryBatch<Data>,
        turn: u64,
        reason: ProcessingFailure,
        max_failures: u32,
    ) {
        for info in &batch.infos {
            let Some(entry) = heap.zone_mut(ZoneKind::Cold).get_mut(info.id) else {
                continue;
            };
            if entry.protected
                || entry.scope != batch.scope
                || entry.item.revision != info.revision
                || entry.item.kind != info.kind
            {
                continue;
            }
            entry
                .item
                .processing
                .cold_summary
                .fail(turn, reason, max_failures);
            catalog.update_processing(info.id, entry.item.processing.clone());
        }
    }
}
