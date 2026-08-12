//! Prompt-template variable expansion (`{{input}}` / `{{var}}`).
//!
//! Ported from OpenFang `workflow.rs::expand_variables` (MIT/Apache-2.0).

use std::collections::HashMap;

/// Replace `{{input}}` with the current pipeline input, then each `{{var}}`
/// with its stored value. Unresolved placeholders are left as-is.
pub fn expand_variables(template: &str, input: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.replace("{{input}}", input);
    for (key, value) in vars {
        result = result.replace(&format!("{{{{{key}}}}}"), value);
    }
    result
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

        let out = expand_variables("Hello {{name}}, do {{task}} on {{input}}", "main.rs", &vars);
        assert_eq!(out, "Hello Alice, do code review on main.rs");
    }

    #[test]
    fn unresolved_placeholders_left_as_is() {
        let out = expand_variables("{{input}} and {{missing}}", "x", &HashMap::new());
        assert_eq!(out, "x and {{missing}}");
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
