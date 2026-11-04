//! 🦣 TUSK: RAZOR-SHARP LINUX SYSTEM & NETWORK MONITOR 🦀
//!
//! A real-time, low-overhead Linux system activity monitor written in Rust.
//! Features:
//! - Direct `/proc` filesystem parsing (zero external daemons)
//! - Real-time CPU per-core utilization & historical sparkline graph
//! - RAM & Swap memory allocation gauges
//! - Live Network I/O throughput (Download / Upload speeds per interface)
//! - Top active processes table with interactive sorting (CPU% vs Memory)
//! - Flicker-free alternate screen buffer with raw keyboard input

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, Read, Write};

use std::thread;
use std::time::{Duration, Instant};

// =========================================================================
// SECTION 1: TERMINAL RAII & RAW MODE HANDLER
// =========================================================================

struct RawTerminalGuard {
    original_termios: libc::termios,
}

impl RawTerminalGuard {
    fn new() -> io::Result<Self> {
        unsafe {
            let mut original_termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut original_termios) != 0 {
                return Err(io::Error::last_os_error());
            }

            let mut raw = original_termios;
            // Disable canonical mode (line buffering) and local echo
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            // Non-blocking reads: return immediately if no key available
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;

            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }

            // Switch to Alternate Screen buffer and hide cursor
            print!("\x1b[?1049h\x1b[?25l");
            io::stdout().flush()?;

            Ok(Self { original_termios })
        }
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        unsafe {
            // Restore original terminal attributes
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original_termios);
            // Return from Alternate Screen buffer and show cursor
            print!("\x1b[?1049l\x1b[?25h\x1b[0m\n");
            let _ = io::stdout().flush();
        }
    }
}

// =========================================================================
// SECTION 2: SYSTEM METRICS DATA STRUCTURES
// =========================================================================

#[derive(Default, Clone, Copy)]
struct CpuRawSnapshot {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuRawSnapshot {
    fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    fn active(&self) -> u64 {
        self.total().saturating_sub(self.idle + self.iowait)
    }
}

#[derive(Default)]
struct MemInfo {
    total_kb: u64,
    free_kb: u64,
    available_kb: u64,
    buffers_kb: u64,
    cached_kb: u64,
    swap_total_kb: u64,
    swap_free_kb: u64,
}

impl MemInfo {
    fn used_kb(&self) -> u64 {
        self.total_kb.saturating_sub(self.available_kb)
    }

    fn swap_used_kb(&self) -> u64 {
        self.swap_total_kb.saturating_sub(self.swap_free_kb)
    }
}

#[derive(Default, Clone, Copy)]
struct NetSnapshot {
    rx_bytes: u64,
    tx_bytes: u64,
}

#[derive(Clone)]
struct ProcessInfo {
    pid: u32,
    comm: String,
    state: char,
    rss_kb: u64,
    cpu_percent: f64,
    _total_time_ticks: u64,
}

#[derive(PartialEq, Clone, Copy)]
enum SortOrder {
    Cpu,
    Memory,
    Pid,
}

// =========================================================================
// SECTION 3: SYSTEM METRICS ENGINE (DIRECT /proc PARSER)
// =========================================================================

struct SystemMonitor {
    prev_global_cpu: CpuRawSnapshot,
    prev_per_core_cpu: Vec<CpuRawSnapshot>,
    cpu_history: VecDeque<f64>,
    prev_net_devices: HashMap<String, NetSnapshot>,
    prev_proc_times: HashMap<u32, u64>,
    prev_tick_time: Instant,
    sort_order: SortOrder,
    refresh_rate_ms: u64,
    paused: bool,
}

impl SystemMonitor {
    fn new() -> Self {
        let mut monitor = Self {
            prev_global_cpu: CpuRawSnapshot::default(),
            prev_per_core_cpu: Vec::new(),
            cpu_history: VecDeque::with_capacity(40),
            prev_net_devices: HashMap::new(),
            prev_proc_times: HashMap::new(),
            prev_tick_time: Instant::now(),
            sort_order: SortOrder::Cpu,
            refresh_rate_ms: 1000,
            paused: false,
        };

        // Seed initial CPU snapshots
        if let Ok((global, cores)) = monitor.read_cpu_snapshots() {
            monitor.prev_global_cpu = global;
            monitor.prev_per_core_cpu = cores;
        }
        if let Ok(net) = monitor.read_net_snapshots() {
            monitor.prev_net_devices = net;
        }

        monitor
    }

    fn read_cpu_snapshots(&self) -> io::Result<(CpuRawSnapshot, Vec<CpuRawSnapshot>)> {
        let content = fs::read_to_string("/proc/stat")?;
        let mut global = CpuRawSnapshot::default();
        let mut cores = Vec::new();

        for line in content.lines() {
            if line.starts_with("cpu ") {
                global = parse_cpu_line(line);
            } else if line.starts_with("cpu") {
                cores.push(parse_cpu_line(line));
            }
        }

        Ok((global, cores))
    }

    fn read_mem_info(&self) -> io::Result<MemInfo> {
        let content = fs::read_to_string("/proc/meminfo")?;
        let mut mem = MemInfo::default();

        for line in content.lines() {
            let mut parts = line.split_whitespace();
            let key = parts.next().unwrap_or("");
            let val = parts.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

            match key {
                "MemTotal:" => mem.total_kb = val,
                "MemFree:" => mem.free_kb = val,
                "MemAvailable:" => mem.available_kb = val,
                "Buffers:" => mem.buffers_kb = val,
                "Cached:" => mem.cached_kb = val,
                "SwapTotal:" => mem.swap_total_kb = val,
                "SwapFree:" => mem.swap_free_kb = val,
                _ => {}
            }
        }

        Ok(mem)
    }

    fn read_net_snapshots(&self) -> io::Result<HashMap<String, NetSnapshot>> {
        let content = fs::read_to_string("/proc/net/dev")?;
        let mut map = HashMap::new();

        for line in content.lines().skip(2) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 10 {
                let iface = parts[0].trim_end_matches(':').to_string();
                let rx_bytes = parts[1].parse::<u64>().unwrap_or(0);
                let tx_bytes = parts[9].parse::<u64>().unwrap_or(0);

                map.insert(iface, NetSnapshot { rx_bytes, tx_bytes });
            }
        }

        Ok(map)
    }

    fn read_load_avg(&self) -> (f64, f64, f64) {
        fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| {
                let mut parts = s.split_whitespace();
                let l1 = parts.next()?.parse::<f64>().ok()?;
                let l5 = parts.next()?.parse::<f64>().ok()?;
                let l15 = parts.next()?.parse::<f64>().ok()?;
                Some((l1, l5, l15))
            })
            .unwrap_or((0.0, 0.0, 0.0))
    }

    fn read_uptime(&self) -> Duration {
        fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| {
                let secs = s.split_whitespace().next()?.parse::<f64>().ok()?;
                Some(Duration::from_secs_f64(secs))
            })
            .unwrap_or_default()
    }

    fn read_processes(&mut self, elapsed_secs: f64) -> Vec<ProcessInfo> {
        let mut procs = Vec::new();
        let page_size_kb = 4; // standard x86_64 4KB page

        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if let Ok(pid) = name.parse::<u32>() {
                        if let Ok(p_info) = self.parse_single_proc(pid, page_size_kb, elapsed_secs) {
                            procs.push(p_info);
                        }
                    }
                }
            }
        }

        // Sort processes
        match self.sort_order {
            SortOrder::Cpu => procs.sort_by(|a, b| b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap_or(std::cmp::Ordering::Equal)),
            SortOrder::Memory => procs.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb)),
            SortOrder::Pid => procs.sort_by(|a, b| a.pid.cmp(&b.pid)),
        }

        procs
    }

    fn parse_single_proc(&mut self, pid: u32, page_size_kb: u64, elapsed_secs: f64) -> io::Result<ProcessInfo> {
        let stat_path = format!("/proc/{}/stat", pid);
        let stat_content = fs::read_to_string(stat_path)?;

        let start_paren = stat_content.find('(').ok_or(io::ErrorKind::InvalidData)?;
        let end_paren = stat_content.rfind(')').ok_or(io::ErrorKind::InvalidData)?;
        let comm = stat_content[start_paren + 1..end_paren].to_string();

        let after_paren = &stat_content[end_paren + 2..];
        let fields: Vec<&str> = after_paren.split_whitespace().collect();
        if fields.len() < 13 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        let state = fields[0].chars().next().unwrap_or('?');
        let utime = fields[11].parse::<u64>().unwrap_or(0);
        let stime = fields[12].parse::<u64>().unwrap_or(0);
        let total_ticks = utime + stime;

        // Read RSS memory from /proc/[pid]/statm
        let statm_path = format!("/proc/{}/statm", pid);
        let rss_pages = fs::read_to_string(statm_path)
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .unwrap_or(0);
        let rss_kb = rss_pages * page_size_kb;

        // Calculate CPU %
        let prev_ticks = self.prev_proc_times.insert(pid, total_ticks).unwrap_or(total_ticks);
        let tick_diff = total_ticks.saturating_sub(prev_ticks);
        let cpu_percent = if elapsed_secs > 0.0 {
            ((tick_diff as f64 / 100.0) / elapsed_secs) * 100.0
        } else {
            0.0
        };

        Ok(ProcessInfo {
            pid,
            comm,
            state,
            rss_kb,
            cpu_percent,
            _total_time_ticks: total_ticks,
        })
    }
}

fn parse_cpu_line(line: &str) -> CpuRawSnapshot {
    let mut nums = line.split_whitespace().skip(1).filter_map(|s| s.parse::<u64>().ok());
    CpuRawSnapshot {
        user: nums.next().unwrap_or(0),
        nice: nums.next().unwrap_or(0),
        system: nums.next().unwrap_or(0),
        idle: nums.next().unwrap_or(0),
        iowait: nums.next().unwrap_or(0),
        irq: nums.next().unwrap_or(0),
        softirq: nums.next().unwrap_or(0),
        steal: nums.next().unwrap_or(0),
    }
}

fn calc_cpu_percent(prev: &CpuRawSnapshot, curr: &CpuRawSnapshot) -> f64 {
    let total_diff = curr.total().saturating_sub(prev.total()) as f64;
    let active_diff = curr.active().saturating_sub(prev.active()) as f64;
    if total_diff > 0.0 {
        ((active_diff / total_diff) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

// =========================================================================
// SECTION 4: TERMINAL UI RENDERING
// =========================================================================

fn render_dashboard(
    monitor: &mut SystemMonitor,
    global_cpu_pct: f64,
    core_cpu_pcts: &[f64],
    mem: &MemInfo,
    net_speeds: &HashMap<String, (f64, f64, u64, u64)>,
    processes: &[ProcessInfo],
) {
    let mut out = String::with_capacity(8192);

    // Reposition cursor to top-left
    out.push_str("\x1b[H");

    // 1. Header Bar
    let (l1, l5, l15) = monitor.read_load_avg();
    let uptime = monitor.read_uptime();
    let uptime_str = format!(
        "{}d {:02}h {:02}m",
        uptime.as_secs() / 86400,
        (uptime.as_secs() % 86400) / 3600,
        (uptime.as_secs() % 3600) / 60
    );

    let status_tag = if monitor.paused {
        "\x1b[1;33m[PAUSED]\x1b[0m"
    } else {
        "\x1b[1;32m[LIVE]\x1b[0m"
    };

    out.push_str(&format!(
        " \x1b[1;36mTUSK\x1b[0m 🦣  {} | Uptime: \x1b[1m{}\x1b[0m | Load: \x1b[1m{:.2} {:.2} {:.2}\x1b[0m | Rate: {}ms\n",
        status_tag, uptime_str, l1, l5, l15, monitor.refresh_rate_ms
    ));
    out.push_str("\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n");

    // 2. CPU Gauges & Historical Sparkline
    let sparkline = render_sparkline(&monitor.cpu_history);
    out.push_str(&format!(
        " \x1b[1mCPU Total:\x1b[0m {:5.1}% {}  History: {}\n",
        global_cpu_pct,
        render_colored_bar(global_cpu_pct, 24),
        sparkline
    ));

    // Per-core mini meters (2 per line)
    for chunk in core_cpu_pcts.chunks(2) {
        out.push_str("   ");
        for (idx, &core_pct) in chunk.iter().enumerate() {
            let core_num = core_cpu_pcts.iter().position(|&p| p == core_pct).unwrap_or(idx);
            out.push_str(&format!(
                "Core {:02}: {:4.1}% {}   ",
                core_num,
                core_pct,
                render_colored_bar(core_pct, 12)
            ));
        }
        out.push('\n');
    }

    out.push_str("\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n");

    // 3. Memory & Swap Gauges
    let used_gb = mem.used_kb() as f64 / 1_048_576.0;
    let total_gb = mem.total_kb as f64 / 1_048_576.0;
    let ram_pct = if total_gb > 0.0 { (used_gb / total_gb) * 100.0 } else { 0.0 };

    let swap_used_gb = mem.swap_used_kb() as f64 / 1_048_576.0;
    let swap_total_gb = mem.swap_total_kb as f64 / 1_048_576.0;
    let swap_pct = if swap_total_gb > 0.0 { (swap_used_gb / swap_total_gb) * 100.0 } else { 0.0 };

    out.push_str(&format!(
        " \x1b[1mRAM Usage:\x1b[0m {:4.1}% {}  {:.2} GB / {:.2} GB (Cached: {:.2} GB)\n",
        ram_pct,
        render_colored_bar(ram_pct, 24),
        used_gb,
        total_gb,
        mem.cached_kb as f64 / 1_048_576.0
    ));

    if swap_total_gb > 0.0 {
        out.push_str(&format!(
            " \x1b[1mSwap Used:\x1b[0m {:4.1}% {}  {:.2} GB / {:.2} GB\n",
            swap_pct,
            render_colored_bar(swap_pct, 24),
            swap_used_gb,
            swap_total_gb
        ));
    }

    out.push_str("\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n");

    // 4. Network I/O Bandwidth
    out.push_str(" \x1b[1mNetwork Traffic:\x1b[0m\n");
    for (iface, (rx_kbps, tx_kbps, tot_rx, tot_tx)) in net_speeds {
        out.push_str(&format!(
            "   \x1b[1;36m{:6}\x1b[0m  ↓ \x1b[1;32m{:7.1} KB/s\x1b[0m ({:6.1} MB total)   ↑ \x1b[1;34m{:7.1} KB/s\x1b[0m ({:6.1} MB total)\n",
            iface,
            rx_kbps,
            *tot_rx as f64 / 1_048_576.0,
            tx_kbps,
            *tot_tx as f64 / 1_048_576.0
        ));
    }

    out.push_str("\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n");

    // 5. Top Active Processes Table
    let sort_label = match monitor.sort_order {
        SortOrder::Cpu => "\x1b[1;32m[Sorted by CPU%]\x1b[0m",
        SortOrder::Memory => "\x1b[1;34m[Sorted by MEM]\x1b[0m",
        SortOrder::Pid => "\x1b[1;33m[Sorted by PID]\x1b[0m",
    };

    out.push_str(&format!(" \x1b[1mTop Processes:\x1b[0m {} (Showing top 10)\n", sort_label));
    out.push_str("   \x1b[1mPID     CPU%     MEM (MB)   STATE   COMMAND\x1b[0m\n");

    for p in processes.iter().take(10) {
        let mem_mb = p.rss_kb as f64 / 1024.0;
        let cpu_color = if p.cpu_percent > 50.0 {
            "\x1b[1;31m"
        } else if p.cpu_percent > 15.0 {
            "\x1b[1;33m"
        } else {
            "\x1b[0m"
        };

        out.push_str(&format!(
            "   {:<7} {}{:5.1}%\x1b[0m    {:7.1}     [{}]     {:<25}\n",
            p.pid, cpu_color, p.cpu_percent, mem_mb, p.state, p.comm
        ));
    }

    out.push_str("\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n");

    // 6. Interactive Hotkey Footer
    out.push_str(" \x1b[1m[c]\x1b[0m Sort CPU  \x1b[1m[m]\x1b[0m Sort RAM  \x1b[1m[p]\x1b[0m Sort PID  \x1b[1m[Space]\x1b[0m Pause  \x1b[1m[+]/[-]\x1b[0m Rate  \x1b[1m[q]\x1b[0m Quit\n");

    // Single flush to terminal
    print!("{}", out);
    let _ = io::stdout().flush();
}

fn render_colored_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round().clamp(0.0, width as f64) as usize;
    let empty = width.saturating_sub(filled);

    let color = if pct >= 80.0 {
        "\x1b[1;31m" // Red
    } else if pct >= 50.0 {
        "\x1b[1;33m" // Yellow
    } else {
        "\x1b[1;32m" // Green
    };

    format!("[{}{}{}\x1b[0m]", color, "█".repeat(filled), "░".repeat(empty))
}

fn render_sparkline(history: &VecDeque<f64>) -> String {
    const SPARK_CHARS: [char; 8] = [' ', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut s = String::new();

    for &val in history {
        let idx = ((val / 100.0) * (SPARK_CHARS.len() - 1) as f64).round() as usize;
        let char_idx = idx.clamp(0, SPARK_CHARS.len() - 1);
        let color = if val >= 80.0 {
            "\x1b[31m"
        } else if val >= 50.0 {
            "\x1b[33m"
        } else {
            "\x1b[32m"
        };
        s.push_str(color);
        s.push(SPARK_CHARS[char_idx]);
    }
    s.push_str("\x1b[0m");
    s
}

// =========================================================================
// SECTION 5: MAIN EVENT & INPUT LOOP
// =========================================================================

fn main() {
    // 1. Enter Raw Mode & Alternate Screen Buffer
    let _guard = match RawTerminalGuard::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Failed to initialize raw terminal: {}", e);
            return;
        }
    };

    let mut monitor = SystemMonitor::new();
    let mut stdin_buffer = [0u8; 16];

    loop {
        let tick_start = Instant::now();
        let elapsed_secs = monitor.prev_tick_time.elapsed().as_secs_f64().max(0.001);
        monitor.prev_tick_time = Instant::now();

        if !monitor.paused {
            // Read fresh CPU
            let (curr_global, curr_cores) = monitor.read_cpu_snapshots().unwrap_or_default();
            let global_cpu_pct = calc_cpu_percent(&monitor.prev_global_cpu, &curr_global);

            let mut core_pcts = Vec::new();
            for (prev, curr) in monitor.prev_per_core_cpu.iter().zip(curr_cores.iter()) {
                core_pcts.push(calc_cpu_percent(prev, curr));
            }

            monitor.prev_global_cpu = curr_global;
            monitor.prev_per_core_cpu = curr_cores;

            // Push to sparkline history
            if monitor.cpu_history.len() >= 24 {
                monitor.cpu_history.pop_front();
            }
            monitor.cpu_history.push_back(global_cpu_pct);

            // Read Memory
            let mem = monitor.read_mem_info().unwrap_or_default();

            // Read Network Throughput
            let curr_net = monitor.read_net_snapshots().unwrap_or_default();
            let mut net_speeds = HashMap::new();

            for (iface, curr_stat) in &curr_net {
                if let Some(prev_stat) = monitor.prev_net_devices.get(iface) {
                    let rx_diff = curr_stat.rx_bytes.saturating_sub(prev_stat.rx_bytes) as f64;
                    let tx_diff = curr_stat.tx_bytes.saturating_sub(prev_stat.tx_bytes) as f64;

                    let rx_kbps = (rx_diff / 1024.0) / elapsed_secs;
                    let tx_kbps = (tx_diff / 1024.0) / elapsed_secs;

                    net_speeds.insert(
                        iface.clone(),
                        (rx_kbps, tx_kbps, curr_stat.rx_bytes, curr_stat.tx_bytes),
                    );
                }
            }
            monitor.prev_net_devices = curr_net;

            // Read Processes
            let procs = monitor.read_processes(elapsed_secs);

            // Render Dashboard Frame
            render_dashboard(&mut monitor, global_cpu_pct, &core_pcts, &mem, &net_speeds, &procs);
        }

        // Handle Non-blocking Keyboard Input
        let mut stdin = io::stdin();
        if let Ok(n) = stdin.read(&mut stdin_buffer) {
            for &byte in &stdin_buffer[..n] {
                match byte {
                    b'q' | b'Q' | 27 => {
                        // 'q' or ESC -> Exit cleanly
                        return;
                    }
                    b' ' => {
                        // Spacebar -> Toggle Pause
                        monitor.paused = !monitor.paused;
                    }
                    b'c' | b'C' => {
                        monitor.sort_order = SortOrder::Cpu;
                    }
                    b'm' | b'M' => {
                        monitor.sort_order = SortOrder::Memory;
                    }
                    b'p' | b'P' => {
                        monitor.sort_order = SortOrder::Pid;
                    }
                    b'+' | b'=' => {
                        monitor.refresh_rate_ms = monitor.refresh_rate_ms.saturating_sub(250).max(250);
                    }
                    b'-' | b'_' => {
                        monitor.refresh_rate_ms = (monitor.refresh_rate_ms + 250).min(5000);
                    }
                    _ => {}
                }
            }
        }

        // Sleep remainder of tick
        let tick_duration = Duration::from_millis(monitor.refresh_rate_ms);
        let elapsed = tick_start.elapsed();
        if elapsed < tick_duration {
            thread::sleep(tick_duration - elapsed);
        }
    }
}
