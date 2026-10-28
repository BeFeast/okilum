use super::*;
impl Projection {
    pub(super) fn visible(&self, item: &Value) -> bool {
        let run = item["runId"].as_str();
        let status = run
            .and_then(|id| self.runs.get(id))
            .and_then(|run| run["status"].as_str());
        if status == Some("rolled_back")
            || (status == Some("cancelled")
                && item["type"] == "user_message"
                && item["inputIntent"] == "queued_turn")
        {
            return false;
        }
        if item["type"] != "run_interrupt_result" {
            return true;
        }
        let Some(run) = run else {
            return true;
        };
        let Some(node) = item["nodeId"].as_str() else {
            return true;
        };
        let superseded = self.attempts.values().any(|attempt| {
            attempt["runId"] == run
                && attempt["rootNodeId"] == node
                && attempt["status"] == "superseded"
        });
        !superseded
            || self.items.values().any(|candidate| {
                candidate["type"] == "run_interrupt_request" && candidate["runId"] == run
            })
    }
    pub(super) fn prune(&mut self) {
        let removed: Vec<_> = self
            .order
            .iter()
            .filter(|key| {
                let row = &self.rows[*key];
                row.local && !self.visible(&row.item)
            })
            .cloned()
            .collect();
        for key in removed {
            self.remove(&key);
        }
    }
    pub(super) fn below_partial_window(&self, item: &Value) -> bool {
        if !self.metadata.has_more_history && self.metadata.history_cursor.is_none() {
            return false;
        }
        let ordinal = item["ordinal"].as_u64().expect("prepared ordinal");
        if let Some(run) = item["runId"].as_str() {
            if self
                .runs
                .get(run)
                .and_then(|run| run["status"].as_str())
                .is_some_and(|status| {
                    matches!(
                        status,
                        "preparing" | "queued" | "starting" | "running" | "waiting"
                    )
                })
            {
                return false;
            }
            if self.rows.values().any(|row| {
                row.local
                    && row.item["runId"] == run
                    && row.item["ordinal"].as_u64().unwrap_or(0) <= ordinal
            }) {
                return false;
            }
        }
        self.metadata
            .latest_local_turn_ordinal
            .is_some_and(|watermark| ordinal <= watermark)
            || self
                .rows
                .values()
                .filter(|row| row.local)
                .filter_map(|row| row.item["ordinal"].as_u64())
                .min()
                .is_some_and(|oldest| ordinal < oldest)
    }
}
