//! Fleet storage-plan records and deterministic state transitions.
//!
//! Prepared updates retain the node's validation-before-lock boundary. Apply
//! them under the host's own synchronization and return the host persistence
//! result from the synchronous callback. No path, mutex or authority discovery
//! is owned here. In particular, a peer's management permission is a trusted
//! caller input, never a property inferred from a received record.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const MAX_ALLOCATIONS: usize = 512;
const MAX_DEVICE_INTENTS: usize = 512;
pub const PLAN_CHUNK: usize = 16;

#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct PlanStamp {
    pub counter: u64,
    pub actor: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StoragePolicy {
    #[serde(alias = "ordinaryReplicas")]
    pub replicas: u8,
    pub reserve_percent: u8,
    pub version_retention_days: u16,
    pub rebalance_gib_per_day: u32,
    pub pause_on_metered: bool,
}

impl Default for StoragePolicy {
    fn default() -> Self {
        Self {
            replicas: 2,
            reserve_percent: 10,
            version_retention_days: 30,
            rebalance_gib_per_day: 50,
            pause_on_metered: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRecord {
    pub value: StoragePolicy,
    pub stamp: PlanStamp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageAllocation {
    pub id: String,
    pub device: String,
    pub volume: String,
    pub quota_bytes: u64,
    pub enabled: bool,
    pub stamp: PlanStamp,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceServiceRole {
    #[default]
    Automatic,
    AlwaysOn,
    Personal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceServiceIntent {
    pub device: String,
    pub role: DeviceServiceRole,
    pub stamp: PlanStamp,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePlanSnapshot {
    pub policy: PolicyRecord,
    pub allocations: Vec<StorageAllocation>,
    pub device_intents: Vec<DeviceServiceIntent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoragePlanMessage {
    Patch {
        policy: Option<PolicyRecord>,
        allocations: Vec<StorageAllocation>,
        #[serde(default)]
        device_intents: Vec<DeviceServiceIntent>,
    },
    Digest {
        digest: String,
    },
    SyncRequest,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename = "Persisted")]
pub struct PlanState {
    policy: PolicyRecord,
    allocations: BTreeMap<String, StorageAllocation>,
    device_intents: BTreeMap<String, DeviceServiceIntent>,
    counters: BTreeMap<String, u64>,
}

impl PlanState {
    /// Sanitize a loaded state exactly as the original host store did.
    ///
    /// Deserialization alone does not sanitize. Counter entries are deliberately
    /// retained, including entries not represented by a surviving record.
    pub fn sanitize(self) -> Self {
        let mut inner = self;
        if !valid_policy_record(&inner.policy) {
            inner.policy = PolicyRecord::default();
        }
        inner
            .allocations
            .retain(|id, allocation| id == &allocation.id && valid_allocation(allocation));
        inner
            .device_intents
            .retain(|device, intent| device == &intent.device && valid_device_intent(intent));
        if inner.allocations.len() > MAX_ALLOCATIONS {
            inner.allocations = inner
                .allocations
                .into_iter()
                .take(MAX_ALLOCATIONS)
                .collect();
        }
        if inner.device_intents.len() > MAX_DEVICE_INTENTS {
            inner.device_intents = inner
                .device_intents
                .into_iter()
                .take(MAX_DEVICE_INTENTS)
                .collect();
        }
        inner
    }

    pub fn snapshot(&self) -> StoragePlanSnapshot {
        StoragePlanSnapshot {
            policy: self.policy.clone(),
            allocations: self.allocations.values().cloned().collect(),
            device_intents: self.device_intents.values().cloned().collect(),
        }
    }

    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }
}

/// A policy value checked before entering the host's critical section.
pub struct PolicyUpdate<'a> {
    actor: &'a str,
    value: StoragePolicy,
}

impl<'a> PolicyUpdate<'a> {
    pub fn new(actor: &'a str, value: StoragePolicy) -> Result<Self, String> {
        validate_policy(&value)?;
        Ok(Self { actor, value })
    }

    /// Apply and persist once. A failed write restores only the policy record;
    /// the advanced actor counter remains, matching the original setter.
    pub fn apply(
        self,
        inner: &mut PlanState,
        persist: impl FnOnce(&PlanState) -> Result<(), String>,
    ) -> Result<PolicyRecord, String> {
        let Self { actor, value } = self;
        let stamp = next_stamp(inner, actor)?;
        let previous = inner.policy.clone();
        inner.policy = PolicyRecord { value, stamp };
        if let Err(error) = persist(inner) {
            inner.policy = previous;
            return Err(error);
        }
        Ok(inner.policy.clone())
    }
}

/// An allocation identity checked before entering the host's critical section.
/// Quota and stamp validation intentionally remain in `apply`, after the clock.
pub struct AllocationUpdate<'a> {
    actor: &'a str,
    id: String,
    device: String,
    volume: String,
    quota_bytes: u64,
    enabled: bool,
}

impl<'a> AllocationUpdate<'a> {
    pub fn new(
        actor: &'a str,
        device: String,
        volume: String,
        quota_bytes: u64,
        enabled: bool,
    ) -> Result<Self, String> {
        let id = allocation_id(&device, &volume)?;
        Ok(Self {
            actor,
            id,
            device,
            volume,
            quota_bytes,
            enabled,
        })
    }

    /// A failed write restores the previous allocation, but retains the clock.
    pub fn apply(
        self,
        inner: &mut PlanState,
        persist: impl FnOnce(&PlanState) -> Result<(), String>,
    ) -> Result<StorageAllocation, String> {
        let Self {
            actor,
            id,
            device,
            volume,
            quota_bytes,
            enabled,
        } = self;
        if !inner.allocations.contains_key(&id) && inner.allocations.len() >= MAX_ALLOCATIONS {
            return Err("the fleet storage plan has too many allocations".into());
        }
        let stamp = next_stamp(inner, actor)?;
        let allocation = StorageAllocation {
            id: id.clone(),
            device,
            volume,
            quota_bytes,
            enabled,
            stamp,
        };
        if !valid_allocation(&allocation) {
            return Err("invalid storage allocation".into());
        }
        let previous = inner.allocations.insert(id.clone(), allocation.clone());
        if let Err(error) = persist(inner) {
            match previous {
                Some(previous) => {
                    inner.allocations.insert(id, previous);
                }
                None => {
                    inner.allocations.remove(&id);
                }
            }
            return Err(error);
        }
        Ok(allocation)
    }
}

/// A device identity checked before entering the host's critical section.
pub struct DeviceIntentUpdate<'a> {
    actor: &'a str,
    device: String,
    role: DeviceServiceRole,
}

impl<'a> DeviceIntentUpdate<'a> {
    pub fn new(actor: &'a str, device: String, role: DeviceServiceRole) -> Result<Self, String> {
        valid_device(&device)?;
        Ok(Self {
            actor,
            device,
            role,
        })
    }

    /// A failed write restores the previous device intent, but retains the clock.
    pub fn apply(
        self,
        inner: &mut PlanState,
        persist: impl FnOnce(&PlanState) -> Result<(), String>,
    ) -> Result<DeviceServiceIntent, String> {
        let Self {
            actor,
            device,
            role,
        } = self;
        if !inner.device_intents.contains_key(&device)
            && inner.device_intents.len() >= MAX_DEVICE_INTENTS
        {
            return Err("the fleet storage plan has too many device roles".into());
        }
        let stamp = next_stamp(inner, actor)?;
        let intent = DeviceServiceIntent {
            device: device.clone(),
            role,
            stamp,
        };
        let previous = inner.device_intents.insert(device.clone(), intent.clone());
        if let Err(error) = persist(inner) {
            match previous {
                Some(previous) => {
                    inner.device_intents.insert(device, previous);
                }
                None => {
                    inner.device_intents.remove(&device);
                }
            }
            return Err(error);
        }
        Ok(intent)
    }
}

/// An authenticated peer patch with the original outer bounds checked.
///
/// The caller must supply the authenticated sender and its actual management
/// permission. This type grants no authority and does not deserialize from wire.
pub struct PeerPatch<'a> {
    sender: &'a str,
    sender_may_manage: bool,
    policy: Option<PolicyRecord>,
    allocations: Vec<StorageAllocation>,
    device_intents: Vec<DeviceServiceIntent>,
}

impl<'a> PeerPatch<'a> {
    pub fn new(
        sender: &'a str,
        sender_may_manage: bool,
        policy: Option<PolicyRecord>,
        allocations: Vec<StorageAllocation>,
        device_intents: Vec<DeviceServiceIntent>,
    ) -> Option<Self> {
        if sender.is_empty()
            || allocations.len() > MAX_ALLOCATIONS
            || device_intents.len() > MAX_DEVICE_INTENTS
        {
            return None;
        }
        Some(Self {
            sender,
            sender_may_manage,
            policy,
            allocations,
            device_intents,
        })
    }

    /// Persist only a changed merge. Unlike setters, failed persistence restores
    /// the entire previous state, including every counter.
    pub fn apply(
        self,
        inner: &mut PlanState,
        persist: impl FnOnce(&PlanState) -> Result<(), String>,
    ) -> bool {
        let Self {
            sender,
            sender_may_manage,
            policy,
            allocations,
            device_intents,
        } = self;
        let previous = inner.clone();
        let mut changed = false;
        if sender_may_manage {
            if let Some(policy) = policy.filter(valid_policy_record) {
                // Managers may relay records; members must remain original authors.
                if policy.stamp > inner.policy.stamp {
                    inner.policy = policy;
                    changed = true;
                }
            }
        }
        for allocation in allocations {
            let authorized = sender_may_manage
                || (allocation.device == sender && allocation.stamp.actor == sender);
            if !authorized || !valid_allocation(&allocation) {
                continue;
            }
            let newer = inner
                .allocations
                .get(&allocation.id)
                .is_none_or(|current| allocation.stamp > current.stamp);
            if newer
                && (inner.allocations.contains_key(&allocation.id)
                    || inner.allocations.len() < MAX_ALLOCATIONS)
            {
                inner
                    .counters
                    .entry(allocation.stamp.actor.clone())
                    .and_modify(|counter| *counter = (*counter).max(allocation.stamp.counter))
                    .or_insert(allocation.stamp.counter);
                inner.allocations.insert(allocation.id.clone(), allocation);
                changed = true;
            }
        }
        for intent in device_intents {
            let authorized =
                sender_may_manage || (intent.device == sender && intent.stamp.actor == sender);
            if !authorized || !valid_device_intent(&intent) {
                continue;
            }
            let newer = inner
                .device_intents
                .get(&intent.device)
                .is_none_or(|current| intent.stamp > current.stamp);
            if newer
                && (inner.device_intents.contains_key(&intent.device)
                    || inner.device_intents.len() < MAX_DEVICE_INTENTS)
            {
                inner
                    .counters
                    .entry(intent.stamp.actor.clone())
                    .and_modify(|counter| *counter = (*counter).max(intent.stamp.counter))
                    .or_insert(intent.stamp.counter);
                inner.device_intents.insert(intent.device.clone(), intent);
                changed = true;
            }
        }
        if changed && persist(inner).is_err() {
            *inner = previous;
            return false;
        }
        changed
    }
}

fn next_stamp(inner: &mut PlanState, actor: &str) -> Result<PlanStamp, String> {
    if actor.is_empty() || actor.len() > 512 {
        return Err("invalid storage-plan actor".into());
    }
    let observed = inner
        .allocations
        .values()
        .map(|allocation| allocation.stamp.counter)
        .chain(
            inner
                .device_intents
                .values()
                .map(|intent| intent.stamp.counter),
        )
        .chain(std::iter::once(inner.policy.stamp.counter))
        .max()
        .unwrap_or_default();
    let counter = inner
        .counters
        .get(actor)
        .copied()
        .unwrap_or_default()
        .max(observed)
        .checked_add(1)
        .ok_or("storage-plan clock exhausted")?;
    inner.counters.insert(actor.into(), counter);
    Ok(PlanStamp {
        counter,
        actor: actor.into(),
    })
}

pub fn allocation_id(device: &str, volume: &str) -> Result<String, String> {
    if device.is_empty()
        || volume.is_empty()
        || device.len() > 512
        || volume.len() > 512
        || device.contains('\0')
        || volume.contains('\0')
    {
        return Err("invalid storage resource identity".into());
    }
    Ok(format!("{}:{device}{volume}", device.len()))
}

pub fn valid_device(device: &str) -> Result<(), String> {
    if device.is_empty() || device.len() > 512 || device.contains('\0') {
        return Err("invalid device identity".into());
    }
    Ok(())
}

pub fn valid_device_intent(intent: &DeviceServiceIntent) -> bool {
    valid_device(&intent.device).is_ok()
        && intent.stamp.counter > 0
        && !intent.stamp.actor.is_empty()
        && intent.stamp.actor.len() <= 512
}

pub fn valid_allocation(allocation: &StorageAllocation) -> bool {
    allocation_id(&allocation.device, &allocation.volume).is_ok_and(|id| id == allocation.id)
        && allocation.quota_bytes > 0
        && allocation.stamp.counter > 0
        && !allocation.stamp.actor.is_empty()
        && allocation.stamp.actor.len() <= 512
}

pub fn valid_policy_record(policy: &PolicyRecord) -> bool {
    validate_policy(&policy.value).is_ok()
        && (policy.stamp == PlanStamp::default()
            || (policy.stamp.counter > 0
                && !policy.stamp.actor.is_empty()
                && policy.stamp.actor.len() <= 512))
}

pub fn validate_policy(policy: &StoragePolicy) -> Result<(), String> {
    if !(1..=8).contains(&policy.replicas)
        || !(5..=50).contains(&policy.reserve_percent)
        || policy.version_retention_days > 3650
        || policy.rebalance_gib_per_day > 10_000
    {
        return Err("storage policy is outside its safe bounds".into());
    }
    Ok(())
}
