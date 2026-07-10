// Wrangler-style `.dev.vars` loader for `agentflare run`. Injects local env vars
// into a launched agent session. Format is dotenv (KEY=VALUE, `#` comments,
// optionally quoted values). Multi-stage mirrors wrangler: with a stage,
// `.dev.vars.<stage>` REPLACES the base `.dev.vars` entirely — per the docs,
// "if .dev.vars.<environment-name> exists then only this will be loaded; the
// .dev.vars file will not be loaded".
use std::path::{Path, PathBuf};

/// Pick the file for `stage` (replacement semantics), parse it, and return
/// (path, vars). `None` when no matching file exists.
pub fn load(dir: &Path, stage: Option<&str>) -> Option<(PathBuf, Vec<(String, String)>)> {
    let staged = stage.map(|s| dir.join(format!(".dev.vars.{s}")));
    let path = match staged {
        Some(p) if p.exists() => p,
        _ => dir.join(".dev.vars"),
    };
    let content = std::fs::read_to_string(&path).ok()?;
    Some((path, parse(&content)))
}

/// Minimal dotenv: skip blank and `#`-comment lines, drop an optional `export`
/// prefix, split on the first `=`, trim, and strip one layer of matching quotes
/// from the value. Intentionally does not do trailing-comment or escape parsing
/// — values with `#` stay intact.
fn parse(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("export ").unwrap_or(l).split_once('='))
        .map(|(k, v)| (k.trim().to_string(), unquote(v.trim()).to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if v.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dotenv_with_comments_quotes_and_export() {
        let vars = parse("# comment\nexport A=1\nB=\"two words\"\nC='x'\n\n  D = 4 \nBAD");
        assert_eq!(
            vars,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two words".into()),
                ("C".into(), "x".into()),
                ("D".into(), "4".into()),
            ]
        );
    }

    #[test]
    fn value_containing_hash_is_preserved() {
        assert_eq!(parse("K=a#b"), vec![("K".into(), "a#b".into())]);
    }

    #[test]
    fn staged_file_replaces_base() {
        let dir = std::env::temp_dir().join(format!("agentflare-devvars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".dev.vars"), "A=base\n").unwrap();
        std::fs::write(dir.join(".dev.vars.prod"), "A=prod\n").unwrap();

        let (path, vars) = load(&dir, Some("prod")).unwrap();
        assert!(path.ends_with(".dev.vars.prod"));
        assert_eq!(vars, vec![("A".into(), "prod".into())]);

        // No staged file → falls back to base.
        let (path, vars) = load(&dir, Some("missing")).unwrap();
        assert!(path.ends_with(".dev.vars"));
        assert_eq!(vars, vec![("A".into(), "base".into())]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn returns_none_when_no_file() {
        let dir = std::env::temp_dir().join(format!("agentflare-devvars-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load(&dir, None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
