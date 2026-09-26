use crate::context::{ContextObject, ScopeId};

use super::backing::ColdBacking;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackingRecord {
    pub object: ContextObject,
    pub scope: ScopeId,
    pub tokens: usize,
}

#[derive(Clone, Debug)]
pub struct ColdCompactionBatch {
    pub(crate) scope: ScopeId,
    pub(crate) records: Vec<BackingRecord>,
}

impl ColdCompactionBatch {
    pub fn scope(&self) -> ScopeId {
        self.scope
    }

    pub fn records(&self) -> &[BackingRecord] {
        &self.records
    }

    pub fn verify(self, backing: &dyn ColdBacking) -> Result<VerifiedColdCompactionBatch, String> {
        for record in &self.records {
            if backing.load(record.object.id)? != Some(record.object.clone()) {
                return Err("stored payload failed exact reload check".into());
            }
        }
        Ok(VerifiedColdCompactionBatch { batch: self })
    }
}

pub struct VerifiedColdCompactionBatch {
    batch: ColdCompactionBatch,
}

impl VerifiedColdCompactionBatch {
    pub(crate) fn into_batch(self) -> ColdCompactionBatch {
        self.batch
    }
}
