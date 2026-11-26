use anyhow::{Context, Result, bail};
use std::{env, ffi::OsStr, os::windows::ffi::OsStrExt};
use windows::{
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
            RegSetKeyValueW,
        },
    },
    core::{PCWSTR, w},
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("WinIslandPortable");

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Disabled,
    Current,
    AnotherLocation,
}

pub fn state() -> Result<State> {
    let mut size = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(State::Disabled);
    }
    status.ok().context("无法读取登录启动设置")?;
    // Windows limits Run commands to 260 characters.
    if size > 522 || size % 2 != 0 {
        return Ok(State::AnotherLocation);
    }
    let mut command = vec![0u16; size as usize / 2];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(command.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .context("无法读取已保存的启动路径")?;
    let existing = String::from_utf16_lossy(&command[..size as usize / 2]);
    let current = String::from_utf16_lossy(&startup_command()?);
    Ok(
        if existing
            .trim_end_matches('\0')
            .eq_ignore_ascii_case(current.trim_end_matches('\0'))
        {
            State::Current
        } else {
            State::AnotherLocation
        },
    )
}

pub fn set_enabled(enabled: bool) -> Result<State> {
    if enabled {
        let command = startup_command()?;
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * size_of::<u16>()) as u32,
            )
        }
        .ok()
        .context("无法设置登录后自动启动")?;
        Ok(State::Current)
    } else {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
        if status != ERROR_FILE_NOT_FOUND {
            status.ok().context("无法关闭登录后自动启动")?;
        }
        Ok(State::Disabled)
    }
}

fn startup_command() -> Result<Vec<u16>> {
    let path = env::current_exe().context("无法获取便携版路径")?;
    let quote = u16::from(b'"');
    let command: Vec<_> = [quote]
        .into_iter()
        .chain(OsStr::encode_wide(path.as_os_str()))
        .chain([quote, 0])
        .collect();
    if command.len() > 261 {
        bail!("便携版路径过长，请移动到更短的目录后开启自动启动");
    }
    Ok(command)
}
