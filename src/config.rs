use serde::{Deserialize, Serialize};

pub const FILE_SIZES_MIB: &[u64] = &[10, 100, 1000];
pub const CHUNK_SIZES_MIB: &[u64] = &[1, 2, 4, 8, 16, 32, 64];

pub const MIB: u64 = 1024 * 1024;

pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:8080";
pub const DEFAULT_RPS: u64 = 10;
pub const DEFAULT_DURATION_SECS: u64 = 300; // 5 minutes
pub const DEFAULT_WARMUP_SECS: u64 = 120; // 2 minutes
pub const DEFAULT_WARMUP_MIN_OPS: u64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Upload,
    Download,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Operation::Upload => write!(f, "Upload"),
            Operation::Download => write!(f, "Download"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkCase {
    pub operation: Operation,
    pub file_size_mib: u64,
    pub chunk_size_mib: u64,
}

impl BenchmarkCase {
    pub fn file_size_bytes(&self) -> u64 {
        self.file_size_mib * MIB
    }

    pub fn chunk_size_bytes(&self) -> u64 {
        self.chunk_size_mib * MIB
    }
}

/// Print theoretical throughput estimates based on component benchmarks.
/// These are upper-bound estimates for each pipeline stage.
pub fn print_theoretical_estimates() {
    println!("=== Theoretical Throughput Estimates ===");
    println!();
    println!("  Pipeline stage upper bounds (single-core, typical modern CPU):");
    println!("  ---------------------------------------------------------------");
    println!("  Loopback network (localhost):  ~20-40 Gbps");
    println!("  Random byte gen (ChaCha20):    ~1.5-3.0 GB/s  = ~12-24 Gbps");
    println!("  SHA-512 (software):            ~1.5-2.5 GB/s  = ~12-20 Gbps");
    println!("  AES-256-GCM (AES-NI):          ~4-8 GB/s      = ~32-64 Gbps");
    println!("  Memory copy/alloc overhead:     ~8-15 GB/s     = ~64-120 Gbps");
    println!();
    println!("  Upload bottleneck:   SHA-512 + AES-GCM on critical path");
    println!("  -> Expected peak:    ~8-12 Gbps (limited by SHA-512 + encrypt combined)");
    println!();
    println!("  Download bottleneck: Random byte generation + network");
    println!("  -> Expected peak:    ~12-20 Gbps (limited by RNG throughput)");
    println!();
    println!("  Per-chunk overhead hypothesis:");
    println!("  - 1 MiB chunks:  highest per-chunk overhead, more SHA/AES init cycles");
    println!("  - 8-32 MiB:      expected sweet spot (amortized overhead)");
    println!("  - 64 MiB:        may increase tail latency from buffer pressure");
    println!("  ---------------------------------------------------------------");
    println!();
}

pub fn all_cases() -> Vec<BenchmarkCase> {
    let mut cases = Vec::new();
    for &op in &[Operation::Upload, Operation::Download] {
        for &file_size in FILE_SIZES_MIB {
            for &chunk_size in CHUNK_SIZES_MIB {
                cases.push(BenchmarkCase {
                    operation: op,
                    file_size_mib: file_size,
                    chunk_size_mib: chunk_size,
                });
            }
        }
    }
    cases
}
