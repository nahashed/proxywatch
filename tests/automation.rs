use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn cli(directory: &Path, args: &[&str], input: &[u8]) -> Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_proxywatch"))
        .arg("--directory")
        .arg(directory)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing stdin")?
        .write_all(input)?;
    Ok(child.wait_with_output()?)
}

#[test]
fn configure_is_atomic_and_errors_do_not_echo_input() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let args = ["configure", "--stdin"];
    let settings_path = directory.path().join("settings.json");
    let input = json!({"subscription_url": "https://example.invalid/private-canary"});
    let success = cli(directory.path(), &args, &serde_json::to_vec(&input)?)?;
    assert!(success.status.success());
    assert!(success.stdout.is_empty() && success.stderr.is_empty());
    let saved = fs::read(&settings_path)?;
    for invalid in [
        r#"{"http_port":"private-canary"}"#,
        r#"{"subscription_url":"private-canary"}"#,
        r#"{"subscription_url":"https://example.invalid","http_por":"private-canary"}"#,
        r#"{"subscription_url":"https://example.invalid","telegram":{"token":"private-canary","chat_id":""}}"#,
        r#"{"incomplete":"private-canary""#,
        "[]",
    ] {
        let failure = cli(directory.path(), &args, invalid.as_bytes())?;
        assert_eq!(failure.status.code(), Some(1));
        assert!(failure.stdout.is_empty());
        assert!(!String::from_utf8(failure.stderr)?.contains("private-canary"));
        assert_eq!(fs::read(&settings_path)?, saved);
    }
    for args in [
        &["configure"][..],
        &["status"],
        &["configure", "--stdin", "private-canary"],
    ] {
        let failure = cli(directory.path(), args, &[])?;
        assert_eq!(failure.status.code(), Some(2));
        assert!(!String::from_utf8(failure.stderr)?.contains("private-canary"));
    }
    Ok(())
}

#[test]
fn status_reads_only_a_public_summary_without_initializing_storage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("instance");
    let args = ["status", "--json"];
    let output = cli(&path, &args, &[])?;
    assert!(output.status.success() && output.stderr.is_empty());
    let empty: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(empty["status"], "unknown");
    assert_eq!(empty["configured"], false);
    assert!(!path.exists());
    fs::create_dir(&path)?;
    let snapshot = json!({
        "active":"private-canary","preferred":"private-canary","updated_at":15,
        "status":"healthy","nodes":{"private-canary":{"available":true}},
        "diagnostics":{"https://example.invalid/private-canary":true},
        "pending":[{"kind":"private-canary"}]
    });
    fs::write(path.join("state.json"), serde_json::to_vec(&snapshot)?)?;
    let output = cli(&path, &args, &[])?;
    assert!(output.status.success() && output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        report,
        json!({"schema_version":1,"configured":false,"status":"healthy","updated_at":15,"active_is_preferred":true,"nodes_total":1,"nodes_available":1,"pending_notifications":1})
    );
    let database = path.join("state.sqlite3");
    assert!(!database.exists());
    let connection = rusqlite::Connection::open(&database)?;
    connection.execute_batch(
        "CREATE TABLE snapshot(id INTEGER PRIMARY KEY, data TEXT NOT NULL); PRAGMA user_version=1;",
    )?;
    connection.execute(
        "INSERT INTO snapshot VALUES (1, ?1)",
        [r#"{"status":"private-canary","updated_at":27}"#],
    )?;
    drop(connection);
    let output = cli(&path, &args, &[])?;
    assert!(output.status.success() && output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-canary"));
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["status"], "unknown");
    assert_eq!(report["updated_at"], 27);
    Ok(())
}
