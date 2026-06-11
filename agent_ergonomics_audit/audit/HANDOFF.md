# Handoff — after pass 1 (v0.12.0)

## Deferred (next pass candidates)
- R-013 typed param declaration on `Command` (`ParamSpec`: type/required/enum/description) — supersedes usage-string flag schema, populates `ActionParam.type`, enables framework-level type checks.
- R-014 `handler_fn` owned-request adapter — removes the `Box::pin` + borrow-before-move tax (the single biggest agcli-developer friction).
- R-015 `list_truncated` guidance names `--limit`, which is not a reserved flag — following the trail can bounce.
- Minor: `select_warning` key could clobber a real result field; `parse_rfc3339_utc` accepts impossible dates (2026-02-31) leniently; truncation temp files accumulate by design (document or expose a sweep helper); `-V extra` consumes `extra` as a value.

## Notes
- Golden tests live in `tests/golden_envelopes.rs` (not audit/regression_tests/) so `cargo nextest` runs them in CI; this directory holds only this trail.
- Pass 1 ran concurrently with another session's hardening commit (690b7da); R-001/R-002 fixed bugs in that commit.
