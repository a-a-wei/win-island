use crate::{
    desktop_tools::file_drag::{DropRegistration, FileDrop},
    preferences::Preferences,
};
use anyhow::{Result, bail};
use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    ffi::c_void,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{
        CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, HGDIOBJ, MONITOR_DEFAULTTOPRIMARY,
        MONITORINFO, MonitorFromPoint, SetWindowRgn,
    },
    UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
};

pub const IDLE_WIDTH: f32 = 128.0;
pub const IDLE_HEIGHT: f32 = 12.0;
pub const FRAME_INTERVAL: Duration = Duration::from_millis(12);
const MOTION: Duration = Duration::from_millis(150);
const PREVIEW_HOVER_DELAY_MS: u64 = 300;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    Idle,
    SingleLinePreview,
    DetailedPreview,
    Notification,
    Expanded,
    Monitor,
}

#[derive(Clone, Copy)]
struct Shape {
    width: f32,
    height: f32,
    radius: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Frame {
    pub expanded: bool,
    pub radius: f32,
    pub panel_opacity: f32,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            expanded: false,
            radius: Layout::Idle.shape().radius,
            panel_opacity: 0.0,
        }
    }
}

impl Layout {
    fn shape(self) -> Shape {
        let (width, height, radius) = match self {
            Self::Idle => (IDLE_WIDTH, IDLE_HEIGHT, IDLE_HEIGHT / 2.0),
            Self::SingleLinePreview => (344.0, 36.0, 10.0),
            Self::DetailedPreview | Self::Notification => (344.0, 70.0, 16.0),
            Self::Expanded => (424.0, 374.0, 18.0),
            Self::Monitor => (684.0, 406.0, 18.0),
        };
        Shape {
            width,
            height,
            radius,
        }
    }
}

pub struct Overlay {
    hwnd: HWND,
    file_drop: Option<DropRegistration>,
    entered_at: Option<Instant>,
    left_at: Option<Instant>,
    hold_closed: bool,
    target: Layout,
    start: Instant,
    from: Shape,
    placement: Option<(i32, i32, i32, i32, i32)>,
}

impl Overlay {
    pub fn new(window: &Window) -> Result<Self> {
        let raw = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("无法访问悬浮窗口句柄 · {error}"))?;
        let RawWindowHandle::Win32(handle) = raw.as_raw() else {
            bail!("无法取得 Windows 悬浮窗口句柄");
        };
        let hwnd = HWND(handle.hwnd.get() as *mut c_void);
        Ok(Self {
            hwnd,
            file_drop: None,
            entered_at: None,
            left_at: None,
            hold_closed: false,
            target: Layout::Idle,
            start: Instant::now(),
            from: Layout::Idle.shape(),
            placement: None,
        })
    }

    pub fn show(&mut self) {
        unsafe {
            // A popup frame can be shorter than an overlapped window's minimum.
            let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, style | WS_POPUP.0 as isize);
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.place(self.target.shape());
        unsafe {
            let _ = SetWindowTextW(self.hwnd, windows::core::w!("灵动岛 · Win Island"));
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn collapse_until_pointer_leaves(&mut self) {
        self.hold_closed = true;
    }

    pub fn register_file_drop(&mut self) -> Result<Receiver<FileDrop>> {
        let (registration, receiver) = DropRegistration::register(self.hwnd)?;
        self.file_drop = Some(registration);
        Ok(receiver)
    }

    pub fn tick(
        &mut self,
        now: Instant,
        collapsed_layout: Layout,
        preview_clickable: bool,
        expanded_layout: Layout,
        dragging_files: bool,
        preferences: Preferences,
    ) -> Frame {
        let inside = self.pointer_inside();
        let was_expanded = matches!(self.target, Layout::Expanded | Layout::Monitor);
        let expanded = if dragging_files {
            self.hold_closed = false;
            self.left_at = None;
            true
        } else if self.hold_closed {
            self.hold_closed = inside;
            self.entered_at = None;
            self.left_at = None;
            false
        } else if inside {
            self.left_at = None;
            let elapsed = now.saturating_duration_since(*self.entered_at.get_or_insert(now));
            let delay = if preview_clickable && self.target == Layout::Notification {
                preferences.hover_delay_ms.max(PREVIEW_HOVER_DELAY_MS)
            } else {
                preferences.hover_delay_ms
            };
            was_expanded || elapsed >= Duration::from_millis(delay)
        } else {
            self.entered_at = None;
            let elapsed = now.saturating_duration_since(*self.left_at.get_or_insert(now));
            was_expanded && elapsed < Duration::from_millis(preferences.collapse_delay_ms)
        };
        let target = if expanded {
            expanded_layout
        } else {
            collapsed_layout
        };
        if target != self.target {
            self.from = self.shape_at(now, preferences.animate);
            self.start = now;
            self.target = target;
        }
        let shape = self.shape_at(now, preferences.animate);
        self.place(shape);
        Frame {
            expanded,
            radius: shape.radius,
            panel_opacity: ((shape.height - 100.0) / 150.0).clamp(0.0, 1.0),
        }
    }

    fn shape_at(&self, now: Instant, animate: bool) -> Shape {
        let target = self.target.shape();
        if !animate {
            return target;
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / MOTION.as_secs_f32())
            .min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        let blend = |from, to| from + (to - from) * eased;
        Shape {
            width: blend(self.from.width, target.width),
            height: blend(self.from.height, target.height),
            radius: blend(self.from.radius, target.radius),
        }
    }

    fn pointer_inside(&self) -> bool {
        let mut point = POINT::default();
        let mut bounds = RECT::default();
        unsafe {
            GetCursorPos(&mut point).is_ok()
                && GetWindowRect(self.hwnd, &mut bounds).is_ok()
                && point.x >= bounds.left
                && point.x < bounds.right
                && point.y >= bounds.top
                && point.y < bounds.bottom
                && WindowFromPoint(point) == self.hwnd
        }
    }

    fn place(&mut self, shape: Shape) {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0;
        let scale = |value: f32| (value * dpi).round() as i32;
        let monitor = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return;
        }
        let area = info.rcMonitor;
        let width = scale(shape.width).min(area.right - area.left);
        let height = scale(shape.height).min(area.bottom - area.top);
        if width <= 0 || height <= 0 {
            return;
        }
        let x = area.left + (area.right - area.left - width) / 2;
        let y = area.top;
        let ellipse = scale(shape.radius * 2.0).min(height).min(width);
        let placement = (x, y, width, height, ellipse);
        if self.placement == Some(placement) {
            return;
        }
        unsafe {
            if SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE,
            )
            .is_err()
            {
                return;
            }
            // Put the upper arc above the screen so the popup meets the top edge.
            // The slightly wider clip leaves room for GPUI's antialiased edge.
            let region = CreateRoundRectRgn(-1, -ellipse, width + 1, height + 1, ellipse, ellipse);
            if !region.is_invalid() && SetWindowRgn(self.hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(HGDIOBJ(region.0));
            }
        }
        self.placement = Some(placement);
    }
}
