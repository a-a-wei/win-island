use anyhow::{Context, Result, bail};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows::{
    Foundation::TypedEventHandler,
    UI::Notifications::{
        KnownNotificationBindings, Management::UserNotificationListener, NotificationKinds,
        UserNotification,
    },
    Win32::{
        Foundation::{E_POINTER, ERROR_INSUFFICIENT_BUFFER, ERROR_NOT_FOUND, PROPERTYKEY},
        Storage::Packaging::Appx::GetCurrentPackageFullName,
        System::{
            Com::{CoTaskMemFree, IBindCtx},
            WinRT::{RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED},
        },
        UI::{
            Shell::{
                BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItem2,
                KF_FLAG_DEFAULT, SEE_MASK_FLAG_NO_UI, SEE_MASK_IDLIST, SEE_MASK_NOASYNC,
                SHELLEXECUTEINFOW, SHGetIDListFromObject, SHGetKnownFolderItem,
                SIGDN_PARENTRELATIVEPARSING, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{GUID, HSTRING, Interface},
};

pub use windows::UI::Notifications::Management::UserNotificationListenerAccessStatus as Access;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NoticeKey {
    pub id: u32,
    pub created: i64,
}

#[derive(Clone)]
pub struct Notice {
    pub key: NoticeKey,
    pub app: String,
    pub app_id: Option<HSTRING>,
    pub title: String,
    pub body: String,
}

pub struct Listener {
    pub handle: UserNotificationListener,
    dirty: Arc<AtomicBool>,
    token: Option<i64>,
}

impl Listener {
    pub fn new() -> Result<Self> {
        let handle = UserNotificationListener::Current()?;
        // A few Windows builds do not fire this event for every change. The UI
        // also reconciles snapshots periodically while access is allowed.
        let mut listener = Self {
            handle,
            dirty: Arc::new(AtomicBool::new(true)),
            token: None,
        };
        if listener.handle.GetAccessStatus() == Ok(Access::Allowed) {
            listener.subscribe();
        }
        Ok(listener)
    }

    pub fn take_change(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn subscribe(&mut self) {
        if self.token.is_some() {
            return;
        }
        let changed = self.dirty.clone();
        self.token = self
            .handle
            .NotificationChanged(&TypedEventHandler::new(move |_, _| {
                changed.store(true, Ordering::Release);
                Ok(())
            }))
            .ok();
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            let _ = self.handle.RemoveNotificationChanged(token);
        }
    }
}

pub async fn read(handle: &UserNotificationListener) -> Result<Vec<Notice>> {
    check_access(handle)?;
    let notifications = handle
        .GetNotificationsAsync(NotificationKinds::Toast)?
        .await?;
    let mut notices: Vec<_> = notifications
        .into_iter()
        .filter_map(|item| parse(&item).ok())
        .collect();
    notices.sort_by_key(|notice| std::cmp::Reverse((notice.key.created, notice.key.id)));
    Ok(notices)
}

pub fn remove(key: NoticeKey) -> Result<()> {
    let _apartment = crate::Apartment::enter(RO_INIT_MULTITHREADED)?;
    // Obtain the listener on the worker instead of sharing the UI apartment's handle.
    let handle = UserNotificationListener::Current()?;
    check_access(&handle)?;
    remove_current(&handle, key)
}

fn remove_current(handle: &UserNotificationListener, key: NoticeKey) -> Result<()> {
    // The notification may have expired or its ID may have been reused since
    // it was displayed. In either case the selected notification is gone.
    let current = match handle.GetNotification(key.id) {
        Ok(current) => current,
        Err(error)
            if error.code().is_ok()
                || error.code() == E_POINTER
                || error.code() == ERROR_NOT_FOUND.to_hresult() =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if current.CreationTime()?.UniversalTime == key.created {
        handle.RemoveNotification(key.id)?;
    }
    Ok(())
}

pub fn clear(keys: &[NoticeKey]) -> Result<()> {
    let _apartment = crate::Apartment::enter(RO_INIT_MULTITHREADED)?;
    let handle = UserNotificationListener::Current()?;
    check_access(&handle)?;
    // Some Windows 10 releases accept ClearNotifications without removing
    // third-party toasts from an unpackaged listener. Remove the visible set by ID.
    let mut first_error = None;
    for key in keys {
        if let Err(error) = remove_current(&handle, *key) {
            first_error.get_or_insert(error);
        }
    }
    let remaining = pollster::block_on(read(&handle))?;
    if remaining.iter().any(|notice| keys.contains(&notice.key)) {
        bail!("Windows 仍保留部分通知，请重试");
    }
    if let Some(error) = first_error {
        return Err(error.into());
    }
    Ok(())
}

fn check_access(handle: &UserNotificationListener) -> Result<()> {
    if handle.GetAccessStatus()? != Access::Allowed {
        bail!("Windows 通知读取权限已关闭");
    }
    Ok(())
}

pub fn open_app(app_id: &HSTRING) -> Result<()> {
    // Some Shell verbs require an STA. This runs on a background worker.
    let _apartment = crate::Apartment::enter(RO_INIT_SINGLETHREADED)?;
    unsafe {
        let folder =
            SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)
                .context("无法访问 Windows 应用列表")?;
        let apps =
            folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems)?;
        loop {
            let mut next = [None];
            apps.Next(&mut next, None)?;
            let [Some(item)] = next else {
                bail!("Windows 应用列表中找不到该通知的来源应用");
            };
            if !matches_app_id(&item, app_id) {
                continue;
            }
            let pidl = SHGetIDListFromObject(&item)?;
            let mut launch = SHELLEXECUTEINFOW {
                cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_IDLIST | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
                lpIDList: pidl.cast(),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            let result = ShellExecuteExW(&mut launch);
            CoTaskMemFree(Some(pidl.cast()));
            return result.context("Windows 无法启动该应用");
        }
    }
}

fn matches_app_id(item: &IShellItem, app_id: &HSTRING) -> bool {
    // PKEY_AppUserModel_ID, used when the Shell parsing name differs.
    const APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };
    unsafe {
        let matches = |candidate: windows::core::PWSTR| {
            let matches = !candidate.is_null() && candidate.as_wide() == &**app_id;
            CoTaskMemFree(Some(candidate.0.cast()));
            matches
        };
        item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING)
            .is_ok_and(matches)
            || item
                .cast::<IShellItem2>()
                .and_then(|item| item.GetString(&APP_USER_MODEL_ID))
                .is_ok_and(matches)
    }
}

fn parse(item: &UserNotification) -> Result<Notice> {
    let text = (|| {
        let visual = item.Notification()?.Visual()?;
        visual
            .GetBinding(&KnownNotificationBindings::ToastGeneric()?)?
            .GetTextElements()
    })()
    .ok();
    let mut text = text.into_iter().flatten().filter_map(|element| {
        let value = element.Text().ok()?.to_string_lossy();
        (!value.trim().is_empty()).then_some(value)
    });
    let app_info = item.AppInfo().ok();
    Ok(Notice {
        key: NoticeKey {
            id: item.Id()?,
            created: item.CreationTime()?.UniversalTime,
        },
        app: app_info
            .as_ref()
            .and_then(|info| info.DisplayInfo().ok())
            .and_then(|info| info.DisplayName().ok())
            .map_or_else(|| "Windows".into(), |name| name.to_string_lossy()),
        app_id: app_info
            .and_then(|info| info.AppUserModelId().ok())
            .filter(|id| !id.is_empty()),
        title: text.next().unwrap_or_else(|| "新通知".into()),
        body: text.collect::<Vec<_>>().join(" · "),
    })
}

pub fn has_package_identity() -> bool {
    let mut length = 0;
    unsafe { GetCurrentPackageFullName(&mut length, None) == ERROR_INSUFFICIENT_BUFFER }
}
