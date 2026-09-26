use std::sync::Arc;

use crate::cold::ColdCatalog;
use crate::compaction::objectization::{
    Objectization, Objectizer, StructuredProposal, make_structured,
};
use crate::context::{ContextId, Representation, ScopeId};
use crate::heap::{ContextHeap, ZoneEntry, ZoneKind};
use crate::scope::Scopes;
use crate::token::TokenCounter;

#[derive(Debug)]
pub(crate) enum ObjectizationError {
    Objectizer(String),
    Invalid(String),
    Invariant(&'static str),
}

pub(crate) struct ObjectizationCommit<'a> {
    pub heap: &'a mut ContextHeap,
    pub scopes: &'a mut Scopes,
    pub catalog: &'a mut ColdCatalog,
    pub next_id: &'a mut u64,
    pub turn: u64,
}

pub(crate) struct ObjectizationManager {
    objectizer: Arc<dyn Objectizer>,
    counter: Arc<dyn TokenCounter>,
}

impl ObjectizationManager {
    pub fn new(objectizer: Arc<dyn Objectizer>, counter: Arc<dyn TokenCounter>) -> Self {
        Self {
            objectizer,
            counter,
        }
    }

    pub fn prepare(
        &self,
        heap: &ContextHeap,
        zone: ZoneKind,
        scope: ScopeId,
        turn: u64,
        hot_high: usize,
    ) -> Option<Objectization> {
        if !heap.zone(zone).above_high() && (zone == ZoneKind::Cold || heap.hot_usage() < hot_high)
        {
            return None;
        }
        let ids: Vec<_> = heap
            .zone(zone)
            .ids_for_scope(scope)
            .into_iter()
            .filter(|id| {
                heap.zone(zone).get(*id).is_some_and(|entry| {
                    entry.is_raw() && !entry.protected && turn.saturating_sub(entry.born_turn) >= 1
                })
            })
            .collect();
        Objectization::prepare(heap, zone, scope, &ids)
    }

    pub async fn extract(
        &self,
        prepared: &Objectization,
    ) -> Result<Vec<StructuredProposal>, ObjectizationError> {
        self.objectizer
            .extract(prepared.inputs())
            .await
            .map_err(ObjectizationError::Objectizer)
    }

    pub fn commit(
        &self,
        state: ObjectizationCommit<'_>,
        prepared: Objectization,
        proposals: Vec<StructuredProposal>,
    ) -> Result<Vec<ContextId>, ObjectizationError> {
        let ObjectizationCommit {
            heap,
            scopes,
            catalog,
            next_id,
            turn,
        } = state;
        prepared
            .validate(heap, &proposals)
            .map_err(ObjectizationError::Invalid)?;
        let zone = prepared.zone();
        let scope = prepared.scope();
        let mut made = Vec::new();
        for proposal in proposals {
            if Self::already_present(heap, &proposal) {
                continue;
            }
            let id = ContextId(*next_id);
            *next_id += 1;
            let tokens = self.counter.count(&proposal.content);
            let object = make_structured(id, proposal);
            heap.zone_mut(zone)
                .insert(ZoneEntry::new(object, scope, tokens, turn))
                .map_err(|_| ObjectizationError::Invariant("duplicate Structured id"))?;
            if !scopes.add(scope, id) {
                return Err(ObjectizationError::Invariant(
                    "Structured scope membership failed",
                ));
            }
            if zone == ZoneKind::Cold {
                let entry = heap.zone(ZoneKind::Cold).get(id).expect("new entry exists");
                catalog.record_cold(entry);
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
