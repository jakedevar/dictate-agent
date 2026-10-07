//! A WAV header must not be able to make the decoder allocate what its bytes
//! cannot back. This binary installs a counting allocator, so it holds only
//! this one test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Largest;

static LARGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Largest {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Largest = Largest;

#[test]
fn a_44_byte_header_claiming_gigabytes_allocates_almost_nothing() {
    // 8-bit mono at 192 kHz: 0xFFFF_FFF0 declared bytes is ~6 hours, so only
    // the absence of those bytes — not the duration limit — can refuse it.
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&u32::MAX.to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&192_000u32.to_le_bytes());
    b.extend_from_slice(&192_000u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&8u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    assert_eq!(b.len(), 44);

    LARGEST.store(0, Ordering::Relaxed);
    let result = dictate_audio::decode::decode_wav(&b, None);
    let largest = LARGEST.load(Ordering::Relaxed);
    assert!(
        matches!(
            result,
            Err(dictate_audio::decode::DecodeError::Malformed(_))
        ),
        "{result:?}"
    );
    assert!(
        largest < 64 * 1024,
        "decoding a 44-byte header allocated {largest} bytes at once"
    );
}
