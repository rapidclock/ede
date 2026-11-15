//! Shared aggregate reservations and caches across all service documents.
use super::{Error, StorageLimits, cache::Cache};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// Shared reservations for all owned buffers/windows in a storage service.
pub(super) struct Budget {
    used: AtomicUsize,
    peak: AtomicUsize,
    limit: usize,
}
impl Budget {
    /// Reserve an empty bounded aggregate buffer pool.
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            limit,
        })
    }
    /// Admission precedes allocation; a failed reservation has no side effects.
    pub fn acquire(self: &Arc<Self>, bytes: usize) -> Result<Permit, Error> {
        let previous = self
            .used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|n| *n <= self.limit)
            })
            .map_err(|_| Error::ResourceLimit("aggregate buffer budget"))?;
        self.peak.fetch_max(previous + bytes, Ordering::AcqRel);
        Ok(Permit {
            budget: self.clone(),
            bytes,
        })
    }
    /// Highest admitted aggregate buffer charge, including active and queued jobs.
    pub fn high_water(&self) -> usize {
        self.peak.load(Ordering::Acquire)
    }
}
/// RAII aggregate buffer charge, released only when the owning result is dropped.
pub(crate) struct Permit {
    budget: Arc<Budget>,
    bytes: usize,
}
impl Drop for Permit {
    /// Release exactly the successfully admitted allocation charge.
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
/// Shared cache/buffer budgets across every document belonging to a service.
pub(super) struct Resources {
    pub cleanup: Arc<super::cleanup::Cleanup>,
    pub text: Arc<Mutex<Cache>>,
    pub nodes: Arc<Mutex<Cache>>,
    pub buffers: Arc<Budget>,
}
impl Resources {
    /// Validate all limits before exposing a resource pool.
    pub fn new(limits: &StorageLimits) -> Result<Arc<Self>, Error> {
        limits.validate()?;
        let buffers = Budget::new(limits.buffer_bytes);
        let capacity = limits.buffer_bytes / (32 << 10);
        let bytes = capacity
            .checked_mul(std::mem::size_of::<super::source::StoreData>() + 64)
            .and_then(|bytes| bytes.checked_add(4096))
            .ok_or(Error::ResourceLimit("cleanup queue size"))?;
        let cleanup = super::cleanup::Cleanup::new(capacity, buffers.acquire(bytes)?)?;
        Ok(Arc::new(Self {
            text: Arc::new(Mutex::new(Cache::new(limits.text_cache_bytes))),
            nodes: Arc::new(Mutex::new(Cache::new(limits.node_cache_bytes))),
            buffers,
            cleanup,
        }))
    }
}
