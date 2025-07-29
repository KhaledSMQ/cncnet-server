# CnCNet Tunnel Server - Software Architecture

## 1. Overall System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           CnCNet Server v3.0.0                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                              main.rs                                        │
│                         ServerManager                                       │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐  │
│  │   Configuration │  │ Signal Handlers │  │     Metrics Collector       │  │
│  │     Parser      │  │  (SIGTERM/INT)  │  │   (60s intervals)           │  │
│  └─────────────────┘  └─────────────────┘  └─────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────────────────┤
│                          Core Server Components                             │
│                                                                             │
│  ┌──────────────────┐  ┌──────────────────┐  ┌─────────────────────────┐    │
│  │   TunnelV3       │  │    TunnelV2      │  │      P2P Servers        │    │
│  │  (port 50001)    │  │  (port 50000)    │  │   (ports 8054, 3478)    │    │
│  │                  │  │                  │  │                         │    │
│  │ • 32-bit IDs     │  │ • 16-bit IDs     │  │ • STUN RFC 5389         │    │
│  │ • Worker threads │  │ • HTTP endpoints │  │ • NAT traversal         │    │
│  │ • Buffer pools   │  │ • Legacy compat  │  │ • Rate limiting         │    │
│  │ • 1M+ clients    │  │ • JSON responses │  │ • Ping responses        │    │
│  └──────────────────┘  └──────────────────┘  └─────────────────────────┘    │
├─────────────────────────────────────────────────────────────────────────────┤
│                           Shared Infrastructure                             │
│                                                                             │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐  │
│  │  ServerStats    │  │   BufferPool    │  │       RateLimiter           │  │
│  │                 │  │                 │  │                             │  │
│  │ • Atomic ctrs   │  │ • Zero-alloc    │  │ • Token bucket              │  │
│  │ • Thread-safe   │  │ • Reusable bufs │  │ • Per-IP limits             │  │
│  │ • Performance   │  │ • Memory opt    │  │ • Automatic cleanup         │  │
│  └─────────────────┘  └─────────────────┘  └─────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

## 2. TunnelV3 High-Performance Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              TunnelV3 Server                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                          Packet Processing Pipeline                         │
│                                                                             │
│  ┌─────────────────┐                                                        │
│  │   UDP Socket    │ ◄─── Game Clients (Red Alert 2 / Yuri's Revenge)       │
│  │  (port 50001)   │                                                        │
│  └─────────┬───────┘                                                        │
│            │                                                                │
│            ▼                                                                │
│  ┌─────────────────┐      ┌──────────────────────────────────────────────┐  │
│  │ Receiver Task   │      │              Buffer Pool                     │  │
│  │                 │◄────►│                                              │  │
│  │ • recv_from()   │      │ ┌──────┐ ┌──────┐ ┌──────┐ ┌──────┐ ┌──────┐ │  │
│  │ • Buffer reuse  │      │ │ Buf1 │ │ Buf2 │ │ Buf3 │ │ ...  │ │ BufN │ │  │
│  │ • Zero-copy     │      │ └──────┘ └──────┘ └──────┘ └──────┘ └──────┘ │  │
│  └─────────┬───────┘      └──────────────────────────────────────────────┘  │
│            │                                                                │
│            ▼                                                                │
│  ┌─────────────────┐                                                        │
│  │ Message Queue   │      Capacity: 100,000 messages                        │
│  │ (flume channel) │                                                        │
│  └─────────┬───────┘                                                        │
│            │                                                                │
│            ▼                                                                │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                    Worker Thread Pool                               │    │
│  │                                                                     │    │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌───────────┐   │    │
│  │  │  Worker 1   │  │  Worker 2   │  │  Worker 3   │  │  Worker N │   │    │
│  │  │             │  │             │  │             │  │           │   │    │
│  │  │ • Parse     │  │ • Parse     │  │ • Parse     │  │ • Parse   │   │    │
│  │  │ • Validate  │  │ • Validate  │  │ • Validate  │  │ • Validate│   │    │
│  │  │ • Forward   │  │ • Forward   │  │ • Forward   │  │ • Forward │   │    │
│  │  │ • Stats     │  │ • Stats     │  │ • Stats     │  │ • Stats   │   │    │
│  │  └─────────────┘  └─────────────┘  └─────────────┘  └───────────┘   │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                   │                                         │
│                                   ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                    Client Management                                │    │
│  │                                                                     │    │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────┐  │    │
│  │  │   DashMap       │  │   IP Tracking   │  │   Rate Limiting     │  │    │
│  │  │                 │  │                 │  │                     │  │    │
│  │  │ • 32-bit IDs    │  │ • Per-IP limits │  │ • Ping protection   │  │    │
│  │  │ • 1M+ capacity  │  │ • Atomic ctrs   │  │ • Token buckets     │  │    │
│  │  │ • Lock-free     │  │ • Connection    │  │ • Auto cleanup      │  │    │
│  │  │ • Concurrent    │  │   tracking      │  │ • Memory efficient  │  │    │
│  │  └─────────────────┘  └─────────────────┘  └─────────────────────┘  │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

## 3. TunnelV2 Legacy Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              TunnelV2 Server                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                        Dual Protocol Handler                                │
│                                                                             │
│  ┌─────────────────────────────────────────┐                                │
│  │              UDP Handler                │                                │
│  │           (Game Traffic)                │                                │
│  │                                         │                                │
│  │  ┌─────────────────┐                    │    ┌─────────────────────────┐ │
│  │  │   UDP Socket    │◄─── Game Packets ──┼───►│    Packet Processor     │ │
│  │  │  (port 50000)   │                    │    │                         │ │
│  │  └─────────────────┘                    │    │ • Parse 16-bit IDs      │ │
│  │                                         │    │ • Handle pings          │ │
│  │  ┌─────────────────┐                    │    │ • Forward packets       │ │
│  │  │  Client Store   │                    │    │ • Update activity       │ │
│  │  │                 │                    │    └─────────────────────────┘ │
│  │  │ • 16-bit IDs    │                    │                                │
│  │  │ • DashMap       │                    │                                │
│  │  │ • Activity      │                    │                                │
│  │  │   tracking      │                    │                                │
│  │  └─────────────────┘                    │                                │
│  └─────────────────────────────────────────┘                                │
│                                                                             │
│  ┌─────────────────────────────────────────┐                                │
│  │              HTTP Handler               │                                │
│  │         (Client Allocation)             │                                │
│  │                                         │                                │
│  │  ┌─────────────────┐                    │    ┌─────────────────────────┐ │
│  │  │   TCP Socket    │◄─── HTTP Requests ─┼───►│   Request Router        │ │
│  │  │  (port 50000)   │                    │    │                         │ │
│  │  └─────────────────┘                    │    │ GET /status             │ │
│  │                                         │    │ GET /request?clients=N  │ │
│  │  ┌─────────────────┐                    │    │ GET /health             │ │
│  │  │   Endpoints     │                    │    │ GET /maintenance/{pwd}  │ │
│  │  │                 │                    │    └─────────────────────────┘ │
│  │  │ • JSON responses│                    │                                │
│  │  │ • Rate limiting │                    │    ┌─────────────────────────┐ │
│  │  │ • Auth checking │                    │    │    ID Generator         │ │
│  │  │ • Error handling│                    │    │                         │ │
│  │  └─────────────────┘                    │    │ • Collision avoidance   │ │
│  └─────────────────────────────────────────┘    │ • Range: i16::MIN..MAX  │ │
│                                                 │ • Skip zero             │ │
│                                                 └─────────────────────────┘ │
├─────────────────────────────────────────────────────────────────────────────┤
│                         Background Tasks                                    │
│                                                                             │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐  │
│  │ Client Cleanup  │  │ Master Heartbeat│  │    Rate Limiter Cleanup     │  │
│  │                 │  │                 │  │                             │  │
│  │ • Remove expired│  │ • 60s intervals │  │ • Remove inactive limiters  │  │
│  │ • Update stats  │  │ • Server status │  │ • Memory pressure relief    │  │
│  │ • 30s intervals │  │ • Maintenance   │  │ • 5min intervals            │  │
│  └─────────────────┘  └─────────────────┘  └─────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

## 4. P2P STUN Server Architecture

```
┌────────────────────────────────────────────────────────────────────────────────┐
│                              P2P STUN Servers                                  │
├────────────────────────────────────────────────────────────────────────────────┤
│                          Dual Port Configuration                               │
│                                                                                │
│  ┌──────────────────────────────────────────┐  ┌─────────────────────────────┐ │
│  │            Port 8054                     │  │         Port 3478           │ │
│  │         (Primary STUN)                   │  │      (Standard STUN)        │ │
│  │                                          │  │                             │ │
│  │  ┌─────────────────┐                     │  │      ┌─────────────────┐    │ │
│  │  │   UDP Socket    │                     │  │      │   UDP Socket    │    │ │
│  │  │                 │                     │  │      │                 │    │ │
│  │  │ • Recv STUN     │                     │  │      │ • Recv STUN     │    │ │
│  │  │   requests      │                     │  │      │   requests      │    │ │
│  │  │ • Rate limited  │                     │  │      │ • Rate limited  │    │ │
│  │  └─────────────────┘                     │  │      └─────────────────┘    │ │
│  │            │                             │  │                │            │ │
│  │            ▼                             │  │                ▼            │ │
│  │  ┌─────────────────────────────────────┐ │  │ ┌─────────────────────────┐ │ │
│  │  │        STUN Processor               │ │  │ │     STUN Processor      │ │ │
│  │  │                                     │ │  │ │                         │ │ │
│  │  │ 1. Validate magic cookie            │ │  │ │ 1. Validate magic       │ │ │
│  │  │    (0x2112A442)                     │ │  │ │    cookie (0x2112A442)  │ │ │
│  │  │                                     │ │  │ │                         │ │ │
│  │  │ 2. Check message length             │ │  │ │ 2. Check message length │ │ │
│  │  │                                     │ │  │ │                         │ │ │
│  │  │ 3. Verify packet format             │ │  │ │ 3. Verify packet format │ │ │
│  │  │                                     │ │  │ │                         │ │ │
│  │  │ 4. Apply rate limiting              │ │  │ │ 4. Apply rate limiting  │ │ │
│  │  │                                     │ │  │ │                         │ │ │
│  │  │ 5. Generate XOR-MAPPED-ADDRESS      │ │  │ │ 5. Generate response    │ │ │
│  │  └─────────────────────────────────────┘ │  │ └─────────────────────────┘ │ │
│  └──────────────────────────────────────────┘  └─────────────────────────────┘ │
│                            │                                    │              │
│                            ▼                                    ▼              │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                         Rate Limiting System                            │   │
│  │                                                                         │   │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐  │   │
│  │  │   Per-IP Limits │  │  Global Limits  │  │    Cleanup Process      │  │   │
│  │  │                 │  │                 │  │                         │  │   │
│  │  │ • 20 req/min    │  │ • 100k total    │  │ • 5min intervals        │  │   │
│  │  │ • Token bucket  │  │ • Memory limit  │  │ • Remove inactive       │  │   │
│  │  │ • Auto refresh  │  │ • DashMap store │  │ • Prevent memory leaks  │  │   │
│  │  └─────────────────┘  └─────────────────┘  └─────────────────────────┘  │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
├────────────────────────────────────────────────────────────────────────────────┤
│                           STUN Protocol Flow                                   │
│                                                                                │
│   Client                    Server                    Response                 │
│     │                         │                         │                      │
│     │  STUN Binding Request   │                         │                      │
│     │ ──────────────────────► │                         │                      │
│     │                         │                         │                      │
│     │                         │ 1. Validate packet      │                      │
│     │                         │ 2. Check rate limit     │                      │
│     │                         │ 3. Extract client addr  │                      │
│     │                         │ 4. XOR with magic       │                      │
│     │                         │ 5. Build response       │                      │
│     │                         │                         │                      │
│     │  STUN Binding Response  │                         │                      │
│     │ ◄────────────────────── │                         │                      │
│     │                         │                         │                      │
│   ┌─┴──────────────────────────────────────────────────────────────────────┐   │
│   │ Response contains XOR-MAPPED-ADDRESS with client's public IP:port      │   │
│   │ This allows client to discover their NAT mapping for P2P connections   │   │
│   └────────────────────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────────────────┘
```

## 5. Metrics and Monitoring System

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                          Metrics Collection System                           │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                         Metrics Collector                               │ │
│  │                                                                         │ │
│  │  ┌─────────────────┐     60s     ┌─────────────────────────────────────┐│ │
│  │  │ Collection Task │ ──interval─►│         Report Generator            ││ │
│  │  │                 │             │                                     ││ │
│  │  │ • Atomic reads  │             │ • Calculate rates                   ││ │
│  │  │ • Rate calc     │             │ • Format messages                   ││ │
│  │  │ • Health check  │             │ • Log warnings                      ││ │
│  │  └─────────────────┘             └─────────────────────────────────────┘│ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│                                           │                                  │
│                                           ▼                                  │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                          ServerStats (Atomic)                           │ │
│  │                                                                         │ │
│  │  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐  ┌───────────┐ │ │
│  │  │ total_packets │  │forwarded_pkts │  │ ping_packets  │  │ dropped   │ │ │
│  │  │   (AtomicU64) │  │  (AtomicU64)  │  │  (AtomicU64)  │  │(AtomicU64)│ │ │
│  │  └───────────────┘  └───────────────┘  └───────────────┘  └───────────┘ │ │
│  │                                                                         │ │
│  │  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐  ┌───────────┐ │ │
│  │  │current_clients│  │ bytes_received│  │  bytes_sent   │  │conn_errors│ │ │
│  │  │   (AtomicU32) │  │  (AtomicU64)  │  │  (AtomicU64)  │  │(AtomicU64)│ │ │
│  │  └───────────────┘  └───────────────┘  └───────────────┘  └───────────┘ │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│                                           │                                  │
│                                           ▼                                  │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                      Performance Monitoring                             │ │
│  │                                                                         │ │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐  │ │
│  │  │ Packet Analysis │  │Capacity Monitor │  │   Health Scoring        │  │ │
│  │  │                 │  │                 │  │                         │  │ │
│  │  │ • Drop rate     │  │ • Client count  │  │ • 0-100 scale           │  │ │
│  │  │ • Throughput    │  │ • Memory usage  │  │ • Drop rate impact      │  │ │
│  │  │ • Latency est   │  │ • Connection    │  │ • Error rate impact     │  │ │
│  │  │ • Error rates   │  │   limits        │  │ • Capacity utilization  │  │ │
│  │  └─────────────────┘  └─────────────────┘  └─────────────────────────┘  │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│                                           │                                  │
│                                           ▼                                  │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                         Alert System                                    │ │
│  │                                                                         │ │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐  │ │
│  │  │ Critical Alerts │  │ Warning Alerts  │  │    Info Messages        │  │ │
│  │  │                 │  │                 │  │                         │  │ │
│  │  │ • Drop rate >5% │  │ • Drop rate >1% │  │ • Regular metrics       │  │ │
│  │  │ • Capacity >90% │  │ • Capacity >80% │  │ • Performance stats     │  │ │
│  │  │ • Memory >1GB   │  │ • High error    │  │ • Uptime reporting      │  │ │
│  │  │ • CPU overload  │  │   count         │  │ • Bandwidth usage       │  │ │
│  │  └─────────────────┘  └─────────────────┘  └─────────────────────────┘  │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

## 6. Data Flow Architecture

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                               Data Flow Overview                             │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   ┌───────────────────────────────────────────────────────────────────────┐  │
│   │                    Game Client Connections                            │  │
│   │                                                                       │  │
│   │ ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌────────────┐  │  │
│   │ │   Player A  │   │   Player B  │   │   Player C  │   │   Player D │  │  │
│   │ │ Red Alert 2 │   │ Red Alert 2 │   │   Yuri's    │   │   Yuri's   │  │  │
│   │ │             │   │             │   │   Revenge   │   │   Revenge  │  │  │
│   │ └──────┬──────┘   └──────┬──────┘   └──────┬──────┘   └──────┬─────┘  │  │
│   └────────┼─────────────────┼─────────────────┼─────────────────┼────────┘  │
│            │                 │                 │                 │           │
│            ▼                 ▼                 ▼                 ▼           │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                          Server Infrastructure                          │ │
│  │                                                                         │ │
│  │  TunnelV3 (50001)      TunnelV2 (50000)           P2P (8054/3478)       │ │
│  │  ┌───────────────┐     ┌───────────────┐       ┌─────────────────┐      │ │
│  │  │ UDP Packets   │     │ HTTP Requests │       │ STUN Requests   │      │ │
│  │  │ Game Traffic  │     │ Client Alloc  │       │ NAT Discovery   │      │ │
│  │  │ 32-bit IDs    │     │ 16-bit IDs    │       │ P2P Setup       │      │ │
│  │  └───────┬───────┘     └───────┬───────┘       └─────────┬───────┘      │ │
│  └──────────┼─────────────────────┼─────────────────────────┼──────────────┘ │
│             │                     │                         │                │
│             ▼                     ▼                         ▼                │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                      Packet Processing Pipeline                         │ │
│  │                                                                         │ │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐  │ │
│  │  │   Validation    │  │   Rate Limiting │  │     Client Lookup       │  │ │
│  │  │                 │  │                 │  │                         │  │ │
│  │  │ • Endpoint      │  │ • Per-IP limits │  │ • ID resolution         │  │ │
│  │  │   filtering     │  │ • Token buckets │  │ • Activity tracking     │  │ │
│  │  │ • Packet format │  │ • DDoS protect  │  │ • Connection state      │  │ │
│  │  │ • Size checks   │  │ • Auto cleanup  │  │ • Timeout handling      │  │ │
│  │  └─────────┬───────┘  └─────────┬───────┘  └─────────┬───────────────┘  │ │
│  │            │                    │                    │                  │ │
│  │            └────────────────────┼────────────────────┘                  │ │
│  │                                 ▼                                       │ │
│  │  ┌─────────────────────────────────────────────────────────────────────┐│ │
│  │  │                     Packet Forwarding Engine                        ││ │
│  │  │                                                                     ││ │
│  │  │  ┌─────────────┐    ┌─────────────┐    ┌─────────────────────────┐  ││ │
│  │  │  │ Sender      │───►│ Receiver    │───►│      Statistics         │  ││ │
│  │  │  │ Resolution  │    │ Resolution  │    │      Tracking           │  ││ │
│  │  │  │             │    │             │    │                         │  ││ │
│  │  │  │ • Find src  │    │ • Find dest │    │ • Increment counters    │  ││ │
│  │  │  │ • Validate  │    │ • Check     │    │ • Update bandwidth      │  ││ │
│  │  │  │ • Update    │    │   active    │    │ • Record errors         │  ││ │
│  │  │  │   activity  │    │ • Forward   │    │ • Performance metrics   │  ││ │
│  │  │  └─────────────┘    └─────────────┘    └─────────────────────────┘  ││ │
│  │  └─────────────────────────────────────────────────────────────────────┘│ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│                                     │                                        │
│                                     ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │                        Output & Monitoring                              │ │
│  │                                                                         │ │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐  │ │
│  │  │ Client Traffic  │  │ Master Server   │  │    Metrics & Alerts     │  │ │
│  │  │                 │  │                 │  │                         │  │ │
│  │  │ • Forwarded     │  │ • Heartbeats    │  │ • Performance logs      │  │ │
│  │  │   packets       │  │ • Status        │  │ • Health monitoring     │  │ │
│  │  │ • Ping          │  │   updates       │  │ • Alert generation      │  │ │
│  │  │   responses     │  │ • Server        │  │ • Capacity tracking     │  │ │
│  │  │ • Error         │  │   discovery     │  │ • Bandwidth reporting   │  │ │
│  │  │   messages      │  │ • Load balance  │  │ • System optimization   │  │ │
│  │  └─────────────────┘  └─────────────────┘  └─────────────────────────┘  │ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

These diagrams illustrate the complete architecture of the CnCNet High-Performance Tunnel Server, showing how the
different components interact, process data, and maintain high performance while handling massive scale operations.