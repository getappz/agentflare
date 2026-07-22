use crate::queue::Queue;
use crate::supervisor::Supervisor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct WorkerPool {
    queue: Arc<Queue>,
    handles: Vec<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl WorkerPool {
    pub fn new(queue: Queue) -> Self {
        Self {
            queue: Arc::new(queue),
            handles: vec![],
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&mut self, num_workers: usize) {
        self.running.store(true, Ordering::SeqCst);
        for _ in 0..num_workers {
            let queue = self.queue.clone();
            let running = self.running.clone();
            self.handles.push(std::thread::spawn(move || {
                worker_loop(&queue, &running);
            }));
        }
    }

    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let handles = std::mem::take(&mut self.handles);
        for h in handles {
            let _ = h.join();
        }
    }
}

fn worker_loop(queue: &Queue, running: &AtomicBool) {
    while running.load(Ordering::SeqCst) {
        match queue.dequeue() {
            Ok(Some((id, job))) => {
                let mut sup = Supervisor::new(
                    job.command.clone(),
                    job.args.clone(),
                    job.env.clone(),
                    job.cwd.clone(),
                    job.timeout_secs,
                    job.kill_after_secs,
                    queue.log_dir().to_path_buf(),
                );
                match sup.spawn() {
                    Ok((output, state)) => {
                        let success = state == crate::types::JobState::Exited;
                        if let Err(e) = queue.complete(&id, &output, success) {
                            eprintln!("agentflare-jobs: failed to complete job {id}: {e}");
                        }
                    }
                    Err(e) => {
                        if let Err(qe) = queue.fail(&id, &e.to_string()) {
                            eprintln!("agentflare-jobs: failed to record failure for {id}: {qe}");
                        }
                    }
                }
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("agentflare-jobs: dequeue error: {e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}
