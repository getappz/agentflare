//! Prompt-template variable expansion (`{{input}}` / `{{var}}` / `{{params.x}}`).
//!
//! Ported from OpenFang `workflow.rs::expand_variables` (MIT/Apache-2.0).

use std::collections::HashMap;

/// Replace `{{input}}` with the current pipeline input, each `{{var}}` with
/// its stored value, then each `{{params.<dotted.path>}}` with the matching
/// value from the structured trigger payload. Unresolved placeholders (a
/// missing var, or a `params.` path with no match) are left as-is.
pub fn expand_variables(
    template: &str,
    input: &str,
    vars: &HashMap<String, String>,
    params: &serde_json::Value,
) -> String {
    let mut result = template.replace("{{input}}", input);
    for (key, value) in vars {
        result = result.replace(&format!("{{{{{key}}}}}"), value);
    }
    expand_params(&result, params)
}

/// Resolve every `{{params.<dotted.path>}}` placeholder against `params` via
/// a recursive `serde_json::Value` walk keyed on `.`-split segments. A
/// missing path (or an unterminated `{{`) is left as literal text.
fn expand_params(template: &str, params: &serde_json::Value) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{params.") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            result.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let path = &after_open["params.".len()..end];
        match resolve_params_path(params, path) {
            Some(text) => result.push_str(&text),
            None => result.push_str(&format!("{{{{params.{path}}}}}")),
        }
        rest = &after_open[end + 2..];
    }
    result.push_str(rest);
    result
}

/// Dotted-path lookup into a `serde_json::Value`, stringifying the result
/// (raw for strings, compact JSON for everything else).
fn resolve_params_path(params: &serde_json::Value, path: &str) -> Option<String> {
    let mut current = params;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(match current {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Record a step's output under `output_var` if one is set; returns the
/// (possibly new) variables map so the caller can persist it.
pub fn capture_output(vars: &mut HashMap<String, String>, output_var: Option<&str>, output: &str) {
    if let Some(name) = output_var {
        vars.insert(name.to_string(), output.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_input_and_variables() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Alice".to_string());
        vars.insert("task".to_string(), "code review".to_string());

        let out = expand_variables(
            "Hello {{name}}, do {{task}} on {{input}}",
            "main.rs",
            &vars,
            &serde_json::Value::Null,
        );
        assert_eq!(out, "Hello Alice, do code review on main.rs");
    }

    #[test]
    fn unresolved_placeholders_left_as_is() {
        let out = expand_variables(
            "{{input}} and {{missing}}",
            "x",
            &HashMap::new(),
            &serde_json::Value::Null,
        );
        assert_eq!(out, "x and {{missing}}");
    }

    #[test]
    fn expands_params_dotted_path() {
        let params = serde_json::json!({"userId": "u-1", "metadata": {"foo": "bar"}});
        let out = expand_variables(
            "user={{params.userId}} foo={{params.metadata.foo}}",
            "",
            &HashMap::new(),
            &params,
        );
        assert_eq!(out, "user=u-1 foo=bar");
    }

    #[test]
    fn unresolved_params_path_left_as_is() {
        let params = serde_json::json!({"userId": "u-1"});
        let out = expand_variables("{{params.missing}}", "", &HashMap::new(), &params);
        assert_eq!(out, "{{params.missing}}");
    }

    #[test]
    fn capture_output_stores_named_var() {
        let mut vars = HashMap::new();
        capture_output(&mut vars, Some("analysis"), "the analysis");
        assert_eq!(
            vars.get("analysis").map(String::as_str),
            Some("the analysis")
        );
    }
}
