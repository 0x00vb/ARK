//! Finite admission shared by all calls to one Exec: a few active runs, a bounded number of
//! waiting calls, and one run per project. A call waiting for its project holds no active slot.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as ProjectLock, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

const CLOSED: &str = "code runs are unavailable after incomplete cleanup; restart the app";

pub(crate) struct Admission {
    slots: Arc<Semaphore>,
    max_waiting: usize,
    waiting: AtomicUsize,
    projects: Mutex<HashMap<PathBuf, Arc<ProjectLock<()>>>>,
}

/// An admitted run. Fields drop in order: the slot, the project, then its unused map entry.
pub(crate) struct Permit {
    _slot: OwnedSemaphorePermit,
    _project: OwnedMutexGuard<()>,
    _entry: Entry,
}

/// Removes a project's lock from the map once no call holds or waits for it.
struct Entry {
    owner: Arc<Admission>,
    project: PathBuf,
}

/// One counted waiting call; cancellation (the future dropped) uncounts it.
struct Waiter<'a>(&'a AtomicUsize);

impl Admission {
    pub(crate) fn new(active: usize, waiting: usize) -> Arc<Self> {
        assert!(active > 0);
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(active)),
            max_waiting: waiting,
            waiting: AtomicUsize::new(0),
            projects: Mutex::default(),
        })
    }

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        project: PathBuf,
    ) -> Result<Permit, &'static str> {
        if self.is_closed() {
            return Err(CLOSED);
        }
        // Declared before `lock`, so every exit path drops the lock's clone before the entry.
        let entry = Entry {
            owner: self.clone(),
            project,
        };
        let lock = self
            .projects
            .lock()
            .unwrap()
            .entry(entry.project.clone())
            .or_default()
            .clone();
        // Admitted at once without counting as waiting; otherwise wait for the project
        // first, so a busy project never holds a slot another project could use.
        let guard = lock.clone().try_lock_owned().ok();
        let slot = match &guard {
            Some(_) => self.slots.clone().try_acquire_owned().ok(),
            None => None,
        };
        let (project, slot) = match (guard, slot) {
            (Some(guard), Some(slot)) => (guard, slot),
            (guard, _) => {
                let _waiter = Waiter::enter(&self.waiting, self.max_waiting)
                    .ok_or("too many code runs are already waiting")?;
                let guard = match guard {
                    Some(guard) => guard,
                    None => lock.lock_owned().await,
                };
                let slot = self.slots.clone().acquire_owned().await;
                (guard, slot.map_err(|_| CLOSED)?)
            }
        };
        Ok(Permit {
            _slot: slot,
            _project: project,
            _entry: entry,
        })
    }

    /// Never admit replacement work when the previous process group could not be stopped.
    pub(crate) fn close(&self) {
        self.slots.close();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.slots.is_closed()
    }

    #[cfg(all(test, unix))]
    pub(crate) fn queued(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        let mut projects = self.owner.projects.lock().unwrap();
        // The map holds one reference; any other is a call that holds or waits for it.
        if projects
            .get(&self.project)
            .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            projects.remove(&self.project);
        }
    }
}

impl<'a> Waiter<'a> {
    fn enter(count: &'a AtomicUsize, max: usize) -> Option<Self> {
        count
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(count))
    }
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn queued(
        gate: Arc<Admission>,
        name: &str,
    ) -> tokio::task::JoinHandle<Result<Permit, &'static str>> {
        let name = PathBuf::from(name);
        let task = tokio::spawn(async move { gate.acquire(name).await });
        tokio::task::yield_now().await;
        task
    }

    #[tokio::test]
    async fn limits_active_and_waiting_and_removes_cancelled_waiters() {
        let gate = Admission::new(1, 1);
        let first = gate.acquire("a".into()).await.unwrap();
        let second = queued(gate.clone(), "b").await;
        assert!(gate.acquire("c".into()).await.is_err());
        second.abort();
        let _ = second.await;
        assert_eq!(gate.waiting.load(Ordering::SeqCst), 0);
        let third = queued(gate.clone(), "c").await;
        drop(first);
        drop(third.await.unwrap().unwrap());
        assert!(gate.projects.lock().unwrap().is_empty());
        assert_eq!(gate.waiting.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_busy_project_does_not_occupy_another_slot_or_block_other_projects() {
        let gate = Admission::new(2, 3);
        let a = gate.acquire("a".into()).await.unwrap();
        let a2 = queued(gate.clone(), "a").await;
        let b = gate.acquire("b".into()).await.unwrap();
        let c = queued(gate.clone(), "c").await;
        drop(b);
        let c = c.await.unwrap().unwrap();
        assert!(!a2.is_finished());
        drop(a);
        drop(a2.await.unwrap().unwrap());
        drop(c);
        assert!(gate.projects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_free_slot_is_not_counted_against_the_waiting_limit() {
        let gate = Admission::new(2, 1);
        let a = gate.acquire("a".into()).await.unwrap();
        let a2 = queued(gate.clone(), "a").await;
        let b = gate.acquire("b".into()).await.unwrap();
        drop((a, b));
        drop(a2.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn closing_rejects_waiting_and_future_work() {
        let gate = Admission::new(1, 2);
        let first = gate.acquire("a".into()).await.unwrap();
        let waiting = queued(gate.clone(), "b").await;
        gate.close();
        assert!(waiting.await.unwrap().is_err());
        drop(first);
        assert!(gate.acquire("c".into()).await.is_err());
    }

    #[tokio::test]
    async fn cancellation_after_grant_returns_the_permit_without_releasing_another_run() {
        for _ in 0..100 {
            let gate = Admission::new(1, 2);
            let first = gate.acquire("a".into()).await.unwrap();
            let waiting = queued(gate.clone(), "b").await;
            drop(first);
            waiting.abort();
            let _ = waiting.await;
            let next =
                tokio::time::timeout(std::time::Duration::from_secs(1), gate.acquire("b".into()))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(gate.slots.available_permits(), 0);
            drop(next);
            assert_eq!(gate.slots.available_permits(), 1);
        }
    }
}
