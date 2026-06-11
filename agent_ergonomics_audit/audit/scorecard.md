# agcli agent-ergonomics scorecard — pass 1 (2026-06-11)

Pre-pass scores at HEAD 690b7da; post-pass verified by behavior probes + 174-test suite.
See ../../implementation notes and recommendations.jsonl for evidence trail.

| Dimension | Pre | Post | Notes |
|---|---|---|---|
| output_parseability | 950 | 980 | exit-code assert can no longer escape the panic guard; non-finite null path closed |
| error_pedagogy | 870 | 940 | reserved-flag did-you-mean; single-dash short flag rendering; EXTRA_ARG names tokens + template |
| intent_inference | 740 | 900 | extra-positional silence closed; bare `version`; =false negation |
| safety_with_recovery | 850 | 900 | EXTRA_ARG protects mutating CLIs from silently dropped args |
| determinism_and_reproducibility | 900 | 950 | SDE clamping + deterministic typo ties + round-trip pin |
| self_documentation | 780 | 860 | error_codes dictionary runtime-discoverable |
| agent_intuitiveness | 800 | 850 | short value flags accepted as advertised |
| agent_ergonomics | 800 | 870 | output flags honored on root/help/version |
| composability | 760 | 800 | error_codes contract export |
| agent_ease_of_use | 720 | 720 | unchanged — R-013/R-014 deferred |
| regression_resistance | 700 | 880 | golden-envelope suite + audit() usage-string checks |

Intent corpus (26 entries): pre 4 silent_fail / post 0 silent_fail. 0 useless_error both passes.
