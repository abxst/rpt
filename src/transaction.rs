//! Serialized system transaction execution primitive.
//! The worker is single-writer *within one process*. Cross-process dpkg locks,
//! durable journaling, and recovery are required before real package mutations.
use anyhow::{anyhow, Result};
use std::{future::Future, pin::Pin};
use tokio::sync::{mpsc, oneshot};

type JobFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
type JobFn = Box<dyn FnOnce() -> JobFuture + Send + 'static>;

struct Request {
    task: JobFn,
    reply: oneshot::Sender<Result<()>>,
}

/// Owns an in-process FIFO queue with exactly one executor task.
#[derive(Clone)]
pub struct TransactionQueue {
    sender: mpsc::Sender<Request>,
}

impl TransactionQueue {
    /// Start the single worker. This queue is not durable.
    pub fn new(capacity: usize) -> Result<Self> {
        if capacity == 0 { return Err(anyhow!("queue capacity must be positive")); }
        let (sender, mut receiver) = mpsc::channel::<Request>(capacity);
        tokio::spawn(async move {
            while let Some(Request { task, reply }) = receiver.recv().await {
                // A caller that abandoned a queued request must not cause a
                // not-yet-started transaction to execute later.
                if reply.is_closed() { continue; }
                // Once started, the task runs to completion even if its
                // caller disconnects; never cancel mid-transaction.
                let result = task().await;
                let _ = reply.send(result);
            }
        });
        Ok(Self { sender })
    }

    /// Submit a job to be executed in FIFO receive order.
    pub async fn submit<F, Fut>(&self, job: F) -> Result<()>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let (reply, recv) = oneshot::channel();
        self.sender.send(Request {
            task: Box::new(move || Box::pin(job())),
            reply,
        }).await.map_err(|_| anyhow!("transaction queue closed"))?;
        recv.await.map_err(|_| anyhow!("transaction worker stopped"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn executes_only_one_job_at_a_time() {
        let q = TransactionQueue::new(8).unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut jobs = Vec::new();
        for _ in 0..8 {
            let a = active.clone();
            let p = peak.clone();
            let queue = q.clone();
            jobs.push(tokio::spawn(async move {
                queue.submit(move || async move {
                    let current = a.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(current, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    a.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                }).await
            }));
        }
        for job in jobs { job.await.unwrap().unwrap(); }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn skips_abandoned_queued_jobs_without_interrupting_active_work() {
        let q = TransactionQueue::new(4).unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let active = tokio::spawn({
            let q = q.clone();
            async move {
                q.submit(move || async move {
                    let _ = started_tx.send(());
                    release_rx.await.map_err(|_| anyhow!("release channel closed"))?;
                    Ok(())
                }).await
            }
        });
        started_rx.await.unwrap();

        let executed = Arc::new(AtomicUsize::new(0));
        let (abandoned_reply, abandoned_receiver) = oneshot::channel();
        let counter = executed.clone();
        // Enqueue deterministically while the first job blocks the worker.
        q.sender.send(Request {
            task: Box::new(move || Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
            reply: abandoned_reply,
        }).await.unwrap();
        drop(abandoned_receiver);

        release_tx.send(()).unwrap();
        active.await.unwrap().unwrap();
        // FIFO sentinel: all earlier requests have been processed or skipped.
        q.submit(|| async { Ok(()) }).await.unwrap();
        assert_eq!(executed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rejects_zero_capacity() {
        assert!(TransactionQueue::new(0).is_err());
    }
}
