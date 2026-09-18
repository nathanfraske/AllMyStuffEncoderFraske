use allmystuff_video_metadata::{
    annexb_nals, insert_au_identity_marker, peek_au_identity_marker, take_au_identity_marker,
    AuIdentity, AuRecovery,
};

mod support;

fn identity((sequence, gradual): support::Identity) -> AuIdentity {
    AuIdentity {
        sequence,
        recovery: if gradual {
            AuRecovery::Gradual
        } else {
            AuRecovery::Reset
        },
    }
}

#[test]
fn annexb_offsets_preserve_prefix_and_header_acceptance() {
    for case in support::SCANS {
        assert_eq!(annexb_nals(case.bytes), case.expected, "{}", case.name);
    }
}

#[test]
fn insertion_matches_literal_bytes_and_fixed_positions() {
    for case in support::INSERTS {
        let expected = [&case.bytes[..case.at], case.marker, &case.bytes[case.at..]].concat();
        let mut actual = case.bytes.to_vec();
        let id = identity(case.identity);
        insert_au_identity_marker(&mut actual, id, case.hevc);
        assert_eq!(actual, expected, "{}", case.name);
        assert_eq!(peek_au_identity_marker(&actual), Some(id), "{}", case.name);
        assert_eq!(actual, expected, "peek must not mutate: {}", case.name);
        assert_eq!(
            take_au_identity_marker(&mut actual),
            Some(id),
            "{}",
            case.name
        );
        assert_eq!(actual, case.bytes, "{}", case.name);
    }
}

#[test]
fn parsing_preserves_exact_acceptance_and_untouched_bytes() {
    for case in support::parse_cases() {
        let mut actual = case.bytes.clone();
        let expected = case.expected.map(identity);
        assert_eq!(peek_au_identity_marker(&actual), expected, "{}", case.name);
        assert_eq!(actual, case.bytes, "{}", case.name);
        assert_eq!(
            take_au_identity_marker(&mut actual),
            expected,
            "{}",
            case.name
        );
        assert_eq!(actual, case.remaining, "{}", case.name);
    }
}

#[test]
fn every_truncated_marker_prefix_remains_untouched() {
    for case in support::truncated_cases() {
        let mut actual = case.bytes.clone();
        assert_eq!(peek_au_identity_marker(&actual), None, "{}", case.name);
        assert_eq!(take_au_identity_marker(&mut actual), None, "{}", case.name);
        assert_eq!(actual, case.remaining, "{}", case.name);
    }
}

#[test]
fn repeated_insertion_and_removal_preserve_first_marker_order() {
    let picture = b"\0\0\x01\x65\x88";
    let first = identity((0, false));
    let second = identity((support::SAMPLE, true));
    let mut actual = picture.to_vec();
    insert_au_identity_marker(&mut actual, first, false);
    insert_au_identity_marker(&mut actual, second, false);
    assert_eq!(
        actual,
        [
            support::H264_RESET_ZERO,
            support::H264_GRADUAL_SAMPLE,
            picture
        ]
        .concat()
    );
    assert_eq!(peek_au_identity_marker(&actual), Some(first));
    assert_eq!(take_au_identity_marker(&mut actual), Some(first));
    assert_eq!(peek_au_identity_marker(&actual), Some(second));
    assert_eq!(take_au_identity_marker(&mut actual), Some(second));
    assert_eq!(take_au_identity_marker(&mut actual), None);
    assert_eq!(actual, picture);
}
