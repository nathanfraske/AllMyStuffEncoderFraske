//! Small, durable fleet storage policy.
//!
//! This is deliberately separate from the Files canvas: allocations and
//! durability policy are operational authority, not layout metadata. The plan
//! is bounded by fleet devices/volumes and converges as per-record LWW state.

use std::collections::BTreeMap;
use std::path::PathBuf;

use parking_lot::Mutex;
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
#[serde(default)]
struct Persisted {
    policy: PolicyRecord,
    allocations: BTreeMap<String, StorageAllocation>,
    device_intents: BTreeMap<String, DeviceServiceIntent>,
    counters: BTreeMap<String, u64>,
}

pub struct StoragePlanStore {
    path: Option<PathBuf>,
    inner: Mutex<Persisted>,
}

impl StoragePlanStore {
    fn load_at(path: Option<PathBuf>) -> Self {
        let mut inner: Persisted = path
            .as_ref()
            .map(|path| crate::persist::load_json(path))
            .unwrap_or_default();
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
        Self {
            path,
            inner: Mutex::new(inner),
        }
    }

    pub fn snapshot(&self) -> StoragePlanSnapshot {
        let inner = self.inner.lock();
        StoragePlanSnapshot {
            policy: inner.policy.clone(),
            allocations: inner.allocations.values().cloned().collect(),
            device_intents: inner.device_intents.values().cloned().collect(),
        }
    }

    pub fn digest(&self) -> String {
        let inner = self.inner.lock();
        let bytes = serde_json::to_vec(&*inner).unwrap_or_default();
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }

    pub fn set_policy(&self, actor: &str, value: StoragePolicy) -> Result<PolicyRecord, String> {
        validate_policy(&value)?;
        let mut inner = self.inner.lock();
        let stamp = next_stamp(&mut inner, actor)?;
        let previous = inner.policy.clone();
        inner.policy = PolicyRecord { value, stamp };
        if let Err(error) = persist(&self.path, &inner) {
            inner.policy = previous;
            return Err(error);
        }
        Ok(inner.policy.clone())
    }

    pub fn set_allocation(
        &self,
        actor: &str,
        device: String,
        volume: String,
        quota_bytes: u64,
        enabled: bool,
    ) -> Result<StorageAllocation, String> {
        let id = allocation_id(&device, &volume)?;
        let mut inner = self.inner.lock();
        if !inner.allocations.contains_key(&id) && inner.allocations.len() >= MAX_ALLOCATIONS {
            return Err("the fleet storage plan has too many allocations".into());
        }
        let stamp = next_stamp(&mut inner, actor)?;
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
        if let Err(error) = persist(&self.path, &inner) {
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
    pub fn set_device_intent(
        &self,
        actor: &str,
        device: String,
        role: DeviceServiceRole,
    ) -> Result<DeviceServiceIntent, String> {
        valid_device(&device)?;
        let mut inner = self.inner.lock();
        if !inner.device_intents.contains_key(&device)
            && inner.device_intents.len() >= MAX_DEVICE_INTENTS
        {
            return Err("the fleet storage plan has too many device roles".into());
        }
        let stamp = next_stamp(&mut inner, actor)?;
        let intent = DeviceServiceIntent {
            device: device.clone(),
            role,
            stamp,
        };
        let previous = inner.device_intents.insert(device.clone(), intent.clone());
        if let Err(error) = persist(&self.path, &inner) {
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
        if sender.is_empty()
            || allocations.len() > MAX_ALLOCATIONS
            || device_intents.len() > MAX_DEVICE_INTENTS
        {
            return false;
        }
        let mut inner = self.inner.lock();
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
        if changed && persist(&self.path, &inner).is_err() {
            *inner = previous;
            return false;
        }
        changed
    }

    #[cfg(test)]
    fn memory() -> Self {
        Self::load_at(None)
    }
}

fn next_stamp(inner: &mut Persisted, actor: &str) -> Result<PlanStamp, String> {
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

fn allocation_id(device: &str, volume: &str) -> Result<String, String> {
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

fn valid_device(device: &str) -> Result<(), String> {
    if device.is_empty() || device.len() > 512 || device.contains('\0') {
        return Err("invalid device identity".into());
    }
    Ok(())
}

fn valid_device_intent(intent: &DeviceServiceIntent) -> bool {
    valid_device(&intent.device).is_ok()
        && intent.stamp.counter > 0
        && !intent.stamp.actor.is_empty()
        && intent.stamp.actor.len() <= 512
}

fn valid_allocation(allocation: &StorageAllocation) -> bool {
    allocation_id(&allocation.device, &allocation.volume).is_ok_and(|id| id == allocation.id)
        && allocation.quota_bytes > 0
        && allocation.stamp.counter > 0
        && !allocation.stamp.actor.is_empty()
        && allocation.stamp.actor.len() <= 512
}

fn valid_policy_record(policy: &PolicyRecord) -> bool {
    validate_policy(&policy.value).is_ok()
        && (policy.stamp == PlanStamp::default()
            || (policy.stamp.counter > 0
                && !policy.stamp.actor.is_empty()
                && policy.stamp.actor.len() <= 512))
}

fn validate_policy(policy: &StoragePolicy) -> Result<(), String> {
    if !(1..=8).contains(&policy.replicas)
        || !(5..=50).contains(&policy.reserve_percent)
        || policy.version_retention_days > 3650
        || policy.rebalance_gib_per_day > 10_000
    {
        return Err("storage policy is outside its safe bounds".into());
    }
    Ok(())
}


// Test-only persistence replaces the host I/O function; decision bodies above
// are unmodified frozen source. No source path ever reaches the filesystem.
thread_local! {
    static LOAD_JSON: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    static SAVE_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static SAVES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn persist(path: &Option<PathBuf>, value: &Persisted) -> Result<(), String> {
    assert!(path.is_none(), "frozen oracle must remain memory-only");
    SAVES.with(|saves| saves.borrow_mut().push(serde_json::to_string(value).unwrap()));
    SAVE_ERROR.with(|error| error.borrow().clone().map_or(Ok(()), Err))
}

pub fn load_fixture<T: serde::de::DeserializeOwned + Default>() -> T {
    LOAD_JSON.with(|json| serde_json::from_str(&json.borrow()).unwrap())
}

pub fn with_save_outcome<T>(error: Option<&str>, action: impl FnOnce() -> T) -> (T, Vec<String>) {
    SAVE_ERROR.with(|slot| *slot.borrow_mut() = error.map(str::to_owned));
    SAVES.with(|saves| saves.borrow_mut().clear());
    let outcome = action();
    SAVE_ERROR.with(|slot| *slot.borrow_mut() = None);
    let saves = SAVES.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
    (outcome, saves)
}

impl StoragePlanStore {
    pub fn fixture_memory() -> Self {
        Self::memory()
    }

    pub fn fixture_from_json(input: &str, sanitize: bool) -> Result<Self, serde_json::Error> {
        let inner: Persisted = serde_json::from_str(input)?;
        if !sanitize {
            return Ok(Self { path: None, inner: Mutex::new(inner) });
        }
        LOAD_JSON.with(|slot| *slot.borrow_mut() = input.to_owned());
        // load_at's original sanitize body receives data through load_fixture.
        let mut store = Self::load_at(Some(PathBuf::from("unused-memory-fixture")));
        store.path = None;
        Ok(store)
    }

    pub fn fixture_json(&self) -> String {
        serde_json::to_string(&*self.inner.lock()).unwrap()
    }
}
