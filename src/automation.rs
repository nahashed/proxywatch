use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::io::{Read, Write};

use crate::cli_text::text;
use crate::storage::{Paths, Settings};

pub fn configure(paths: &Paths, mut input: impl Read, merge: bool) -> Result<()> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    input
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!(text().read_input))?;
    ensure!(
        bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'),
        text().expected_object
    );
    // Serde diagnostics can contain the input value, including credentials.
    let invalid = |_| anyhow!(text().invalid_settings);
    if merge {
        // Mapping rejects duplicate keys recursively. The parser still accepts JSON syntax only.
        let object: serde_yaml_ng::Mapping = serde_json::from_slice(&bytes).map_err(invalid)?;
        let patch = serde_json::to_value(object).map_err(invalid)?;
        paths.update_settings(|| {
            let mut current = serde_json::to_value(paths.load_settings()?).map_err(invalid)?;
            apply_patch(&mut current, patch);
            serde_json::from_value(current).map_err(invalid)
        })
    } else {
        let settings: Settings = serde_json::from_slice(&bytes).map_err(invalid)?;
        paths.save_settings(&settings)
    }
}

fn apply_patch(current: &mut Value, patch: Value) {
    match (current, patch) {
        (Value::Object(current), Value::Object(patch)) => {
            for (key, value) in patch {
                apply_patch(current.entry(key).or_insert(Value::Null), value);
            }
        }
        (current, value) => *current = value,
    }
}

pub fn status(paths: &Paths, mut output: impl Write) -> Result<()> {
    let state = paths
        .read_snapshot()
        .map_err(|_| anyhow!(text().read_state))?;
    let status = match state.status.as_str() {
        value @ ("starting" | "healthy" | "degraded" | "outage" | "stopped") => value,
        _ => "unknown",
    };
    // Subscription labels, diagnostic URLs and queued messages may contain credentials.
    let summary = json!({
        "schema_version": 1,
        "configured": paths.has_settings(),
        "status": status,
        "updated_at": state.updated_at,
        "active_is_preferred": state.active.as_ref().zip(state.preferred.as_ref()).map(|(active, preferred)| active == preferred),
        "nodes_total": state.nodes.len(),
        "nodes_available": state.nodes.values().filter(|node| node.available == Some(true)).count(),
        "pending_notifications": state.pending.len(),
    });
    serde_json::to_writer(&mut output, &summary).map_err(|_| anyhow!(text().write_status))?;
    writeln!(output).map_err(|_| anyhow!(text().write_status))?;
    Ok(())
}
