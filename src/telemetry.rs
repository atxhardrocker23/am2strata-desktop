use serde::Serialize;
use std::collections::VecDeque;
use std::mem::size_of;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::w;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GlobalMemoryStatusEx, MEMORYSTATUSEX, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::GetSystemTimes;

const HIST_MAX: usize = 60;

#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySnapshot {
    pub cpu_name: Option<String>,
    pub cores: Option<u32>,
    pub threads: u32,
    pub cpu_pct: Option<f64>,
    pub ram_used: u64,
    pub ram_total: u64,
    pub disk_read_mb: f64,
    pub disk_write_mb: f64,
    pub hist_cpu: Vec<Option<f64>>,
    pub hist_ram_used: Vec<Option<u64>>,
    pub hist_disk_read_mb: Vec<Option<f64>>,
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

fn read_cpu_times() -> Option<(u64, u64)> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe {
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?;
    }
    let i = filetime_to_u64(idle);
    let total = filetime_to_u64(kernel).saturating_add(filetime_to_u64(user));
    Some((total.saturating_sub(i), i))
}

fn read_ram_bytes() -> (u64, u64) {
    let mut ms = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe {
        if GlobalMemoryStatusEx(&mut ms).is_ok() {
            let total = ms.ullTotalPhys;
            let used = total.saturating_sub(ms.ullAvailPhys);
            return (used, total);
        }
    }
    (0, 0)
}

fn read_cpu_name() -> Option<String> {
    let mut size: u32 = 0;
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"),
            w!("ProcessorNameString"),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if err.0 != 0 || size < 2 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize / 2) + 1];
    let mut bytes = (buf.len() * 2) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"),
            w!("ProcessorNameString"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        )
    };
    if err.0 != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let name = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn read_physical_cores() -> Option<u32> {
    let mut len: u32 = 0;
    unsafe {
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len);
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let ok = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut len,
        )
        .is_ok()
    };
    if !ok {
        return None;
    }
    let mut offset = 0usize;
    let mut count = 0u32;
    while offset + size_of::<u32>() * 2 <= len as usize {
        let ptr = unsafe { buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX };
        let entry_size = unsafe { (*ptr).Size } as usize;
        if entry_size == 0 {
            break;
        }
        count += 1;
        offset += entry_size;
    }
    if count > 0 {
        Some(count)
    } else {
        None
    }
}

struct DiskPdh {
    query: PDH_HQUERY,
    read_counter: PDH_HCOUNTER,
    write_counter: PDH_HCOUNTER,
}

impl DiskPdh {
    fn new() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        if unsafe { PdhOpenQueryW(None, 0, &mut query) } != 0 {
            return None;
        }
        let mut read_counter = PDH_HCOUNTER::default();
        let mut write_counter = PDH_HCOUNTER::default();
        let r_ok = unsafe {
            PdhAddEnglishCounterW(
                query,
                w!(r"\PhysicalDisk(_Total)\Disk Read Bytes/sec"),
                0,
                &mut read_counter,
            )
        } == 0;
        let w_ok = unsafe {
            PdhAddEnglishCounterW(
                query,
                w!(r"\PhysicalDisk(_Total)\Disk Write Bytes/sec"),
                0,
                &mut write_counter,
            )
        } == 0;
        if !r_ok || !w_ok {
            unsafe {
                let _ = PdhCloseQuery(query);
            }
            return None;
        }
        unsafe {
            let _ = PdhCollectQueryData(query);
        }
        Some(Self {
            query,
            read_counter,
            write_counter,
        })
    }

    fn sample_mb(&self) -> (f64, f64) {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return (0.0, 0.0);
        }
        let read_val = read_pdh_double(self.read_counter);
        let write_val = read_pdh_double(self.write_counter);
        let to_mb = |b: f64| ((b / 1_048_576.0) * 100.0).round() / 100.0;
        (to_mb(read_val), to_mb(write_val))
    }
}

impl Drop for DiskPdh {
    fn drop(&mut self) {
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

fn read_pdh_double(counter: PDH_HCOUNTER) -> f64 {
    let mut val = PDH_FMT_COUNTERVALUE::default();
    let status = unsafe { PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, None, &mut val) };
    if status == 0 {
        let d = unsafe { val.Anonymous.doubleValue };
        if d.is_finite() && d >= 0.0 {
            return d;
        }
    }
    0.0
}

pub fn spawn_sampler<F>(on_sample: F) -> Arc<Mutex<TelemetrySnapshot>>
where
    F: Fn(String) + Send + 'static,
{
    let cpu_name = read_cpu_name();
    let cores = read_physical_cores();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let (ram_used, ram_total) = read_ram_bytes();

    let initial = TelemetrySnapshot {
        cpu_name: cpu_name.clone(),
        cores,
        threads,
        cpu_pct: None,
        ram_used,
        ram_total,
        disk_read_mb: 0.0,
        disk_write_mb: 0.0,
        hist_cpu: Vec::new(),
        hist_ram_used: Vec::new(),
        hist_disk_read_mb: Vec::new(),
    };
    let shared = Arc::new(Mutex::new(initial));
    let shared_bg = shared.clone();

    std::thread::spawn(move || {
        let disk_pdh = DiskPdh::new();
        let mut prev_cpu = read_cpu_times();
        let mut hist_cpu: VecDeque<Option<f64>> = VecDeque::with_capacity(HIST_MAX);
        let mut hist_ram: VecDeque<Option<u64>> = VecDeque::with_capacity(HIST_MAX);
        let mut hist_disk: VecDeque<Option<f64>> = VecDeque::with_capacity(HIST_MAX);

        loop {
            std::thread::sleep(Duration::from_secs(1));

            let now_cpu = read_cpu_times();
            let cpu_pct = match (prev_cpu, now_cpu) {
                (Some((b0, i0)), Some((b1, i1))) => {
                    let db = b1.saturating_sub(b0) as f64;
                    let di = i1.saturating_sub(i0) as f64;
                    if db + di > 0.0 {
                        Some(((db / (db + di)) * 1000.0).round() / 10.0)
                    } else {
                        Some(0.0)
                    }
                }
                _ => None,
            };
            prev_cpu = now_cpu;

            let (ram_used, ram_total) = read_ram_bytes();
            let (disk_read_mb, disk_write_mb) =
                disk_pdh.as_ref().map(|d| d.sample_mb()).unwrap_or((0.0, 0.0));

            if hist_cpu.len() >= HIST_MAX {
                hist_cpu.pop_front();
            }
            if hist_ram.len() >= HIST_MAX {
                hist_ram.pop_front();
            }
            if hist_disk.len() >= HIST_MAX {
                hist_disk.pop_front();
            }
            hist_cpu.push_back(cpu_pct);
            hist_ram.push_back(if ram_total > 0 { Some(ram_used) } else { None });
            hist_disk.push_back(Some(disk_read_mb));

            let snap = TelemetrySnapshot {
                cpu_name: cpu_name.clone(),
                cores,
                threads,
                cpu_pct,
                ram_used,
                ram_total,
                disk_read_mb,
                disk_write_mb,
                hist_cpu: hist_cpu.iter().cloned().collect(),
                hist_ram_used: hist_ram.iter().cloned().collect(),
                hist_disk_read_mb: hist_disk.iter().cloned().collect(),
            };

            if let Ok(mut guard) = shared_bg.lock() {
                *guard = snap.clone();
            }
            if let Ok(json) = serde_json::to_string(&snap) {
                on_sample(json);
            }
        }
    });

    shared
}
