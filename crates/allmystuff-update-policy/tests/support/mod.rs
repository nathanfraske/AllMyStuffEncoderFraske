use std::cmp::Ordering::{self, Equal, Greater, Less};

// Fixed compatibility cases, including behavior that differs from strict SemVer.
pub const COMPARISONS: &[(&str, &str, Ordering)] = &[
    ("1.2.3", "1.2.4", Less),
    ("1.10", "1.2.99", Greater),
    ("1.2", "1.2.0", Equal),
    ("1", "1.0.0", Equal),
    ("", "0.0.0", Equal),
    ("1..3", "1.0.3", Equal),
    ("1.invalid.3", "1.0.3", Equal),
    ("1.2.4294967296", "1.2.0", Equal),
    ("4294967296.1.2", "0.1.2", Equal),
    ("1.2.3.999", "1.2.3", Equal),
    ("1.2.3-rc10", "1.2.3-rc2", Less),
    ("1.2.3-rc.10", "1.2.3-rc.2", Less),
    ("1.2.3-Z", "1.2.3-a", Less),
    ("1.2.3-", "1.2.3", Equal),
    ("1.2.3", "1.2.3-rc1", Greater),
    ("1.2.3+build", "1.2.0", Equal),
    ("v1.2.3", "0.2.3", Equal),
    (" 1.2.3", "0.2.3", Equal),
    ("1.2.3 ", "1.2.0", Equal),
    ("+1.02.003", "1.2.3", Equal),
];

// Expected decisions are ordered Patch, Minor, All, None.
pub const DECISIONS: &[(&str, &str, [bool; 4])] = &[
    ("1.2.3", "1.2.4", [true, true, true, false]),
    ("1.2.3", "1.3.0", [false, true, true, false]),
    ("1.2.3", "2.0.0", [false, false, true, false]),
    ("1.2.3", "1.2.3", [false, false, false, false]),
    ("1.2.3", "1.2.2", [false, false, false, false]),
    ("1.2.3-rc1", "1.2.3", [true, true, true, false]),
    ("1.2.3-rc2", "1.2.3-rc10", [false, false, false, false]),
    ("1.2.9", "1.2.10+build", [false, false, false, false]),
    ("1.bad.0", "1.0.1", [true, true, true, false]),
    ("garbage", "0.0.1", [true, true, true, false]),
    ("1.2.3", "1.2.3.4", [false, false, false, false]),
    ("1.2.4294967296", "1.2.1", [true, true, true, false]),
    ("1.2.3", "1.2.3-", [false, false, false, false]),
    ("1.2", "1.2.1", [true, true, true, false]),
];

pub const POLICY_NAMES: [&str; 4] = ["patch", "minor", "all", "none"];
pub const INVALID_POLICY_NAMES: &[&str] = &[
    "", "Patch", "PATCH", "Minor", "ALL", "None", "off", " patch", "patch ", "none\n",
];
pub const INVALID_POLICY_JSON: &[&str] = &["null", "0", "true", "[]", "{}"];
