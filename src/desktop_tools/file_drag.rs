use anyhow::{Context, Result, bail};
use std::{
    cell::{Cell, RefCell},
    ffi::OsString,
    fs::File,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, POINTL},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            GetFileInformationByHandle,
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, IBindCtx,
                IDataObject, STGMEDIUM, TYMED_HGLOBAL,
            },
            DataExchange::RegisterClipboardFormatW,
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::{
                CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE,
                IDropSource, IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize,
                RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
            },
            SystemServices::MODIFIERKEYS_FLAGS,
        },
        UI::{
            Input::KeyboardAndMouse::ReleaseCapture,
            Shell::{
                BHID_DataObject, CFSTR_LOGICALPERFORMEDDROPEFFECT, CFSTR_PERFORMEDDROPEFFECT,
                DragQueryFileW, FOF_ALLOWUNDO, FOFX_RECYCLEONDELETE, FileOperation, HDROP,
                IFileOperation, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop,
            },
        },
    },
    core::{HSTRING, PCWSTR, Ref, implement},
};

pub enum FileDrop {
    Entered,
    Left,
    Submitted(Vec<PathBuf>),
}

thread_local! { static OUTBOUND: Cell<bool> = const { Cell::new(false) }; }

struct OleApartment;

impl OleApartment {
    fn enter() -> Result<Self> {
        unsafe { OleInitialize(None)? };
        Ok(Self)
    }
}

impl Drop for OleApartment {
    fn drop(&mut self) {
        unsafe { OleUninitialize() }
    }
}

pub struct DropRegistration {
    hwnd: HWND,
    _apartment: OleApartment,
}

impl DropRegistration {
    pub fn register(hwnd: HWND) -> Result<(Self, Receiver<FileDrop>)> {
        let apartment = OleApartment::enter()?;
        let (events, receiver) = mpsc::channel();
        let target: IDropTarget = ShelfTarget {
            events,
            paths: RefCell::default(),
        }
        .into();
        unsafe {
            // GPUI installs a generic target. Use the native file event itself
            // instead of routing entry through a synthetic hover hit test.
            let _ = RevokeDragDrop(hwnd);
            RegisterDragDrop(hwnd, &target).context("无法注册文件中转站拖放区域")?;
        }
        Ok((
            Self {
                hwnd,
                _apartment: apartment,
            },
            receiver,
        ))
    }
}

impl Drop for DropRegistration {
    fn drop(&mut self) {
        let _ = unsafe { RevokeDragDrop(self.hwnd) };
    }
}

#[implement(IDropTarget)]
struct ShelfTarget {
    events: Sender<FileDrop>,
    paths: RefCell<Vec<PathBuf>>,
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for ShelfTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let accepts_copy = effect_copy(effect);
        let files = if accepts_copy && !OUTBOUND.get() {
            data.as_ref().map(read_paths).unwrap_or_default()
        } else {
            Vec::new()
        };
        let accepted = !files.is_empty();
        *self.paths.borrow_mut() = files;
        put_effect(effect, accepted);
        if accepted {
            let _ = self.events.send(FileDrop::Entered);
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        put_effect(
            effect,
            !self.paths.borrow().is_empty() && effect_copy(effect),
        );
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if !self.paths.take().is_empty() {
            let _ = self.events.send(FileDrop::Left);
        }
        Ok(())
    }

    fn Drop(
        &self,
        _: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let files = self.paths.take();
        let had_files = !files.is_empty();
        let accepted = had_files
            && effect_copy(effect)
            && self.events.send(FileDrop::Submitted(files)).is_ok();
        if had_files && !accepted {
            let _ = self.events.send(FileDrop::Left);
        }
        put_effect(effect, accepted);
        Ok(())
    }
}

fn effect_copy(effect: *const DROPEFFECT) -> bool {
    !effect.is_null() && unsafe { *effect & DROPEFFECT_COPY == DROPEFFECT_COPY }
}

fn put_effect(effect: *mut DROPEFFECT, accepted: bool) {
    if !effect.is_null() {
        unsafe {
            *effect = if accepted {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            };
        }
    }
}

fn format(id: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: id,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    }
}

struct DataMedium(STGMEDIUM);

impl Drop for DataMedium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

fn read_paths(data: &IDataObject) -> Vec<PathBuf> {
    let Ok(medium) = (unsafe { data.GetData(&format(CF_HDROP.0 as u16)) }) else {
        return Vec::new();
    };
    let medium = DataMedium(medium);
    if medium.0.tymed != TYMED_HGLOBAL.0 as u32 {
        return Vec::new();
    }
    let files = HDROP(unsafe { medium.0.u.hGlobal.0 });
    let count = unsafe { DragQueryFileW(files, u32::MAX, None) };
    (0..count)
        .filter_map(|index| {
            let length = unsafe { DragQueryFileW(files, index, None) };
            if length == 0 {
                return None;
            }
            let mut units = vec![0; length as usize + 1];
            let read = unsafe { DragQueryFileW(files, index, Some(&mut units)) } as usize;
            let path = PathBuf::from(OsString::from_wide(&units[..read]));
            path.is_absolute().then_some(path)
        })
        .collect()
}

fn drop_effect(data: &IDataObject, name: PCWSTR) -> Option<DROPEFFECT> {
    let id = unsafe { RegisterClipboardFormatW(name) };
    if id == 0 {
        return None;
    }
    let medium = DataMedium(unsafe { data.GetData(&format(id as u16)) }.ok()?);
    if medium.0.tymed != TYMED_HGLOBAL.0 as u32 {
        return None;
    }
    unsafe {
        let handle = medium.0.u.hGlobal;
        if GlobalSize(handle) < size_of::<u32>() {
            return None;
        }
        let ptr = GlobalLock(handle).cast::<u32>();
        if ptr.is_null() {
            return None;
        }
        let effect = DROPEFFECT(ptr.read_unaligned());
        let _ = GlobalUnlock(handle);
        Some(effect)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DragOutcome {
    Cancelled,
    Copied,
    Moved,
}

pub fn drag_file(path: &Path, owner: HWND) -> Result<DragOutcome> {
    let _ole = OleApartment::enter()?;
    let name = HSTRING::from(path.as_os_str());
    let before = file_identity(path);
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&name, None::<&IBindCtx>)? };
    let data: IDataObject = unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_DataObject)? };
    let effect = OUTBOUND.with(|active| {
        active.set(true);
        let effect = unsafe {
            let _ = ReleaseCapture();
            SHDoDragDrop(
                Some(owner),
                &data,
                None::<&IDropSource>,
                DROPEFFECT_COPY | DROPEFFECT_MOVE,
            )
        };
        active.set(false);
        effect
    })?;
    // Explorer may return COPY or NONE after an optimized move. The logical
    // effect and the old path together tell us whether the original moved.
    let logical = drop_effect(&data, CFSTR_LOGICALPERFORMEDDROPEFFECT);
    if (logical == Some(DROPEFFECT_MOVE) || effect != DROPEFFECT_NONE)
        && matches!(path.try_exists(), Ok(false))
    {
        return Ok(DragOutcome::Moved);
    }
    if logical == Some(DROPEFFECT_MOVE) || effect == DROPEFFECT_MOVE {
        if effect == DROPEFFECT_MOVE
            && drop_effect(&data, CFSTR_PERFORMEDDROPEFFECT) == Some(DROPEFFECT_MOVE)
        {
            if before.is_none() || file_identity(path) != before {
                bail!("目标已接收文件，但原项目已变化，已保留原项目");
            }
            recycle_original(&item, owner)?;
            if !matches!(path.try_exists(), Ok(false)) {
                bail!("移动未完成，原项目仍保留");
            }
            return Ok(DragOutcome::Moved);
        }
        bail!("目标未确认移动原项目，已保留原项目");
    }
    if effect == DROPEFFECT_COPY || logical == Some(DROPEFFECT_COPY) {
        Ok(DragOutcome::Copied)
    } else {
        Ok(DragOutcome::Cancelled)
    }
}

fn file_identity(path: &Path) -> Option<(u32, u32, u32, u32, u32, u32, u32)> {
    let file: File = File::options()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info).ok()?;
    }
    Some((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
        info.ftLastWriteTime.dwHighDateTime,
        info.ftLastWriteTime.dwLowDateTime,
        info.nFileSizeHigh,
        info.nFileSizeLow,
    ))
}

fn recycle_original(item: &IShellItem, owner: HWND) -> Result<()> {
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOwnerWindow(owner)?;
        operation.SetOperationFlags(FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE)?;
        operation.DeleteItem(item, None)?;
        operation.PerformOperations()?;
        if operation.GetAnyOperationsAborted()?.as_bool() {
            bail!("移动未完成，原项目仍保留");
        }
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn accepts_real_shell_file_and_directory_payloads_without_moving_them() -> Result<()> {
    let _ole = OleApartment::enter()?;
    let folder = tempfile::tempdir()?;
    let file = folder.path().join("拖放验证.txt");
    std::fs::write(&file, "test")?;
    let (sender, receiver) = mpsc::channel();
    let target: IDropTarget = ShelfTarget {
        events: sender,
        paths: RefCell::default(),
    }
    .into();
    for path in [file.as_path(), folder.path()] {
        let name = HSTRING::from(path.as_os_str());
        let item: IShellItem = unsafe { SHCreateItemFromParsingName(&name, None::<&IBindCtx>)? };
        let data: IDataObject = unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_DataObject)? };
        let mut effect = DROPEFFECT_COPY | DROPEFFECT_MOVE;
        unsafe {
            target.DragEnter(
                &data,
                MODIFIERKEYS_FLAGS::default(),
                POINTL::default(),
                &mut effect,
            )?;
            assert!(matches!(receiver.try_recv()?, FileDrop::Entered));
            assert_eq!(effect, DROPEFFECT_COPY);
            target.Drop(
                &data,
                MODIFIERKEYS_FLAGS::default(),
                POINTL::default(),
                &mut effect,
            )?;
        }
        assert!(matches!(receiver.try_recv()?, FileDrop::Submitted(paths) if paths == [path]));
        assert!(path.exists());
    }
    Ok(())
}
