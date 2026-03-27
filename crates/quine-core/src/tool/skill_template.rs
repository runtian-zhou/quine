use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::skill::SkillToolDef;

/// Default timeout for skill template tool execution (120 seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// A tool generated from a skill's tool definition.
///
/// Substitutes `{param}` placeholders in the command template with argument
/// values and executes the resulting command via `/bin/sh`.
pub struct SkillTemplateTool {
    def: SkillToolDef,
}

impl SkillTemplateTool {
    pub fn new(def: SkillToolDef) -> Self {
        Self { def }
    }
}

/// Shell-escape a string value to prevent command injection.
///
/// Wraps the value in single quotes and escapes any embedded single quotes.
fn shell_escape(value: &str) -> String {
    // Replace ' with '\'' (end quote, escaped quote, start quote).
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[async_trait]
impl Tool for SkillTemplateTool {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.def.parameters.clone()
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        if self.def.handler != "bash" {
            return Err(ToolError::Internal {
                message: format!("unsupported handler: {}", self.def.handler),
            });
        }

        // Substitute {param} placeholders with shell-escaped argument values.
        let mut command = self.def.command_template.clone();
        if let Some(obj) = arguments.as_object() {
            for (key, value) in obj {
                let raw = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let escaped = shell_escape(&raw);
                command = command.replace(&format!("{{{key}}}"), &escaped);
            }
        }

        // Check for unsubstituted placeholders.
        if let Some(start) = command.find('{') {
            if let Some(end) = command[start..].find('}') {
                let param = &command[start + 1..start + end];
                return Err(ToolError::InvalidArguments {
                    message: format!("missing required parameter: {param}"),
                });
            }
        }

        let timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS);

        let result = tokio::time::timeout(timeout, async {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&command)
                .current_dir(&context.working_directory)
                .output()
                .await
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                let content =
                    format!("exit code: {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}");

                if exit_code == 0 {
                    Ok(ToolOutput::success(content))
                } else {
                    Ok(ToolOutput::error(content))
                }
            }
            Ok(Err(e)) => Err(ToolError::Internal {
                message: format!("failed to execute command: {e}"),
            }),
            Err(_) => Err(ToolError::Timeout {
                seconds: DEFAULT_TIMEOUT_SECS,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool_def(command_template: &str) -> SkillToolDef {
        SkillToolDef {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "count": { "type": "integer" }
                },
                "required": ["name"]
            }),
            handler: "bash".to_string(),
            command_template: command_template.to_string(),
        }
    }

    #[test]
    fn shell_escape_basic() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn shell_escape_with_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_with_special_chars() {
        assert_eq!(shell_escape("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    #[test]
    fn tool_name_and_description() {
        let tool = SkillTemplateTool::new(make_tool_def("echo {name}"));
        assert_eq!(tool.name(), "test_tool");
        assert_eq!(tool.description(), "A test tool");
    }

    #[test]
    fn tool_parameters_schema() {
        let tool = SkillTemplateTool::new(make_tool_def("echo {name}"));
        let schema = tool.parameters_schema();
        assert!(schema.get("properties").is_some());
    }

    #[tokio::test]
    async fn execute_substitutes_params() {
        let tool = SkillTemplateTool::new(make_tool_def("echo {name}"));
        let args = serde_json::json!({ "name": "world" });

        let ctx = ExecutionContext {
            session_id: crate::session::SessionId::new(),
            filesystem: std::sync::Arc::new(crate::filesystem::NullFilesystem),
            working_directory: std::env::temp_dir(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            core_input: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };

        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("world"));
    }

    #[tokio::test]
    async fn execute_missing_param_errors() {
        let tool = SkillTemplateTool::new(make_tool_def("echo {name} {missing}"));
        let args = serde_json::json!({ "name": "hello" });

        let ctx = ExecutionContext {
            session_id: crate::session::SessionId::new(),
            filesystem: std::sync::Arc::new(crate::filesystem::NullFilesystem),
            working_directory: std::env::temp_dir(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            core_input: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };

        let result = tool.execute(args, &ctx).await;
        match result {
            Err(ToolError::InvalidArguments { message }) => {
                assert_eq!(message, "missing required parameter: missing");
            }
            other => panic!("expected InvalidArguments, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_unsupported_handler() {
        let mut def = make_tool_def("echo hello");
        def.handler = "python".to_string();
        let tool = SkillTemplateTool::new(def);
        let args = serde_json::json!({});

        let ctx = ExecutionContext {
            session_id: crate::session::SessionId::new(),
            filesystem: std::sync::Arc::new(crate::filesystem::NullFilesystem),
            working_directory: std::env::temp_dir(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            core_input: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };

        let result = tool.execute(args, &ctx).await;
        match result {
            Err(ToolError::Internal { message }) => {
                assert!(message.contains("unsupported handler"));
            }
            other => panic!("expected Internal error, got: {other:?}"),
        }
    }
}
