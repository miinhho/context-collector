use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cold::ColdCatalog;
use crate::collection::{CollectionScheduler, Job};
use crate::context::{ContextId, ScopeId};
use crate::heap::{ContextHeap, Watermark};
use crate::scope::Scopes;

mod api;
mod cold_compactor;
mod maintenance;
mod objectization;
mod turn;
mod view;

pub use api::Runtime;

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
    #[error("invalid runtime configuration: {0}")]
    InvalidConfigDetail(String),
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

// Canonical mutable state. Work policy and external services live in composed workers.
struct RuntimeState {
    config: RuntimeConfig,
    heap: ContextHeap,
    scopes: Scopes,
    catalog: ColdCatalog,
    scheduler: CollectionScheduler,
    turn: u64,
    next_id: u64,
}

impl RuntimeState {
    fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        if !config.valid() {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(Self {
            config,
            heap: ContextHeap::new(config.watermarks).ok_or(RuntimeError::InvalidConfig)?,
            scopes: Scopes::default(),
            catalog: ColdCatalog::default(),
            scheduler: CollectionScheduler::default(),
            turn: 0,
            next_id: 1,
        })
    }

    fn schedule(&mut self) {
        self.scheduler
            .schedule(&self.heap, &self.scopes, self.config.hot_high);
    }

    fn next_context_id(&mut self) -> ContextId {
        let id = ContextId(self.next_id);
        self.next_id += 1;
        id
    }
}
