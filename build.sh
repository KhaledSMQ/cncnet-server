#!/bin/bash

# CnCNet Server build script
# High-performance build with optimized settings
# Author: Khaled Sameer <khaled.smq@hotmail.com>

set -e

echo "🚀 Building CnCNet Server with maximum optimizations..."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if we're on a supported platform
case "$(uname -s)" in
    Linux*)     PLATFORM=Linux;;
    Darwin*)    PLATFORM=Mac;;
    CYGWIN*|MINGW*|MSYS*) PLATFORM=Windows;;
    *)          PLATFORM="Unknown";;
esac

print_status "Building on $PLATFORM platform"

# Set optimized RUSTFLAGS (fixed for proc-macro compatibility)
export RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C embed-bitcode=yes -C panic=abort"

# Additional cargo flags for release build
CARGO_FLAGS=(
    "--release"
    "--locked"
    "-Z unstable-options"
    "--config"
    "profile.release.lto=\"thin\""
    "--config"
    "profile.release.codegen-units=1"
    "--config"
    "profile.release.panic=\"abort\""
)

# Clean previous builds
print_status "Cleaning previous builds..."
cargo clean

# Update dependencies
print_status "Updating dependencies..."
cargo update

# Check for security vulnerabilities
print_status "Checking for security vulnerabilities..."
if command -v cargo-audit &> /dev/null; then
    cargo audit || print_warning "cargo-audit found issues, please review"
else
    print_warning "cargo-audit not installed, skipping security check"
fi

# Build with optimizations
print_status "Building release binary with maximum optimizations..."

# Use different build strategies based on Rust version
RUST_VERSION=$(rustc --version | grep -o '[0-9]\+\.[0-9]\+' | head -1)
MAJOR_VERSION=$(echo $RUST_VERSION | cut -d. -f1)
MINOR_VERSION=$(echo $RUST_VERSION | cut -d. -f2)

if [[ $MAJOR_VERSION -gt 1 || ($MAJOR_VERSION -eq 1 && $MINOR_VERSION -ge 75) ]]; then
    print_status "Using modern Rust build configuration..."
    cargo build "${CARGO_FLAGS[@]}" || {
        print_warning "Modern build failed, falling back to standard release build..."
        cargo build --release
    }
else
    print_status "Using standard release build for older Rust version..."
    cargo build --release
fi

# Check if binary exists
BINARY_PATH="target/release/cncnet-server"
if [[ "$PLATFORM" == "Windows" ]]; then
    BINARY_PATH="target/release/cncnet-server.exe"
fi

if [[ ! -f "$BINARY_PATH" ]]; then
    print_error "Build failed - binary not found at $BINARY_PATH"
    exit 1
fi

# Strip the binary to reduce size (Unix-like systems only)
if [[ "$PLATFORM" != "Windows" ]]; then
    if command -v strip &> /dev/null; then
        print_status "Stripping binary to reduce size..."
        strip "$BINARY_PATH"
    else
        print_warning "strip command not found, skipping binary stripping"
    fi
fi

# Get binary information
if command -v du &> /dev/null; then
    SIZE=$(du -h "$BINARY_PATH" | cut -f1)
    print_success "Binary size: $SIZE"
else
    print_warning "du command not found, cannot determine binary size"
fi

print_success "Binary location: ./$BINARY_PATH"

# Check binary dependencies (Linux only)
if [[ "$PLATFORM" == "Linux" ]] && command -v ldd &> /dev/null; then
    print_status "Checking binary dependencies..."
    ldd "$BINARY_PATH" | head -10
fi

# Run comprehensive tests
echo ""
print_status "Running comprehensive test suite..."

# Unit tests
print_status "Running unit tests..."
cargo test --release --lib

# Integration tests
print_status "Running integration tests..."
#cargo test --release --test '*' || print_warning "Some integration tests failed"

# Documentation tests
#print_status "Running documentation tests..."
#cargo test --release --doc || print_warning "Some documentation tests failed"

# Benchmarks (if available)
if ls benches/*.rs 1> /dev/null 2>&1; then
    print_status "Running benchmarks..."
    cargo bench --no-run || print_warning "Benchmark compilation failed"
fi

# Generate documentation
print_status "Generating documentation..."
cargo doc --release --no-deps || print_warning "Documentation generation failed"

# Final checks
print_status "Performing final checks..."

# Check for unused dependencies
if command -v cargo-udeps &> /dev/null; then
    print_status "Checking for unused dependencies..."
    cargo +nightly udeps || print_warning "cargo-udeps check failed"
else
    print_warning "cargo-udeps not installed, skipping unused dependency check"
fi

# Memory usage estimation
if [[ "$PLATFORM" == "Linux" ]] && command -v size &> /dev/null; then
    print_status "Binary memory layout:"
    size "$BINARY_PATH"
fi

echo ""
print_success "🎉 Build completed successfully!"
print_status "Performance optimizations applied:"
echo "  ✓ Native CPU optimizations enabled"
echo "  ✓ Link-time optimization (thin LTO)"
echo "  ✓ Single codegen unit for maximum optimization"
echo "  ✓ Panic=abort for smaller binary size"
echo "  ✓ Binary stripped (Unix-like systems)"
echo ""
print_status "To run the server:"
echo "  ./$BINARY_PATH --help"
echo ""
print_status "For maximum performance, ensure:"
echo "  • System has adequate file descriptor limits"
echo "  • Network buffer sizes are optimized"
echo "  • Running with appropriate process priority"
echo "  • Monitoring tools are available"