#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_DIR="results/${TIMESTAMP}"
LOG_DIR="${OUTPUT_DIR}/logs"
mkdir -p "$LOG_DIR"

echo "=== Building release binaries ==="
cargo build --release 2>&1 | tee "${LOG_DIR}/build.log"

echo ""
echo "=== Starting server ==="
cargo run --release --bin server -- --addr 127.0.0.1:8080 > "${LOG_DIR}/server.log" 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"
sleep 2

# Verify server is up
if ! curl -s http://127.0.0.1:8080/health > /dev/null; then
    echo "ERROR: Server failed to start"
    kill $SERVER_PID 2>/dev/null
    exit 1
fi
echo "Server health: OK"

cleanup() {
    echo ""
    echo "=== Stopping server (PID: $SERVER_PID) ==="
    kill $SERVER_PID 2>/dev/null
    wait $SERVER_PID 2>/dev/null
    echo "Done."
}
trap cleanup EXIT

echo ""
echo "=== Running benchmark ==="
echo "Output: ${OUTPUT_DIR}"
echo ""

# Pass through any extra args (e.g. --quick, --upload-only, --file-size 10)
cargo run --release --bin client -- \
    --server http://127.0.0.1:8080 \
    --output "${OUTPUT_DIR}" \
    "$@" \
    2>&1 | tee "${LOG_DIR}/client.log"

echo ""
echo "=== Results ==="
echo "Reports:    ${OUTPUT_DIR}/"
echo "Raw logs:   ${LOG_DIR}/"
echo "CSV:        ${OUTPUT_DIR}/benchmark_results.csv"
echo "JSON:       ${OUTPUT_DIR}/benchmark_results.json"
echo "Markdown:   ${OUTPUT_DIR}/benchmark_report.md"
echo "Per-request: ${OUTPUT_DIR}/per_request_results.csv"
echo "Server log: ${LOG_DIR}/server.log"
echo "Client log: ${LOG_DIR}/client.log"
