# Large Content Streaming Performance Benchmark

End-to-end benchmarking tool for upload and download paths where files are segmented into configurable chunk sizes (1–64 MiB). Measures the full data pipeline: network transfer, SHA-512 hashing, AES-256-GCM encryption, and streaming overhead.

## Architecture

```
CLIENT (upload)                         SERVER (upload)
+-----------+    bounded     +--------+    read-ahead    +---------+    write-back    +------------+
| RNG Pool  | --> channel -> | Stream | --> queue (4) -> | Crypto  | --> queue (4) -> | Aggregator |
| (2 thrds) |    (2 chunks)  | Body   |                 | Pool    |                  | (response) |
+-----------+                +--------+                 | (4 thrds)|                  +------------+
                                                        +---------+
                                                        SHA-512 + AES-GCM
                                                        in 64 KiB sub-blocks

CLIENT (download)                       SERVER (download)
+--------+    bytes_stream    +--------+    bounded     +-----------+
| Count  | <-- response  <-- | Stream | <-- queue  <-- | RNG Pool  |
| Bytes  |                   | Body   |    (4 chunks)  | (2 thrds) |
+--------+                   +--------+                +-----------+
```

### Key design decisions

- **Fully streaming**: no full-body buffering anywhere. Peak memory scales with `queue_depth * chunk_size`, not file size.
- **Dedicated thread pools**: RNG and crypto work run on separate rayon pools, isolated from tokio's async I/O runtime.
- **Backpressure**: bounded channels enforce natural backpressure. Producer blocks when consumer is slow.
- **Reusable buffers**: AES-GCM encrypt uses a reusable buffer instead of per-block `to_vec()` allocations.
- **Sub-block encryption**: 64 KiB AES-GCM blocks avoid huge single allocations for large chunks.

## Prerequisites

- Rust 1.75+ (edition 2021)
- cargo

## Build

```bash
cargo build --release
```

Binaries: `target/release/server` and `target/release/client`

## Quick Start

### 1. Start the server

```bash
./target/release/server
```

Options:
| Flag | Default | Description |
|------|---------|-------------|
| `--addr` | `127.0.0.1:8080` | Listen address |
| `--rng-threads` | `2` | Dedicated RNG thread pool size |
| `--crypto-threads` | `4` | Dedicated crypto thread pool size |

### 2. Run the benchmark

```bash
# Full 42-case matrix (3 file sizes x 7 chunk sizes x 2 operations)
./target/release/client

# Quick mode (30s warmup, 60s run, 5 RPS)
./target/release/client --quick

# Specific test: 100 MiB upload, only 4 and 8 MiB chunks
./target/release/client --upload-only --file-size 100 --chunk-size 4
./target/release/client --upload-only --file-size 100 --chunk-size 8
```

### 3. Or use the run script (starts server + client together)

```bash
chmod +x run_benchmark.sh

# Full run with custom params
./run_benchmark.sh --warmup 5 --warmup-min-ops 10 --duration 15 --rps 3

# Upload only, 100 MiB, all chunk sizes
./run_benchmark.sh --upload-only --file-size 100 --duration 30 --rps 5
```

## Client Options

| Flag | Default | Description |
|------|---------|-------------|
| `--server` | `http://127.0.0.1:8080` | Server URL |
| `--rps` | `10` | Requests per second |
| `--duration` | `300` | Measured run duration (seconds) |
| `--warmup` | `120` | Warm-up duration (seconds) |
| `--warmup-min-ops` | `200` | Minimum warm-up operations |
| `--output` | `results` | Output directory for reports |
| `--upload-only` | - | Only run upload cases |
| `--download-only` | - | Only run download cases |
| `--file-size` | all | Filter to specific file size (MiB): 10, 100, 1000 |
| `--chunk-size` | all | Filter to specific chunk size (MiB): 1, 2, 4, 8, 16, 32, 64 |
| `--quick` | - | Quick mode: 30s warmup, 60s run, 5 RPS |
| `--rng-threads` | `2` | Client RNG thread pool size |

## Test Matrix

| Dimension | Values |
|-----------|--------|
| File sizes | 10, 100, 1000 MiB |
| Chunk sizes | 1, 2, 4, 8, 16, 32, 64 MiB |
| Operations | Upload, Download |
| **Total** | **42 benchmark cases** |

## Theoretical Throughput Estimates

Shown before every benchmark run:

```
Pipeline stage upper bounds (single-core, typical modern CPU):
---------------------------------------------------------------
Loopback network (localhost):  ~20-40 Gbps
Random byte gen (ChaCha20):    ~1.5-3.0 GB/s  = ~12-24 Gbps
SHA-512 (software):            ~1.5-2.5 GB/s  = ~12-20 Gbps
AES-256-GCM (AES-NI):          ~4-8 GB/s      = ~32-64 Gbps
Memory copy/alloc overhead:     ~8-15 GB/s     = ~64-120 Gbps

Upload bottleneck:   SHA-512 + AES-GCM on critical path
-> Expected peak:    ~8-12 Gbps

Download bottleneck: Random byte generation + network
-> Expected peak:    ~12-20 Gbps
```

## Output Files

All reports are written to the `--output` directory:

| File | Description |
|------|-------------|
| `benchmark_results.csv` | Aggregated results per case |
| `benchmark_results.json` | Full structured results |
| `benchmark_report.md` | Markdown summary tables |
| `per_request_results.csv` | Raw per-request latency and throughput |
| `logs/server.log` | Server stdout/stderr (when using run_benchmark.sh) |
| `logs/client.log` | Client stdout/stderr (when using run_benchmark.sh) |

## Metrics Collected

### Per case (p50 / p95 / p99 / p999)

- **Latency** (ms): end-to-end request time
- **Throughput** (Mbps): `(bytes_transferred * 8) / latency_seconds / 1,000,000`

### Resource metrics

- Average / Peak CPU %
- Average / Peak memory usage
- Error rate %

## Sample Results (100 MiB Upload)

```
| Chunk (MiB) | p50 Lat (ms) | p99 Lat (ms) | p50 Tp (Mbps) | p99 Tp (Mbps) | Err% |
|------------:|-------------:|-------------:|--------------:|--------------:|-----:|
|           1 |       613.89 |       645.63 |       1367.04 |       1412.10 | 0.00 |
|           2 |       619.52 |       628.22 |       1354.75 |       1371.13 | 0.00 |
|           4 |       634.88 |       644.61 |       1321.98 |       1342.46 | 0.00 |
|           8 |       653.82 |       664.06 |       1284.10 |       1295.36 | 0.00 |
|          16 |       668.16 |       682.50 |       1256.45 |       1270.78 | 0.00 |
|          32 |       686.59 |       696.83 |       1222.65 |       1240.06 | 0.00 |
|          64 |       709.12 |       719.87 |       1182.72 |       1196.03 | 0.00 |
```

Observations:
- Smaller chunks (1-2 MiB) achieve highest throughput (~1.37 Gbps) due to better pipeline overlap
- Larger chunks (32-64 MiB) show ~13% lower throughput from increased per-chunk crypto cost
- Tail latency (p99) stays within 5% of median — streaming pipeline keeps variance low
- 0% error rate across all chunk sizes

## Project Structure

```
src/
├── lib.rs          Module root
├── config.rs       Constants, test matrix, theoretical estimates
├── metrics.rs      HdrHistogram percentile collection + resource metrics
├── report.rs       CSV, JSON, and Markdown report generation
├── server.rs       Axum server: streaming upload/download with dedicated pools
└── client.rs       Benchmark runner: demand-driven streaming client
```

## Upload Pipeline (Server)

```
Body stream → [read-ahead queue, 4 chunks] → Crypto worker (rayon pool)
                                                ├── SHA-512 update
                                                └── AES-GCM encrypt (64 KiB sub-blocks, reusable buffer)
                                              → [write-back queue, 4 chunks] → Aggregator → Response
```

No full-body accumulation. Memory bounded by `read_ahead_chunks * chunk_size`.

## Methodology

See [upload_download_benchmarking_methodology.md](upload_download_benchmarking_methodology.md) for the full benchmarking methodology, including:

- Warm-up requirements
- Workload profile (RPS, duration)
- Benchmark rules for reproducibility
- Metric definitions
- Report format templates
- Interpretation guidance
