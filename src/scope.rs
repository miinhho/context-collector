use std::collections::{BTreeMap, BTreeSet};

use crate::context::{ContextId, ScopeId};

#[derive(Clone, Debug)]
pub struct Scope {
    pub id: ScopeId,
    members: BTreeSet<ContextId>,
    last_transition_out: Option<u64>,
}

impl Scope {
    pub fn members(&self) -> impl Iterator<Item = ContextId> + '_ {
        self.members.iter().copied()
    }

    pub fn last_transition_out(&self) -> Option<u64> {
        self.last_transition_out
    }
}

#[derive(Debug)]
pub struct Scopes {
    items: BTreeMap<ScopeId, Scope>,
    current: ScopeId,
    next_id: u64,
}

impl Default for Scopes {
    fn default() -> Self {
        let initial = ScopeId(1);
        let mut items = BTreeMap::new();
        items.insert(
            initial,
            Scope {
                id: initial,
                members: BTreeSet::new(),
                last_transition_out: None,
            },
        );
        Self {
            items,
            current: initial,
            next_id: 2,
        }
    }
}

impl Scopes {
    pub fn current(&self) -> ScopeId {
        self.current
    }

    pub fn get(&self, id: ScopeId) -> Option<&Scope> {
        self.items.get(&id)
    }

    pub fn all(&self) -> impl Iterator<Item = &Scope> {
        self.items.values()
    }

    pub fn owner_of(&self, object: ContextId) -> Option<ScopeId> {
        self.items
            .values()
            .find(|scope| scope.members.contains(&object))
            .map(|scope| scope.id)
    }

    pub(crate) fn add(&mut self, scope: ScopeId, object: ContextId) -> bool {
        self.items
            .get_mut(&scope)
            .is_some_and(|item| item.members.insert(object))
    }

    pub(crate) fn transition(&mut self, turn: u64) -> ScopeId {
        self.items
            .get_mut(&self.current)
            .expect("current scope exists")
            .last_transition_out = Some(turn);
        let id = ScopeId(self.next_id);
        self.next_id += 1;
        self.items.insert(
            id,
            Scope {
                id,
                members: BTreeSet::new(),
                last_transition_out: None,
            },
        );
        self.current = id;
        id
    }

    pub fn select_existing(&mut self, id: ScopeId) -> bool {
        if self.items.contains_key(&id) {
            self.current = id;
            true
        } else {
            false
        }
    }
}
