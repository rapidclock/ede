//! A fixed anonymous arena, enabled only in the private syntax subprocess.
//!
//! The linker redirects the four allocator calls audited in the generated C
//! scanners and Tree-sitter runtime. Normal editor processes use the system
//! heap. All Rust layouts used by the analysis subprocess share the arena.

use nix::libc;
use rlsf::Tlsf;
use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::c_void;
use std::io;
use std::ptr::{self, NonNull};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

const BYTES: usize = 64 * 1024 * 1024;
const OUT_OF_MEMORY: libc::c_int = 73;

static START: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static NATIVE: AtomicUsize = AtomicUsize::new(0);
struct Heap {
    pool: Tlsf<'static, u32, u32, 24, 32>,
    charged: usize,
}

static HEAP: Mutex<Heap> = Mutex::new(Heap {
    pool: Tlsf::new(),
    charged: 0,
});

/// Outcome counters reported separately from operating system resident memory.
pub(super) fn counters() -> (usize, usize) {
    (PEAK.load(Ordering::Relaxed), NATIVE.load(Ordering::Relaxed))
}

/// Install the worker arena once, before any protocol input is allocated.
pub(super) fn enable() -> io::Result<()> {
    // SAFETY: isolated anonymous mapping deliberately held until worker exit.
    let memory = unsafe {
        libc::mmap(
            ptr::null_mut(),
            BYTES,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if memory == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: mmap returned an exclusive writable region held for the life of the worker.
    let block =
        NonNull::slice_from_raw_parts(unsafe { NonNull::new_unchecked(memory.cast()) }, BYTES);
    if unsafe { HEAP.lock().unwrap().pool.insert_free_block_ptr(block) }.is_none() {
        return Err(io::Error::other("unable to initialize syntax arena"));
    }
    START.store(memory.addr(), Ordering::Release);
    Ok(())
}

/// Detect pointers allocated after activation without reading foreign memory.
fn owned(pointer: *const u8) -> bool {
    let start = START.load(Ordering::Acquire);
    start != 0 && pointer.addr().wrapping_sub(start) < BYTES
}

/// A worker exits immediately on budget exhaustion; editor and allocator stay separate.
fn exhausted() -> ! {
    // SAFETY: _exit ends only this child, without destructor or allocator reentry.
    unsafe { libc::_exit(OUT_OF_MEMORY) }
}

/// Allocate inside the fixed mapping, including the allocator's own free-list metadata.
fn allocate(layout: Layout) -> NonNull<u8> {
    let mut heap = HEAP.lock().unwrap();
    let Some(memory) = heap.pool.allocate(layout) else {
        exhausted()
    };
    heap.charged += layout.size();
    PEAK.fetch_max(heap.charged, Ordering::Relaxed);
    memory
}

/// Deallocate a live arena pointer with the identical layout used for allocation.
unsafe fn release(pointer: NonNull<u8>, layout: Layout) {
    // SAFETY: caller supplies the original bounds and alignment.
    let mut heap = HEAP.lock().unwrap();
    unsafe { heap.pool.deallocate(pointer, layout.align()) };
    heap.charged -= layout.size();
}

struct WorkerAllocator;

#[global_allocator]
static ALLOCATOR: WorkerAllocator = WorkerAllocator;

// SAFETY: valid System allocations are forwarded unchanged; arena allocations retain
// the caller's original layout for deallocation and support any Rust alignment.
unsafe impl GlobalAlloc for WorkerAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if START.load(Ordering::Acquire) != 0 {
            allocate(layout).as_ptr()
        } else {
            // SAFETY: forwarded from the GlobalAlloc caller.
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if owned(pointer) {
            // SAFETY: same pointer and layout returned by alloc above.
            unsafe { release(NonNull::new_unchecked(pointer), layout) };
        } else {
            // SAFETY: only a System allocation can exist outside the worker arena.
            unsafe { System.dealloc(pointer, layout) };
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if START.load(Ordering::Acquire) == 0 {
            // SAFETY: forwarded from the GlobalAlloc caller before activation.
            return unsafe { System.realloc(pointer, layout, size) };
        }
        let target = Layout::from_size_align(size, layout.align()).unwrap_or_else(|_| exhausted());
        let next = allocate(target);
        // SAFETY: two distinct live buffers of the corresponding size.
        unsafe {
            ptr::copy_nonoverlapping(pointer, next.as_ptr(), layout.size().min(size));
            self.dealloc(pointer, layout);
        }
        next.as_ptr()
    }
}

/// Native C layout preserves max_align_t alignment and tracks realloc/free sizes.
#[repr(C, align(16))]
struct Header {
    size: usize,
}

const PREFIX: usize = std::mem::size_of::<Header>();

fn native_layout(size: usize) -> Layout {
    let size = size
        .max(1)
        .checked_add(PREFIX)
        .unwrap_or_else(|| exhausted());
    Layout::from_size_align(size, std::mem::align_of::<Header>()).unwrap_or_else(|_| exhausted())
}

fn native_alloc(size: usize) -> *mut c_void {
    NATIVE.fetch_add(1, Ordering::Relaxed);
    let block = allocate(native_layout(size));
    // SAFETY: max-aligned Header fits at the start of the new allocation.
    unsafe {
        block.cast::<Header>().as_ptr().write(Header { size });
        block.as_ptr().add(PREFIX).cast()
    }
}

/// Linked C allocation wrapper. The editor's regular heap uses the system allocator.
#[cfg_attr(target_os = "linux", unsafe(export_name = "__wrap_malloc"))]
#[cfg_attr(target_os = "macos", unsafe(export_name = "ede_malloc"))]
extern "C" fn wrapped_malloc(size: usize) -> *mut c_void {
    if START.load(Ordering::Acquire) == 0 {
        // SAFETY: calling the corresponding unredirected system entry point.
        unsafe { native::malloc(size) }
    } else {
        native_alloc(size)
    }
}

/// Linked C zeroed allocation wrapper.
#[cfg_attr(target_os = "linux", unsafe(export_name = "__wrap_calloc"))]
#[cfg_attr(target_os = "macos", unsafe(export_name = "ede_calloc"))]
extern "C" fn wrapped_calloc(n: usize, size: usize) -> *mut c_void {
    if START.load(Ordering::Acquire) == 0 {
        // SAFETY: allocator overflow behavior is supplied by libc.
        return unsafe { native::calloc(n, size) };
    }
    let total = n.checked_mul(size).unwrap_or_else(|| exhausted());
    let pointer = native_alloc(total);
    // SAFETY: native_alloc provides at least total spare bytes including when zero.
    unsafe { pointer.cast::<u8>().write_bytes(0, total) };
    pointer
}

/// Linked C reallocation wrapper.
#[cfg_attr(target_os = "linux", unsafe(export_name = "__wrap_realloc"))]
#[cfg_attr(target_os = "macos", unsafe(export_name = "ede_realloc"))]
unsafe extern "C" fn wrapped_realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
    if START.load(Ordering::Acquire) == 0 {
        // SAFETY: system pointer or null as required by C realloc.
        return unsafe { native::realloc(pointer, size) };
    }
    if pointer.is_null() {
        return native_alloc(size);
    }
    if size == 0 {
        // SAFETY: C caller supplied an active allocation.
        unsafe { wrapped_free(pointer) };
        return ptr::null_mut();
    }
    let pointer = pointer.cast::<u8>();
    // SAFETY: the original allocation is live and the arena case retains a header.
    let old_size = unsafe {
        if owned(pointer) {
            (*pointer.sub(PREFIX).cast::<Header>()).size
        } else {
            native::size(pointer.cast())
        }
    };
    let next = native_alloc(size).cast::<u8>();
    // SAFETY: both allocations are live and the copied interval fits in each.
    unsafe {
        ptr::copy_nonoverlapping(pointer, next, size.min(old_size));
        wrapped_free(pointer.cast());
    }
    next.cast()
}

/// Linked C deallocation wrapper, preserving ownership of pre-activation pointers.
#[cfg_attr(target_os = "linux", unsafe(export_name = "__wrap_free"))]
#[cfg_attr(target_os = "macos", unsafe(export_name = "ede_free"))]
unsafe extern "C" fn wrapped_free(pointer: *mut c_void) {
    let data = pointer.cast::<u8>();
    if owned(data) {
        // SAFETY: arena C pointers always follow an initialized Header.
        unsafe {
            let header = NonNull::new_unchecked(data.sub(PREFIX).cast::<Header>());
            release(header.cast(), native_layout(header.as_ref().size));
        }
    } else {
        // SAFETY: the pointer is system-owned or null.
        unsafe { native::free(pointer) };
    }
}

#[cfg(target_os = "linux")]
mod native {
    use super::c_void;

    // GNU ld/lld map __real_* to libc, bypassing executable references to the wrappers.
    unsafe extern "C" {
        #[link_name = "__real_malloc"]
        pub(super) fn malloc(size: usize) -> *mut c_void;
        #[link_name = "__real_calloc"]
        pub(super) fn calloc(n: usize, size: usize) -> *mut c_void;
        #[link_name = "__real_realloc"]
        pub(super) fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void;
        #[link_name = "__real_free"]
        pub(super) fn free(pointer: *mut c_void);
        #[link_name = "malloc_usable_size"]
        pub(super) fn size(pointer: *mut c_void) -> usize;
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::c_void;

    unsafe extern "C" {
        fn malloc_default_zone() -> *mut c_void;
        fn malloc_zone_from_ptr(pointer: *const c_void) -> *mut c_void;
        fn malloc_zone_malloc(zone: *mut c_void, size: usize) -> *mut c_void;
        fn malloc_zone_calloc(zone: *mut c_void, n: usize, size: usize) -> *mut c_void;
        fn malloc_zone_realloc(zone: *mut c_void, pointer: *mut c_void, size: usize)
        -> *mut c_void;
        fn malloc_zone_free(zone: *mut c_void, pointer: *mut c_void);
        #[link_name = "malloc_size"]
        pub(super) fn size(pointer: *mut c_void) -> usize;
    }

    pub(super) unsafe fn malloc(size: usize) -> *mut c_void {
        // SAFETY: use the actual system default zone via its nonaliased entry point.
        unsafe { malloc_zone_malloc(malloc_default_zone(), size) }
    }
    pub(super) unsafe fn calloc(n: usize, size: usize) -> *mut c_void {
        // SAFETY: use the actual system default zone via its nonaliased entry point.
        unsafe { malloc_zone_calloc(malloc_default_zone(), n, size) }
    }
    pub(super) unsafe fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
        if pointer.is_null() {
            // SAFETY: a null realloc input is a fresh allocation.
            return unsafe { malloc(size) };
        }
        // SAFETY: caller supplied a system-allocated pointer, possibly from another zone.
        unsafe { malloc_zone_realloc(malloc_zone_from_ptr(pointer), pointer, size) }
    }
    pub(super) unsafe fn free(pointer: *mut c_void) {
        if !pointer.is_null() {
            // SAFETY: caller supplied a system-allocated pointer, possibly from another zone.
            unsafe { malloc_zone_free(malloc_zone_from_ptr(pointer), pointer) };
        }
    }
}
