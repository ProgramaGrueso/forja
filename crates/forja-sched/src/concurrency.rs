use std::sync::Arc;

use tokio::sync::Notify;

struct ConcurrencyState {
    active: usize,
    limit: usize,
}

pub(crate) struct ConcurrencyLimiter {
    state: std::sync::Mutex<ConcurrencyState>,
    notify: Notify,
}

pub(crate) struct ConcurrencyPermit {
    limiter: Arc<ConcurrencyLimiter>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        let mut st = self.limiter.state.lock().unwrap_or_else(|e| e.into_inner());
        st.active = st.active.saturating_sub(1);
        drop(st);
        self.limiter.notify.notify_waiters();
    }
}

impl ConcurrencyLimiter {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(ConcurrencyState { active: 0, limit }),
            notify: Notify::new(),
        })
    }

    pub(crate) async fn acquire(self: &Arc<Self>) -> ConcurrencyPermit {
        loop {
            let notified = self.notify.notified();
            {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if st.active < st.limit {
                    st.active += 1;
                    return ConcurrencyPermit {
                        limiter: Arc::clone(self),
                    };
                }
            }
            notified.await;
        }
    }

    pub(crate) fn set_limit(&self, new_limit: usize) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.limit = new_limit;
        drop(st);
        self.notify.notify_waiters();
    }

    pub(crate) fn limit(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).limit
    }
}
