use anyhow::{Context, Result, bail};
use std::{future::Future, ptr, time::Duration};
use windows::{
    Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    },
    Win32::{
        Foundation::E_POINTER,
        Media::Audio::{
            Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia,
            eRender,
        },
        System::Com::{CLSCTX_ALL, CoCreateInstance},
    },
};

pub use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as SessionManager;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const METADATA_TIMEOUT: Duration = Duration::from_millis(600);

#[derive(Clone, Copy)]
pub enum PlaybackAction {
    Previous,
    Play,
    Pause,
    Next,
}

pub struct Playback {
    session: GlobalSystemMediaTransportControlsSession,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    status: Status,
    can_play: bool,
    can_pause: bool,
    can_previous: bool,
    can_next: bool,
}

pub async fn connect() -> Result<SessionManager> {
    with_timeout(RESPONSE_TIMEOUT, async {
        Ok(SessionManager::RequestAsync()?.await?)
    })
    .await
}

pub async fn playback(manager: &SessionManager) -> Result<Option<Playback>> {
    let current = manager.GetCurrentSession();
    let mut best = current
        .as_ref()
        .ok()
        .and_then(|session| Playback::from_session(session.clone()).ok().flatten());
    if !best.as_ref().is_some_and(Playback::controllable) {
        match manager.GetSessions() {
            Ok(sessions) => {
                for session in sessions {
                    let Ok(Some(candidate)) = Playback::from_session(session) else {
                        continue;
                    };
                    if best
                        .as_ref()
                        .is_none_or(|player| player.priority() < candidate.priority())
                    {
                        best = Some(candidate);
                    }
                }
            }
            Err(error) if best.is_none() && error.code().is_err() => return Err(error.into()),
            Err(_) => {}
        }
    }
    let Some(mut player) = best else {
        if let Err(error) = current
            && error.code().is_err()
            && error.code() != E_POINTER
        {
            return Err(error.into());
        }
        return Ok(None);
    };
    let metadata = with_timeout(METADATA_TIMEOUT, async {
        Ok(player.session.TryGetMediaPropertiesAsync()?.await?)
    })
    .await;
    if let Ok(metadata) = metadata {
        if let Ok(title) = metadata.Title()
            && !title.is_empty()
        {
            player.title = title.to_string_lossy();
        }
        player.artist = metadata.Artist().unwrap_or_default().to_string_lossy();
    }
    Ok(Some(player))
}

impl Playback {
    fn from_session(session: GlobalSystemMediaTransportControlsSession) -> Result<Option<Self>> {
        let info = session.GetPlaybackInfo()?;
        let status = info.PlaybackStatus()?;
        if status == Status::Closed {
            return Ok(None);
        }
        let controls = info.Controls()?;
        let toggle =
            status != Status::Changing && controls.IsPlayPauseToggleEnabled().unwrap_or(false);
        Ok(Some(Self {
            session,
            title: "当前媒体".into(),
            artist: String::new(),
            playing: status == Status::Playing,
            status,
            can_play: toggle || controls.IsPlayEnabled().unwrap_or(false),
            can_pause: toggle || controls.IsPauseEnabled().unwrap_or(false),
            can_previous: controls.IsPreviousEnabled().unwrap_or(false),
            can_next: controls.IsNextEnabled().unwrap_or(false),
        }))
    }

    pub fn preview_status(&self) -> Option<&'static str> {
        match self.status {
            Status::Playing => Some("正在播放"),
            Status::Paused => Some("已暂停"),
            _ => None,
        }
    }

    fn priority(&self) -> u8 {
        match (self.playing, self.controllable()) {
            (true, true) => 3,
            (false, true) => 2,
            (true, false) => 1,
            (false, false) => 0,
        }
    }

    fn controllable(&self) -> bool {
        self.supports(self.primary_action()) || self.can_previous || self.can_next
    }

    pub fn primary_action(&self) -> PlaybackAction {
        if self.playing {
            PlaybackAction::Pause
        } else {
            PlaybackAction::Play
        }
    }

    pub fn supports(&self, action: PlaybackAction) -> bool {
        match action {
            PlaybackAction::Previous => self.can_previous,
            PlaybackAction::Play => self.can_play,
            PlaybackAction::Pause => self.can_pause,
            PlaybackAction::Next => self.can_next,
        }
    }

    pub async fn control(
        session: GlobalSystemMediaTransportControlsSession,
        action: PlaybackAction,
    ) -> Result<()> {
        let performed = with_timeout(RESPONSE_TIMEOUT, async {
            match action {
                PlaybackAction::Previous => Ok(session.TrySkipPreviousAsync()?.await?),
                PlaybackAction::Next => Ok(session.TrySkipNextAsync()?.await?),
                PlaybackAction::Play => set_playing(&session, true).await,
                PlaybackAction::Pause => set_playing(&session, false).await,
            }
        })
        .await?;
        if !performed {
            bail!("媒体应用未执行此操作");
        }
        Ok(())
    }

    pub fn session(&self) -> GlobalSystemMediaTransportControlsSession {
        self.session.clone()
    }
}

async fn set_playing(
    session: &GlobalSystemMediaTransportControlsSession,
    play: bool,
) -> Result<bool> {
    let info = session.GetPlaybackInfo()?;
    let status = info.PlaybackStatus()?;
    if status == Status::Closed {
        bail!("媒体会话已关闭");
    }
    if status != Status::Changing && (status == Status::Playing) == play {
        return Ok(true);
    }
    let controls = info.Controls()?;
    let direct = if play && controls.IsPlayEnabled().unwrap_or(false) {
        Some(session.TryPlayAsync()?)
    } else if !play && controls.IsPauseEnabled().unwrap_or(false) {
        Some(session.TryPauseAsync()?)
    } else {
        None
    };
    if let Some(direct) = direct
        && direct.await?
    {
        return Ok(true);
    }
    // A toggle is only safe if it still moves the player toward the requested state.
    let info = session.GetPlaybackInfo()?;
    let status = info.PlaybackStatus()?;
    if matches!(status, Status::Closed | Status::Changing) {
        return Ok(false);
    }
    if (status == Status::Playing) == play {
        return Ok(true);
    }
    if info.Controls()?.IsPlayPauseToggleEnabled().unwrap_or(false) {
        return Ok(session.TryTogglePlayPauseAsync()?.await?);
    }
    Ok(false)
}

async fn with_timeout<T>(
    duration: Duration,
    operation: impl Future<Output = Result<T>>,
) -> Result<T> {
    smol::future::or(operation, async {
        smol::Timer::after(duration).await;
        bail!("等待 Windows 媒体响应超时");
    })
    .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Volume {
    pub percent: u8,
    pub muted: bool,
}

#[derive(Clone, Copy)]
pub enum VolumeAction {
    Level(u8),
    Step(i8),
    ToggleMute,
}

pub fn volume() -> Result<Volume> {
    read_volume(&endpoint()?)
}

pub fn change_volume(action: VolumeAction) -> Result<Volume> {
    let device = endpoint()?;
    let current = read_volume(&device)?;
    let percent = match action {
        VolumeAction::ToggleMute => {
            unsafe { device.SetMute(!current.muted, ptr::null())? };
            return read_volume(&device);
        }
        VolumeAction::Level(percent) => percent.min(100),
        VolumeAction::Step(step) => current.percent.saturating_add_signed(step).min(100),
    };
    if percent != current.percent {
        unsafe { device.SetMasterVolumeLevelScalar(f32::from(percent) / 100.0, ptr::null())? };
    }
    read_volume(&device)
}

fn endpoint() -> Result<IAudioEndpointVolume> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .context("未找到默认音频输出设备")?;
        Ok(device.Activate(CLSCTX_ALL, None)?)
    }
}

fn read_volume(device: &IAudioEndpointVolume) -> Result<Volume> {
    unsafe {
        Ok(Volume {
            percent: (device.GetMasterVolumeLevelScalar()?.clamp(0.0, 1.0) * 100.0).round() as u8,
            muted: device.GetMute()?.as_bool(),
        })
    }
}
