//! Small, durable fleet storage policy.
//!
//! This is deliberately separate from the Files canvas: allocations and
//! durability policy are operational authority, not layout metadata. The plan
//! is bounded by fleet devices/volumes and converges as per-record LWW state.

use std::path::PathBuf;

use allmystuff_storage::plan::{
    AllocationUpdate, DeviceIntentUpdate, PeerPatch, PlanState as Persisted, PolicyUpdate,
};
use parking_lot::Mutex;

pub use allmystuff_storage::plan::{
    DeviceServiceIntent, DeviceServiceRole, PlanStamp, PolicyRecord, StorageAllocation,
    StoragePlanMessage, StoragePlanSnapshot, StoragePolicy, PLAN_CHUNK,
};

#[cfg(test)]
use allmystuff_storage::plan::allocation_id;

pub struct StoragePlanStore {
    path: Option<PathBuf>,
    inner: Mutex<Persisted>,
}

impl StoragePlanStore {
    pub fn load() -> Self {
        Self::load_at(
            allmystuff_protocol::myownmesh_state_dir()
                .map(|dir| dir.join("allmystuff-fleet-storage-plan.json")),
        )
    }

    fn load_at(path: Option<PathBuf>) -> Self {
        let inner: Persisted = path
            .as_ref()
            .map(|path| crate::persist::load_json(path))
            .unwrap_or_default();
        Self {
            path,
            inner: Mutex::new(inner.sanitize()),
        }
    }

    pub fn snapshot(&self) -> StoragePlanSnapshot {
        self.inner.lock().snapshot()
    }

    pub fn digest(&self) -> String {
        self.inner.lock().digest()
    }

    pub fn set_policy(&self, actor: &str, value: StoragePolicy) -> Result<PolicyRecord, String> {
        let update = PolicyUpdate::new(actor, value)?;
        let mut inner = self.inner.lock();
        update.apply(&mut inner, |value| persist(&self.path, value))
    }

    pub fn set_allocation(
        &self,
        actor: &str,
        device: String,
        volume: String,
        quota_bytes: u64,
        enabled: bool,
    ) -> Result<StorageAllocation, String> {
        let update = AllocationUpdate::new(actor, device, volume, quota_bytes, enabled)?;
        let mut inner = self.inner.lock();
        update.apply(&mut inner, |value| persist(&self.path, value))
    }

    pub fn set_device_intent(
        &self,
        actor: &str,
        device: String,
        role: DeviceServiceRole,
    ) -> Result<DeviceServiceIntent, String> {
        let update = DeviceIntentUpdate::new(actor, device, role)?;
        let mut inner = self.inner.lock();
        update.apply(&mut inner, |value| persist(&self.path, value))
    }

    /// Merge an authenticated peer patch. A manager may author fleet policy or
    /// any allocation; an ordinary member may author only its own device.
    pub fn merge(
        &self,
        sender: &str,
        sender_may_manage: bool,
        policy: Option<PolicyRecord>,
        allocations: Vec<StorageAllocation>,
        device_intents: Vec<DeviceServiceIntent>,
    ) -> bool {
        let Some(patch) = PeerPatch::new(
            sender,
            sender_may_manage,
            policy,
            allocations,
            device_intents,
        ) else {
            return false;
        };
        let mut inner = self.inner.lock();
        patch.apply(&mut inner, |value| persist(&self.path, value))
    }

    #[cfg(test)]
    fn memory() -> Self {
        Self::load_at(None)
    }
}

fn persist(path: &Option<PathBuf>, value: &Persisted) -> Result<(), String> {
    let Some(path) = path else { return Ok(()) };
    let parent = path.parent().ok_or("storage-plan path has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create storage-plan directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize fleet storage plan: {error}"))?;
    crate::persist::write_atomic(path, &bytes)
        .map_err(|error| format!("save fleet storage plan: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_and_allocations_are_separate_bounded_records() {
        let store = StoragePlanStore::memory();
        let policy = store
            .set_policy(
                "owner",
                StoragePolicy {
                    replicas: 3,
                    ..StoragePolicy::default()
                },
            )
            .unwrap();
        let allocation = store
            .set_allocation("owner", "desk".into(), "disk-1".into(), 1_000, true)
            .unwrap();
        let snapshot = store.snapshot();
        assert_eq!(snapshot.policy, policy);
        assert_eq!(snapshot.allocations, vec![allocation]);
    }
    #[test]
    fn device_role_is_durable_cleanly_serialized_and_backward_compatible() {
        let store = StoragePlanStore::memory();
        let intent = store
            .set_device_intent("owner", "server".into(), DeviceServiceRole::AlwaysOn)
            .unwrap();
        assert_eq!(store.snapshot().device_intents, vec![intent.clone()]);
        assert_eq!(
            serde_json::to_value(intent.role).unwrap(),
            serde_json::json!("alwaysOn")
        );

        let legacy = serde_json::json!({
            "policy": PolicyRecord::default(),
            "allocations": {},
            "counters": {}
        });
        let persisted: Persisted = serde_json::from_value(legacy).unwrap();
        assert!(persisted.snapshot().device_intents.is_empty());
    }

    #[test]
    fn member_cannot_edit_another_device_or_policy() {
        let store = StoragePlanStore::memory();
        let forged_policy = PolicyRecord {
            value: StoragePolicy::default(),
            stamp: PlanStamp {
                counter: 3,
                actor: "member".into(),
            },
        };
        let forged = StorageAllocation {
            id: allocation_id("other", "disk").unwrap(),
            device: "other".into(),
            volume: "disk".into(),
            quota_bytes: 1,
            enabled: true,
            stamp: PlanStamp {
                counter: 4,
                actor: "member".into(),
            },
        };
        let relayed = StorageAllocation {
            id: allocation_id("owner", "archive").unwrap(),
            device: "owner".into(),
            volume: "archive".into(),
            quota_bytes: 1,
            enabled: true,
            stamp: PlanStamp {
                counter: 5,
                actor: "owner".into(),
            },
        };
        assert!(!store.merge(
            "member",
            false,
            Some(forged_policy),
            vec![forged, relayed],
            Vec::new()
        ));
        assert!(store.snapshot().allocations.is_empty());
        assert_eq!(store.snapshot().policy, PolicyRecord::default());
    }

    #[test]
    fn manager_can_relay_records_and_older_patch_is_ignored() {
        let source = StoragePlanStore::memory();
        let policy = source
            .set_policy("owner", StoragePolicy::default())
            .unwrap();
        let allocation = source
            .set_allocation("owner", "laptop".into(), "ssd".into(), 500, true)
            .unwrap();
        let intent = source
            .set_device_intent("owner", "laptop".into(), DeviceServiceRole::Personal)
            .unwrap();
        let target = StoragePlanStore::memory();
        assert!(target.merge(
            "controller",
            true,
            Some(policy.clone()),
            vec![allocation.clone()],
            vec![intent.clone()]
        ));
        assert!(!target.merge(
            "controller",
            true,
            Some(policy),
            vec![allocation],
            vec![intent.clone()]
        ));
        assert_eq!(target.snapshot().device_intents, vec![intent]);
    }

    #[test]
    fn replica_and_reserve_bounds_prevent_nonsensical_policy() {
        let store = StoragePlanStore::memory();
        let bad = StoragePolicy {
            replicas: 0,
            reserve_percent: 0,
            ..StoragePolicy::default()
        };
        assert!(store.set_policy("owner", bad).is_err());
    }

    #[test]
    fn legacy_replica_policy_migrates_to_one_copy_count() {
        let legacy = serde_json::json!({
            "ordinaryReplicas": 4,
            "criticalReplicas": 6,
            "reservePercent": 10,
            "versionRetentionDays": 30,
            "rebalanceGibPerDay": 50,
            "pauseOnMetered": true
        });

        let policy: StoragePolicy = serde_json::from_value(legacy).unwrap();
        assert_eq!(policy.replicas, 4);

        let current = serde_json::to_value(policy).unwrap();
        assert_eq!(current["replicas"], 4);
        assert!(current.get("ordinaryReplicas").is_none());
        assert!(current.get("criticalReplicas").is_none());
    }

    #[test]
    fn allocation_identity_cannot_collide_on_colons() {
        assert_ne!(
            allocation_id("device:volume", "tail").unwrap(),
            allocation_id("device", "volume:tail").unwrap()
        );
    }
}
