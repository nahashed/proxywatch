use crate::runtime_text::message;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    env, fs,
    fs::File,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    time::Duration,
};
use tempfile::NamedTempFile;

pub const PRESETS: &[(&str, &str)] = &[
    ("Telegram", "https://telegram.org/"),
    ("OpenAI", "https://openai.com/"),
    ("Claude", "https://claude.ai/"),
    ("Gemini", "https://gemini.google.com/"),
    ("YouTube", "https://www.youtube.com/"),
    ("Instagram", "https://www.instagram.com/"),
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Telegram {
    pub token: String,
    pub chat_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub language: crate::i18n::Language,
    pub instance_name: String,
    pub instance_description: String,
    pub subscription_url: String,
    pub http_port: Option<u16>,
    pub socks_port: Option<u16>,
    pub check_url: String,
    pub diagnostic_urls: Vec<String>,
    pub interval_seconds: u64,
    pub reserve_interval_seconds: u64,
    pub failure_threshold: u32,
    pub timeout_seconds: u64,
    pub refresh_seconds: u64,
    pub xray_binary: String,
    pub telegram: Option<Telegram>,
    pub smtp: Option<Smtp>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: crate::i18n::Language::default(),
            instance_name: hostname::get()
                .ok()
                .and_then(|name| name.into_string().ok())
                .filter(|name| valid_label(name, 64) && !name.trim().is_empty())
                .unwrap_or_else(|| "Proxywatch".into()),
            instance_description: String::new(),
            subscription_url: String::new(),
            http_port: Some(10809),
            socks_port: Some(10808),
            check_url: PRESETS[0].1.into(),
            diagnostic_urls: Vec::new(),
            interval_seconds: 30,
            reserve_interval_seconds: 900,
            failure_threshold: 3,
            timeout_seconds: 8,
            refresh_seconds: 3600,
            xray_binary: "xray".into(),
            telegram: None,
            smtp: None,
        }
    }
}

fn valid_label(value: &str, limit: usize) -> bool {
    value.chars().count() <= limit
        && !value.contains("://")
        && !value.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.instance_name.trim().is_empty()
                && valid_label(&self.instance_name, 64)
                && valid_label(&self.instance_description, 160),
            message!(invalid_labels)
        );
        for raw in std::iter::once(&self.subscription_url)
            .chain(std::iter::once(&self.check_url))
            .chain(&self.diagnostic_urls)
        {
            let url = reqwest::Url::parse(raw).ok();
            ensure!(
                url.is_some_and(|u| matches!(u.scheme(), "https" | "http")
                    && u.has_host()
                    && u.username().is_empty()
                    && u.password().is_none()),
                message!(invalid_urls)
            );
        }
        ensure!(
            self.http_port.is_some() || self.socks_port.is_some(),
            message!(enable_listener)
        );
        ensure!(
            self.http_port != Some(0)
                && self.socks_port != Some(0)
                && self.http_port != self.socks_port,
            message!(different_ports)
        );
        ensure!(
            (1..=86400).contains(&self.interval_seconds)
                && (1..=86400).contains(&self.reserve_interval_seconds)
                && (1..=300).contains(&self.timeout_seconds)
                && (1..=100).contains(&self.failure_threshold)
                && (1..=2_592_000).contains(&self.refresh_seconds),
            message!(invalid_intervals)
        );
        ensure!(
            !self.xray_binary.trim().is_empty() && self.diagnostic_urls.len() <= 16,
            message!(invalid_diagnostics)
        );
        if let Some(t) = &self.telegram {
            ensure!(
                !t.token.trim().is_empty() && !t.chat_id.trim().is_empty(),
                message!(telegram_credentials)
            );
        }
        if let Some(s) = &self.smtp {
            ensure!(
                !s.host.trim().is_empty()
                    && s.port != 0
                    && s.from.parse::<lettre::Address>().is_ok()
                    && s.to.parse::<lettre::Address>().is_ok()
                    && (s.username.is_empty() == s.password.is_empty()),
                message!(smtp_settings)
            );
        }
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub at: u64,
    pub kind: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub active: Option<String>,
    pub available: usize,
    pub total: usize,
    pub started_at: u64,
    pub changes: Vec<(String, bool)>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeHealth {
    pub available: Option<bool>,
    pub since: u64,
    pub checked_at: u64,
    pub latency_ms: Option<u64>,
    pub failures: u32,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub active: Option<String>,
    pub status: String,
    pub last_probe: Option<bool>,
    pub diagnostics: BTreeMap<String, bool>,
    #[serde(skip_serializing)]
    pub history: Vec<Event>,
    #[serde(skip)]
    pub(crate) saved_history: usize,
    pub nodes: BTreeMap<String, NodeHealth>,
    pub preferred: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
    pub outage: bool,
    pub pending: Vec<Event>,
    pub skipped: Vec<crate::subscription::SkippedNode>,
}

pub struct Paths {
    directory: PathBuf,
}

const HISTORY_WINDOW: usize = 500;

fn write_snapshot(transaction: &Transaction<'_>, state: &State) -> Result<()> {
    transaction.execute(
        "INSERT INTO snapshot(id, data) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        [serde_json::to_string(state)?],
    )?;
    let mut statement =
        transaction.prepare("INSERT INTO events(occurred_at, kind, data) VALUES (?1, ?2, ?3)")?;
    for event in state
        .history
        .get(state.saved_history..)
        .context(message!(truncated_history))?
    {
        statement.execute(params![event.at, event.kind, serde_json::to_string(event)?])?;
    }
    Ok(())
}

impl Paths {
    pub fn resolve(directory: Option<PathBuf>) -> Result<Self> {
        let directory = directory
            .or_else(|| env::var_os("XDG_CONFIG_HOME").map(|p| PathBuf::from(p).join("proxywatch")))
            .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".config/proxywatch")))
            .context(message!(choose_directory))?;
        Ok(Self { directory })
    }
    pub fn ensure_directory(&self) -> Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.directory)
            .context(message!(open_directory))?;
        Ok(())
    }
    pub fn has_settings(&self) -> bool {
        self.directory.join("settings.json").is_file()
    }
    pub fn load_settings(&self) -> Result<Settings> {
        let s: Settings = self
            .read("settings.json")?
            .context(message!(setup_required))?;
        s.validate()?;
        Ok(s)
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        s.validate()?;
        self.write("settings.json", s)
            .map_err(|_| anyhow::anyhow!(message!(save_settings)))
    }
    pub fn read_snapshot(&self) -> Result<State> {
        let path = self.directory.join("state.sqlite3");
        if path.try_exists()? {
            let connection =
                Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            connection.busy_timeout(Duration::from_secs(5))?;
            let version: u32 =
                connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
            ensure!(version <= 1, message!(database_version));
            if version == 1 {
                let snapshot: String =
                    connection.query_row("SELECT data FROM snapshot WHERE id = 1", [], |row| {
                        row.get(0)
                    })?;
                return Ok(serde_json::from_str(&snapshot)?);
            }
        }
        Ok(self.read("state.json")?.unwrap_or_default())
    }
    pub fn load_state(&self) -> Result<State> {
        let mut connection = self.database()?;
        let transaction = connection.transaction()?;
        let snapshot: String =
            transaction.query_row("SELECT data FROM snapshot WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let mut state: State = serde_json::from_str(&snapshot).context(message!(read_state))?;
        {
            let mut statement =
                transaction.prepare("SELECT data FROM events ORDER BY id DESC LIMIT ?1")?;
            let mut rows = statement.query([HISTORY_WINDOW])?;
            while let Some(row) = rows.next()? {
                let data: String = row.get(0)?;
                state
                    .history
                    .push(serde_json::from_str(&data).context(message!(read_event))?);
            }
        }
        state.history.reverse();
        state.saved_history = state.history.len();
        transaction.commit()?;
        Ok(state)
    }
    pub fn save_state(&self, state: &mut State) -> Result<()> {
        let mut connection = self.database()?;
        let transaction = connection.transaction()?;
        write_snapshot(&transaction, state)?;
        transaction.commit()?;
        let excess = state.history.len().saturating_sub(HISTORY_WINDOW);
        state.history.drain(..excess);
        state.saved_history = state.history.len();
        Ok(())
    }
    fn database(&self) -> Result<Connection> {
        let path = self.directory.join("state.sqlite3");
        match File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context(message!(create_database)),
        }
        let mut connection = Connection::open(path).context(message!(open_database))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let version = |connection: &Connection| -> rusqlite::Result<u32> {
            connection.pragma_query_value(None, "user_version", |row| row.get(0))
        };
        if version(&connection)? == 0 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if version(&transaction)? == 0 {
                let legacy: State = self.read("state.json")?.unwrap_or_default();
                transaction.execute_batch(
                    "CREATE TABLE snapshot(id INTEGER PRIMARY KEY CHECK(id = 1), data TEXT NOT NULL);
                     CREATE TABLE events(id INTEGER PRIMARY KEY, occurred_at INTEGER NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL);
                     PRAGMA user_version = 1;",
                )?;
                write_snapshot(&transaction, &legacy)?;
            }
            transaction.commit()?;
        }
        ensure!(version(&connection)? == 1, message!(database_version));
        Ok(connection)
    }
    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        match File::open(self.directory.join(name)) {
            Ok(f) => serde_json::from_reader(f)
                .map(Some)
                .map_err(|_| anyhow::anyhow!(message!(parse_file, name))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| message!(open_file, name)),
        }
    }
    pub fn write<T: Serialize>(&self, name: &str, data: &T) -> Result<()> {
        let mut f = NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(f.as_file_mut(), data)?;
        f.flush()?;
        f.as_file().sync_all()?;
        f.persist(self.directory.join(name))?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
    pub fn lock(&self) -> Result<File> {
        let f = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.directory.join("daemon.lock"))?;
        FileExt::try_lock_exclusive(&f).context(message!(lock_unavailable))?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn upgrades_legacy_settings_with_instance_name_and_independent_reserve_interval() -> Result<()>
    {
        let mut settings: Settings = serde_json::from_value(serde_json::json!({
            "subscription_url": "https://example.invalid/sub",
            "interval_seconds": 12
        }))?;
        settings.validate()?;
        assert_eq!(settings.language, crate::i18n::Language::Ru);
        assert!(!settings.instance_name.trim().is_empty());
        assert!(settings.instance_description.is_empty());
        assert_eq!(settings.interval_seconds, 12);
        assert_eq!(settings.reserve_interval_seconds, 900);
        settings.instance_name = "container\nsecond header".into();
        assert!(settings.validate().is_err());
        Ok(())
    }

    #[test]
    fn migrates_history_and_pending_once_and_preserves_node_state() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let paths = Paths::resolve(Some(temp.path().into()))?;
        let event = serde_json::json!({"at": 1, "kind": "outage", "from": "first", "to": null});
        paths.write(
            "state.json",
            &serde_json::json!({
                "history": [event], "pending": [event], "outage": true
            }),
        )?;
        let mut state = paths.load_state()?;
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.pending.len(), 1);
        assert!(state.outage);
        state.pending.clear();
        state.nodes.insert(
            "other".into(),
            NodeHealth {
                available: Some(true),
                since: 20,
                ..Default::default()
            },
        );
        state.history.push(Event {
            at: 20,
            kind: "recovery".into(),
            ..Default::default()
        });
        paths.save_state(&mut state)?;
        paths.save_state(&mut state)?;
        paths.write("state.json", &serde_json::json!({"history": ["invalid"]}))?;
        let restored = paths.load_state()?;
        assert_eq!(
            restored
                .history
                .iter()
                .map(|event| event.at)
                .collect::<Vec<_>>(),
            [1, 20]
        );
        assert!(restored.pending.is_empty());
        assert_eq!(restored.nodes["other"].since, 20);
        let mode = fs::metadata(temp.path().join("state.sqlite3"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        Ok(())
    }

    #[test]
    fn keeps_database_history_beyond_display_window() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let paths = Paths::resolve(Some(temp.path().into()))?;
        let mut state = paths.load_state()?;
        for at in 0..=HISTORY_WINDOW as u64 {
            state.history.push(Event {
                at,
                kind: "switch".into(),
                ..Default::default()
            });
        }
        paths.save_state(&mut state)?;
        assert_eq!(state.history.len(), HISTORY_WINDOW);
        assert_eq!(paths.load_state()?.history[0].at, 1);
        let count: usize =
            paths
                .database()?
                .query_row("SELECT count(*) FROM events", [], |row| row.get(0))?;
        assert_eq!(count, HISTORY_WINDOW + 1);
        Ok(())
    }

    #[test]
    fn failed_transaction_never_partially_advances_state_or_history() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let paths = Paths::resolve(Some(temp.path().into()))?;
        let mut state = paths.load_state()?;
        state.status = "changed".into();
        state.history.push(Event {
            at: u64::MAX,
            ..Default::default()
        });
        assert!(paths.save_state(&mut state).is_err());
        let restored = paths.load_state()?;
        assert!(restored.history.is_empty());
        assert!(restored.status.is_empty());
        state.history[0].at = 123;
        paths.save_state(&mut state)?;
        assert_eq!(paths.load_state()?.history.len(), 1);
        Ok(())
    }
}
