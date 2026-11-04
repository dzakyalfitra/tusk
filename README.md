# 📊 TermWatch 🦀

> A blazing-fast, lightweight Linux system and network monitor written in pure Rust.

Zero daemons. Zero bloat. Direct `/proc` parsing.

```text
 TERMWATCH 🦀  [LIVE] | Uptime: 0d 04h 12m | Load: 0.18 0.06 0.01 | Rate: 1000ms
─────────────────────────────────────────────────────────────────────────────────
 CPU Total:  12.4% [███░░░░░░░░░░░░░░░░░░░░░]  History:  ▂▃ ▂▅ 
   Core 00:  8.2% [█░░░░░░░░░░░]   Core 01: 14.5% [██░░░░░░░░░░]   
   Core 02: 18.0% [██░░░░░░░░░░]   Core 03:  9.1% [█░░░░░░░░░░░]   
─────────────────────────────────────────────────────────────────────────────────
 RAM Usage: 41.0% [████████░░░░░░░░░░░░░░░░]  3.18 GB / 7.75 GB (Cached: 0.89 GB)
 Swap Used:  0.0% [░░░░░░░░░░░░░░░░░░░░░░░░]  0.00 GB / 2.00 GB
─────────────────────────────────────────────────────────────────────────────────
 Network Traffic:
   eth0    ↓   12.4 KB/s (  22.7 MB total)   ↑    1.8 KB/s (   0.5 MB total)
─────────────────────────────────────────────────────────────────────────────────
 Top Processes: [Sorted by CPU%] (Showing top 10)
   PID     CPU%     MEM (MB)   STATE   COMMAND                  
   1245    14.2%       85.4     [S]     rustc                    
   3412     4.5%      120.1     [S]     node                     
   1        0.0%       15.9     [S]     systemd                  
─────────────────────────────────────────────────────────────────────────────────
 [c] Sort CPU  [m] Sort RAM  [p] Sort PID  [Space] Pause  [+]/[-] Rate  [q] Quit
```

---

## ⚡ Features

- **Direct Kernel Parsing:** Reads `/proc/stat`, `/proc/meminfo`, and `/proc/net/dev` with zero background service overhead.
- **Per-Core Visualization:** Real-time multi-core CPU usage meters and recent history sparkline.
- **Dynamic Memory Tracking:** Active RAM, Free, Buffers, Cache, and Swap consumption.
- **Live Network Bandwidth:** Upload/Download throughput per network interface in real-time.
- **Interactive Process Manager:** Sort top active processes by CPU% (`[c]`), RAM (`[m]`), or PID (`[p]`).
- **Flicker-Free Terminal UI:** Uses an alternate screen buffer (`\x1b[?1049h`) with non-blocking raw input. On exit, your terminal is restored cleanly with zero clutter.

---

## 🚀 Quick Start

### Build & Run
```bash
git clone https://github.com/<your-username>/termwatch.git
cd termwatch

# Run in debug mode
cargo run

# Build optimized release binary
cargo build --release
./target/release/termwatch
```

### Install to System
```bash
cargo install --path .
termwatch
```

---

## 🎮 Interactive Controls

| Key | Action |
|---|---|
| **`c`** | Sort processes by **CPU%** (Highest first) |
| **`m`** | Sort processes by **Memory (MB)** |
| **`p`** | Sort processes by **PID** |
| **`Space`** | **Pause / Resume** live data collection |
| **`+`** / **`=`** | Increase refresh rate (faster updates) |
| **`-`** / **`_`** | Decrease refresh rate (save CPU) |
| **`q`** or **`Esc`** | Exit cleanly and restore terminal |

---

## 📜 License

MIT License. Built with ❤️ in Rust.
