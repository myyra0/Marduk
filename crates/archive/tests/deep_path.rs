//! Admission memory bound for a deeply nested single path.

#[allow(dead_code)]
mod fixtures;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};

use fixtures::{ClassicZip, Record};
use marduk_archive::{M1_ZIP_LIMITS, admit_zip};

struct CountingAllocator;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let current = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(current, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !new_pointer.is_null() {
            if new_size >= layout.size() {
                let current = CURRENT.fetch_add(new_size - layout.size(), Ordering::Relaxed)
                    + (new_size - layout.size());
                PEAK.fetch_max(current, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        new_pointer
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn deep_path_admission_stays_linear_in_the_name_length() {
    // A single record deep enough that cloning every prefix would retain about
    // 400 MiB; one component per trie node keeps it under a few MiB.
    let depth = 20_000;
    let mut name = String::with_capacity(depth * 2 + 1);
    for _ in 0..depth {
        name.push_str("a/");
    }
    name.push('b');
    assert!(name.len() < usize::from(u16::MAX));

    let zip = ClassicZip::new(&[Record::file(name.as_bytes(), b"x")]).bytes;
    let baseline = CURRENT.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);

    let admitted = admit_zip(Cursor::new(zip), M1_ZIP_LIMITS).expect("deep path is admissible");
    assert_eq!(admitted.entries().len(), 1);
    assert_eq!(admitted.entries()[0].path.as_str(), name);

    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    assert!(
        peak < 64 << 20,
        "deep-path admission peaked at {peak} bytes"
    );
}
