use clap::Parser;
use futures::StreamExt;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::System;
use tokio::sync::Mutex;

use streaming_bench::config::*;
use streaming_bench::metrics::*;
use streaming_bench::report;

const CLIENT_READ_AHEAD: usize = 2; // bounded read-ahead: 2 chunks max

#[derive(Parser)]
#[command(name = "streaming-bench-client")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    server: String,

    #[arg(long, default_value_t = DEFAULT_RPS)]
    rps: u64,

    #[arg(long, default_value_t = DEFAULT_DURATION_SECS)]
    duration: u64,

    #[arg(long, default_value_t = DEFAULT_WARMUP_SECS)]
    warmup: u64,

    #[arg(long, default_value_t = DEFAULT_WARMUP_MIN_OPS)]
    warmup_min_ops: u64,

    #[arg(long, default_value = "results")]
    output: PathBuf,

    #[arg(long)]
    upload_only: bool,

    #[arg(long)]
    download_only: bool,

    #[arg(long)]
    file_size: Option<u64>,

    #[arg(long)]
    chunk_size: Option<u64>,

    #[arg(long)]
    quick: bool,

    /// RNG thread pool size
    #[arg(long, default_value_t = 2)]
    rng_threads: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let mut args = Args::parse();

    if args.quick {
        args.warmup = 30;
        args.duration = 60;
        args.rps = 5;
    }

    // Dedicated RNG pool for client byte generation
    let rng_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.rng_threads)
            .thread_name(|i| format!("client-rng-{}", i))
            .build()?,
    );

    let mut cases: Vec<BenchmarkCase> = all_cases();

    if args.upload_only {
        cases.retain(|c| c.operation == Operation::Upload);
    }
    if args.download_only {
        cases.retain(|c| c.operation == Operation::Download);
    }
    if let Some(fs) = args.file_size {
        cases.retain(|c| c.file_size_mib == fs);
    }
    if let Some(cs) = args.chunk_size {
        cases.retain(|c| c.chunk_size_mib == cs);
    }

    println!("=== Streaming Benchmark Client ===");
    println!("Server: {}", args.server);
    println!("RPS: {}, Duration: {}s, Warmup: {}s", args.rps, args.duration, args.warmup);
    println!("Total cases to run: {}", cases.len());
    println!("Client read-ahead: {} chunks, RNG threads: {}", CLIENT_READ_AHEAD, args.rng_threads);
    println!();

    print_theoretical_estimates();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .pool_max_idle_per_host(100)
        .build()?;

    match client.get(format!("{}/health", args.server)).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("Server health check: OK");
        }
        _ => {
            eprintln!("ERROR: Server not reachable at {}", args.server);
            std::process::exit(1);
        }
    }

    let mut all_results: Vec<BenchmarkResult> = Vec::new();
    let mut per_request_records: Vec<(String, u64, u64, f64, f64, bool)> = Vec::new();

    for (i, case) in cases.iter().enumerate() {
        println!(
            "\n[{}/{}] {} - File: {} MiB, Chunk: {} MiB",
            i + 1, cases.len(), case.operation, case.file_size_mib, case.chunk_size_mib
        );

        // --- Warm-up ---
        println!("  Warming up ({} seconds or {} ops)...", args.warmup, args.warmup_min_ops);
        let warmup_start = Instant::now();
        let mut warmup_ops = 0u64;

        loop {
            let elapsed = warmup_start.elapsed().as_secs();
            if elapsed >= args.warmup && warmup_ops >= args.warmup_min_ops {
                break;
            }
            if elapsed >= args.warmup * 2 {
                break;
            }

            run_single_request(&client, &args.server, case, &rng_pool).await;
            warmup_ops += 1;

            if warmup_ops % 50 == 0 {
                println!("    Warmup: {} ops, {}s elapsed", warmup_ops, elapsed);
            }
        }
        let warmup_complete = warmup_ops >= args.warmup_min_ops
            || warmup_start.elapsed().as_secs() >= args.warmup;
        println!(
            "  Warmup complete: {} ops in {:.1}s",
            warmup_ops, warmup_start.elapsed().as_secs_f64()
        );

        // --- Measured run ---
        println!("  Running measured phase ({} seconds at {} RPS)...", args.duration, args.rps);
        let mut collector = MetricsCollector::new();
        let sys = Arc::new(Mutex::new(System::new_all()));

        // Resource sampling
        let sys_clone = sys.clone();
        let sample_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let mut s = sys_clone.lock().await;
                s.refresh_all();
            }
        });

        let interval = Duration::from_millis(1000 / args.rps);
        let run_start = Instant::now();
        let run_duration = Duration::from_secs(args.duration);
        let mut ticker = tokio::time::interval(interval);
        let mut pending = futures::stream::FuturesUnordered::new();
        let mut completed = 0u64;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if run_start.elapsed() < run_duration {
                        let client_clone = client.clone();
                        let server = args.server.clone();
                        let case_clone = case.clone();
                        let pool = rng_pool.clone();
                        pending.push(tokio::spawn(async move {
                            let start = Instant::now();
                            let result = run_single_request(&client_clone, &server, &case_clone, &pool).await;
                            let latency = start.elapsed();
                            (latency, result)
                        }));
                    }
                }
                Some(join_result) = pending.next() => {
                    if let Ok((latency, success)) = join_result {
                        let bytes = case.file_size_bytes();
                        collector.record_request(latency, bytes, success);

                        let lat_ms = latency.as_secs_f64() * 1000.0;
                        let tp_mbps = if latency.as_secs_f64() > 0.0 {
                            (bytes as f64 * 8.0) / latency.as_secs_f64() / 1_000_000.0
                        } else {
                            0.0
                        };
                        per_request_records.push((
                            case.operation.to_string(),
                            case.file_size_mib, case.chunk_size_mib,
                            lat_ms, tp_mbps, success,
                        ));

                        completed += 1;
                        if completed % 100 == 0 {
                            println!(
                                "    Completed: {} requests, {:.1}s elapsed",
                                completed, run_start.elapsed().as_secs_f64()
                            );
                        }
                    }
                }
                else => {}
            }

            if run_start.elapsed() >= run_duration && pending.is_empty() {
                break;
            }
        }

        sample_handle.abort();

        {
            let s = sys.lock().await;
            collector.record_resource_sample(&s);
        }

        let result = collector.finalize(case, warmup_complete);
        println!(
            "  Done: {} requests | p50={:.1}ms p99={:.1}ms | p50 tp={:.1} Mbps p99 tp={:.1} Mbps | err={:.1}%",
            result.total_requests,
            result.latency.p50_ms, result.latency.p99_ms,
            result.throughput.p50_mbps, result.throughput.p99_mbps,
            result.resources.error_rate_percent,
        );
        all_results.push(result);
    }

    println!("\n=== Generating Reports ===");
    report::write_csv_report(&all_results, &args.output)?;
    report::write_per_request_csv(&per_request_records, &args.output)?;

    println!("\n=== Benchmark Complete ===");
    Ok(())
}

async fn run_single_request(
    client: &reqwest::Client,
    server: &str,
    case: &BenchmarkCase,
    rng_pool: &Arc<rayon::ThreadPool>,
) -> bool {
    match case.operation {
        Operation::Upload => run_upload(client, server, case, rng_pool).await,
        Operation::Download => run_download(client, server, case).await,
    }
}

/// Upload: demand-driven streaming with bounded read-ahead.
/// RNG generates next chunk only when downstream can accept it.
async fn run_upload(
    client: &reqwest::Client,
    server: &str,
    case: &BenchmarkCase,
    rng_pool: &Arc<rayon::ThreadPool>,
) -> bool {
    let file_size = case.file_size_bytes();
    let chunk_size = case.chunk_size_bytes();

    // Bounded read-ahead: 2 chunks max in flight
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(CLIENT_READ_AHEAD);

    // RNG on dedicated pool — generates only when channel has space (backpressure)
    rng_pool.spawn(move || {
        let mut remaining = file_size;
        let mut rng = StdRng::from_entropy();
        while remaining > 0 {
            let this_chunk = std::cmp::min(remaining, chunk_size) as usize;
            let mut buf = vec![0u8; this_chunk];
            rng.fill_bytes(&mut buf);
            remaining -= this_chunk as u64;
            // blocking_send respects bounded channel — backpressure here
            if tx.blocking_send(Ok(buf)).is_err() {
                break;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = reqwest::Body::wrap_stream(stream);

    let url = format!("{}/upload?chunk_size_mib={}", server, case.chunk_size_mib);

    match client.post(&url).body(body).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn run_download(
    client: &reqwest::Client,
    server: &str,
    case: &BenchmarkCase,
) -> bool {
    let url = format!(
        "{}/download?file_size_mib={}&chunk_size_mib={}",
        server, case.file_size_mib, case.chunk_size_mib
    );

    match client.get(&url).send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                return false;
            }
            let mut stream = resp.bytes_stream();
            let mut total = 0u64;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => total += bytes.len() as u64,
                    Err(_) => return false,
                }
            }
            total > 0
        }
        Err(_) => false,
    }
}
