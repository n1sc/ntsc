use std::sync::{Condvar, Mutex, MutexGuard};

use super::core::Goroutine;
use crate::idmap::IdMap;

pub(crate) const SHARD_COUNT: usize = 64;

struct Shard {
    tasks: Mutex<IdMap<Goroutine>>,
    completed: Condvar,
}

/// Task storage is independent of the parking lock. Callbacks must not acquire
/// GLOBAL or re-enter this table; parking paths acquire GLOBAL before a shard.
pub(crate) struct TaskTable {
    shards: Box<[Shard]>,
}

impl Default for TaskTable {
    fn default() -> Self {
        Self {
            shards: (0..SHARD_COUNT)
                .map(|_| Shard {
                    tasks: Mutex::new(IdMap::default()),
                    completed: Condvar::new(),
                })
                .collect(),
        }
    }
}

impl TaskTable {
    fn shard(&self, id: i64) -> &Shard {
        &self.shards[(id.unsigned_abs() as usize) & (SHARD_COUNT - 1)]
    }

    pub(crate) fn insert(&self, id: i64, task: Goroutine) {
        self.shard(id)
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, task);
    }

    pub(crate) fn with<R>(&self, id: i64, read: impl FnOnce(&Goroutine) -> R) -> Option<R> {
        let tasks = self
            .shard(id)
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        tasks.get(&id).map(read)
    }

    pub(crate) fn with_mut<R>(
        &self,
        id: i64,
        update: impl FnOnce(&mut Goroutine) -> R,
    ) -> Option<R> {
        let mut tasks = self
            .shard(id)
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        tasks.get_mut(&id).map(update)
    }

    pub(crate) fn contains(&self, id: i64) -> bool {
        self.with(id, |_| ()).is_some()
    }

    fn take(&self, id: i64) -> Option<Goroutine> {
        self.shard(id)
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id)
    }

    pub(crate) fn remove(&self, id: i64) -> Option<Goroutine> {
        let removed = self.take(id);
        self.notify_completion(id);
        removed
    }

    /// Successful detached completion has neither a wrapper nor async joiners.
    pub(crate) fn reclaim_detached(&self, id: i64) -> Option<Goroutine> {
        self.take(id)
    }

    pub(crate) fn notify_completion(&self, id: i64) {
        self.shard(id).completed.notify_all();
    }

    pub(crate) fn join(&self, id: i64) -> Option<(i64, i64)> {
        let shard = self.shard(id);
        let tasks = shard.tasks.lock().unwrap_or_else(|p| p.into_inner());
        Self::join_locked(shard, tasks, id)
    }

    fn join_locked(
        shard: &Shard,
        mut tasks: MutexGuard<'_, IdMap<Goroutine>>,
        id: i64,
    ) -> Option<(i64, i64)> {
        loop {
            let task = tasks.get_mut(&id)?;
            if task.done {
                return Some((task.result, std::mem::take(&mut task.pending_exception)));
            }
            (tasks, _) = shard
                .completed
                .wait_timeout(tasks, std::time::Duration::from_millis(10))
                .unwrap_or_else(|p| p.into_inner());
        }
    }

    pub(crate) fn drain<R>(&self, mut take: impl FnMut(Goroutine) -> R) -> Vec<R> {
        let mut drained = Vec::new();
        for shard in &self.shards {
            let mut tasks = shard.tasks.lock().unwrap_or_else(|p| p.into_inner());
            drained.extend(tasks.drain().map(|(_, task)| take(task)));
            drop(tasks);
            shard.completed.notify_all();
        }
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    fn waiting_task() -> Goroutine {
        Goroutine {
            tasks: super::super::task_stack::TaskStack::new(),
            handle: 1,
            cleanup: None,
            pending_send: 0,
            pending_send_owned: false,
            recv_result: 0,
            recv_result_owned: false,
            recv_ok: false,
            done: false,
            result: 0,
            pending_exception: 0,
            joiners: Vec::new(),
        }
    }

    #[test]
    fn unrelated_shards_can_progress_independently() {
        let table = Arc::new(TaskTable::default());
        table.insert(1, waiting_task());
        table.insert(2, waiting_task());
        let locked = table.shard(1).tasks.lock().unwrap();
        let (send, recv) = mpsc::channel();
        let other = Arc::clone(&table);
        let worker = std::thread::spawn(move || {
            send.send(other.with_mut(2, |task| task.done = true))
                .unwrap();
        });
        assert_eq!(recv.recv_timeout(Duration::from_secs(5)).unwrap(), Some(()));
        drop(locked);
        worker.join().unwrap();
    }

    #[test]
    fn completion_and_removal_wake_registered_joiners() {
        for remove in [false, true] {
            let table = Arc::new(TaskTable::default());
            table.insert(1, waiting_task());
            let (started_send, started_recv) = mpsc::channel();
            let (finished_send, finished_recv) = mpsc::channel();
            let other = Arc::clone(&table);
            let waiter = std::thread::spawn(move || {
                let shard = other.shard(1);
                let locked = shard.tasks.lock().unwrap();
                started_send.send(()).unwrap();
                finished_send
                    .send(TaskTable::join_locked(shard, locked, 1))
                    .unwrap();
            });
            started_recv.recv_timeout(Duration::from_secs(5)).unwrap();
            if remove {
                table.remove(1);
            } else {
                table.with_mut(1, |task| {
                    task.done = true;
                    task.result = 7;
                    task.pending_exception = 42;
                });
                table.notify_completion(1);
            }
            let expected = if remove { None } else { Some((7, 42)) };
            assert_eq!(
                finished_recv.recv_timeout(Duration::from_secs(5)).unwrap(),
                expected
            );
            waiter.join().unwrap();
            if !remove {
                assert_eq!(table.join(1), Some((7, 0)));
            }
        }
    }
}
