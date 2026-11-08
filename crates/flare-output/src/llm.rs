//! LLM invocation: raw HTTP to the Anthropic API when `ANTHROPIC_API_KEY` is
//! set, else shell out to `claude --print` (handles desktop/OAuth auth).
//! Every I/O boundary here is explicit UTF-8 — unlike the ported Python
//! version, which only pinned UTF-8 on this subprocess path and left plain
//! file I/O elsewhere on the platform-default locale encoding (the bug this
//! whole port exists partly to eliminate).

use crate::error::CavemanError;
use std::io::Write as _;
use std::process::Stdio;

pub trait Llm {
    /// # Errors
    ///
    /// Returns `Err` if the underlying HTTP request or `claude --print`
    /// subprocess fails.
    fn call(&self, prompt: &str) -> Result<String, CavemanError>;
}

#[derive(Default)]
pub struct RealLlm;

impl Llm for RealLlm {
    fn call(&self, prompt: &str) -> Result<String, CavemanError> {
        if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
            return call_via_api(&api_key, prompt, None);
        }
        call_via_cli(prompt, None, false)
    }
}

/// A configurable text-only model call. CLI customizations, tools, MCP and persistence are disabled.
/// Other callers using `RealLlm` retain their existing behavior.
#[derive(Default)]
pub struct TextLlm {
    pub model: Option<String>,
}

impl Llm for TextLlm {
    fn call(&self, prompt: &str) -> Result<String, CavemanError> {
        if let Some(api_key) = env_model("ANTHROPIC_API_KEY") {
            return call_via_api(&api_key, prompt, self.model.as_deref());
        }
        call_via_cli(prompt, self.model.as_deref(), true)
    }
}

fn env_model(key: &str) -> Option<String> {
    // A blank value (e.g. `FLARE_OUTPUT_MODEL=""` from an unset shell var
    // interpolated into an env file) must not win over the next fallback —
    // treat it the same as unset.
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn api_body(prompt: &str, model_override: Option<&str>) -> serde_json::Value {
    let model = model_override
        .map(str::to_owned)
        .or_else(|| env_model("FLARE_OUTPUT_MODEL"))
        .or_else(|| env_model("CAVEMAN_MODEL"))
        .unwrap_or_else(|| "claude-sonnet-4-5".to_string());
    serde_json::json!({
        "model": model,
        "max_tokens": 8192,
        "messages": [{"role": "user", "content": prompt}],
    })
}

fn call_via_api(
    api_key: &str,
    prompt: &str,
    model_override: Option<&str>,
) -> Result<String, CavemanError> {
    let body = api_body(prompt, model_override);
    let resp = ureq::post("https://api.anthropic.com/v1/messages")
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .timeout(std::time::Duration::from_mins(2))
        .send_json(body)
        .map_err(|e| CavemanError::Llm(format!("API call failed: {e}")))?;
    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| CavemanError::Llm(format!("bad API response: {e}")))?;
    json["content"][0]["text"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| CavemanError::Llm("API response missing content[0].text".to_string()))
}

fn cli_args(model: Option<&str>, text_only: bool) -> Vec<String> {
    let mut args = vec!["--print".to_string()];
    if let Some(model) = model {
        args.extend(["--model".into(), model.into()]);
    }
    if text_only {
        args.extend(
            [
                "--safe-mode",
                "--tools",
                "",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--disable-slash-commands",
                "--no-session-persistence",
            ]
            .map(str::to_owned),
        );
    }
    args
}

fn call_via_cli(
    prompt: &str,
    model: Option<&str>,
    text_only: bool,
) -> Result<String, CavemanError> {
    let claude_bin =
        which::which("claude").map_or_else(|_| "claude".to_string(), |p| p.display().to_string());
    let mut child = flare_process::command(&claude_bin)
        .args(cli_args(model, text_only))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CavemanError::Llm(format!("spawn '{claude_bin}' failed: {e}")))?;
    // Write stdin on a separate thread, concurrently with wait_with_output()
    // draining stdout/stderr below — writing the whole prompt first and only
    // then waiting would deadlock if the child fills its stdout/stderr pipe
    // buffer before finishing reading stdin (both sides then block forever).
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let prompt_owned = prompt.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(prompt_owned.as_bytes()));
    let output = child
        .wait_with_output()
        .map_err(|e| CavemanError::Llm(format!("'{claude_bin}' failed: {e}")))?;
    let write_result = writer.join().map_err(|_| {
        CavemanError::Llm(format!("stdin writer thread for '{claude_bin}' panicked"))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CavemanError::Llm(format!("Claude call failed:\n{stderr}")));
    }
    // A stdin-write failure (typically a broken pipe) only means something if
    // the child *also* failed -- checked above. A child that exits
    // successfully without draining all of stdin (it printed a canned
    // response and exited, it only needed the first N bytes, etc.) closing
    // its read end early is the ordinary "downstream closed the pipe early"
    // case, not a real error; surfacing it as fatal here is exactly what
    // made this path flaky under CI's compile-load scheduling jitter.
    if let Err(e) = write_result {
        eprintln!("warning: write to '{claude_bin}' stdin did not complete: {e}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
pub struct FakeLlm {
    pub responses: std::cell::RefCell<std::collections::VecDeque<String>>,
}

#[cfg(test)]
impl FakeLlm {
    pub fn queue(responses: &[&str]) -> Self {
        Self {
            responses: std::cell::RefCell::new(
                responses
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
impl Llm for FakeLlm {
    fn call(&self, _prompt: &str) -> Result<String, CavemanError> {
        self.responses
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| CavemanError::Llm("FakeLlm: no more queued responses".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_model_options_override_model_and_disable_cli_tools() {
        let body = api_body("summarize", Some("explicit-model"));
        assert_eq!(body["model"], "explicit-model");
        assert_eq!(body["messages"][0]["content"], "summarize");
        let args = cli_args(Some("explicit-model"), true);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "explicit-model"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert!(args.contains(&"--no-session-persistence".to_string()));
        assert!(args.contains(&"--safe-mode".to_string()));
        assert_eq!(cli_args(None, false), ["--print"]);
    }

    #[test]
    fn fake_llm_returns_queued_responses_in_order() {
        let fake = FakeLlm::queue(&["first", "second"]);
        assert_eq!(fake.call("p").unwrap(), "first");
        assert_eq!(fake.call("p").unwrap(), "second");
    }

    #[test]
    fn fake_llm_errors_when_exhausted() {
        let fake = FakeLlm::queue(&["only"]);
        fake.call("p").unwrap();
        assert!(fake.call("p").is_err());
    }
}
