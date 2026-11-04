//! A small record per job in progress (state/jobs/<id>.json): enough for a restarted daemon to find a job it lost, stop the agent if it
//! is still running, and tell Wheelhouse how that job ended. Port of `daemon/src/state.mjs`.

use crate::config::write_private;
use serde_json::{Map, Value};
use std::io;
use std::path::{Path, PathBuf};

pub struct JobRecords {
    dir: PathBuf,
}

impl JobRecords {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        JobRecords { dir: dir.into() }
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Save a record; it must have a string `id`.
    pub fn write(&self, record: &Value) -> io::Result<()> {
        let id = record.get("id").and_then(Value::as_str).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a job record needs a string id",
            )
        })?;
        std::fs::create_dir_all(&self.dir)?;
        let mut text = record.to_string();
        text.push('\n');
        write_private(&self.file(id), text.as_bytes())
    }

    /// Merge `patch` into the record, when there is one.
    pub fn update(&self, id: &str, patch: Map<String, Value>) -> io::Result<()> {
        if let Some(Value::Object(mut r)) = self.read(id) {
            r.extend(patch);
            self.write(&Value::Object(r))?;
        }
        Ok(())
    }

    pub fn read(&self, id: &str) -> Option<Value> {
        std::fs::read_to_string(self.file(id))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    }

    pub fn remove(&self, id: &str) -> io::Result<()> {
        match std::fs::remove_file(self.file(id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Every readable record, in file-name order.
    pub fn all(&self) -> Vec<Value> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort();
        names
            .iter()
            .filter_map(|n| self.read(&n[..n.len() - 5]))
            .collect()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wh-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn write_read_update_remove_round_trip() {
        let dir = scratch("jobs");
        let jobs = JobRecords::new(&dir);
        assert!(jobs.all().is_empty(), "no folder yet is no records");
        jobs.write(&json!({"id": "b", "state": "running"})).unwrap();
        jobs.write(&json!({"id": "a", "state": "queued"})).unwrap();
        assert_eq!(jobs.read("a").unwrap()["state"], json!("queued"));
        jobs.update("a", Map::from_iter([("pid".to_string(), json!(42))]))
            .unwrap();
        assert_eq!(
            jobs.read("a").unwrap(),
            json!({"id": "a", "state": "queued", "pid": 42})
        );
        assert_eq!(
            jobs.all()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        jobs.remove("a").unwrap();
        jobs.remove("a").unwrap();
        assert!(jobs.read("a").is_none());
        assert!(jobs.write(&json!({"state": "no id"})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
