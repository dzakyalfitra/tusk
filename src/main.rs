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
use std::io::{self, Write};
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

            // Install panic hook to restore terminal on panic
            let default_panic = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                println!("\x1b[?1049l\x1b[?25h\x1b[0m");
                let _ = io::stdout().flush();
                default_panic(info);
            }));

            // Switch to Alternate Screen buffer, clear screen, and hide cursor
            print!("\x1b[?1049h\x1b[2J\x1b[?25l");
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
            println!("\x1b[?1049l\x1b[?25h\x1b[0m");
            let _ = io::stdout().flush();
        }
    }
}

fn get_terminal_size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 {
            let cols = if ws.ws_col > 0 {
                ws.ws_col as usize
            } else {
                80
            };
            let rows = if ws.ws_row > 0 {
                ws.ws_row as usize
            } else {
                24
            };
            (cols, rows)
        } else {
            (80, 24)
        }
    }
}

fn poll_stdin(timeout_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let res = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    res > 0 && (pfd.revents & libc::POLLIN) != 0
}

fn read_stdin_bytes(buf: &mut [u8]) -> usize {
    let res = unsafe {
        libc::read(
            libc::STDIN_FILENO,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if res > 0 {
        res as usize
    } else {
        0
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum KeyAction {
    Quit,
    TogglePause,
    Sort(SortOrder),
    AdjustRate(i64),
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

fn parse_keys(bytes: &[u8]) -> Vec<KeyAction> {
    let mut actions = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'q' | b'Q' => {
                actions.push(KeyAction::Quit);
                i += 1;
            }
            27 => {
                // Escape sequence (Arrow keys, PageUp/Down, Home, End, etc.)
                if i + 1 < bytes.len() && (bytes[i + 1] == b'[' || bytes[i + 1] == b'O') {
                    if i + 2 < bytes.len() {
                        match bytes[i + 2] {
                            b'A' => {
                                actions.push(KeyAction::Up);
                                i += 3;
                            }
                            b'B' => {
                                actions.push(KeyAction::Down);
                                i += 3;
                            }
                            b'C' => {
                                i += 3;
                            } // Right arrow
                            b'D' => {
                                i += 3;
                            } // Left arrow
                            b'H' => {
                                actions.push(KeyAction::Home);
                                i += 3;
                            }
                            b'F' => {
                                actions.push(KeyAction::End);
                                i += 3;
                            }
                            b'1'..=b'9' if i + 3 < bytes.len() && bytes[i + 3] == b'~' => {
                                match bytes[i + 2] {
                                    b'1' | b'7' => actions.push(KeyAction::Home),
                                    b'4' | b'8' => actions.push(KeyAction::End),
                                    b'5' => actions.push(KeyAction::PageUp),
                                    b'6' => actions.push(KeyAction::PageDown),
                                    _ => {}
                                }
                                i += 4;
                            }
                            _ => {
                                // Skip unfamiliar CSI parameter bytes
                                i += 2;
                                while i < bytes.len() && (bytes[i] < 0x40 || bytes[i] > 0x7E) {
                                    i += 1;
                                }
                                if i < bytes.len() {
                                    i += 1;
                                }
                            }
                        }
                    } else {
                        i = bytes.len();
                    }
                } else {
                    // Lone ESC press -> Quit
                    actions.push(KeyAction::Quit);
                    i += 1;
                }
            }
            b'k' | b'K' => {
                actions.push(KeyAction::Up);
                i += 1;
            }
            b'j' | b'J' => {
                actions.push(KeyAction::Down);
                i += 1;
            }
            b'g' => {
                actions.push(KeyAction::Home);
                i += 1;
            }
            b'G' => {
                actions.push(KeyAction::End);
                i += 1;
            }
            b' ' => {
                actions.push(KeyAction::TogglePause);
                i += 1;
            }
            b'c' | b'C' => {
                actions.push(KeyAction::Sort(SortOrder::Cpu));
                i += 1;
            }
            b'm' | b'M' => {
                actions.push(KeyAction::Sort(SortOrder::Memory));
                i += 1;
            }
            b'p' | b'P' => {
                actions.push(KeyAction::Sort(SortOrder::Pid));
                i += 1;
            }
            b'+' | b'=' => {
                actions.push(KeyAction::AdjustRate(-250));
                i += 1;
            }
            b'-' | b'_' => {
                actions.push(KeyAction::AdjustRate(250));
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    actions
}

fn format_time_ticks(ticks: u64) -> String {
    let total_secs = ticks / 100;
    let centis = ticks % 100;
    let secs = total_secs % 60;
    let total_mins = total_secs / 60;
    let mins = total_mins % 60;
    let hours = total_mins / 60;

    if hours > 0 {
        format!("{:>2}h{:02}m", hours, mins)
    } else {
        format!("{:>2}:{:02}.{:02}", mins, secs, centis)
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
    total_time_ticks: u64,
}

#[derive(Debug, PartialEq, Clone, Copy)]
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
    selected_index: usize,
    scroll_offset: usize,
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
            selected_index: 0,
            scroll_offset: 0,
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

    fn sort_processes(&self, procs: &mut [ProcessInfo]) {
        match self.sort_order {
            SortOrder::Cpu => procs.sort_by(|a, b| {
                b.cpu_percent
                    .partial_cmp(&a.cpu_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortOrder::Memory => procs.sort_by_key(|a| std::cmp::Reverse(a.rss_kb)),
            SortOrder::Pid => procs.sort_by_key(|a| a.pid),
        }
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
            let val = parts
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);

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
                        if let Ok(p_info) = self.parse_single_proc(pid, page_size_kb, elapsed_secs)
                        {
                            procs.push(p_info);
                        }
                    }
                }
            }
        }

        // Sort processes
        self.sort_processes(&mut procs);

        procs
    }

    fn parse_single_proc(
        &mut self,
        pid: u32,
        page_size_kb: u64,
        elapsed_secs: f64,
    ) -> io::Result<ProcessInfo> {
        let stat_path = format!("/proc/{}/stat", pid);
        let stat_content = fs::read_to_string(stat_path)?;

        let start_paren = stat_content.find('(').ok_or(io::ErrorKind::InvalidData)?;
        let end_paren = stat_content.rfind(')').ok_or(io::ErrorKind::InvalidData)?;
        let short_comm = stat_content[start_paren + 1..end_paren].to_string();

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
        let prev_ticks = self
            .prev_proc_times
            .insert(pid, total_ticks)
            .unwrap_or(total_ticks);
        let tick_diff = total_ticks.saturating_sub(prev_ticks);
        let cpu_percent = if elapsed_secs > 0.0 {
            ((tick_diff as f64 / 100.0) / elapsed_secs) * 100.0
        } else {
            0.0
        };

        // Try reading cmdline for full command line arguments, fall back to [short_comm]
        let comm = fs::read(format!("/proc/{}/cmdline", pid))
            .ok()
            .and_then(|bytes| {
                if bytes.is_empty() {
                    None
                } else {
                    let s: String = bytes
                        .iter()
                        .map(|&b| if b == 0 { ' ' } else { b as char })
                        .collect();
                    let trimmed = s.trim().to_string();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                }
            })
            .unwrap_or_else(|| format!("[{}]", short_comm));

        Ok(ProcessInfo {
            pid,
            comm,
            state,
            rss_kb,
            cpu_percent,
            total_time_ticks: total_ticks,
        })
    }
}

fn parse_cpu_line(line: &str) -> CpuRawSnapshot {
    let mut nums = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse::<u64>().ok());
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

#[allow(clippy::too_many_arguments)]
fn render_dashboard(
    monitor: &mut SystemMonitor,
    cols: usize,
    rows: usize,
    global_cpu_pct: f64,
    core_cpu_pcts: &[f64],
    mem: &MemInfo,
    net_speeds: &HashMap<String, (f64, f64, u64, u64)>,
    processes: &[ProcessInfo],
) {
    let mut out = String::with_capacity(16384);
    let mut rendered_lines = 0;

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

    let running_count = processes.iter().filter(|p| p.state == 'R').count();
    let header = format!(
        " \x1b[1;36mTUSK\x1b[0m 🦣  {} | Tasks: \x1b[1m{}\x1b[0m ({} running) | Uptime: \x1b[1m{}\x1b[0m | Load: \x1b[1m{:.2} {:.2} {:.2}\x1b[0m | Rate: {}ms\x1b[K\n",
        status_tag, processes.len(), running_count, uptime_str, l1, l5, l15, monitor.refresh_rate_ms
    );
    out.push_str(&header);
    rendered_lines += 1;

    let div_len = cols.clamp(40, 1000);
    let div = format!("\x1b[90m{}\x1b[0m\x1b[K\n", "─".repeat(div_len));

    out.push_str(&div);
    rendered_lines += 1;

    // 2. CPU Gauges & Historical Sparkline
    let sparkline = render_sparkline(&monitor.cpu_history);
    out.push_str(&format!(
        " \x1b[1mCPU Total:\x1b[0m {:5.1}% {}  History: {}\x1b[K\n",
        global_cpu_pct,
        render_colored_bar(global_cpu_pct, 24),
        sparkline
    ));
    rendered_lines += 1;

    // Per-core mini meters (adaptive columns: 4 columns if wide and >8 cores, else 2)
    let cores_per_row = if cols >= 120 && core_cpu_pcts.len() > 8 {
        4
    } else {
        2
    };
    let bar_len = if cores_per_row == 4 { 10 } else { 12 };

    for (chunk_idx, chunk) in core_cpu_pcts.chunks(cores_per_row).enumerate() {
        out.push(' ');
        for (sub_idx, &core_pct) in chunk.iter().enumerate() {
            let core_num = chunk_idx * cores_per_row + sub_idx;
            out.push_str(&format!(
                "Core {:02}: {:4.1}% {}   ",
                core_num,
                core_pct,
                render_colored_bar(core_pct, bar_len)
            ));
        }
        out.push_str("\x1b[K\n");
        rendered_lines += 1;
    }

    out.push_str(&div);
    rendered_lines += 1;

    // 3. Memory & Swap Gauges
    let used_gb = mem.used_kb() as f64 / 1_048_576.0;
    let total_gb = mem.total_kb as f64 / 1_048_576.0;
    let ram_pct = if total_gb > 0.0 {
        (used_gb / total_gb) * 100.0
    } else {
        0.0
    };

    let swap_used_gb = mem.swap_used_kb() as f64 / 1_048_576.0;
    let swap_total_gb = mem.swap_total_kb as f64 / 1_048_576.0;
    let swap_pct = if swap_total_gb > 0.0 {
        (swap_used_gb / swap_total_gb) * 100.0
    } else {
        0.0
    };

    out.push_str(&format!(
        " \x1b[1mRAM Usage:\x1b[0m {:4.1}% {}  {:.2} GB / {:.2} GB (Cached: {:.2} GB)\x1b[K\n",
        ram_pct,
        render_colored_bar(ram_pct, 24),
        used_gb,
        total_gb,
        mem.cached_kb as f64 / 1_048_576.0
    ));
    rendered_lines += 1;

    if swap_total_gb > 0.0 {
        out.push_str(&format!(
            " \x1b[1mSwap Used:\x1b[0m {:4.1}% {}  {:.2} GB / {:.2} GB\x1b[K\n",
            swap_pct,
            render_colored_bar(swap_pct, 24),
            swap_used_gb,
            swap_total_gb
        ));
        rendered_lines += 1;
    }

    out.push_str(&div);
    rendered_lines += 1;

    // 4. Network Traffic (sorted by activity, max 2-3 interfaces to save space)
    out.push_str(" \x1b[1mNetwork Traffic:\x1b[0m\x1b[K\n");
    rendered_lines += 1;

    let mut active_ifaces: Vec<_> = net_speeds.iter().collect();
    active_ifaces.sort_by(|a, b| {
        let act_a = (a.1).0 + (a.1).1 + (a.1).2 as f64 + (a.1).3 as f64;
        let act_b = (b.1).0 + (b.1).1 + (b.1).2 as f64 + (b.1).3 as f64;
        act_b
            .partial_cmp(&act_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let max_ifaces = if rows >= 35 { 3 } else { 2 };
    for (iface, (rx_kbps, tx_kbps, tot_rx, tot_tx)) in active_ifaces.iter().take(max_ifaces) {
        out.push_str(&format!(
            "   \x1b[1;36m{:6}\x1b[0m  ↓ \x1b[1;32m{:7.1} KB/s\x1b[0m ({:6.1} MB)   ↑ \x1b[1;34m{:7.1} KB/s\x1b[0m ({:6.1} MB)\x1b[K\n",
            iface,
            rx_kbps,
            *tot_rx as f64 / 1_048_576.0,
            tx_kbps,
            *tot_tx as f64 / 1_048_576.0
        ));
        rendered_lines += 1;
    }

    out.push_str(&div);
    rendered_lines += 1;

    // 5. Top Active Processes Table (Full coverage down to footer)
    // 1 line for table header, 1 line for footer
    let available_proc_rows = rows.saturating_sub(rendered_lines + 2);

    // Calculate command column width
    // Fixed columns: PID (7), CPU% (7), MEM(MB) (10), TIME+ (9), STATE (6) = 39 chars.
    let cmd_width = cols.saturating_sub(41).max(10);

    let proc_header = format!(
        "\x1b[7m\x1b[1m {:<6} {:>6} {:>10} {:>9}  {:<5} {:<cmd_width$}\x1b[0m\x1b[K\n",
        "PID",
        "CPU%",
        "MEM (MB)",
        "TIME+",
        "STATE",
        "COMMAND",
        cmd_width = cmd_width
    );
    out.push_str(&proc_header);

    // Adjust scrolling and selection bounds
    if processes.is_empty() {
        monitor.selected_index = 0;
        monitor.scroll_offset = 0;
    } else {
        if monitor.selected_index >= processes.len() {
            monitor.selected_index = processes.len() - 1;
        }
        if monitor.selected_index < monitor.scroll_offset {
            monitor.scroll_offset = monitor.selected_index;
        } else if monitor.selected_index >= monitor.scroll_offset + available_proc_rows {
            monitor.scroll_offset = monitor.selected_index + 1 - available_proc_rows;
        }
        if monitor.scroll_offset + available_proc_rows > processes.len() {
            monitor.scroll_offset = processes.len().saturating_sub(available_proc_rows);
        }
    }

    for row_idx in 0..available_proc_rows {
        let abs_idx = monitor.scroll_offset + row_idx;
        if abs_idx < processes.len() {
            let p = &processes[abs_idx];
            let mem_mb = p.rss_kb as f64 / 1024.0;
            let is_selected = abs_idx == monitor.selected_index;
            let time_str = format_time_ticks(p.total_time_ticks);

            let comm_display = if p.comm.len() > cmd_width {
                &p.comm[..cmd_width]
            } else {
                &p.comm
            };

            if is_selected {
                // Highlight selected process with inverse bar
                let line =
                    format!(
                    "\x1b[7m {:<6} {:>5.1}% {:>10.1} {:>9}  [{}]  {:<cmd_width$}\x1b[0m\x1b[K\n",
                    p.pid, p.cpu_percent, mem_mb, time_str, p.state, comm_display,
                    cmd_width = cmd_width
                );
                out.push_str(&line);
            } else {
                let cpu_color = if p.cpu_percent > 50.0 {
                    "\x1b[1;31m"
                } else if p.cpu_percent > 15.0 {
                    "\x1b[1;33m"
                } else if p.cpu_percent > 0.0 {
                    "\x1b[32m"
                } else {
                    "\x1b[90m"
                };

                let state_color = match p.state {
                    'R' => "\x1b[1;32m",
                    'D' => "\x1b[1;33m",
                    'Z' => "\x1b[1;31m",
                    _ => "\x1b[37m",
                };

                let line = format!(
                    " \x1b[36m{:<6}\x1b[0m {}{:>5.1}%\x1b[0m {:>10.1} \x1b[90m{:>9}\x1b[0m  {}[{}]\x1b[0m  {:<cmd_width$}\x1b[K\n",
                    p.pid, cpu_color, p.cpu_percent, mem_mb, time_str, state_color, p.state, comm_display,
                    cmd_width = cmd_width
                );
                out.push_str(&line);
            }
        } else {
            // Fill unused process lines so terminal is completely covered
            out.push_str("\x1b[K\n");
        }
    }

    // 6. Interactive Hotkey Footer (Pinned to bottom line)
    out.push_str(" \x1b[7m c \x1b[0m CPU  \x1b[7m m \x1b[0m MEM  \x1b[7m p \x1b[0m PID  \x1b[7m Space \x1b[0m Pause  \x1b[7m +/- \x1b[0m Rate  \x1b[7m ↑↓ \x1b[0m Scroll  \x1b[7m q \x1b[0m Quit\x1b[K");

    // Erase any remaining lines below the footer
    out.push_str("\x1b[J");

    // Single flush to terminal
    let mut stdout = io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

fn render_colored_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64)
        .round()
        .clamp(0.0, width as f64) as usize;
    let empty = width.saturating_sub(filled);

    let color = if pct >= 80.0 {
        "\x1b[1;31m" // Red
    } else if pct >= 50.0 {
        "\x1b[1;33m" // Yellow
    } else {
        "\x1b[1;32m" // Green
    };

    format!(
        "[{}{}{}\x1b[0m]",
        color,
        "█".repeat(filled),
        "░".repeat(empty)
    )
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
    let mut stdin_buffer = [0u8; 64];

    let mut global_cpu_pct = 0.0;
    let mut core_pcts = Vec::new();
    let mut mem = MemInfo::default();
    let mut net_speeds = HashMap::new();
    let mut procs = Vec::new();

    let mut next_metric_tick = Instant::now();
    let mut needs_render = true;

    loop {
        let now = Instant::now();

        // 1. Tick metrics when time is up
        if now >= next_metric_tick {
            let elapsed_secs = monitor.prev_tick_time.elapsed().as_secs_f64().max(0.001);
            monitor.prev_tick_time = Instant::now();

            if !monitor.paused {
                // Read fresh CPU
                let (curr_global, curr_cores) = monitor.read_cpu_snapshots().unwrap_or_default();
                global_cpu_pct = calc_cpu_percent(&monitor.prev_global_cpu, &curr_global);

                core_pcts.clear();
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
                mem = monitor.read_mem_info().unwrap_or_default();

                // Read Network Throughput
                let curr_net = monitor.read_net_snapshots().unwrap_or_default();
                net_speeds.clear();
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
                procs = monitor.read_processes(elapsed_secs);
            }

            needs_render = true;
            next_metric_tick = now + Duration::from_millis(monitor.refresh_rate_ms);
        }

        // 2. Render frame if needed
        if needs_render {
            let (cols, rows) = get_terminal_size();
            render_dashboard(
                &mut monitor,
                cols,
                rows,
                global_cpu_pct,
                &core_pcts,
                &mem,
                &net_speeds,
                &procs,
            );
            needs_render = false;
        }

        // 3. Poll stdin for user interaction until next metric tick
        let now = Instant::now();
        let timeout_ms = if next_metric_tick > now {
            (next_metric_tick - now).as_millis().min(1000) as i32
        } else {
            0
        };

        if poll_stdin(timeout_ms) {
            let mut n = read_stdin_bytes(&mut stdin_buffer);
            if n == 1 && stdin_buffer[0] == 27 {
                // Check if following escape sequence arrives
                if poll_stdin(25) {
                    let m = read_stdin_bytes(&mut stdin_buffer[1..]);
                    n += m;
                }
            }

            let actions = parse_keys(&stdin_buffer[..n]);
            for action in actions {
                match action {
                    KeyAction::Quit => return,
                    KeyAction::TogglePause => {
                        monitor.paused = !monitor.paused;
                        needs_render = true;
                    }
                    KeyAction::Sort(order) => {
                        monitor.sort_order = order;
                        monitor.sort_processes(&mut procs);
                        monitor.selected_index = 0;
                        monitor.scroll_offset = 0;
                        needs_render = true;
                    }
                    KeyAction::AdjustRate(delta) => {
                        if delta < 0 {
                            monitor.refresh_rate_ms = monitor
                                .refresh_rate_ms
                                .saturating_sub((-delta) as u64)
                                .max(250);
                        } else {
                            monitor.refresh_rate_ms =
                                (monitor.refresh_rate_ms + delta as u64).min(5000);
                        }
                        needs_render = true;
                    }
                    KeyAction::Up => {
                        monitor.selected_index = monitor.selected_index.saturating_sub(1);
                        needs_render = true;
                    }
                    KeyAction::Down => {
                        if !procs.is_empty() {
                            monitor.selected_index =
                                (monitor.selected_index + 1).min(procs.len() - 1);
                        }
                        needs_render = true;
                    }
                    KeyAction::PageUp => {
                        let (_, rows) = get_terminal_size();
                        let page = rows.saturating_sub(15).max(5);
                        monitor.selected_index = monitor.selected_index.saturating_sub(page);
                        needs_render = true;
                    }
                    KeyAction::PageDown => {
                        let (_, rows) = get_terminal_size();
                        let page = rows.saturating_sub(15).max(5);
                        if !procs.is_empty() {
                            monitor.selected_index =
                                (monitor.selected_index + page).min(procs.len() - 1);
                        }
                        needs_render = true;
                    }
                    KeyAction::Home => {
                        monitor.selected_index = 0;
                        needs_render = true;
                    }
                    KeyAction::End => {
                        if !procs.is_empty() {
                            monitor.selected_index = procs.len() - 1;
                        }
                        needs_render = true;
                    }
                }
            }
        }
    }
}
