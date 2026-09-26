use crate::error::ExternalError;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::context::{ContextId, ContextObject};

/// Stores the exact object and returns that same revision and payload on later loads.
/// A backing must not mutate an object without changing its revision.
pub trait ColdBacking<Data = ()>: Send + Sync {
    fn store(&self, object: &ContextObject<Data>) -> Result<(), ExternalError>;
    fn load(&self, id: ContextId) -> Result<Option<ContextObject<Data>>, ExternalError>;
}

pub struct InMemoryColdBacking<Data = ()> {
    objects: Mutex<BTreeMap<ContextId, ContextObject<Data>>>,
}

impl<Data> Default for InMemoryColdBacking<Data> {
    fn default() -> Self {
        Self {
            objects: Mutex::new(BTreeMap::new()),
        }
    }
}

impl<Data: Clone + Send + Sync> ColdBacking<Data> for InMemoryColdBacking<Data> {
    fn store(&self, object: &ContextObject<Data>) -> Result<(), ExternalError> {
        self.objects
            .lock()
            .map_err(|error| {
                std::sync::Arc::new(std::io::Error::other(error.to_string())) as ExternalError
            })?
            .insert(object.id, object.clone());
        Ok(())
    }

    fn load(&self, id: ContextId) -> Result<Option<ContextObject<Data>>, ExternalError> {
        Ok(self
            .objects
            .lock()
            .map_err(|error| {
                std::sync::Arc::new(std::io::Error::other(error.to_string())) as ExternalError
            })?
            .get(&id)
            .cloned())
    }
}
