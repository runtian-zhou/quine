use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::conversation::ToolOutput;
use crate::skill::{build_skill_prompt, discover_skills};
use crate::tool::Tool;

pub struct SkillTool {
    working_dir: PathBuf,
}

impl SkillTool {
    pub fn new(working_dir: &Path) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
        }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        "Load a predefined skill into the conversation context. Skills are discovered from `skills/*/SKILL.md` files. Use action \"list\" to see available skills, or \"execute\" to load one by name."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "execute"],
                    "description": "\"list\" to show available skills, \"execute\" to load one into context"
                },
                "skill_name": {
                    "type": "string",
                    "description": "The name of the skill to load (required for 'execute')"
                },
                "input": {
                    "type": "string",
                    "description": "User input / instructions to pass to the skill (optional for 'execute')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let action = arguments["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("action is required"))?;

        match action {
            "list" => {
                let skills = discover_skills(&self.working_dir);
                if skills.is_empty() {
                    return Ok(ToolOutput {
                        success: true,
                        output: "No skills found. Create skills in `skills/<name>/SKILL.md`.".to_string(),
                    });
                }
                let mut lines = vec!["Available skills:".to_string()];
                for skill in &skills {
                    lines.push(format!("  - **{}**: {}", skill.name, skill.description));
                }
                Ok(ToolOutput {
                    success: true,
                    output: lines.join("\n"),
                })
            }
            "execute" => {
                let skill_name = arguments["skill_name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("skill_name is required for 'execute'"))?;
                let user_input = arguments["input"].as_str().unwrap_or("");

                let skills = discover_skills(&self.working_dir);
                let skill = skills.iter().find(|s| s.name == skill_name);

                let Some(skill) = skill else {
                    let available: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
                    return Ok(ToolOutput {
                        success: false,
                        output: format!(
                            "Skill '{}' not found. Available: {}",
                            skill_name,
                            if available.is_empty() {
                                "none".to_string()
                            } else {
                                available.join(", ")
                            }
                        ),
                    });
                };

                let prompt = build_skill_prompt(skill, user_input);
                Ok(ToolOutput {
                    success: true,
                    output: prompt,
                })
            }
            _ => Ok(ToolOutput {
                success: false,
                output: format!("Unknown action '{}'. Use: list, execute", action),
            }),
        }
    }
}
