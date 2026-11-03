//! Shared aggregate reservations and caches across all service documents.
use super::{Error, StorageLimits, cache::Cache};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// Shared reservations for all owned buffers/windows in a storage service.
pub(super) struct Budget {
    used: AtomicUsize,
    limit: usize,
}
impl Budget {
    /// Reserve an empty bounded aggregate buffer pool.
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            limit,
        })
    }
    /// Admission precedes allocation; a failed reservation has no side effects.
    pub fn acquire(self: &Arc<Self>, bytes: usize) -> Result<Permit, Error> {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|n| *n <= self.limit)
            })
            .map_err(|_| Error::ResourceLimit("aggregate buffer budget"))?;
        Ok(Permit {
            budget: self.clone(),
            bytes,
        })
    }
}
/// RAII aggregate buffer charge, released only when the owning result is dropped.
pub(super) struct Permit {
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
    pub text: Arc<Mutex<Cache>>,
    pub nodes: Arc<Mutex<Cache>>,
    pub buffers: Arc<Budget>,
}
impl Resources {
    /// Validate all limits before exposing a resource pool.
    pub fn new(limits: &StorageLimits) -> Result<Arc<Self>, Error> {
        limits.validate()?;
        Ok(Arc::new(Self {
            text: Arc::new(Mutex::new(Cache::new(limits.text_cache_bytes))),
            nodes: Arc::new(Mutex::new(Cache::new(limits.node_cache_bytes))),
            buffers: Budget::new(limits.buffer_bytes),
        }))
    }
}
