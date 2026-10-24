//! Context protection for long line-oriented output:
//! [`truncate_lines_with_file`] keeps the tail inline and writes everything to
//! a file the result points at.

use std::env;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Line-oriented output cut to fit an agent's context, as
/// [`truncate_lines_with_file`] returns it.
///
/// When truncated, `entries` holds the tail, the last `lines` lines, because
/// logs, the common case, want the most recent output. `dropped` says how many
/// lines were cut from the head, and `full_output` points at a file with every
/// line, so an agent can still read what was cut.
///
/// With the `deserialize` feature it decodes only a consistent shape: `lines`
/// counts `entries`, `dropped` is `total - lines`, `truncated` is true
/// exactly when lines were dropped, and `full_output` is present exactly when
/// truncated.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct TruncatedEntries {
    /// How many lines `entries` holds.
    pub lines: usize,
    /// How many lines there were in all.
    pub total: usize,
    /// Number of lines dropped from the **head** (`total - lines`). `0` when
    /// not truncated. The kept lines are always the tail.
    pub dropped: usize,
    /// Whether anything was cut.
    pub truncated: bool,
    /// The path of the file holding every line, when anything was cut. Left
    /// out of the JSON otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output: Option<String>,
    /// The lines kept inline: all of them, or the tail.
    pub entries: Vec<String>,
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for TruncatedEntries {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        #[derive(serde::Deserialize)]
        struct Raw {
            lines: usize,
            total: usize,
            dropped: usize,
            truncated: bool,
            #[serde(default, deserialize_with = "crate::envelope::present")]
            full_output: Option<String>,
            entries: Vec<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.lines != raw.entries.len() {
            return Err(D::Error::custom(format!(
                "`lines` is {} but `entries` holds {} lines",
                raw.lines,
                raw.entries.len()
            )));
        }
        if raw.total < raw.lines || raw.dropped != raw.total - raw.lines {
            return Err(D::Error::custom(format!(
                "`dropped` is {} but `total - lines` is {} - {}",
                raw.dropped, raw.total, raw.lines
            )));
        }
        if raw.truncated != (raw.dropped > 0) {
            return Err(D::Error::custom(format!(
                "`truncated` is {} with {} lines dropped",
                raw.truncated, raw.dropped
            )));
        }
        if raw.full_output.is_some() != raw.truncated {
            return Err(D::Error::custom(
                "`full_output` is present exactly when the output was truncated",
            ));
        }
        Ok(Self {
            lines: raw.lines,
            total: raw.total,
            dropped: raw.dropped,
            truncated: raw.truncated,
            full_output: raw.full_output,
            entries: raw.entries,
        })
    }
}

impl TruncatedEntries {
    /// Remove the temp file written during truncation, if any.
    ///
    /// The `full_output` file is **not** removed automatically: it must outlive
    /// the command so the agent can follow the pointer and read the full
    /// output. The caller (or a downstream sweeper) owns its lifecycle and
    /// removes it by calling this method once the file is no longer needed.
    ///
    /// Idempotent: succeeds even if the file was already removed.
    ///
    /// # Errors
    ///
    /// Any I/O error from removing the file other than
    /// [`io::ErrorKind::NotFound`], e.g. a permission error.
    pub fn cleanup(&self) -> io::Result<()> {
        if let Some(path) = &self.full_output {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Truncate to the last `max_lines` lines and, when truncated, write the full
/// output to a temp file (see [`TruncatedEntries`] for the tail/`dropped`
/// semantics and the file's ownership rules).
///
/// The temp file is named `<file_prefix>-<pid>-<nanos>.log` in the system
/// temp directory, so `file_prefix` must be a non-empty run of ASCII letters,
/// digits, `-` and `_`: nothing that could leave the directory or need
/// quoting.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidInput`] when `max_lines` is `0` (it would keep
///   no line inline), or when `file_prefix` is empty or holds any other
///   character. Checked before anything is written. Validate an
///   agent-supplied count before forwarding it, so the agent gets an error
///   that names its flag.
/// - Only when truncation is needed: any I/O error from creating or writing
///   the temp file, or [`io::ErrorKind::AlreadyExists`] if five attempts in a
///   row found the candidate file name already taken.
pub fn truncate_lines_with_file(
    lines: Vec<String>,
    max_lines: usize,
    file_prefix: &str,
) -> io::Result<TruncatedEntries> {
    if max_lines == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "max_lines must be at least 1",
        ));
    }
    let is_prefix_char = |ch: char| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_';
    if file_prefix.is_empty() || !file_prefix.chars().all(is_prefix_char) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "file_prefix must be non-empty ASCII letters, digits, `-` or `_`: {file_prefix:?}"
            ),
        ));
    }
    let total = lines.len();

    if total <= max_lines {
        return Ok(TruncatedEntries {
            lines: total,
            total,
            dropped: 0,
            truncated: false,
            full_output: None,
            entries: lines,
        });
    }

    let path = write_full_output(&lines, file_prefix)?;
    let start = total.saturating_sub(max_lines);
    let entries: Vec<String> = lines.into_iter().skip(start).collect();
    let kept = entries.len();

    Ok(TruncatedEntries {
        lines: kept,
        total,
        dropped: total - kept,
        truncated: true,
        full_output: Some(path.to_string_lossy().into_owned()),
        entries,
    })
}

fn write_full_output(lines: &[String], prefix: &str) -> io::Result<PathBuf> {
    // The timestamp is only a filename uniquifier (the create_new loop below
    // resolves any collision), so never panic on a pre-1970 clock: a handler
    // panic here would escape as non-JSON, violating the framework contract.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let pid = process::id();

    for attempt in 0u32..5 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let filename = format!("{prefix}-{pid}-{now}{suffix}.log");
        let path = env::temp_dir().join(filename);

        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        match opts.open(&path) {
            Ok(file) => {
                let mut writer = BufWriter::new(file);
                if let Some((last, rest)) = lines.split_last() {
                    for line in rest {
                        writer.write_all(line.as_bytes())?;
                        writer.write_all(b"\n")?;
                    }
                    writer.write_all(last.as_bytes())?;
                }
                writer.flush()?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
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
    use std::path::Path;

    use super::*;

    #[cfg(feature = "deserialize")]
    #[test]
    fn truncated_entries_round_trip_strictly() {
        let lines: Vec<_> = (0..10).map(|idx| format!("line-{idx}")).collect();
        let truncated = truncate_lines_with_file(lines, 3, "roundtrip").expect("must truncate");
        let small = truncate_lines_with_file(vec!["a".to_string()], 3, "roundtrip")
            .expect("must not truncate");
        for value in [&truncated, &small] {
            let json = serde_json::to_string(value).expect("serialize");
            let decoded: TruncatedEntries = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&decoded, value);
        }
        // `dropped` is always emitted, so a line without it was not ours.
        let mut missing = serde_json::to_value(&small).expect("serialize");
        missing.as_object_mut().expect("object").remove("dropped");
        assert!(serde_json::from_value::<TruncatedEntries>(missing).is_err());
        truncated.cleanup().expect("cleanup must succeed");
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn truncated_entries_reject_an_inconsistent_shape() {
        // Each of these decoded before, so a reader could not trust that
        // `truncated` meant a file exists or that `dropped` counted anything.
        let consistent = serde_json::json!({
            "lines": 2, "total": 5, "dropped": 3, "truncated": true,
            "full_output": "/tmp/x.log", "entries": ["d", "e"],
        });
        assert!(serde_json::from_value::<TruncatedEntries>(consistent.clone()).is_ok());
        for (key, bad) in [
            ("full_output", serde_json::Value::Null),
            ("dropped", serde_json::json!(1)),
            ("truncated", serde_json::json!(false)),
            ("lines", serde_json::json!(3)),
            ("total", serde_json::json!(1)),
        ] {
            let mut shape = consistent.clone();
            shape[key] = bad;
            assert!(
                serde_json::from_value::<TruncatedEntries>(shape.clone()).is_err(),
                "must reject {shape}"
            );
        }
        let mut no_file = consistent.clone();
        no_file
            .as_object_mut()
            .expect("object")
            .remove("full_output");
        assert!(serde_json::from_value::<TruncatedEntries>(no_file).is_err());
        let with_file_but_whole = serde_json::json!({
            "lines": 2, "total": 2, "dropped": 0, "truncated": false,
            "full_output": "/tmp/x.log", "entries": ["d", "e"],
        });
        assert!(serde_json::from_value::<TruncatedEntries>(with_file_but_whole).is_err());
    }

    #[test]
    fn no_truncation_for_small_output() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let result = truncate_lines_with_file(lines, 10, "logs").expect("must truncate");
        assert!(!result.truncated);
        assert_eq!(result.total, 2);
        assert!(result.full_output.is_none());
    }

    #[test]
    fn truncation_writes_full_output_file() {
        let lines: Vec<_> = (0..10).map(|idx| format!("line-{idx}")).collect();
        let result = truncate_lines_with_file(lines, 3, "logs").expect("must truncate");
        assert!(result.truncated);
        assert_eq!(result.lines, 3);
        assert_eq!(result.total, 10);
        let full_path = result.full_output.as_ref().expect("expected temp file");
        assert!(Path::new(full_path).exists());
        result.cleanup().expect("cleanup must succeed");
        assert!(!Path::new(full_path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_has_restrictive_permissions() {
        use std::os::unix::fs::MetadataExt;
        let lines: Vec<_> = (0..10).map(|idx| format!("line-{idx}")).collect();
        let result = truncate_lines_with_file(lines, 3, "perms-test").expect("must truncate");
        let full_path = result.full_output.as_ref().expect("expected temp file");
        let metadata = fs::metadata(full_path).expect("must stat");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        result.cleanup().expect("cleanup must succeed");
    }

    #[test]
    fn cleanup_is_idempotent() {
        let lines: Vec<_> = (0..10).map(|idx| format!("line-{idx}")).collect();
        let result = truncate_lines_with_file(lines, 3, "idempotent-test").expect("must truncate");
        assert!(result.full_output.is_some());
        result.cleanup().expect("first cleanup must succeed");
        result.cleanup().expect("second cleanup must also succeed");
    }

    #[test]
    fn truncation_reports_dropped_head_count() {
        let lines: Vec<_> = (0..10).map(|idx| format!("line-{idx}")).collect();
        let result = truncate_lines_with_file(lines, 3, "dropped-test").expect("must truncate");
        // Tail is kept; the first 7 lines were dropped.
        assert_eq!(result.entries, vec!["line-7", "line-8", "line-9"]);
        assert_eq!(result.lines, 3);
        assert_eq!(result.dropped, 7);
        assert_eq!(result.dropped, result.total - result.lines);
        result.cleanup().expect("cleanup must succeed");
    }

    #[test]
    fn no_truncation_reports_zero_dropped() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let result = truncate_lines_with_file(lines, 10, "logs").expect("must truncate");
        assert!(!result.truncated);
        assert_eq!(result.dropped, 0);
    }

    #[test]
    fn max_lines_zero_is_invalid_input() {
        // A zero cap would keep no line inline; it is an error, not a floor.
        let lines: Vec<_> = (0..5).map(|idx| format!("line-{idx}")).collect();
        let error = truncate_lines_with_file(lines, 0, "zero-test").expect_err("must refuse");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        // Refused even when nothing would be truncated.
        let error = truncate_lines_with_file(Vec::new(), 0, "zero-test").expect_err("must refuse");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn an_invalid_file_prefix_is_invalid_input() {
        for prefix in ["", "///", "../escape", "has space", "ünï"] {
            let lines: Vec<_> = (0..5).map(|idx| format!("line-{idx}")).collect();
            let error = truncate_lines_with_file(lines, 1, prefix).expect_err("must refuse");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{prefix:?}");
        }
    }

    #[test]
    fn cleanup_no_file_succeeds() {
        let result = TruncatedEntries {
            lines: 0,
            total: 0,
            dropped: 0,
            truncated: false,
            full_output: None,
            entries: vec![],
        };
        result.cleanup().expect("cleanup with no file must succeed");
    }
}
