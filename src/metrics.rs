use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use sysinfo::System;

use crate::config::{BenchmarkCase, MIB};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyPercentiles {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputPercentiles {
    pub p50_mbps: f64,
    pub p95_mbps: f64,
    pub p99_mbps: f64,
    pub p999_mbps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceMetrics {
    pub avg_cpu_percent: f64,
    pub peak_cpu_percent: f64,
    pub avg_mem_bytes: u64,
    pub peak_mem_bytes: u64,
    pub error_rate_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub case: BenchmarkCase,
    pub warmup_complete: bool,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub latency: LatencyPercentiles,
    pub throughput: ThroughputPercentiles,
    pub resources: ResourceMetrics,
}

pub struct MetricsCollector {
    latency_hist: Histogram<u64>,
    throughput_hist: Histogram<u64>,
    total_requests: u64,
    failed_requests: u64,
    cpu_samples: Vec<f64>,
    mem_samples: Vec<u64>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            // Latency in microseconds, max 10 minutes
            latency_hist: Histogram::new_with_bounds(1, 600_000_000, 3).unwrap(),
            // Throughput in kbps (to keep as integer), max 100 Gbps
            throughput_hist: Histogram::new_with_bounds(1, 100_000_000, 3).unwrap(),
            total_requests: 0,
            failed_requests: 0,
            cpu_samples: Vec::new(),
            mem_samples: Vec::new(),
        }
    }

    pub fn record_request(&mut self, latency: Duration, bytes_transferred: u64, success: bool) {
        self.total_requests += 1;
        if !success {
            self.failed_requests += 1;
            return;
        }

        let latency_us = latency.as_micros() as u64;
        let _ = self.latency_hist.record(latency_us.max(1));

        // Throughput in kbps: (bytes * 8) / seconds / 1000
        let secs = latency.as_secs_f64();
        if secs > 0.0 {
            let throughput_kbps = ((bytes_transferred as f64 * 8.0) / secs / 1000.0) as u64;
            let _ = self.throughput_hist.record(throughput_kbps.max(1));
        }
    }

    pub fn record_resource_sample(&mut self, sys: &System) {
        let total_cpu: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum();
        let avg_cpu = total_cpu / sys.cpus().len().max(1) as f32;
        self.cpu_samples.push(avg_cpu as f64);
        self.mem_samples.push(sys.used_memory());
    }

    pub fn finalize(self, case: &BenchmarkCase, warmup_complete: bool) -> BenchmarkResult {
        let latency = if self.latency_hist.len() > 0 {
            LatencyPercentiles {
                p50_ms: self.latency_hist.value_at_percentile(50.0) as f64 / 1000.0,
                p95_ms: self.latency_hist.value_at_percentile(95.0) as f64 / 1000.0,
                p99_ms: self.latency_hist.value_at_percentile(99.0) as f64 / 1000.0,
                p999_ms: self.latency_hist.value_at_percentile(99.9) as f64 / 1000.0,
            }
        } else {
            LatencyPercentiles {
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                p999_ms: 0.0,
            }
        };

        // Throughput: convert from kbps back to Mbps (decimal)
        // Standard percentiles: p50 = median, p95 = 95th percentile, etc.
        let throughput = if self.throughput_hist.len() > 0 {
            ThroughputPercentiles {
                p50_mbps: self.throughput_hist.value_at_percentile(50.0) as f64 / 1000.0,
                p95_mbps: self.throughput_hist.value_at_percentile(95.0) as f64 / 1000.0,
                p99_mbps: self.throughput_hist.value_at_percentile(99.0) as f64 / 1000.0,
                p999_mbps: self.throughput_hist.value_at_percentile(99.9) as f64 / 1000.0,
            }
        } else {
            ThroughputPercentiles {
                p50_mbps: 0.0,
                p95_mbps: 0.0,
                p99_mbps: 0.0,
                p999_mbps: 0.0,
            }
        };

        let avg_cpu = if self.cpu_samples.is_empty() {
            0.0
        } else {
            self.cpu_samples.iter().sum::<f64>() / self.cpu_samples.len() as f64
        };
        let peak_cpu = self
            .cpu_samples
            .iter()
            .cloned()
            .fold(0.0_f64, f64::max);

        let avg_mem = if self.mem_samples.is_empty() {
            0
        } else {
            self.mem_samples.iter().sum::<u64>() / self.mem_samples.len() as u64
        };
        let peak_mem = self.mem_samples.iter().cloned().max().unwrap_or(0);

        let error_rate = if self.total_requests > 0 {
            (self.failed_requests as f64 / self.total_requests as f64) * 100.0
        } else {
            0.0
        };

        BenchmarkResult {
            case: case.clone(),
            warmup_complete,
            total_requests: self.total_requests,
            successful_requests: self.total_requests - self.failed_requests,
            failed_requests: self.failed_requests,
            latency,
            throughput,
            resources: ResourceMetrics {
                avg_cpu_percent: avg_cpu,
                peak_cpu_percent: peak_cpu,
                avg_mem_bytes: avg_mem,
                peak_mem_bytes: peak_mem,
                error_rate_percent: error_rate,
            },
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * MIB {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * MIB as f64))
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}
