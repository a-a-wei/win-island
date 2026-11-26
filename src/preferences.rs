use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preview {
    #[default]
    Details,
    Count,
    Hidden,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    #[default]
    SingleLine,
    MultiLine,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub hover_delay_ms: u64,
    pub collapse_delay_ms: u64,
    pub animate: bool,
    pub preview: Preview,
    pub media_preview: PreviewMode,
    pub wifi_preview: PreviewMode,
    pub bluetooth_preview: PreviewMode,
    pub visible_tabs: u16,
    pub clipboard_max_items: usize,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            hover_delay_ms: 0,
            collapse_delay_ms: 100,
            animate: true,
            preview: Preview::Details,
            media_preview: PreviewMode::SingleLine,
            wifi_preview: PreviewMode::SingleLine,
            bluetooth_preview: PreviewMode::SingleLine,
            visible_tabs: u16::MAX,
            clipboard_max_items: 50,
        }
    }
}

pub fn load() -> Result<Preferences> {
    let path = settings_path()?;
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Preferences::default()),
        Err(error) => return Err(error).context("无法读取本地设置"),
    };
    decode(&data).context("本地设置文件格式无效")
}

fn decode(data: &[u8]) -> Result<Preferences> {
    let mut fields: Map<String, Value> = serde_json::from_slice(data)?;
    // Older settings used one layout for every tab and a separate visibility flag.
    let shared_layout = fields
        .remove("preview_layout")
        .unwrap_or_else(|| Value::from("single_line"));
    for field in ["media_preview", "wifi_preview", "bluetooth_preview"] {
        let mode = fields.entry(field).or_insert_with(|| shared_layout.clone());
        if let Some(visible) = mode.as_bool() {
            *mode = if visible {
                shared_layout.clone()
            } else {
                Value::from("hidden")
            };
        }
    }
    let mut settings: Preferences = serde_json::from_value(Value::Object(fields))?;
    settings.hover_delay_ms = settings.hover_delay_ms.min(1000);
    settings.collapse_delay_ms = settings.collapse_delay_ms.min(2000);
    settings.clipboard_max_items = settings.clipboard_max_items.clamp(1, 500);
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::{PreviewMode, decode};

    #[test]
    fn legacy_visibility_and_shared_layout_are_preserved() {
        let settings =
            decode(br#"{"preview_layout":"multi_line","wifi_preview":false,"media_preview":true}"#)
                .unwrap();
        assert_eq!(settings.media_preview, PreviewMode::MultiLine);
        assert_eq!(settings.wifi_preview, PreviewMode::Hidden);
        assert_eq!(settings.bluetooth_preview, PreviewMode::MultiLine);

        let settings = decode(br#"{"wifi_preview":true}"#).unwrap();
        assert_eq!(settings.wifi_preview, PreviewMode::SingleLine);
    }

    #[test]
    fn individual_preview_modes_survive_saving_and_loading() {
        let mut settings = decode(br#"{"media_preview":"multi_line","wifi_preview":"multi_line","bluetooth_preview":"hidden"}"#).unwrap();
        settings.media_preview = PreviewMode::SingleLine;
        let restored = decode(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(restored.media_preview, PreviewMode::SingleLine);
        assert_eq!(restored.wifi_preview, PreviewMode::MultiLine);
        assert_eq!(restored.bluetooth_preview, PreviewMode::Hidden);
    }
}

pub fn save(settings: Preferences) -> Result<()> {
    let path = settings_path()?;
    let directory = path.parent().context("无效的设置路径")?;
    fs::create_dir_all(directory).context("无法创建设置目录")?;
    let mut file = tempfile::NamedTempFile::new_in(directory).context("无法创建设置文件")?;
    serde_json::to_writer_pretty(&mut file, &settings).context("无法写入本地设置")?;
    file.write_all(b"\n").context("无法写入本地设置")?;
    file.as_file().sync_all().context("无法保存本地设置")?;
    file.persist(&path).context("无法替换本地设置")?;
    Ok(())
}

pub(crate) fn settings_path() -> Result<PathBuf> {
    let local = env::var_os("LOCALAPPDATA").context("找不到 Windows 本地应用数据目录")?;
    Ok(PathBuf::from(local).join("WinIsland").join("settings.json"))
}
