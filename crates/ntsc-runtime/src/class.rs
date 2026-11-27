//! Heap class storage. LLVM sees the aligned payload pointer and passes it
//! unchanged through base-class references. The header records its concrete
//! destructor so the last owner can reclaim derived fields as well.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::sync::atomic::{AtomicUsize, Ordering, fence};

#[repr(C, align(16))]
struct Header {
    owners: AtomicUsize,
    layout: Layout,
    drop_fields: unsafe extern "C" fn(*mut u8),
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_class_alloc(
    size: u64,
    drop_fields: unsafe extern "C" fn(*mut u8),
) -> *mut u8 {
    let size = usize::try_from(size).expect("class too large");
    let bytes = size
        .checked_add(size_of::<Header>())
        .expect("class too large");
    let layout = Layout::from_size_align(bytes, align_of::<Header>()).expect("invalid class size");
    // The payload begins at the aligned end of the initialized header.
    unsafe {
        let ptr = alloc_zeroed(layout).cast::<Header>();
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        ptr.write(Header {
            owners: AtomicUsize::new(1),
            layout,
            drop_fields,
        });
        ptr.add(1).cast()
    }
}

/// # Safety
/// A non-null pointer must be a live payload returned by ntsc_class_alloc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ntsc_class_retain(payload: *mut u8) {
    if !payload.is_null() {
        let header = unsafe { &*payload.cast::<Header>().sub(1) };
        if header.owners.fetch_add(1, Ordering::Relaxed) >= isize::MAX as usize {
            std::process::abort();
        }
    }
}

/// # Safety
/// The caller transfers one live reference to the payload or passes null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ntsc_class_release(payload: *mut u8) {
    if payload.is_null() {
        return;
    }
    unsafe {
        let ptr = payload.cast::<Header>().sub(1);
        if (*ptr).owners.fetch_sub(1, Ordering::Release) == 1 {
            fence(Ordering::Acquire);
            ((*ptr).drop_fields)(payload);
            dealloc(ptr.cast(), (*ptr).layout);
        }
    }
}
