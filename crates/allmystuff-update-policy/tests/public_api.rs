use allmystuff_update_policy::{compare_semver, policy_allows, ApplyPolicy};

mod support;

#[test]
fn version_ordering_preserves_permissive_compatibility_cases() {
    for &(left, right, expected) in support::COMPARISONS {
        assert_eq!(
            compare_semver(left, right),
            expected,
            "{left:?} vs {right:?}"
        );
        assert_eq!(
            compare_semver(right, left),
            expected.reverse(),
            "reverse of {left:?} vs {right:?}"
        );
    }
}

#[test]
fn policy_decisions_preserve_upgrade_and_version_edge_cases() {
    let policies = [
        ApplyPolicy::Patch,
        ApplyPolicy::Minor,
        ApplyPolicy::All,
        ApplyPolicy::None,
    ];
    for &(current, candidate, expected) in support::DECISIONS {
        for (policy, allowed) in policies.into_iter().zip(expected) {
            assert_eq!(
                policy_allows(policy, current, candidate),
                allowed,
                "{policy:?}: {current:?} -> {candidate:?}"
            );
        }
    }
}

#[test]
fn policy_tokens_and_serde_remain_exact_lowercase_strings() {
    let policies = [
        ApplyPolicy::Patch,
        ApplyPolicy::Minor,
        ApplyPolicy::All,
        ApplyPolicy::None,
    ];
    for (name, policy) in support::POLICY_NAMES.into_iter().zip(policies) {
        assert_eq!(ApplyPolicy::parse(name), Some(policy));
        let json = format!("\"{name}\"");
        assert_eq!(serde_json::to_string(&policy).unwrap(), json);
        assert_eq!(serde_json::from_str::<ApplyPolicy>(&json).unwrap(), policy);
    }
    for &name in support::INVALID_POLICY_NAMES {
        assert_eq!(ApplyPolicy::parse(name), None, "{name:?}");
        let json = serde_json::to_string(name).unwrap();
        assert!(
            serde_json::from_str::<ApplyPolicy>(&json).is_err(),
            "{json}"
        );
    }
    for &json in support::INVALID_POLICY_JSON {
        assert!(serde_json::from_str::<ApplyPolicy>(json).is_err(), "{json}");
    }
}
