// Kills the levenshtein first-column-init mutant (src/cli.rs:2385:21).
//
// Line 2385:
//   curr[0] = i + 1;
// The mutant replaces `+` with `*`: curr[0] = i * 1 == i.
//
// `curr[0]` is the DP first-column initializer — the cost of deleting the
// first (i+1) characters of string `a` with no matches in `b`. Under the
// mutant, that cost is `i` instead of `i+1`, so every deletion-heavy path
// gets a "free" deletion (the running deletion cost is understated by 1).
//
// How the discriminating input was chosen
// ----------------------------------------
// Consider lev("aaabxy", "bxy"):
//   Real recurrence: the optimal path deletes "aaa" (3 ops), result = 3.
//   Mutant:          each row's curr[0] is 1 less → accumulated saving of 1
//                    on deletion-dominated paths → result = 2.
//
// Detailed trace (real vs mutant):
//   a = "aaabxy" (len 6),  b = "bxy" (len 3)
//   Initial prev = [0,1,2,3]
//
//   i=0 ca='a':  curr[0] = 1 (real) / 0 (mutant)
//     ...propagates through the row; by the end, real prev accumulates
//     1 extra deletion credit in every row's first column. After 6 rows:
//     real result = 3, mutant result = 2.
//
// `nearest_name` (src/cli.rs:2361) suggests a command only when:
//   dist <= 2  AND  dist < cand.chars().count()
//
// For command "bxy" (len = 3):
//   Real:    dist 3 > 2  → filter FAILS → NO "Did you mean" prefix
//   Mutant:  dist 2 ≤ 2  AND  2 < 3  → filter PASSES → "Did you mean `bxy`" emitted
//
// Asserting `!fix.contains("Did you mean")` therefore:
//   - PASSES on real code  (dist 3, no suggestion)
//   - FAILS on the mutant  (dist 2, suggestion emitted) → mutant killed

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::{Value, json};

#[tokio::test]
async fn kill_mutant_src_cli_rs_2385() {
    // Pin timestamp so the envelope is deterministic.
    // SAFETY: nextest runs each test in its own process; no concurrent env readers.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "0") };

    // Register a command named "bxy" (length 3).  The unknown token we pass is
    // "aaabxy" — it shares the suffix "bxy" but requires 3 deletions to reach it.
    // Real levenshtein("aaabxy","bxy") = 3 (> threshold 2) → no suggestion.
    // Mutant levenshtein("aaabxy","bxy") = 2 (≤ threshold 2, 2 < len("bxy")=3) → suggestion.
    let cli = AgentCli::new("prog", "Mutant-kill prog for cli.rs:2385").command(
        Command::new("bxy", "A three-letter command").handler(|_req, _ctx| {
            Box::pin(async move { Ok(CommandOutput::new(json!({"ok": true}))) })
        }),
    );

    // "aaabxy" is the unknown command token; prog name is consumed first.
    let exec = cli.run_argv(["prog", "aaabxy"]).await;
    let raw = exec.to_json();
    let envelope: Value = serde_json::from_str(&raw).expect("envelope must be valid JSON");

    // Confirm this is an UNKNOWN_COMMAND error (not a handler error).
    assert_eq!(
        envelope["ok"], false,
        "expected error envelope for unknown command 'aaabxy', got: {envelope}"
    );
    assert_eq!(
        envelope["error"]["code"], "UNKNOWN_COMMAND",
        "expected UNKNOWN_COMMAND code, got: {envelope}"
    );

    let fix = envelope["fix"]
        .as_str()
        .expect("fix field must be present on UNKNOWN_COMMAND error");

    // Emit the fix string for easy debugging if the test fails.
    eprintln!("fix = {fix:?}");

    // On real code: lev("aaabxy","bxy") = 3, which exceeds the <= 2 threshold,
    // so nearest_name returns None and no "Did you mean" prefix is emitted.
    // On the mutant: lev = 2, threshold passes, suggestion IS emitted.
    // The assertion passes on real code and fails on the mutant, killing it.
    assert!(
        !fix.contains("Did you mean"),
        "expected no did-you-mean suggestion (lev dist 3 > threshold 2), but got: {fix:?}\n\
         (if this fires on REAL code the distance calculation changed; \
          if it fires only under mutation, curr[0]=i*1 is lowering dist from 3 to 2 \
          and pushing it past the <= 2 gate)"
    );
}
