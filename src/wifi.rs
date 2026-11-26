use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    ffi::c_void,
    ptr::{self, NonNull},
    slice,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HANDLE, WIN32_ERROR},
        NetworkManagement::{
            IpHelper::{ConvertInterfaceGuidToLuid, GetIfEntry2, MIB_IF_ROW2},
            WiFi::*,
        },
    },
    core::{GUID, PCWSTR},
};

#[derive(Clone)]
pub struct Network {
    adapter: GUID,
    ssid: DOT11_SSID,
    bss_type: DOT11_BSS_TYPE,
    profile: String,
    pub name: String,
    pub quality: u32,
    pub connected: bool,
    pub secure: bool,
    pub connectable: bool,
}

impl Network {
    pub fn needs_credentials(&self) -> bool {
        self.secure && self.profile.is_empty() && !self.connected
    }

    pub fn traffic<'a>(&self, samples: &'a [Traffic]) -> Option<&'a Traffic> {
        samples
            .iter()
            .find(|sample| sample.adapter == self.adapter && sample.ssid == ssid_bytes(&self.ssid))
    }
}

#[derive(PartialEq, Eq)]
pub struct Traffic {
    adapter: GUID,
    ssid: Vec<u8>,
    pub name: String,
    pub rate: Option<(u64, u64)>,
}

impl Traffic {
    pub fn summary(&self) -> String {
        self.rate.map_or_else(
            || "等待网速数据…".into(),
            |(down, up)| format!("↓ {}   ↑ {}", format_rate(down), format_rate(up)),
        )
    }
}

pub(crate) fn format_rate(bytes: u64) -> String {
    match bytes {
        0..1000 => format!("{bytes} B/s"),
        1000..1_000_000 => format!("{:.1} KB/s", bytes as f64 / 1_000.0),
        1_000_000..1_000_000_000 => format!("{:.1} MB/s", bytes as f64 / 1_000_000.0),
        _ => format!("{:.1} GB/s", bytes as f64 / 1_000_000_000.0),
    }
}

#[derive(Default)]
pub struct TrafficMonitor {
    previous: HashMap<GUID, (Vec<u8>, Instant, u64, u64)>,
}

impl TrafficMonitor {
    pub fn sample(&mut self) -> Result<Vec<Traffic>> {
        let previous = std::mem::take(&mut self.previous);
        let client = Client::open()?;
        let interfaces = client.interfaces()?;
        let mut traffic = Vec::new();
        for interface in interfaces.entries() {
            if interface.isState != wlan_interface_state_connected {
                continue;
            }
            let adapter = interface.InterfaceGuid;
            let Ok(Some(connection)) = client.current(&adapter) else {
                continue;
            };
            let ssid = &connection.get().wlanAssociationAttributes.dot11Ssid;
            let mut row = MIB_IF_ROW2::default();
            let rate = if unsafe { ConvertInterfaceGuidToLuid(&adapter, &mut row.InterfaceLuid) }
                .is_ok()
                && unsafe { GetIfEntry2(&mut row) }.is_ok()
            {
                let now = Instant::now();
                let rate = previous
                    .get(&adapter)
                    .and_then(|(old_ssid, then, received, sent)| {
                        let elapsed = now.duration_since(*then).as_secs_f64();
                        if old_ssid != ssid_bytes(ssid) || elapsed == 0.0 {
                            return None;
                        }
                        let down = row.InOctets.checked_sub(*received)?;
                        let up = row.OutOctets.checked_sub(*sent)?;
                        Some((
                            (down as f64 / elapsed).round() as u64,
                            (up as f64 / elapsed).round() as u64,
                        ))
                    });
                self.previous.insert(
                    adapter,
                    (ssid_bytes(ssid).to_vec(), now, row.InOctets, row.OutOctets),
                );
                rate
            } else {
                None
            };
            traffic.push(Traffic {
                adapter,
                ssid: ssid_bytes(ssid).to_vec(),
                name: ssid_name(ssid),
                rate,
            });
        }
        Ok(traffic)
    }
}

pub enum Command {
    Scan,
    Toggle(Network),
}

pub enum Event {
    Networks(Result<Vec<Network>>),
    Finished(Result<String>),
    Traffic(Vec<Traffic>),
}

pub fn start() -> (Sender<Command>, Receiver<Event>) {
    let (commands, work) = mpsc::channel();
    let (output, events) = mpsc::channel();
    let traffic_output = output.clone();
    thread::spawn(move || {
        let mut monitor = TrafficMonitor::default();
        loop {
            if traffic_output
                .send(Event::Traffic(monitor.sample().unwrap_or_default()))
                .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
    thread::spawn(move || {
        loop {
            if output.send(Event::Networks(networks())).is_err() {
                break;
            }
            match work.recv_timeout(Duration::from_secs(8)) {
                Ok(Command::Scan) => {
                    let result = scan().map(|()| "已刷新可用网络".to_owned());
                    if output.send(Event::Finished(result)).is_err() {
                        break;
                    }
                }
                Ok(Command::Toggle(network)) => {
                    let result = toggle(&network);
                    if output.send(Event::Finished(result)).is_err() {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    (commands, events)
}

struct Client(HANDLE);

impl Client {
    fn open() -> Result<Self> {
        let mut version = 0;
        let mut handle = HANDLE::default();
        check(unsafe { WlanOpenHandle(2, None, &mut version, &mut handle) })
            .context("无法打开 Windows WLAN 服务")?;
        Ok(Self(handle))
    }

    fn interfaces(&self) -> Result<WlanMemory<WLAN_INTERFACE_INFO_LIST>> {
        let mut list = ptr::null_mut();
        check(unsafe { WlanEnumInterfaces(self.0, None, &mut list) })?;
        WlanMemory::new(list)
    }

    fn available(&self, adapter: &GUID) -> Result<WlanMemory<WLAN_AVAILABLE_NETWORK_LIST>> {
        let mut list = ptr::null_mut();
        check(unsafe { WlanGetAvailableNetworkList(self.0, adapter, 0, None, &mut list) })?;
        WlanMemory::new(list)
    }

    fn current(&self, adapter: &GUID) -> Result<Option<WlanMemory<WLAN_CONNECTION_ATTRIBUTES>>> {
        let mut size = 0;
        let mut data: *mut c_void = ptr::null_mut();
        let status = unsafe {
            WlanQueryInterface(
                self.0,
                adapter,
                wlan_intf_opcode_current_connection,
                None,
                &mut size,
                &mut data,
                None,
            )
        };
        // ERROR_INVALID_STATE and ERROR_NOT_CONNECTED mean the interface has no active link.
        if matches!(status, 5023 | 2250) {
            return Ok(None);
        }
        check(status)?;
        let data = WlanMemory::new(data.cast())?;
        if (size as usize) < size_of::<WLAN_CONNECTION_ATTRIBUTES>() {
            bail!("Windows 返回了不完整的 WLAN 连接信息");
        }
        Ok(Some(data))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe { WlanCloseHandle(self.0, None) };
    }
}

struct WlanMemory<T>(NonNull<T>);

impl<T> WlanMemory<T> {
    fn new(ptr: *mut T) -> Result<Self> {
        NonNull::new(ptr)
            .map(Self)
            .context("Windows 未返回 WLAN 数据")
    }

    fn get(&self) -> &T {
        // The WLAN API owns this allocation until WlanFreeMemory.
        unsafe { self.0.as_ref() }
    }
}

impl WlanMemory<WLAN_INTERFACE_INFO_LIST> {
    fn entries(&self) -> &[WLAN_INTERFACE_INFO] {
        let list = self.0.as_ptr();
        // The API allocates the documented trailing array with dwNumberOfItems entries.
        unsafe {
            slice::from_raw_parts(
                ptr::addr_of!((*list).InterfaceInfo).cast(),
                (*list).dwNumberOfItems as usize,
            )
        }
    }
}

impl WlanMemory<WLAN_AVAILABLE_NETWORK_LIST> {
    fn entries(&self) -> &[WLAN_AVAILABLE_NETWORK] {
        let list = self.0.as_ptr();
        unsafe {
            slice::from_raw_parts(
                ptr::addr_of!((*list).Network).cast(),
                (*list).dwNumberOfItems as usize,
            )
        }
    }
}

impl<T> Drop for WlanMemory<T> {
    fn drop(&mut self) {
        unsafe { WlanFreeMemory(self.0.as_ptr().cast()) };
    }
}

pub fn networks() -> Result<Vec<Network>> {
    let client = Client::open()?;
    let interfaces = client.interfaces()?;
    if interfaces.entries().is_empty() {
        bail!("未找到可用的 Wi‑Fi 网卡");
    }
    let mut networks = Vec::new();
    let mut scan_error = None;
    for adapter in interfaces.entries() {
        let id = adapter.InterfaceGuid;
        let first = networks.len();
        match client.available(&id) {
            Ok(list) => {
                networks.extend(list.entries().iter().map(|network| Network {
                    adapter: id,
                    ssid: network.dot11Ssid,
                    bss_type: network.dot11BssType,
                    profile: wide_name(&network.strProfileName),
                    name: ssid_name(&network.dot11Ssid),
                    quality: network.wlanSignalQuality,
                    connected: network.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    secure: network.bSecurityEnabled.as_bool(),
                    connectable: network.bNetworkConnectable.as_bool(),
                }));
            }
            Err(error) => scan_error = Some(error),
        }
        if adapter.isState == wlan_interface_state_connected
            && !networks[first..].iter().any(|network| network.connected)
            && let Ok(Some(connection)) = client.current(&id)
        {
            let connection = connection.get();
            let association = &connection.wlanAssociationAttributes;
            networks.push(Network {
                adapter: id,
                ssid: association.dot11Ssid,
                bss_type: association.dot11BssType,
                profile: wide_name(&connection.strProfileName),
                name: ssid_name(&association.dot11Ssid),
                quality: association.wlanSignalQuality,
                connected: true,
                secure: connection.wlanSecurityAttributes.bSecurityEnabled.as_bool(),
                connectable: true,
            });
        }
    }
    if networks.is_empty()
        && let Some(error) = scan_error
    {
        return Err(error.context("无法读取可用的 Wi‑Fi 网络"));
    }
    networks.retain(|network| network.connected || !network.name.is_empty());
    networks.sort_by_key(|network| (!network.connected, std::cmp::Reverse(network.quality)));
    Ok(networks)
}

fn scan() -> Result<()> {
    let client = Client::open()?;
    let interfaces = client.interfaces()?;
    let mut result = Err(anyhow::anyhow!("未找到可扫描的 Wi‑Fi 网卡"));
    for adapter in interfaces.entries() {
        match check(unsafe { WlanScan(client.0, &adapter.InterfaceGuid, None, None, None) }) {
            Ok(()) => result = Ok(()),
            Err(error) if result.is_err() => result = Err(error),
            Err(_) => {}
        }
    }
    result.context("Wi‑Fi 扫描失败")?;
    // WLAN scanning is asynchronous. Refresh the cached list after the scan window.
    thread::sleep(Duration::from_secs(4));
    Ok(())
}

fn toggle(network: &Network) -> Result<String> {
    let client = Client::open()?;
    if network.connected {
        let current = client.current(&network.adapter)?;
        if !current
            .as_ref()
            .is_some_and(|active| matches_network(active.get(), network))
        {
            bail!("当前连接已变化，请刷新后重试");
        }
        check(unsafe { WlanDisconnect(client.0, &network.adapter, None) })
            .context("Windows 未能断开 Wi‑Fi")?;
        wait_for_link(&client, network, false)?;
        return Ok(format!("已断开 {}", network.name));
    }
    if !network.connectable || network.needs_credentials() {
        bail!("请从 Windows 网络面板连接此网络");
    }
    let profile: Vec<u16> = network.profile.encode_utf16().chain([0]).collect();
    let mut ssid = network.ssid;
    let parameters = if network.profile.is_empty() {
        WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_discovery_unsecure,
            pDot11Ssid: &mut ssid,
            dot11BssType: network.bss_type,
            ..Default::default()
        }
    } else {
        WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(profile.as_ptr()),
            dot11BssType: dot11_BSS_type_any,
            ..Default::default()
        }
    };
    check(unsafe { WlanConnect(client.0, &network.adapter, &parameters, None) })
        .context("Windows 未能发起 Wi‑Fi 连接")?;
    wait_for_link(&client, network, true)?;
    Ok(format!("已连接 {}", network.name))
}

fn matches_network(connection: &WLAN_CONNECTION_ATTRIBUTES, network: &Network) -> bool {
    ssid_bytes(&connection.wlanAssociationAttributes.dot11Ssid) == ssid_bytes(&network.ssid)
        && (network.profile.is_empty() || wide_name(&connection.strProfileName) == network.profile)
}

fn wait_for_link(client: &Client, network: &Network, connected: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(14);
    loop {
        let current = client.current(&network.adapter)?;
        if current
            .as_ref()
            .is_some_and(|active| matches_network(active.get(), network))
            == connected
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("等待 Windows 更新连接状态超时，请检查网络后重试");
        }
        thread::sleep(Duration::from_millis(350));
    }
}

fn ssid_bytes(ssid: &DOT11_SSID) -> &[u8] {
    &ssid.ucSSID[..(ssid.uSSIDLength as usize).min(ssid.ucSSID.len())]
}

fn ssid_name(ssid: &DOT11_SSID) -> String {
    String::from_utf8_lossy(ssid_bytes(ssid)).into_owned()
}

fn wide_name(name: &[u16]) -> String {
    let end = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end])
}

fn check(status: u32) -> Result<()> {
    WIN32_ERROR(status).ok()?;
    Ok(())
}
