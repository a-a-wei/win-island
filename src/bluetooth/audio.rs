use anyhow::{Context, Result, bail};
use std::{
    collections::{HashMap, HashSet},
    ptr, thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_ContainerId,
        Media::{
            Audio::{
                DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_UNPLUGGED, IDeviceTopology,
                IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eAll,
            },
            KernelStreaming::{
                IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0, KSPROPERTY_BTAUDIO,
                KSPROPERTY_ONESHOT_DISCONNECT, KSPROPERTY_ONESHOT_RECONNECT,
                KSPROPERTY_TYPE_BASICSUPPORT, KSPROPERTY_TYPE_GET, KSPROPSETID_BtAudio,
            },
        },
        System::{
            Com::{
                CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ,
                StructuredStorage::{PropVariantClear, PropVariantToGUID},
            },
            WinRT::RO_INIT_MULTITHREADED,
        },
    },
    core::{GUID, HSTRING},
};

struct Endpoint {
    adapter: HSTRING,
    container: GUID,
    active: bool,
}

pub(super) fn connection_states() -> Result<HashMap<GUID, bool>> {
    let enumerator = enumerator()?;
    let mut states = HashMap::new();
    for endpoint in endpoints(&enumerator, None)? {
        let active = states.entry(endpoint.container).or_insert(false);
        *active |= endpoint.active;
    }
    Ok(states)
}

pub fn verified_filters() -> Result<(usize, usize)> {
    let enumerator = enumerator()?;
    let mut seen = HashSet::new();
    let mut verified = 0;
    for endpoint in endpoints(&enumerator, None)? {
        if seen.insert(endpoint.adapter.to_string_lossy().to_ascii_lowercase())
            && adapter_control(&enumerator, &endpoint.adapter)
                .and_then(|control| supports_connections(&control))
                .unwrap_or(false)
        {
            verified += 1;
        }
    }
    Ok((verified, seen.len()))
}

pub fn set_connected(container: GUID, connect: bool) -> Result<()> {
    let _apartment = crate::Apartment::enter(RO_INIT_MULTITHREADED)?;
    let enumerator = enumerator()?;
    let targets = endpoints(&enumerator, Some(container))?;
    if targets.is_empty() {
        bail!("未找到此设备的蓝牙音频端点");
    }
    if targets.iter().any(|endpoint| endpoint.active) == connect {
        return Ok(());
    }
    let mut sent = false;
    let mut first_error = None;
    let mut seen = HashSet::new();
    // A headset can expose separate music and hands-free filters. Address each
    // filter once, even if it has both microphone and speaker endpoints.
    for target in targets
        .into_iter()
        .filter(|endpoint| endpoint.active != connect)
    {
        if seen.insert(target.adapter.to_string_lossy().to_ascii_lowercase()) {
            match request(&enumerator, &target.adapter, connect) {
                Ok(()) => sent = true,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
    }
    if !sent {
        return Err(first_error.unwrap_or_else(|| anyhow::anyhow!("找不到可操作的音频端点")))
            .context("Windows 未接受蓝牙音频连接请求");
    }

    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let active = endpoints(&enumerator, Some(container))?
            .iter()
            .any(|endpoint| endpoint.active);
        if active == connect {
            return Ok(());
        }
        if Instant::now() >= deadline {
            if connect {
                bail!("等待连接超时，请确认设备已开机且在附近");
            }
            bail!("Windows 音频端点仍处于连接状态");
        }
        thread::sleep(Duration::from_millis(240));
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .context("无法访问 Windows 音频设备")
}

fn endpoints(enumerator: &IMMDeviceEnumerator, target: Option<GUID>) -> Result<Vec<Endpoint>> {
    // UNPLUGGED is how Windows exposes a paired but disconnected headset.
    let states = DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0);
    let collection = unsafe { enumerator.EnumAudioEndpoints(eAll, states)? };
    let mut endpoints = Vec::new();
    for index in 0..unsafe { collection.GetCount()? } {
        let Ok(device) = (unsafe { collection.Item(index) }) else {
            continue;
        };
        let Ok(container) = container_id(&device) else {
            continue;
        };
        if container == GUID::default() || target.is_some_and(|target| target != container) {
            continue;
        }
        let Ok(adapter) = audio_adapter(&device) else {
            continue;
        };
        let path = adapter.to_string_lossy().to_ascii_lowercase();
        if !path.contains("bthenum#") && !path.contains("bthhfenum#") {
            continue;
        }
        if let Ok(state) = unsafe { device.GetState() } {
            endpoints.push(Endpoint {
                adapter,
                container,
                active: state == DEVICE_STATE_ACTIVE,
            });
        }
    }
    Ok(endpoints)
}

fn container_id(device: &IMMDevice) -> Result<GUID> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        let mut value = store.GetValue(&PKEY_Device_ContainerId)?;
        let id = PropVariantToGUID(&value);
        let _ = PropVariantClear(&mut value);
        Ok(id?)
    }
}

fn audio_adapter(device: &IMMDevice) -> Result<HSTRING> {
    unsafe {
        let topology: IDeviceTopology = device.Activate(CLSCTX_ALL, None)?;
        let id = topology.GetConnector(0)?.GetDeviceIdConnectedTo()?;
        let adapter = HSTRING::from_wide(id.as_wide());
        CoTaskMemFree(Some(id.0.cast()));
        Ok(adapter)
    }
}

fn request(enumerator: &IMMDeviceEnumerator, adapter: &HSTRING, connect: bool) -> Result<()> {
    let property = if connect {
        KSPROPERTY_ONESHOT_RECONNECT
    } else {
        KSPROPERTY_ONESHOT_DISCONNECT
    };
    let request = audio_property(property, KSPROPERTY_TYPE_GET);
    let control = adapter_control(enumerator, adapter)?;
    let mut returned = 0;
    unsafe {
        control.KsProperty(
            &request,
            size_of::<KSIDENTIFIER>() as u32,
            ptr::null_mut(),
            0,
            &mut returned,
        )?;
    }
    Ok(())
}

fn adapter_control(enumerator: &IMMDeviceEnumerator, adapter: &HSTRING) -> Result<IKsControl> {
    unsafe { Ok(enumerator.GetDevice(adapter)?.Activate(CLSCTX_ALL, None)?) }
}

fn supports_connections(control: &IKsControl) -> Result<bool> {
    for id in [KSPROPERTY_ONESHOT_RECONNECT, KSPROPERTY_ONESHOT_DISCONNECT] {
        let request = audio_property(id, KSPROPERTY_TYPE_BASICSUPPORT);
        let mut flags = 0u32;
        let mut returned = 0;
        unsafe {
            control.KsProperty(
                &request,
                size_of::<KSIDENTIFIER>() as u32,
                ptr::from_mut(&mut flags).cast(),
                size_of::<u32>() as u32,
                &mut returned,
            )?;
        }
        if flags & KSPROPERTY_TYPE_GET == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn audio_property(property: KSPROPERTY_BTAUDIO, flags: u32) -> KSIDENTIFIER {
    KSIDENTIFIER {
        Anonymous: KSIDENTIFIER_0 {
            Anonymous: KSIDENTIFIER_0_0 {
                Set: KSPROPSETID_BtAudio,
                Id: property.0 as u32,
                Flags: flags,
            },
        },
    }
}
