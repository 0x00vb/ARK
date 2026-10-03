//! Vendor session state in the ARK store. A [`StoreLedger`] keeps each managed session's lane
//! (see [`ark_model::session`]) so a restarted app resumes a healthy vendor session and
//! rehydrates one that was cut off mid-call.

use std::sync::Arc;

use ark_model::session::{LaneRecord, SessionLedger};
use ark_store::Store;

const LANE: &str = "vendor_session";

/// A [`SessionLedger`] in the ARK store. The record's scope is the lane's first `/` part.
pub struct StoreLedger {
    store: Arc<Store>,
}

impl StoreLedger {
    pub fn new(store: Arc<Store>) -> Self {
        StoreLedger { store }
    }
}

impl SessionLedger for StoreLedger {
    fn load(&self, lane: &str) -> Option<LaneRecord> {
        self.store
            .get_record(LANE, lane)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value(v).ok())
    }
    fn save(&self, lane: &str, record: &LaneRecord) {
        if let Ok(v) = serde_json::to_value(record) {
            let scope = lane.split('/').next().unwrap_or("");
            if let Err(e) = self.store.put_record(LANE, lane, scope, &v) {
                tracing::warn!(%lane, "could not save vendor session state: {e}");
            }
        }
    }
    fn remove(&self, lane: &str) {
        let _ = self.store.delete_record(LANE, lane);
    }
}
