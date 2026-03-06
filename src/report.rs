use crate::config::Operation;
use crate::metrics::{format_bytes, BenchmarkResult};
use std::io::Write;
use std::path::Path;

pub fn write_csv_report(results: &[BenchmarkResult], output_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(output_dir)?;

    let csv_path = output_dir.join("benchmark_results.csv");
    let mut wtr = csv::Writer::from_path(&csv_path)?;

    wtr.write_record(&[
        "Operation",
        "File Size (MiB)",
        "Chunk Size (MiB)",
        "Warmup Complete",
        "Requests",
        "p50 Latency (ms)",
        "p95 Latency (ms)",
        "p99 Latency (ms)",
        "p999 Latency (ms)",
        "p50 Throughput (Mbps)",
        "p95 Throughput (Mbps)",
        "p99 Throughput (Mbps)",
        "p999 Throughput (Mbps)",
        "Avg CPU %",
        "Peak CPU %",
        "Avg Mem",
        "Peak Mem",
        "Error Rate %",
    ])?;

    for r in results {
        wtr.write_record(&[
            r.case.operation.to_string(),
            r.case.file_size_mib.to_string(),
            r.case.chunk_size_mib.to_string(),
            r.warmup_complete.to_string(),
            r.total_requests.to_string(),
            format!("{:.2}", r.latency.p50_ms),
            format!("{:.2}", r.latency.p95_ms),
            format!("{:.2}", r.latency.p99_ms),
            format!("{:.2}", r.latency.p999_ms),
            format!("{:.2}", r.throughput.p50_mbps),
            format!("{:.2}", r.throughput.p95_mbps),
            format!("{:.2}", r.throughput.p99_mbps),
            format!("{:.2}", r.throughput.p999_mbps),
            format!("{:.1}", r.resources.avg_cpu_percent),
            format!("{:.1}", r.resources.peak_cpu_percent),
            format_bytes(r.resources.avg_mem_bytes),
            format_bytes(r.resources.peak_mem_bytes),
            format!("{:.2}", r.resources.error_rate_percent),
        ])?;
    }
    wtr.flush()?;
    println!("CSV report written to: {}", csv_path.display());

    // Write JSON
    let json_path = output_dir.join("benchmark_results.json");
    let json = serde_json::to_string_pretty(results).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, e)
    })?;
    std::fs::write(&json_path, json)?;
    println!("JSON report written to: {}", json_path.display());

    // Write markdown summary
    write_markdown_report(results, output_dir)?;

    Ok(())
}

fn write_markdown_report(results: &[BenchmarkResult], output_dir: &Path) -> std::io::Result<()> {
    let md_path = output_dir.join("benchmark_report.md");
    let mut f = std::fs::File::create(&md_path)?;

    writeln!(f, "# Upload/Download Streaming Benchmark Report\n")?;
    writeln!(f, "Generated: {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"))?;
    writeln!(f, "---\n")?;

    for op in &[Operation::Upload, Operation::Download] {
        for &file_size in crate::config::FILE_SIZES_MIB {
            let case_results: Vec<&BenchmarkResult> = results
                .iter()
                .filter(|r| r.case.operation == *op && r.case.file_size_mib == file_size)
                .collect();

            if case_results.is_empty() {
                continue;
            }

            writeln!(f, "## {} Report - {} MiB File\n", op, file_size)?;
            writeln!(
                f,
                "| Chunk (MiB) | Requests | p50 Lat (ms) | p95 Lat (ms) | p99 Lat (ms) | p999 Lat (ms) | p50 Tp (Mbps) | p95 Tp (Mbps) | p99 Tp (Mbps) | p999 Tp (Mbps) | Avg CPU% | Peak CPU% | Avg Mem | Peak Mem | Err% |"
            )?;
            writeln!(
                f,
                "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
            )?;

            for r in &case_results {
                writeln!(
                    f,
                    "| {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.1} | {:.1} | {} | {} | {:.2} |",
                    r.case.chunk_size_mib,
                    r.total_requests,
                    r.latency.p50_ms,
                    r.latency.p95_ms,
                    r.latency.p99_ms,
                    r.latency.p999_ms,
                    r.throughput.p50_mbps,
                    r.throughput.p95_mbps,
                    r.throughput.p99_mbps,
                    r.throughput.p999_mbps,
                    r.resources.avg_cpu_percent,
                    r.resources.peak_cpu_percent,
                    format_bytes(r.resources.avg_mem_bytes),
                    format_bytes(r.resources.peak_mem_bytes),
                    r.resources.error_rate_percent,
                )?;
            }
            writeln!(f)?;
        }
    }

    // Best chunk size summary
    writeln!(f, "---\n")?;
    writeln!(f, "## Best Chunk Size Summary\n")?;

    for op in &[Operation::Upload, Operation::Download] {
        writeln!(f, "### {}\n", op)?;
        writeln!(f, "| File Size (MiB) | Best Chunk (MiB) | p50 Throughput (Mbps) |")?;
        writeln!(f, "|---:|---:|---:|")?;

        for &file_size in crate::config::FILE_SIZES_MIB {
            let best = results
                .iter()
                .filter(|r| r.case.operation == *op && r.case.file_size_mib == file_size)
                .max_by(|a, b| {
                    a.throughput
                        .p50_mbps
                        .partial_cmp(&b.throughput.p50_mbps)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

            if let Some(best) = best {
                writeln!(
                    f,
                    "| {} | {} | {:.2} |",
                    file_size, best.case.chunk_size_mib, best.throughput.p50_mbps
                )?;
            }
        }
        writeln!(f)?;
    }

    println!("Markdown report written to: {}", md_path.display());
    Ok(())
}

pub fn write_per_request_csv(
    records: &[(String, u64, u64, f64, f64, bool)], // (operation, file_mib, chunk_mib, latency_ms, throughput_mbps, success)
    output_dir: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(output_dir)?;
    let path = output_dir.join("per_request_results.csv");
    let mut wtr = csv::Writer::from_path(&path)?;

    wtr.write_record(&[
        "Operation",
        "File Size (MiB)",
        "Chunk Size (MiB)",
        "Latency (ms)",
        "Throughput (Mbps)",
        "Success",
    ])?;

    for (op, file_mib, chunk_mib, lat, tp, success) in records {
        wtr.write_record(&[
            op.clone(),
            file_mib.to_string(),
            chunk_mib.to_string(),
            format!("{:.3}", lat),
            format!("{:.3}", tp),
            success.to_string(),
        ])?;
    }
    wtr.flush()?;
    println!("Per-request CSV written to: {}", path.display());
    Ok(())
}
