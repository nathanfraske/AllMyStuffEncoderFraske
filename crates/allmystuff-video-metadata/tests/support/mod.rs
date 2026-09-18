pub type Identity = (u64, bool);

pub const SAMPLE: u64 = 0x0123_4567_89ab_cdef;
pub const H264_RESET_ZERO: &[u8] = b"\0\0\0\x01\x06\x05\x21AMS-AU-SEQ-V2!!!R0000000000000000\x80";
pub const H264_GRADUAL_SAMPLE: &[u8] =
    b"\0\0\0\x01\x06\x05\x21AMS-AU-SEQ-V2!!!G0123456789abcdef\x80";
pub const H264_RESET_MAX: &[u8] = b"\0\0\0\x01\x06\x05\x21AMS-AU-SEQ-V2!!!Rffffffffffffffff\x80";
pub const HEVC_RESET_ZERO: &[u8] =
    b"\0\0\0\x01\x4e\x01\x05\x21AMS-AU-SEQ-V2!!!R0000000000000000\x80";
pub const HEVC_GRADUAL_SAMPLE: &[u8] =
    b"\0\0\0\x01\x4e\x01\x05\x21AMS-AU-SEQ-V2!!!G0123456789abcdef\x80";
pub const HEVC_RESET_MAX: &[u8] =
    b"\0\0\0\x01\x4e\x01\x05\x21AMS-AU-SEQ-V2!!!Rffffffffffffffff\x80";

pub struct ScanCase {
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub expected: &'static [(usize, u8)],
}

pub const SCANS: &[ScanCase] = &[
    ScanCase {
        name: "empty",
        bytes: b"",
        expected: &[],
    },
    ScanCase {
        name: "missing header",
        bytes: b"\0\0\x01",
        expected: &[],
    },
    ScanCase {
        name: "three byte prefix",
        bytes: b"\0\0\x01\x65",
        expected: &[(0, 0x65)],
    },
    ScanCase {
        name: "mixed prefixes",
        bytes: b"\0\0\0\x01\x67\0\0\x01\x65",
        expected: &[(0, 0x67), (5, 0x65)],
    },
    ScanCase {
        name: "extra leading zeros",
        bytes: b"\x09\0\0\0\0\x01\x65",
        expected: &[(2, 0x65)],
    },
    ScanCase {
        name: "escaped non-prefix",
        bytes: b"\0\0\x03\x01\x65",
        expected: &[],
    },
    ScanCase {
        name: "header is not validated",
        bytes: b"\0\0\x01\0\0\x01\xff",
        expected: &[(0, 0), (3, 0xff)],
    },
    ScanCase {
        name: "trailing prefix has no header",
        bytes: b"\0\0\0\0\x01\x4e\x01\0\0\x01",
        expected: &[(1, 0x4e)],
    },
];

pub struct InsertCase {
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub at: usize,
    pub hevc: bool,
    pub identity: Identity,
    pub marker: &'static [u8],
}

pub const INSERTS: &[InsertCase] = &[
    InsertCase {
        name: "empty H264",
        bytes: b"",
        at: 0,
        hevc: false,
        identity: (0, false),
        marker: H264_RESET_ZERO,
    },
    InsertCase {
        name: "opaque prefix without VCL",
        bytes: b"\x09\x08\x07",
        at: 3,
        hevc: false,
        identity: (SAMPLE, true),
        marker: H264_GRADUAL_SAMPLE,
    },
    InsertCase {
        name: "H264 AUD parameters and SEI stay before identity",
        bytes: b"\0\0\0\x01\x09\xf0\0\0\x01\x67\x0a\0\0\0\x01\x68\x0b\0\0\x01\x06\x0c\0\0\x01\x65\x0d\0\0\0\x01\x41\x0e",
        at: 22,
        hevc: false,
        identity: (u64::MAX, false),
        marker: H264_RESET_MAX,
    },
    InsertCase {
        name: "non-IDR is also VCL",
        bytes: b"\0\0\0\x01\x41\x11",
        at: 0,
        hevc: false,
        identity: (SAMPLE, true),
        marker: H264_GRADUAL_SAMPLE,
    },
    InsertCase {
        name: "H264 parameters only append",
        bytes: b"\0\0\x01\x67\x12",
        at: 5,
        hevc: false,
        identity: (0, false),
        marker: H264_RESET_ZERO,
    },
    InsertCase {
        name: "H264 VCL classification masks the header",
        bytes: b"\0\0\x01\xe5",
        at: 0,
        hevc: false,
        identity: (u64::MAX, false),
        marker: H264_RESET_MAX,
    },
    InsertCase {
        name: "empty HEVC",
        bytes: b"",
        at: 0,
        hevc: true,
        identity: (0, false),
        marker: HEVC_RESET_ZERO,
    },
    InsertCase {
        name: "HEVC parameters stay before identity",
        bytes: b"\0\0\0\x01\x40\x01\x09\0\0\x01\x42\x01\x08\0\0\0\x01\x26\x01\x07",
        at: 13,
        hevc: true,
        identity: (SAMPLE, true),
        marker: HEVC_GRADUAL_SAMPLE,
    },
    InsertCase {
        name: "HEVC classification does not require a second header byte",
        bytes: b"\0\0\x01\x02",
        at: 0,
        hevc: true,
        identity: (u64::MAX, false),
        marker: HEVC_RESET_MAX,
    },
    InsertCase {
        name: "HEVC parameters only append",
        bytes: b"\0\0\x01\x40\x01\x03",
        at: 6,
        hevc: true,
        identity: (0, false),
        marker: HEVC_RESET_ZERO,
    },
];

pub struct ParseCase {
    pub name: String,
    pub bytes: Vec<u8>,
    pub expected: Option<Identity>,
    pub remaining: Vec<u8>,
}

fn rejected(name: impl Into<String>, bytes: Vec<u8>) -> ParseCase {
    ParseCase {
        name: name.into(),
        remaining: bytes.clone(),
        bytes,
        expected: None,
    }
}

pub fn parse_cases() -> Vec<ParseCase> {
    let mut cases = vec![
        ParseCase {
            name: "H264 literal".into(),
            bytes: H264_GRADUAL_SAMPLE.to_vec(),
            expected: Some((SAMPLE, true)),
            remaining: vec![],
        },
        ParseCase {
            name: "HEVC literal".into(),
            bytes: HEVC_RESET_MAX.to_vec(),
            expected: Some((u64::MAX, false)),
            remaining: vec![],
        },
        ParseCase {
            name: "leading zero is preserved".into(),
            bytes: [b"\0".as_slice(), H264_RESET_ZERO].concat(),
            expected: Some((0, false)),
            remaining: vec![0],
        },
        ParseCase {
            name: "surrounding opaque bytes are preserved".into(),
            bytes: [b"\x09\x08".as_slice(), HEVC_RESET_ZERO, b"\x07\x06"].concat(),
            expected: Some((0, false)),
            remaining: vec![9, 8, 7, 6],
        },
        ParseCase {
            name: "first valid marker only".into(),
            bytes: [H264_GRADUAL_SAMPLE, HEVC_RESET_MAX].concat(),
            expected: Some((SAMPLE, true)),
            remaining: HEVC_RESET_MAX.to_vec(),
        },
        rejected("three byte H264 prefix", H264_RESET_ZERO[1..].to_vec()),
        rejected("three byte HEVC prefix", HEVC_RESET_ZERO[1..].to_vec()),
    ];
    for (name, hevc, at, value) in [
        ("start code", false, 3, 2),
        ("H264 header", false, 4, 0x26),
        ("payload type", false, 5, 6),
        ("short payload length", false, 6, 32),
        ("long payload length", false, 6, 34),
        ("foreign UUID", false, 7, b'X'),
        ("lowercase mode", false, 23, b'r'),
        ("unknown mode", false, 23, b'X'),
        ("uppercase hex", false, 24, b'A'),
        ("invalid last hex", false, 39, b'g'),
        ("trailing bits", false, 40, 0x81),
        ("HEVC header", true, 4, 0x50),
        ("HEVC second header", true, 5, 2),
        ("HEVC payload type", true, 6, 6),
        ("HEVC payload length", true, 7, 32),
        ("HEVC UUID", true, 8, b'X'),
        ("HEVC mode", true, 24, b'g'),
        ("HEVC hex", true, 25, b'F'),
        ("HEVC trailing bits", true, 41, 0),
    ] {
        let mut bytes = if hevc {
            HEVC_RESET_ZERO.to_vec()
        } else {
            H264_RESET_ZERO.to_vec()
        };
        bytes[at] = value;
        cases.push(rejected(name, bytes));
    }
    let mut foreign = H264_RESET_ZERO.to_vec();
    foreign[7] = b'X';
    cases.push(ParseCase {
        name: "skip foreign marker before valid marker".into(),
        bytes: [foreign.as_slice(), HEVC_GRADUAL_SAMPLE].concat(),
        expected: Some((SAMPLE, true)),
        remaining: foreign,
    });
    cases
}

pub fn truncated_cases() -> Vec<ParseCase> {
    let mut cases = Vec::new();
    for (name, marker) in [("H264", H264_RESET_ZERO), ("HEVC", HEVC_RESET_ZERO)] {
        for len in 0..marker.len() {
            cases.push(rejected(format!("{name} prefix length {len}"), marker[..len].to_vec()));
        }
    }
    cases
}
