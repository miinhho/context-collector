use super::*;

impl RuntimeState {
    #[cfg(test)]
    pub fn run_next_job(&mut self) -> Result<Option<MaintenanceResult>, RuntimeError> {
        let Some(job) = self.take_job() else {
            return Ok(None);
        };
        self.run_job(job).map(Some)
    }

    pub(crate) fn take_job(&mut self) -> Option<Job> {
        self.scheduler.pop()
    }

    pub(crate) fn reschedule(&mut self) {
        self.scheduler
            .schedule(&self.heap, &self.scopes, self.config.hot_high);
    }

    pub(crate) fn run_job(&mut self, job: Job) -> Result<MaintenanceResult, RuntimeError> {
        tracing::debug!(?job, "maintenance job started");
        let affected = match job {
            Job::Minor(source) => CollectionManager::minor(&mut self.heap, source, self.turn)
                .map_err(RuntimeError::Invariant)?,
            Job::Cooling(source) => CollectionManager::cooling(
                &mut self.heap,
                &self.scopes,
                source,
                self.turn,
                self.config.hot_high,
            )
            .map_err(RuntimeError::Invariant)?,
            Job::Major => {
                let moved = CollectionManager::major(&mut self.heap, self.config.hot_high)
                    .map_err(RuntimeError::Invariant)?;
                for id in &moved {
                    let entry = self
                        .heap
                        .zone(ZoneKind::Cold)
                        .get(*id)
                        .expect("moved Cold entry exists");
                    self.catalog.record_cold(entry);
                }
                moved
            }
            Job::HotObjectization(zone, scope) => self.objectize(zone, scope)?,
            Job::ColdObjectization(scope) => self.objectize(ZoneKind::Cold, scope)?,
            Job::ColdCompaction(scope) => self.compact_cold(scope)?,
        };
        if !affected.is_empty() {
            self.reschedule();
        }
        tracing::debug!(?job, affected = affected.len(), "maintenance job completed");
        Ok(MaintenanceResult { job, affected })
    }

    #[cfg(test)]
    pub(super) fn move_entry(
        &mut self,
        id: ContextId,
        source: ZoneKind,
        target: ZoneKind,
    ) -> Result<(), RuntimeError> {
        CollectionManager::move_entry(&mut self.heap, id, source, target)
            .map_err(RuntimeError::Invariant)?;
        if target == ZoneKind::Cold {
            let entry = self
                .heap
                .zone(ZoneKind::Cold)
                .get(id)
                .expect("moved entry exists");
            self.catalog.record_cold(entry);
        }
        Ok(())
    }
}
