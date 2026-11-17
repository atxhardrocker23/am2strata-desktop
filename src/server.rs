use serde::{Deserialize, Serialize};
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const CREATE_NO_WINDOW: u32 = 0x08000000;

pub const BASE_DIR: &str = r"G:\Strata-3060";
pub const AM2_DIR: &str = r"G:\Strata-3060\am2strata";
pub const CONFIGS_DIR: &str = r"G:\Strata-3060\am2strata\configs";
pub const WEB_DIR: &str = r"G:\Strata-3060\am2strata\web";
pub const AM2_EXE: &str = r"G:\Strata-3060\am2strata\am2strata.exe";
pub const AM2_MEDIA_EXE: &str = r"G:\Strata-3060\am2strata\am2strata-media.exe";
pub const AM2_WORKSHOP_EXE: &str = r"G:\Strata-3060\am2strata\am2strata-workshop.exe";
pub const MOLD_HOME_DIR: &str = r"G:\Strata-3060\media";
pub const POPPLER_BIN: &str = r"G:\Strata-3060\am2strata\poppler\bin";
pub const CUDA_BIN: &str = r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.0\bin";
pub const CUDA_BIN_X64: &str = r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.0\bin\x64";
pub const ANDROID_PLATFORM_TOOLS: &str = r"C:\Users\Kenny\AppData\Local\Microsoft\WinGet\Packages\Google.PlatformTools_Microsoft.Winget.Source_8wekyb3d8bbwe\platform-tools";
pub const ANDROID_CLI_BIN: &str = r"C:\Users\Kenny\AppData\AndroidCLI";
pub const WINGET_LINKS_BIN: &str = r"C:\Users\Kenny\AppData\Local\Microsoft\WinGet\Links";
pub const SERVER_HOST: &str = "127.0.0.1";
pub const SERVER_PORT: u16 = 8088;
pub const SERVER_URL: &str = "http://127.0.0.1:8088/";
pub const DEFAULT_MODEL: &str = "cyber-tiel-coder-nanoplus";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfigEntry {
    pub model_name: String,
    pub display_name: String,
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HealthResponse {
    pub status: Option<String>,
    pub model: Option<String>,
    pub loaded: Option<bool>,
    pub images: Option<bool>,
    pub video: Option<bool>,
    pub image_generation: Option<bool>,
    pub alerts: Option<u64>,
    pub server: Option<String>,
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .into()
}

pub fn discover_configs() -> Vec<ModelConfigEntry> {
    let mut entries = Vec::new();
    let Ok(rd) = fs::read_dir(CONFIGS_DIR) else {
        return entries;
    };
    for item in rd.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(model_name) = val.get("model_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let display_name = val
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(model_name)
            .to_string();
        let log_path = val
            .get("log")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        entries.push(ModelConfigEntry {
            model_name: model_name.to_string(),
            display_name,
            log_path,
        });
    }
    entries.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    entries
}

pub fn get_health() -> Option<HealthResponse> {
    let url = format!("http://{SERVER_HOST}:{SERVER_PORT}/health");
    let agent = http_agent();
    let mut resp = agent.get(&url).call().ok()?;
    resp.body_mut().read_json::<HealthResponse>().ok()
}

pub fn start_server_headless(model_override: Option<&str>) -> Result<(), String> {
    if get_health().is_some() {
        return Ok(());
    }

    // Stop any leftover orphaned engine or WSL instance competing for the RTX 3060 12GB VRAM
    stop_server_processes();

    let model = model_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| discover_configs().first().map(|c| c.model_name.clone()))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!(
        "{POPPLER_BIN};{CUDA_BIN};{CUDA_BIN_X64};{ANDROID_PLATFORM_TOOLS};{ANDROID_CLI_BIN};{WINGET_LINKS_BIN};{existing_path}"
    );

    let mut cmd = Command::new(AM2_EXE);
    cmd.current_dir(AM2_DIR)
        .env("PATH", new_path)
        .env("AM2STRATA_MEDIA_EXE", AM2_MEDIA_EXE)
        .env("AM2STRATA_WORKSHOP_EXE", AM2_WORKSHOP_EXE)
        .env("MOLD_HOME", MOLD_HOME_DIR)
        .args([
            "--models-dir",
            CONFIGS_DIR,
            "--model",
            &model,
            "--port",
            &SERVER_PORT.to_string(),
            "--host",
            SERVER_HOST,
            "--web-dir",
            WEB_DIR,
            "--data-dir",
            BASE_DIR,
            "--api-monitor",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);

    cmd.spawn()
        .map_err(|e| format!("Failed to start am2strata.exe: {e}"))?;

    Ok(())
}

pub fn stop_server_processes() {
    for exe in [
        "am2strata.exe",
        "am2strata-run.exe",
        "am2strata-media.exe",
        "am2strata-workshop.exe",
        "strata-poly.exe",
        "strata-vision.exe",
    ] {
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", exe])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

pub fn switch_model(model_id: &str) -> Result<String, String> {
    if get_health().is_none() {
        start_server_headless(Some(model_id))?;
        return Ok(format!("Starting server with {model_id}"));
    }

    let url = format!("http://{SERVER_HOST}:{SERVER_PORT}/api/models/load");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into();

    let payload = serde_json::json!({ "model": model_id });
    let mut resp = agent
        .post(&url)
        .send_json(&payload)
        .map_err(|e| format!("Failed to switch model: {e}"))?;
    let val = resp
        .body_mut()
        .read_json::<serde_json::Value>()
        .unwrap_or_default();
    let loaded_id = val
        .pointer("/model/id")
        .and_then(|v| v.as_str())
        .unwrap_or(model_id);
    Ok(format!("Loaded {loaded_id}"))
}

pub fn unload_model() -> Result<String, String> {
    if get_health().is_none() {
        return Ok("Server is not running".to_string());
    }
    let url = format!("http://{SERVER_HOST}:{SERVER_PORT}/api/models/unload");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    agent
        .post(&url)
        .header("Content-Type", "application/json")
        .send("{}")
        .map_err(|e| format!("Failed to unload model: {e}"))?;
    Ok("Model unloaded; VRAM freed".to_string())
}

pub fn latest_log_tail(max_lines: usize) -> Vec<String> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(rd) = fs::read_dir(BASE_DIR) {
        for entry in rd.flatten() {
            let p = entry.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with("strata-") && name.ends_with(".log") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
                            newest = Some((mtime, p));
                        }
                    }
                }
            }
        }
    }
    let Some((_, path)) = newest else {
        return Vec::new();
    };
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].to_vec()
}

pub fn run_github_update_check() -> Result<String, String> {
    // 1. Run the existing Python tracker
    let py = Command::new("python")
        .arg(r"G:\Strata-3060\tools\check_github_updates.py")
        .current_dir(BASE_DIR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    if let Err(e) = py {
        return Err(format!("Could not run check_github_updates.py: {e}"));
    }

    // 2. Also fetch latest remote commits on local git clones if present
    let mut local_git_notes = String::new();
    for (label, repo_dir) in [
        ("am2strata (abhaymin/am2strata)", r"G:\Strata-3060\am2strata-src"),
        ("PolyStrata (VecSzn/PolyStrata)", r"G:\PolyStrata"),
    ] {
        if Path::new(repo_dir).join(".git").exists() {
            let _ = Command::new("git")
                .args(["-C", repo_dir, "fetch", "--quiet"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status();

            if let Ok(out) = Command::new("git")
                .args(["-C", repo_dir, "log", "-n", "6", "--oneline", "HEAD..@{u}"])
                .stdin(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                let ahead = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !ahead.is_empty() {
                    local_git_notes.push_str(&format!("\n### New upstream commits for {label}\n```\n{ahead}\n```\n"));
                }
            }
        }
    }

    let report_path = Path::new(BASE_DIR).join("LATEST_GITHUB_UPDATES.md");
    let mut report = fs::read_to_string(&report_path)
        .unwrap_or_else(|_| "Report generated, but LATEST_GITHUB_UPDATES.md could not be read.".to_string());
    if !local_git_notes.is_empty() {
        report.push_str("\n---\n## Local Git Clone Status\n");
        report.push_str(&local_git_notes);
    }
    Ok(report)
}
