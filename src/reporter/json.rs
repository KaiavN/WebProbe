use crate::types::Report;
use anyhow::{Context, Result};
use std::path::Path;

pub fn write_report(report: &Report, path: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(report)
        .context("Failed to serialize report to JSON")?;
    std::fs::write(path, json)
        .with_context(|| format!("Failed to write report to {}", path.display()))?;
    Ok(())
}
