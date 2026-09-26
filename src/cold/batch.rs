use crate::context::{ContextItem, ScopeId};
use crate::error::ExternalError;

use super::backing::ColdBacking;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackingRecord<Data = ()> {
    pub item: ContextItem<Data>,
    pub scope: ScopeId,
    pub tokens: usize,
}

#[derive(Clone, Debug)]
pub struct ColdCompactionBatch<Data = ()> {
    pub(crate) scope: ScopeId,
    pub(crate) records: Vec<BackingRecord<Data>>,
}

impl<Data: Clone + PartialEq> ColdCompactionBatch<Data> {
    pub fn scope(&self) -> ScopeId {
        self.scope
    }

    pub fn records(&self) -> &[BackingRecord<Data>] {
        &self.records
    }

    pub fn verify(
        self,
        backing: &dyn ColdBacking<Data>,
    ) -> Result<VerifiedColdCompactionBatch<Data>, ExternalError> {
        for record in &self.records {
            if backing.load(record.item.id)? != Some(record.item.clone()) {
                return Err(std::sync::Arc::new(std::io::Error::other(
                    "stored payload failed exact reload check",
                )));
            }
        }
        Ok(VerifiedColdCompactionBatch { batch: self })
    }
}

pub struct VerifiedColdCompactionBatch<Data = ()> {
    batch: ColdCompactionBatch<Data>,
}

impl<Data> VerifiedColdCompactionBatch<Data> {
    pub(crate) fn into_batch(self) -> ColdCompactionBatch<Data> {
        self.batch
    }
}
