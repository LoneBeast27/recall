//! Test builds only: the system allocator, counting the bytes the calling thread allocates
//! inside [`bytes_in`] (p9 gate P2-c: per-packet audio work must not allocate). Other threads
//! and code outside `bytes_in` are not counted.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every call goes to `System` unchanged; the count is a thread-local add.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn note(size: usize) {
    // `try_with`: the thread may be ending (its locals already gone).
    let _ = ON.try_with(|on| {
        if on.get() {
            let _ = BYTES.try_with(|b| b.set(b.get() + size));
        }
    });
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// `f`'s result and the bytes this thread allocated while it ran.
pub(crate) fn bytes_in<R>(f: impl FnOnce() -> R) -> (R, usize) {
    BYTES.with(|b| b.set(0));
    ON.with(|on| on.set(true));
    let out = f();
    ON.with(|on| on.set(false));
    (out, BYTES.with(Cell::get))
}
