use std::sync::Arc;

use tokio::sync::Mutex;

use crate::compaction::objectization::{
    Objectization, Objectizer, StructuredProposal, make_structured,
};
use crate::context::{ContextId, Representation, ScopeId};
use crate::heap::{ContextHeap, ZoneEntry, ZoneKind};
use crate::token::TokenCounter;

use super::{RuntimeError, RuntimeState};

pub(super) struct ObjectizationWorker {
    objectizer: Arc<dyn Objectizer>,
    counter: Arc<dyn TokenCounter>,
}

impl ObjectizationWorker {
    pub fn new(objectizer: Arc<dyn Objectizer>, counter: Arc<dyn TokenCounter>) -> Self {
        Self {
            objectizer,
            counter,
        }
    }

    pub async fn execute(
        &self,
        shared: &Arc<Mutex<RuntimeState>>,
        zone: ZoneKind,
        scope: ScopeId,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let prepared = {
            let state = shared.lock().await;
            Self::prepare(&state, zone, scope)
        };
        let Some(prepared) = prepared else {
            return Ok(Vec::new());
        };
        let inputs = prepared.inputs().to_vec();
        let proposals = self
            .objectizer
            .extract(&inputs)
            .await
            .map_err(RuntimeError::Objectizer)?;
        let mut state = shared.lock().await;
        let made = self.commit(&mut state, prepared, proposals)?;
        if !made.is_empty() {
            state.schedule();
        }
        Ok(made)
    }

    fn prepare(state: &RuntimeState, zone: ZoneKind, scope: ScopeId) -> Option<Objectization> {
        if !state.heap.zone(zone).above_high()
            && (zone == ZoneKind::Cold || state.heap.hot_usage() < state.config.hot_high)
        {
            return None;
        }
        let ids: Vec<_> = state
            .heap
            .zone(zone)
            .ids_for_scope(scope)
            .into_iter()
            .filter(|id| {
                state.heap.zone(zone).get(*id).is_some_and(|entry| {
                    entry.is_raw()
                        && !entry.protected
                        && state.turn.saturating_sub(entry.born_turn) >= 1
                })
            })
            .collect();
        Objectization::prepare(&state.heap, zone, scope, &ids)
    }

    fn commit(
        &self,
        state: &mut RuntimeState,
        prepared: Objectization,
        proposals: Vec<StructuredProposal>,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        prepared
            .validate(&state.heap, &proposals)
            .map_err(RuntimeError::InvalidObjectization)?;
        let zone = prepared.zone();
        let scope = prepared.scope();
        let mut made = Vec::new();
        for proposal in proposals {
            if Self::already_present(&state.heap, &proposal) {
                continue;
            }
            let id = state.next_context_id();
            let tokens = self.counter.count(&proposal.content);
            let object = make_structured(id, proposal);
            state
                .heap
                .zone_mut(zone)
                .insert(ZoneEntry::new(object, scope, tokens, state.turn))
                .map_err(|_| RuntimeError::Invariant("duplicate Structured id"))?;
            if !state.scopes.add(scope, id) {
                return Err(RuntimeError::Invariant(
                    "Structured scope membership failed",
                ));
            }
            if zone == ZoneKind::Cold {
                let entry = state
                    .heap
                    .zone(ZoneKind::Cold)
                    .get(id)
                    .expect("new entry exists");
                state.catalog.record_cold(entry);
            }
            made.push(id);
        }
        Ok(made)
    }

    fn already_present(heap: &ContextHeap, proposal: &StructuredProposal) -> bool {
        ZoneKind::ALL.iter().any(|zone| {
            heap.zone(*zone).entries().any(|entry| {
                matches!(&entry.object.representation,
                Representation::Structured { content, sources }
                if content == &proposal.content && sources == &proposal.sources)
            })
        })
    }
}
