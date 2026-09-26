use std::collections::VecDeque;

use crate::context::ScopeId;
use crate::heap::{ContextHeap, ZoneKind};
use crate::scope::Scopes;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Job {
    Minor(ZoneKind),
    HotObjectization(ZoneKind, ScopeId),
    Cooling(ZoneKind),
    Major,
    ColdObjectization(ScopeId),
    ColdCompaction(ScopeId),
}

#[derive(Default)]
pub struct CollectionScheduler {
    pending: VecDeque<Job>,
}

impl CollectionScheduler {
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn pop(&mut self) -> Option<Job> {
        self.pending.pop_front()
    }

    pub fn schedule<Data>(&mut self, heap: &ContextHeap<Data>, scopes: &Scopes, hot_high: usize) {
        let hot_pressure = heap.hot_usage() >= hot_high;
        tracing::debug!(
            hot_usage = heap.hot_usage(),
            hot_high,
            hot_pressure,
            "watermark review"
        );
        for zone in [ZoneKind::Eden, ZoneKind::Survivor] {
            if heap.zone(zone).above_high() {
                self.add(Job::Minor(zone));
            }
        }
        for zone in [ZoneKind::Survivor, ZoneKind::Mature] {
            if heap.zone(zone).above_high() || hot_pressure {
                for block in heap.zone(zone).blocks() {
                    if block
                        .ids()
                        .iter()
                        .any(|id| heap.zone(zone).get(*id).is_some_and(|e| e.is_raw()))
                    {
                        self.add(Job::HotObjectization(zone, block.scope));
                    }
                }
                self.add(Job::Cooling(zone));
            }
        }
        if heap.zone(ZoneKind::Cooling).above_high() || hot_pressure {
            self.add(Job::Major);
        }
        if heap.zone(ZoneKind::Cold).above_high() {
            for scope in scopes.all() {
                if heap
                    .zone(ZoneKind::Cold)
                    .ids_for_scope(scope.id)
                    .iter()
                    .any(|id| {
                        heap.zone(ZoneKind::Cold)
                            .get(*id)
                            .is_some_and(|e| e.is_raw())
                    })
                {
                    self.add(Job::ColdObjectization(scope.id));
                }
            }
            for block in heap.zone(ZoneKind::Cold).blocks() {
                if block.ids().iter().any(|id| {
                    heap.zone(ZoneKind::Cold)
                        .get(*id)
                        .is_some_and(|entry| !entry.protected)
                }) {
                    self.add(Job::ColdCompaction(block.scope));
                }
            }
        }
    }

    fn add(&mut self, job: Job) {
        if !self.pending.contains(&job) {
            self.pending.push_back(job);
        }
    }
}
