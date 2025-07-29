# Memory

Estimated RAM usage: ~128 MB

- Client storage: ~32 KB (250 * ~128 bytes)
- Buffer pools: ~32 MB (500 * 2KB buffers * 2 pools)
- Rate limiters: ~64 KB
- Overhead: ~96 MB

# Network

- Socket buffers: 64 KB (32KB * 2)
- File descriptors: ~1000
- Network throughput: ~10-50 Mbps peak