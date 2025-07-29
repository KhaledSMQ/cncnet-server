# CnCNet Server Integration Tests

This directory contains comprehensive integration, load, and performance tests for the CnCNet server.

## Test Categories

### 1. Load Tests (`integration_load_test.rs`)
Tests the server's ability to handle high concurrent loads:
- **200 Concurrent Clients**: Simulates a typical game server load
- **500 Client Stress Test**: Pushes the server to its limits
- **Sustained High Throughput**: Tests performance over extended periods
- **Connection Churn**: Tests memory efficiency with rapid connect/disconnect cycles

### 2. Race Condition Tests (`race_condition_test.rs`)
Specifically targets potential race conditions in:
- **Client ID Allocation**: Concurrent connection attempts
- **Endpoint Updates**: IP address changes under load
- **Rate Limiter Token Bucket**: Atomic operation consistency
- **Connection State**: Consistency during concurrent operations
- **Cleanup Tasks**: Race conditions during timeout cleanup

### 3. Performance Benchmarks (`performance_benchmark.rs`)
Measures key performance metrics:
- **Throughput Scaling**: How performance scales with client count
- **Latency Distribution**: P50, P95, P99 latency percentiles
- **Packet Size Impact**: Performance with different packet sizes
- **Memory Efficiency**: Resource usage under varying loads

## Running Tests

### Basic Test Suite
```bash
# Run all tests (excluding extended tests)
cargo test

# Run with more threads for better load simulation
cargo test -- --test-threads=8

# Run with logging enabled
RUST_LOG=info cargo test -- --nocapture
```

### Specific Test Categories
```bash
# Run only load tests
cargo test --test integration_load_test

# Run only race condition tests
cargo test --test race_condition_test

# Run only performance benchmarks
cargo test --test performance_benchmark
```

### Extended Tests
Some tests are marked with `#[ignore]` due to their long runtime. Run these with:
```bash
# Run the 500-client stress test
cargo test stress_test_500_clients -- --ignored

# Run the 5-minute sustained load test
cargo test benchmark_sustained_load -- --ignored
```

## Test Configuration

### Environment Variables
- `RUST_LOG`: Set logging level (e.g., `RUST_LOG=debug`)
- `RUST_TEST_THREADS`: Number of test threads (default: number of CPUs)

### Test Parameters
Key parameters can be adjusted in the test files:
- `LOAD_TEST_CLIENTS`: Number of clients for standard load test (default: 200)
- `STRESS_TEST_CLIENTS`: Number of clients for stress test (default: 500)
- `LOAD_TEST_DURATION`: Duration of sustained load tests (default: 60 seconds)
- `MAX_PACKET_LOSS`: Acceptable packet loss percentage (default: 1.0%)
- `MAX_AVG_RTT_MS`: Maximum acceptable average RTT (default: 50ms)

## Performance Expectations

Based on the tests, the server should achieve:
- **Concurrent Clients**: 200+ simultaneous connections
- **Throughput**: 10+ Mbps sustained
- **Latency**: <50ms average RTT under load
- **Packet Loss**: <1% under normal load
- **Memory**: Linear scaling with active connections

## Debugging Failed Tests

### Common Issues

1. **Port Already in Use**
   ```bash
   # Check if port is in use
   lsof -i :50001
   ```

2. **Resource Limits**
   ```bash
   # Increase file descriptor limit
   ulimit -n 65536
   ```

3. **Timeout Errors**
    - Increase timeout values in test configuration
    - Check system load during tests

### Detailed Logging
Enable detailed logging for debugging:
```bash
RUST_LOG=cncnet_server=debug,test=debug cargo test -- --nocapture
```

## Adding New Tests

When adding new tests:
1. Use the common test utilities in `tests/common/mod.rs`
2. Follow the existing naming conventions
3. Add appropriate assertions for performance metrics
4. Document any special requirements or configurations
5. Consider adding both normal and stress variants

## Continuous Integration

These tests are designed to run in CI environments:
- Basic tests run on every commit
- Extended tests run nightly or on release branches
- Performance regression detection through metric tracking

## Safety and Reliability

The tests are designed to verify:
- **Memory Safety**: No leaks under connection churn
- **Concurrency Safety**: No data races under high load
- **Resource Limits**: Graceful handling of limits
- **Error Recovery**: Continued operation after errors
- **Performance Consistency**: Stable performance over time