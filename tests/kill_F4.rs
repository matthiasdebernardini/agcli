// Kills the levenshtein `+ -> *` recurrence mutant (src/cli.rs:2388:35).
//
// Line 2388:
//   curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
// The mutant rewrites `prev[j + 1]` as `prev[j * 1]` == `prev[j]`, corrupting
// the deletion term of the DP recurrence. This is NOT a uniform shift — the
// effect is input-dependent, so the typo must be chosen carefully.
//
// `nearest_name` (src/cli.rs:2361) only offers a suggestion when the edit
// distance is <= 2. `unknown_command_fix` (src/cli.rs:2345) therefore emits a
// "Did you mean `<cmd>`? " prefix ONLY when a near match is found, and ALWAYS
// appends "Valid commands: <list>." regardless. So `fix.contains("status")`
// is VACUOUS (the valid-commands list always names "status"); the kill must
// assert on the CONDITIONAL "Did you mean" prefix instead.
//
// Why "stats" does NOT work: brute-forced against both recurrences,
//   lev("stats","status")  = 1 (real)  and = 1 (mutant)
// — the mutant stays inside the <= 2 gate, the suggestion still appears, and
// the mutant SURVIVES. (This is the trap the prior attempt fell into.)
//
// Why "statuss" KILLS it: the typo is "status" plus an extra trailing 's',
//   lev("statuss","status") = 1 (real)  and = 6 (mutant)
// — real code: dist 1 <= 2  → nearest_name => "status" → fix STARTS WITH
//                              "Did you mean `status`? "
//   mutant:    dist 6 >  2  → nearest_name => None     → no "Did you mean"
//                              prefix (fix == "Valid commands: status.")
// Asserting the fix contains the "Did you mean `status`" prefix is true on
// real code and false under the mutant, so the mutant is caught.

use agcli::{AgentCli, Command, CommandOutput};
use serde_json::{Value, json};

#[tokio::test]
async fn kill_mutant_src_cli_rs_2388() {
    // Pin timestamp so the envelope is deterministic.
    // SAFETY: nextest runs each test in its own process; no concurrent env readers.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "0") };

    let cli = AgentCli::new("prog", "p").command(Command::new("status", "Show status").handler(
        |_req, _ctx| Box::pin(async move { Ok(CommandOutput::new(json!({"ok": true}))) }),
    ));

    // First element is the program name (consumed by parse_invocation); "statuss"
    // is the unknown command. "statuss" = "status" + trailing 's':
    //   real recurrence:  lev = 1 (<= 2 gate)  → "Did you mean `status`?" emitted
    //   mutant recurrence: lev = 6 (> 2 gate)  → suggestion suppressed
    let exec = cli.run_argv(["prog", "statuss"]).await;
    let raw = exec.to_json();
    let envelope: Value = serde_json::from_str(&raw).expect("envelope must be valid JSON");

    let fix = envelope["fix"]
        .as_str()
        .expect("fix field must be present on UNKNOWN_COMMAND error");

    // Confirm the exact emitted wording when iterating; harmless in CI.
    eprintln!("fix = {fix:?}");

    // True on real code (dist 1 => suggestion). False under the mutant
    // (dist 6 => no suggestion). Specific to the conditional prefix — NOT a
    // bare "status" check, which would be vacuous against "Valid commands:".
    assert!(
        fix.contains("Did you mean `status`"),
        "expected did-you-mean suggestion for `status` in fix field, got: {fix:?}\n\
         (mutant inflates lev(\"statuss\",\"status\") from 1 to 6, pushing it past \
         the <= 2 gate and suppressing the suggestion)"
    );
}
