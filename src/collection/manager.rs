use crate::context::ContextId;
use crate::heap::{ContextHeap, ZoneKind};
use crate::scope::Scopes;
use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CollectionError {
    #[error("duplicate target context id")]
    DuplicateTarget,
    #[error("source context is missing")]
    MissingSource,
    #[error("collection rollback failed")]
    RollbackFailed,
    #[error("target insertion failed")]
    TargetInsertionFailed,
    #[error("invalid Minor source Zone")]
    InvalidMinorSource,
}

pub(crate) fn minor_candidates<Data>(
    heap: &ContextHeap<Data>,
    zone: ZoneKind,
    turn: u64,
) -> Vec<ContextId> {
    heap.zone(zone)
        .entries()
        .filter(|entry| !entry.protected && entry.born_turn < turn)
        .map(|entry| entry.id)
        .collect()
}

pub(crate) fn cooling_candidates<Data>(
    heap: &ContextHeap<Data>,
    scopes: &Scopes,
    zone: ZoneKind,
    turn: u64,
) -> Vec<ContextId> {
    let mut ids = Vec::new();
    for block in heap.zone(zone).blocks() {
        let Some(transition_turn) = scopes
            .get(block.scope)
            .and_then(|s| s.last_transition_out())
        else {
            continue;
        };
        if scopes.current() == block.scope {
            continue;
        }
        for id in block.ids() {
            let Some(entry) = heap.zone(zone).get(*id) else {
                continue;
            };
            if !entry.protected
                && entry.born_turn < transition_turn
                && turn > transition_turn
                && entry
                    .last_used_turn
                    .is_none_or(|used| used < transition_turn)
            {
                ids.push(*id);
            }
        }
    }
    ids
}

pub struct CollectionManager {
    hot_high: usize,
}

impl CollectionManager {
    pub fn new(hot_high: usize) -> Self {
        Self { hot_high }
    }

    fn move_entry<Data>(
        heap: &mut ContextHeap<Data>,
        id: ContextId,
        source: ZoneKind,
        target: ZoneKind,
    ) -> Result<(), CollectionError> {
        if heap.zone(target).get(id).is_some() {
            return Err(CollectionError::DuplicateTarget);
        }
        let mut entry = heap
            .zone_mut(source)
            .remove(id)
            .ok_or(CollectionError::MissingSource)?;
        entry.collections += 1;
        match heap.zone_mut(target).insert(entry) {
            Ok(()) => Ok(()),
            Err(entry) => {
                heap.zone_mut(source)
                    .insert(*entry)
                    .map_err(|_| CollectionError::RollbackFailed)?;
                Err(CollectionError::TargetInsertionFailed)
            }
        }
    }

    pub(crate) fn minor<Data>(
        &self,
        heap: &mut ContextHeap<Data>,
        source: ZoneKind,
        turn: u64,
    ) -> Result<Vec<ContextId>, CollectionError> {
        if !heap.zone(source).above_high() {
            return Ok(Vec::new());
        }
        let target = match source {
            ZoneKind::Eden => ZoneKind::Survivor,
            ZoneKind::Survivor => ZoneKind::Mature,
            _ => return Err(CollectionError::InvalidMinorSource),
        };
        let mut moved = Vec::new();
        for id in minor_candidates(heap, source, turn) {
            if heap.zone(source).usage().total() <= heap.zone(source).watermark().low {
                break;
            }
            Self::move_entry(heap, id, source, target)?;
            moved.push(id);
        }
        Ok(moved)
    }

    pub(crate) fn cooling<Data>(
        &self,
        heap: &mut ContextHeap<Data>,
        scopes: &Scopes,
        source: ZoneKind,
        turn: u64,
    ) -> Result<Vec<ContextId>, CollectionError> {
        if !heap.zone(source).above_high() && heap.hot_usage() < self.hot_high {
            return Ok(Vec::new());
        }
        let mut moved = Vec::new();
        for id in cooling_candidates(heap, scopes, source, turn) {
            if heap.zone(source).usage().total() <= heap.zone(source).watermark().low
                && heap.hot_usage() < self.hot_high
            {
                break;
            }
            Self::move_entry(heap, id, source, ZoneKind::Cooling)?;
            moved.push(id);
        }
        Ok(moved)
    }

    pub(crate) fn major<Data>(
        &self,
        heap: &mut ContextHeap<Data>,
    ) -> Result<Vec<ContextId>, CollectionError> {
        if !heap.zone(ZoneKind::Cooling).above_high() && heap.hot_usage() < self.hot_high {
            return Ok(Vec::new());
        }
        let ids: Vec<_> = heap
            .zone(ZoneKind::Cooling)
            .entries()
            .filter(|entry| !entry.protected)
            .map(|entry| entry.id)
            .collect();
        let mut moved = Vec::new();
        for id in ids {
            if heap.zone(ZoneKind::Cooling).usage().total()
                <= heap.zone(ZoneKind::Cooling).watermark().low
                && heap.hot_usage() < self.hot_high
            {
                break;
            }
            Self::move_entry(heap, id, ZoneKind::Cooling, ZoneKind::Cold)?;
            moved.push(id);
        }
        Ok(moved)
    }
}
