use super::*;

impl RuntimeState {
    pub fn objectizer_handle(&self) -> Arc<dyn Objectizer> {
        Arc::clone(&self.objectizer)
    }

    pub fn backing_handle(&self) -> Arc<dyn ColdBacking> {
        Arc::clone(&self.backing)
    }

    pub fn summarizer_handle(&self) -> Arc<dyn ScopeSummarizer> {
        Arc::clone(&self.summarizer)
    }

    pub fn prepare_objectization(&self, zone: ZoneKind, scope: ScopeId) -> Option<Objectization> {
        if !self.heap.zone(zone).above_high()
            && (zone == ZoneKind::Cold || self.heap.hot_usage() < self.config.hot_high)
        {
            return None;
        }
        let ids: Vec<_> = self
            .heap
            .zone(zone)
            .ids_for_scope(scope)
            .into_iter()
            .filter(|id| {
                self.heap.zone(zone).get(*id).is_some_and(|entry| {
                    entry.is_raw()
                        && !entry.protected
                        && self.turn.saturating_sub(entry.born_turn) >= 1
                })
            })
            .collect();
        Objectization::prepare(&self.heap, zone, scope, &ids)
    }

    pub fn commit_objectization(
        &mut self,
        prepared: Objectization,
        proposals: Vec<StructuredProposal>,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        prepared
            .validate(&self.heap, &proposals)
            .map_err(RuntimeError::InvalidObjectization)?;
        let zone = prepared.zone();
        let scope = prepared.scope();
        let mut made = Vec::new();
        for proposal in proposals {
            let duplicate = ZoneKind::ALL.iter().any(|kind| {
                self.heap.zone(*kind).entries().any(|entry| {
                    {
                        matches!(&entry.object.representation,
                            crate::context::Representation::Structured { content, sources }
                            if content == &proposal.content && sources == &proposal.sources)
                    }
                })
            });
            if duplicate {
                continue;
            }
            let id = self.fresh_id();
            let tokens = self.counter.count(&proposal.content);
            let object = make_structured(id, proposal);
            self.heap
                .zone_mut(zone)
                .insert(ZoneEntry::new(object, scope, tokens, self.turn))
                .map_err(|_| RuntimeError::Invariant("duplicate Structured id"))?;
            if !self.scopes.add(scope, id) {
                return Err(RuntimeError::Invariant(
                    "Structured scope membership failed",
                ));
            }
            if zone == ZoneKind::Cold {
                let entry = self
                    .heap
                    .zone(ZoneKind::Cold)
                    .get(id)
                    .expect("new entry exists");
                self.catalog.record_cold(entry);
            }
            made.push(id);
        }
        Ok(made)
    }

    pub(super) fn objectize(
        &mut self,
        zone: ZoneKind,
        scope: ScopeId,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let Some(prepared) = self.prepare_objectization(zone, scope) else {
            return Ok(Vec::new());
        };
        let proposals = self
            .objectizer
            .extract(prepared.inputs())
            .map_err(RuntimeError::Objectizer)?;
        self.commit_objectization(prepared, proposals)
    }

    pub fn prepare_cold_compaction(&self, scope: ScopeId) -> ColdCompactionBatch {
        if !self.heap.zone(ZoneKind::Cold).above_high() {
            return ColdCompactionBatch {
                scope,
                records: Vec::new(),
            };
        }
        let mut remaining = self.heap.zone(ZoneKind::Cold).usage().total();
        let low = self.heap.zone(ZoneKind::Cold).watermark().low;
        let mut records = Vec::new();
        for id in self.heap.zone(ZoneKind::Cold).ids_for_scope(scope) {
            if remaining <= low {
                break;
            }
            let entry = self
                .heap
                .zone(ZoneKind::Cold)
                .get(id)
                .expect("listed entry exists");
            if entry.protected {
                continue;
            }
            records.push(BackingRecord {
                object: entry.object.clone(),
                scope: entry.scope,
                tokens: entry.tokens,
            });
            remaining = remaining.saturating_sub(entry.tokens);
        }
        ColdCompactionBatch { scope, records }
    }

    pub fn commit_cold_compaction(
        &mut self,
        verified: VerifiedColdCompactionBatch,
        proposal: Option<ScopeSummaryProposal>,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let batch = verified.into_batch();
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        if let Some(proposal) = &proposal {
            if proposal.content.trim().is_empty() || proposal.references.is_empty() {
                return Err(RuntimeError::InvalidScopeSummary(
                    "summary requires content and references".into(),
                ));
            }
            let covered: BTreeSet<_> = batch.records().iter().map(|r| r.object.id).collect();
            if proposal.references.iter().any(|id| !covered.contains(id)) {
                return Err(RuntimeError::InvalidScopeSummary(
                    "summary references must belong to the selected Scope cohort".into(),
                ));
            }
        }
        for record in batch.records() {
            if record.scope != batch.scope {
                return Err(RuntimeError::Invariant("Cold batch mixed scopes"));
            }
            let current = self
                .heap
                .zone(ZoneKind::Cold)
                .get(record.object.id)
                .ok_or(RuntimeError::Invariant("Cold candidate moved"))?;
            if current.protected
                || current.object != record.object
                || current.scope != record.scope
                || current.tokens != record.tokens
            {
                return Err(RuntimeError::Invariant("Cold candidate changed"));
            }
            let catalog = self
                .catalog
                .get(record.object.id)
                .ok_or(RuntimeError::Invariant(
                    "Cold candidate missing from catalog",
                ))?;
            if catalog.location != CatalogLocation::ColdZone
                || catalog.scope != record.scope
                || catalog.revision != record.object.revision
            {
                return Err(RuntimeError::Invariant("Cold catalog entry changed"));
            }
        }
        let mut stored = Vec::new();
        let coverage = batch
            .records()
            .iter()
            .map(|record| (record.object.id, record.object.revision))
            .collect();
        for record in batch.records {
            self.heap
                .zone_mut(ZoneKind::Cold)
                .remove(record.object.id)
                .ok_or(RuntimeError::Invariant("Cold removal failed"))?;
            self.catalog.record_backing(record.object.id);
            stored.push(record.object.id);
        }
        if let Some(proposal) = proposal {
            self.catalog.add_summary(
                batch.scope,
                ScopeSummary {
                    content: proposal.content,
                    references: proposal.references,
                    coverage,
                },
            );
        }
        Ok(stored)
    }

    pub(super) fn compact_cold(&mut self, scope: ScopeId) -> Result<Vec<ContextId>, RuntimeError> {
        let batch = self.prepare_cold_compaction(scope);
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        let inputs: Vec<_> = batch
            .records()
            .iter()
            .map(|record| ScopeSummaryInput {
                object: record.object.clone(),
            })
            .collect();
        let proposal = self
            .summarizer
            .summarize(scope, &inputs)
            .map_err(RuntimeError::ScopeSummary)?;
        for record in batch.records() {
            self.backing
                .store(&record.object)
                .map_err(RuntimeError::ColdBacking)?;
        }
        let verified = batch
            .verify(self.backing.as_ref())
            .map_err(RuntimeError::ColdBacking)?;
        self.commit_cold_compaction(verified, proposal)
    }
}
