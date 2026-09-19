//! Host persistence regressions frozen before the storage extraction.
//!
//! Baseline: ac548bddd23f413de60c4edd4b47c8e0b60342e0.
//! storage_plan.rs blob: 367b85c012c322b4fc571af2ddf951625ec07f67.
//! persist.rs blob: 150d824afc7e9d179398dbbd0658779e1bdfe72a.
//! Include as a child of storage_plan to retain its private load_at seam.
//! Tests never discover the ordinary store, construct Mesh, or change env vars.
//! Original fixture draft by A2; completed by C1 after the provider interruption.

use super::{DeviceServiceRole, PlanStamp, PolicyRecord, StoragePlanStore, StoragePolicy};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const EMPTY_DIGEST: &str = "89ff81d50437e30f";
const EMPTY_WITH_OWNER_COUNTER_ONE_DIGEST: &str = "dbfe50029192c55d";
const DEFAULT_DOCUMENT: &str = concat!(
    r#"{"policy":{"value":{"replicas":2,"reservePercent":10,"versionRetentionDays":30,"rebalanceGibPerDay":50,"pauseOnMetered":true},"stamp":{"counter":0,"actor":""}},"#,
    r#""allocations":{},"device_intents":{},"counters":{}}"#,
);

/// Reserve a new directory exclusively; never reuse or pre-delete an old path.
/// Each test owns its entire subtree, including deliberately obstructed paths.
struct TestDirectory {
    root: PathBuf,
    parent: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::temp_dir()
            .canonicalize()
            .expect("temporary parent");
        for _ in 0..4096 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = parent.join(format!(
                "ams-storage-plan-compat-{}-{sequence}",
                std::process::id()
            ));
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&root) {
                Ok(()) => return Self { root, parent },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private storage fixture: {error}"),
            }
        }
        panic!("could not reserve a new storage fixture directory");
    }

    fn path(&self) -> PathBuf {
        self.root.join("plan.json")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        // Both paths came from the canonical temporary parent and an exclusive
        // create_dir. No caller-provided path can become a recursive target.
        assert_eq!(self.root.parent(), Some(self.parent.as_path()));
        if let Err(error) = fs::remove_dir_all(&self.root) {
            if !std::thread::panicking() {
                panic!("remove private storage fixture: {error}");
            }
        }
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().expect("fixture file name").to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn snapshot(store: &StoragePlanStore) -> Value {
    serde_json::to_value(store.snapshot()).expect("snapshot JSON")
}

fn default_snapshot() -> Value {
    json!({
        "policy": {
            "value": {
                "replicas": 2,
                "reservePercent": 10,
                "versionRetentionDays": 30,
                "rebalanceGibPerDay": 50,
                "pauseOnMetered": true
            },
            "stamp": {"counter": 0, "actor": ""}
        },
        "allocations": [],
        "deviceIntents": []
    })
}

fn read_document(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read private plan")).expect("persisted JSON")
}

fn assert_round_trip(store: &StoragePlanStore, path: &Path) {
    let reopened = StoragePlanStore::load_at(Some(path.to_path_buf()));
    assert_eq!(snapshot(&reopened), snapshot(store));
    assert_eq!(reopened.digest(), store.digest());
}

fn policy(replicas: u8) -> StoragePolicy {
    StoragePolicy {
        replicas,
        reserve_percent: 10,
        version_retention_days: 30,
        rebalance_gib_per_day: 50,
        pause_on_metered: true,
    }
}

fn seed(store: &StoragePlanStore) {
    assert_eq!(
        store.set_policy("owner", policy(3)).unwrap().stamp.counter,
        1
    );
    assert_eq!(
        store
            .set_allocation("owner", "desk".into(), "disk".into(), 1_000, true)
            .unwrap()
            .stamp
            .counter,
        2
    );
    assert_eq!(
        store
            .set_device_intent("owner", "desk".into(), DeviceServiceRole::AlwaysOn)
            .unwrap()
            .stamp
            .counter,
        3
    );
}

fn remote_patch(store: &StoragePlanStore) -> bool {
    store.merge(
        "relay",
        true,
        Some(PolicyRecord {
            value: policy(4),
            stamp: PlanStamp {
                counter: 9,
                actor: "remote".into(),
            },
        }),
        vec![super::StorageAllocation {
            id: "6:serverarchive".into(),
            device: "server".into(),
            volume: "archive".into(),
            quota_bytes: 2_000,
            enabled: false,
            stamp: PlanStamp {
                counter: 10,
                actor: "remote".into(),
            },
        }],
        vec![super::DeviceServiceIntent {
            device: "server".into(),
            role: DeviceServiceRole::Personal,
            stamp: PlanStamp {
                counter: 11,
                actor: "remote".into(),
            },
        }],
    )
}

fn assert_io_context(error: &str, prefix: &str) {
    // Freeze the application's exact context; the OS suffix is platform-local.
    assert!(error.starts_with(prefix), "unexpected error: {error}");
    assert!(error.len() > prefix.len(), "missing OS error: {error}");
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    Policy,
    InsertAllocation,
    ReplaceAllocation,
    InsertIntent,
    ReplaceIntent,
}

impl Mutation {
    fn apply(self, store: &StoragePlanStore) -> Result<PlanStamp, String> {
        match self {
            Self::Policy => store.set_policy("owner", policy(5)).map(|v| v.stamp),
            Self::InsertAllocation => store
                .set_allocation("owner", "phone".into(), "flash".into(), 3_000, false)
                .map(|v| v.stamp),
            Self::ReplaceAllocation => store
                .set_allocation("owner", "desk".into(), "disk".into(), 4_000, false)
                .map(|v| v.stamp),
            Self::InsertIntent => store
                .set_device_intent("owner", "phone".into(), DeviceServiceRole::Personal)
                .map(|v| v.stamp),
            Self::ReplaceIntent => store
                .set_device_intent("owner", "desk".into(), DeviceServiceRole::Automatic)
                .map(|v| v.stamp),
        }
    }
}

#[test]
fn absent_file_loads_exact_defaults_without_creating_state() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    assert_eq!(snapshot(&store), default_snapshot());
    assert_eq!(store.digest(), EMPTY_DIGEST);
    assert!(!path.exists());
    assert!(!sibling(&path, ".tmp").exists());
    assert!(!sibling(&path, ".corrupt").exists());
}

#[test]
fn malformed_and_schema_invalid_files_are_quarantined_byte_for_byte() {
    for bytes in [
        b"{\"policy\":".as_slice(),
        b"{\"allocations\":[]}".as_slice(),
    ] {
        let directory = TestDirectory::new();
        let path = directory.path();
        fs::write(&path, bytes).unwrap();
        let store = StoragePlanStore::load_at(Some(path.clone()));
        assert_eq!(snapshot(&store), default_snapshot());
        assert_eq!(store.digest(), EMPTY_DIGEST);
        assert!(!path.exists());
        assert_eq!(fs::read(sibling(&path, ".corrupt")).unwrap(), bytes);
        // Loading the now-absent path does not overwrite the quarantine.
        let again = StoragePlanStore::load_at(Some(path.clone()));
        assert_eq!(again.digest(), EMPTY_DIGEST);
        assert_eq!(fs::read(sibling(&path, ".corrupt")).unwrap(), bytes);
    }
}

#[test]
fn non_utf8_read_failure_defaults_without_quarantining() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let bytes = [0xff, 0xfe, b'{'];
    fs::write(&path, bytes).unwrap();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    assert_eq!(snapshot(&store), default_snapshot());
    assert_eq!(store.digest(), EMPTY_DIGEST);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(!sibling(&path, ".corrupt").exists());
}

#[test]
fn load_sanitizes_before_ordered_caps_without_rewriting_or_pruning_counters() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let mut document: Value = serde_json::from_str(DEFAULT_DOCUMENT).unwrap();
    document["policy"]["value"]["replicas"] = json!(0);
    document["policy"]["stamp"] = json!({"counter": 5_000, "actor": "bad-policy"});
    document["counters"] = json!({"": 901, "owner": 41, "unused": 9_001});
    for index in 0..513 {
        let volume = format!("volume-{index:03}");
        let id = format!("3:dev{volume}");
        let counter = if index == 512 { u64::MAX } else { 7 };
        document["allocations"].as_object_mut().unwrap().insert(
            id.clone(),
            json!({
                "id": id, "device": "dev", "volume": volume,
                "quotaBytes": 256, "enabled": true,
                "stamp": {"counter": counter, "actor": "peer"}
            }),
        );
        let device = format!("device-{index:03}");
        document["device_intents"].as_object_mut().unwrap().insert(
            device.clone(),
            json!({
                "device": device, "role": "automatic",
                "stamp": {"counter": counter, "actor": "peer"}
            }),
        );
    }
    // These sort before valid entries: truncation must happen after filtering.
    document["allocations"]["0-mismatched"] = document["allocations"]["3:devvolume-000"].clone();
    document["allocations"]["3:devbad-quota"] = json!({
        "id": "3:devbad-quota", "device": "dev", "volume": "bad-quota",
        "quotaBytes": 0, "enabled": true,
        "stamp": {"counter": 9_999, "actor": "peer"}
    });
    document["device_intents"]["0-mismatched"] = document["device_intents"]["device-000"].clone();
    document["device_intents"]["0-invalid-stamp"] = json!({
        "device": "0-invalid-stamp", "role": "personal",
        "stamp": {"counter": 9_999, "actor": ""}
    });
    let bytes = serde_json::to_vec_pretty(&document).unwrap();
    fs::write(&path, &bytes).unwrap();

    let store = StoragePlanStore::load_at(Some(path.clone()));
    let loaded = store.snapshot();
    assert_eq!(
        serde_json::to_value(loaded.policy).unwrap(),
        default_snapshot()["policy"]
    );
    assert_eq!(loaded.allocations.len(), 512);
    assert_eq!(loaded.device_intents.len(), 512);
    for (index, allocation) in loaded.allocations.iter().enumerate() {
        assert_eq!(allocation.volume, format!("volume-{index:03}"));
    }
    for (index, intent) in loaded.device_intents.iter().enumerate() {
        assert_eq!(intent.device, format!("device-{index:03}"));
    }
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "load must not rewrite semantic invalidity"
    );
    assert!(!sibling(&path, ".corrupt").exists());
    // Discarded records do not exhaust/advance the observed clock; unrelated
    // counters remain stored but do not become this actor's observed counter.
    assert_eq!(
        store.set_policy("owner", policy(3)).unwrap().stamp.counter,
        42
    );
    let saved = read_document(&path);
    assert_eq!(
        saved["counters"],
        json!({"": 901, "owner": 42, "unused": 9_001})
    );
    assert_eq!(saved["allocations"].as_object().unwrap().len(), 512);
    assert_eq!(saved["device_intents"].as_object().unwrap().len(), 512);
    assert_round_trip(&store, &path);
}

#[test]
fn legacy_document_loads_without_rewriting_then_saves_current_shape() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let bytes = br#"{"policy":{"value":{"ordinaryReplicas":4,"criticalReplicas":6},"stamp":{"counter":7,"actor":"peer"}},"allocations":{},"counters":{"owner":19}}"#;
    fs::write(&path, bytes).unwrap();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    let mut expected = default_snapshot();
    expected["policy"]["value"]["replicas"] = json!(4);
    expected["policy"]["stamp"] = json!({"counter": 7, "actor": "peer"});
    assert_eq!(snapshot(&store), expected);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let intent = store
        .set_device_intent("owner", "desk".into(), DeviceServiceRole::AlwaysOn)
        .unwrap();
    assert_eq!(intent.stamp.counter, 20);
    let saved = read_document(&path);
    assert_eq!(saved["policy"]["value"]["replicas"], 4);
    assert!(saved["policy"]["value"].get("ordinaryReplicas").is_none());
    assert!(saved["policy"]["value"].get("criticalReplicas").is_none());
    assert_eq!(saved["device_intents"]["desk"]["role"], "alwaysOn");
    assert_round_trip(&store, &path);
}

#[test]
fn single_write_preserves_exact_pretty_document_bytes() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    store.set_policy("owner", policy(3)).unwrap();
    let expected = br#"{
  "policy": {
    "value": {
      "replicas": 3,
      "reservePercent": 10,
      "versionRetentionDays": 30,
      "rebalanceGibPerDay": 50,
      "pauseOnMetered": true
    },
    "stamp": {
      "counter": 1,
      "actor": "owner"
    }
  },
  "allocations": {},
  "device_intents": {},
  "counters": {
    "owner": 1
  }
}"#;
    assert_eq!(fs::read(&path).unwrap(), expected);
    assert_round_trip(&store, &path);
    assert!(!sibling(&path, ".tmp").exists());
}

#[test]
fn successful_writes_round_trip_replace_atomically_and_leave_no_temp() {
    let directory = TestDirectory::new();
    let path = directory.root.join("nested/deeper/plan.json");
    let store = StoragePlanStore::load_at(Some(path.clone()));
    seed(&store);
    assert_round_trip(&store, &path);
    assert!(!sibling(&path, ".tmp").exists());
    assert!(!sibling(&path, ".corrupt").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    }
    // A same-directory stale temporary file is truncated and consumed, not
    // appended to or confused with the authoritative document.
    fs::write(sibling(&path, ".tmp"), vec![b'x'; 32_768]).unwrap();
    let untouched = directory.root.join("unrelated.txt");
    fs::write(&untouched, b"fixture sentinel").unwrap();
    assert!(remote_patch(&store));
    assert!(!sibling(&path, ".tmp").exists());
    let saved = read_document(&path);
    assert_eq!(saved["counters"], json!({"owner": 3, "remote": 11}));
    assert_eq!(fs::read(untouched).unwrap(), b"fixture sentinel");
    assert_round_trip(&store, &path);
}

#[test]
fn each_setter_save_failure_restores_records_but_consumes_the_clock() {
    for mutation in [
        Mutation::Policy,
        Mutation::InsertAllocation,
        Mutation::ReplaceAllocation,
        Mutation::InsertIntent,
        Mutation::ReplaceIntent,
    ] {
        let directory = TestDirectory::new();
        let path = directory.path();
        let store = StoragePlanStore::load_at(Some(path.clone()));
        seed(&store);
        let before = snapshot(&store);
        let digest = store.digest();
        let bytes = fs::read(&path).unwrap();
        let obstruction = sibling(&path, ".tmp");
        fs::create_dir(&obstruction).unwrap();

        let error = mutation.apply(&store).unwrap_err();
        assert_io_context(&error, "save fleet storage plan: ");
        assert_eq!(snapshot(&store), before, "record rollback: {mutation:?}");
        assert_ne!(store.digest(), digest, "counter retention: {mutation:?}");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            StoragePlanStore::load_at(Some(path.clone())).digest(),
            digest
        );
        assert!(
            obstruction.is_dir(),
            "failed open must leave the obstruction alone"
        );

        fs::remove_dir(&obstruction).unwrap();
        let stamp = mutation.apply(&store).unwrap();
        assert_eq!(
            stamp.counter, 5,
            "failed counter 4 remains consumed: {mutation:?}"
        );
        assert_eq!(stamp.actor, "owner");
        assert_eq!(read_document(&path)["counters"]["owner"], 5);
        assert!(!obstruction.exists());
        assert_round_trip(&store, &path);
    }
}

#[test]
fn changed_merge_save_failure_restores_the_entire_state_and_can_retry() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    seed(&store);
    let before = snapshot(&store);
    let digest = store.digest();
    let bytes = fs::read(&path).unwrap();
    let obstruction = sibling(&path, ".tmp");
    fs::create_dir(&obstruction).unwrap();
    assert!(!remote_patch(&store));
    assert_eq!(snapshot(&store), before);
    assert_eq!(
        store.digest(),
        digest,
        "merge must also roll back the counters"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_round_trip(&store, &path);
    fs::remove_dir(&obstruction).unwrap();
    // The failed remote counter 11 must not leak into a subsequent local write.
    assert_eq!(
        store.set_policy("remote", policy(3)).unwrap().stamp.counter,
        4
    );
    assert!(
        remote_patch(&store),
        "the exact failed patch must remain retryable"
    );
    assert!(!remote_patch(&store), "successful retry is idempotent");
    assert_eq!(
        read_document(&path)["counters"],
        json!({"owner": 3, "remote": 11})
    );
    assert_eq!(
        store.set_policy("owner", policy(5)).unwrap().stamp.counter,
        12
    );
    assert_round_trip(&store, &path);
    assert!(!obstruction.exists());
}

#[test]
fn unchanged_merge_does_not_attempt_a_write() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    seed(&store);
    let before = store.snapshot();
    let digest = store.digest();
    let bytes = fs::read(&path).unwrap();
    let obstruction = sibling(&path, ".tmp");
    // If persistence is attempted, opening this regular sentinel truncates it.
    fs::write(&obstruction, b"no write expected").unwrap();
    assert!(!store.merge(
        "relay",
        true,
        Some(before.policy),
        before.allocations,
        before.device_intents,
    ));
    assert_eq!(store.digest(), digest);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(obstruction).unwrap(), b"no write expected");
}

#[test]
fn file_as_parent_reports_directory_context_and_retains_setter_counter() {
    let directory = TestDirectory::new();
    let parent = directory.root.join("not-a-directory");
    fs::write(&parent, b"parent sentinel").unwrap();
    let path = parent.join("plan.json");
    let store = StoragePlanStore::load_at(Some(path));
    let error = store.set_policy("owner", policy(3)).unwrap_err();
    assert_io_context(&error, "create storage-plan directory: ");
    assert_eq!(snapshot(&store), default_snapshot());
    assert_eq!(store.digest(), EMPTY_WITH_OWNER_COUNTER_ONE_DIGEST);
    assert_eq!(fs::read(parent).unwrap(), b"parent sentinel");
}

#[test]
fn rename_failure_cleans_temporary_file_and_preserves_destination() {
    let directory = TestDirectory::new();
    let path = directory.path();
    fs::create_dir(&path).unwrap();
    let sentinel = path.join("keep.txt");
    fs::write(&sentinel, b"destination sentinel").unwrap();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    let error = store.set_policy("owner", policy(3)).unwrap_err();
    assert_io_context(&error, "save fleet storage plan: ");
    assert_eq!(snapshot(&store), default_snapshot());
    assert_eq!(store.digest(), EMPTY_WITH_OWNER_COUNTER_ONE_DIGEST);
    assert_eq!(fs::read(&sentinel).unwrap(), b"destination sentinel");
    assert!(
        !sibling(&path, ".tmp").exists(),
        "rename failure cleans its temporary file"
    );
    fs::remove_file(sentinel).unwrap();
    fs::remove_dir(&path).unwrap();
    assert_eq!(
        store.set_policy("owner", policy(3)).unwrap().stamp.counter,
        2
    );
    assert_round_trip(&store, &path);
}

#[test]
fn absent_persistence_path_is_successful_memory_operation() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let memory = StoragePlanStore::load_at(None);
    let durable = StoragePlanStore::load_at(Some(path));
    assert_eq!(memory.digest(), EMPTY_DIGEST);
    seed(&memory);
    seed(&durable);
    assert!(remote_patch(&memory));
    assert!(remote_patch(&durable));
    assert_eq!(snapshot(&memory), snapshot(&durable));
    assert_eq!(memory.digest(), durable.digest());
}

#[test]
fn empty_explicit_path_reports_exact_error_for_each_setter_and_rolls_back_merge() {
    for mutation in [
        Mutation::Policy,
        Mutation::InsertAllocation,
        Mutation::InsertIntent,
    ] {
        let store = StoragePlanStore::load_at(Some(PathBuf::new()));
        assert_eq!(
            mutation.apply(&store).unwrap_err(),
            "storage-plan path has no parent"
        );
        assert_eq!(snapshot(&store), default_snapshot());
        assert_eq!(store.digest(), EMPTY_WITH_OWNER_COUNTER_ONE_DIGEST);
        assert!(!remote_patch(&store));
        assert_eq!(snapshot(&store), default_snapshot());
        assert_eq!(store.digest(), EMPTY_WITH_OWNER_COUNTER_ONE_DIGEST);
    }
}

#[test]
fn validation_before_and_after_stamping_preserves_errors_and_retry_counters() {
    let directory = TestDirectory::new();
    let path = directory.path();
    let store = StoragePlanStore::load_at(Some(path.clone()));
    let obstruction = sibling(&path, ".tmp");
    fs::create_dir(&obstruction).unwrap();
    assert_eq!(
        store.set_policy("owner", policy(0)).unwrap_err(),
        "storage policy is outside its safe bounds"
    );
    assert_eq!(
        store.set_policy("", policy(3)).unwrap_err(),
        "invalid storage-plan actor"
    );
    assert_eq!(
        store
            .set_allocation("", "".into(), "disk".into(), 1, true)
            .unwrap_err(),
        "invalid storage resource identity"
    );
    assert_eq!(
        store
            .set_device_intent("", "".into(), DeviceServiceRole::Automatic)
            .unwrap_err(),
        "invalid device identity"
    );
    assert_eq!(
        store.digest(),
        EMPTY_DIGEST,
        "pre-stamp errors consume no clock"
    );
    assert_eq!(
        store
            .set_allocation("owner", "desk".into(), "disk".into(), 0, true)
            .unwrap_err(),
        "invalid storage allocation"
    );
    assert_eq!(snapshot(&store), default_snapshot());
    assert_eq!(store.digest(), EMPTY_WITH_OWNER_COUNTER_ONE_DIGEST);
    assert!(!path.exists());
    let error = store
        .set_allocation("owner", "desk".into(), "disk".into(), 1, true)
        .unwrap_err();
    assert_io_context(&error, "save fleet storage plan: ");
    fs::remove_dir(&obstruction).unwrap();
    let allocation = store
        .set_allocation("owner", "desk".into(), "disk".into(), 1, true)
        .unwrap();
    assert_eq!(
        allocation.stamp.counter, 3,
        "validation and save failures both consumed a counter"
    );
    assert_round_trip(&store, &path);
}
