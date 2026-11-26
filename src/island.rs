use crate::{
    bluetooth,
    desktop_tools::{ClipboardHistory, DesktopLists, file_drag::FileDrop},
    monitor,
    notifications::{self, NoticeKey},
    overlay::{FRAME_INTERVAL, Frame, Layout as OverlayLayout, Overlay},
    preferences::{self, Preferences, Preview, PreviewMode},
    startup, wifi,
};
use gpui_kit::{prelude::FluentBuilder, *};
use std::{
    collections::{HashMap, HashSet},
    process::Command,
    sync::mpsc::Sender,
    time::{Duration, Instant},
};
use windows::{Devices::Radios::RadioState, core::HSTRING};

mod media_panel;
mod monitor_panel;
mod preferences_panel;
mod utility_panels;
use media_panel::MediaPanel;
use monitor_panel::MonitorPanel;

const SURFACE: u32 = 0x111a20;
const CARD: u32 = 0x1b2931;
const BORDER: u32 = 0x2d414b;
const INK: u32 = 0xf1fbfb;
const MUTED: u32 = 0xa6b9c1;
const ACCENT: u32 = 0x84e6c4;
const WARNING: u32 = 0xffc689;
const TAB_WIDTH: f32 = 94.0;
const TAB_SLIDE_TIME: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Notices,
    Media,
    Wifi,
    Bluetooth,
    Clipboard,
    Files,
    Launcher,
    Monitor,
}

impl Tab {
    const ALL: [Self; 8] = [
        Self::Notices,
        Self::Media,
        Self::Wifi,
        Self::Bluetooth,
        Self::Clipboard,
        Self::Files,
        Self::Launcher,
        Self::Monitor,
    ];

    fn bit(self) -> u16 {
        1 << self as u16
    }

    fn name(self) -> &'static str {
        match self {
            Self::Notices => "通知",
            Self::Media => "媒体",
            Self::Wifi => "Wi‑Fi",
            Self::Bluetooth => "蓝牙",
            Self::Clipboard => "剪贴板",
            Self::Files => "文件中转",
            Self::Launcher => "快捷启动",
            Self::Monitor => "系统监控",
        }
    }
}

enum NoticeState {
    Request,
    Denied,
    Loading,
    Active,
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoticeAction {
    OpenFromList(NoticeKey),
    OpenFromPreview(NoticeKey),
    Remove(NoticeKey),
    Clear,
}

pub struct Island {
    frame: Frame,
    tab: Tab,
    tab_slide: Option<TabSlide>,
    preferences: Preferences,
    preferences_open: bool,
    preferences_error: Option<String>,
    startup_state: Result<startup::State, String>,
    listener: Option<notifications::Listener>,
    notice_state: NoticeState,
    notices: Vec<notifications::Notice>,
    notice_busy: bool,
    notice_revision: u64,
    notice_action: Option<NoticeAction>,
    notice_error: Option<String>,
    collapse_after_launch: bool,
    media: MediaPanel,
    clipboard: ClipboardHistory,
    desktop_lists: DesktopLists,
    desktop_status: Option<String>,
    dragging_files: bool,
    monitor: MonitorPanel,
    wifi_sender: Sender<wifi::Command>,
    networks: Vec<wifi::Network>,
    wifi_traffic: Vec<wifi::Traffic>,
    wifi_busy: bool,
    wifi_status: String,
    wifi_error: bool,
    bluetooth: Option<bluetooth::Snapshot>,
    ble_sessions: HashMap<HSTRING, bluetooth::Connection>,
    bluetooth_loading: bool,
    bluetooth_busy: bool,
    radio_status: String,
    radio_error: bool,
}

struct TabSlide {
    from: f32,
    to: f32,
    started: Instant,
}

fn nearest_tab_position(position: f32, tab_index: usize, count: usize) -> f32 {
    let index = tab_index as f32;
    let cycle = count.max(1) as f32;
    index + ((position - index) / cycle).round() * cycle
}

#[cfg(test)]
mod tab_tests {
    use super::nearest_tab_position;

    #[test]
    fn adjacent_slots_cross_the_start_and_end_of_the_tab_cycle() {
        assert_eq!(nearest_tab_position(7.0, 0, 8), 8.0);
        assert_eq!(nearest_tab_position(0.0, 7, 8), -1.0);
        assert_eq!(nearest_tab_position(-0.6, 6, 8), -2.0);
    }
}

impl Island {
    fn tab_visible(&self, tab: Tab) -> bool {
        self.preferences.visible_tabs & tab.bit() != 0
    }

    fn visible_tabs(&self) -> impl Iterator<Item = Tab> + '_ {
        Tab::ALL.into_iter().filter(|tab| self.tab_visible(*tab))
    }

    fn tab_position(&self, now: Instant) -> f32 {
        self.tab_slide.as_ref().map_or_else(
            || self.selected_tab_index() as f32,
            |slide| {
                let progress = (now.saturating_duration_since(slide.started).as_secs_f32()
                    / TAB_SLIDE_TIME.as_secs_f32())
                .min(1.0);
                slide.to + (slide.from - slide.to) * (1.0 - progress).powi(3)
            },
        )
    }

    fn selected_tab_index(&self) -> usize {
        self.visible_tabs()
            .position(|tab| tab == self.tab)
            .unwrap_or(0)
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let now = Instant::now();
        let position = self.tab_position(now);
        let index = self
            .visible_tabs()
            .position(|visible| visible == tab)
            .unwrap_or(0);
        self.select_tab_at(
            tab,
            nearest_tab_position(position, index, self.visible_tabs().count()),
            cx,
        );
    }

    fn select_tab_at(&mut self, tab: Tab, to: f32, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        let now = Instant::now();
        self.tab_slide = self.preferences.animate.then(|| TabSlide {
            from: self.tab_position(now),
            to,
            started: now,
        });
        self.tab = tab;
        self.media.dragging = false;
        match tab {
            Tab::Bluetooth => self.refresh_bluetooth(cx),
            Tab::Media => self.refresh_media(cx),
            _ => {}
        }
        cx.notify();
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let visible: Vec<_> = self.visible_tabs().collect();
        let position = self.tab_position(Instant::now());
        let active_slot = self
            .tab_slide
            .as_ref()
            .map_or_else(|| self.selected_tab_index() as f32, |slide| slide.to)
            as isize;
        let center = position.floor() as isize;
        let slots = match visible.len() {
            0 => 0..0,
            1 => 0..1,
            _ => center - 4..center + 5,
        };
        let left = slots.start as f32 - position - 0.5;
        let notice_count = self.notices.len();
        div()
            .id("tab-bar")
            .relative()
            .h(px(37.0))
            .flex_shrink_0()
            .overflow_hidden()
            .rounded_lg()
            .bg(rgb(0x0c151a))
            .on_scroll(cx.listener(|view, event: &ScrollEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(1.0));
                let movement = if delta.y != px(0.0) { delta.y } else { delta.x };
                if movement == px(0.0) {
                    return;
                }
                cx.stop_propagation();
                let tabs: Vec<_> = view.visible_tabs().collect();
                if tabs.is_empty() {
                    return;
                }
                let current = tabs.iter().position(|tab| *tab == view.tab);
                if let Some(index) = current {
                    let step = if movement < px(0.0) { 1 } else { -1 };
                    let next = (index as isize + step).rem_euclid(tabs.len() as isize) as usize;
                    let target = view
                        .tab_slide
                        .as_ref()
                        .map_or(index as f32, |slide| slide.to);
                    view.select_tab_at(tabs[next], target + step as f32, cx);
                } else {
                    view.select_tab(tabs[0], cx);
                }
            }))
            .child(
                div()
                    .absolute()
                    .top(px(3.0))
                    .left(relative(0.5))
                    .ml(px(TAB_WIDTH * left))
                    .flex()
                    .children(slots.map(|slot| {
                        let tab = visible[slot.rem_euclid(visible.len() as isize) as usize];
                        let selected = self.tab == tab && slot == active_slot;
                        let label = if tab == Tab::Notices && notice_count > 0 {
                            format!("通知 {notice_count}")
                        } else {
                            tab.name().into()
                        };
                        div()
                            .id(("tab-slot", slot as usize))
                            .w(px(TAB_WIDTH))
                            .flex_shrink_0()
                            .cursor_pointer()
                            .rounded_md()
                            .py_1p5()
                            .text_center()
                            .text_sm()
                            .text_color(rgb(if selected { ACCENT } else { MUTED }))
                            .when(selected, |item| item.bg(rgb(CARD)))
                            .hover(|style| style.bg(rgb(CARD)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.select_tab_at(tab, slot as f32, cx)
                            }))
                            .child(label)
                    })),
            )
    }

    pub fn new(mut overlay: Overlay, cx: &mut Context<Self>) -> Self {
        let (wifi_sender, wifi_events) = wifi::start();
        let (monitor_request, monitor_events) = monitor::start();
        let (desktop_lists, mut desktop_status) = DesktopLists::load().map_or_else(
            |error| (DesktopLists::default(), Some(error.to_string())),
            |lists| (lists, None),
        );
        let file_drops = match overlay.register_file_drop() {
            Ok(events) => Some(events),
            Err(error) => {
                desktop_status = Some(format!("{error:#}"));
                None
            }
        };
        let (preferences, preferences_error) = match preferences::load() {
            Ok(preferences) => (preferences, None),
            Err(error) => (Preferences::default(), Some(error.to_string())),
        };
        let (listener, notice_state) = match notifications::Listener::new() {
            Ok(listener) => {
                let state = match listener.handle.GetAccessStatus() {
                    Ok(notifications::Access::Allowed) => NoticeState::Loading,
                    Ok(notifications::Access::Denied) => NoticeState::Denied,
                    Ok(_) => NoticeState::Request,
                    Err(error) => NoticeState::Error(error.to_string()),
                };
                (Some(listener), state)
            }
            Err(error) => (None, NoticeState::Error(error.to_string())),
        };
        cx.spawn(async move |this, cx| {
            // SetWindowPos synchronously delivers WM_SIZE. Leave the app available
            // for GPUI's bounds callback instead of resizing from an entity update.
            overlay.show();
            let mut notice_tick = Instant::now();
            let mut bluetooth_tick = Instant::now();
            let mut media_tick = Instant::now();
            let mut clipboard_tick = Instant::now();
            let mut monitor_tick = Instant::now();
            let mut collapsed_layout = OverlayLayout::Idle;
            let mut expanded_layout = OverlayLayout::Expanded;
            let mut dragging_files = false;
            let mut preview_clickable = false;
            let mut current_preferences = preferences;
            loop {
                let now = Instant::now();
                let frame = overlay.tick(
                    now,
                    collapsed_layout,
                    preview_clickable,
                    expanded_layout,
                    dragging_files,
                    current_preferences,
                );
                let running = this.update(cx, |view, cx| {
                    let just_expanded = frame.expanded && !view.frame.expanded;
                    let mut redraw = view.frame != frame;
                    view.frame = frame;
                    if !frame.expanded {
                        view.media.dragging = false;
                    }
                    for event in file_drops.iter().flat_map(|events| events.try_iter()) {
                        match event {
                            FileDrop::Entered => {
                                view.dragging_files = true;
                                view.preferences_open = false;
                                view.select_tab(Tab::Files, cx);
                            }
                            FileDrop::Left => view.dragging_files = false,
                            FileDrop::Submitted(paths) => {
                                view.dragging_files = false;
                                view.preferences_open = false;
                                view.select_tab(Tab::Files, cx);
                                view.add_desktop_paths(paths, false, cx);
                            }
                        }
                        redraw = true;
                    }
                    if view
                        .tab_slide
                        .as_ref()
                        .is_some_and(|slide| now.duration_since(slide.started) >= TAB_SLIDE_TIME)
                    {
                        view.tab_slide = None;
                        redraw = true;
                    } else if view.tab_slide.is_some() {
                        redraw = true;
                    }
                    if now >= clipboard_tick {
                        clipboard_tick = now + Duration::from_millis(300);
                        if view
                            .clipboard
                            .sample(cx, view.preferences.clipboard_max_items)
                            && view.tab == Tab::Clipboard
                        {
                            redraw = true;
                        }
                    }
                    if now >= monitor_tick
                        && frame.expanded
                        && view.tab == Tab::Monitor
                        && !view.preferences_open
                    {
                        monitor_tick = now + Duration::from_secs(1);
                        let _ = monitor_request.try_send(());
                    }
                    for sample in monitor_events.try_iter() {
                        view.monitor.push(sample);
                        redraw |= view.tab == Tab::Monitor && frame.expanded;
                    }
                    for event in wifi_events.try_iter() {
                        view.apply_wifi(event);
                        redraw = true;
                    }
                    let changed = view
                        .listener
                        .as_ref()
                        .is_some_and(|listener| listener.take_change());
                    if changed || now >= notice_tick {
                        notice_tick = now + Duration::from_secs(3);
                        view.refresh_notices(cx);
                    }
                    if (now >= bluetooth_tick || just_expanded)
                        && ((view.tab == Tab::Bluetooth
                            && ((!view.preferences_open && frame.expanded)
                                || (!frame.expanded
                                    && view.preferences.bluetooth_preview != PreviewMode::Hidden)))
                            || (frame.expanded && !view.ble_sessions.is_empty()))
                    {
                        bluetooth_tick =
                            now + Duration::from_secs(if frame.expanded { 5 } else { 10 });
                        view.refresh_bluetooth(cx);
                    }
                    if now >= media_tick
                        && view.tab == Tab::Media
                        && ((!view.preferences_open && frame.expanded)
                            || (!frame.expanded
                                && view.preferences.media_preview != PreviewMode::Hidden))
                    {
                        media_tick = now + Duration::from_millis(750);
                        view.refresh_media(cx);
                    }
                    if redraw {
                        cx.notify();
                    }
                    (
                        view.preferences,
                        view.collapsed_layout(),
                        if view.tab == Tab::Monitor && !view.preferences_open {
                            OverlayLayout::Monitor
                        } else {
                            OverlayLayout::Expanded
                        },
                        view.dragging_files,
                        view.preferences.preview == Preview::Details
                            && view.preview_notice().is_some(),
                        std::mem::take(&mut view.collapse_after_launch),
                    )
                });
                let Ok((selected, preview, expanded, dragging, clickable, collapse)) = running
                else {
                    break;
                };
                current_preferences = selected;
                collapsed_layout = preview;
                expanded_layout = expanded;
                dragging_files = dragging;
                preview_clickable = clickable;
                if collapse {
                    overlay.collapse_until_pointer_leaves();
                }
                cx.background_executor().timer(FRAME_INTERVAL).await;
            }
        })
        .detach();

        Self {
            frame: Frame::default(),
            tab: Tab::ALL
                .into_iter()
                .find(|tab| preferences.visible_tabs & tab.bit() != 0)
                .unwrap_or(Tab::Notices),
            tab_slide: None,
            preferences,
            preferences_open: false,
            preferences_error,
            startup_state: startup::state().map_err(|error| error.to_string()),
            listener,
            notice_state,
            notices: Vec::new(),
            notice_busy: false,
            notice_revision: 0,
            notice_action: None,
            notice_error: None,
            collapse_after_launch: false,
            media: MediaPanel::default(),
            clipboard: ClipboardHistory::default(),
            desktop_lists,
            desktop_status,
            dragging_files: false,
            monitor: MonitorPanel::default(),
            wifi_sender,
            networks: Vec::new(),
            wifi_traffic: Vec::new(),
            wifi_busy: false,
            wifi_status: "正在读取可用网络…".into(),
            wifi_error: false,
            bluetooth: None,
            ble_sessions: HashMap::new(),
            bluetooth_loading: false,
            bluetooth_busy: false,
            radio_status: String::new(),
            radio_error: false,
        }
    }

    fn refresh_notices(&mut self, cx: &mut Context<Self>) {
        let Some(listener) = &mut self.listener else {
            return;
        };
        if self.notice_busy {
            return;
        }
        match listener.handle.GetAccessStatus() {
            Ok(notifications::Access::Allowed) => listener.subscribe(),
            Ok(status) => {
                let next = if status == notifications::Access::Denied {
                    NoticeState::Denied
                } else {
                    NoticeState::Request
                };
                if std::mem::discriminant(&next) != std::mem::discriminant(&self.notice_state) {
                    self.notice_state = next;
                    self.notices.clear();
                    cx.notify();
                }
                return;
            }
            Err(error) => {
                self.notice_state = NoticeState::Error(error.to_string());
                self.notices.clear();
                cx.notify();
                return;
            }
        }
        let handle = listener.handle.clone();
        let revision = self.notice_revision;
        self.notice_busy = true;
        cx.spawn(async move |this, cx| {
            let result = notifications::read(&handle).await;
            let _ = this.update(cx, |view, cx| {
                view.notice_busy = false;
                if revision != view.notice_revision {
                    view.refresh_notices(cx);
                    return;
                }
                match result {
                    Ok(notices) => {
                        view.notices = notices;
                        view.notice_state = NoticeState::Active;
                    }
                    Err(error) => {
                        view.notices.clear();
                        view.notice_state = NoticeState::Error(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_notice_action(&mut self, action: NoticeAction, cx: &mut Context<Self>) {
        if self.notice_action.is_some() || !matches!(self.notice_state, NoticeState::Active) {
            return;
        }
        let (key, app_id) = match action {
            NoticeAction::OpenFromList(key)
            | NoticeAction::OpenFromPreview(key)
            | NoticeAction::Remove(key) => {
                let Some(notice) = self.notices.iter().find(|notice| notice.key == key) else {
                    return;
                };
                (Some(key), notice.app_id.clone())
            }
            NoticeAction::Clear => (None, None),
        };
        self.notice_action = Some(action);
        self.notice_error = None;
        let clear_keys: Vec<_> = if action == NoticeAction::Clear {
            self.notices.iter().map(|notice| notice.key).collect()
        } else {
            Vec::new()
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let from_list = matches!(action, NoticeAction::OpenFromList(_));
            let launch = if from_list
                || (matches!(action, NoticeAction::OpenFromPreview(_)) && app_id.is_some())
            {
                Some(
                    cx.background_executor()
                        .spawn(async move {
                            let Some(app_id) = app_id else {
                                anyhow::bail!("这条通知没有可打开的来源应用");
                            };
                            notifications::open_app(&app_id)
                        })
                        .await,
                )
            } else {
                None
            };
            let launch_error = launch
                .as_ref()
                .and_then(|result| result.as_ref().err())
                .map(|error| format!("打开应用失败 · {error}"));
            if from_list && launch_error.is_some() {
                let _ = this.update(cx, |view, cx| {
                    view.notice_action = None;
                    view.notice_error = launch_error;
                    cx.notify();
                });
                return;
            }
            let keys_to_remove = clear_keys.clone();
            let removal = cx
                .background_executor()
                .spawn(async move {
                    match key {
                        Some(key) => notifications::remove(key),
                        None => notifications::clear(&keys_to_remove),
                    }
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.notice_action = None;
                view.notice_error = launch_error;
                match removal {
                    Ok(()) => {
                        view.notice_revision += 1;
                        match key {
                            Some(key) => view.notices.retain(|notice| notice.key != key),
                            None => view
                                .notices
                                .retain(|notice| !clear_keys.contains(&notice.key)),
                        }
                        view.collapse_after_launch |= launch.is_some_and(|result| result.is_ok());
                    }
                    Err(error) => {
                        let message = format!("删除通知失败 · {error}");
                        view.notice_error = Some(match view.notice_error.take() {
                            Some(previous) => format!("{previous}\n{message}"),
                            None => message,
                        });
                    }
                }
                view.refresh_notices(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn request_notices(&mut self, cx: &mut Context<Self>) {
        if self.notice_busy {
            return;
        }
        if self.listener.is_none() {
            match notifications::Listener::new() {
                Ok(listener) => self.listener = Some(listener),
                Err(error) => {
                    self.notice_state = NoticeState::Error(error.to_string());
                    cx.notify();
                    return;
                }
            }
        }
        let Some(listener) = &self.listener else {
            return;
        };
        if listener.handle.GetAccessStatus() == Ok(notifications::Access::Allowed) {
            self.refresh_notices(cx);
            return;
        }
        let request = match listener.handle.RequestAccessAsync() {
            Ok(request) => request,
            Err(error) => {
                self.notice_state = NoticeState::Error(error.to_string());
                cx.notify();
                return;
            }
        };
        self.notice_busy = true;
        self.notice_state = NoticeState::Loading;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let status = request.await;
            let _ = this.update(cx, |view, cx| {
                view.notice_busy = false;
                view.notice_state = match status {
                    Ok(notifications::Access::Allowed) => {
                        if let Some(listener) = &mut view.listener {
                            listener.subscribe();
                        }
                        view.refresh_notices(cx);
                        NoticeState::Loading
                    }
                    Ok(notifications::Access::Denied) => NoticeState::Denied,
                    Ok(_) => NoticeState::Request,
                    Err(error) => NoticeState::Error(error.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_wifi(&mut self, event: wifi::Event) {
        match event {
            wifi::Event::Traffic(traffic) => self.wifi_traffic = traffic,
            wifi::Event::Networks(Ok(networks)) => {
                self.networks = networks;
                if self.wifi_status == "正在读取可用网络…" {
                    self.wifi_status.clear();
                }
            }
            wifi::Event::Networks(Err(error)) => {
                self.networks.clear();
                self.wifi_status = error.to_string();
                self.wifi_error = true;
            }
            wifi::Event::Finished(result) => {
                self.wifi_busy = false;
                match result {
                    Ok(message) => {
                        self.wifi_status = message;
                        self.wifi_error = false;
                    }
                    Err(error) => {
                        self.wifi_status = format!("{error:#}");
                        self.wifi_error = true;
                    }
                }
            }
        }
    }

    fn send_wifi(&mut self, command: wifi::Command, cx: &mut Context<Self>) {
        if self.wifi_busy {
            return;
        }
        self.wifi_busy = self.wifi_sender.send(command).is_ok();
        self.wifi_error = !self.wifi_busy;
        self.wifi_status = if self.wifi_busy {
            "等待 Windows 更新网络状态…".into()
        } else {
            "Wi‑Fi 服务已停止".into()
        };
        cx.notify();
    }

    fn refresh_bluetooth(&mut self, cx: &mut Context<Self>) {
        if self.bluetooth_loading || self.bluetooth_busy {
            return;
        }
        self.bluetooth_loading = true;
        cx.spawn(async move |this, cx| {
            let result = bluetooth::snapshot().await;
            let _ = this.update(cx, |view, cx| {
                view.bluetooth_loading = false;
                match result {
                    Ok(snapshot) => {
                        if snapshot
                            .radio
                            .as_ref()
                            .is_some_and(|radio| radio.State() == Ok(RadioState::Off))
                        {
                            view.ble_sessions.clear();
                        }
                        view.bluetooth = Some(snapshot);
                    }
                    Err(error) => {
                        view.radio_status = format!("无法读取蓝牙状态 · {error}");
                        view.radio_error = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_bluetooth_radio(&mut self, cx: &mut Context<Self>) {
        if self.bluetooth_busy {
            return;
        }
        let Some(radio) = self
            .bluetooth
            .as_ref()
            .and_then(|snapshot| snapshot.radio.clone())
        else {
            return;
        };
        self.bluetooth_busy = true;
        self.radio_status = "正在切换蓝牙…".into();
        self.radio_error = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = bluetooth::toggle_radio(radio).await;
            let _ = this.update(cx, |view, cx| {
                view.bluetooth_busy = false;
                view.radio_error = result.is_err();
                view.radio_status = result
                    .err()
                    .map_or_else(String::new, |error| error.to_string());
                view.refresh_bluetooth(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_ble(&mut self, device: bluetooth::Device, cx: &mut Context<Self>) {
        if self.bluetooth_busy {
            return;
        }
        if let Some(session) = self.ble_sessions.remove(&device.id) {
            let connected = device.connected || session.active();
            drop(session);
            self.radio_error = false;
            if connected {
                self.wait_for_ble_disconnect(device, cx);
            } else {
                self.radio_status = format!("已取消连接「{}」", device.name);
                self.refresh_bluetooth(cx);
            }
            cx.notify();
            return;
        }
        self.bluetooth_busy = true;
        self.radio_status = format!("正在请求连接 {}…", device.name);
        self.radio_error = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = bluetooth::Connection::open(&device.id).await;
            let _ = this.update(cx, |view, cx| {
                view.bluetooth_busy = false;
                match result {
                    Ok(connection) => {
                        view.ble_sessions.insert(device.id, connection);
                        view.radio_status.clear();
                    }
                    Err(error) => {
                        view.radio_error = true;
                        view.radio_status = format!("{error:#}");
                    }
                }
                view.refresh_bluetooth(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn wait_for_ble_disconnect(&mut self, device: bluetooth::Device, cx: &mut Context<Self>) {
        self.bluetooth_busy = true;
        self.radio_status = format!("正在确认「{}」已断开…", device.name);
        cx.spawn(async move |this, cx| {
            let deadline = Instant::now() + Duration::from_secs(7);
            let result = loop {
                match bluetooth::ble_connected(&device.id).await {
                    Ok(false) => break Ok(()),
                    Ok(true) if Instant::now() >= deadline => {
                        break Err(anyhow::anyhow!("系统或其他程序仍维持此低功耗连接"));
                    }
                    Err(error) => break Err(error),
                    Ok(true) => {
                        cx.background_executor()
                            .timer(Duration::from_millis(320))
                            .await
                    }
                }
            };
            let _ = this.update(cx, |view, cx| {
                view.finish_bluetooth(device, false, result, cx)
            });
        })
        .detach();
    }

    fn toggle_bluetooth_audio(
        &mut self,
        device: bluetooth::Device,
        container: windows::core::GUID,
        cx: &mut Context<Self>,
    ) {
        if self.bluetooth_busy {
            return;
        }
        let connect = !device.connected;
        let operation = if connect { "连接" } else { "断开" };
        self.bluetooth_busy = true;
        self.radio_error = false;
        self.radio_status = format!("正在{operation}「{}」…", device.name);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { bluetooth::set_audio_connected(container, connect) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.finish_bluetooth(device, connect, result, cx)
            });
        })
        .detach();
    }

    fn finish_bluetooth(
        &mut self,
        device: bluetooth::Device,
        connected: bool,
        result: anyhow::Result<()>,
        cx: &mut Context<Self>,
    ) {
        let operation = if connected { "连接" } else { "断开" };
        self.bluetooth_busy = false;
        self.radio_error = result.is_err();
        self.radio_status = match result {
            Ok(()) => {
                if let Some(item) = self.bluetooth.as_mut().and_then(|snapshot| {
                    snapshot
                        .devices
                        .iter_mut()
                        .find(|item| item.id == device.id)
                }) {
                    item.connected = connected;
                }
                format!("已{operation}「{}」", device.name)
            }
            Err(error) => format!("{operation}「{}」失败 · {error:#}", device.name),
        };
        self.refresh_bluetooth(cx);
        cx.notify();
    }

    fn notices_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = action("notice-settings", "通知隐私设置", false)
            .on_click(|_, _, _| open_settings("ms-settings:privacy-notifications"));
        match &self.notice_state {
            NoticeState::Request => empty(
                "启用通知读取",
                "请在 Windows 提示中允许读取通知。授权后，便携版会自动监听系统和应用通知。",
            )
            .child(
                action("grant", "配置通知权限", false)
                    .on_click(cx.listener(|view, _, _, cx| view.request_notices(cx))),
            )
            .into_any_element(),
            NoticeState::Denied => empty(
                "通知读取已关闭",
                "在 Windows 通知隐私设置中允许灵动岛读取通知。",
            )
            .child(settings)
            .into_any_element(),
            NoticeState::Loading => {
                empty("正在读取通知", "正在等待 Windows 返回通知与访问状态。").into_any_element()
            }
            NoticeState::Error(message) => empty("暂时无法读取通知", message.clone())
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            action("retry-notice-access", "重试", false)
                                .on_click(cx.listener(|view, _, _, cx| view.request_notices(cx))),
                        )
                        .child(settings),
                )
                .into_any_element(),
            NoticeState::Active => div()
                .flex()
                .flex_col()
                .gap_2()
                .when_some(self.notice_error.clone(), |list, error| {
                    list.child(div().px_1().text_xs().text_color(rgb(WARNING)).child(error))
                })
                .when(self.notices.is_empty(), |list| {
                    list.child(empty(
                        "暂无通知",
                        "Windows 通知中心里有新通知时，会显示在这里。",
                    ))
                })
                .children(
                    self.notices
                        .iter()
                        .map(|notice| self.notice_card(notice, cx)),
                )
                .into_any_element(),
        }
    }

    fn notice_card(&self, notice: &notifications::Notice, cx: &mut Context<Self>) -> Stateful<Div> {
        let key = notice.key;
        let id = key.id as usize;
        let busy = self.notice_action.is_some();
        div()
            .id(("notice-card", id))
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .bg(rgb(CARD))
            .p_3()
            .when(!busy, |card| {
                card.cursor_pointer()
                    .hover(|style| style.bg(rgb(BORDER)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.apply_notice_action(NoticeAction::OpenFromList(key), cx);
                    }))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(ACCENT))
                            .child(notice.app.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(notice.title.clone()),
                    )
                    .when(!notice.body.is_empty(), |row| {
                        row.child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child(notice.body.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .justify_end()
                    .when(
                        self.notice_action == Some(NoticeAction::OpenFromList(key)),
                        |row| row.child(div().text_xs().text_color(rgb(MUTED)).child("正在打开…")),
                    )
                    .child(
                        action(
                            ("notice-remove", id),
                            if self.notice_action == Some(NoticeAction::Remove(key)) {
                                "正在删除…"
                            } else {
                                "删除"
                            },
                            busy,
                        )
                        .when(!busy, |button| {
                            button.on_click(cx.listener(move |view, _, _, cx| {
                                cx.stop_propagation();
                                view.apply_notice_action(NoticeAction::Remove(key), cx);
                            }))
                        }),
                    ),
            )
    }

    fn wifi_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(MUTED)).child("可用网络"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(
                                action("wifi-settings", "网络设置", false)
                                    .on_click(|_, _, _| open_settings("ms-settings:network-wifi")),
                            )
                            .child(action("scan", "扫描", self.wifi_busy).on_click(cx.listener(
                                |view, _, _, cx| view.send_wifi(wifi::Command::Scan, cx),
                            ))),
                    ),
            )
            .when(!self.wifi_status.is_empty(), |content| {
                content.child(status_line(self.wifi_status.clone(), self.wifi_error))
            })
            .when(
                self.networks.is_empty() && self.wifi_status.is_empty(),
                |content| {
                    content.child(empty("未找到可用网络", "点击扫描，重新查找附近的 Wi‑Fi。"))
                },
            )
            .children(
                self.networks
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, network)| {
                        let description = if network.connected {
                            let speed = network
                                .traffic(&self.wifi_traffic)
                                .map_or_else(|| "等待网速数据…".into(), wifi::Traffic::summary);
                            format!("已连接 · {}% · {speed}", network.quality)
                        } else if network.secure {
                            format!("安全网络 · {}%", network.quality)
                        } else {
                            format!("开放网络 · {}%", network.quality)
                        };
                        let label = if network.connected {
                            "断开"
                        } else if network.needs_credentials() || !network.connectable {
                            "系统连接"
                        } else {
                            "连接"
                        };
                        let name = network.name.clone();
                        let button = action(("wifi-row", index), label, self.wifi_busy).on_click(
                            cx.listener(move |view, _, _, cx| {
                                if network.needs_credentials()
                                    || (!network.connectable && !network.connected)
                                {
                                    open_settings("ms-settings:network-wifi");
                                } else {
                                    view.send_wifi(wifi::Command::Toggle(network.clone()), cx);
                                }
                            }),
                        );
                        device_row(name, description, button)
                    }),
            )
    }

    fn bluetooth_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self
            .bluetooth
            .as_ref()
            .and_then(|snapshot| snapshot.radio.as_ref())
            .and_then(|radio| radio.State().ok());
        let label = match state {
            Some(RadioState::On) => "关闭蓝牙",
            Some(RadioState::Off) => "开启蓝牙",
            _ => "切换蓝牙",
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("已配对与已连接设备"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(
                                action("bluetooth-settings", "蓝牙设置", false)
                                    .on_click(|_, _, _| open_settings("ms-settings:bluetooth")),
                            )
                            .when(
                                matches!(state, Some(RadioState::On | RadioState::Off)),
                                |row| {
                                    row.child(action("radio", label, self.bluetooth_busy).on_click(
                                        cx.listener(|view, _, _, cx| {
                                            view.toggle_bluetooth_radio(cx)
                                        }),
                                    ))
                                },
                            ),
                    ),
            )
            .when(!self.radio_status.is_empty(), |content| {
                content.child(status_line(self.radio_status.clone(), self.radio_error))
            })
            .when(
                self.bluetooth.is_none() && self.radio_status.is_empty(),
                |content| content.child(empty("正在读取蓝牙", "正在查找蓝牙适配器和已配对设备。")),
            )
            .when(state == Some(RadioState::Off), |content| {
                content.child(empty("蓝牙已关闭", "开启蓝牙后可查看设备。"))
            })
            .when(
                self.bluetooth.as_ref().is_some_and(|snapshot| {
                    snapshot.devices.is_empty() && state != Some(RadioState::Off)
                }),
                |content| content.child(empty("暂无设备", "可以在 Windows 蓝牙设置中添加设备。")),
            )
            .children(
                self.bluetooth
                    .iter()
                    .flat_map(|snapshot| &snapshot.devices)
                    .cloned()
                    .map(|device| {
                        let managed = self.ble_sessions.get(&device.id);
                        let connected =
                            device.connected || managed.is_some_and(bluetooth::Connection::active);
                        let connecting = managed.is_some() && !connected;
                        let can_control = device.audio_container.is_some()
                            || (device.low_energy && (!connected || managed.is_some()));
                        let (state, label) = if connected {
                            ("已连接", "断开")
                        } else if connecting {
                            ("等待设备", "取消连接")
                        } else {
                            ("未连接", "连接")
                        };
                        let transport = if device.audio_container.is_some() {
                            "蓝牙音频"
                        } else if device.low_energy {
                            "BLE"
                        } else {
                            "经典蓝牙"
                        };
                        let name = device.name.clone();
                        let mut detail = if can_control {
                            format!("{transport} · {state}")
                        } else if device.low_energy {
                            format!("{transport} · {state} · 由系统或其他程序维持")
                        } else {
                            format!("{transport} · {state} · 无直接控制接口")
                        };
                        if connected {
                            match device.battery {
                                Some(level) => detail.push_str(&format!(" · 电量 {level}%")),
                                None => detail.push_str(" · 电量未报告"),
                            }
                        }
                        let row_id = SharedString::from(format!(
                            "bluetooth:{}",
                            device.id.to_string_lossy()
                        ));
                        let disabled = self.bluetooth_busy || !can_control;
                        let button = action(row_id, label, disabled).when(!disabled, |button| {
                            button.on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(container) = device.audio_container {
                                    view.toggle_bluetooth_audio(device.clone(), container, cx);
                                } else {
                                    view.toggle_ble(device.clone(), cx);
                                }
                            }))
                        });
                        device_row(name, detail, button)
                    }),
            )
    }

    fn preview_notice(&self) -> Option<&notifications::Notice> {
        self.notices
            .first()
            .filter(|_| self.preferences.preview != Preview::Hidden)
    }

    fn collapsed_layout(&self) -> OverlayLayout {
        if self.preview_notice().is_some() {
            return OverlayLayout::Notification;
        }
        let mode = match self.tab {
            tab if !self.tab_visible(tab) => PreviewMode::Hidden,
            Tab::Media if self.preview_media().is_some() => self.preferences.media_preview,
            Tab::Wifi if !self.wifi_traffic.is_empty() => self.preferences.wifi_preview,
            Tab::Bluetooth if self.preview_device().is_some() => self.preferences.bluetooth_preview,
            _ => PreviewMode::Hidden,
        };
        match mode {
            PreviewMode::SingleLine => OverlayLayout::SingleLinePreview,
            PreviewMode::MultiLine => OverlayLayout::DetailedPreview,
            PreviewMode::Hidden => OverlayLayout::Idle,
        }
    }

    fn wifi_preview(&self) -> Option<Div> {
        let sample = self.wifi_traffic.first()?;
        match self.preferences.wifi_preview {
            PreviewMode::SingleLine => {
                Some(single_line_preview(sample.name.clone(), sample.summary()))
            }
            PreviewMode::MultiLine => Some(detailed_preview(
                "Wi‑Fi · 已连接",
                sample.name.clone(),
                sample.summary(),
            )),
            PreviewMode::Hidden => None,
        }
    }

    fn connected_bluetooth_devices(&self) -> impl Iterator<Item = &bluetooth::Device> {
        self.bluetooth
            .iter()
            .flat_map(|snapshot| &snapshot.devices)
            .filter(|device| device.connected)
    }

    fn preview_device(&self) -> Option<&bluetooth::Device> {
        self.connected_bluetooth_devices()
            .min_by_key(|device| device.battery.unwrap_or(u8::MAX))
    }

    fn bluetooth_preview(&self) -> Option<Div> {
        let device = self.preview_device()?;
        let detail = device
            .battery
            .map_or_else(|| "电量未报告".into(), |level| format!("电量 {level}%"));
        match self.preferences.bluetooth_preview {
            PreviewMode::SingleLine => Some(single_line_preview(device.name.clone(), detail)),
            PreviewMode::MultiLine => {
                let mut containers = HashSet::new();
                let count = self
                    .connected_bluetooth_devices()
                    .filter(|device| device.container.is_none_or(|id| containers.insert(id)))
                    .count();
                Some(detailed_preview(
                    format!("蓝牙 · {count} 台已连接"),
                    device.name.clone(),
                    detail,
                ))
            }
            PreviewMode::Hidden => None,
        }
    }
}

impl Render for Island {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel_visible = self.frame.expanded || self.frame.panel_opacity > 0.0;
        let latest = self.preview_notice();
        if !panel_visible && self.collapsed_layout() == OverlayLayout::Idle {
            return div()
                .size_full()
                .rounded_b(px(self.frame.radius))
                .bg(rgb(0x080a0d));
        }
        let wifi_active = self.networks.iter().any(|network| network.connected);
        let notice_count = self.notices.len();
        let content = if panel_visible {
            div()
                .size_full()
                .opacity(self.frame.panel_opacity)
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(mark(wifi_active))
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(
                                    if self.preferences_open {
                                        "设置"
                                    } else {
                                        "灵动岛"
                                    },
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    action(
                                        "preferences",
                                        if self.preferences_open {
                                            "返回"
                                        } else {
                                            "设置"
                                        },
                                        false,
                                    )
                                    .on_click(cx.listener(
                                        |view, _, _, cx| {
                                            view.preferences_open = !view.preferences_open;
                                            view.media.dragging = false;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    action("exit", "退出", false).on_click(|_, _, cx| cx.quit()),
                                ),
                        ),
                )
                .when(!self.preferences_open, |panel| {
                    panel.child(self.tab_bar(cx))
                })
                .when(
                    !self.preferences_open
                        && self.tab == Tab::Notices
                        && self.tab_visible(Tab::Notices)
                        && !self.notices.is_empty(),
                    |panel| {
                        let busy = self.notice_action.is_some();
                        panel.child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .px_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(MUTED))
                                        .child("同步删除 Windows 通知中心的通知"),
                                )
                                .child(
                                    action(
                                        "notice-clear",
                                        if self.notice_action == Some(NoticeAction::Clear) {
                                            "正在清空…"
                                        } else {
                                            "全部删除"
                                        },
                                        busy,
                                    )
                                    .when(!busy, |button| {
                                        button.on_click(cx.listener(|view, _, _, cx| {
                                            view.apply_notice_action(NoticeAction::Clear, cx);
                                        }))
                                    }),
                                ),
                        )
                    },
                )
                .child(
                    div()
                        .id((
                            "list",
                            if self.preferences_open {
                                Tab::ALL.len()
                            } else {
                                self.tab as usize
                            },
                        ))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(if self.preferences_open {
                            self.preferences_panel(cx).into_any_element()
                        } else if !self.tab_visible(self.tab)
                            && !(self.tab == Tab::Files && self.dragging_files)
                        {
                            empty("标签页已隐藏", "可在设置中重新显示标签页。").into_any_element()
                        } else {
                            match self.tab {
                                Tab::Notices => self.notices_tab(cx),
                                Tab::Media => self.media_tab(cx).into_any_element(),
                                Tab::Wifi => self.wifi_tab(cx).into_any_element(),
                                Tab::Bluetooth => self.bluetooth_tab(cx).into_any_element(),
                                Tab::Clipboard => self.clipboard_tab(window, cx).into_any_element(),
                                Tab::Files => self.files_tab(cx).into_any_element(),
                                Tab::Launcher => self.launcher_tab(cx).into_any_element(),
                                Tab::Monitor => self.monitor_tab(cx).into_any_element(),
                            }
                        }),
                )
                .into_any_element()
        } else if let Some(notice) = latest {
            if self.preferences.preview == Preview::Count {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(mark(true))
                    .child(div().text_sm().child(format!("{notice_count} 条通知")))
                    .into_any_element()
            } else {
                let key = notice.key;
                let busy = self.notice_action.is_some();
                notification_preview(notice)
                    .id(("notice-preview", key.id as usize))
                    .when(!busy, |preview| {
                        preview
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(CARD)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.apply_notice_action(NoticeAction::OpenFromPreview(key), cx);
                            }))
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(rgb(if busy { MUTED } else { ACCENT }))
                            .child(
                                if self.notice_action == Some(NoticeAction::OpenFromPreview(key)) {
                                    "处理中…"
                                } else if notice.app_id.is_some() {
                                    "打开并删除"
                                } else {
                                    "删除"
                                },
                            ),
                    )
                    .into_any_element()
            }
        } else {
            match self.tab {
                Tab::Media => self.media_preview(),
                Tab::Wifi => self.wifi_preview(),
                Tab::Bluetooth => self.bluetooth_preview(),
                _ => None,
            }
            .unwrap_or_else(div)
            .into_any_element()
        };
        div()
            .size_full()
            .overflow_hidden()
            .font_family("Microsoft YaHei UI")
            .text_color(rgb(INK))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded_b(px(self.frame.radius))
            .child(content)
    }
}

fn single_line(text: SharedString) -> SharedString {
    if text.contains(['\r', '\n']) {
        text.split_whitespace().collect::<Vec<_>>().join(" ").into()
    } else {
        text
    }
}

fn single_line_preview(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Div {
    let title = div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .truncate()
        .child(single_line(title.into()));
    let detail = div()
        .text_xs()
        .text_color(rgb(MUTED))
        .truncate()
        .child(single_line(detail.into()));
    div()
        .size_full()
        .flex()
        .items_center()
        .px(px(18.0))
        .gap(px(16.0))
        .child(title.flex_1().min_w(px(28.0)))
        .child(detail.flex_initial().min_w_0().text_right())
}

fn detailed_preview(
    section: impl Into<SharedString>,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Div {
    let detail = single_line(detail.into());
    div()
        .size_full()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .child(div().flex_shrink_0().child(mark(true)))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(ACCENT))
                        .truncate()
                        .child(single_line(section.into())),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(single_line(title.into())),
                )
                .when(!detail.trim().is_empty(), |preview| {
                    preview.child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .truncate()
                            .child(detail),
                    )
                }),
        )
}

fn notification_preview(notice: &notifications::Notice) -> Div {
    detailed_preview(
        notice.app.clone(),
        notice.title.clone(),
        notice.body.split_whitespace().collect::<Vec<_>>().join(" "),
    )
}

fn mark(online: bool) -> impl IntoElement {
    div()
        .size(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(if online { ACCENT } else { MUTED }))
        .child(
            div()
                .size(px(5.0))
                .rounded_full()
                .bg(rgb(if online { ACCENT } else { MUTED })),
        )
}

fn action(id: impl Into<ElementId>, label: &'static str, disabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(BORDER))
        .text_xs()
        .text_color(rgb(if disabled { MUTED } else { INK }))
        .when(!disabled, |button| {
            button.cursor_pointer().hover(|style| style.bg(rgb(CARD)))
        })
        .child(label)
}

fn empty(title: &'static str, body: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(rgb(CARD))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_sm()
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(body.into()))
}

fn device_row(name: String, description: String, button: Stateful<Div>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .rounded_lg()
        .bg(rgb(CARD))
        .px_3()
        .py_2()
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex_col()
                .flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(name),
                )
                .child(div().text_xs().text_color(rgb(MUTED)).child(description)),
        )
        .child(button)
}

fn status_line(message: String, error: bool) -> Div {
    div()
        .text_xs()
        .text_color(rgb(if error { WARNING } else { MUTED }))
        .px_1()
        .child(message)
}

fn open_settings(uri: &'static str) {
    let _ = Command::new("explorer.exe").arg(uri).spawn();
}
