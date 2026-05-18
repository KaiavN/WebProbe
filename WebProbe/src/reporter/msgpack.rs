use crate::types::Report;
use anyhow::{Context, Result};
use rmp_serde::encode::to_vec;
use std::path::Path;

/// Write the report as MessagePack (compact binary format)
pub fn write_report(report: &Report, path: &Path) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
    }
    let data = to_vec(report).context("Failed to serialize report to MessagePack")?;
    std::fs::write(path, data)
        .with_context(|| format!("Failed to write report to {}", path.display()))?;
    Ok(())
}
