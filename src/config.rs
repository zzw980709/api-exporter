//! 读取 `src/main/resources` 下的 `application*.yaml|yml|properties`，
//! 取出 `server.servlet.context-path` 和 `server.port`（按 profile 生效）。

use std::path::{Path, PathBuf};

use serde::Deserialize;
use walkdir::WalkDir;

#[derive(Debug, Default, Clone)]
pub struct ServerConfig {
    pub context_path: String,
    pub port: Option<u16>,
    pub source_file: Option<String>,
}

impl ServerConfig {
    pub fn describe(&self) -> String {
        format!(
            "context-path={}, port={}{}",
            if self.context_path.is_empty() { "(空)" } else { &self.context_path },
            self.port.map(|p| p.to_string()).unwrap_or_else(|| "(默认)".to_string()),
            self.source_file
                .as_ref()
                .map(|s| format!(", 来源={s}"))
                .unwrap_or_default()
        )
    }
}

pub fn read_server_config(project_dir: &Path, profile: &str, extra: Option<&Path>) -> ServerConfig {
    let mut config = ServerConfig::default();
    let mut files = locate_config_files(project_dir, profile);
    if let Some(extra) = extra {
        files.push(extra.to_path_buf());
    }
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".properties") {
            apply_properties(&mut config, &text, &file);
        } else {
            apply_yaml(&mut config, &text, profile, &file);
        }
    }
    config
}

fn locate_config_files(project_dir: &Path, profile: &str) -> Vec<PathBuf> {
    let resources = project_dir.join("src/main/resources");
    if !resources.is_dir() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = WalkDir::new(&resources)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_yaml_like = name.ends_with(".yaml") || name.ends_with(".yml") || name.ends_with(".properties");
            is_yaml_like
                && (name == "application.yaml"
                    || name == "application.yml"
                    || name == "application.properties"
                    || name.contains(&format!("-{profile}.")))
        })
        .collect();
    // 基础配置在前，profile 覆盖在后
    files.sort_by_key(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        (if name.starts_with("application.") { 0 } else { 1 }, p.to_string_lossy().into_owned())
    });
    files
}

fn apply_yaml(config: &mut ServerConfig, text: &str, profile: &str, file: &Path) {
    for document in serde_yaml::Deserializer::from_str(text) {
        let Ok(value) = serde_yaml::Value::deserialize(document) else {
            continue;
        };
        if !document_applies(&value, profile) {
            continue;
        }
        if let Some(server) = value.get("server") {
            if let Some(port) = server.get("port").and_then(|p| p.as_u64()) {
                config.port = u16::try_from(port).ok();
            } else if let Some(port) = server.get("port").and_then(|p| p.as_str()) {
                config.port = port.trim().parse().ok();
            }
            if let Some(ctx) = server
                .get("servlet")
                .and_then(|s| s.get("context-path").or_else(|| s.get("contextPath")))
                .and_then(|c| c.as_str())
            {
                config.context_path = normalize(ctx);
                config.source_file = Some(file.display().to_string());
            }
        }
    }
}

fn document_applies(value: &serde_yaml::Value, profile: &str) -> bool {
    let Some(on_profile) = value
        .get("spring")
        .and_then(|s| s.get("config"))
        .and_then(|c| c.get("activate"))
        .and_then(|a| a.get("on-profile"))
    else {
        return true;
    };
    match on_profile {
        serde_yaml::Value::String(s) => {
            if let Some(negated) = s.strip_prefix('!') {
                return negated != profile;
            }
            s.as_str() == profile
        }
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .any(|i| i.as_str().map(|s| s == profile).unwrap_or(false)),
        _ => true,
    }
}

fn apply_properties(config: &mut ServerConfig, text: &str, file: &Path) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key == "server.servlet.context-path" {
            config.context_path = normalize(value);
            config.source_file = Some(file.display().to_string());
        } else if key == "server.port" {
            config.port = value.parse().ok();
        }
    }
}

fn normalize(path: &str) -> String {
    let mut p = path.trim().to_string();
    if p.is_empty() || p == "/" {
        return String::new();
    }
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    while p.len() > 1 && p.ends_with('/') {
        p.pop();
    }
    p
}
