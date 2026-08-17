use std::{
    collections::BTreeMap,
    ffi::OsStr,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use sysinfo::{Disks, Networks, System};

/// Known virtual/ram filesystems to exclude from disk monitoring.
fn is_real_filesystem(fs: &OsStr) -> bool {
    !matches!(
        fs.to_str(),
        Some("tmpfs" | "devtmpfs" | "ramfs" | "overlay" | "aufs")
    )
}

#[derive(Debug, Clone, Default)]
pub struct DiskSample {
    pub mount: String,
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteProcess {
    pub pid: u32,
    pub user: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub command: String,
}

#[derive(Debug, Clone, Default)]
pub struct RemotePort {
    pub protocol: String,
    pub address: String,
    pub port: u16,
    pub state: String,
    pub pid: Option<u32>,
    pub process: String,
}

#[derive(Debug, Clone, Default)]
pub struct SystemSnapshot {
    pub cpu_percent: f32,
    pub mem_percent: f32,
    pub swap_percent: f32,
    pub mem_detail: String,
    pub swap_detail: String,
    pub net_rx: String,
    pub net_tx: String,
    pub net_rx_rate: u64,
    pub net_tx_rate: u64,
    pub disks: Vec<DiskSample>,
    pub total_swap: u64,
}

pub struct SystemSampler {
    sys: System,
    nets: Networks,
    disks: Disks,
    last_rx_total: u64,
    last_tx_total: u64,
    last_instant: Instant,
}

impl SystemSampler {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        let nets = Networks::new_with_refreshed_list();
        let disks = Disks::new_with_refreshed_list();
        let last_rx_total = nets.values().map(|d| d.total_received()).sum();
        let last_tx_total = nets.values().map(|d| d.total_transmitted()).sum();

        Self {
            sys,
            nets,
            disks,
            last_rx_total,
            last_tx_total,
            last_instant: Instant::now(),
        }
    }

    pub fn interval() -> Duration {
        Duration::from_millis(1000)
    }

    pub fn sample(&mut self) -> SystemSnapshot {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.nets.refresh(true);
        self.disks.refresh(true);

        let cpu_percent = self.sys.global_cpu_usage() / 100.0;
        let mem_total = self.sys.total_memory();
        let mem_used = self.sys.used_memory();
        let swap_total = self.sys.total_swap();
        let swap_used = self.sys.used_swap();

        let rx_total: u64 = self.nets.values().map(|d| d.total_received()).sum();
        let tx_total: u64 = self.nets.values().map(|d| d.total_transmitted()).sum();
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last_instant)
            .as_secs_f64()
            .max(0.001);
        let rx_rate = (rx_total.saturating_sub(self.last_rx_total) as f64 / elapsed) as u64;
        let tx_rate = (tx_total.saturating_sub(self.last_tx_total) as f64 / elapsed) as u64;
        self.last_rx_total = rx_total;
        self.last_tx_total = tx_total;
        self.last_instant = now;

        let mut disks: Vec<DiskSample> = self
            .disks
            .iter()
            .filter(|disk| disk.total_space() > 0 && is_real_filesystem(disk.file_system()))
            .map(|disk| DiskSample {
                mount: disk.mount_point().to_string_lossy().to_string(),
                available_bytes: disk.available_space(),
                total_bytes: disk.total_space(),
            })
            .collect();
        disks.sort_by(|a, b| {
            if a.mount == "/" {
                return std::cmp::Ordering::Less;
            }
            if b.mount == "/" {
                return std::cmp::Ordering::Greater;
            }
            a.mount.cmp(&b.mount)
        });

        SystemSnapshot {
            cpu_percent,
            mem_percent: ratio(mem_used, mem_total),
            swap_percent: ratio(swap_used, swap_total),
            mem_detail: format!("{}/{}", format_bytes(mem_used), format_bytes(mem_total)),
            swap_detail: format!("{}/{}", format_bytes(swap_used), format_bytes(swap_total)),
            net_rx: format!("{}/s", format_bytes(rx_rate)),
            net_tx: format!("{}/s", format_bytes(tx_rate)),
            net_rx_rate: rx_rate,
            net_tx_rate: tx_rate,
            disks,
            total_swap: swap_total,
        }
    }
}

fn ratio(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f32 / total as f32).clamp(0.0, 1.0)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn remote_snapshot_from_kv(raw: &str) -> Result<SystemSnapshot> {
    let mut kv = BTreeMap::new();
    let mut disks = Vec::new();

    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(rest) = line.strip_prefix("DISK=") {
            let mut parts = rest.split('\t');
            let mount = parts.next().unwrap_or_default().to_string();
            let available_bytes = parts
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or_default();
            let total_bytes = parts
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or_default();
            disks.push(DiskSample {
                mount,
                available_bytes: available_bytes.min(total_bytes),
                total_bytes,
            });
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        kv.insert(key.to_string(), value.to_string());
    }

    let cpu_percent = kv
        .get("CPU_PERCENT")
        .ok_or_else(|| anyhow!("missing CPU_PERCENT"))?
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .unwrap_or_default()
        / 100.0;

    let mem_total = parse_u64(&kv, "MEM_TOTAL");
    let swap_total = parse_u64(&kv, "SWAP_TOTAL");
    let mem_used = parse_u64(&kv, "MEM_USED").min(mem_total);
    let swap_used = parse_u64(&kv, "SWAP_USED").min(swap_total);
    let rx_rate = parse_u64(&kv, "NET_RX");
    let tx_rate = parse_u64(&kv, "NET_TX");

    // Safety filter: exclude entries with zero/negligible total size
    // (catches any virtual fs lines that slipped past the script filter)
    disks.retain(|d| d.total_bytes >= 1024 * 1024);

    disks.sort_by(|a, b| {
        if a.mount == "/" {
            return std::cmp::Ordering::Less;
        }
        if b.mount == "/" {
            return std::cmp::Ordering::Greater;
        }
        a.mount.cmp(&b.mount)
    });

    Ok(SystemSnapshot {
        cpu_percent: cpu_percent.clamp(0.0, 1.0),
        mem_percent: ratio(mem_used, mem_total),
        swap_percent: ratio(swap_used, swap_total),
        mem_detail: format!("{}/{}", format_bytes(mem_used), format_bytes(mem_total)),
        swap_detail: format!("{}/{}", format_bytes(swap_used), format_bytes(swap_total)),
        net_rx: format!("{}/s", format_bytes(rx_rate)),
        net_tx: format!("{}/s", format_bytes(tx_rate)),
        net_rx_rate: rx_rate,
        net_tx_rate: tx_rate,
        disks,
        total_swap: swap_total,
    })
}

pub fn remote_processes_from_ps(raw: &str) -> Vec<RemoteProcess> {
    raw.lines()
        .filter_map(|line| {
            if let Some(record) = line.strip_prefix("PROCESS\t") {
                let mut fields = record.splitn(5, '\t');
                let pid = fields.next()?.parse::<u32>().ok()?;
                let user = fields.next()?.to_string();
                let cpu_percent = fields
                    .next()?
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .map(|value| value.max(0.0))?;
                let memory_bytes = fields.next()?.parse::<u64>().ok()?;
                let command = fields.next()?.trim().to_string();
                return (!command.is_empty()).then_some(RemoteProcess {
                    pid,
                    user,
                    cpu_percent,
                    memory_bytes,
                    command,
                });
            }

            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let user = fields.next()?.to_string();
            let cpu_percent = fields
                .next()?
                .parse::<f32>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|value| value.max(0.0))?;
            let memory_bytes = fields.next()?.parse::<u64>().ok()?.saturating_mul(1024);
            let command = fields.collect::<Vec<_>>().join(" ");
            if command.is_empty() {
                return None;
            }
            Some(RemoteProcess {
                pid,
                user,
                cpu_percent,
                memory_bytes,
                command,
            })
        })
        .collect()
}

pub fn remote_ports_from_probe(raw: &str) -> Vec<RemotePort> {
    raw.lines()
        .filter_map(|line| {
            let record = line.strip_prefix("PORT\t")?;
            let mut fields = record.splitn(6, '\t');
            let protocol = fields.next()?.trim().to_string();
            let address = fields.next()?.trim().to_string();
            let port = fields.next()?.parse::<u16>().ok()?;
            let state = fields.next()?.trim().to_string();
            let pid = match fields.next()?.trim() {
                "" | "-" | "0" => None,
                value => value.parse::<u32>().ok(),
            };
            let process = fields.next()?.trim().to_string();
            if protocol.is_empty() || address.is_empty() || state.is_empty() {
                return None;
            }
            Some(RemotePort {
                protocol,
                address,
                port,
                state,
                pid,
                process: if process.is_empty() {
                    "-".to_string()
                } else {
                    process
                },
            })
        })
        .collect()
}

fn parse_u64(kv: &BTreeMap<String, String>, key: &str) -> u64 {
    kv.get(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{remote_ports_from_probe, remote_processes_from_ps, remote_snapshot_from_kv};

    #[test]
    fn parses_remote_process_rows_and_rss_bytes() {
        let processes = remote_processes_from_ps(
            "  42 alice 12.5 2048 worker --queue main\n  7 root 0.0 512 sshd\n",
        );

        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].pid, 42);
        assert_eq!(processes[0].user, "alice");
        assert_eq!(processes[0].cpu_percent, 12.5);
        assert_eq!(processes[0].memory_bytes, 2 * 1024 * 1024);
        assert_eq!(processes[0].command, "worker --queue main");
    }

    #[test]
    fn parses_structured_current_process_rows() {
        let processes =
            remote_processes_from_ps("PROCESS\t42\talice\t37.25\t2097152\tworker --queue main\n");

        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 42);
        assert_eq!(processes[0].user, "alice");
        assert_eq!(processes[0].cpu_percent, 37.25);
        assert_eq!(processes[0].memory_bytes, 2 * 1024 * 1024);
        assert_eq!(processes[0].command, "worker --queue main");
    }

    #[test]
    fn skips_malformed_remote_process_rows() {
        let processes = remote_processes_from_ps("PID USER CPU RSS COMMAND\n9 root bad 64 init\n");

        assert!(processes.is_empty());
    }

    #[test]
    fn clamps_remote_resource_values_to_valid_ranges() {
        let snapshot = remote_snapshot_from_kv(
            "CPU_PERCENT=250\nMEM_TOTAL=100\nMEM_USED=150\nSWAP_TOTAL=0\nSWAP_USED=10\nNET_RX=12\nNET_TX=34\nDISK=/\t3145728\t2097152\n",
        )
        .unwrap();

        assert_eq!(snapshot.cpu_percent, 1.0);
        assert_eq!(snapshot.mem_percent, 1.0);
        assert_eq!(snapshot.swap_percent, 0.0);
        assert_eq!(snapshot.disks[0].available_bytes, 2097152);
    }

    #[test]
    fn parses_structured_remote_port_rows() {
        let ports = remote_ports_from_probe(
            "PORT\ttcp\t0.0.0.0\t22\tLISTEN\t123\tsshd\nPORT\tudp\t127.0.0.1\t5353\tUNCONN\t-\t-\n",
        );

        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0].protocol, "tcp");
        assert_eq!(ports[0].address, "0.0.0.0");
        assert_eq!(ports[0].port, 22);
        assert_eq!(ports[0].state, "LISTEN");
        assert_eq!(ports[0].pid, Some(123));
        assert_eq!(ports[0].process, "sshd");
        assert_eq!(ports[1].pid, None);
    }

    #[test]
    fn skips_malformed_remote_port_rows() {
        let ports = remote_ports_from_probe(
            "PORT\ttcp\t0.0.0.0\tbad\tLISTEN\t1\tsshd\nPORT\ttcp\t\t80\tLISTEN\t1\tnginx\n",
        );

        assert!(ports.is_empty());
    }
}
