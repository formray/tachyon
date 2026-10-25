//! One compact sample per sampler tick, shared by the flight recorder, the bottleneck
//! explainer, the VRAM forecast and the job history. Built from what the sampler already
//! reads, so it costs no extra driver calls.
use crate::monitor::ProcView;
use crate::nvml::GpuSample;
use crate::sysinfo::{CpuLoad, MemInfo};
use serde::{Deserialize, Serialize};

/// Throttle-reason bits (NVML clock event reasons) the analysis cares about.
pub const THR_POWER: u32 = 0x4 | 0x80;
pub const THR_THERMAL: u32 = 0x20 | 0x40 | 0x8;

/// A GPU process in a sample (the top few only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Proc {
    pub pid: u32,
    /// Executable name only, never a path.
    pub name: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_mb: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sm: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enc: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dec: Option<u8>,
}

/// Everything known at one instant. Missing values are None (the driver or OS did not say).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Sample {
    /// Unix time, milliseconds.
    pub t_ms: i64,
    pub util: Option<u8>,
    pub mem_ctrl: Option<u8>,
    pub clk_sm: Option<u16>,
    pub clk_sm_max: Option<u16>,
    pub clk_mem: Option<u16>,
    pub temp: Option<u8>,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub vram_mb: Option<f32>,
    pub vram_total_mb: Option<f32>,
    pub throttle: Option<u32>,
    pub pcie_rx: Option<f32>,
    pub pcie_tx: Option<f32>,
    /// Estimated one-direction PCIe capacity of the current link, MB/s.
    pub pcie_max: Option<f32>,
    pub enc: Option<u8>,
    pub dec: Option<u8>,
    pub fan: Option<u8>,
    pub cpu: Option<f32>,
    pub cpu_core_max: Option<f32>,
    pub cores: Option<u16>,
    pub ram_used_mb: Option<f32>,
    pub ram_total_mb: Option<f32>,
    pub commit_mb: Option<f32>,
    pub commit_limit_mb: Option<f32>,
    /// NVML total energy counter, millijoules.
    pub energy_mj: Option<u64>,
    pub procs: Vec<Proc>,
}

/// Usable one-direction PCIe bandwidth per lane in MB/s for a link generation.
pub fn pcie_lane_mbs(gen_: u32) -> Option<f32> {
    Some(match gen_ {
        1 => 250.0,
        2 => 500.0,
        3 => 985.0,
        4 => 1969.0,
        5 => 3938.0,
        6 => 7563.0,
        _ => return None,
    })
}

fn u8c(v: Option<u32>) -> Option<u8> {
    v.map(|x| x.min(255) as u8)
}
fn u16c(v: Option<u32>) -> Option<u16> {
    v.map(|x| x.min(65535) as u16)
}

impl Sample {
    /// Build from one sampler tick. `procs` are attributed processes; the top `keep` by VRAM
    /// (then SM) are kept.
    pub fn from_tick(
        t_ms: i64,
        g: Option<&GpuSample>,
        procs: Option<&[ProcView]>,
        cpu: Option<CpuLoad>,
        mem: Option<MemInfo>,
        keep: usize,
    ) -> Self {
        let mut s = Sample { t_ms, ..Default::default() };
        if let Some(g) = g {
            s.util = u8c(g.util_pct);
            s.mem_ctrl = u8c(g.mem_ctrl_pct);
            s.clk_sm = u16c(g.clk_gfx_mhz);
            s.clk_sm_max = u16c(g.clk_gfx_max_mhz);
            s.clk_mem = u16c(g.clk_mem_mhz);
            s.temp = u8c(g.temp_c);
            s.power_w = g.power_w.map(|v| v as f32);
            s.power_limit_w = g.power_limit_w.map(|v| v as f32);
            s.vram_mb = g.vram_used_mb.map(|v| v as f32);
            s.vram_total_mb = g.vram_total_mb.map(|v| v as f32);
            s.throttle = g.throttle.map(|v| v as u32);
            s.pcie_rx = g.pcie_rx_mbs.map(|v| v as f32);
            s.pcie_tx = g.pcie_tx_mbs.map(|v| v as f32);
            s.pcie_max = match (g.pcie_gen.and_then(pcie_lane_mbs), g.pcie_width) {
                (Some(l), Some(w)) if w > 0 => Some(l * w as f32),
                _ => None,
            };
            s.enc = u8c(g.enc_pct);
            s.dec = u8c(g.dec_pct);
            s.fan = u8c(g.fan_pct);
            s.energy_mj = g.energy_mj;
        }
        if let Some(c) = cpu {
            s.cpu = Some(c.total);
            s.cpu_core_max = Some(c.max_core);
            s.cores = Some(c.cores);
        }
        if let Some(m) = mem {
            s.ram_used_mb = Some(m.ram_used_mb);
            s.ram_total_mb = Some(m.ram_total_mb);
            s.commit_mb = Some(m.commit_mb);
            s.commit_limit_mb = Some(m.commit_limit_mb);
        }
        if let Some(list) = procs {
            let mut v: Vec<&ProcView> = list.iter().collect();
            v.sort_by(|a, b| {
                let ka = (a.info.vram_mb.unwrap_or(0.0), a.info.sm_pct.unwrap_or(0));
                let kb = (b.info.vram_mb.unwrap_or(0.0), b.info.sm_pct.unwrap_or(0));
                kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
            });
            s.procs = v
                .into_iter()
                .take(keep)
                .map(|p| Proc {
                    pid: p.info.pid,
                    name: p.info.name.clone(),
                    category: p.category.clone(),
                    vram_mb: p.info.vram_mb.map(|v| v as f32),
                    sm: u8c(p.info.sm_pct),
                    enc: u8c(p.info.enc_pct),
                    dec: u8c(p.info.dec_pct),
                })
                .collect();
        }
        s
    }

    pub fn power_throttled(&self) -> bool {
        self.throttle.is_some_and(|t| t & THR_POWER != 0)
    }
    pub fn thermal_throttled(&self) -> bool {
        self.throttle.is_some_and(|t| t & THR_THERMAL != 0)
    }
    /// VRAM used as a fraction of total.
    pub fn vram_frac(&self) -> Option<f32> {
        match (self.vram_mb, self.vram_total_mb) {
            (Some(u), Some(t)) if t > 0.0 => Some(u / t),
            _ => None,
        }
    }
    /// Local time of the sample.
    pub fn local(&self) -> chrono::DateTime<chrono::Local> {
        chrono::DateTime::from_timestamp_millis(self.t_ms).unwrap_or_default().with_timezone(&chrono::Local)
    }
}

/// Unix time now in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcie_capacity_from_link() {
        let g = GpuSample { pcie_gen: Some(4), pcie_width: Some(16), util_pct: Some(300), ..Default::default() };
        let s = Sample::from_tick(0, Some(&g), None, None, None, 3);
        assert_eq!(s.pcie_max, Some(1969.0 * 16.0));
        assert_eq!(s.util, Some(255), "clamped into u8");
        assert_eq!(pcie_lane_mbs(9), None);
    }
}
