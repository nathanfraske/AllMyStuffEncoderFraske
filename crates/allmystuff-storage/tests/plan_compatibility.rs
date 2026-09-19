use allmystuff_storage::plan as current;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

// Preserve the original bodies through ordinary workspace formatting.
#[rustfmt::skip]
#[path = "support/oracle.rs"]
mod old;

// The frozen load_at body calls this memory-only loader. It never opens a file.
mod persist {
    pub fn load_json<T: serde::de::DeserializeOwned + Default>(_: &std::path::Path) -> T {
        super::old::load_fixture()
    }
}

fn wire() -> Value {
    serde_json::from_str(include_str!("baseline/wire_vectors.json")).unwrap()
}

fn contracts() -> Value {
    serde_json::from_str(include_str!("baseline/contract_vectors.json")).unwrap()
}

fn policy() -> Value {
    serde_json::from_str(wire()["policy_default"].as_str().unwrap()).unwrap()
}

fn stamp(counter: u64, actor: &str) -> Value {
    json!({ "counter": counter, "actor": actor })
}

fn allocation(device: &str, volume: &str, counter: u64, actor: &str) -> Value {
    json!({
        "id": format!("{}:{device}{volume}", device.len()),
        "device": device, "volume": volume, "quotaBytes": 1, "enabled": true,
        "stamp": stamp(counter, actor),
    })
}

fn intent(device: &str, counter: u64, actor: &str) -> Value {
    json!({ "device": device, "role": "automatic", "stamp": stamp(counter, actor) })
}

fn policy_record(counter: u64, actor: &str) -> Value {
    json!({ "value": policy(), "stamp": stamp(counter, actor) })
}

fn convert<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn result_json<T: Serialize>(result: Result<T, String>) -> Result<Value, String> {
    result.map(|value| serde_json::to_value(value).unwrap())
}

fn save(
    state: &current::PlanState,
    writes: &mut Vec<String>,
    error: Option<&str>,
) -> Result<(), String> {
    writes.push(serde_json::to_string(state).unwrap());
    error.map_or(Ok(()), |message| Err(message.to_owned()))
}

struct Pair {
    new: current::PlanState,
    old: old::StoragePlanStore,
    writes: Vec<String>,
}

impl Pair {
    fn new() -> Self {
        let pair = Self {
            new: current::PlanState::default(),
            old: old::StoragePlanStore::fixture_memory(),
            writes: Vec::new(),
        };
        pair.assert_state();
        pair
    }

    fn from_json(text: &str, sanitize: bool) -> Self {
        let state: current::PlanState = serde_json::from_str(text).unwrap();
        let pair = Self {
            new: if sanitize { state.sanitize() } else { state },
            old: old::StoragePlanStore::fixture_from_json(text, sanitize).unwrap(),
            writes: Vec::new(),
        };
        pair.assert_state();
        pair
    }

    fn state(&self) -> Value {
        serde_json::to_value(&self.new).unwrap()
    }

    fn assert_state(&self) {
        assert_eq!(
            serde_json::to_string(&self.new).unwrap(),
            self.old.fixture_json()
        );
        assert_eq!(self.new.digest(), self.old.digest());
        assert_eq!(
            serde_json::to_value(self.new.snapshot()).unwrap(),
            serde_json::to_value(self.old.snapshot()).unwrap()
        );
    }

    fn finish<T: Serialize, U: Serialize>(
        &mut self,
        actual: Result<T, String>,
        expected: Result<U, String>,
        writes: Vec<String>,
        old_writes: Vec<String>,
    ) -> Result<Value, String> {
        let actual = result_json(actual);
        assert_eq!(actual, result_json(expected));
        assert_eq!(writes, old_writes);
        self.writes = writes;
        self.assert_state();
        actual
    }

    fn policy(&mut self, actor: &str, value: Value, error: Option<&str>) -> Result<Value, String> {
        let mut writes = Vec::new();
        let actual = current::PolicyUpdate::new(actor, convert(value.clone())).and_then(|update| {
            update.apply(&mut self.new, |state| save(state, &mut writes, error))
        });
        let (expected, old_writes) =
            old::with_save_outcome(error, || self.old.set_policy(actor, convert(value)));
        self.finish(actual, expected, writes, old_writes)
    }

    fn allocation(
        &mut self,
        actor: &str,
        device: &str,
        volume: &str,
        quota: u64,
        enabled: bool,
        error: Option<&str>,
    ) -> Result<Value, String> {
        let mut writes = Vec::new();
        let actual =
            current::AllocationUpdate::new(actor, device.into(), volume.into(), quota, enabled)
                .and_then(|update| {
                    update.apply(&mut self.new, |state| save(state, &mut writes, error))
                });
        let (expected, old_writes) = old::with_save_outcome(error, || {
            self.old
                .set_allocation(actor, device.into(), volume.into(), quota, enabled)
        });
        self.finish(actual, expected, writes, old_writes)
    }

    fn intent(
        &mut self,
        actor: &str,
        device: &str,
        role: &str,
        error: Option<&str>,
    ) -> Result<Value, String> {
        let mut writes = Vec::new();
        let actual = current::DeviceIntentUpdate::new(actor, device.into(), convert(json!(role)))
            .and_then(|update| {
                update.apply(&mut self.new, |state| save(state, &mut writes, error))
            });
        let (expected, old_writes) = old::with_save_outcome(error, || {
            self.old
                .set_device_intent(actor, device.into(), convert(json!(role)))
        });
        self.finish(actual, expected, writes, old_writes)
    }

    fn merge(
        &mut self,
        sender: &str,
        manager: bool,
        policy: Option<Value>,
        allocations: Vec<Value>,
        intents: Vec<Value>,
        error: Option<&str>,
    ) -> bool {
        let mut writes = Vec::new();
        let actual = current::PeerPatch::new(
            sender,
            manager,
            policy.clone().map(convert),
            allocations.iter().cloned().map(convert).collect(),
            intents.iter().cloned().map(convert).collect(),
        )
        .is_some_and(|patch| patch.apply(&mut self.new, |state| save(state, &mut writes, error)));
        let (expected, old_writes) = old::with_save_outcome(error, || {
            self.old.merge(
                sender,
                manager,
                policy.map(convert),
                allocations.into_iter().map(convert).collect(),
                intents.into_iter().map(convert).collect(),
            )
        });
        assert_eq!(actual, expected);
        assert_eq!(writes, old_writes);
        self.writes = writes;
        self.assert_state();
        actual
    }
}

fn compare_serde<N: DeserializeOwned + Serialize, O: DeserializeOwned + Serialize>(
    input: &str,
) -> Result<String, String> {
    let actual = serde_json::from_str::<N>(input)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| e.to_string());
    let expected = serde_json::from_str::<O>(input)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| e.to_string());
    assert_eq!(actual, expected, "input: {input}");
    actual
}

#[test]
fn policy_defaults_aliases_unknowns_and_exact_bytes() {
    let vectors = wire();
    for (input, output) in [
        ("{}", vectors["policy_default"].as_str().unwrap()),
        (
            vectors["policy_alias_input"].as_str().unwrap(),
            vectors["policy_alias_output"].as_str().unwrap(),
        ),
        (
            vectors["policy_unknown_input"].as_str().unwrap(),
            vectors["policy_unknown_output"].as_str().unwrap(),
        ),
    ] {
        assert_eq!(
            compare_serde::<current::StoragePolicy, old::StoragePolicy>(input).unwrap(),
            output
        );
    }
    let error = compare_serde::<current::StoragePolicy, old::StoragePolicy>(
        vectors["policy_duplicate_input"].as_str().unwrap(),
    )
    .unwrap_err();
    assert!(error.starts_with("duplicate field `replicas`"));
}

#[test]
fn messages_snapshots_and_persisted_state_keep_distinct_shapes() {
    let vectors = wire();
    for name in ["patch_default", "digest_message", "sync_request"] {
        let text = vectors[name].as_str().unwrap();
        assert_eq!(
            compare_serde::<current::StoragePlanMessage, old::StoragePlanMessage>(text).unwrap(),
            text
        );
    }
    assert_eq!(
        compare_serde::<current::StoragePlanMessage, old::StoragePlanMessage>(
            vectors["patch_legacy_input"].as_str().unwrap()
        )
        .unwrap(),
        vectors["patch_default"].as_str().unwrap()
    );
    let text = vectors["snapshot_default"].as_str().unwrap();
    assert_eq!(
        compare_serde::<current::StoragePlanSnapshot, old::StoragePlanSnapshot>(text).unwrap(),
        text
    );
    assert_eq!(
        serde_json::to_string(&Pair::new().new).unwrap(),
        vectors["persisted_default"].as_str().unwrap()
    );
    for role in vectors["roles"].as_array().unwrap() {
        let text = role.to_string();
        assert_eq!(
            compare_serde::<current::DeviceServiceRole, old::DeviceServiceRole>(&text).unwrap(),
            text
        );
    }
}

#[test]
fn required_fields_and_type_errors_match_the_original_diagnostics() {
    for case in wire()["required_failures"].as_array().unwrap() {
        let text = case["json"].as_str().unwrap();
        let result = match case["type"].as_str().unwrap() {
            "stamp" => compare_serde::<current::PlanStamp, old::PlanStamp>(text),
            "policy_record" => compare_serde::<current::PolicyRecord, old::PolicyRecord>(text),
            "allocation" => {
                compare_serde::<current::StorageAllocation, old::StorageAllocation>(text)
            }
            "intent" => {
                compare_serde::<current::DeviceServiceIntent, old::DeviceServiceIntent>(text)
            }
            "snapshot" => {
                compare_serde::<current::StoragePlanSnapshot, old::StoragePlanSnapshot>(text)
            }
            "message" => {
                compare_serde::<current::StoragePlanMessage, old::StoragePlanMessage>(text)
            }
            "policy" => compare_serde::<current::StoragePolicy, old::StoragePolicy>(text),
            "role" => compare_serde::<current::DeviceServiceRole, old::DeviceServiceRole>(text),
            "persisted" => {
                let actual = serde_json::from_str::<current::PlanState>(text)
                    .err()
                    .unwrap()
                    .to_string();
                let expected = old::StoragePlanStore::fixture_from_json(text, false)
                    .err()
                    .unwrap()
                    .to_string();
                assert_eq!(actual, expected);
                assert!(actual.contains("Persisted"));
                Err(actual)
            }
            other => panic!("unknown fixture type {other}"),
        };
        assert!(result.is_err());
    }
}

#[test]
fn persisted_defaults_unknown_fields_and_numeric_type_failures() {
    for text in [
        "{}",
        "{\"allocations\":{}}",
        "{\"deviceIntents\":{\"ignored\":9},\"future\":true}",
    ] {
        let pair = Pair::from_json(text, false);
        assert_eq!(pair.state(), Pair::new().state());
    }
    for text in [
        "null",
        "true",
        "\"string\"",
        "{\"policy\":null}",
        "{\"counters\":{\"a\":-1}}",
        "{\"counters\":{\"a\":18446744073709551616}}",
    ] {
        let actual = serde_json::from_str::<current::PlanState>(text)
            .err()
            .unwrap()
            .to_string();
        let expected = old::StoragePlanStore::fixture_from_json(text, false)
            .err()
            .unwrap()
            .to_string();
        assert_eq!(actual, expected, "{text}");
    }
}

#[test]
fn literal_compact_persisted_bytes_and_digests_match() {
    let vectors: Value =
        serde_json::from_str(include_str!("baseline/digest_vectors.json")).unwrap();
    for case in vectors.as_array().unwrap() {
        let text = case["persisted"].as_str().unwrap();
        let pair = Pair::from_json(text, false);
        assert_eq!(serde_json::to_string(&pair.new).unwrap(), text);
        assert_eq!(
            pair.new.digest(),
            case["digest"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn stamp_order_is_counter_then_actor_and_chunk_stays_sixteen() {
    let expected: Vec<Value> = contracts()["stamp_order"].as_array().unwrap().clone();
    let mut new: Vec<current::PlanStamp> = expected.iter().rev().cloned().map(convert).collect();
    let mut old: Vec<old::PlanStamp> = expected.iter().rev().cloned().map(convert).collect();
    new.sort();
    old.sort();
    assert_eq!(serde_json::to_value(&new).unwrap(), json!(expected));
    assert_eq!(
        serde_json::to_value(new).unwrap(),
        serde_json::to_value(old).unwrap()
    );
    assert_eq!(current::PLAN_CHUNK, 16);
    assert_eq!(current::PLAN_CHUNK, old::PLAN_CHUNK);
}

#[test]
fn policy_range_endpoints_and_error_precedence_preserve_state() {
    for case in contracts()["policy_cases"].as_array().unwrap() {
        let field = case["field"].as_str().unwrap();
        for value in case["accepted"].as_array().unwrap() {
            let mut pair = Pair::new();
            let mut p = policy();
            p[field] = value.clone();
            assert_eq!(
                pair.policy("owner", p, None).unwrap()["stamp"]["counter"],
                1
            );
            assert_eq!(pair.writes.len(), 1);
        }
        for value in case["rejected"].as_array().unwrap() {
            let mut pair = Pair::new();
            let before = pair.state();
            let mut p = policy();
            p[field] = value.clone();
            assert_eq!(
                pair.policy("", p, None).unwrap_err(),
                "storage policy is outside its safe bounds"
            );
            assert_eq!(pair.state(), before);
            assert!(pair.writes.is_empty());
        }
    }
}

#[test]
fn resource_ids_are_utf8_byte_prefixed_and_colon_safe() {
    for case in contracts()["identity_cases"].as_array().unwrap() {
        let mut pair = Pair::new();
        let device = case["device"].as_str().unwrap();
        let volume = case["volume"].as_str().unwrap();
        assert_eq!(
            current::allocation_id(device, volume).unwrap(),
            case["id"].as_str().unwrap()
        );
        assert_eq!(
            pair.allocation("owner", device, volume, u64::MAX, false, None)
                .unwrap()["id"],
            case["id"]
        );
    }
    for value in ["a".repeat(512), "é".repeat(256)] {
        let mut pair = Pair::new();
        assert!(pair
            .allocation("owner", &value, &value, 1, true, None)
            .is_ok());
        assert!(pair.intent("owner", &value, "automatic", None).is_ok());
    }
    for value in [
        String::new(),
        "a\0b".into(),
        "a".repeat(513),
        "é".repeat(257),
    ] {
        let mut pair = Pair::new();
        assert_eq!(
            pair.allocation("", &value, "disk", 0, false, None)
                .unwrap_err(),
            "invalid storage resource identity"
        );
        assert_eq!(
            pair.allocation("", "desk", &value, 0, false, None)
                .unwrap_err(),
            "invalid storage resource identity"
        );
        assert_eq!(
            pair.intent("", &value, "automatic", None).unwrap_err(),
            "invalid device identity"
        );
        assert_eq!(pair.state(), Pair::new().state());
    }
}

#[test]
fn actors_keep_byte_bounds_and_the_existing_nul_acceptance() {
    for actor in ["x\0y".to_owned(), "a".repeat(512), "é".repeat(256)] {
        let mut pair = Pair::new();
        assert_eq!(
            pair.policy(&actor, policy(), None).unwrap()["stamp"]["actor"],
            actor
        );
        assert_eq!(
            pair.allocation(&actor, "desk", "disk", 1, true, None)
                .unwrap()["stamp"]["counter"],
            2
        );
        assert_eq!(
            pair.intent(&actor, "desk", "personal", None).unwrap()["stamp"]["counter"],
            3
        );
    }
    for actor in [String::new(), "a".repeat(513), "é".repeat(257)] {
        let mut pair = Pair::new();
        assert_eq!(
            pair.policy(&actor, policy(), None).unwrap_err(),
            "invalid storage-plan actor"
        );
        assert_eq!(
            pair.allocation(&actor, "desk", "disk", 0, true, None)
                .unwrap_err(),
            "invalid storage-plan actor"
        );
        assert_eq!(
            pair.intent(&actor, "desk", "automatic", None).unwrap_err(),
            "invalid storage-plan actor"
        );
        assert_eq!(pair.state(), Pair::new().state());
        assert!(pair.writes.is_empty());
    }
}

#[test]
fn invalid_quota_consumes_clock_without_record_or_save() {
    for enabled in [false, true] {
        let mut pair = Pair::new();
        let initial_snapshot = serde_json::to_value(pair.new.snapshot()).unwrap();
        assert_eq!(
            pair.allocation("owner", "desk", "disk", 0, enabled, None)
                .unwrap_err(),
            "invalid storage allocation"
        );
        assert!(pair.writes.is_empty());
        assert_eq!(pair.state()["counters"], json!({"owner": 1}));
        assert_eq!(
            serde_json::to_value(pair.new.snapshot()).unwrap(),
            initial_snapshot
        );
        assert_eq!(pair.new.digest(), "dbfe50029192c55d");
        assert_eq!(
            pair.allocation("owner", "desk", "disk", 1, enabled, None)
                .unwrap()["stamp"]["counter"],
            2
        );
    }
}

#[test]
fn equal_value_setters_still_advance_and_save() {
    let mut pair = Pair::new();
    for counter in [1, 2] {
        assert_eq!(
            pair.policy("owner", policy(), None).unwrap()["stamp"]["counter"],
            counter
        );
        assert_eq!(pair.writes.len(), 1);
    }
    for counter in [3, 4] {
        assert_eq!(
            pair.allocation("second", "desk", "disk", 1, true, None)
                .unwrap()["stamp"]["counter"],
            counter
        );
        assert_eq!(pair.writes.len(), 1);
    }
    for counter in [5, 6] {
        assert_eq!(
            pair.intent("owner", "desk", "alwaysOn", None).unwrap()["stamp"]["counter"],
            counter
        );
        assert_eq!(pair.writes.len(), 1);
    }
    assert_eq!(pair.state()["counters"], json!({"owner": 6, "second": 4}));
}

#[test]
fn policy_merge_has_no_direct_counter_entry_but_advances_next_local_stamp() {
    let mut pair = Pair::new();
    assert!(pair.merge(
        "relay",
        true,
        Some(policy_record(7, "remote")),
        vec![],
        vec![],
        None
    ));
    assert_eq!(pair.state()["counters"], json!({}));
    assert_eq!(pair.new.digest(), "eb12e8b0aeed961e");
    assert_eq!(
        pair.policy("local", policy(), None).unwrap()["stamp"]["counter"],
        8
    );
}

#[test]
fn clock_observes_live_records_and_only_the_selected_actor_counter() {
    let mut base = Pair::new().state();
    base["counters"] = json!({"unused": u64::MAX, "owner": 12});
    base["policy"] = policy_record(4, "p");
    base["allocations"] = json!({"1:dv": allocation("d", "v", 8, "a")});
    base["device_intents"] = json!({"d": intent("d", 9, "i")});
    let mut owner = Pair::from_json(&base.to_string(), false);
    assert_eq!(
        owner.policy("owner", policy(), None).unwrap()["stamp"]["counter"],
        13
    );
    let mut fresh = Pair::from_json(&base.to_string(), false);
    assert_eq!(
        fresh.policy("fresh", policy(), None).unwrap()["stamp"]["counter"],
        10
    );
}

#[test]
fn exhausted_clock_never_wraps_or_persists() {
    for source in ["policy", "allocation", "intent", "actor"] {
        let mut state = Pair::new().state();
        match source {
            "policy" => state["policy"] = policy_record(u64::MAX, "remote"),
            "allocation" => {
                state["allocations"] = json!({"1:dv": allocation("d", "v", u64::MAX, "remote")})
            }
            "intent" => state["device_intents"] = json!({"d": intent("d", u64::MAX, "remote")}),
            _ => state["counters"] = json!({"owner": u64::MAX}),
        }
        let mut pair = Pair::from_json(&state.to_string(), false);
        assert_eq!(
            pair.policy("owner", policy(), None).unwrap_err(),
            "storage-plan clock exhausted"
        );
        assert_eq!(
            pair.allocation("owner", "new", "disk", 1, true, None)
                .unwrap_err(),
            "storage-plan clock exhausted"
        );
        assert_eq!(
            pair.intent("owner", "new", "automatic", None).unwrap_err(),
            "storage-plan clock exhausted"
        );
        assert_eq!(pair.state(), state);
        assert!(pair.writes.is_empty());
    }
}

#[test]
fn ordinary_members_require_both_device_and_original_actor() {
    for (device, actor, accepted) in [
        ("member", "member", true),
        ("other", "member", false),
        ("member", "other", false),
        ("other", "other", false),
    ] {
        let mut pair = Pair::new();
        assert_eq!(
            pair.merge(
                "member",
                false,
                Some(policy_record(9, "member")),
                vec![allocation(device, "disk", 1, actor)],
                vec![intent(device, 2, actor)],
                None
            ),
            accepted
        );
        assert_eq!(pair.state()["policy"]["stamp"], stamp(0, ""));
        assert_eq!(pair.new.snapshot().allocations.len(), usize::from(accepted));
        assert_eq!(
            pair.new.snapshot().device_intents.len(),
            usize::from(accepted)
        );
    }
}

#[test]
fn managers_relay_and_sender_precheck_is_only_nonempty() {
    for sender in ["relay".to_owned(), "x\0y".to_owned(), "s".repeat(513)] {
        let mut pair = Pair::new();
        assert!(pair.merge(
            &sender,
            true,
            Some(policy_record(3, "author")),
            vec![allocation("other", "disk", 4, "a")],
            vec![intent("other", 5, "b")],
            None
        ));
        assert_eq!(pair.state()["counters"], json!({"a": 4, "b": 5}));
    }
    let mut pair = Pair::new();
    assert!(!pair.merge(
        "",
        true,
        Some(policy_record(9, "author")),
        vec![],
        vec![],
        None
    ));
    assert_eq!(pair.state(), Pair::new().state());
}

#[test]
fn invalid_peer_records_are_skipped_without_counter_effects() {
    let good_a = allocation("desk", "disk", 1, "owner");
    let good_i = intent("desk", 1, "owner");
    let mut allocations = Vec::new();
    for (pointer, value) in [
        ("/id", json!("wrong")),
        ("/device", json!("")),
        ("/volume", json!("bad\0volume")),
        ("/quotaBytes", json!(0)),
        ("/stamp/counter", json!(0)),
        ("/stamp/actor", json!("")),
        ("/stamp/actor", json!("x".repeat(513))),
    ] {
        let mut a = good_a.clone();
        *a.pointer_mut(pointer).unwrap() = value;
        assert!(!current::valid_allocation(&convert(a.clone())));
        allocations.push(a);
    }
    let mut intents = Vec::new();
    for (pointer, value) in [
        ("/device", json!("")),
        ("/device", json!("bad\0device")),
        ("/stamp/counter", json!(0)),
        ("/stamp/actor", json!("")),
        ("/stamp/actor", json!("x".repeat(513))),
    ] {
        let mut i = good_i.clone();
        *i.pointer_mut(pointer).unwrap() = value;
        assert!(!current::valid_device_intent(&convert(i.clone())));
        intents.push(i);
    }
    let mut bad_policy = policy_record(3, "owner");
    bad_policy["value"]["replicas"] = json!(0);
    let mut pair = Pair::new();
    assert!(!pair.merge(
        "relay",
        true,
        Some(bad_policy),
        allocations,
        intents,
        Some("must not save")
    ));
    assert_eq!(pair.state(), Pair::new().state());
    assert!(pair.writes.is_empty());
}

#[test]
fn zero_policy_stamp_and_nul_actor_validation_match_existing_rules() {
    let mut p = policy_record(0, "");
    p["value"]["replicas"] = json!(8);
    assert!(current::valid_policy_record(&convert(p.clone())));
    let mut pair = Pair::new();
    assert!(!pair.merge("relay", true, Some(p), vec![], vec![], None));
    for stamp in [stamp(0, "actor"), stamp(1, ""), stamp(1, &"x".repeat(513))] {
        let mut p = policy_record(1, "good");
        p["stamp"] = stamp;
        assert!(!current::valid_policy_record(&convert(p.clone())));
        assert!(!pair.merge("relay", true, Some(p), vec![], vec![], None));
    }
    assert!(pair.merge(
        "relay",
        true,
        Some(policy_record(1, "x\0y")),
        vec![allocation("d", "v", 2, "x\0y")],
        vec![intent("d", 3, "x\0y")],
        None
    ));
}

#[test]
fn equal_and_stale_stamps_preserve_first_value_and_skip_save() {
    let mut pair = Pair::new();
    let a = allocation("desk", "disk", 7, "b");
    let i = intent("desk", 7, "b");
    let p = policy_record(7, "b");
    assert!(pair.merge(
        "relay",
        true,
        Some(p.clone()),
        vec![a.clone()],
        vec![i.clone()],
        None
    ));
    let before = pair.state();
    for (counter, actor) in [(6, "z"), (7, "a"), (7, "b")] {
        let mut changed_a = a.clone();
        changed_a["quotaBytes"] = json!(999);
        changed_a["stamp"] = stamp(counter, actor);
        let mut changed_i = i.clone();
        changed_i["role"] = json!("personal");
        changed_i["stamp"] = stamp(counter, actor);
        let mut changed_p = p.clone();
        changed_p["value"]["replicas"] = json!(8);
        changed_p["stamp"] = stamp(counter, actor);
        assert!(!pair.merge(
            "relay",
            true,
            Some(changed_p),
            vec![changed_a],
            vec![changed_i],
            Some("must not save")
        ));
        assert_eq!(pair.state(), before);
        assert!(pair.writes.is_empty());
    }
    assert!(pair.merge(
        "relay",
        true,
        Some(policy_record(7, "c")),
        vec![allocation("desk", "disk", 7, "c")],
        vec![intent("desk", 7, "c")],
        None
    ));
}

#[test]
fn ordered_duplicates_affect_first_value_and_historical_counters() {
    let a = allocation("desk", "disk", 5, "a");
    let mut tied = a.clone();
    tied["quotaBytes"] = json!(2);
    let mut pair = Pair::new();
    assert!(pair.merge(
        "relay",
        true,
        None,
        vec![a.clone(), tied.clone()],
        vec![],
        None
    ));
    assert_eq!(pair.new.snapshot().allocations[0].quota_bytes, 1);
    let mut reversed = Pair::new();
    assert!(reversed.merge("relay", true, None, vec![tied, a.clone()], vec![], None));
    assert_eq!(reversed.new.snapshot().allocations[0].quota_bytes, 2);
    let b = allocation("desk", "disk", 6, "b");
    let mut forward = Pair::new();
    assert!(forward.merge(
        "relay",
        true,
        None,
        vec![a.clone(), b.clone()],
        vec![],
        None
    ));
    let mut backward = Pair::new();
    assert!(backward.merge("relay", true, None, vec![b, a], vec![], None));
    assert_eq!(
        serde_json::to_value(forward.new.snapshot()).unwrap(),
        serde_json::to_value(backward.new.snapshot()).unwrap()
    );
    assert_eq!(forward.state()["counters"], json!({"a": 5, "b": 6}));
    assert_eq!(backward.state()["counters"], json!({"b": 6}));
    assert_ne!(forward.new.digest(), backward.new.digest());
}

#[test]
fn oversized_patch_rejects_policy_and_other_vectors_before_mutation() {
    for kind in ["allocations", "intents"] {
        let mut pair = Pair::new();
        let allocations = if kind == "allocations" {
            vec![allocation("d", "v", 1, "a"); 513]
        } else {
            vec![]
        };
        let intents = if kind == "intents" {
            vec![intent("d", 1, "a"); 513]
        } else {
            vec![]
        };
        assert!(!pair.merge(
            "relay",
            true,
            Some(policy_record(9, "a")),
            allocations,
            intents,
            None
        ));
        assert_eq!(pair.state(), Pair::new().state());
        assert!(pair.writes.is_empty());
    }
    let mut pair = Pair::new();
    assert!(pair.merge(
        "relay",
        true,
        None,
        vec![allocation("d", "v", 1, "a"); 512],
        vec![intent("d", 1, "a"); 512],
        None
    ));
    assert_eq!(pair.new.snapshot().allocations.len(), 1);
    assert_eq!(pair.new.snapshot().device_intents.len(), 1);
}

fn full_state(count: usize) -> Value {
    let mut state = Pair::new().state();
    for index in 0..count {
        let device = format!("d{index:03}");
        state["allocations"][format!("4:{device}v")] = allocation(&device, "v", 1, "old");
        state["device_intents"][device.clone()] = intent(&device, 1, "old");
    }
    state
}

#[test]
fn full_maps_allow_updates_and_cap_errors_precede_actor_validation() {
    let state = full_state(512);
    let mut pair = Pair::from_json(&state.to_string(), false);
    assert_eq!(
        pair.allocation("", "new", "v", 0, false, None).unwrap_err(),
        "the fleet storage plan has too many allocations"
    );
    assert_eq!(
        pair.intent("", "new", "automatic", None).unwrap_err(),
        "the fleet storage plan has too many device roles"
    );
    assert_eq!(pair.state(), state);
    assert_eq!(
        pair.allocation("", "d000", "v", 0, false, None)
            .unwrap_err(),
        "invalid storage-plan actor"
    );
    assert_eq!(
        pair.intent("", "d000", "automatic", None).unwrap_err(),
        "invalid storage-plan actor"
    );
    assert!(pair
        .allocation("owner", "d000", "v", 2, false, None)
        .is_ok());
    assert!(pair.intent("owner", "d000", "personal", None).is_ok());
    assert_eq!(pair.new.snapshot().allocations.len(), 512);
    assert_eq!(pair.new.snapshot().device_intents.len(), 512);
}

#[test]
fn last_free_slot_obeys_input_order_and_rejected_records_do_not_raise_clocks() {
    for (first, second) in [("first", "second"), ("second", "first")] {
        let mut pair = Pair::from_json(&full_state(511).to_string(), false);
        assert!(pair.merge(
            "relay",
            true,
            None,
            vec![
                allocation(first, "v", 2, "a"),
                allocation(second, "v", 900, "rejected")
            ],
            vec![intent(first, 3, "a"), intent(second, 900, "rejected")],
            None
        ));
        assert_eq!(pair.new.snapshot().allocations.len(), 512);
        assert_eq!(pair.new.snapshot().device_intents.len(), 512);
        assert!(pair
            .new
            .snapshot()
            .allocations
            .iter()
            .any(|a| a.device == first));
        assert!(!pair
            .new
            .snapshot()
            .allocations
            .iter()
            .any(|a| a.device == second));
        assert_eq!(pair.state()["counters"], json!({"a": 3}));
        assert_eq!(
            pair.policy("owner", policy(), None).unwrap()["stamp"]["counter"],
            4
        );
    }
}

#[test]
fn failed_setters_restore_only_their_record_and_retries_skip_consumed_clock() {
    let mut pair = Pair::new();
    let before = serde_json::to_value(pair.new.snapshot()).unwrap();
    assert_eq!(
        pair.policy("owner", policy(), Some("save failed"))
            .unwrap_err(),
        "save failed"
    );
    assert_eq!(serde_json::to_value(pair.new.snapshot()).unwrap(), before);
    assert_eq!(pair.state()["counters"], json!({"owner": 1}));
    assert_eq!(pair.writes.len(), 1);
    assert_eq!(
        pair.policy("owner", policy(), None).unwrap()["stamp"]["counter"],
        2
    );
    assert_eq!(
        pair.allocation("owner", "d", "v", 1, true, Some("save failed"))
            .unwrap_err(),
        "save failed"
    );
    assert!(pair.new.snapshot().allocations.is_empty());
    assert_eq!(
        pair.allocation("owner", "d", "v", 1, true, None).unwrap()["stamp"]["counter"],
        4
    );
    let retained = pair.new.snapshot().allocations[0].clone();
    assert!(pair
        .allocation("owner", "d", "v", 9, false, Some("save failed"))
        .is_err());
    assert_eq!(pair.new.snapshot().allocations, vec![retained]);
    assert_eq!(
        pair.intent("owner", "d", "personal", Some("save failed"))
            .unwrap_err(),
        "save failed"
    );
    assert!(pair.new.snapshot().device_intents.is_empty());
    assert_eq!(
        pair.intent("owner", "d", "personal", None).unwrap()["stamp"]["counter"],
        7
    );
    let retained = pair.new.snapshot().device_intents[0].clone();
    assert!(pair
        .intent("owner", "d", "alwaysOn", Some("save failed"))
        .is_err());
    assert_eq!(pair.new.snapshot().device_intents, vec![retained]);
    assert_eq!(pair.state()["counters"], json!({"owner": 8}));
    assert_eq!(pair.writes.len(), 1);
}

#[test]
fn failed_merge_restores_full_state_and_noop_never_calls_persistence() {
    let mut pair = Pair::new();
    pair.policy("local", policy(), None).unwrap();
    let before = pair.state();
    let digest = pair.new.digest();
    let p = policy_record(40, "remote");
    let a = allocation("d", "v", 41, "remote");
    let i = intent("d", 42, "remote");
    assert!(!pair.merge(
        "relay",
        true,
        Some(p.clone()),
        vec![a.clone()],
        vec![i.clone()],
        Some("failed merge")
    ));
    assert_eq!(pair.state(), before);
    assert_eq!(pair.new.digest(), digest);
    assert_eq!(pair.writes.len(), 1);
    let attempted: Value = serde_json::from_str(&pair.writes[0]).unwrap();
    assert_eq!(attempted["counters"], json!({"local": 1, "remote": 42}));
    assert!(pair.merge(
        "relay",
        true,
        Some(p.clone()),
        vec![a.clone()],
        vec![i.clone()],
        None
    ));
    assert!(!pair.merge(
        "relay",
        true,
        Some(p),
        vec![a],
        vec![i],
        Some("must not save")
    ));
    assert!(pair.writes.is_empty());
    assert!(!pair.merge("relay", true, None, vec![], vec![], Some("must not save")));
    assert!(pair.writes.is_empty());
}

#[test]
fn sanitize_removes_invalid_and_mismatched_records_without_counter_cleanup() {
    let mut input = Pair::new().state();
    input["policy"]["value"]["replicas"] = json!(0);
    input["allocations"] = json!({"wrong": allocation("d", "v", 1, "a"), "1:av": allocation("a", "v", 0, "a"), "1:bv": allocation("b", "v", 2, "x\0y")});
    input["device_intents"] = json!({"wrong": intent("d", 1, "a"), "a": intent("a", 0, "a"), "b": intent("b", 2, "x\0y")});
    input["counters"] = json!({"": u64::MAX, "unrelated": 800, "bad\0actor": 900});
    let raw = Pair::from_json(&input.to_string(), false);
    assert_eq!(raw.state(), input);
    let pair = Pair::from_json(&input.to_string(), true);
    assert_eq!(pair.state()["policy"], Pair::new().state()["policy"]);
    assert_eq!(pair.new.snapshot().allocations.len(), 1);
    assert_eq!(pair.new.snapshot().allocations[0].device, "b");
    assert_eq!(pair.new.snapshot().device_intents.len(), 1);
    assert_eq!(pair.state()["counters"], input["counters"]);
}

#[test]
fn sanitize_truncates_valid_maps_in_lexicographic_order_and_is_idempotent() {
    let mut input = full_state(514);
    input["allocations"]["0:bad"] = allocation("x", "v", 1, "a");
    input["device_intents"]["0:bad"] = intent("x", 1, "a");
    for n in 0..520 {
        input["counters"][format!("unused{n:03}")] = json!(n);
    }
    let pair = Pair::from_json(&input.to_string(), true);
    let snapshot = pair.new.snapshot();
    assert_eq!(snapshot.allocations.len(), 512);
    assert_eq!(snapshot.allocations.first().unwrap().device, "d000");
    assert_eq!(snapshot.allocations.last().unwrap().device, "d511");
    assert_eq!(snapshot.device_intents.len(), 512);
    assert_eq!(snapshot.device_intents.last().unwrap().device, "d511");
    assert_eq!(pair.state()["counters"].as_object().unwrap().len(), 520);
    let again = Pair::from_json(&serde_json::to_string(&pair.new).unwrap(), true);
    assert_eq!(again.state(), pair.state());
}

#[test]
fn prepared_updates_take_the_clock_from_apply_time_state() {
    let mut pair = Pair::new();
    let update = current::PolicyUpdate::new("local", current::StoragePolicy::default()).unwrap();
    assert!(pair.merge(
        "relay",
        true,
        Some(policy_record(40, "remote")),
        vec![],
        vec![],
        None
    ));
    let mut writes = Vec::new();
    let actual = update.apply(&mut pair.new, |state| save(state, &mut writes, None));
    let (expected, old_writes) = old::with_save_outcome(None, || {
        pair.old.set_policy("local", old::StoragePolicy::default())
    });
    assert_eq!(
        pair.finish(actual, expected, writes, old_writes).unwrap()["stamp"]["counter"],
        41
    );
}
