//! Validates every `agentflare ...` command quoted in shipped docs/skills/hook
//! messages against the live clap tree (no hand-kept table).

use clap::{Arg, Command, CommandFactory};

/// Known real drift: `(file, command)` pairs that are tolerated. Keep empty
/// unless drift is intentional; list each entry with the reason.
const ALLOW: &[(&str, &str)] = &[];

/// Shipped text that tells users/agents which `agentflare` commands to run,
/// discovered from `git ls-files` so new skills/hooks are covered automatically.
fn is_shipped(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path == "AGENTS.md"
        || (path.starts_with(".claude/skills/") && path.ends_with(".md"))
        || (path.starts_with(".claude/commands/") && path.ends_with(".md"))
        || (path.starts_with("crates/flare-code/src/skill") && path.ends_with(".md"))
        || (path.starts_with("plugins/") && (path.ends_with(".md") || name == "hooks.json"))
        || path == "hooks/hooks.json"
        || (path.starts_with("src/hook") && path.ends_with(".rs") && !path.contains("test"))
}

fn shipped_files(base: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["ls-files"])
        .current_dir(base)
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "git ls-files failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|p| is_shipped(p))
        .map(str::to_string)
        .collect()
}

/// Cut `s` at the first shell operator outside `<..>`/`[..]`/`(..)`/`{..}`.
fn cut_command(s: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth -= 1,
            '>' if depth > 0 => depth -= 1,
            '|' | ';' | '>' | '#' | '&' | '"' | '\\' if depth <= 0 => return &s[..i],
            _ => {}
        }
    }
    s
}

/// Every `agentflare ...` command inside `span`, wherever it occurs in it.
fn commands_in(span: &str, out: &mut Vec<String>) {
    let mut from = 0;
    while let Some(off) = span[from..].find("agentflare ") {
        let at = from + off;
        from = at + "agentflare ".len();
        let prev = span[..at].chars().next_back();
        if prev.is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '@')) {
            continue;
        }
        out.push(cut_command(&span[at..]).trim().to_string());
    }
}

fn extract(line: &str, in_fence: bool, whole_line: bool) -> Vec<String> {
    let mut out = Vec::new();
    if in_fence || whole_line {
        commands_in(line, &mut out);
    } else {
        // Inline code spans: text between paired backticks.
        let parts: Vec<&str> = line.split('`').collect();
        for (i, p) in parts.iter().enumerate() {
            if i % 2 == 1 && i + 1 < parts.len() {
                commands_in(p, &mut out);
            }
        }
    }
    out
}

fn find_arg<'a>(cmd: &'a Command, chain: &[&'a Command], flag: &str) -> Option<&'a Arg> {
    let long = flag.strip_prefix("--");
    let short = if long.is_none() {
        flag.chars().nth(1)
    } else {
        None
    };
    let matches = |a: &Arg| {
        (long.is_some()
            && (a.get_long() == long
                || long.is_some_and(|l| a.get_all_aliases().is_some_and(|v| v.contains(&l)))))
            || (short.is_some() && a.get_short() == short)
    };
    cmd.get_arguments()
        .find(|a| matches(a))
        // Ancestors only contribute their global args.
        .or_else(|| {
            chain
                .iter()
                .flat_map(|c| c.get_arguments())
                .find(|a| a.is_global_set() && matches(a))
        })
}

/// Err(reason) when the command path or a flag does not exist.
fn validate(root: &Command, text: &str) -> Result<(), String> {
    let mut chain: Vec<&Command> = vec![];
    let mut cur = root;
    let mut descending = true;
    let mut skip_value = false;
    for word in text.split_whitespace().skip(1) {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        if word.starts_with(['<', '[', '$', '\'', '(', '{', '.']) {
            descending = false;
            continue;
        }
        if word.starts_with('-') {
            let name = word.split('=').next().unwrap_or(word);
            if !name
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '-')
                || name == "--"
            {
                continue;
            }
            if matches!(name, "--help" | "-h" | "--version" | "-V") {
                continue;
            }
            let Some(arg) = find_arg(cur, &chain, name) else {
                return Err(format!("unknown flag `{name}` for `{}`", cur.get_name()));
            };
            skip_value =
                !word.contains('=') && arg.get_num_args().is_some_and(|r| r.takes_values());
            continue;
        }
        if descending && cur.has_subcommands() {
            match cur.find_subcommand(word) {
                Some(sub) => {
                    chain.push(cur);
                    cur = sub;
                }
                None if word.chars().all(|c| c.is_ascii_lowercase() || c == '-') => {
                    return Err(format!(
                        "unknown subcommand `{word}` under `{}`",
                        cur.get_name()
                    ));
                }
                None => descending = false,
            }
        } else {
            descending = false;
        }
    }
    Ok(())
}

/// Scan `content`; return `file:line: command — reason` for each bad command.
fn check(root: &Command, file: &str, content: &str, allow: &[(&str, &str)]) -> Vec<String> {
    let whole_line = file.ends_with(".json");
    let mut bad = vec![];
    let mut in_fence = false;
    for (i, line) in content.lines().enumerate() {
        let bare = line
            .trim_start()
            .trim_start_matches("///")
            .trim_start_matches("//!")
            .trim_start();
        if !whole_line && bare.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        for cmd in extract(line, in_fence, whole_line) {
            if allow.contains(&(file, cmd.as_str())) {
                continue;
            }
            if let Err(why) = validate(root, &cmd) {
                bad.push(format!("{file}:{}: `{cmd}` — {why}", i + 1));
            }
        }
    }
    bad
}

#[test]
fn shipped_docs_only_reference_real_agentflare_commands() {
    let root = super::Cli::command();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = shipped_files(base);
    assert!(
        files.iter().any(|f| f == "AGENTS.md"),
        "discovery found {files:?}"
    );
    let mut bad = vec![];
    for f in &files {
        let text = std::fs::read_to_string(base.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
        bad.extend(check(&root, f, &text, ALLOW));
    }
    assert!(
        bad.is_empty(),
        "stale agentflare commands:\n{}",
        bad.join("\n")
    );
}

#[test]
fn wrong_subcommand_or_flag_is_reported_with_file_and_line() {
    let root = super::Cli::command();
    let fixture = "intro\n```bash\nagentflare item list\nagentflare itemz list\n```\nuse `agentflare doctor --no-such-flag` here\n";
    let bad = check(&root, "fixture.md", fixture, &[]);
    assert_eq!(bad.len(), 2, "{bad:?}");
    assert!(bad[0].starts_with("fixture.md:4:"), "{bad:?}");
    assert!(bad[1].starts_with("fixture.md:6:"), "{bad:?}");
}

#[test]
fn extractor_and_validator_edge_cases() {
    let root = super::Cli::command();
    // Valid: plain, embedded after `&&`/env prefix, placeholder, --flag=value, flag value, alias-free.
    let ok = "`agentflare item list`\n`cd x && agentflare doctor`\n`FOO=1 agentflare doctor`\n`agentflare item get <id>`\n";
    assert!(check(&root, "ok.md", ok, &[]).is_empty());
    // Embedded commands are no longer missed.
    let bad = check(&root, "e.md", "`cd x && agentflare itemz list`\n", &[]);
    assert_eq!(bad.len(), 1, "{bad:?}");
    // Unknown short flag and unknown `--flag=value` on a leaf.
    assert_eq!(
        check(&root, "s.md", "`agentflare doctor -Z`\n", &[]).len(),
        1
    );
    assert!(validate(&root, "agentflare doctor --no-such=1").is_err());
    // Built-in --help is accepted anywhere.
    assert!(validate(&root, "agentflare item list --help").is_ok());
    // Rust doc fences are tracked too.
    let rs = "/// ```\n/// agentflare nosuch\n/// ```\n";
    assert_eq!(check(&root, "x.rs", rs, &[]).len(), 1);
    // JSON command strings.
    let js = r#"{"command": "agentflare nosuch --x"}"#;
    assert_eq!(check(&root, "h.json", js, &[]).len(), 1);
    // ALLOW entries are honoured.
    let allow = [("a.md", "agentflare nosuch")];
    assert!(check(&root, "a.md", "`agentflare nosuch`\n", &allow).is_empty());
}
