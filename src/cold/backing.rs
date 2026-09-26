use crate::error::ExternalError;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::context::{ContextId, ContextItem};

/// Stores the exact item and returns that same revision and payload on later loads.
/// A backing must not mutate an item without changing its revision.
pub trait ColdBacking<Data = ()>: Send + Sync {
    fn store(&self, item: &ContextItem<Data>) -> Result<(), ExternalError>;
    fn load(&self, id: ContextId) -> Result<Option<ContextItem<Data>>, ExternalError>;
}

pub struct InMemoryColdBacking<Data = ()> {
    items: Mutex<BTreeMap<ContextId, ContextItem<Data>>>,
}

impl<Data> Default for InMemoryColdBacking<Data> {
    fn default() -> Self {
        Self {
            items: Mutex::new(BTreeMap::new()),
        }
    }
}

impl<Data: Clone + Send + Sync> ColdBacking<Data> for InMemoryColdBacking<Data> {
    fn store(&self, item: &ContextItem<Data>) -> Result<(), ExternalError> {
        self.items
            .lock()
            .map_err(|error| {
                std::sync::Arc::new(std::io::Error::other(error.to_string())) as ExternalError
            })?
            .insert(item.id, item.clone());
        Ok(())
    }

    fn load(&self, id: ContextId) -> Result<Option<ContextItem<Data>>, ExternalError> {
        Ok(self
            .items
            .lock()
            .map_err(|error| {
                std::sync::Arc::new(std::io::Error::other(error.to_string())) as ExternalError
            })?
            .get(&id)
            .cloned())
    }
}
