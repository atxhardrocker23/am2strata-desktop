use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const MEMORY_DIR: &str = r"G:\Strata-3060\memory";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: 120,
            y: 60,
            width: 1440,
            height: 920,
            maximized: false,
        }
    }
}

pub fn window_state_path() -> PathBuf {
    Path::new(MEMORY_DIR).join("desktop-window.json")
}

pub fn chats_backup_path() -> PathBuf {
    Path::new(MEMORY_DIR).join("desktop-chats.json")
}

pub fn pending_model_path() -> PathBuf {
    Path::new(MEMORY_DIR).join("desktop-pending-model.txt")
}

pub fn load_window_state() -> WindowState {
    let path = window_state_path();
    let Ok(text) = fs::read_to_string(path) else {
        return WindowState::default();
    };
    let mut st: WindowState = serde_json::from_str(&text).unwrap_or_default();
    st.width = st.width.clamp(800, 7680);
    st.height = st.height.clamp(560, 4320);
    if st.x < -3000 || st.x > 7000 {
        st.x = 120;
    }
    if st.y < -2000 || st.y > 4000 {
        st.y = 60;
    }
    st
}

pub fn save_window_state(state: &WindowState) {
    let _ = fs::create_dir_all(MEMORY_DIR);
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = write_atomic(&window_state_path(), json.as_bytes());
    }
}

pub fn load_synced_storage() -> BTreeMap<String, String> {
    let path = chats_backup_path();
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn update_synced_storage_key(key: &str, value: Option<&str>) {
    if !key.starts_with("strata.") {
        return;
    }
    let mut map = load_synced_storage();
    match value {
        Some(v) => {
            map.insert(key.to_string(), v.to_string());
        }
        None => {
            map.remove(key);
        }
    }
    let _ = fs::create_dir_all(MEMORY_DIR);
    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let _ = write_atomic(&chats_backup_path(), json.as_bytes());
    }
}

pub fn write_pending_model(model: Option<&str>) {
    let _ = fs::create_dir_all(MEMORY_DIR);
    let path = pending_model_path();
    match model {
        Some(m) if !m.trim().is_empty() => {
            let _ = fs::write(path, m.trim());
        }
        _ => {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn take_pending_model() -> Option<String> {
    let path = pending_model_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return None;
    };
    let _ = fs::remove_file(path);
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = target.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, target)
}
