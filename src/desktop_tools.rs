use anyhow::{Context, Result, bail};
use gpui_kit::{App, Asset, ClipboardEntry, ClipboardItem, Image, RenderImage, Window};
use image::{Frame, ImageReader, Limits};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::c_void,
    fs,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::Arc,
};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HWND},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
            Ole::CF_HDROP,
        },
        UI::{
            Shell::{DROPFILES, ShellExecuteW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

pub mod file_drag;

pub struct ClipboardThumbnail;

impl ClipboardThumbnail {
    pub fn supports(path: &Path) -> bool {
        const EXTENSIONS: &[&str] = &[
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico",
        ];
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                EXTENSIONS
                    .iter()
                    .any(|format| ext.eq_ignore_ascii_case(format))
            })
    }

    fn load_file(path: &Path) -> Option<Arc<RenderImage>> {
        if !path.is_file() {
            return None;
        }
        let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
        let mut limits = Limits::default();
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let mut thumbnail = reader.decode().ok()?.thumbnail(160, 104).into_rgba8();
        for pixel in thumbnail.pixels_mut() {
            // GPUI takes BGRA pixels for RenderImage.
            pixel.0.swap(0, 2);
        }
        Some(Arc::new(RenderImage::new([Frame::new(thumbnail)])))
    }
}

impl Asset for ClipboardThumbnail {
    type Source = PathBuf;
    type Output = Option<Arc<RenderImage>>;

    fn load(
        path: Self::Source,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move { Self::load_file(&path) }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Clip {
    Text(String),
    Image(Arc<Image>),
    Files(Vec<PathBuf>),
}

impl Clip {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Text(_) => "文本",
            Self::Image(_) => "图片",
            Self::Files(_) => "文件",
        }
    }

    fn from_item(item: ClipboardItem) -> Option<Self> {
        let mut selected = None;
        for entry in item.entries {
            match entry {
                ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => {
                    return Some(Self::Files(paths.paths().to_vec()));
                }
                ClipboardEntry::Image(image) if image.bytes().len() <= 12 * 1024 * 1024 => {
                    selected = Some(Self::Image(Arc::new(image)));
                }
                ClipboardEntry::String(text) if selected.is_none() => {
                    if !text.text().is_empty() && text.text().len() <= 1024 * 1024 {
                        selected = Some(Self::Text(text.text().to_owned()));
                    }
                }
                _ => {}
            }
        }
        selected
    }

    fn size(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Image(image) => image.bytes().len(),
            Self::Files(paths) => paths.iter().map(|path| path.as_os_str().len()).sum(),
        }
    }

    fn file_paths(&self) -> &[PathBuf] {
        match self {
            Self::Files(paths) => paths,
            _ => &[],
        }
    }

    pub fn copy(&self, window: &Window, cx: &App) -> Result<()> {
        match self {
            Self::Files(paths) => copy_files(paths, window),
            Self::Text(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                Ok(())
            }
            Self::Image(image) => {
                cx.write_to_clipboard(ClipboardItem::new_image(image));
                Ok(())
            }
        }
    }
}

#[derive(Default)]
pub struct ClipboardHistory {
    pub items: Vec<Clip>,
    pub paused: bool,
    sequence: u32,
}

impl ClipboardHistory {
    pub fn sample(&mut self, cx: &mut App, max_items: usize) -> bool {
        let before = unsafe { GetClipboardSequenceNumber() };
        if before == 0 || before == self.sequence {
            return false;
        }
        if self.paused {
            self.sequence = before;
            return false;
        }
        let Some(eligible) = clipboard_history_allowed() else {
            return false;
        };
        let value = eligible
            .then(|| cx.read_from_clipboard())
            .flatten()
            .and_then(Clip::from_item);
        let after = unsafe { GetClipboardSequenceNumber() };
        if after != before {
            return false;
        }
        self.sequence = after;
        let Some(value) = value else { return false };
        if self.items.first() == Some(&value) {
            return false;
        }
        self.items.retain(|item| item != &value);
        self.items.insert(0, value);
        let mut bytes = 0;
        let keep = self
            .items
            .iter()
            .take(max_items)
            .take_while(|item| {
                bytes += item.size();
                bytes <= 48 * 1024 * 1024
            })
            .count();
        if keep < self.items.len() {
            let (retained, removed) = self.items.split_at(keep);
            let retained_paths: HashSet<_> = retained.iter().flat_map(Clip::file_paths).collect();
            for path in removed.iter().flat_map(Clip::file_paths) {
                if !retained_paths.contains(path) {
                    cx.remove_asset::<ClipboardThumbnail>(path);
                }
            }
            self.items.truncate(keep);
        }
        true
    }

    pub fn clear(&mut self, cx: &mut App) {
        for path in self.items.iter().flat_map(Clip::file_paths) {
            cx.remove_asset::<ClipboardThumbnail>(path);
        }
        self.items.clear();
        self.sequence = unsafe { GetClipboardSequenceNumber() };
    }
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open(owner: Option<HWND>) -> Result<Self> {
        unsafe {
            OpenClipboard(owner)?;
        }
        Ok(Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

fn clipboard_history_allowed() -> Option<bool> {
    let _clipboard = ClipboardGuard::open(None).ok()?;
    unsafe {
        let excluded = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if excluded != 0 && IsClipboardFormatAvailable(excluded).is_ok() {
            return Some(false);
        }
        let included = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
        if included != 0 && IsClipboardFormatAvailable(included).is_ok() {
            let data = windows::Win32::Foundation::HGLOBAL(GetClipboardData(included).ok()?.0);
            if GlobalSize(data) >= 4 {
                let value = GlobalLock(data);
                if !value.is_null() {
                    let allowed = value.cast::<u32>().read_unaligned() != 0;
                    let _ = GlobalUnlock(data);
                    return Some(allowed);
                }
            }
        }
    }
    Some(true)
}

fn copy_files(paths: &[PathBuf], window: &Window) -> Result<()> {
    if paths.is_empty()
        || paths
            .iter()
            .any(|path| !path.is_absolute() || !path.exists())
    {
        bail!("一个或多个文件已不存在");
    }
    let owner = window_handle(window)?;
    let names: Vec<u16> = paths
        .iter()
        .flat_map(|path| path.as_os_str().encode_wide().chain([0]))
        .chain([0])
        .collect();
    write_file_clipboard(owner, &names)
}

pub fn window_handle(window: &Window) -> Result<HWND> {
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .as_raw()
    else {
        bail!("找不到灵动岛窗口");
    };
    Ok(HWND(handle.hwnd.get() as *mut c_void))
}

fn write_file_clipboard(owner: HWND, names: &[u16]) -> Result<()> {
    let offset = size_of::<DROPFILES>();
    let _clipboard = ClipboardGuard::open(Some(owner))?;
    unsafe {
        let data = GlobalAlloc(GMEM_MOVEABLE, offset + names.len() * size_of::<u16>())?;
        let value = GlobalLock(data).cast::<u8>();
        if value.is_null() {
            let _ = GlobalFree(Some(data));
            bail!("无法复制文件");
        }
        value.cast::<DROPFILES>().write_unaligned(DROPFILES {
            pFiles: offset as u32,
            fWide: true.into(),
            ..Default::default()
        });
        ptr::copy_nonoverlapping(names.as_ptr(), value.add(offset).cast(), names.len());
        let _ = GlobalUnlock(data);
        let result = EmptyClipboard()
            .and_then(|()| SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(data.0))));
        if let Err(error) = result {
            let _ = GlobalFree(Some(data));
            return Err(error.into());
        }
    }
    Ok(())
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopLists {
    pub files: Vec<PathBuf>,
    pub shortcuts: Vec<PathBuf>,
}

impl DesktopLists {
    fn path() -> Result<PathBuf> {
        Ok(crate::preferences::settings_path()?.with_file_name("desktop-lists.json"))
    }

    pub fn load() -> Result<Self> {
        match fs::read(Self::path()?) {
            Ok(data) => Ok(serde_json::from_slice(&data).context("无法解析中转站和快捷启动清单")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let folder = path.parent().context("找不到设置目录")?;
        fs::create_dir_all(folder)?;
        let mut temp = tempfile::NamedTempFile::new_in(folder)?;
        serde_json::to_writer_pretty(&mut temp, self)?;
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        Ok(())
    }
}

pub fn open_path(path: &Path) -> Result<()> {
    if !path.is_absolute() || !path.exists() {
        bail!("路径已不存在");
    }
    let _apartment =
        crate::Apartment::enter(windows::Win32::System::WinRT::RO_INIT_SINGLETHREADED)?;
    let path = HSTRING::from(path.as_os_str());
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        bail!("Windows 无法打开此项目");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{ExternalPaths, ImageFormat};

    #[test]
    fn clipboard_thumbnail_loads_image_files_and_falls_back_for_invalid_paths() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("sample.PNG");
        image::RgbaImage::from_pixel(320, 160, image::Rgba([220, 80, 42, 255]))
            .save_with_format(&image_path, image::ImageFormat::Png)
            .unwrap();
        assert!(ClipboardThumbnail::supports(&image_path));
        let preview = ClipboardThumbnail::load_file(&image_path).unwrap();
        assert_eq!(preview.size(0), gpui_kit::size(160.into(), 80.into()));
        assert_eq!(&preview.as_bytes(0).unwrap()[..4], &[42, 80, 220, 255]);

        let ordinary_path = dir.path().join("notes.txt");
        assert!(!ClipboardThumbnail::supports(&ordinary_path));
        let invalid_path = dir.path().join("invalid.jpg");
        fs::write(&invalid_path, "invalid image").unwrap();
        assert!(ClipboardThumbnail::load_file(&invalid_path).is_none());
        let directory_path = dir.path().join("folder.webp");
        fs::create_dir(&directory_path).unwrap();
        assert!(ClipboardThumbnail::load_file(&directory_path).is_none());
    }

    #[test]
    fn clipboard_keeps_the_native_file_format_when_text_is_also_available() {
        let path = PathBuf::from(r"C:\test\folder");
        let clipboard = ClipboardItem {
            entries: vec![
                ClipboardEntry::from("text fallback".to_owned()),
                ClipboardEntry::ExternalPaths(ExternalPaths([path.clone()].into_iter().collect())),
            ],
        };
        assert!(matches!(Clip::from_item(clipboard), Some(Clip::Files(paths)) if paths == [path]));
    }

    #[test]
    fn clipboard_keeps_images_with_a_text_alternative() {
        let clipboard = ClipboardItem {
            entries: vec![
                ClipboardEntry::Image(Image::from_bytes(ImageFormat::Png, vec![1])),
                ClipboardEntry::from("text fallback".to_owned()),
            ],
        };
        assert!(matches!(Clip::from_item(clipboard), Some(Clip::Image(_))));
    }
}
