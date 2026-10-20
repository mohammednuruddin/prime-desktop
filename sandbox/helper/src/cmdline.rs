//! Turns an argv array into the single command line a Windows process receives.
//!
//! MXC takes one command string, so the quoting is ours to get right. The rules are the ones the MSVC runtime
//! and `CommandLineToArgvW` use to split it back: a quote is escaped with a backslash, and backslashes that
//! come right before a quote, or before the closing quote, are doubled. Getting this wrong is how an argument
//! turns into two, so the tests below parse the result back with a reference parser.

#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

#[cfg_attr(not(windows), allow(dead_code))]
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Turns an argv array into one `sh -c` command line that a POSIX shell splits back into exactly `argv`. Every
/// argument is wrapped in single quotes unless it is made only of characters no shell treats specially, and a
/// single quote inside is closed, escaped and reopened. Nothing inside single quotes is expanded, so `$(id)`,
/// backticks, `;`, `&`, globs and newlines stay text.
#[cfg_attr(windows, allow(dead_code))]
pub fn posix(argv: &[String]) -> String {
    argv.iter().map(|a| posix_quote(a)).collect::<Vec<_>>().join(" ")
}

#[cfg_attr(windows, allow(dead_code))]
fn posix_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented parsing rules for arguments after the program name (CommandLineToArgvW, 2008+ rules).
    fn parse(line: &str) -> Vec<String> {
        let chars: Vec<char> = line.chars().collect();
        let (mut args, mut i) = (Vec::new(), 0);
        while i < chars.len() {
            while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            let (mut arg, mut in_quotes) = (String::new(), false);
            while i < chars.len() {
                let mut backslashes = 0;
                while i < chars.len() && chars[i] == '\\' {
                    backslashes += 1;
                    i += 1;
                }
                if i < chars.len() && chars[i] == '"' {
                    arg.push_str(&"\\".repeat(backslashes / 2));
                    if backslashes % 2 == 1 {
                        arg.push('"');
                    } else {
                        in_quotes = !in_quotes;
                    }
                    i += 1;
                } else {
                    arg.push_str(&"\\".repeat(backslashes));
                    if i >= chars.len() || (!in_quotes && (chars[i] == ' ' || chars[i] == '\t')) {
                        break;
                    }
                    arg.push(chars[i]);
                    i += 1;
                }
            }
            args.push(arg);
        }
        args
    }

    #[test]
    fn every_awkward_argument_survives_a_round_trip() {
        let argv: Vec<String> = [
            "plain",
            "",
            "with space",
            "tab\there",
            r#"say "hi""#,
            r"trailing\",
            r"trailing space \",
            r"C:\Program Files\App\app.exe",
            r#"\\"quoted\\""#,
            r"back\\slashes\\ and space",
            "%SystemRoot% %%x ^& | > < &&",
            "$(id) `id` ; touch pwned",
            "新しい ファイル",
            r#""#,
            r#"\""#,
            r#"a\"b c"#,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let line = windows(&argv);
        assert_eq!(parse(&line), argv, "command line was: {line}");
    }

    /// Asks a real shell to split the line: `printf '%s\0' <line>` prints each argument NUL-terminated.
    #[cfg(unix)]
    #[test]
    fn posix_quoting_survives_a_real_shell() {
        let argv: Vec<String> = [
            "plain",
            "",
            "with space",
            "it's",
            "'''",
            r#"say "hi""#,
            r"back\slash and \n",
            "$(id) `id` ; touch pwned && echo $HOME | cat > /dev/null",
            "glob * ? [a-z] ~ {a,b}",
            "line one\nline two\ttab",
            "-n",
            "--flag=value",
            "%SystemRoot% %%x ^&",
            "新しい ファイル",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let line = format!("printf '%s\\0' {}", posix(&argv));
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&line)
            .output()
            .expect("sh runs");
        // Each argument ends with a NUL, so splitting leaves one empty chunk after the last.
        let mut parts: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        parts.pop();
        assert_eq!(parts, argv, "command line was: {line}");
    }

    #[test]
    fn a_simple_argument_is_left_alone() {
        assert_eq!(
            windows(&["git".into(), "status".into(), "--short".into()]),
            "git status --short"
        );
    }
}
