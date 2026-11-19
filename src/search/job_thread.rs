//! Threads kept waiting for work

use std::fmt;
use std::io;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SendError, Sender};
use std::thread::{self, JoinHandle};

use super::STACK_SIZE;

/// A thread with a search's stack that runs the jobs it is sent, in order, until dropped.
pub(crate) struct JobThread<J> {
    /// `None` once dropped, which ends the thread's loop.
    jobs: Option<Sender<J>>,
    /// Set when dropped, so the thread skips the jobs still queued.
    closing: Arc<AtomicBool>,
    /// The thread, which `drop` waits for.
    handle: Option<JoinHandle<()>>,
}

impl<J: Send + 'static> JobThread<J> {
    /// Starts a thread named `name` that calls `run_job` on each job it is sent.
    pub(crate) fn spawn(
        name: String,
        mut run_job: impl FnMut(J) + Send + 'static,
    ) -> io::Result<Self> {
        let (jobs, inbox) = mpsc::channel::<J>();
        let closing = Arc::new(AtomicBool::new(false));
        let closed = Arc::clone(&closing);
        let handle = thread::Builder::new().name(name).stack_size(STACK_SIZE).spawn(move || {
            while let Ok(job) = inbox.recv() {
                if closed.load(Ordering::Relaxed) {
                    break;
                }
                _ = panic::catch_unwind(panic::AssertUnwindSafe(|| run_job(job)));
            }
        })?;
        Ok(Self { jobs: Some(jobs), closing, handle: Some(handle) })
    }

    /// Queues `job`, or gives it back if the thread is gone.
    pub(crate) fn send(&self, job: J) -> Result<(), J> {
        match &self.jobs {
            Some(jobs) => jobs.send(job).map_err(|SendError(job)| job),
            None => Err(job),
        }
    }
}

impl<J> Drop for JobThread<J> {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Relaxed);
        self.jobs = None;
        if let Some(handle) = self.handle.take() {
            _ = handle.join();
        }
    }
}

/// Sends its message when dropped, at the end of its scope or during a panic
pub(crate) struct SendOnDrop<T> {
    to: Sender<T>,
    /// Taken when sent.
    message: Option<T>,
}

impl<T> SendOnDrop<T> {
    pub(crate) const fn new(to: Sender<T>, message: T) -> Self {
        Self { to, message: Some(message) }
    }
}

impl<T> Drop for SendOnDrop<T> {
    fn drop(&mut self) {
        if let Some(message) = self.message.take() {
            _ = self.to.send(message);
        }
    }
}

impl<T> fmt::Debug for SendOnDrop<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendOnDrop").field("sent", &self.message.is_none()).finish_non_exhaustive()
    }
}

impl<J> fmt::Debug for JobThread<J> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.handle.as_ref().and_then(|handle| handle.thread().name());
        f.debug_struct("JobThread").field("name", &name).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::time::Duration;

    use super::*;

    #[test]
    fn runs_jobs_in_order_and_outlives_a_panic() {
        let (results, inbox) = mpsc::channel();
        let thread = JobThread::spawn("test".into(), move |n: u32| {
            assert_ne!(n, 2, "job 2 panics");
            results.send(n).expect("the test should be listening");
        })
        .expect("the job thread should start");
        for n in 1..=3 {
            thread.send(n).expect("the job thread should be running");
        }
        assert_eq!(inbox.iter().take(2).collect::<Vec<_>>(), [1, 3]);
    }

    #[test]
    fn dropping_finishes_the_running_job_and_skips_the_queued_ones() {
        let (results, inbox) = mpsc::channel();
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let jobs = JobThread::spawn("test".into(), {
            let (started, release) = (Arc::clone(&started), Arc::clone(&release));
            move |n: u32| {
                if n == 1 {
                    started.wait();
                    release.wait();
                }
                results.send(n).expect("the test should be listening");
            }
        })
        .expect("the job thread should start");
        for n in 1..=3 {
            jobs.send(n).expect("the job thread should be running");
        }
        started.wait();
        // Job 1 is running and the others queued: let job 1 go once the drop has begun.
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            release.wait();
        });
        drop(jobs);
        releaser.join().expect("the releaser should not panic");
        assert_eq!(inbox.iter().collect::<Vec<_>>(), [1]);
    }
}
