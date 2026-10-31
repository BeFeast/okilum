//! Revocable optional-module access, captured by clients on construction.
//! A synchronous scope carries permission through existing blocking call stacks.
use std::{
    cell::RefCell,
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
thread_local! { static CURRENT: RefCell<Option<NetworkAccess>> = const { RefCell::new(None) }; }
#[derive(Clone)]
pub struct NetworkAccess(Arc<AtomicBool>);
impl Default for NetworkAccess {
    fn default() -> Self {
        Self::new(true)
    }
}
impl NetworkAccess {
    pub fn new(enabled: bool) -> Self {
        Self(Arc::new(AtomicBool::new(enabled)))
    }
    pub fn current() -> Self {
        CURRENT.with(|v| v.borrow().clone()).unwrap_or_default()
    }
    pub fn allowed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn revoke(&self) {
        self.0.store(false, Ordering::Release);
    }
    pub fn enter(&self) -> Scope {
        Scope {
            previous: CURRENT.with(|v| v.replace(Some(self.clone()))),
            thread: PhantomData,
        }
    }
}
/// This guard must not cross an await or move to another thread.
pub struct Scope {
    previous: Option<NetworkAccess>,
    thread: PhantomData<Rc<()>>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|v| {
            v.replace(self.previous.take());
        });
    }
}

/// Carry a permission across executor threads, scoped to each poll (never across
/// an await on a shared worker). Blocking descendants capture the same lease.
pub fn scoped<F: std::future::Future>(access: NetworkAccess, future: F) -> Scoped<F> {
    Scoped {
        access,
        future: Box::pin(future),
    }
}
pub fn scoped_current<F: std::future::Future>(future: F) -> Scoped<F> {
    scoped(NetworkAccess::current(), future)
}
pub struct Scoped<F> {
    access: NetworkAccess,
    future: std::pin::Pin<Box<F>>,
}
impl<F: std::future::Future> std::future::Future for Scoped<F> {
    type Output = F::Output;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let _scope = this.access.enter();
        this.future.as_mut().poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revoke_is_shared_but_reenable_does_not_revive_old_work() {
        let first = NetworkAccess::new(true);
        let worker = first.clone();
        assert!(
            worker.allowed(),
            "positive control: original work is admitted"
        );
        first.revoke();
        assert!(!worker.allowed());
        let next = NetworkAccess::new(true);
        assert!(next.allowed());
        assert!(!worker.allowed(), "reenabling requires a fresh lease");
    }
    #[test]
    fn nested_scopes_restore_the_callers_access_on_success_and_panic() {
        let outer = NetworkAccess::new(false);
        let _outer = outer.enter();
        assert!(!NetworkAccess::current().allowed());
        {
            let enabled = NetworkAccess::new(true);
            let _inner = enabled.enter();
            assert!(
                NetworkAccess::current().allowed(),
                "positive control: inner scope is effective"
            );
        }
        assert!(!NetworkAccess::current().allowed());
        let _ = std::panic::catch_unwind(|| {
            let _inner = NetworkAccess::new(true).enter();
            panic!("synthetic worker panic")
        });
        assert!(!NetworkAccess::current().allowed());
    }
    #[test]
    fn future_poll_carries_permission_to_a_worker_and_does_not_leak_it() {
        struct Wake;
        impl std::task::Wake for Wake {
            fn wake(self: Arc<Self>) {}
        }
        let access = NetworkAccess::new(true);
        let retained = access.clone();
        let future = scoped(access, async { NetworkAccess::current().allowed() });
        retained.revoke();
        std::thread::spawn(move || {
            assert!(
                NetworkAccess::current().allowed(),
                "standalone worker starts unbound"
            );
            let waker = std::task::Waker::from(Arc::new(Wake));
            let mut cx = std::task::Context::from_waker(&waker);
            let mut future = Box::pin(future);
            assert_eq!(
                std::future::Future::poll(future.as_mut(), &mut cx),
                std::task::Poll::Ready(false)
            );
            assert!(
                NetworkAccess::current().allowed(),
                "poll must restore worker TLS"
            );
        })
        .join()
        .unwrap();
    }
}
