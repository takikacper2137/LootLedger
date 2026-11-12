//! config.json, shared with LootLedger 0.7.1. Unknown and legacy detector fields are kept when it is rewritten.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub struct Config {
    path: PathBuf,
    data: Map<String, Value>,
}

impl Config {
    pub fn read(path: &Path) -> Config {
        let data = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        Config {
            path: path.to_owned(),
            data,
        }
    }

    pub fn replay_folder(&self) -> PathBuf {
        match self
            .data
            .get("replay_folder")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            Some(folder) => PathBuf::from(folder),
            None => default_folder(),
        }
    }
    pub fn count_broken(&self) -> bool {
        self.data
            .get("count_broken")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }
    pub fn auto_watch(&self) -> bool {
        self.data
            .get("auto_watch")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }
    /// Whether this PC lets friends connect to share matches (off unless the player turns it on).
    pub fn share_host(&self) -> bool {
        self.data
            .get("share_host")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    /// Text settings of match sharing: `share_password` (hosting), `share_peer` and `share_join_password` (connecting to
    /// a friend), `share_relay` and `share_room` (the online room).
    pub fn share_text(&self, name: &str) -> String {
        self.data
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    }
    pub fn scan_seconds(&self) -> i64 {
        self.data
            .get("scan_seconds")
            .and_then(Value::as_i64)
            .filter(|s| (5..=600).contains(s))
            .unwrap_or(20)
    }
    pub fn hotkey(&self, name: &str) -> String {
        let default = if name == "hotkey_add" {
            "Ctrl+Alt+F8"
        } else {
            "Ctrl+Alt+F9"
        };
        self.data
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_owned()
    }

    /// Change one field and save atomically; invalid values are refused with a reason.
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), String> {
        match name {
            "scan_seconds" if !value.as_i64().is_some_and(|s| (5..=600).contains(&s)) => {
                return Err("Replay check interval must be 5–600 seconds".into());
            }
            "replay_folder"
            | "hotkey_add"
            | "hotkey_undo"
            | "share_password"
            | "share_peer"
            | "share_join_password"
                if !value.is_string() =>
            {
                return Err("Expected text".into());
            }
            "count_broken" | "auto_watch" | "share_host" if !value.is_boolean() => {
                return Err("Expected on or off".into());
            }
            _ => {}
        }
        let mut data = self.data.clone();
        data.insert(name.into(), value);
        for (key, default) in [
            ("count_broken", json!(true)),
            ("auto_watch", json!(true)),
            ("scan_seconds", json!(20)),
        ] {
            data.entry(key).or_insert(default);
        }
        let temp = self.path.with_extension("tmp");
        std::fs::create_dir_all(self.path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?;
        std::fs::write(
            &temp,
            serde_json::to_vec_pretty(&Value::Object(data.clone())).map_err(|e| e.to_string())?,
        )
        .and_then(|_| std::fs::rename(&temp, &self.path))
        .map_err(|e| e.to_string())?;
        self.data = data;
        Ok(())
    }
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("LootLedger")
}

pub fn default_folder() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FortniteGame")
        .join("Saved")
        .join("Demos")
}

pub fn fortnite_log() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FortniteGame")
        .join("Saved")
        .join("Logs")
        .join("FortniteGame.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_fields_survive_a_rewrite() {
        let dir = std::env::temp_dir().join(format!("lootledger-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"detector_backend": "model", "scan_seconds": 45, "regions": {"chest": [0.1, 0.1, 0.1, 0.1]}}"#).unwrap();
        let mut config = Config::read(&path);
        assert_eq!(config.scan_seconds(), 45);
        config.set("auto_watch", json!(false)).unwrap();
        assert!(config.set("scan_seconds", json!(2)).is_err());
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["detector_backend"], "model");
        assert_eq!(saved["regions"]["chest"][0], 0.1);
        assert_eq!(saved["auto_watch"], false);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn match_sharing_is_off_until_chosen_and_remembered() {
        let dir = std::env::temp_dir().join(format!("lootledger-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let mut config = Config::read(&path);
        assert!(!config.share_host());
        assert_eq!(config.share_text("share_peer"), "");
        config.set("share_host", json!(true)).unwrap();
        config.set("share_peer", json!("100.64.1.2")).unwrap();
        assert!(config.set("share_host", json!("yes")).is_err());
        let again = Config::read(&path);
        assert!(again.share_host());
        assert_eq!(again.share_text("share_peer"), "100.64.1.2");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
