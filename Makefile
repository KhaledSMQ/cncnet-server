.PHONY: all build release test bench clean run docker help

# Default target
all: build

# Build in debug mode
build:
	@echo "Building debug version..."
	@cargo build

# Build optimized release version
release:
	@echo "Building release version with optimizations..."
	@RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C lto=fat" cargo build --release
	@strip target/release/cncnet-server
	@echo "Release build complete: target/release/cncnet-server"

# Run tests
test:
	@echo "Running tests..."
	@cargo test --all

# Run benchmarks
bench:
	@echo "Running benchmarks..."
	@cargo bench

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	@cargo clean

# Run server in debug mode
run:
	@echo "Running server..."
	@RUST_LOG=debug cargo run -- --name "Debug Server"

# Run server in release mode
run-release: release
	@echo "Running release server..."
	@RUST_LOG=info ./target/release/cncnet-server --name "Production Server"

# Build Docker image
docker:
	@echo "Building Docker image..."
	@docker build -t cncnet-server:latest .

# Run with Docker
docker-run: docker
	@echo "Running with Docker..."
	@docker run -it --rm \
		--network host \
		-e RUST_LOG=info \
		cncnet-server:latest \
		--name "Docker Server"

# Install systemd service (requires sudo)
install: release
	@echo "Installing systemd service..."
	@sudo useradd -r -s /bin/false cncnet || true
	@sudo mkdir -p /opt/cncnet-server /var/log/cncnet
	@sudo cp target/release/cncnet-server /opt/cncnet-server/
	@sudo chown -R cncnet:cncnet /opt/cncnet-server /var/log/cncnet
	@sudo cp cncnet-server.service /etc/systemd/system/
	@sudo systemctl daemon-reload
	@echo "Installation complete. Start with: sudo systemctl start cncnet-server"

# Format code
fmt:
	@echo "Formatting code..."
	@cargo fmt

# Check code without building
check:
	@echo "Checking code..."
	@cargo check --all-targets

# Run clippy linter
clippy:
	@echo "Running clippy..."
	@cargo clippy -- -D warnings

# Run example client
example: build
	@echo "Running example client..."
	@cargo run --example test_client

# Show help
help:
	@echo "CnCNet Server - Makefile targets:"
	@echo ""
	@echo "  make build       - Build debug version"
	@echo "  make release     - Build optimized release version"
	@echo "  make test        - Run all tests"
	@echo "  make bench       - Run benchmarks"
	@echo "  make clean       - Clean build artifacts"
	@echo "  make run         - Run server in debug mode"
	@echo "  make run-release - Run server in release mode"
	@echo "  make docker      - Build Docker image"
	@echo "  make docker-run  - Run with Docker"
	@echo "  make install     - Install as systemd service (requires sudo)"
	@echo "  make fmt         - Format code"
	@echo "  make check       - Check code without building"
	@echo "  make clippy      - Run clippy linter"
	@echo "  make example     - Run example client"
	@echo "  make help        - Show this help message"