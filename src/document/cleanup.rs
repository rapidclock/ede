//! Bounded destruction of private generations away from foreground owners.
use super::{Error, IoPoint, resources::Permit, source::StoreData};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, SyncSender, TrySendError},
};
thread_local! { static RECLAIMING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// The same 32 KiB reservation held by every StoreData bounds this queue.
/// A queue slot is therefore available before a live generation can retire.
pub(super) struct Cleanup {
    sender: SyncSender<Box<dyn Garbage>>,
    state: Arc<State>,
    _permit: Permit,
    enabled: AtomicBool,
}
struct State {
    pending: Mutex<usize>,
    idle: Condvar,
}
/// Every queued object owns a distinct generation's existing 32 KiB admission.
pub(super) trait Garbage: Send {
    fn observe(&self);
    fn dispose(self: Box<Self>) {
        drop(self);
    }
}
impl Garbage for StoreData {
    fn observe(&self) {
        let _ = self.source.check(IoPoint::Cleanup);
    }
}
impl Cleanup {
    /// Start one reclaimer per resource pool. Its channel storage is admitted first.
    pub(super) fn new(capacity: usize, permit: Permit) -> Result<Arc<Self>, Error> {
        let (sender, receiver) = mpsc::sync_channel::<Box<dyn Garbage>>(capacity);
        let state = Arc::new(State {
            pending: Mutex::new(0),
            idle: Condvar::new(),
        });
        let worker_state = state.clone();
        std::thread::Builder::new()
            .name("ede-reclaim".into())
            .spawn(move || {
                for data in receiver {
                    // Observation only: publication already happened or was rejected.
                    // A cleanup fault cannot turn an acknowledged edit into an error.
                    RECLAIMING.set(true);
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| data.observe()));
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| data.dispose()));
                    RECLAIMING.set(false);
                    let mut pending = worker_state
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *pending -= 1;
                    if *pending == 0 {
                        worker_state.idle.notify_all();
                    }
                }
            })?;
        Ok(Arc::new(Self {
            sender,
            state,
            _permit: permit,
            enabled: AtomicBool::new(false),
        }))
    }
    /// Nonblocking transfer; the generation retains its original admission charge.
    pub(super) fn retire(&self, data: impl Garbage + 'static) {
        if RECLAIMING.get() {
            drop(data);
            return;
        }
        if !self.enabled.load(Ordering::Acquire) {
            drop(data);
            return;
        }
        *self.state.pending.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        match self.sender.try_send(Box::new(data)) {
            Ok(()) => {}
            Err(TrySendError::Full(data) | TrySendError::Disconnected(data)) => {
                // Reserved capacity makes Full impossible. If a reclaimer has
                // catastrophically stopped, leak only private temporary storage
                // instead of doing unbounded filesystem work on the caller.
                std::mem::forget(data);
                let mut pending = self.state.pending.lock().unwrap_or_else(|e| e.into_inner());
                *pending -= 1;
                if *pending == 0 {
                    self.state.idle.notify_all();
                }
            }
        }
    }
    /// Standalone blocking documents retain synchronous disposal; interactive
    /// services enable deferred disposal before admitting any generations.
    pub(super) fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }
    /// Normal shutdown waits here only after terminal restoration and owner joins.
    pub(super) fn wait(&self) {
        let mut pending = self.state.pending.lock().unwrap_or_else(|e| e.into_inner());
        while *pending != 0 {
            pending = self
                .state
                .idle
                .wait(pending)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}
