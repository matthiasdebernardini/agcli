//! Usage-template parsing: the usage string is both documentation and the
//! flag and arity schema.

use std::collections::HashSet;

use crate::next_action::{ActionParam, NextAction};

/// Extract a `<name>` placeholder from `text`, returning the inner name when it
/// is a usable param — non-empty and free of `.` (which marks a variadic like
/// `args...`). Returns `None` when there is no placeholder or it is not usable.
fn placeholder_param_name(text: &str) -> Option<&str> {
    let angle_start = text.find('<')?;
    let angle_end = text[angle_start..].find('>')?;
    let param_name = &text[angle_start + 1..angle_start + angle_end];
    (!param_name.is_empty() && !param_name.contains('.')).then_some(param_name)
}

/// Build a `NextAction` from a usage string and description.
///
/// A bare `<name>` placeholder becomes a **required** positional param. Any
/// placeholder inside brackets is **optional**: `[--flag=<name>]`,
/// `[--flag <name>]`, short value flags `[-v <level>]`, and bare optional
/// positionals `[<optional>]`. Brackets without a `<...>` placeholder
/// (`[--follow]`, `[args...]`) contribute no param. If no placeholders are
/// found, the action is literal (no `params`) and its command drops the
/// optional `[...]` segments, so it runs as printed: `app tail [--follow]`
/// becomes `app tail`.
pub(crate) fn next_action_from_usage(usage: &str, description: &str) -> NextAction {
    let mut action = NextAction::new(usage, description);
    let bytes = usage.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Any bracketed token is optional: `[--flag=<name>]`, `[--flag <name>]`,
        // short value flags `[-v <level>]`, and bare optional positionals
        // `[<optional>]` all map their inner `<name>` placeholder to an optional
        // param. A bracket without a `<...>` placeholder (e.g. `[--follow]`,
        // `[args...]`) contributes no param.
        if bytes[i] == b'['
            && let Some(close) = usage[i..].find(']')
        {
            let bracket_content = &usage[i + 1..i + close]; // strip [ and ]
            if let Some(param_name) = placeholder_param_name(bracket_content) {
                action = action.with_param(param_name, ActionParam::new().required(false));
            }
            i += close + 1;
            continue;
        }

        // Positional placeholder: <name> (not inside [...])
        if bytes[i] == b'<'
            && let Some(close) = usage[i..].find('>')
        {
            let param_name = &usage[i + 1..i + close];
            if !param_name.is_empty() && !param_name.contains('.') {
                action = action.with_param(param_name, ActionParam::new().required(true));
            }
            i += close + 1;
            continue;
        }

        i += 1;
    }

    if action.params.is_none() {
        action.command = without_optional_segments(usage);
    }
    action
}

/// `usage` with every `[...]` segment removed and its whitespace re-joined.
fn without_optional_segments(usage: &str) -> String {
    let mut kept = String::with_capacity(usage.len());
    let mut rest = usage;
    while let Some(open) = rest.find('[')
        && let Some(close) = rest[open..].find(']')
    {
        kept.push_str(&rest[..open]);
        kept.push(' ');
        rest = &rest[open + close + 1..];
    }
    kept.push_str(rest);
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How many positionals a usage string declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Arity {
    /// Bare `<name>` placeholders.
    pub(crate) required: usize,
    /// Bracketed positionals: `[<name>]`, `[path]`.
    pub(crate) optional: usize,
}

/// Count the positional placeholders a usage string declares. Returns `None`
/// when arity cannot be bounded — the usage declares a variadic tail
/// (`...`). Flag-value placeholders (`--flag <v>`, `[-n <count>]`,
/// `--flag=<v>`) are not positionals and are skipped; plain words are the
/// program/command-path literals.
pub(crate) fn positional_arity(usage: &str) -> Option<Arity> {
    let mut required = 0usize;
    let mut optional = 0usize;
    let mut tokens = usage.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token.contains("...") {
            return None;
        }
        if let Some(inner) = token.strip_prefix('[') {
            // Bracketed group. Multi-token forms (`[--flag <v>]`,
            // `[-n <count>]`) start with a flag, so the whole group is
            // flag-ish; anything else (`[<x>]`, bare `[path]`) is an
            // optional positional. Either way, consume tokens through the
            // closing `]`.
            let is_positional = !inner.starts_with('-');
            let mut token = token;
            while !token.ends_with(']') {
                match tokens.next() {
                    Some(next) if next.contains("...") => return None,
                    Some(next) => token = next,
                    None => break,
                }
            }
            if is_positional {
                optional += 1;
            }
            continue;
        }
        if token.starts_with('-') {
            // A flag. When it doesn't carry `=value`, its value may be the
            // next token (`--flag <v>` / `-n <count>`): that placeholder
            // belongs to the flag, not the positionals.
            if !token.contains('=') && tokens.peek().is_some_and(|t| t.starts_with('<')) {
                tokens.next();
            }
            continue;
        }
        if token.starts_with('<') {
            required += 1;
        }
    }
    Some(Arity { required, optional })
}

/// Render a flag name with the dash count an agent must actually type:
/// `-n` for single-character flags, `--select` for long ones.
pub(crate) fn flag_display(name: &str) -> String {
    if name.chars().count() == 1 {
        format!("-{name}")
    } else {
        format!("--{name}")
    }
}

/// Pull *every* flag name out of a usage string — value flags (`--flag <v>`,
/// `--flag=<v>`, bracketed or not) as well as booleans (`[--flag]`, `[-x]`).
/// This is the declared-flag schema used to reject unknown flags, so it is
/// deliberately permissive about surrounding syntax: any `--name` token in the
/// template counts as declared.
pub(crate) fn extract_all_flag_names(usage: &str, set: &mut HashSet<String>) {
    // Bracketed booleans, including short flags (`[-x]`, `[-abc]`).
    extract_bool_flag_names(usage, set);
    for raw in usage.split(|c: char| c.is_whitespace() || c == '[' || c == ']') {
        // Long flags anywhere in the template.
        if let Some(rest) = raw.strip_prefix("--") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !name.is_empty() {
                set.insert(name);
            }
        } else if let Some(rest) = raw.strip_prefix('-') {
            // Short *value* flags (`-n <count>`, bracketed or not). The
            // bool extractor above only sees bracketed forms without a
            // `<value>`, so without this arm a usage-declared `[-n <count>]`
            // would be rejected as unknown — the framework refusing its own
            // advertised affordance. Letters only: `-1`-style tokens are
            // negative-number examples, not flags.
            let mut chars = rest.chars();
            if let Some(ch) = chars.next()
                && ch.is_ascii_alphabetic()
                && matches!(chars.next(), None | Some('='))
            {
                set.insert(ch.to_string());
            }
        }
    }
}

/// Pull boolean flag names out of a usage string. Recognizes bracketed
/// optional flags of the form `[--name]` (and short `[-x]`). Skips any
/// bracketed flag that carries a `<value>` placeholder or `=` — those are
/// value flags, not booleans.
pub(crate) fn extract_bool_flag_names(usage: &str, set: &mut HashSet<String>) {
    let bytes = usage.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(close_rel) = usage[i + 1..].find(']') else {
            break;
        };
        let inner = &usage[i + 1..i + 1 + close_rel];
        i += close_rel + 2;
        if inner.contains('<') || inner.contains('=') {
            continue;
        }
        if let Some(rest) = inner.strip_prefix("--") {
            let name = rest.split_whitespace().next().unwrap_or("");
            if !name.is_empty() {
                set.insert(name.to_string());
            }
        } else if let Some(rest) = inner.strip_prefix('-') {
            // Short single-char or bundled boolean: `[-x]` or `[-abc]`.
            let token = rest.split_whitespace().next().unwrap_or("");
            for ch in token.chars() {
                set.insert(ch.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::literal_actions::literal_actions_run_as_printed;
    use crate::test_support::{rich_cli, sample_cli};

    #[test]
    fn extract_bool_flag_names_basic() {
        let mut set = HashSet::new();
        extract_bool_flag_names(
            "agplan submit [path] [--title=<title>] [--no-git] [--dry-run]",
            &mut set,
        );
        assert!(set.contains("no-git"));
        assert!(set.contains("dry-run"));
        assert!(!set.contains("title"));
    }

    // --- next_action_from_usage tests ---

    #[test]
    fn next_action_from_usage_literal_no_params() {
        let action = next_action_from_usage("wokhei status", "Check status");
        assert!(action.params.is_none());
        assert_eq!(action.command, "wokhei status");
    }

    #[test]
    fn next_action_from_usage_literal_drops_optional_segments() {
        // A literal runs as printed, so the brackets that carry no param
        // (booleans, variadics) are not part of it.
        let action = next_action_from_usage("wokhei gateway stream [--follow]", "Stream");
        assert!(action.params.is_none());
        assert_eq!(action.command, "wokhei gateway stream");
        let action = next_action_from_usage("ops echo [args...]", "Echo");
        assert_eq!(action.command, "ops echo");
        let action = next_action_from_usage("t log [-x] [--follow] now", "Log");
        assert_eq!(action.command, "t log now");
        // A template keeps its bracketed parts.
        let action = next_action_from_usage("t send <event> [--follow]", "Send");
        assert_eq!(action.command, "t send <event> [--follow]");
    }

    #[tokio::test]
    async fn literal_actions_of_the_test_clis_run_as_printed() {
        for (cli, literal) in [(sample_cli(), "wokhei gateway stream"), (rich_cli(), "app")] {
            let ran = literal_actions_run_as_printed(&cli.name, async |argv: Vec<String>| {
                let run = cli.run(argv).await;
                serde_json::from_str(&run.to_json()).expect("the envelope is JSON")
            })
            .await;
            assert!(ran.contains(literal), "{ran:?}");
        }
    }

    #[test]
    fn next_action_from_usage_positional_placeholder() {
        let action = next_action_from_usage("wokhei run <run-id>", "Run a job");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.len(), 1);
        let p = params.get("run-id").expect("run-id param");
        assert_eq!(p.required, Some(true));
    }

    #[test]
    fn next_action_from_usage_optional_flag_placeholder() {
        let action = next_action_from_usage("wokhei logs [--lines=<lines>]", "View logs");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.len(), 1);
        let p = params.get("lines").expect("lines param");
        assert_eq!(p.required, Some(false));
    }

    #[test]
    fn next_action_from_usage_mixed_placeholders() {
        let action = next_action_from_usage(
            "wokhei send <event> [--data=<data>] [--follow]",
            "Send event",
        );
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.len(), 2);
        assert_eq!(params.get("event").unwrap().required, Some(true));
        assert_eq!(params.get("data").unwrap().required, Some(false));
        // [--follow] has no <placeholder> so no param entry
        assert!(params.get("follow").is_none());
    }

    #[test]
    fn next_action_from_usage_default_generated_usage() {
        // The default usage_or_default for commands with no custom usage
        let action = next_action_from_usage("prog cmd [--flag=<value>] [args...]", "Do something");
        let params = action.params.as_ref().expect("params should be present");
        // <value> from [--flag=<value>] should be parsed
        assert!(params.contains_key("value"));
        // [args...] has no angle brackets, no param
    }

    // --- next_action_from_usage: short value flags & bare optional positionals ---

    #[test]
    fn next_action_from_usage_short_value_flag_is_optional() {
        let action = next_action_from_usage("app log [-v <level>] <file>", "Log a file");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.get("level").unwrap().required, Some(false));
        assert_eq!(params.get("file").unwrap().required, Some(true));
    }

    #[test]
    fn next_action_from_usage_bare_optional_positional() {
        let action = next_action_from_usage("app x [<optional>]", "X");
        let params = action.params.as_ref().expect("params should be present");
        assert_eq!(params.get("optional").unwrap().required, Some(false));
    }

    // --- Short value flags are declared flags (R1) ---

    #[test]
    fn extract_all_flag_names_includes_short_value_flags() {
        let mut set = HashSet::new();
        extract_all_flag_names("t log [-n <count>] [--follow] -v <level>", &mut set);
        assert!(set.contains("n"), "{set:?}");
        assert!(set.contains("v"), "{set:?}");
        assert!(set.contains("follow"));
        // `-1`-style tokens are negative-number examples, not flags.
        let mut neg = HashSet::new();
        extract_all_flag_names("t add -1 <b>", &mut neg);
        assert!(!neg.contains("1"));
    }

    #[test]
    fn positional_arity_counts() {
        let arity = |required, optional| Some(Arity { required, optional });
        assert_eq!(positional_arity("calc add <a> <b>"), arity(2, 0));
        assert_eq!(
            positional_arity("agplan submit [path] [--title=<title>] [--no-git]"),
            arity(0, 1)
        );
        assert_eq!(
            positional_arity("t log [-n <count>] [--follow]"),
            arity(0, 0)
        );
        assert_eq!(positional_arity("t get <id> [--format <fmt>]"), arity(1, 0));
        assert_eq!(positional_arity("t run <cmd> [args...]"), None);
        assert_eq!(positional_arity("t pick <a> [<b>]"), arity(1, 1));
    }
}
