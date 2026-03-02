use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthProfile {
    pub name: String,
    pub login_url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub username_selector: Option<String>,
    pub password_selector: Option<String>,
    pub submit_selector: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ProfileFile {
    profiles: HashMap<String, AuthProfile>,
}

pub struct ProfileStore {
    file_path: PathBuf,
    data: ProfileFile,
}

impl ProfileStore {
    /// Load profiles from ~/.config/webprobe/profiles.json (creates file/dirs if missing).
    pub fn load() -> Result<Self> {
        let file_path = profile_path();
        let data = if file_path.exists() {
            let contents = std::fs::read_to_string(&file_path)
                .with_context(|| format!("Failed to read {}", file_path.display()))?;
            serde_json::from_str(&contents)
                .with_context(|| format!("Failed to parse {}", file_path.display()))?
        } else {
            ProfileFile::default()
        };
        Ok(Self { file_path, data })
    }

    /// Persist current state to disk.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let contents = serde_json::to_string_pretty(&self.data)
            .context("Failed to serialize profiles")?;
        std::fs::write(&self.file_path, contents)
            .with_context(|| format!("Failed to write {}", self.file_path.display()))?;
        Ok(())
    }

    /// All profiles sorted by name.
    pub fn list(&self) -> Vec<&AuthProfile> {
        let mut profiles: Vec<&AuthProfile> = self.data.profiles.values().collect();
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        profiles
    }

    /// Look up a profile by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&AuthProfile> {
        self.data.profiles.get(&name.to_lowercase())
    }

    /// Insert or replace a profile (keyed by profile.name lowercased).
    pub fn upsert(&mut self, profile: AuthProfile) {
        let key = profile.name.to_lowercase();
        self.data.profiles.insert(key, profile);
    }

    /// Delete by name. Returns true if it existed.
    pub fn delete(&mut self, name: &str) -> bool {
        self.data.profiles.remove(&name.to_lowercase()).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.data.profiles.is_empty()
    }
}

impl Default for ProfileStore {
    fn default() -> Self {
        Self {
            file_path: PathBuf::from("profiles.json"),
            data: ProfileFile::default(),
        }
    }
}

fn profile_path() -> PathBuf {
    if let Some(config) = dirs::config_dir() {
        config.join("webprobe").join("profiles.json")
    } else {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".webprobe").join("profiles.json")
    }
}
