use std::collections::VecDeque;

use crate::context::ScopeId;
use crate::heap::{ContextHeap, ZoneKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Job {
    Minor(ZoneKind),
    HotRefinement(ZoneKind, ScopeId),
    Cooling(ZoneKind),
    Major,
    ColdRefinement(ScopeId),
    ColdSummary(ScopeId),
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

    pub fn schedule<Data>(&mut self, heap: &ContextHeap<Data>, hot_high: usize) {
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
                    if block.ids().iter().any(|id| {
                        heap.zone(zone).get(*id).is_some_and(|entry| {
                            entry.is_raw()
                                && !entry.protected
                                && !entry.item.processing.hot_refinement.completed
                                && !entry.item.processing.hot_refinement.exhausted
                        })
                    }) {
                        self.add(Job::HotRefinement(zone, block.scope));
                    }
                }
                self.add(Job::Cooling(zone));
            }
        }
        if heap.zone(ZoneKind::Cooling).above_high() || hot_pressure {
            self.add(Job::Major);
        }
        for block in heap.zone(ZoneKind::Cold).blocks() {
            let entries: Vec<_> = block
                .ids()
                .iter()
                .filter_map(|id| heap.zone(ZoneKind::Cold).get(*id))
                .filter(|entry| !entry.protected)
                .collect();
            if entries.iter().any(|entry| {
                entry.is_raw()
                    && !entry.item.processing.cold_refinement.completed
                    && !entry.item.processing.cold_refinement.exhausted
            }) {
                self.add(Job::ColdRefinement(block.scope));
            }
            if entries.iter().any(|entry| {
                !entry.item.processing.cold_summary.completed
                    && !entry.item.processing.cold_summary.exhausted
            }) {
                self.add(Job::ColdSummary(block.scope));
            }
            if heap.zone(ZoneKind::Cold).above_high()
                && entries.iter().any(|entry| {
                    entry.item.processing.cold_summary.completed
                        || entry.item.processing.cold_summary.exhausted
                })
            {
                self.add(Job::ColdCompaction(block.scope));
            }
        }
    }

    fn add(&mut self, job: Job) {
        if !self.pending.contains(&job) {
            self.pending.push_back(job);
        }
    }
}
