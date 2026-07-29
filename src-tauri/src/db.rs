use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};
use thiserror::Error;

use crate::domain::{PadMapProfile, SessionSummary};

#[derive(Debug, Error)]
pub enum DbError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
}

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn open(app_data_dir: &Path) -> Result<Self, DbError> {
        fs::create_dir_all(app_data_dir)?;
        let path = db_path(app_data_dir);
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS pad_maps (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                device_name_hint TEXT,
                schema_version INTEGER NOT NULL,
                bindings_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY NOT NULL,
                song_path TEXT,
                drum_track_id INTEGER,
                started_at TEXT NOT NULL,
                ended_at TEXT NOT NULL,
                total_expected INTEGER NOT NULL,
                hit_counts_json TEXT NOT NULL,
                note_accuracy REAL NOT NULL,
                timing_mean_ms REAL,
                timing_abs_mean_ms REAL,
                score_percent INTEGER NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn list_pad_maps(&self) -> Result<Vec<PadMapProfile>, DbError> {
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, device_name_hint, schema_version, bindings_json, created_at, updated_at
             FROM pad_maps
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_profile)?;
        let mut profiles = Vec::new();
        for row in rows {
            profiles.push(row?);
        }
        Ok(profiles)
    }

    pub fn get_pad_map(&self, id: &str) -> Result<Option<PadMapProfile>, DbError> {
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, device_name_hint, schema_version, bindings_json, created_at, updated_at
             FROM pad_maps WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_profile)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn save_pad_map(&self, profile: &PadMapProfile) -> Result<(), DbError> {
        let bindings_json = serde_json::to_string(&profile.bindings)?;
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        conn.execute(
            "INSERT INTO pad_maps (id, name, device_name_hint, schema_version, bindings_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                device_name_hint = excluded.device_name_hint,
                schema_version = excluded.schema_version,
                bindings_json = excluded.bindings_json,
                updated_at = excluded.updated_at",
            params![
                profile.id,
                profile.name,
                profile.device_name_hint,
                profile.schema_version as i64,
                bindings_json,
                profile.created_at,
                profile.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn delete_pad_map(&self, id: &str) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        conn.execute("DELETE FROM pad_maps WHERE id = ?1", params![id])?;
        if let Ok(Some(active)) = self.get_setting_locked(&conn, "active_pad_map_id") {
            if active == id {
                conn.execute(
                    "DELETE FROM settings WHERE key = 'active_pad_map_id'",
                    [],
                )?;
            }
        }
        Ok(())
    }

    pub fn set_active_pad_map_id(&self, id: Option<&str>) -> Result<(), DbError> {
        self.set_setting("active_pad_map_id", id)
    }

    pub fn get_active_pad_map_id(&self) -> Result<Option<String>, DbError> {
        self.get_setting("active_pad_map_id")
    }

    pub fn get_latency_offset_ms(&self) -> Result<i64, DbError> {
        match self.get_setting("latency_offset_ms")? {
            Some(v) => Ok(v.parse().unwrap_or(0)),
            None => Ok(0),
        }
    }

    pub fn set_latency_offset_ms(&self, ms: i64) -> Result<(), DbError> {
        self.set_setting("latency_offset_ms", Some(&ms.to_string()))
    }

    pub fn get_audio_device_id(&self) -> Result<Option<String>, DbError> {
        self.get_setting("audio_device_id")
    }

    pub fn set_audio_device_id(&self, id: Option<&str>) -> Result<(), DbError> {
        self.set_setting("audio_device_id", id)
    }

    pub fn insert_session(&self, summary: &SessionSummary) -> Result<(), DbError> {
        let hit_counts_json = serde_json::to_string(&summary.hit_counts)?;
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        conn.execute(
            "INSERT INTO sessions (
                id, song_path, drum_track_id, started_at, ended_at,
                total_expected, hit_counts_json, note_accuracy,
                timing_mean_ms, timing_abs_mean_ms, score_percent
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                summary.id,
                summary.song_path,
                summary.drum_track_id.map(|id| id as i64),
                summary.started_at,
                summary.ended_at,
                summary.total_expected as i64,
                hit_counts_json,
                summary.note_accuracy,
                summary.timing_mean_ms,
                summary.timing_abs_mean_ms,
                summary.score_percent as i64,
            ],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        self.get_setting_locked(&conn, key)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|e| DbError::Message(e.to_string()))?;
        match value {
            Some(value) => {
                conn.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, value],
                )?;
            }
            None => {
                conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
            }
        }
        Ok(())
    }

    fn get_setting_locked(
        &self,
        conn: &Connection,
        key: &str,
    ) -> Result<Option<String>, DbError> {
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }
}

fn db_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("drumentor.sqlite")
}

fn row_to_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<PadMapProfile> {
    let bindings_json: String = row.get(4)?;
    let bindings = serde_json::from_str(&bindings_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(PadMapProfile {
        id: row.get(0)?,
        name: row.get(1)?,
        device_name_hint: row.get(2)?,
        schema_version: row.get::<_, i64>(3)? as u32,
        bindings,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}
