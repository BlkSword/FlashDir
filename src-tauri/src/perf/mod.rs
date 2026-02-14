use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use lazy_static::lazy_static;

/// 扫描性能指标
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanMetrics {
    pub scan_id: String,
    pub path: String,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: u64,
    pub io_phase_ms: u64,
    pub compute_phase_ms: u64,
    pub serialize_phase_ms: u64,
    pub cache_phase_ms: u64,
    pub files_scanned: usize,
    pub dirs_scanned: usize,
    pub bytes_read: u64,
    pub io_operations: usize,
    pub io_throughput_mbps: f64,
    pub threads_used: usize,
    pub cpu_usage_percent: f64,
    pub memory_peak_mb: f64,
    pub memory_allocated_mb: f64,
    pub cache_hit: bool,
    pub cache_read_time_ms: u64,
    pub errors: Vec<String>,
}

impl Default for ScanMetrics {
    fn default() -> Self {
        Self {
            scan_id: uuid::Uuid::new_v4().to_string(),
            path: String::new(),
            start_time: chrono::Utc::now(),
            end_time: None,
            duration_ms: 0,
            io_phase_ms: 0,
            compute_phase_ms: 0,
            serialize_phase_ms: 0,
            cache_phase_ms: 0,
            files_scanned: 0,
            dirs_scanned: 0,
            bytes_read: 0,
            io_operations: 0,
            io_throughput_mbps: 0.0,
            threads_used: 0,
            cpu_usage_percent: 0.0,
            memory_peak_mb: 0.0,
            memory_allocated_mb: 0.0,
            cache_hit: false,
            cache_read_time_ms: 0,
            errors: Vec::new(),
        }
    }
}

pub struct PerformanceMonitor {
    current_scan: Mutex<Option<ScanSession>>,
    history: Mutex<VecDeque<ScanMetrics>>,
    max_history: usize,
}

struct ScanSession {
    metrics: ScanMetrics,
    io_timer: Instant,
    compute_timer: Instant,
    start_instant: Instant,
}

lazy_static! {
    static ref MONITOR: Arc<PerformanceMonitor> = Arc::new(PerformanceMonitor::new(50));
}

impl PerformanceMonitor {
    pub fn new(max_history: usize) -> Self {
        Self {
            current_scan: Mutex::new(None),
            history: Mutex::new(VecDeque::with_capacity(max_history)),
            max_history,
        }
    }

    pub fn instance() -> Arc<PerformanceMonitor> {
        MONITOR.clone()
    }

    pub fn start_scan(&self, path: &str) -> String {
        let scan_id = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();

        let session = ScanSession {
            metrics: ScanMetrics {
                scan_id: scan_id.clone(),
                path: path.to_string(),
                start_time: chrono::Utc::now(),
                ..Default::default()
            },
            io_timer: now,
            compute_timer: now,
            start_instant: now,
        };

        *self.current_scan.lock() = Some(session);
        scan_id
    }

    pub fn start_io_phase(&self) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.io_timer = Instant::now();
        }
    }

    pub fn end_io_phase(&self) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.metrics.io_phase_ms = session.io_timer.elapsed().as_millis() as u64;
        }
    }

    pub fn start_compute_phase(&self) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.compute_timer = Instant::now();
        }
    }

    pub fn end_compute_phase(&self) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.metrics.compute_phase_ms = session.compute_timer.elapsed().as_millis() as u64;
        }
    }

    pub fn update_io_stats(&self, files: usize, dirs: usize, bytes: u64, operations: usize) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.metrics.files_scanned = files;
            session.metrics.dirs_scanned = dirs;
            session.metrics.bytes_read = bytes;
            session.metrics.io_operations = operations;

            let elapsed_sec = session.io_timer.elapsed().as_secs_f64();
            if elapsed_sec > 0.0 {
                session.metrics.io_throughput_mbps = (bytes as f64 / 1024.0 / 1024.0) / elapsed_sec;
            }
        }
    }

    pub fn update_memory_stats(&self, peak_mb: f64, allocated_mb: f64) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.metrics.memory_peak_mb = peak_mb;
            session.metrics.memory_allocated_mb = allocated_mb;
        }
    }

    pub fn set_threads_used(&self, threads: usize) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.metrics.threads_used = threads;
        }
    }

    pub fn record_cache_hit(&self, read_time_ms: u64) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.metrics.cache_hit = true;
            session.metrics.cache_read_time_ms = read_time_ms;
        }
    }

    pub fn add_error(&self, error: String) {
        if let Some(session) = self.current_scan.lock().as_mut() {
            session.metrics.errors.push(error);
        }
    }

    pub fn end_scan(&self) -> Option<ScanMetrics> {
        let mut current = self.current_scan.lock();

        if let Some(session) = current.take() {
            let mut metrics = session.metrics;
            metrics.end_time = Some(chrono::Utc::now());
            metrics.duration_ms = session.start_instant.elapsed().as_millis() as u64;

            let mut history = self.history.lock();
            if history.len() >= self.max_history {
                history.pop_front();
            }
            history.push_back(metrics.clone());

            return Some(metrics);
        }

        None
    }

    pub fn get_current_metrics(&self) -> Option<ScanMetrics> {
        self.current_scan.lock().as_ref().map(|s| s.metrics.clone())
    }

    pub fn get_history(&self) -> Vec<ScanMetrics> {
        self.history.lock().iter().cloned().collect()
    }

    pub fn clear_history(&self) {
        self.history.lock().clear();
    }

    pub fn get_summary(&self) -> PerformanceSummary {
        let history = self.history.lock();

        if history.is_empty() {
            return PerformanceSummary::default();
        }

        let total_scans = history.len();
        let cache_hits = history.iter().filter(|m| m.cache_hit).count();
        let avg_duration = history.iter().map(|m| m.duration_ms).sum::<u64>() / total_scans as u64;
        let avg_io_time = history.iter().map(|m| m.io_phase_ms).sum::<u64>() / total_scans as u64;
        let avg_throughput = history.iter().map(|m| m.io_throughput_mbps).sum::<f64>() / total_scans as f64;

        let (min_duration, max_duration) = history.iter().fold(
            (u64::MAX, u64::MIN),
            |(min, max), m| (min.min(m.duration_ms), max.max(m.duration_ms))
        );

        PerformanceSummary {
            total_scans,
            cache_hits,
            cache_hit_rate: cache_hits as f64 / total_scans as f64,
            avg_scan_duration_ms: avg_duration,
            min_scan_duration_ms: min_duration,
            max_scan_duration_ms: max_duration,
            avg_io_time_ms: avg_io_time,
            avg_throughput_mbps: avg_throughput,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceSummary {
    pub total_scans: usize,
    pub cache_hits: usize,
    pub cache_hit_rate: f64,
    pub avg_scan_duration_ms: u64,
    pub min_scan_duration_ms: u64,
    pub max_scan_duration_ms: u64,
    pub avg_io_time_ms: u64,
    pub avg_throughput_mbps: f64,
}
