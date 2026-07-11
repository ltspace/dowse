use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 落盘在 `%LOCALAPPDATA%\dowse\config.json`，独立于索引目录。
/// 设计文档明确本里程碑不做设置界面——所有配置走托盘菜单和这个文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 上一次成功建索引的目标目录，托盘"重建索引"复用这个。
    pub target_dir: Option<PathBuf>,
    /// 玻璃效果开关，对应托盘菜单"关闭透明效果"。
    #[serde(default = "default_true")]
    pub transparency_enabled: bool,
    /// 设计文档要求"开机自启（可在托盘菜单关掉）"——默认开，用户关掉之后
    /// 重启应用不该又被悄悄打开。这个字段只记"用户是否主动关过"，
    /// 跟 autostart 插件自己的系统态分开：插件那边问的是"现在是不是开着"，
    /// 这边问的是"要不要在启动时把它摆回默认开"。
    #[serde(default)]
    pub autostart_user_disabled: bool,
    /// 全局呼出快捷键，格式跟 tauri-plugin-global-shortcut 的 `Shortcut::from_str`
    /// 一致（如 "Alt+Backquote"）。默认 Alt+`（反引号），原先的 Alt+Space
    /// 跟部分用户机器上的 PowerToys Run 冲突。
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
}

fn default_true() -> bool {
    true
}

fn default_hotkey() -> String {
    "Alt+Backquote".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            target_dir: None,
            transparency_enabled: true,
            autostart_user_disabled: false,
            hotkey: default_hotkey(),
        }
    }
}

fn config_path() -> Result<PathBuf> {
    let dirs =
        directories::ProjectDirs::from("", "", "dowse").context("拿不到用户数据目录")?;
    Ok(dirs.data_local_dir().join("config.json"))
}

/// 索引目录固定放在 `%LOCALAPPDATA%\dowse\index`，跟被索引的目录无关，
/// 和 dowse-cli 的约定保持一致，这样 CLI 建的索引浮窗也能直接用。
pub fn index_dir() -> Result<PathBuf> {
    let dirs =
        directories::ProjectDirs::from("", "", "dowse").context("拿不到用户数据目录")?;
    Ok(dirs.data_local_dir().join("index"))
}

pub fn load() -> AppConfig {
    let Ok(path) = config_path() else {
        return AppConfig::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return AppConfig::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save(cfg: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(cfg)?;
    std::fs::write(&path, bytes).context("写配置文件失败")?;
    Ok(())
}

/// 进程内的配置缓存，避免每次读写都打开文件。
pub struct ConfigState(pub Mutex<AppConfig>);

impl ConfigState {
    pub fn new() -> Self {
        Self(Mutex::new(load()))
    }

    pub fn get(&self) -> AppConfig {
        self.0.lock().expect("config mutex poisoned").clone()
    }

    pub fn set_target_dir(&self, dir: PathBuf) -> Result<()> {
        let mut guard = self.0.lock().expect("config mutex poisoned");
        guard.target_dir = Some(dir);
        save(&guard)
    }

    pub fn set_transparency_enabled(&self, enabled: bool) -> Result<()> {
        let mut guard = self.0.lock().expect("config mutex poisoned");
        guard.transparency_enabled = enabled;
        save(&guard)
    }

    pub fn set_autostart_user_disabled(&self, disabled: bool) -> Result<()> {
        let mut guard = self.0.lock().expect("config mutex poisoned");
        guard.autostart_user_disabled = disabled;
        save(&guard)
    }
}
