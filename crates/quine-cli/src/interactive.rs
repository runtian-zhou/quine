use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use quine_core::config::Config;
use quine_core::conversation::{Entry, ToolCall, ToolOutput};
use quine_core::log::ConversationLog;
use quine_core::prompt::build_system_prompt;
use quine_core::tool::ask_user::AskUserTool;
use quine_core::tool::subagent::SubagentTool;
use quine_core::tool::ToolRegistry;
use quine_llm::anthropic::AnthropicProvider;
use quine_llm::openai::OpenAiProvider;
use quine_llm::provider::LlmProvider;
use quine_llm::types::*;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::render::Renderer;

pub async fn run_chat(
    provider_name: &str,
    model: &str,
    base_url: Option<&str>,
    continue_from: Option<PathBuf>,
    stream: bool,
) -> anyhow::Result<()> {
    let config = Config::new(provider_name, model);
    let provider = create_provider(provider_name, base_url)?;

    let system_prompt = build_system_prompt(&config.working_dir);
    let registry = build_registry_with_subagent(
        &config.working_dir,
        Arc::clone(&provider),
        model.to_string(),
        system_prompt.clone(),
    );
    let tool_schemas = registry.all_schemas();
    let renderer = Renderer::new();

    let mut conv_log = if let Some(log_path) = &continue_from {
        println!("Continuing from: {}", log_path.display());
        ConversationLog::load(log_path)?
    } else {
        ConversationLog::new(model.to_string(), provider_name.to_string(), system_prompt.clone())
    };

    // Build messages from existing log entries for continuation.
    // Group consecutive ToolExecution entries into a single user message.
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

    for entry in &conv_log.entries {
        match entry {
            Entry::ToolExecution {
                tool_call_id,
                result,
                ..
            } => {
                pending_tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: result.output.clone(),
                });
            }
            other => {
                if !pending_tool_results.is_empty() {
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: ChatContent::Blocks(pending_tool_results.drain(..).collect()),
                    });
                }
                match other {
                    Entry::UserMessage { content } => {
                        messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: ChatContent::Text(content.clone()),
                        });
                    }
                    Entry::AssistantMessage {
                        content,
                        tool_calls,
                    } => {
                        let mut blocks = Vec::new();
                        if !content.is_empty() {
                            blocks.push(ContentBlock::Text {
                                text: content.clone(),
                            });
                        }
                        for tc in tool_calls {
                            blocks.push(ContentBlock::ToolUse {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: tc.arguments.clone(),
                            });
                        }
                        messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: if blocks.len() == 1
                                && matches!(&blocks[0], ContentBlock::Text { .. })
                            {
                                ChatContent::Text(content.clone())
                            } else {
                                ChatContent::Blocks(blocks)
                            },
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    // Flush any remaining tool results
    if !pending_tool_results.is_empty() {
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Blocks(pending_tool_results),
        });
    }

    println!("\x1b[1;36mQuine\x1b[0m - Self-bootstrapping CLI assistant");
    println!("\x1b[90mProvider: {} | Model: {}\x1b[0m", provider_name, model);
    println!("\x1b[90mType your message (Ctrl+D to exit)\x1b[0m\n");

    let mut rl = DefaultEditor::new()?;

    loop {
        let user_input = match rl.readline("\x1b[1;32m❯\x1b[0m ") {
            Ok(line) => {
                let trimmed = line.trim_end().to_string();
                if !trimmed.is_empty() {
                    let _ = rl.add_history_entry(&trimmed);
                }
                trimmed
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
            Err(e) => return Err(e.into()),
        };

        if user_input.is_empty() {
            continue;
        }
        if user_input == "/quit" || user_input == "/exit" {
            break;
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Text(user_input.clone()),
        });
        conv_log.push(Entry::UserMessage {
            content: user_input,
        });

        // Conversation loop: keep going while LLM wants to call tools
        loop {
            let request = CompletionRequest {
                model: model.to_string(),
                system: system_prompt.clone(),
                messages: messages.clone(),
                tools: tool_schemas.clone(),
                max_tokens: 8192,
            };

            let response = if stream {
                complete_with_streaming(&*provider, request, &renderer).await?
            } else {
                let spinner = crate::render::Spinner::start("Thinking...");
                let resp = provider.complete(request).await?;
                spinner.stop();
                if !resp.content.is_empty() {
                    renderer.print_assistant_message(&resp.content);
                }
                resp
            };

            // Record assistant message
            let tool_calls_for_log: Vec<ToolCall> = response
                .tool_calls
                .iter()
                .map(|tc| ToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: tc.arguments.clone(),
                })
                .collect();

            conv_log.push(Entry::AssistantMessage {
                content: response.content.clone(),
                tool_calls: tool_calls_for_log.clone(),
            });

            // Build assistant message for context
            let mut assistant_blocks = Vec::new();
            if !response.content.is_empty() {
                assistant_blocks.push(ContentBlock::Text {
                    text: response.content.clone(),
                });
            }
            for tc in &response.tool_calls {
                assistant_blocks.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                });
            }
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: if assistant_blocks.len() == 1
                    && matches!(&assistant_blocks[0], ContentBlock::Text { .. })
                {
                    ChatContent::Text(response.content.clone())
                } else {
                    ChatContent::Blocks(assistant_blocks)
                },
            });

            // If no tool calls, we're done with this turn
            if response.tool_calls.is_empty() {
                break;
            }

            // Execute tool calls and feed results back
            let mut tool_result_blocks = Vec::new();
            for tc in &response.tool_calls {
                renderer.print_tool_call(&tc.name, &tc.arguments);

                let result = match registry.get(&tc.name) {
                    Some(tool) => match tool.execute(tc.arguments.clone()).await {
                        Ok(output) => output,
                        Err(e) => ToolOutput {
                            success: false,
                            output: format!("Tool execution error: {}", e),
                        },
                    },
                    None => ToolOutput {
                        success: false,
                        output: format!("Unknown tool: {}", tc.name),
                    },
                };

                renderer.print_tool_result(result.success, &result.output);

                // Record tool execution
                conv_log.push(Entry::ToolExecution {
                    tool_call_id: tc.id.clone(),
                    tool_name: tc.name.clone(),
                    arguments: tc.arguments.clone(),
                    result: result.clone(),
                });

                tool_result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: result.output.clone(),
                });
            }

            // Add all tool results in a single user message
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: ChatContent::Blocks(tool_result_blocks),
            });
        }

        // Save log after each turn
        let log_path = config.log_dir.join(format!(
            "{}.json",
            conv_log.created_at.format("%Y%m%d_%H%M%S")
        ));
        conv_log.save(&log_path)?;
    }

    println!("\n\x1b[90mGoodbye!\x1b[0m");
    Ok(())
}

async fn complete_with_streaming(
    provider: &dyn LlmProvider,
    request: CompletionRequest,
    renderer: &Renderer,
) -> anyhow::Result<CompletionResponse> {
    let mut stream = provider.complete_stream(request).await?;

    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut current_tool_id = String::new();
    let mut current_tool_name = String::new();
    let mut current_tool_args = String::new();
    let mut stop_reason = StopReason::EndTurn;

    let mut spinner = Some(crate::render::Spinner::start("Thinking..."));

    while let Some(event) = stream.next().await {
        if let Some(s) = spinner.take() {
            s.stop();
        }
        match event? {
            StreamEvent::ContentDelta(text) => {
                renderer.print_delta(&text);
                content.push_str(&text);
            }
            StreamEvent::ToolCallStart { id, name } => {
                current_tool_id = id;
                current_tool_name = name;
                current_tool_args.clear();
            }
            StreamEvent::ToolCallDelta(json) => {
                current_tool_args.push_str(&json);
            }
            StreamEvent::ToolCallEnd => {
                if !current_tool_id.is_empty() {
                    let args: serde_json::Value =
                        serde_json::from_str(&current_tool_args).unwrap_or(serde_json::Value::Null);
                    tool_calls.push(ToolCall {
                        id: current_tool_id.clone(),
                        name: current_tool_name.clone(),
                        arguments: args,
                    });
                    current_tool_id.clear();
                    current_tool_name.clear();
                    current_tool_args.clear();
                }
            }
            StreamEvent::Done(reason) => {
                stop_reason = reason;
            }
        }
    }
    println!();

    // Handle any remaining tool call that wasn't closed with ToolCallEnd
    if !current_tool_id.is_empty() {
        let args: serde_json::Value =
            serde_json::from_str(&current_tool_args).unwrap_or(serde_json::Value::Null);
        tool_calls.push(ToolCall {
            id: current_tool_id,
            name: current_tool_name,
            arguments: args,
        });
    }

    Ok(CompletionResponse {
        content,
        tool_calls,
        stop_reason,
    })
}

/// Non-interactive print mode: run a single prompt and output the result.
pub async fn run_print(
    provider_name: &str,
    model: &str,
    base_url: Option<&str>,
    prompt: &str,
    working_dir: Option<PathBuf>,
    output_format: &str,
) -> anyhow::Result<()> {
    let wd = working_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config = Config::new(provider_name, model);
    let provider = create_provider(provider_name, base_url)?;

    let system_prompt = quine_core::prompt::build_system_prompt(&wd);
    let registry = build_registry_with_subagent(
        &wd,
        Arc::clone(&provider),
        model.to_string(),
        system_prompt.clone(),
    );
    let tool_schemas = registry.all_schemas();

    let mut conv_log = ConversationLog::new(
        model.to_string(),
        provider_name.to_string(),
        system_prompt.clone(),
    );

    let mut messages: Vec<ChatMessage> = vec![ChatMessage {
        role: "user".to_string(),
        content: ChatContent::Text(prompt.to_string()),
    }];
    conv_log.push(Entry::UserMessage {
        content: prompt.to_string(),
    });

    // Conversation loop: keep going while LLM wants to call tools
    loop {
        let request = CompletionRequest {
            model: model.to_string(),
            system: system_prompt.clone(),
            messages: messages.clone(),
            tools: tool_schemas.clone(),
            max_tokens: 8192,
        };

        let spinner = crate::render::Spinner::start("Thinking...");
        let response = provider.complete(request).await?;
        spinner.stop();

        let tool_calls_for_log: Vec<ToolCall> = response
            .tool_calls
            .iter()
            .map(|tc| ToolCall {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
            })
            .collect();

        conv_log.push(Entry::AssistantMessage {
            content: response.content.clone(),
            tool_calls: tool_calls_for_log.clone(),
        });

        let mut assistant_blocks = Vec::new();
        if !response.content.is_empty() {
            assistant_blocks.push(ContentBlock::Text {
                text: response.content.clone(),
            });
        }
        for tc in &response.tool_calls {
            assistant_blocks.push(ContentBlock::ToolUse {
                id: tc.id.clone(),
                name: tc.name.clone(),
                input: tc.arguments.clone(),
            });
        }
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: if assistant_blocks.len() == 1
                && matches!(&assistant_blocks[0], ContentBlock::Text { .. })
            {
                ChatContent::Text(response.content.clone())
            } else {
                ChatContent::Blocks(assistant_blocks)
            },
        });

        if response.tool_calls.is_empty() {
            break;
        }

        let mut tool_result_blocks = Vec::new();
        for tc in &response.tool_calls {
            let result = match registry.get(&tc.name) {
                Some(tool) => match tool.execute(tc.arguments.clone()).await {
                    Ok(output) => output,
                    Err(e) => ToolOutput {
                        success: false,
                        output: format!("Tool execution error: {}", e),
                    },
                },
                None => ToolOutput {
                    success: false,
                    output: format!("Unknown tool: {}", tc.name),
                },
            };

            conv_log.push(Entry::ToolExecution {
                tool_call_id: tc.id.clone(),
                tool_name: tc.name.clone(),
                arguments: tc.arguments.clone(),
                result: result.clone(),
            });

            tool_result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tc.id.clone(),
                content: result.output.clone(),
            });
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Blocks(tool_result_blocks),
        });
    }

    // Save log
    let log_path = config.log_dir.join(format!(
        "{}.json",
        conv_log.created_at.format("%Y%m%d_%H%M%S")
    ));
    conv_log.save(&log_path)?;

    // Output
    match output_format {
        "json" => {
            let output = serde_json::json!({
                "log_file": log_path.to_string_lossy(),
                "entries": conv_log.entries,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            // Print just the final assistant text
            for entry in conv_log.entries.iter().rev() {
                if let Entry::AssistantMessage { content, .. } = entry {
                    if !content.is_empty() {
                        println!("{}", content);
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Run a single-prompt agent loop (used by subagents). Returns the final assistant text.
/// The tool registry should NOT include the Subagent tool.
async fn run_subagent_loop(
    provider: &dyn LlmProvider,
    model: &str,
    system_prompt: &str,
    registry: &ToolRegistry,
    prompt: &str,
) -> anyhow::Result<String> {
    let tool_schemas = registry.all_schemas();
    let mut messages: Vec<ChatMessage> = vec![ChatMessage {
        role: "user".to_string(),
        content: ChatContent::Text(prompt.to_string()),
    }];

    loop {
        let request = CompletionRequest {
            model: model.to_string(),
            system: system_prompt.to_string(),
            messages: messages.clone(),
            tools: tool_schemas.clone(),
            max_tokens: 8192,
        };

        let response = provider.complete(request).await?;

        let mut assistant_blocks = Vec::new();
        if !response.content.is_empty() {
            assistant_blocks.push(ContentBlock::Text {
                text: response.content.clone(),
            });
        }
        for tc in &response.tool_calls {
            assistant_blocks.push(ContentBlock::ToolUse {
                id: tc.id.clone(),
                name: tc.name.clone(),
                input: tc.arguments.clone(),
            });
        }
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: if assistant_blocks.len() == 1
                && matches!(&assistant_blocks[0], ContentBlock::Text { .. })
            {
                ChatContent::Text(response.content.clone())
            } else {
                ChatContent::Blocks(assistant_blocks)
            },
        });

        if response.tool_calls.is_empty() {
            return Ok(response.content);
        }

        let mut tool_result_blocks = Vec::new();
        for tc in &response.tool_calls {
            let result = match registry.get(&tc.name) {
                Some(tool) => match tool.execute(tc.arguments.clone()).await {
                    Ok(output) => output,
                    Err(e) => ToolOutput {
                        success: false,
                        output: format!("Tool execution error: {}", e),
                    },
                },
                None => ToolOutput {
                    success: false,
                    output: format!("Unknown tool: {}", tc.name),
                },
            };

            tool_result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tc.id.clone(),
                content: result.output.clone(),
            });
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Blocks(tool_result_blocks),
        });
    }
}

/// Build a ToolRegistry with the Subagent tool included.
/// The subagent's inner loop uses a registry WITHOUT the Subagent tool.
fn build_registry_with_subagent(
    working_dir: &Path,
    provider: Arc<dyn LlmProvider>,
    model: String,
    system_prompt: String,
) -> ToolRegistry {
    let mut registry = ToolRegistry::register_defaults(working_dir);

    // Build the inner registry (no subagent) for the subagent's own tool loop
    let inner_working_dir = working_dir.to_path_buf();
    let completion_fn: quine_core::tool::subagent::CompletionFn =
        Arc::new(move |prompt: String| {
            let provider = Arc::clone(&provider);
            let model = model.clone();
            let system_prompt = system_prompt.clone();
            let working_dir = inner_working_dir.clone();
            Box::pin(async move {
                let inner_registry = ToolRegistry::register_defaults(&working_dir);
                run_subagent_loop(&*provider, &model, &system_prompt, &inner_registry, &prompt)
                    .await
            })
        });

    registry.register(Box::new(SubagentTool::new(completion_fn)));

    // AskUserQuestion tool: prompts the user for input via stdin
    let ask_fn: quine_core::tool::ask_user::AskUserFn = Arc::new(|question: String| {
        Box::pin(async move {
            println!("\n\x1b[1;33m? {}\x1b[0m", question);
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => Err(anyhow::anyhow!("EOF")),
                Ok(_) => Ok(line.trim_end().to_string()),
                Err(e) => Err(e.into()),
            }
        })
    });
    registry.register(Box::new(AskUserTool::new(ask_fn)));

    registry
}

fn create_provider(
    provider_name: &str,
    base_url: Option<&str>,
) -> anyhow::Result<Arc<dyn LlmProvider>> {
    match provider_name {
        "anthropic" => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY environment variable not set"))?;
            let mut provider = AnthropicProvider::new(api_key);
            if let Some(url) = base_url.or(std::env::var("ANTHROPIC_BASE_URL").ok().as_deref()) {
                provider = provider.with_base_url(url.to_string());
            }
            Ok(Arc::new(provider))
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY environment variable not set"))?;
            let mut provider = OpenAiProvider::new(api_key);
            if let Some(url) = base_url.or(std::env::var("OPENAI_BASE_URL").ok().as_deref()) {
                provider = provider.with_base_url(url.to_string());
            }
            Ok(Arc::new(provider))
        }
        _ => anyhow::bail!("Unknown provider: {}. Use 'anthropic' or 'openai'.", provider_name),
    }
}


