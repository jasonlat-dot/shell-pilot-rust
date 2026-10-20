//! Read-only Linux telemetry on a separate channel of the original SSH transport.
//! Never use the Agent exec runner here: it mirrors commands into the interactive PTY.
use super::{agent_exec::Lease, ChannelMsg, TerminalManager};
use crate::error::Error;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 128 * 1024;
const COMMAND: &str = include_str!("resources.sh");

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSnapshot {
    pub sampled_at_ms: u64,
    pub supported: bool,
    pub stale: bool,
    pub message: Option<String>,
    pub cpu_percent: Option<f64>,
    pub memory_total_bytes: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub uptime_seconds: Option<f64>,
    pub boot_time_seconds: Option<u64>,
    pub network: Vec<NetworkUsage>,
    pub disks: Vec<DiskUsage>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkUsage {
    pub name: String,
    pub primary: bool,
    pub receive_bytes_per_second: Option<f64>,
    pub transmit_bytes_per_second: Option<f64>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub use_percent: f64,
}
#[derive(Default)]
pub(super) struct MonitorState {
    previous: Option<Sample>,
    cached: Option<ResourceSnapshot>,
    attempted: Option<Instant>,
}
#[derive(Default)]
struct Sample {
    supported: bool,
    cpu: Option<(u64, u64)>,
    memory_total: Option<u64>,
    memory_used: Option<u64>,
    uptime: Option<f64>,
    boot: Option<u64>,
    network: BTreeMap<String, (u64, u64)>,
    primary: Option<String>,
    disks: Vec<DiskUsage>,
}

impl TerminalManager {
    pub async fn resources(&self, id: &str) -> Result<ResourceSnapshot, Error> {
        let session = self.session(id)?;
        let lease = Lease {
            session: session.clone(),
        };
        self.check_agent_lease(&lease)
            .map_err(|_| Error::TerminalMissing)?;
        // A session owns its cache. Reconnects and other tabs cannot inherit its counters.
        let mut state = session.resources.lock().await;
        self.check_agent_lease(&lease)
            .map_err(|_| Error::TerminalMissing)?;
        if let (Some(attempted), Some(cached)) = (state.attempted, &state.cached) {
            let minimum = if cached.supported { 2 } else { 60 };
            if attempted.elapsed() < Duration::from_secs(minimum) {
                return Ok(cached.clone());
            }
        }
        state.attempted = Some(Instant::now());
        let cancel = CancellationToken::new();
        let collect = async {
            let mut prepared = self
                .prepare_agent_exec(&lease, &cancel, false)
                .await
                .map_err(|_| Error::Busy)?;
            let channel = prepared.opening.0.as_mut().ok_or(Error::Ssh)?;
            // The account's login shell may be fish/csh; the fixed probe uses POSIX sh.
            let script = COMMAND.replace("\r\n", "\n");
            let command = format!("sh -c '{}'", script.replace('\'', "'\\''"));
            channel
                .exec(true, command.as_bytes())
                .await
                .map_err(|_| Error::Ssh)?;
            channel.eof().await.map_err(|_| Error::Ssh)?;
            let mut output = Vec::new();
            let mut stderr_bytes = 0usize;
            let mut status = None;
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        if output.len() + stderr_bytes + data.len() > LIMIT {
                            return Err(Error::Ssh);
                        }
                        output.extend_from_slice(&data);
                    }
                    ChannelMsg::ExtendedData { data, .. } => {
                        stderr_bytes = stderr_bytes.saturating_add(data.len());
                        if output.len() + stderr_bytes > LIMIT {
                            return Err(Error::Ssh);
                        }
                    }
                    ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    ChannelMsg::Failure | ChannelMsg::ExitSignal { .. } => return Err(Error::Ssh),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            if status != Some(0) {
                return Err(Error::Ssh);
            }
            parse_sample(std::str::from_utf8(&output).map_err(|_| Error::Ssh)?)
        };
        let result = tokio::select! {
            biased;
            _ = session.stop.cancelled() => return Err(Error::TerminalMissing),
            _ = self.shutdown.cancelled() => return Err(Error::TerminalMissing),
            result = tokio::time::timeout(Duration::from_secs(8), collect) => result.unwrap_or(Err(Error::ConnectTimeout)),
        };
        self.check_agent_lease(&lease)
            .map_err(|_| Error::TerminalMissing)?;
        let snapshot = match result {
            Ok(sample) => {
                let snapshot = sample.snapshot(state.previous.as_ref());
                state.previous = Some(sample);
                snapshot
            }
            Err(_) => {
                let mut snapshot = state
                    .cached
                    .clone()
                    .unwrap_or_else(|| Sample::default().snapshot(None));
                snapshot.supported = true;
                snapshot.stale = true;
                snapshot.message = Some("资源采集暂不可用；将自动重试，不影响终端。".into());
                snapshot
            }
        };
        state.attempted = Some(Instant::now());
        state.cached = Some(snapshot.clone());
        Ok(snapshot)
    }
}

impl Sample {
    fn snapshot(&self, previous: Option<&Self>) -> ResourceSnapshot {
        // Uptime is measured remotely, avoiding client clock skew and variable SSH latency.
        let previous = previous.filter(|old| {
            self.boot == old.boot && matches!((self.uptime, old.uptime), (Some(now), Some(then)) if now > then && now - then <= 30.0)
        });
        let cpu_percent = previous.and_then(|old| {
            let ((total, idle), (old_total, old_idle)) = self.cpu.zip(old.cpu)?;
            let total = total.checked_sub(old_total)?;
            let idle = idle.checked_sub(old_idle)?;
            (total > 0 && idle <= total).then(|| (total - idle) as f64 * 100.0 / total as f64)
        });
        let elapsed = previous.and_then(|old| Some(self.uptime? - old.uptime?));
        ResourceSnapshot {
            sampled_at_ms: chrono::Utc::now().timestamp_millis().max(0) as u64,
            supported: self.supported,
            stale: false,
            message: (!self.supported)
                .then(|| "当前仅支持提供 /proc 的 Linux / WSL 服务器。".into()),
            cpu_percent,
            memory_total_bytes: self.memory_total,
            memory_used_bytes: self.memory_used,
            uptime_seconds: self.uptime,
            boot_time_seconds: self.boot,
            network: self
                .network
                .iter()
                .map(|(name, &(receive, transmit))| {
                    let rates = previous
                        .and_then(|old| old.network.get(name))
                        .zip(elapsed)
                        .and_then(|(&(old_receive, old_transmit), elapsed)| {
                            Some((
                                receive.checked_sub(old_receive)? as f64 / elapsed,
                                transmit.checked_sub(old_transmit)? as f64 / elapsed,
                            ))
                        });
                    NetworkUsage {
                        name: name.clone(),
                        primary: self.primary.as_ref() == Some(name),
                        receive_bytes_per_second: rates.map(|rate| rate.0),
                        transmit_bytes_per_second: rates.map(|rate| rate.1),
                    }
                })
                .collect(),
            disks: self.disks.clone(),
        }
    }
}

fn parse_sample(text: &str) -> Result<Sample, Error> {
    let normalized = text.replace("\r\n", "\n");
    let (_, text) = normalized
        .split_once("SHELLPILOT_RESOURCES_V1\n")
        .ok_or(Error::Ssh)?;
    if text.trim() == "UNSUPPORTED" {
        return Ok(Sample::default());
    }
    let mut sample = Sample {
        supported: true,
        ..Sample::default()
    };
    let mut section = "";
    let mut memory = BTreeMap::new();
    let mut default_metric = u64::MAX;
    for line in text.lines() {
        // The probe separates sections with blank lines. Do not erase a parsed
        // uptime on the separator: rate calculations depend on that timestamp.
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line;
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        match section {
            "[stat]" if fields.first() == Some(&"cpu") => {
                // guest and guest_nice are already included in user/nice. Ignore them.
                let values: Option<Vec<u64>> = fields
                    .iter()
                    .skip(1)
                    .take(8)
                    .map(|value| value.parse().ok())
                    .collect();
                if let Some(values) = values.filter(|values| values.len() >= 4) {
                    let total = values
                        .iter()
                        .try_fold(0u64, |sum, value| sum.checked_add(*value));
                    let idle = values[3].checked_add(values.get(4).copied().unwrap_or(0));
                    sample.cpu = total.zip(idle);
                }
            }
            "[stat]" if fields.first() == Some(&"btime") => {
                sample.boot = fields.get(1).and_then(|value| value.parse().ok())
            }
            "[memory]" if fields.len() >= 2 => {
                if let Ok(value) = fields[1].parse::<u64>() {
                    memory.insert(fields[0], value);
                }
            }
            "[uptime]" => {
                sample.uptime = fields
                    .first()
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value >= 0.0)
            }
            "[network]" => {
                if let Some((name, counters)) = line.split_once(':') {
                    let name = name.trim();
                    let counters: Vec<_> = counters.split_whitespace().collect();
                    if name != "lo" && !name.is_empty() && counters.len() >= 9 {
                        if let (Ok(receive), Ok(transmit)) =
                            (counters[0].parse(), counters[8].parse())
                        {
                            sample.network.insert(name.to_owned(), (receive, transmit));
                        }
                    }
                }
            }
            "[route]"
                if fields.len() >= 8 && fields[1] == "00000000" && fields[7] == "00000000" =>
            {
                if let (Ok(flags), Ok(metric)) =
                    (u64::from_str_radix(fields[3], 16), fields[6].parse::<u64>())
                {
                    if flags & 1 == 1 && metric < default_metric {
                        default_metric = metric;
                        sample.primary = Some(fields[0].to_owned());
                    }
                }
            }
            "[disks]" if sample.disks.len() < 64 => {
                if let Some(disk) = parse_disk(&fields) {
                    sample.disks.push(disk);
                }
            }
            _ => {}
        }
    }
    // A truncated or foreign shell response must never become a successful all-zero sample.
    if !text.lines().any(|line| line == "[end]")
        || (sample.cpu.is_none() && sample.uptime.is_none() && memory.is_empty())
    {
        return Err(Error::Ssh);
    }
    sample.memory_total = memory
        .get("MemTotal:")
        .copied()
        .and_then(|value| value.checked_mul(1024));
    let available = memory
        .get("MemAvailable:")
        .copied()
        .or_else(|| {
            Some(
                memory
                    .get("MemFree:")?
                    .saturating_add(*memory.get("Buffers:").unwrap_or(&0))
                    .saturating_add(*memory.get("Cached:").unwrap_or(&0))
                    .saturating_add(*memory.get("SReclaimable:").unwrap_or(&0))
                    .saturating_sub(*memory.get("Shmem:").unwrap_or(&0)),
            )
        })
        .and_then(|value| value.checked_mul(1024));
    sample.memory_used = sample
        .memory_total
        .zip(available)
        .map(|(total, available)| total.saturating_sub(available));
    if !sample
        .primary
        .as_ref()
        .is_some_and(|name| sample.network.contains_key(name))
    {
        sample.primary = sample
            .network
            .keys()
            .find(|name| {
                !name.starts_with("veth") && !name.starts_with("br-") && !name.starts_with("docker")
            })
            .or_else(|| sample.network.keys().next())
            .cloned();
    }
    sample.disks.sort_by(|a, b| {
        (a.mount_point != "/", &a.mount_point).cmp(&(b.mount_point != "/", &b.mount_point))
    });
    Ok(sample)
}

fn parse_disk(fields: &[&str]) -> Option<DiskUsage> {
    if fields.len() < 6 || matches!(fields[0], "tmpfs" | "devtmpfs" | "udev") {
        return None;
    }
    let mount = fields[5..].join(" ");
    if mount == "/proc"
        || mount.starts_with("/proc/")
        || mount == "/sys"
        || mount.starts_with("/sys/")
        || mount == "/dev"
        || mount.starts_with("/dev/")
    {
        return None;
    }
    let total = fields[1].parse::<u64>().ok()?.checked_mul(1024)?;
    let used = fields[2].parse::<u64>().ok()?.checked_mul(1024)?;
    let available = fields[3].parse::<u64>().ok()?.checked_mul(1024)?;
    let percent = fields[4].strip_suffix('%')?.parse::<f64>().ok()?;
    if total == 0 || !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        return None;
    }
    Some(DiskUsage {
        mount_point: mount,
        total_bytes: total,
        used_bytes: used,
        available_bytes: available,
        use_percent: percent,
    })
}

#[cfg(test)]
mod tests;
