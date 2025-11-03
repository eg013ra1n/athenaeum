// Worker pool for parallel image processing with LibVIPS

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use super::{VipsProcessor, ProcessParams, ProcessedImage};

/// Priority levels for processing jobs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// User-requested, immediate processing
    Immediate = 0,
    /// Prefetch adjacent images
    High = 1,
    /// Normal batch processing
    Normal = 2,
    /// Background cache warming
    Low = 3,
}

/// Processing job for the worker pool
#[derive(Debug)]
pub enum Job {
    /// Process a single image
    ProcessImage {
        file_path: PathBuf,
        params: ProcessParams,
        priority: Priority,
        callback: oneshot::Sender<Result<ProcessedImage>>,
    },
    /// Prefetch multiple images
    Prefetch {
        files: Vec<PathBuf>,
        params: ProcessParams,
        priority: Priority,
    },
    /// Warm cache for an entire frame set
    WarmCache {
        frame_set_id: i64,
        params: ProcessParams,
    },
    /// Shutdown signal
    Shutdown,
}

/// Worker pool for concurrent image processing
pub struct CacheWorkerPool {
    workers: Vec<Worker>,
    sender: mpsc::Sender<Job>,
    stats: Arc<Mutex<WorkerPoolStats>>,
}

/// Individual worker in the pool
struct Worker {
    id: usize,
    handle: tokio::task::JoinHandle<()>,
}

/// Statistics for the worker pool
#[derive(Debug, Default)]
pub struct WorkerPoolStats {
    pub jobs_processed: u64,
    pub jobs_failed: u64,
    pub total_processing_time_ms: u64,
    pub average_processing_time_ms: u64,
}

impl CacheWorkerPool {
    /// Create a new worker pool
    pub fn new(num_workers: Option<usize>) -> Result<Self> {
        // Default to number of CPU cores, max 8
        let num_workers = num_workers.unwrap_or_else(num_cpus::get).min(8);
        println!("🚀 Creating VipsProcessor worker pool with {} workers", num_workers);

        let (sender, receiver) = mpsc::channel::<Job>(100);
        let receiver = Arc::new(Mutex::new(receiver));
        let stats = Arc::new(Mutex::new(WorkerPoolStats::default()));

        let mut workers = Vec::with_capacity(num_workers);

        for id in 0..num_workers {
            let receiver = Arc::clone(&receiver);
            let stats = Arc::clone(&stats);

            let handle = tokio::spawn(async move {
                // Each worker gets its own VipsProcessor
                let processor = match VipsProcessor::new() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("❌ Worker {} failed to initialize VipsProcessor: {}", id, e);
                        return;
                    }
                };

                println!("✅ Worker {} initialized with VipsProcessor", id);

                loop {
                    let job = {
                        let mut rx = receiver.lock().await;
                        rx.recv().await
                    };

                    match job {
                        Some(Job::ProcessImage { file_path, params, callback, .. }) => {
                            let start = std::time::Instant::now();
                            let result = processor.process_fits_to_jpeg(&file_path, &params);
                            let duration = start.elapsed();

                            // Update stats
                            {
                                let mut s = stats.lock().await;
                                if result.is_ok() {
                                    s.jobs_processed += 1;
                                } else {
                                    s.jobs_failed += 1;
                                }
                                s.total_processing_time_ms += duration.as_millis() as u64;
                                if s.jobs_processed > 0 {
                                    s.average_processing_time_ms =
                                        s.total_processing_time_ms / s.jobs_processed;
                                }
                            }

                            // Send result back
                            let _ = callback.send(result);

                            println!(
                                "  Worker {}: Processed {} in {:?}",
                                id,
                                file_path.display(),
                                duration
                            );
                        }
                        Some(Job::Prefetch { files, params, .. }) => {
                            println!("  Worker {}: Prefetching {} files", id, files.len());

                            for file_path in files {
                                let _ = processor.process_fits_to_jpeg(&file_path, &params);
                            }
                        }
                        Some(Job::WarmCache { frame_set_id, params }) => {
                            println!("  Worker {}: Warming cache for frame set {}", id, frame_set_id);
                            // Implementation would load all files for the frame set
                            // and process them with the given parameters
                        }
                        Some(Job::Shutdown) | None => {
                            println!("  Worker {} shutting down", id);
                            break;
                        }
                    }
                }
            });

            workers.push(Worker { id, handle });
        }

        Ok(Self {
            workers,
            sender,
            stats,
        })
    }

    /// Process a single image
    pub async fn process(&self, file_path: PathBuf, params: ProcessParams) -> Result<ProcessedImage> {
        let (tx, rx) = oneshot::channel();

        let job = Job::ProcessImage {
            file_path,
            params,
            priority: Priority::Immediate,
            callback: tx,
        };

        self.sender.send(job).await?;
        rx.await?
    }

    /// Process multiple images in parallel
    pub async fn process_batch(
        &self,
        files: Vec<PathBuf>,
        params: ProcessParams,
    ) -> Vec<Result<ProcessedImage>> {
        let mut receivers = Vec::new();

        for file_path in files {
            let (tx, rx) = oneshot::channel();

            let job = Job::ProcessImage {
                file_path,
                params: params.clone(),
                priority: Priority::Normal,
                callback: tx,
            };

            if self.sender.send(job).await.is_ok() {
                receivers.push(rx);
            }
        }

        // Collect results
        let mut results = Vec::new();
        for rx in receivers {
            match rx.await {
                Ok(result) => results.push(result),
                Err(_) => results.push(Err(anyhow::anyhow!("Worker failed"))),
            }
        }

        results
    }

    /// Prefetch images in the background
    pub async fn prefetch(&self, files: Vec<PathBuf>, params: ProcessParams) -> Result<()> {
        let job = Job::Prefetch {
            files,
            params,
            priority: Priority::Low,
        };

        self.sender.send(job).await?;
        Ok(())
    }

    /// Get current statistics
    pub async fn get_stats(&self) -> WorkerPoolStats {
        self.stats.lock().await.clone()
    }

    /// Shutdown the worker pool gracefully
    pub async fn shutdown(self) -> Result<()> {
        // Send shutdown signal to all workers
        for _ in 0..self.workers.len() {
            let _ = self.sender.send(Job::Shutdown).await;
        }

        // Wait for all workers to finish
        for worker in self.workers {
            let _ = worker.handle.await;
        }

        println!("✅ Worker pool shut down successfully");
        Ok(())
    }
}

impl Clone for WorkerPoolStats {
    fn clone(&self) -> Self {
        Self {
            jobs_processed: self.jobs_processed,
            jobs_failed: self.jobs_failed,
            total_processing_time_ms: self.total_processing_time_ms,
            average_processing_time_ms: self.average_processing_time_ms,
        }
    }
}