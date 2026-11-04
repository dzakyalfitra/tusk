# CLAUDE.md

This file provides guidance to Claude Code and AI coding assistants working in the `tusk` repository.

## What This Is

`tusk` is an ultra-lightweight, high-performance terminal activity monitor for Linux (and WSL2) written in pure Rust. It tracks real-time multi-core CPU utilization, RAM/Swap consumption, network I/O throughput, and top active processes.

## Build & Run Commands

Run all commands from the repository root:

```bash
cargo check                # Fast compile-time verification without binary output
cargo run                  # Run in development mode
cargo build --release      # Build optimized release binary (target/release/tusk)
cargo test                 # Run tests (if test modules are added)
cargo clippy               # Lint code for idiomatic Rust patterns
cargo fmt                  # Format code according to Rust standard style
```

## Architecture & Code Structure

The entire tool is intentionally self-contained inside `src/main.rs` to maintain a single static binary with zero runtime bloat:

- **Section 1: Terminal RAII & Raw Mode (`RawTerminalGuard`)**
  Uses `libc::termios` to disable canonical line-buffering and local echo. Uses alternate screen buffer (`\x1b[?1049h`) and hides cursor (`\x1b[?25l`).
  *Safety rule:* Terminal state MUST be restored on `Drop`. Never bypass this guard.

- **Section 2: Metrics Data Structures**
  Defines `CpuRawSnapshot`, `MemInfo`, `NetSnapshot`, and `ProcessInfo`.

- **Section 3: System Metrics Engine (`SystemMonitor`)**
  Directly parses Linux virtual `/proc` filesystem:
  - `/proc/stat` -> Total & per-core CPU jiffies.
  - `/proc/meminfo` -> Physical RAM, Free, Buffers, Cache, Swap.
  - `/proc/net/dev` -> Interface RX/TX byte counters.
  - `/proc/loadavg` & `/proc/uptime` -> System load and uptime.
  - `/proc/[pid]/stat` & `/proc/[pid]/statm` -> Process CPU ticks and RSS memory.

- **Section 4: Terminal UI Rendering (`render_dashboard`)**
  Draws flicker-free UI into an in-memory `String` buffer and flushes in a single write operation (`\x1b[H`).

- **Section 5: Event Loop & Non-Blocking Input**
  Reads single bytes from `io::stdin()` without blocking, supporting hotkeys:
  - `c` (sort CPU), `m` (sort RAM), `p` (sort PID), `Space` (pause), `+`/`-` (rate), `q`/`Esc` (exit).

## Engineering Invariants & Constraints

1. **Zero External Daemons or Subprocesses:** NEVER shell out to `ps`, `top`, `free`, or `grep`. All metrics MUST be read directly from `/proc/` to maintain sub-millisecond efficiency.
2. **Minimal Dependencies:** The project intentionally relies only on `libc` for raw terminal I/O. Do NOT introduce heavy crates (like heavy async engines or bloated TUI frameworks) unless explicitly asked.
3. **No Terminal Clutter on Exit:** The terminal state MUST be restored cleanly whenever the process terminates or panics.
