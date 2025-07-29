#!/bin/bash

# CnCNet Server build script
# This script builds the server with maximum optimizations

set -e

echo "Building CnCNet Server..."

# Set environment variables for maximum optimization
export RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C lto=fat -C embed-bitcode=yes"

# Clean previous builds
cargo clean

# Build in release mode with all optimizations
echo "Building release binary..."
cargo build --release

# Strip the binary to reduce size
echo "Stripping binary..."
strip target/release/cncnet-server

# Get binary size
SIZE=$(du -h target/release/cncnet-server | cut -f1)
echo "Build complete! Binary size: $SIZE"
echo "Binary location: ./target/release/cncnet-server"

# Run tests to ensure everything works
echo ""
echo "Running tests..."
cargo test --release

echo ""
echo "Build successful!"