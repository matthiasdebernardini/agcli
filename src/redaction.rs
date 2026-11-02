//! Redaction: keeping secrets given on the command line out of every envelope.
//!
//! The `command` field of each envelope echoes the argv, and some errors
//! quote an argument. What gets masked: the value of a flag the command's
//! usage does not declare, anything the command marks with
//! [`Command::secret`](crate::Command::secret), and whatever the CLI's
//! [`redact_arg`](crate::AgentCli::redact_arg) hook rewrites. The handler
//! always gets the real values; only what the framework prints is masked.

use crate::invocation::{looks_like_negative_number, shell_quote};

/// What replaces a masked value.
pub(crate) const MARKER: &str = "<redacted>";

/// The CLI-level hook: given one argv element, the text to echo in its place,
/// or `None` to leave it.
pub(crate) type RedactFn = dyn Fn(&str) -> Option<String> + Send + Sync;

/// A flag token with the value after its `=` masked, as text: `--key=value`
/// becomes `--key=<redacted>`. A token with no `=` is returned as it is.
pub(crate) fn mask_flag_value(token: &str) -> String {
    token
        .split_once('=')
        .map_or_else(|| token.to_string(), |(key, _)| format!("{key}={MARKER}"))
}

/// A flag token with its value masked, as a shell word: `--key=value` becomes
/// `--key=<redacted>`. A token with no `=` has no value in it and is only
/// quoted. The key is quoted and the marker stays bare, so the word is
/// re-runnable shell.
pub(crate) fn masked_flag_word(token: &str) -> String {
    token.split_once('=').map_or_else(
        || shell_quote(token).into_owned(),
        |(key, _)| format!("{}{MARKER}", shell_quote(&format!("{key}="))),
    )
}

/// The hook's text for `token` as one shell word, or `None` when the hook
/// leaves the token alone, in which case the caller decides how to show it.
pub(crate) fn hook_word(hook: Option<&RedactFn>, token: &str) -> Option<String> {
    hook.and_then(|hook| hook(token))
        .map(|text| shell_quote(&text).into_owned())
}

/// True for a token that starts a flag: `-` and something, not a number.
fn is_flag_token(token: &str) -> bool {
    token.starts_with('-') && token.len() > 1 && !looks_like_negative_number(token)
}

/// The indexes of the flag tokens in `args` that sit right after a bare flag
/// or a bundle of short flags. Read without the command tree, any of them may
/// be the value of the flag before it, so a line the framework could not
/// read masks them whole.
pub(crate) fn tied_flag_tokens(args: &[String]) -> Vec<usize> {
    let mut tied = Vec::new();
    let mut after_bare = false;
    for (at, token) in args.iter().enumerate() {
        if token == "--" {
            break;
        }
        if !is_flag_token(token) {
            after_bare = false;
            continue;
        }
        if after_bare {
            tied.push(at);
        }
        after_bare = !token.contains('=');
    }
    tied
}

/// The shell words for `args` when the framework cannot tell which tokens are
/// values: a line it failed to parse, or a raw command's tokens, which it
/// never parses. A flag keeps its name and loses its `=value`, and every other
/// token is masked except a command name, as `names_command(path, token)`
/// recognizes it after the command `path`. A token the hook rewrites keeps the
/// hook's text.
pub(crate) fn opaque_words(
    args: &[String],
    hook: Option<&RedactFn>,
    names_command: impl Fn(&[String], &str) -> bool,
) -> Vec<String> {
    let tied = tied_flag_tokens(args);
    // The state carried from token to token: the command path read so far,
    // and whether a `--` has ended the flags.
    let start: (Vec<String>, bool) = (Vec::new(), false);
    args.iter()
        .enumerate()
        .scan(start, |(path, flags_ended), (at, token)| {
            let word = if let Some(word) = hook_word(hook, token) {
                word
            } else if tied.contains(&at) {
                MARKER.to_string()
            } else if *flags_ended || !token.starts_with('-') || looks_like_negative_number(token) {
                if names_command(path, token) {
                    path.push(token.clone());
                    shell_quote(token).into_owned()
                } else {
                    MARKER.to_string()
                }
            } else {
                *flags_ended = token == "--";
                masked_flag_word(token)
            };
            Some(word)
        })
        .collect()
}

/// The shell words for the tokens of a raw command, which the framework never
/// parses: it cannot tell a flag's value from a positional, so a flag loses its
/// `=value`, and the token after a bare flag is masked as the value it may be.
/// The rest are the command's positionals, which only the hook can know are
/// secret. A token the hook rewrites keeps the hook's text.
pub(crate) fn raw_tail_words(tail: &[String], hook: Option<&RedactFn>) -> Vec<String> {
    let mut flags_ended = false;
    let mut after_bare_flag = false;
    let mut words = Vec::with_capacity(tail.len());
    for token in tail {
        let is_flag = !flags_ended && token.starts_with('-') && !looks_like_negative_number(token);
        if let Some(word) = hook_word(hook, token) {
            after_bare_flag = false;
            words.push(word);
        } else if is_flag && after_bare_flag && token != "--" {
            // It may be the value of the flag before it.
            after_bare_flag = !token.contains('=');
            words.push(MARKER.to_string());
        } else if is_flag {
            flags_ended = token == "--";
            after_bare_flag = !flags_ended && !token.contains('=');
            words.push(masked_flag_word(token));
        } else if after_bare_flag {
            after_bare_flag = false;
            words.push(MARKER.to_string());
        } else {
            words.push(shell_quote(token).into_owned());
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens<const N: usize>(tokens: [&str; N]) -> Vec<String> {
        tokens.map(str::to_string).to_vec()
    }

    fn names(path: &[String], token: &str) -> bool {
        match path {
            [] => token == "store",
            [only] if only == "store" => token == "unlock",
            _ => false,
        }
    }

    #[test]
    fn a_flag_word_keeps_its_key_and_masks_its_value() {
        assert_eq!(masked_flag_word("--k=v w"), "--k=<redacted>");
        assert_eq!(masked_flag_word("--a b=v"), "'--a b='<redacted>");
        assert_eq!(masked_flag_word("--k"), "--k");
        assert_eq!(mask_flag_value("-abc=5"), "-abc=<redacted>");
        assert_eq!(mask_flag_value("--k"), "--k");
    }

    #[test]
    fn the_hook_text_replaces_a_token_and_is_quoted() {
        let hook: &RedactFn = &|arg| arg.starts_with("pw=").then(|| "pw=<x>".to_string());
        assert_eq!(hook_word(Some(hook), "pw=1"), Some("'pw=<x>'".to_string()));
        assert_eq!(hook_word(Some(hook), "a b"), None);
        assert_eq!(hook_word(None, "pw=1"), None);
    }

    #[test]
    fn opaque_words_keep_only_flag_names_and_command_names() {
        let args = tokens(["--k=v", "store", "-x", "secret", "unlock", "--", "store"]);
        assert_eq!(
            opaque_words(&args, None, names),
            [
                "--k=<redacted>",
                "store",
                "-x",
                "<redacted>",
                "unlock",
                "--",
                "<redacted>"
            ]
        );
    }

    #[test]
    fn raw_tail_words_mask_flag_values_and_the_token_after_a_bare_flag() {
        let tail = tokens(["--k=v", "-C", "3", "pat", "-5", "--", "-x", "tail"]);
        assert_eq!(
            raw_tail_words(&tail, None),
            [
                "--k=<redacted>",
                "-C",
                "<redacted>",
                "pat",
                "-5",
                "--",
                "-x",
                "tail"
            ]
        );
        let hook: &RedactFn = &|arg| (arg == "pat").then(|| "p".to_string());
        assert_eq!(raw_tail_words(&tail, Some(hook))[3], "p");
    }
}
