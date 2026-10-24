//! Did-you-mean suggestions for unknown commands and flags.

use std::mem;

/// Corrective `fix` text for an unknown command/subcommand error.
///
/// Lists every valid name inline so a *semantic* miss (guessing `list` when
/// the verb is `history`, which no edit-distance check would ever relate) is
/// corrected on the first read instead of triggering another blind guess.
/// When the bad token also looks like a typo of a real name, a nearest-match
/// nudge is prefixed. `next_actions` already carry the full templates; this puts
/// the names where the agent reads first.
pub(crate) fn unknown_command_fix(scope: &str, bad: &str, valid: &[&str]) -> String {
    use std::fmt::Write as _;

    if valid.is_empty() {
        return format!("Run the root command to inspect the available {scope}s.");
    }
    let mut fix = String::new();
    if let Some(near) = nearest_name(bad, valid) {
        let _ = write!(&mut fix, "Did you mean `{near}`? ");
    }
    let _ = write!(&mut fix, "Valid {scope}s: {}.", valid.join(", "));
    fix
}

/// Closest candidate by case-insensitive Levenshtein distance, gated so only a
/// plausible typo is offered: edit distance ≤ 2 and strictly less than the
/// candidate's length (so an unrelated short word can't match by coincidence).
pub(crate) fn nearest_name(bad: &str, valid: &[&str]) -> Option<String> {
    let bad_lower = bad.to_lowercase();
    valid
        .iter()
        .map(|candidate| {
            (
                *candidate,
                levenshtein(&bad_lower, &candidate.to_lowercase()),
            )
        })
        .filter(|(candidate, dist)| *dist <= 2 && *dist < candidate.chars().count().max(1))
        .min_by_key(|(_, dist)| *dist)
        .map(|(candidate, _)| candidate.to_string())
}

/// Classic single-row-DP Levenshtein edit distance. Inputs are command names
/// (a handful of chars), so the per-call allocation is negligible.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::codes::ErrorCode;
    use crate::envelope::Envelope;
    use crate::test_support::sample_cli;

    // --- did-you-mean / inline-valid-names on unknown command|subcommand ---

    #[tokio::test]
    async fn unknown_subcommand_fix_inlines_valid_names() {
        let cli = sample_cli();
        // `list` is a semantic miss (no edit-distance relation to `stream`), so
        // no nudge, but the valid subcommand must be inlined so the agent
        // corrects in one read instead of guessing again.
        let run = cli.run(["wokhei", "gateway", "list"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::UNKNOWN_SUBCOMMAND);
        assert!(
            envelope.fix.contains("Valid subcommands: stream."),
            "fix should inline valid subcommands, got: {}",
            envelope.fix
        );
        assert!(!envelope.fix.contains("Did you mean"));
    }

    #[tokio::test]
    async fn unknown_subcommand_fix_suggests_typo() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "gateway", "streem"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::UNKNOWN_SUBCOMMAND);
        assert!(
            envelope.fix.contains("Did you mean `stream`?"),
            "fix should nudge the nearest name, got: {}",
            envelope.fix
        );
    }

    #[tokio::test]
    async fn unknown_command_fix_inlines_valid_names() {
        let cli = sample_cli();
        let run = cli.run(["wokhei", "bogus"]).await;
        let Envelope::Error(envelope) = run.envelope() else {
            panic!("expected error envelope");
        };
        assert_eq!(envelope.error.code, ErrorCode::UNKNOWN_COMMAND);
        assert!(envelope.fix.contains("Valid commands:"));
        assert!(envelope.fix.contains("gateway"));
        assert!(envelope.fix.contains("status"));
    }

    #[test]
    fn nearest_name_gates_unrelated_words() {
        // typo within distance 2 → suggested
        assert_eq!(
            nearest_name("streem", &["stream", "status"]),
            Some("stream".to_string())
        );
        // semantic miss, far from any real name → no suggestion
        assert_eq!(nearest_name("list", &["stream", "status"]), None);
        // empty candidate set → no suggestion
        assert_eq!(nearest_name("x", &[]), None);
    }
}
