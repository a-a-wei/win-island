use std::{
    collections::HashMap,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::Instant,
};
use sysinfo::{Networks, ProcessRefreshKind, ProcessesToUpdate, System};
use windows::{
    Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW,
    },
    core::{PCWSTR, w},
};

#[derive(Default)]
pub struct Process {
    pub name: String,
    pub cpu: f32,
    pub memory: u64,
    pub instances: usize,
}

pub struct Sample {
    pub cpu: f32,
    pub memory: u64,
    pub total_memory: u64,
    pub download: u64,
    pub upload: u64,
    pub gpu: Option<Gpu>,
    pub processes: Vec<Process>,
}

#[derive(Clone, Copy, Default)]
pub struct Gpu {
    pub usage: f32,
    pub engines: [f32; 4],
}

pub fn start() -> (SyncSender<()>, Receiver<Sample>) {
    let (request, work) = mpsc::sync_channel(1);
    let (output, events) = mpsc::channel();
    thread::spawn(move || {
        let mut sampler = None;
        while work.recv().is_ok() {
            if output
                .send(sampler.get_or_insert_with(Sampler::new).sample())
                .is_err()
            {
                break;
            }
        }
    });
    (request, events)
}

struct Sampler {
    system: System,
    networks: Networks,
    previous: Instant,
    gpu: Option<GpuCounters>,
}

impl Sampler {
    fn new() -> Self {
        Self {
            system: System::new(),
            networks: Networks::new_with_refreshed_list(),
            previous: Instant::now(),
            gpu: GpuCounters::new(),
        }
    }

    fn sample(&mut self) -> Sample {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            ProcessRefreshKind::new().with_cpu().with_memory(),
        );
        self.networks.refresh_list();
        let now = Instant::now();
        let seconds = now.duration_since(self.previous).as_secs_f64().max(0.2);
        self.previous = now;
        let (down, up) = self
            .networks
            .values()
            .fold((0_u64, 0_u64), |(down, up), adapter| {
                (
                    down.saturating_add(adapter.received()),
                    up.saturating_add(adapter.transmitted()),
                )
            });
        let cpu_count = self.system.cpus().len().max(1) as f32;
        let mut processes = HashMap::new();
        for process in self.system.processes().values() {
            let name = process.name().to_string_lossy();
            if name.is_empty() {
                continue;
            }
            let entry: &mut Process = processes.entry(name.into_owned()).or_default();
            entry.instances += 1;
            entry.cpu += process.cpu_usage() / cpu_count;
            entry.memory = entry.memory.saturating_add(process.memory());
        }
        let processes = processes
            .into_iter()
            .map(|(name, mut process)| {
                process.name = name;
                process
            })
            .collect();
        Sample {
            cpu: self.system.global_cpu_usage(),
            memory: self.system.used_memory(),
            total_memory: self.system.total_memory(),
            download: (down as f64 / seconds) as u64,
            upload: (up as f64 / seconds) as u64,
            gpu: self.gpu.as_ref().and_then(GpuCounters::sample),
            processes,
        }
    }
}

struct GpuCounters {
    query: PDH_HQUERY,
    engines: PDH_HCOUNTER,
}

impl GpuCounters {
    fn new() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut counters = Self {
                query,
                engines: PDH_HCOUNTER::default(),
            };
            if PdhAddEnglishCounterW(
                query,
                w!(r"\GPU Engine(*)\Utilization Percentage"),
                0,
                &mut counters.engines,
            ) != 0
            {
                return None;
            }
            PdhCollectQueryData(query);
            Some(counters)
        }
    }

    fn sample(&self) -> Option<Gpu> {
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let mut bytes = 0;
            let mut count = 0;
            if PdhGetFormattedCounterArrayW(
                self.engines,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                None,
            ) != PDH_MORE_DATA
            {
                return None;
            }
            // PDH returns fixed-size entries followed by their UTF-16 names.
            let mut buffer = vec![0_u64; (bytes as usize).div_ceil(size_of::<u64>())];
            let entries = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            if PdhGetFormattedCounterArrayW(
                self.engines,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                Some(entries),
            ) != 0
                || count as usize
                    > buffer.len() * size_of::<u64>() / size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()
            {
                return None;
            }
            let mut by_engine = HashMap::<String, f32>::new();
            for entry in std::slice::from_raw_parts(entries, count as usize) {
                if entry.FmtValue.CStatus > 1 || entry.szName.is_null() {
                    continue;
                }
                let Ok(name) = entry.szName.to_string() else {
                    continue;
                };
                let Some((_, engine)) = name.split_once("_luid_") else {
                    continue;
                };
                let usage = entry.FmtValue.Anonymous.doubleValue as f32;
                if usage.is_finite() && usage >= 0.0 {
                    *by_engine.entry(engine.to_owned()).or_default() += usage;
                }
            }
            if by_engine.is_empty() {
                return None;
            }
            let mut gpu = Gpu::default();
            for (engine, value) in by_engine {
                let value = value.min(100.0);
                gpu.usage = gpu.usage.max(value);
                if let Some(index) = [
                    "engtype_3D",
                    "engtype_Copy",
                    "engtype_VideoDecode",
                    "engtype_VideoEncode",
                ]
                .iter()
                .position(|kind| engine.ends_with(kind))
                {
                    gpu.engines[index] = gpu.engines[index].max(value);
                }
            }
            Some(gpu)
        }
    }
}

impl Drop for GpuCounters {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}
