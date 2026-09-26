use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use thiserror::Error;

use crate::cold::{
    BackingRecord, CatalogLocation, ColdBacking, ColdCatalog, ColdCatalogEntry,
    ColdCompactionBatch, ScopeSummary, VerifiedColdCompactionBatch,
};
use crate::collection::{CollectionManager, CollectionScheduler, Job};
use crate::context::{ContextId, ContextObject, ScopeId};
use crate::heap::{ContextHeap, Watermark, ZoneEntry, ZoneKind};
use crate::objectization::{Objectization, Objectizer, StructuredProposal, make_structured};
use crate::scope::Scopes;
use crate::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use crate::token::TokenCounter;
use crate::view::{ColdScopeSummaryView, ColdScopeView, ContextView, ContextViewItem, TokenSpace};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScopeReport {
    Continue,
    Transition,
    Uncertain,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnObservation {
    pub uses: Option<Vec<ContextId>>,
    pub scope: Option<ScopeReport>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub watermarks: [Watermark; 5],
    pub hot_high: usize,
}

impl RuntimeConfig {
    pub fn valid(self) -> bool {
        self.hot_high > 0 && self.watermarks.iter().all(|mark| mark.valid())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RuntimeError {
    #[error("invalid runtime configuration")]
    InvalidConfig,
    #[error("unknown context {0:?}")]
    UnknownContext(ContextId),
    #[error("unknown scope {0:?}")]
    UnknownScope(ScopeId),
    #[error("invalid objectization: {0}")]
    InvalidObjectization(String),
    #[error("objectizer failed: {0}")]
    Objectizer(String),
    #[error("scope summary failed: {0}")]
    ScopeSummary(String),
    #[error("invalid scope summary: {0}")]
    InvalidScopeSummary(String),
    #[error("Cold backing failed: {0}")]
    ColdBacking(String),
    #[error("maintenance worker failed: {0}")]
    Worker(String),
    #[error("runtime invariant failed: {0}")]
    Invariant(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnReceipt {
    pub turn: u64,
    pub scope: ScopeId,
    pub user: ContextId,
    pub agent: ContextId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceResult {
    pub job: Job,
    pub affected: Vec<ContextId>,
}

pub(crate) struct RuntimeState {
    config: RuntimeConfig,
    heap: ContextHeap,
    scopes: Scopes,
    counter: Box<dyn TokenCounter>,
    objectizer: Arc<dyn Objectizer>,
    summarizer: Arc<dyn ScopeSummarizer>,
    backing: Arc<dyn ColdBacking>,
    catalog: ColdCatalog,
    scheduler: CollectionScheduler,
    turn: u64,
    next_id: u64,
}

impl RuntimeState {
    #[cfg(test)]
    pub fn new(
        config: RuntimeConfig,
        counter: Box<dyn TokenCounter>,
        objectizer: Box<dyn Objectizer>,
        backing: Box<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        Self::with_summarizer(
            config,
            counter,
            objectizer,
            Box::new(crate::scope_summary::NoopScopeSummarizer),
            backing,
        )
    }

    pub fn with_summarizer(
        config: RuntimeConfig,
        counter: Box<dyn TokenCounter>,
        objectizer: Box<dyn Objectizer>,
        summarizer: Box<dyn ScopeSummarizer>,
        backing: Box<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        if !config.valid() {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(Self {
            config,
            heap: ContextHeap::new(config.watermarks).ok_or(RuntimeError::InvalidConfig)?,
            scopes: Scopes::default(),
            counter,
            objectizer: Arc::from(objectizer),
            summarizer: Arc::from(summarizer),
            backing: Arc::from(backing),
            catalog: ColdCatalog::default(),
            scheduler: CollectionScheduler::default(),
            turn: 0,
            next_id: 1,
        })
    }

    pub fn heap(&self) -> &ContextHeap {
        &self.heap
    }

    pub fn scopes(&self) -> &Scopes {
        &self.scopes
    }

    pub fn turn(&self) -> u64 {
        self.turn
    }

    pub fn pending_jobs(&self) -> usize {
        self.scheduler.pending()
    }

    pub fn cold_scope_entries(&self, scope: ScopeId) -> Vec<ColdCatalogEntry> {
        self.catalog.entries_for_scope(scope).cloned().collect()
    }

    pub fn select_scope(&mut self, id: ScopeId) -> Result<(), RuntimeError> {
        if self.scopes.select_existing(id) {
            Ok(())
        } else {
            Err(RuntimeError::UnknownScope(id))
        }
    }

    pub fn complete_turn(
        &mut self,
        user: String,
        agent: String,
        report: TurnObservation,
    ) -> Result<TurnReceipt, RuntimeError> {
        if let Some(uses) = &report.uses {
            for id in uses {
                if self.scopes.owner_of(*id).is_none() {
                    return Err(RuntimeError::UnknownContext(*id));
                }
            }
        }
        self.turn += 1;
        let scope = if report.scope == Some(ScopeReport::Transition) {
            self.scopes.transition(self.turn)
        } else {
            self.scopes.current()
        };
        let user_id = self.insert_raw(scope, user)?;
        let agent_id = self.insert_raw(scope, agent)?;
        if let Some(uses) = report.uses {
            for id in uses {
                if let Some((zone, _)) = self.heap.find(id)
                    && let Some(entry) = self.heap.zone_mut(zone).get_mut(id)
                {
                    entry.last_used_turn = Some(self.turn);
                }
            }
        }
        self.reschedule();
        Ok(TurnReceipt {
            turn: self.turn,
            scope,
            user: user_id,
            agent: agent_id,
        })
    }

    fn insert_raw(&mut self, scope: ScopeId, content: String) -> Result<ContextId, RuntimeError> {
        let id = self.fresh_id();
        let tokens = self.counter.count(&content);
        let object = ContextObject::raw(id, content);
        self.heap
            .zone_mut(ZoneKind::Eden)
            .insert(ZoneEntry::new(object, scope, tokens, self.turn))
            .map_err(|_| RuntimeError::Invariant("duplicate Eden id"))?;
        if !self.scopes.add(scope, id) {
            return Err(RuntimeError::Invariant("scope membership insertion failed"));
        }
        Ok(id)
    }

    fn fresh_id(&mut self) -> ContextId {
        let id = ContextId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn read(&self, id: ContextId) -> Result<Option<ContextObject>, RuntimeError> {
        if let Some((_, entry)) = self.heap.find(id) {
            return Ok(Some(entry.object.clone()));
        }
        match self.catalog.get(id) {
            None => return Ok(None),
            Some(entry) if entry.location == CatalogLocation::ColdZone => {
                return Err(RuntimeError::Invariant(
                    "ColdCatalog points to missing Cold entry",
                ));
            }
            Some(_) => {}
        }
        self.backing
            .load(id)
            .map_err(RuntimeError::ColdBacking)?
            .map(|object| {
                if object.id == id {
                    Ok(object)
                } else {
                    Err(RuntimeError::Invariant("backing returned wrong id"))
                }
            })
            .transpose()
    }

    pub fn protect(&mut self, id: ContextId, protected: bool) -> Result<(), RuntimeError> {
        let zone = self
            .heap
            .find(id)
            .map(|(zone, _)| zone)
            .ok_or(RuntimeError::UnknownContext(id))?;
        self.heap
            .zone_mut(zone)
            .get_mut(id)
            .expect("found entry")
            .protected = protected;
        Ok(())
    }
}

mod compaction;
mod maintenance;
mod view;

pub mod async_runtime;

#[cfg(test)]
mod tests;
