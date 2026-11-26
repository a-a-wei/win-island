#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bluetooth;
mod desktop_tools;
mod island;
mod media;
mod monitor;
mod notifications;
mod overlay;
mod preferences;
mod startup;
mod wifi;

use gpui_kit::*;
use island::Island;
use overlay::{IDLE_HEIGHT, IDLE_WIDTH, Overlay};
use windows::{
    Win32::{
        System::WinRT::{
            RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED, RO_INIT_TYPE, RoInitialize,
            RoUninitialize,
        },
        UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW},
    },
    core::{HSTRING, w},
};

struct Apartment;

impl Apartment {
    fn enter(mode: RO_INIT_TYPE) -> windows::core::Result<Self> {
        unsafe { RoInitialize(mode)? };
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

fn main() {
    let probe_requested = std::env::args_os().any(|arg| arg == "--probe");
    let mode = if probe_requested {
        RO_INIT_MULTITHREADED
    } else {
        RO_INIT_SINGLETHREADED
    };
    let _apartment = match Apartment::enter(mode) {
        Ok(apartment) => apartment,
        Err(error) => {
            report(&format!("无法初始化 Windows 服务\n{error}"));
            return;
        }
    };
    if probe_requested {
        probe();
        return;
    }
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(IDLE_WIDTH), px(IDLE_HEIGHT)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            titlebar: None,
            window_background: WindowBackgroundAppearance::Transparent,
            is_resizable: false,
            is_movable: false,
            is_minimizable: false,
            window_min_size: Some(size(px(IDLE_WIDTH), px(IDLE_HEIGHT))),
            ..Default::default()
        };
        let launch = cx.open_window(options, |window, cx| {
            let overlay = Overlay::new(window).unwrap_or_else(|error| {
                report(&format!("无法打开悬浮岛\n{error:#}"));
                std::process::exit(1);
            });
            cx.new(|cx| Island::new(overlay, cx))
        });
        if let Err(error) = launch {
            report(&format!("无法创建灵动岛窗口\n{error:#}"));
            cx.quit();
        }
    });
}

fn probe() {
    let (monitor_request, metrics) = monitor::start();
    let _ = monitor_request.send(());
    if metrics
        .recv_timeout(std::time::Duration::from_secs(5))
        .is_ok()
    {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let _ = monitor_request.send(());
        if let Ok(sample) = metrics.recv_timeout(std::time::Duration::from_secs(5)) {
            println!(
                "System monitor: CPU {:.1}%, memory {} / {} MiB, GPU {}, ↓ {} ↑ {}, {} process groups",
                sample.cpu,
                sample.memory / 1048576,
                sample.total_memory / 1048576,
                sample
                    .gpu
                    .map_or_else(|| "unavailable".into(), |gpu| format!("{:.1}%", gpu.usage)),
                wifi::format_rate(sample.download),
                wifi::format_rate(sample.upload),
                sample.processes.len(),
            );
        }
    }
    match wifi::networks() {
        Ok(networks) => println!(
            "WLAN: {} visible, {} connected",
            networks.len(),
            networks.iter().filter(|network| network.connected).count(),
        ),
        Err(error) => println!("WLAN: {error:#}"),
    }
    let mut wifi_traffic = wifi::TrafficMonitor::default();
    match wifi_traffic.sample() {
        Ok(samples) if samples.is_empty() => println!("WLAN traffic: no active link"),
        Ok(_) => {
            std::thread::sleep(std::time::Duration::from_secs(1));
            match wifi_traffic.sample() {
                Ok(samples) => {
                    for sample in samples {
                        println!("WLAN traffic: {}", sample.summary());
                    }
                }
                Err(error) => println!("WLAN traffic: {error:#}"),
            }
        }
        Err(error) => println!("WLAN traffic: {error:#}"),
    }
    match pollster::block_on(bluetooth::snapshot()) {
        Ok(snapshot) => {
            println!(
                "Bluetooth: adapter {}, {} paired or connected, {} direct audio control",
                if snapshot.radio.is_some() {
                    "available"
                } else {
                    "unavailable"
                },
                snapshot.devices.len(),
                snapshot
                    .devices
                    .iter()
                    .filter(|device| device.audio_container.is_some())
                    .count(),
            );
            for device in snapshot.devices.iter().filter(|device| device.connected) {
                if let Some(level) = device.battery {
                    println!("Bluetooth battery: {level}%");
                } else {
                    println!("Bluetooth battery: unavailable");
                }
            }
        }
        Err(error) => println!("Bluetooth: {error}"),
    }
    match bluetooth::verify_audio_filters() {
        Ok((verified, total)) => println!(
            "Bluetooth audio: {verified}/{total} filters advertise both connection controls"
        ),
        Err(error) => println!("Bluetooth audio: {error:#}"),
    }
    match media::volume() {
        Ok(volume) => println!(
            "Audio endpoint: {}%, muted: {}",
            volume.percent, volume.muted
        ),
        Err(error) => println!("Audio endpoint: {error:#}"),
    }
    match pollster::block_on(async {
        let manager = media::connect().await?;
        media::playback(&manager).await
    }) {
        Ok(Some(playback)) => println!(
            "Media session: available, playing: {}, playback controls: {}",
            playback.playing,
            playback.supports(playback.primary_action()),
        ),
        Ok(None) => println!("Media session: no current session"),
        Err(error) => println!("Media session: {error:#}"),
    }
    println!(
        "Notification package identity: {}",
        notifications::has_package_identity()
    );
    match notifications::Listener::new() {
        Ok(listener) => match pollster::block_on(notifications::read(&listener.handle)) {
            Ok(notices) => println!(
                "Notification listener: {} authorized Windows notifications",
                notices.len()
            ),
            Err(error) => println!("Notification listener: {error:#}"),
        },
        Err(error) => println!("Notification listener: {error:#}"),
    }
}

fn report(message: &str) {
    eprintln!("{message}");
    let message = HSTRING::from(message);
    unsafe { MessageBoxW(None, &message, w!("灵动岛"), MB_ICONERROR) };
}
