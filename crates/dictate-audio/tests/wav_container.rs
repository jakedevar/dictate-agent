//! RIFF container handling in `decode_wav`, against byte-exact fixtures that
//! were written by hand rather than by the encoder under test.

use dictate_audio::decode::{decode_wav, DecodeError};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}.wav",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A 44-byte canonical 16 kHz mono 16-bit header whose `data` chunk declares
/// `declared` bytes, followed by `present` bytes of silence.
fn lying_header(declared: u32, present: usize) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&36u32.wrapping_add(declared).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&16_000u32.to_le_bytes());
    b.extend_from_slice(&32_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&declared.to_le_bytes());
    b.resize(44 + present, 0);
    b
}

#[test]
fn an_odd_length_chunk_with_its_pad_byte_is_skipped_correctly() {
    // `LIST` has a 17-byte body and the RIFF-mandated pad byte after it.
    let padded = decode_wav(&fixture("odd_list_chunk_padded"), None)
        .expect("a valid WAV with a padded odd-length chunk must decode");
    let plain = decode_wav(&fixture("plain_pcm16"), None).unwrap();
    assert_eq!(padded.samples.len(), 160);
    assert_eq!(
        padded, plain,
        "the ancillary chunk must not change the audio"
    );
}

#[test]
fn a_data_chunk_larger_than_the_file_is_malformed() {
    let err = decode_wav(&lying_header(0xFFFF_FFF0, 64), None).unwrap_err();
    match err {
        DecodeError::Malformed(m) => assert!(m.contains("declares"), "{m}"),
        other => panic!("expected malformed, got {other:?}"),
    }
}

#[test]
fn an_ancillary_chunk_larger_than_the_file_is_malformed() {
    let mut bytes = fixture("plain_pcm16");
    // Replace the data chunk id so the walker reads it as unknown, then make
    // it claim more than is there.
    let at = bytes.windows(4).position(|w| w == b"data").unwrap();
    bytes[at..at + 4].copy_from_slice(b"junk");
    bytes[at + 4..at + 8].copy_from_slice(&1_000_000u32.to_le_bytes());
    assert!(matches!(
        decode_wav(&bytes, None),
        Err(DecodeError::Malformed(_))
    ));
}

#[test]
fn data_before_fmt_is_malformed() {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF\x24\x00\x00\x00WAVE");
    b.extend_from_slice(b"data\x02\x00\x00\x00\x00\x00");
    assert!(matches!(
        decode_wav(&b, None),
        Err(DecodeError::Malformed(_))
    ));
}

#[test]
fn an_honest_header_still_decodes() {
    // Must-not-change: exactly the bytes the header declares.
    let decoded = decode_wav(&lying_header(3_200, 3_200), None).unwrap();
    assert_eq!(decoded.samples.len(), 1_600);
}
