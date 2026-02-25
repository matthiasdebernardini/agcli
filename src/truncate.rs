use std::fs;
use std::io;
use std::path::PathBuf;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Context-safe result for potentially large line-oriented output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TruncatedEntries {
    pub lines: usize,
    pub total: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output: Option<String>,
    pub entries: Vec<String>,
}

/// Truncate lines to `max_lines` and, when truncated, write full output to a temp file.
pub fn truncate_lines_with_file(
    lines: &[String],
    max_lines: usize,
    file_prefix: &str,
) -> io::Result<TruncatedEntries> {
    let total = lines.len();
    let safe_prefix = sanitize_prefix(file_prefix);

    if total <= max_lines {
        return Ok(TruncatedEntries {
            lines: total,
            total,
            truncated: false,
            full_output: None,
            entries: lines.to_vec(),
        });
    }

    let start = total.saturating_sub(max_lines);
    let entries = lines[start..].to_vec();
    let path = write_full_output(lines, &safe_prefix)?;

    Ok(TruncatedEntries {
        lines: entries.len(),
        total,
        truncated: true,
        full_output: Some(path.to_string_lossy().to_string()),
        entries,
    })
}

fn sanitize_prefix(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "agcli-output".to_string()
    } else {
        out
    }
}

fn write_full_output(lines: &[String], prefix: &str) -> io::Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let filename = format!("{prefix}-{}-{now}.log", process::id());
    let path = std::env::temp_dir().join(filename);
    fs::write(&path, lines.join("\n"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_for_small_output() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let result = truncate_lines_with_file(&lines, 10, "logs").expect("must truncate");
        assert!(!result.truncated);
        assert_eq!(result.total, 2);
        assert!(result.full_output.is_none());
    }

    #[test]
    fn truncation_writes_full_output_file() {
        let lines = (0..10).map(|idx| format!("line-{idx}")).collect::<Vec<_>>();
        let result = truncate_lines_with_file(&lines, 3, "logs").expect("must truncate");
        assert!(result.truncated);
        assert_eq!(result.lines, 3);
        assert_eq!(result.total, 10);
        let full_path = result.full_output.expect("expected temp file");
        assert!(std::path::Path::new(&full_path).exists());
    }
}
