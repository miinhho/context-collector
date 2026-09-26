use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::context::{ContextId, ContextObject};

pub trait ColdBacking: Send + Sync {
    fn store(&self, object: &ContextObject) -> Result<(), String>;
    fn load(&self, id: ContextId) -> Result<Option<ContextObject>, String>;
}

#[derive(Default)]
pub struct InMemoryColdBacking {
    objects: Mutex<BTreeMap<ContextId, ContextObject>>,
}

impl ColdBacking for InMemoryColdBacking {
    fn store(&self, object: &ContextObject) -> Result<(), String> {
        self.objects
            .lock()
            .map_err(|error| error.to_string())?
            .insert(object.id, object.clone());
        Ok(())
    }

    fn load(&self, id: ContextId) -> Result<Option<ContextObject>, String> {
        Ok(self
            .objects
            .lock()
            .map_err(|error| error.to_string())?
            .get(&id)
            .cloned())
    }
}
