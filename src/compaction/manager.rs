use crate::error::ExternalError;
use std::sync::Arc;
use thiserror::Error;

use crate::cold::ColdCatalog;
use crate::compaction::refinement::{
    InfoDraft, InfoRefiner, Refinement, RefinementInput, RefinementResult,
    RefinementValidationError, make_info,
};
use crate::context::{ContextId, InfoKind, ScopeId};
use crate::heap::{ContextHeap, ZoneEntry, ZoneKind};
use crate::scope::Scopes;
use crate::token::TokenCounter;

#[derive(Clone, Debug, Error)]
pub enum RefinementError {
    #[error("refiner failed")]
    InfoRefiner(#[source] ExternalError),
    #[error(transparent)]
    Invalid(#[from] RefinementValidationError),
    #[error("refinement invariant failed: {0}")]
    Invariant(&'static str),
}

pub(crate) struct RefinementCommit<'a, Data, SummaryData> {
    pub heap: &'a mut ContextHeap<Data>,
    pub scopes: &'a mut Scopes,
    pub catalog: &'a mut ColdCatalog<SummaryData>,
    pub next_id: &'a mut u64,
    pub turn: u64,
}

pub(crate) struct RefinementManager<Data = ()> {
    refiner: Arc<dyn InfoRefiner<Data>>,
    counter: Arc<dyn TokenCounter>,
    batch_tokens: usize,
}

impl<Data: Clone + PartialEq + Send + Sync + 'static> RefinementManager<Data> {
    pub fn new(
        refiner: Arc<dyn InfoRefiner<Data>>,
        counter: Arc<dyn TokenCounter>,
        batch_tokens: usize,
    ) -> Self {
        Self {
            refiner,
            counter,
            batch_tokens,
        }
    }

    pub fn prepare(
        &self,
        heap: &ContextHeap<Data>,
        zone: ZoneKind,
        scope: ScopeId,
        turn: u64,
        hot_high: usize,
    ) -> Option<Refinement> {
        if zone != ZoneKind::Cold && !heap.zone(zone).above_high() && heap.hot_usage() < hot_high {
            return None;
        }
        let mut candidates: Vec<_> = heap
            .zone(zone)
            .ids_for_scope(scope)
            .into_iter()
            .filter_map(|id| {
                let entry = heap.zone(zone).get(id)?;
                let attempt = if zone == ZoneKind::Cold {
                    &entry.item.processing.cold_refinement
                } else {
                    &entry.item.processing.hot_refinement
                };
                (entry.is_raw()
                    && !entry.protected
                    && !attempt.completed
                    && !attempt.exhausted
                    && (attempt.last_failure.is_some() || attempt.last_attempt_turn != Some(turn))
                    && (zone == ZoneKind::Cold || turn.saturating_sub(entry.born_turn) >= 1))
                    .then_some((id, entry.tokens, entry.last_used_turn, entry.born_turn))
            })
            .collect();
        candidates.sort_by_key(|(_, _, used, born)| (std::cmp::Reverse(*used), *born));
        let mut ids = Vec::new();
        let mut tokens = 0usize;
        for (id, size, _, _) in candidates {
            if !ids.is_empty() && tokens.saturating_add(size) > self.batch_tokens {
                break;
            }
            tokens = tokens.saturating_add(size);
            ids.push(id);
        }
        Refinement::prepare(heap, zone, scope, &ids)
    }

    pub async fn refine(
        &self,
        prepared: &Refinement,
    ) -> Result<RefinementResult<Data>, RefinementError> {
        self.refiner
            .refine(RefinementInput {
                scope: prepared.scope(),
                zone: prepared.zone(),
                raw: prepared.inputs(),
            })
            .await
            .map_err(RefinementError::InfoRefiner)
    }

    pub fn commit<SummaryData>(
        &self,
        state: RefinementCommit<'_, Data, SummaryData>,
        prepared: Refinement,
        result: RefinementResult<Data>,
    ) -> Result<Vec<ContextId>, RefinementError> {
        let RefinementCommit {
            heap,
            scopes,
            catalog,
            next_id,
            turn,
        } = state;
        prepared.validate(heap, &result)?;
        let zone = prepared.zone();
        let scope = prepared.scope();
        let mut made = Vec::new();
        for proposal in result.infos {
            if Self::already_present(heap, &proposal) {
                continue;
            }
            let id = ContextId(*next_id);
            *next_id += 1;
            let tokens = self.counter.count(&proposal.content);
            let info = make_info(id, proposal);
            heap.zone_mut(zone)
                .insert(ZoneEntry::new(info, scope, tokens, turn))
                .map_err(|_| RefinementError::Invariant("duplicate Info id"))?;
            if !scopes.add(scope, id) {
                return Err(RefinementError::Invariant("Info scope membership failed"));
            }
            if zone == ZoneKind::Cold {
                let entry = heap.zone(ZoneKind::Cold).get(id).expect("new entry exists");
                catalog.record_cold(entry);
            }
            made.push(id);
        }
        Ok(made)
    }

    fn already_present(heap: &ContextHeap<Data>, proposal: &InfoDraft<Data>) -> bool {
        ZoneKind::ALL.iter().any(|zone| {
            heap.zone(*zone).entries().any(|entry| {
                matches!(&entry.item.kind,
                InfoKind::Info(info)
                if info.content == proposal.content && info.sources == proposal.sources)
            })
        })
    }
}
