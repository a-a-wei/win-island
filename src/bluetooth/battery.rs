use futures_util::future::join_all;
use std::{collections::HashMap, time::Duration};
use windows::{
    Devices::Bluetooth::{
        BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice,
        GenericAttributeProfile::{
            GattCharacteristicUuids, GattCommunicationStatus, GattServiceUuids,
        },
    },
    Storage::Streams::DataReader,
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA,
                SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
                SetupDiGetDevicePropertyW,
            },
            Properties::{
                DEVPKEY_Device_ContainerId, DEVPROP_TYPE_BYTE, DEVPROP_TYPE_GUID,
                DEVPROP_TYPE_UINT32, DEVPROPTYPE,
            },
        },
        Foundation::DEVPROPKEY,
    },
    core::{GUID, w},
};

const BLUETOOTH_BATTERY_LEVEL: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5),
    pid: 2,
};

pub(super) async fn read_connected(devices: &mut [super::Device]) {
    if !devices.iter().any(|device| device.connected) {
        return;
    }
    let levels = windows_levels();
    for device in devices.iter_mut().filter(|device| device.connected) {
        device.battery = device.container.and_then(|id| levels.get(&id).copied());
    }
    let read_ble = join_all(
        devices
            .iter_mut()
            .filter(|device| device.connected && device.low_energy && device.battery.is_none())
            .map(|device| async {
                device.battery = gatt_level(&device.id).await.ok().flatten();
            }),
    );
    smol::future::or(
        async {
            read_ble.await;
        },
        async {
            smol::Timer::after(Duration::from_secs(2)).await;
        },
    )
    .await;
}

struct DeviceInfoSet(HDEVINFO);

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn windows_levels() -> HashMap<GUID, u8> {
    // Windows can report classic headset battery on a child profile node.
    // Join by device container rather than by the localized profile name.
    let mut levels = HashMap::new();
    for enumerator in [
        w!("BTHENUM"),
        w!("BTHHFENUM"),
        w!("BTHLE"),
        w!("BTHLEDEVICE"),
    ] {
        let Ok(set) = (unsafe {
            SetupDiGetClassDevsW(None, enumerator, None, DIGCF_ALLCLASSES | DIGCF_PRESENT)
        }) else {
            continue;
        };
        let set = DeviceInfoSet(set);
        for index in 0.. {
            let mut node = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut node) }.is_err() {
                break;
            }
            let mut level_bytes = [0; 4];
            let level = match property(&set, &node, &BLUETOOTH_BATTERY_LEVEL, &mut level_bytes) {
                Some(DEVPROP_TYPE_BYTE) => u32::from(level_bytes[0]),
                Some(DEVPROP_TYPE_UINT32) => u32::from_le_bytes(level_bytes),
                _ => continue,
            };
            if level > 100 {
                continue;
            }
            let mut container_bytes = [0; 16];
            if property(
                &set,
                &node,
                &DEVPKEY_Device_ContainerId,
                &mut container_bytes,
            ) == Some(DEVPROP_TYPE_GUID)
            {
                // GUID is a fixed-size struct of integers, and the buffer may be unaligned.
                let container = unsafe { container_bytes.as_ptr().cast::<GUID>().read_unaligned() };
                if container != GUID::zeroed() {
                    levels.entry(container).or_insert(level as u8);
                }
            }
        }
    }
    levels
}

fn property(
    set: &DeviceInfoSet,
    node: &SP_DEVINFO_DATA,
    key: &DEVPROPKEY,
    bytes: &mut [u8],
) -> Option<DEVPROPTYPE> {
    let mut kind = DEVPROPTYPE::default();
    unsafe { SetupDiGetDevicePropertyW(set.0, node, key, &mut kind, Some(bytes), None, 0) }.ok()?;
    Some(kind)
}

async fn gatt_level(id: &windows::core::HSTRING) -> windows::core::Result<Option<u8>> {
    let device = BluetoothLEDevice::FromIdAsync(id)?.await?;
    if device.ConnectionStatus()? != BluetoothConnectionStatus::Connected {
        return Ok(None);
    }
    let services = device
        .GetGattServicesForUuidWithCacheModeAsync(
            GattServiceUuids::Battery()?,
            BluetoothCacheMode::Cached,
        )?
        .await?;
    if services.Status()? != GattCommunicationStatus::Success {
        return Ok(None);
    }
    for service in services.Services()? {
        let characteristics = service
            .GetCharacteristicsForUuidWithCacheModeAsync(
                GattCharacteristicUuids::BatteryLevel()?,
                BluetoothCacheMode::Cached,
            )?
            .await?;
        if characteristics.Status()? != GattCommunicationStatus::Success {
            continue;
        }
        for characteristic in characteristics.Characteristics()? {
            let value = characteristic
                .ReadValueWithCacheModeAsync(BluetoothCacheMode::Uncached)?
                .await?;
            if value.Status()? == GattCommunicationStatus::Success {
                let reader = DataReader::FromBuffer(&value.Value()?)?;
                return Ok(Some(reader.ReadByte()?).filter(|level| *level <= 100));
            }
        }
    }
    Ok(None)
}
