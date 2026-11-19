use anyhow::{Result, anyhow, ensure};
use serde_json::json;
use std::io::{Read, Write};

use crate::cli_text::text;
use crate::storage::{Paths, Settings};

pub fn configure(paths: &Paths, mut input: impl Read) -> Result<()> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    input
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!(text().read_input))?;
    ensure!(
        bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'),
        text().expected_object
    );
    // Serde diagnostics can contain the input value, including credentials.
    let settings: Settings =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!(text().invalid_settings))?;
    paths.save_settings(&settings)
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
