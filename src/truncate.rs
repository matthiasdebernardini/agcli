use std::io;
use std::path::PathBuf;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::fs;
use tokio::io::{AsyncWriteExt, BufWriter};

/// Context-safe result for potentially large line-oriented output.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct TruncatedEntries {
    pub lines: usize,
    pub total: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output: Option<String>,
    pub entries: Vec<String>,
}

impl TruncatedEntries {
    /// Remove the temp file written during truncation, if any.
    ///
    /// Idempotent: succeeds even if the file was already removed.
    pub async fn cleanup(&self) -> io::Result<()> {
        if let Some(path) = &self.full_output {
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Truncate lines to `max_lines` and, when truncated, write full output to a temp file.
pub async fn truncate_lines_with_file(
    lines: Vec<String>,
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
            entries: lines,
        });
    }

    let path = write_full_output(&lines, &safe_prefix).await?;
    let start = total.saturating_sub(max_lines);
    let entries: Vec<String> = lines.into_iter().skip(start).collect();

    Ok(TruncatedEntries {
        lines: entries.len(),
        total,
        truncated: true,
        full_output: Some(path.to_string_lossy().into_owned()),
        entries,
    })
}

fn sanitize_prefix(value: &str) -> String {
    let out: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    if out.is_empty() {
        "agcli-output".to_string()
    } else {
        out
    }
}

async fn write_full_output(lines: &[String], prefix: &str) -> io::Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_nanos();
    let pid = process::id();

    for attempt in 0u32..5 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let filename = format!("{prefix}-{pid}-{now}{suffix}.log");
        let path = std::env::temp_dir().join(filename);

        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);

        #[cfg(unix)]
        {
            opts.mode(0o600);
        }

        match opts.open(&path).await {
            Ok(file) => {
                let mut writer = BufWriter::new(file);
                if let Some((last, rest)) = lines.split_last() {
                    for line in rest {
                        writer.write_all(line.as_bytes()).await?;
                        writer.write_all(b"\n").await?;
                    }
                    writer.write_all(last.as_bytes()).await?;
                }
                writer.flush().await?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create unique temp file after 5 attempts",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_truncation_for_small_output() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let result = truncate_lines_with_file(lines, 10, "logs")
            .await
            .expect("must truncate");
        assert!(!result.truncated);
        assert_eq!(result.total, 2);
        assert!(result.full_output.is_none());
    }

    #[tokio::test]
    async fn truncation_writes_full_output_file() {
        let lines = (0..10).map(|idx| format!("line-{idx}")).collect::<Vec<_>>();
        let result = truncate_lines_with_file(lines, 3, "logs")
            .await
            .expect("must truncate");
        assert!(result.truncated);
        assert_eq!(result.lines, 3);
        assert_eq!(result.total, 10);
        let full_path = result.full_output.as_ref().expect("expected temp file");
        assert!(std::path::Path::new(full_path).exists());
        result.cleanup().await.expect("cleanup must succeed");
        assert!(!std::path::Path::new(full_path).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn temp_file_has_restrictive_permissions() {
        use std::os::unix::fs::MetadataExt;
        let lines = (0..10).map(|idx| format!("line-{idx}")).collect::<Vec<_>>();
        let result = truncate_lines_with_file(lines, 3, "perms-test")
            .await
            .expect("must truncate");
        let full_path = result.full_output.as_ref().expect("expected temp file");
        let metadata = std::fs::metadata(full_path).expect("must stat");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        result.cleanup().await.expect("cleanup must succeed");
    }

    #[tokio::test]
    async fn cleanup_is_idempotent() {
        let lines = (0..10).map(|idx| format!("line-{idx}")).collect::<Vec<_>>();
        let result = truncate_lines_with_file(lines, 3, "idempotent-test")
            .await
            .expect("must truncate");
        assert!(result.full_output.is_some());
        result.cleanup().await.expect("first cleanup must succeed");
        result
            .cleanup()
            .await
            .expect("second cleanup must also succeed");
    }

    #[tokio::test]
    async fn cleanup_no_file_succeeds() {
        let result = TruncatedEntries {
            lines: 0,
            total: 0,
            truncated: false,
            full_output: None,
            entries: vec![],
        };
        result
            .cleanup()
            .await
            .expect("cleanup with no file must succeed");
    }
}
