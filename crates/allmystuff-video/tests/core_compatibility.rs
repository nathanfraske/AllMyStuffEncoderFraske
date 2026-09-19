//! Independent byte fixtures and frozen pre-extraction source oracles.
//! These tests never initialize an environment dial, decoder or device.

use std::ops::Range;

use allmystuff_video::{codec, framing, metadata, receive};
use serde_json::Value;

// Keep ordinary workspace formatting from rewriting the recorded source spans.
#[rustfmt::skip]
#[path = "baseline/codec.rs"]
mod old_codec;
#[rustfmt::skip]
#[path = "baseline/wire_host.rs"]
mod old_host;
#[rustfmt::skip]
#[path = "baseline/wire_stub.rs"]
mod old_stub;
#[rustfmt::skip]
#[path = "baseline/metadata.rs"]
mod video_wire;

mod old_sequence {
    include!("baseline/sequence.rs");

    pub(super) fn classify(previous: Option<u64>, incoming: u64, clean: bool) -> &'static str {
        match observe_au_sequence(previous, incoming, clean) {
            AuSequenceObservation::Accept => "Accept",
            AuSequenceObservation::Gap => "Gap",
            AuSequenceObservation::DropDuplicateOrStale => "DropDuplicateOrStale",
        }
    }
}

fn vectors() -> Value {
    serde_json::from_str(include_str!("baseline/byte_vectors.json")).unwrap()
}

fn bytes(value: &Value) -> Vec<u8> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
        .collect()
}

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn ranges(value: &Value) -> Vec<Range<usize>> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            usize::try_from(pair[0].as_u64().unwrap()).unwrap()
                ..usize::try_from(pair[1].as_u64().unwrap()).unwrap()
        })
        .collect()
}

fn assert_partition(data: &[u8], parts: &[Range<usize>]) {
    assert!(!parts.is_empty());
    assert_eq!(parts[0].start, 0);
    assert_eq!(parts.last().unwrap().end, data.len());
    for pair in parts.windows(2) {
        assert_eq!(pair[0].end, pair[1].start);
    }
    let joined: Vec<u8> = parts
        .iter()
        .flat_map(|part| data[part.clone()].iter().copied())
        .collect();
    assert_eq!(joined, data);
}

#[test]
fn literal_split_ranges_preserve_each_callers_policy() {
    let all = vectors();
    for case in all["split"].as_array().unwrap() {
        let data = bytes(&case["bytes"]);
        let cap = usize::try_from(case["max_chunk"].as_u64().unwrap()).unwrap();
        let host = framing::split_annexb_paced_host(&data, cap);
        let stub = framing::split_annexb_paced_stub(&data, cap);
        assert_eq!(host, ranges(&case["host"]), "host {}", case["name"]);
        assert_eq!(stub, ranges(&case["stub"]), "stub {}", case["name"]);
        assert_eq!(host, old_host::split_annexb_paced(&data, cap));
        assert_eq!(stub, old_stub::split_annexb_paced(&data, cap));
        assert_partition(&data, &host);
        assert_partition(&data, &stub);
    }
    assert_eq!(framing::PACE_SLICE_BYTES, 24 * 1024);
}

#[test]
fn splitters_match_separate_frozen_walks_across_headers_and_caps() {
    for header in 0..=u8::MAX {
        for prefix in [&[0, 0, 1][..], &[0, 0, 0, 1][..]] {
            let mut data = vec![9];
            data.extend_from_slice(prefix);
            data.extend_from_slice(&[header, 0, 0, 1, 0x65, 7, 0, 0, 1, 0x41, 8]);
            for cap in [0, 1, 5, 7, 12, 64] {
                let host = framing::split_annexb_paced_host(&data, cap);
                let stub = framing::split_annexb_paced_stub(&data, cap);
                assert_eq!(host, old_host::split_annexb_paced(&data, cap));
                assert_eq!(stub, old_stub::split_annexb_paced(&data, cap));
                assert_partition(&data, &host);
                assert_partition(&data, &stub);
            }
        }
    }
}

#[test]
fn marker_bytes_and_builder_saturation_are_unchanged() {
    let all = vectors();
    for case in all["marker"].as_array().unwrap() {
        let chunks = usize::try_from(case["chunks"].as_u64().unwrap()).unwrap();
        let expected = hex(case["hex"].as_str().unwrap());
        let count = usize::try_from(case["parsed_count"].as_u64().unwrap()).unwrap();
        assert_eq!(expected.len(), 26);
        assert_eq!(framing::paced_au_marker(chunks), expected);
        assert_eq!(framing::host::paced_au_marker(chunks), expected);
        assert_eq!(framing::stub::paced_au_marker(chunks), expected);
        assert_eq!(old_host::paced_au_marker(chunks), expected);
        assert_eq!(old_stub::paced_au_marker(chunks), expected);
        assert_eq!(framing::paced_au_marker_count(&expected), Some(count));
        assert_eq!(framing::stub::paced_au_marker_count(&expected), Some(count));
    }
    assert_eq!(
        framing::paced_au_marker(usize::MAX),
        old_host::paced_au_marker(usize::MAX)
    );
}

#[test]
fn marker_shape_is_strict_while_all_u16_counts_are_accepted() {
    let all = vectors();
    let original = hex(all["marker"][1]["hex"].as_str().unwrap());
    for index in all["marker_rejections"]["mutate_indexes"]
        .as_array()
        .unwrap()
    {
        let mut damaged = original.clone();
        damaged[usize::try_from(index.as_u64().unwrap()).unwrap()] ^= 1;
        assert_eq!(framing::paced_au_marker_count(&damaged), None);
        assert_eq!(framing::stub::paced_au_marker_count(&damaged), None);
        assert_eq!(old_host::paced_au_marker_count(&damaged), None);
        assert_eq!(old_stub::paced_au_marker_count(&damaged), None);
    }
    for len in all["marker_rejections"]["lengths"].as_array().unwrap() {
        let mut damaged = original.clone();
        damaged.resize(usize::try_from(len.as_u64().unwrap()).unwrap(), 0);
        assert_eq!(framing::paced_au_marker_count(&damaged), None);
        assert_eq!(framing::stub::paced_au_marker_count(&damaged), None);
    }
    let mut marker = original;
    for count in 0..=u16::MAX {
        marker[23..25].copy_from_slice(&count.to_le_bytes());
        let expected = Some(usize::from(count));
        assert_eq!(framing::paced_au_marker_count(&marker), expected);
        assert_eq!(framing::stub::paced_au_marker_count(&marker), expected);
        assert_eq!(old_host::paced_au_marker_count(&marker), expected);
        assert_eq!(old_stub::paced_au_marker_count(&marker), expected);
    }
}

fn codec_name(value: Option<codec::AuCodec>) -> Option<&'static str> {
    value.map(|value| match value {
        codec::AuCodec::H264 => "H264",
        codec::AuCodec::Hevc => "Hevc",
        codec::AuCodec::Av1 => "Av1",
    })
}

fn old_codec_name(value: Option<old_codec::AuCodec>) -> Option<&'static str> {
    value.map(|value| match value {
        old_codec::AuCodec::H264 => "H264",
        old_codec::AuCodec::Hevc => "Hevc",
        old_codec::AuCodec::Av1 => "Av1",
    })
}

#[test]
fn literal_codec_and_clean_entry_acceptance_includes_existing_quirks() {
    let all = vectors();
    for case in all["codec"].as_array().unwrap() {
        let data = bytes(&case["bytes"]);
        let expected = case["codec"].as_str();
        assert_eq!(
            codec_name(codec::sniff_codec(&data)),
            expected,
            "{}",
            case["name"]
        );
        assert_eq!(old_codec_name(old_codec::sniff_codec(&data)), expected);
        let clean = case["clean_entry"].as_bool().unwrap();
        assert_eq!(codec::is_decode_entry(&data), clean);
        assert_eq!(old_codec::is_decode_entry(&data), clean);
    }
}

#[test]
fn codec_classification_matches_frozen_source_for_headers_and_truncation() {
    for header in 0..=u8::MAX {
        for data in [
            vec![header, 0, 0],
            vec![0, 0, 1, header, 1],
            vec![0, 0, 0, 1, header, 1],
        ] {
            for end in 0..=data.len() {
                let input = &data[..end];
                assert_eq!(
                    codec_name(codec::sniff_codec(input)),
                    old_codec_name(old_codec::sniff_codec(input))
                );
                assert_eq!(
                    codec::is_decode_entry(input),
                    old_codec::is_decode_entry(input)
                );
            }
        }
    }
}

#[test]
fn metadata_reexport_keeps_identity_bytes_and_removal() {
    for hevc in [false, true] {
        for gradual in [false, true] {
            let input = if hevc {
                vec![0, 0, 1, 0x40, 1, 0, 0, 1, 0x26, 1]
            } else {
                vec![0, 0, 1, 0x67, 9, 0, 0, 1, 0x65, 7]
            };
            let mut old = input.clone();
            let mut current = input.clone();
            let identity = metadata::AuIdentity {
                sequence: 1,
                recovery: if gradual {
                    metadata::AuRecovery::Gradual
                } else {
                    metadata::AuRecovery::Reset
                },
            };
            let old_identity = video_wire::AuIdentity {
                sequence: 1,
                recovery: if gradual {
                    video_wire::AuRecovery::Gradual
                } else {
                    video_wire::AuRecovery::Reset
                },
            };
            video_wire::insert_au_identity_marker(&mut old, old_identity, hevc);
            metadata::insert_au_identity_marker(&mut current, identity, hevc);
            let mut marker = if hevc {
                vec![0, 0, 0, 1, 0x4e, 1, 5, 33]
            } else {
                vec![0, 0, 0, 1, 6, 5, 33]
            };
            marker.extend_from_slice(b"AMS-AU-SEQ-V2!!!");
            marker.push(if gradual { b'G' } else { b'R' });
            marker.extend_from_slice(b"0000000000000001\x80");
            let expected = [&input[..5], marker.as_slice(), &input[5..]].concat();
            assert_eq!(current, expected);
            assert_eq!(current, old);
            assert_eq!(metadata::annexb_nals(&current), video_wire::annexb_nals(&old));
            assert_eq!(metadata::peek_au_identity_marker(&current), Some(identity));
            assert_eq!(video_wire::peek_au_identity_marker(&old), Some(old_identity));
            assert_eq!(
                metadata::take_au_identity_marker(&mut current),
                Some(identity)
            );
            assert_eq!(
                video_wire::take_au_identity_marker(&mut old),
                Some(old_identity)
            );
            assert_eq!(current, input);
            assert_eq!(old, input);
        }
    }
}

#[test]
fn sequence_wrap_and_clean_entry_rules_match_frozen_boundaries() {
    let all: Value = serde_json::from_str(include_str!("baseline/sequence_vectors.json")).unwrap();
    for case in all["cases"].as_array().unwrap() {
        let previous = case["previous"].as_u64();
        let incoming = case["incoming"].as_u64().unwrap();
        let clean = case["clean_entry"].as_bool().unwrap();
        let expected = case["expected"].as_str().unwrap();
        let actual = match receive::observe_au_sequence(previous, incoming, clean) {
            receive::AuSequenceObservation::Accept => "Accept",
            receive::AuSequenceObservation::Gap => "Gap",
            receive::AuSequenceObservation::DropDuplicateOrStale => "DropDuplicateOrStale",
        };
        assert_eq!(actual, expected);
        assert_eq!(actual, old_sequence::classify(previous, incoming, clean));
    }
}

#[test]
fn dependent_suppression_only_fences_known_deltas() {
    for (awaiting, key, expected) in [
        (false, None, false),
        (false, Some(false), false),
        (false, Some(true), false),
        (true, None, false),
        (true, Some(false), true),
        (true, Some(true), false),
    ] {
        assert_eq!(
            receive::suppress_dependent_after_drop(awaiting, key),
            expected
        );
    }
}

#[test]
fn paced_dial_value_parser_preserves_exact_opt_outs_without_global_env() {
    for (value, expected) in [
        (None, true),
        (Some(""), true),
        (Some(" 0 "), false),
        (Some("OFF"), false),
        (Some("\tFaLsE\n"), false),
        (Some("no"), true),
        (Some("false0"), true),
        (Some("1"), true),
    ] {
        assert_eq!(framing::host::paced_slices_requested(value), expected);
    }
}
