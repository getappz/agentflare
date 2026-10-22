use super::{HarnessTree, RefinementError};

pub fn validate_tree(tree: &HarnessTree) -> Result<(), RefinementError> {
    let invalid = |s: String| RefinementError::Invalid(s);
    let agents = tree
        .get("AGENTS.md")
        .ok_or_else(|| invalid("AGENTS.md is required".into()))?;
    if agents.trim().is_empty()
        || agents.chars().count() > 8000
        || tree.iter().map(|(p, s)| p.len() + s.len()).sum::<usize>() > 256 * 1024
    {
        return Err(invalid(
            "empty or oversized harness (AGENTS.md: 8000 characters; tree: 256 KiB)".into(),
        ));
    }
    for (path, body) in tree {
        if path == "AGENTS.md" {
            continue;
        }
        let parts: Vec<_> = path.split('/').collect();
        let name = parts.get(2).copied().unwrap_or("");
        let device = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&device.as_str())
            || (device.len() == 4
                && (device.starts_with("COM") || device.starts_with("LPT"))
                && device.as_bytes()[3].is_ascii_digit());
        if parts.len() != 4
            || parts[0] != ".agents"
            || parts[1] != "skills"
            || parts[3] != "SKILL.md"
            || name.is_empty()
            || !name.as_bytes()[0].is_ascii_lowercase() && !name.as_bytes()[0].is_ascii_digit()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || reserved
        {
            return Err(invalid(format!(
                "unsupported or unsafe harness path: {path}"
            )));
        }
        let mut lines = body.lines();
        let mut description = false;
        let mut closed = false;
        let opens = lines.next() == Some("---");
        if opens {
            for line in lines {
                if line == "---" {
                    closed = true;
                    break;
                }
                if let Some(value) = line.strip_prefix("description:") {
                    description = !value.trim().trim_matches(['\'', '"']).is_empty();
                }
            }
        }
        let front = opens && closed && description;
        if !front || !agents.contains(path) {
            return Err(invalid(format!(
                "{path} needs frontmatter description and a trigger reference in AGENTS.md"
            )));
        }
    }
    Ok(())
}
