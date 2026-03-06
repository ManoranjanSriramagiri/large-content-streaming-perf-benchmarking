use aes_gcm::aead::KeyInit;
use aes_gcm::{AeadInPlace, Aes256Gcm, Nonce};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures::StreamExt;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use sha2::{Digest, Sha512};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;

use clap::Parser;

const AES_BLOCK_SIZE: usize = 64 * 1024;
const READ_AHEAD_CHUNKS: usize = 4;
const WRITE_BACK_CHUNKS: usize = 4;
const DEFAULT_RNG_THREADS: usize = 2;
const DEFAULT_CRYPTO_THREADS: usize = 4;

#[derive(Parser)]
#[command(name = "streaming-bench-server")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    #[arg(long, default_value_t = DEFAULT_RNG_THREADS)]
    rng_threads: usize,
    #[arg(long, default_value_t = DEFAULT_CRYPTO_THREADS)]
    crypto_threads: usize,
}

/// Shared state: dedicated thread pools
struct AppState {
    rng_pool: rayon::ThreadPool,
    crypto_pool: rayon::ThreadPool,
}

#[derive(serde::Deserialize)]
struct UploadParams {
    chunk_size_mib: u64,
}

#[derive(serde::Deserialize)]
struct DownloadParams {
    file_size_mib: u64,
    chunk_size_mib: u64,
}

#[derive(serde::Serialize)]
struct UploadResponse {
    bytes_received: u64,
    sha512_hex: String,
    chunks_processed: u64,
}

/// Completion record from crypto worker
struct CryptoResult {
    bytes_processed: usize,
}

/// Upload: stream body -> read-ahead queue -> crypto worker -> write-back queue -> aggregator
async fn handle_upload(
    State(state): State<Arc<AppState>>,
    Query(params): Query<UploadParams>,
    body: Body,
) -> Result<impl IntoResponse, StatusCode> {
    let chunk_size = (params.chunk_size_mib * 1024 * 1024) as usize;

    // Read-ahead queue: async body reader -> crypto worker
    let (read_tx, read_rx) = tokio::sync::mpsc::channel::<Bytes>(READ_AHEAD_CHUNKS);
    // Write-back queue: crypto worker -> aggregator
    let (wb_tx, mut wb_rx) = tokio::sync::mpsc::channel::<CryptoResult>(WRITE_BACK_CHUNKS);

    // --- Reader task: body stream -> read-ahead queue ---
    let reader = tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    if read_tx.send(chunk).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // --- Crypto worker: read-ahead -> process -> write-back ---
    let state_ref = state.clone();
    let crypto_worker = tokio::spawn(async move {
        let crypto_pool = &state_ref.crypto_pool;
        let mut hasher = Sha512::new();
        let mut rng = StdRng::from_entropy();
        let mut key_bytes = [0u8; 32];
        rng.fill_bytes(&mut key_bytes);
        let cipher = Aes256Gcm::new_from_slice(&key_bytes).unwrap();

        let mut buffer = Vec::with_capacity(chunk_size);
        let mut chunks_processed: u64 = 0;
        let mut read_rx = read_rx;

        // Reusable encrypt buffer to avoid per-block allocation
        let mut encrypt_buf = vec![0u8; AES_BLOCK_SIZE + 16]; // +16 for GCM tag

        while let Some(data) = read_rx.recv().await {
            buffer.extend_from_slice(&data);

            while buffer.len() >= chunk_size {
                // Split without drain+collect: split_off keeps first chunk_size bytes
                let remainder = buffer.split_off(chunk_size);
                let processing_chunk = std::mem::replace(&mut buffer, remainder);

                let chunk_len = processing_chunk.len();

                // SHA-512
                hasher.update(&processing_chunk);

                // AES-GCM in sub-blocks on crypto pool
                let cipher_ref = cipher.clone();
                let chunk_id = chunks_processed;
                let mut eb = std::mem::take(&mut encrypt_buf);
                let pc = processing_chunk;

                let eb_result = crypto_pool.install(|| {
                    encrypt_chunk_in_blocks(&cipher_ref, &pc, chunk_id, &mut eb);
                    eb
                });
                encrypt_buf = eb_result;

                let _ = wb_tx.send(CryptoResult { bytes_processed: chunk_len }).await;
                chunks_processed += 1;
            }
        }

        // Remaining bytes
        if !buffer.is_empty() {
            let chunk_len = buffer.len();
            hasher.update(&buffer);

            let cipher_ref = cipher.clone();
            let chunk_id = chunks_processed;
            let mut eb = encrypt_buf;

            crypto_pool.install(|| {
                encrypt_chunk_in_blocks(&cipher_ref, &buffer, chunk_id, &mut eb);
            });

            let _ = wb_tx.send(CryptoResult { bytes_processed: chunk_len }).await;
            chunks_processed += 1;
        }

        let hash = hasher.finalize();
        (hex::encode(hash), chunks_processed)
    });

    // --- Aggregator: consume write-back results ---
    let aggregator = tokio::spawn(async move {
        let mut total_bytes: u64 = 0;
        while let Some(result) = wb_rx.recv().await {
            total_bytes += result.bytes_processed as u64;
        }
        total_bytes
    });

    // Wait for all stages
    let _ = reader.await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (sha512_hex, chunks_processed) = crypto_worker.await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total_bytes = aggregator.await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(UploadResponse {
        bytes_received: total_bytes,
        sha512_hex,
        chunks_processed,
    }))
}

/// AES-GCM encrypt in sub-blocks using a reusable buffer
fn encrypt_chunk_in_blocks(cipher: &Aes256Gcm, data: &[u8], chunk_id: u64, buf: &mut Vec<u8>) {
    let mut offset = 0usize;
    let mut block_id = 0u32;
    while offset < data.len() {
        let end = std::cmp::min(offset + AES_BLOCK_SIZE, data.len());
        let block_len = end - offset;

        // Reuse buffer instead of to_vec()
        buf.clear();
        buf.extend_from_slice(&data[offset..end]);

        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..8].copy_from_slice(&chunk_id.to_le_bytes());
        nonce_bytes[8..12].copy_from_slice(&block_id.to_le_bytes());
        let nonce = Nonce::from_slice(&nonce_bytes);

        let _ = cipher.encrypt_in_place(nonce, b"", buf);

        offset += block_len;
        block_id += 1;
    }
}

/// Download: RNG pool generates bytes -> stream via bounded channel
async fn handle_download(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DownloadParams>,
) -> impl IntoResponse {
    let file_size = params.file_size_mib * 1024 * 1024;
    let chunk_size = params.chunk_size_mib * 1024 * 1024;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(READ_AHEAD_CHUNKS);

    // RNG work on dedicated pool
    state.rng_pool.spawn(move || {
        let mut remaining = file_size;
        let mut rng = StdRng::from_entropy();

        while remaining > 0 {
            let this_chunk = std::cmp::min(remaining, chunk_size) as usize;
            let mut buf = vec![0u8; this_chunk];
            rng.fill_bytes(&mut buf);
            remaining -= this_chunk as u64;

            if tx.blocking_send(Ok(Bytes::from(buf))).is_err() {
                break;
            }
        }
    });

    let stream = ReceiverStream::new(rx);
    Body::from_stream(stream)
}

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let rng_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.rng_threads)
        .thread_name(|i| format!("rng-{}", i))
        .build()
        .expect("Failed to build RNG thread pool");

    let crypto_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.crypto_threads)
        .thread_name(|i| format!("crypto-{}", i))
        .build()
        .expect("Failed to build crypto thread pool");

    println!("Thread pools: RNG={} threads, Crypto={} threads", args.rng_threads, args.crypto_threads);
    println!("Read-ahead: {} chunks, Write-back: {} chunks", READ_AHEAD_CHUNKS, WRITE_BACK_CHUNKS);

    let state = Arc::new(AppState { rng_pool, crypto_pool });

    let app = Router::new()
        .route("/upload", post(handle_upload))
        .route("/download", get(handle_download))
        .route("/health", get(health))
        .with_state(state);

    let addr: SocketAddr = args.addr.parse().expect("Invalid address");
    println!("Benchmark server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
