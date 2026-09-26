use std::sync::Arc;

use crate::context::{ContextId, ContextObject, ScopeId};
use crate::heap::{ZoneEntry, ZoneKind};
use crate::token::TokenCounter;

use super::{RuntimeError, RuntimeState, ScopeReport, TurnObservation, TurnReceipt};

pub(super) struct TurnRecorder {
    counter: Arc<dyn TokenCounter>,
}

impl TurnRecorder {
    pub fn new(counter: Arc<dyn TokenCounter>) -> Self {
        Self { counter }
    }

    pub fn record<Data, SummaryData>(
        &self,
        state: &mut RuntimeState<Data, SummaryData>,
        user: String,
        agent: String,
        report: TurnObservation,
    ) -> Result<TurnReceipt, RuntimeError> {
        if let Some(uses) = &report.uses {
            for id in uses {
                if state.scopes.owner_of(*id).is_none() {
                    return Err(RuntimeError::UnknownContext(*id));
                }
            }
        }
        state.turn += 1;
        let scope = if report.scope == Some(ScopeReport::Transition) {
            state.scopes.transition(state.turn)
        } else {
            state.scopes.current()
        };
        let user_id = self.insert_raw(state, scope, user)?;
        let agent_id = self.insert_raw(state, scope, agent)?;
        if let Some(uses) = report.uses {
            for id in uses {
                if let Some((zone, _)) = state.heap.find(id)
                    && let Some(entry) = state.heap.zone_mut(zone).get_mut(id)
                {
                    entry.last_used_turn = Some(state.turn);
                }
            }
        }
        state.schedule();
        Ok(TurnReceipt {
            turn: state.turn,
            scope,
            user: user_id,
            agent: agent_id,
        })
    }

    fn insert_raw<Data, SummaryData>(
        &self,
        state: &mut RuntimeState<Data, SummaryData>,
        scope: ScopeId,
        content: String,
    ) -> Result<ContextId, RuntimeError> {
        let id = state.next_context_id();
        let tokens = self.counter.count(&content);
        let object = ContextObject::raw(id, content);
        state
            .heap
            .zone_mut(ZoneKind::Eden)
            .insert(ZoneEntry::new(object, scope, tokens, state.turn))
            .map_err(|_| RuntimeError::Invariant("duplicate Eden id"))?;
        if !state.scopes.add(scope, id) {
            return Err(RuntimeError::Invariant("scope membership insertion failed"));
        }
        Ok(id)
    }
}
