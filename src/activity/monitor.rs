//! The machine's vital signs, sampled in the background every second. CPU, GPU, memory, network
//! and disk traffic are always sampled, so their graphs have history and the status bar can show
//! them. The rest (processes, ports, battery and the like) only while an Activity tab is shown.

use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    path::PathBuf,
    process::Command,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gpui_kit::*;
use objc2_core_foundation::{CFRetained, CFString};
use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};
use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, Uid, UpdateKind,
    Users,
};

use super::registry::{self, Entry};

/// Two minutes of history.
pub const HISTORY: usize = 120;
const INTERVAL: Duration = Duration::from_secs(1);
/// Ports change rarely and take a while to ask about, so they're sampled less often.
const PORTS_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Default)]
pub struct Sample {
    /// Percent of all cores.
    pub cpu: f32,
    /// Percent of the time the GPU was busy.
    pub gpu: f32,
    /// Percent of physical memory.
    pub memory: f32,
    /// Bytes per second.
    pub received: u64,
    pub transmitted: u64,
    pub read: u64,
    pub written: u64,
}

#[derive(Clone)]
pub struct Process {
    pub pid: u32,
    pub name: SharedString,
    pub user: SharedString,
    pub cpu: f32,
    pub gpu: f32,
    pub memory: u64,
    /// Bytes read and written per second.
    pub disk: u64,
    pub command: SharedString,
    pub exe: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Port {
    pub port: u16,
    pub address: SharedString,
    pub pid: u32,
    pub process: SharedString,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Pressure {
    #[default]
    Normal,
    Warning,
    Critical,
}

/// How hard the machine is working to keep cool.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum Thermal {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Clone, Copy)]
pub struct Battery {
    pub percent: i64,
    pub charging: bool,
}

/// What's sampled only while someone's looking.
#[derive(Default)]
pub struct Details {
    pub processes: Vec<Process>,
    pub ports: Vec<Port>,
    pub pressure: Pressure,
    pub thermal: Thermal,
    pub battery: Option<Battery>,
}

/// What's read each second besides the sample, of which only the latest is kept.
#[derive(Default)]
pub struct Readings {
    /// Percent of each core.
    pub cores: Vec<f32>,
    /// Used and total bytes.
    pub memory: (u64, u64),
    pub swap: (u64, u64),
    /// Bytes of memory the GPU is using.
    pub gpu_memory: u64,
    pub load: [f64; 3],
    pub uptime: u64,
}

/// What the sampling thread reports each tick.
struct Report {
    sample: Sample,
    now: Readings,
    details: Option<Details>,
}

pub struct Monitor {
    pub host: SharedString,
    pub os: SharedString,
    pub cpu_brand: SharedString,
    pub gpu_cores: Option<i64>,
    /// Oldest first, always `HISTORY` long.
    pub history: VecDeque<Sample>,
    pub now: Readings,
    pub details: Details,
    watchers: Arc<AtomicUsize>,
    _task: Task<()>,
}

struct GlobalMonitor(Entity<Monitor>);

impl Global for GlobalMonitor {}

/// Keeps the detailed sampling going while it's alive.
pub struct Watch(Arc<AtomicUsize>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn init(cx: &mut App) {
    let monitor = cx.new(Monitor::new);
    cx.set_global(GlobalMonitor(monitor));
}

impl Monitor {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalMonitor>().0.clone()
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let watchers = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = smol::channel::bounded(2);
        let thread_watchers = watchers.clone();
        std::thread::Builder::new()
            .name("monitor".into())
            .spawn(move || sample(thread_watchers, tx))
            .expect("the monitor thread starts");
        let task = cx.spawn(async move |this, cx| {
            while let Ok(report) = rx.recv().await {
                if this.update(cx, |this, cx| this.apply(report, cx)).is_err() {
                    break;
                }
            }
        });
        let cpu_brand = System::new_with_specifics(RefreshKind::new().with_cpu(CpuRefreshKind::new()))
            .cpus()
            .first()
            .map(|cpu| cpu.brand().to_string())
            .unwrap_or_default();
        Self {
            host: computer_name().unwrap_or_default().into(),
            os: System::long_os_version().unwrap_or_default().replace("MacOS", "macOS").into(),
            cpu_brand: cpu_brand.into(),
            gpu_cores: registry::services(c"IOAccelerator").first().and_then(|gpu| gpu.get("gpu-core-count")?.int()),
            history: VecDeque::from(vec![Sample::default(); HISTORY]),
            now: Readings::default(),
            details: Details::default(),
            watchers,
            _task: task,
        }
    }

    /// Starts sampling the details until the returned guard drops.
    pub fn watch(&self) -> Watch {
        self.watchers.fetch_add(1, Ordering::Relaxed);
        Watch(self.watchers.clone())
    }

    pub fn latest(&self) -> Sample {
        self.history[HISTORY - 1]
    }

    fn apply(&mut self, report: Report, cx: &mut Context<Self>) {
        self.history.pop_front();
        self.history.push_back(report.sample);
        self.now = report.now;
        if let Some(details) = report.details {
            self.details = details;
        }
        // Nothing shows the samples while Mu is in the background, unless an Activity tab does.
        // Coming back to Mu draws the windows again, with what was sampled meanwhile.
        if cx.active_window().is_some() || self.watchers.load(Ordering::Relaxed) > 0 {
            cx.notify();
        }
    }
}

/// The sampling loop, on its own thread so a slow `lsof` never stalls the UI.
fn sample(watchers: Arc<AtomicUsize>, tx: smol::channel::Sender<Report>) {
    let mut system = System::new_with_specifics(
        RefreshKind::new()
            .with_cpu(CpuRefreshKind::new().with_cpu_usage())
            .with_memory(MemoryRefreshKind::everything()),
    );
    let gpu = registry::services(c"IOAccelerator").into_iter().next();
    let mut disk = disk_bytes();
    let mut network = network_bytes().unwrap_or_default();
    let mut last = Instant::now();
    // Made once the details are first watched.
    let mut processes: Option<Processes> = None;
    // The ports, and when they were asked about.
    let mut ports: Option<(Instant, Vec<Port>)> = None;
    loop {
        std::thread::sleep(INTERVAL);
        let elapsed = last.elapsed().as_secs_f64().max(0.001);
        last = Instant::now();
        system.refresh_cpu_usage();
        system.refresh_memory();
        let network_before = network;
        network = network_bytes().unwrap_or(network);
        let disk_before = std::mem::replace(&mut disk, disk_bytes());
        let statistics = gpu.as_ref().and_then(|gpu| gpu.get("PerformanceStatistics"));
        let statistic = |key| statistics.as_ref().and_then(|statistics| statistics.get(key)?.int()).unwrap_or(0);
        let total = system.total_memory().max(1);
        let load = System::load_average();
        let details = (watchers.load(Ordering::Relaxed) > 0).then(|| {
            ports.take_if(|(at, _)| at.elapsed() >= PORTS_INTERVAL);
            Details {
                processes: processes.get_or_insert_with(Processes::new).sample(&mut system, gpu.as_ref()),
                ports: ports.get_or_insert_with(|| (Instant::now(), listening_ports())).1.clone(),
                pressure: memory_pressure(),
                thermal: thermal(),
                battery: battery(),
            }
        });
        let rate = |bytes: u64| (bytes as f64 / elapsed) as u64;
        let report = Report {
            sample: Sample {
                cpu: system.global_cpu_usage(),
                gpu: statistic("Device Utilization %") as f32,
                memory: system.used_memory() as f32 / total as f32 * 100.,
                // A network interface or disk that's gone takes its count with it, which isn't
                // negative traffic.
                received: rate(network.0.saturating_sub(network_before.0)),
                transmitted: rate(network.1.saturating_sub(network_before.1)),
                read: rate(disk.0.saturating_sub(disk_before.0)),
                written: rate(disk.1.saturating_sub(disk_before.1)),
            },
            now: Readings {
                cores: system.cpus().iter().map(|cpu| cpu.cpu_usage()).collect(),
                memory: (system.used_memory(), system.total_memory()),
                swap: (system.used_swap(), system.total_swap()),
                gpu_memory: statistic("In use system memory") as u64,
                load: [load.one, load.five, load.fifteen],
                uptime: System::uptime(),
            },
            details,
        };
        if tx.send_blocking(report).is_err() {
            return;
        }
    }
}

/// Samples the processes, keeping what's needed to tell how much each used since the last time.
struct Processes {
    /// Each user's name, by uid.
    names: HashMap<Uid, SharedString>,
    /// Each process's GPU nanoseconds and disk bytes so far.
    used: HashMap<u32, (u64, u64)>,
    last: Instant,
}

impl Processes {
    fn new() -> Self {
        let users = Users::new_with_refreshed_list();
        let names = users.iter().map(|user| (user.id().clone(), user.name().to_string().into())).collect();
        Self { names, used: HashMap::new(), last: Instant::now() }
    }

    /// The processes running, with what they used since the last time.
    fn sample(&mut self, system: &mut System, gpu: Option<&Entry>) -> Vec<Process> {
        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            ProcessRefreshKind::new()
                .with_cpu()
                .with_memory()
                .with_disk_usage()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        let gpu_times = gpu.map(gpu_times).unwrap_or_default();
        let mut used = HashMap::new();
        let processes = system
            .processes()
            .values()
            .map(|process| {
                let pid = process.pid().as_u32();
                let disk = process.disk_usage();
                let so_far =
                    (gpu_times.get(&pid).copied().unwrap_or(0), disk.total_read_bytes + disk.total_written_bytes);
                // A process seen for the first time hasn't used anything yet as far as this knows.
                let (gpu_before, disk_before) = self.used.get(&pid).copied().unwrap_or(so_far);
                used.insert(pid, so_far);
                Process {
                    pid,
                    name: process.name().to_string_lossy().into_owned().into(),
                    user: process.user_id().and_then(|uid| self.names.get(uid)).cloned().unwrap_or_default(),
                    cpu: process.cpu_usage(),
                    gpu: (so_far.0.saturating_sub(gpu_before) as f64 / (elapsed * 1e9) * 100.) as f32,
                    memory: process.memory(),
                    disk: (so_far.1.saturating_sub(disk_before) as f64 / elapsed) as u64,
                    command: process.cmd().iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>().join(" ").into(),
                    exe: process.exe().map(PathBuf::from),
                }
            })
            .collect();
        self.used = used;
        processes
    }
}

/// Bytes read from and written to all disks since they were attached.
fn disk_bytes() -> (u64, u64) {
    registry::services(c"IOBlockStorageDriver").iter().filter_map(|driver| driver.get("Statistics")).fold(
        (0, 0),
        |(read, written), statistics| {
            let bytes = |key| statistics.get(key).and_then(|value| value.int()).unwrap_or(0) as u64;
            (read + bytes("Bytes (Read)"), written + bytes("Bytes (Write)"))
        },
    )
}

/// Bytes received and sent on all network interfaces but loopback since they came up, read in one
/// go. (Asking each interface by name, as sysinfo does, costs a lookup of all of them per interface.)
fn network_bytes() -> Option<(u64, u64)> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut sysctl = |buffer: *mut c_void, len: &mut usize| unsafe {
        libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, buffer, len, ptr::null_mut(), 0) == 0
    };
    let mut len = 0;
    if !sysctl(ptr::null_mut(), &mut len) {
        return None;
    }
    let mut buffer = vec![0u8; len];
    // Fails if an interface came up since it said how much room it needs.
    if !sysctl(buffer.as_mut_ptr().cast(), &mut len) {
        return None;
    }
    let (mut received, mut sent, mut offset) = (0, 0, 0);
    // Each message starts with its length and type, and interfaces' are `if_msghdr2`s.
    while offset + 4 <= len {
        let message = &buffer[offset..len];
        if message[3] as i32 == libc::RTM_IFINFO2 {
            let info = &message[..size_of::<libc::if_msghdr2>()];
            let info = unsafe { ptr::read_unaligned(info.as_ptr().cast::<libc::if_msghdr2>()) };
            if info.ifm_flags & libc::IFF_LOOPBACK == 0 {
                received += info.ifm_data.ifi_ibytes;
                sent += info.ifm_data.ifi_obytes;
            }
        }
        offset += u16::from_ne_bytes([message[0], message[1]]) as usize;
    }
    Some((received, sent))
}

/// The Mac's name, as set in System Settings. Its host name is up to the network, which may call it
/// something like dhcp-10-0-0-1.example.com.
pub fn computer_name() -> Option<String> {
    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" {
        fn SCDynamicStoreCopyComputerName(store: *const c_void, encoding: *mut u32) -> Option<NonNull<CFString>>;
    }
    let name = unsafe { SCDynamicStoreCopyComputerName(ptr::null(), ptr::null_mut()) }?;
    Some(unsafe { CFRetained::from_raw(name) }.to_string())
}

/// How many nanoseconds of GPU time each process has used, by pid. Each connection a process makes
/// to the GPU is a child of it that counts its own.
fn gpu_times(gpu: &Entry) -> HashMap<u32, u64> {
    let mut times = HashMap::new();
    for client in gpu.children() {
        // Like "pid 173, WindowServer".
        let creator = client.get("IOUserClientCreator").and_then(|creator| creator.string());
        let Some(pid) =
            creator.as_deref().and_then(|creator| creator.strip_prefix("pid ")?.split(',').next()?.parse().ok())
        else {
            continue;
        };
        let usage = client.get("AppUsage").map(|usage| usage.items()).unwrap_or_default();
        let time: i64 = usage.iter().filter_map(|app| app.get("accumulatedGPUTime")?.int()).sum();
        *times.entry(pid).or_default() += time as u64;
    }
    times
}

fn thermal() -> Thermal {
    match NSProcessInfo::processInfo().thermalState() {
        NSProcessInfoThermalState::Fair => Thermal::Fair,
        NSProcessInfoThermalState::Serious => Thermal::Serious,
        NSProcessInfoThermalState::Critical => Thermal::Critical,
        _ => Thermal::Nominal,
    }
}

fn memory_pressure() -> Pressure {
    let (mut level, mut size) = (0i32, size_of::<i32>());
    let name = c"kern.memorystatus_vm_pressure_level";
    unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut level).cast(), &mut size, std::ptr::null_mut(), 0) };
    match level {
        4 => Pressure::Critical,
        2 => Pressure::Warning,
        _ => Pressure::Normal,
    }
}

fn battery() -> Option<Battery> {
    let battery = registry::services(c"AppleSmartBattery").into_iter().next()?;
    if !battery.get("BatteryInstalled")?.bool()? {
        return None;
    }
    let number = |key| battery.get(key)?.int();
    Some(Battery {
        // A percent already on Apple silicon, and milliamp hours on Intel.
        percent: number("CurrentCapacity")? * 100 / number("MaxCapacity")?.max(1),
        charging: battery.get("IsCharging").and_then(|charging| charging.bool()).unwrap_or(false),
    })
}

/// TCP ports something is listening on, from `lsof` (only your own processes without root).
fn listening_ports() -> Vec<Port> {
    match Command::new("/usr/sbin/lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"]).output() {
        Ok(output) => parse_ports(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => Vec::new(),
    }
}

/// The ports in what `lsof -F pcn` printed, a field to a line, each tagged by its first letter: a
/// process's pid and command, then the address of each file it has open.
fn parse_ports(lsof: &str) -> Vec<Port> {
    let mut ports: Vec<Port> = Vec::new();
    let (mut pid, mut process) = (0, SharedString::default());
    for line in lsof.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = value.parse().unwrap_or(0),
            "c" => process = value.to_string().into(),
            "n" => {
                let Some((address, port)) = value.rsplit_once(':') else { continue };
                let Ok(port) = port.parse() else { continue };
                // IPv4 and IPv6 listeners on one port are one entry.
                if ports.iter().any(|p| p.pid == pid && p.port == port) {
                    continue;
                }
                let address = match address {
                    "*" | "[::]" => "All interfaces".to_string(),
                    other => other.trim_matches(['[', ']']).to_string(),
                };
                ports.push(Port { port, address: address.into(), pid, process: process.clone() });
            }
            _ => {}
        }
    }
    ports.sort_by_key(|port| port.port);
    ports
}

/// Sends `signal` to `pid`. Pid 0 (the kernel, or a port whose owner `lsof` didn't say) is turned
/// down: to `kill` it means Mu's own process group.
pub fn signal(pid: u32, signal: i32) -> std::io::Result<()> {
    if pid == 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    if unsafe { libc::kill(pid as i32, signal) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(test)]
mod tests {
    use super::parse_ports;

    #[test]
    fn reads_listening_ports_from_lsof() {
        // IPv4 and IPv6 listeners on one port are one entry, and the ports come out in order.
        let lsof = "p88\ncpostgres\nf7\nn127.0.0.1:5432\nf8\nn[::1]:5433\n\
                    p501\ncnode\nf21\nn*:3000\nf22\nn[::]:3000\n";
        let ports: Vec<_> = parse_ports(lsof)
            .into_iter()
            .map(|port| (port.port, port.address.to_string(), port.pid, port.process.to_string()))
            .collect();
        assert_eq!(
            ports,
            [
                (3000, "All interfaces".into(), 501, "node".into()),
                (5432, "127.0.0.1".into(), 88, "postgres".into()),
                (5433, "::1".into(), 88, "postgres".into()),
            ]
        );
    }
}
