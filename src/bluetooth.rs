use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use windows::{
    Devices::{
        Bluetooth::{
            BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice,
            GenericAttributeProfile::{GattSession, GattSessionStatus},
        },
        Enumeration::{DeviceInformation, DeviceInformationCollection, DeviceInformationKind},
        Radios::{Radio, RadioAccessStatus, RadioKind, RadioState},
    },
    Foundation::IPropertyValue,
    core::{GUID, HSTRING, Interface},
};
use windows_collections::IIterable;

mod audio;
mod battery;
pub use audio::set_connected as set_audio_connected;
pub use audio::verified_filters as verify_audio_filters;

#[derive(Clone)]
pub struct Device {
    pub id: HSTRING,
    pub name: String,
    pub low_energy: bool,
    pub connected: bool,
    pub audio_container: Option<GUID>,
    pub battery: Option<u8>,
    pub container: Option<GUID>,
}

pub struct Snapshot {
    pub radio: Option<Radio>,
    pub devices: Vec<Device>,
}

pub async fn snapshot() -> Result<Snapshot> {
    let radio = Radio::GetRadiosAsync()?
        .await?
        .into_iter()
        .find(|radio| radio.Kind() == Ok(RadioKind::Bluetooth));
    let mut devices = Vec::new();
    if radio
        .as_ref()
        .is_none_or(|radio| radio.State() == Ok(RadioState::On))
    {
        let low_energy = read_devices(
            BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)?,
            BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(
                BluetoothConnectionStatus::Connected,
            )?,
            true,
        )
        .await;
        let classic = read_devices(
            BluetoothDevice::GetDeviceSelectorFromPairingState(true)?,
            BluetoothDevice::GetDeviceSelectorFromConnectionStatus(
                BluetoothConnectionStatus::Connected,
            )?,
            false,
        )
        .await;
        match (low_energy, classic) {
            (Ok(ble), Ok(classic)) => devices = ble.into_iter().chain(classic).collect(),
            (Ok(partial), Err(_)) | (Err(_), Ok(partial)) => devices = partial,
            (Err(error), Err(_)) => return Err(error),
        }
    }
    devices.sort_by(|left, right| {
        (!left.connected, &left.name, !left.low_energy).cmp(&(
            !right.connected,
            &right.name,
            !right.low_energy,
        ))
    });
    battery::read_connected(&mut devices).await;
    Ok(Snapshot { radio, devices })
}

async fn read_devices(
    paired: HSTRING,
    connected: HSTRING,
    low_energy: bool,
) -> Result<Vec<Device>> {
    let container_key = HSTRING::from("System.Devices.Aep.ContainerId");
    let properties = vec![container_key.clone()];
    let connected = find(&connected, properties.clone()).await?;
    let paired = find(&paired, properties).await?;
    let active: HashSet<_> = connected
        .clone()
        .into_iter()
        .filter_map(|item| item.Id().ok())
        .collect();
    let mut seen = HashSet::new();
    let audio_states = if low_energy {
        Default::default()
    } else {
        audio::connection_states().unwrap_or_default()
    };
    Ok(paired
        .into_iter()
        .chain(connected)
        .filter_map(|item| {
            let id = item.Id().ok()?;
            if !seen.insert(id.clone()) {
                return None;
            }
            let name = item.Name().ok()?.to_string_lossy();
            let container = item
                .Properties()
                .ok()
                .and_then(|values| values.Lookup(&container_key).ok())
                .and_then(|value| value.cast::<IPropertyValue>().ok())
                .and_then(|value| value.GetGuid().ok());
            let audio = container.and_then(|id| audio_states.get(&id).map(|state| (id, *state)));
            Some(Device {
                connected: audio.map_or_else(|| active.contains(&id), |(_, connected)| connected),
                audio_container: audio.map(|(id, _)| id),
                battery: None,
                container,
                id,
                name: if name.is_empty() {
                    "未命名设备".into()
                } else {
                    name
                },
                low_energy,
            })
        })
        .collect())
}

async fn find(selector: &HSTRING, keys: Vec<HSTRING>) -> Result<DeviceInformationCollection> {
    let properties = IIterable::<HSTRING>::from(keys);
    Ok(
        DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(
            selector,
            &properties,
            DeviceInformationKind::AssociationEndpoint,
        )?
        .await?,
    )
}

pub async fn ble_connected(id: &HSTRING) -> Result<bool> {
    let selector = BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(
        BluetoothConnectionStatus::Connected,
    )?;
    Ok(find(&selector, Vec::new())
        .await?
        .into_iter()
        .any(|device| device.Id().as_ref() == Ok(id)))
}

pub async fn toggle_radio(radio: Radio) -> Result<()> {
    let state = match radio.State()? {
        RadioState::On => RadioState::Off,
        RadioState::Off => RadioState::On,
        _ => bail!("蓝牙适配器当前无法切换状态"),
    };
    if Radio::RequestAccessAsync()?.await? != RadioAccessStatus::Allowed {
        bail!("Windows 未授予蓝牙开关权限，请在系统设置中切换");
    }
    if radio.SetStateAsync(state)?.await? != RadioAccessStatus::Allowed {
        bail!("Windows 未能切换蓝牙状态");
    }
    Ok(())
}

pub struct Connection {
    device: BluetoothLEDevice,
    session: GattSession,
}

impl Connection {
    pub async fn open(id: &HSTRING) -> Result<Self> {
        let device = BluetoothLEDevice::FromIdAsync(id)?
            .await
            .context("无法访问此蓝牙低功耗设备")?;
        let session = GattSession::FromDeviceIdAsync(&device.BluetoothDeviceId()?)?.await?;
        if !session.CanMaintainConnection()? {
            bail!("此设备不支持应用维持低功耗连接");
        }
        session.SetMaintainConnection(true)?;
        Ok(Self { device, session })
    }

    pub fn active(&self) -> bool {
        self.session.SessionStatus() == Ok(GattSessionStatus::Active)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.session.SetMaintainConnection(false);
        let _ = self.session.Close();
        let _ = self.device.Close();
    }
}
