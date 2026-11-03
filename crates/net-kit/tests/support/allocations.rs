use libmimalloc_sys::{mi_block_visit_fun, mi_heap_area_t, mi_heap_t, mi_theap_t};
use std::ffi::c_void;

unsafe extern "C" {
    fn mi_theap_get_default() -> *mut mi_theap_t;
    fn mi_theap_visit_blocks(
        heap: *const mi_theap_t,
        blocks: bool,
        visitor: mi_block_visit_fun,
        arg: *mut c_void,
    ) -> bool;
}

// Inspect only this thread's heap; callers use a current-thread runtime.
pub fn live_bytes() -> u64 {
    unsafe extern "C" fn visit(
        _: *const mi_heap_t,
        area: *const mi_heap_area_t,
        _: *mut c_void,
        _: usize,
        total: *mut c_void,
    ) -> bool {
        unsafe {
            *total.cast::<u64>() += ((*area).used * (*area).block_size) as u64;
        }
        true
    }
    let mut total = 0u64;
    let success = unsafe {
        mi_theap_visit_blocks(
            mi_theap_get_default(),
            false,
            Some(visit),
            (&mut total as *mut u64).cast(),
        )
    };
    assert!(success, "mimalloc heap snapshot failed");
    total
}
