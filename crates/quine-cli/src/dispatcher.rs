use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use quine_core::config::Config;
use quine_core::conversation::ToolOutput;
use quine_core::log::ConversationLog;
use quine_core::tool::{GlobalContext, ToolRegistry};
use quine_core::worktree::Worktree;
use quine_llm::provider::LlmProvider;
use quine_llm::types::*;

use crate::agent::{Agent, AgentConfig, AgentId, AgentState};
use crate::commands::{self, CommandResult, SessionUsage};
use crate::event::Event;
use crate::permissions::PermissionManager;
use crate::render::Renderer;

/// Tracks a subagent's relationship to its parent.
struct SubagentInfo {
    parent_id: AgentId,
    tool_call_id: String,
    _worktree: Option<Worktree>,
}

/// When an AskUserQuestion tool is pending, the next user input is routed here.
struct PendingAskUser {
    agent_id: AgentId,
    tool_call_id: String,
    tool_name: String,
    arguments: serde_json::Value,
}

pub struct Dispatcher {
    agents: HashMap<AgentId, Agent>,
    registries: HashMap<AgentId, ToolRegistry>,
    subagent_info: HashMap<AgentId, SubagentInfo>,
    pending_ask_user: Option<PendingAskUser>,
    event_tx: mpsc::Sender<Event>,
    event_rx: mpsc::Receiver<Event>,
    provider: Arc<dyn LlmProvider>,
    config: Config,
    model: String,
    system_prompt: String,
    global_ctx: GlobalContext,
    permissions: PermissionManager,
    session_usage: SessionUsage,
    renderer: Renderer,
    stream: bool,
    next_id: u64,
}

impl Dispatcher {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        config: Config,
        model: String,
        system_prompt: String,
        conv_log: ConversationLog,
        initial_messages: Vec<ChatMessage>,
        stream: bool,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        let global_ctx = GlobalContext::new();

        let root_id = AgentId(0);
        let agent_config = AgentConfig {
            model: model.clone(),
            system_prompt: system_prompt.clone(),
            max_tokens: 8192,
        };
        let root_agent = Agent::new_root(agent_config, conv_log, initial_messages);

        let root_registry = ToolRegistry::register_defaults_with_context(
            &config.working_dir,
            &global_ctx,
        );

        let mut agents = HashMap::new();
        agents.insert(root_id, root_agent);

        let mut registries = HashMap::new();
        registries.insert(root_id, root_registry);

        Self {
            agents,
            registries,
            subagent_info: HashMap::new(),
            pending_ask_user: None,
            event_tx,
            event_rx,
            provider,
            config,
            model,
            system_prompt,
            global_ctx,
            permissions: PermissionManager::new(),
            session_usage: SessionUsage::default(),
            renderer: Renderer::new(),
            stream,
            next_id: 1,
        }
    }

    fn root_id(&self) -> AgentId {
        AgentId(0)
    }

    fn next_agent_id(&mut self) -> AgentId {
        let id = AgentId(self.next_id);
        self.next_id += 1;
        id
    }

    pub async fn run(&mut self) -> Result<()> {
        println!("\x1b[1;36mQuine\x1b[0m - Self-bootstrapping CLI assistant");
        println!(
            "\x1b[90mProvider: {} | Model: {}\x1b[0m",
            self.config.provider, self.model
        );
        println!("\x1b[90mType your message, /help for commands, Ctrl+D to exit\x1b[0m\n");

        self.spawn_input_reader();

        while let Some(event) = self.event_rx.recv().await {
            match event {
                Event::UserInput(text) => {
                    if self.handle_user_input(text).await? {
                        break;
                    }
                }
                Event::LlmResponse { agent_id, response } => {
                    self.handle_llm_response(agent_id, response).await?;
                }
                Event::StreamChunk { agent_id, event } => {
                    self.handle_stream_chunk(agent_id, event).await?;
                }
                Event::Shutdown => break,
            }
        }

        println!("\n\x1b[90mGoodbye!\x1b[0m");
        Ok(())
    }

    /// Run a single non-interactive prompt (--print mode).
    pub async fn run_oneshot(
        &mut self,
        prompt: &str,
        output_format: &str,
    ) -> Result<()> {
        let root_id = self.root_id();

        {
            let agent = self.agents.get_mut(&root_id).unwrap();
            agent.record_user_input(prompt)?;
        }

        // Run the agent loop synchronously (no stdin reader)
        loop {
            let response = {
                let agent = self.agents.get(&root_id).unwrap();
                let registry = self.registries.get(&root_id).unwrap();
                let tool_schemas = registry.all_schemas();
                let request = agent.build_request(&tool_schemas);
                let spinner = crate::render::Spinner::start("Thinking...");
                let resp = self.provider.complete(request).await?;
                spinner.stop();
                resp
            };

            {
                let agent = self.agents.get_mut(&root_id).unwrap();
                agent.record_llm_response(&response)?;
            }

            if response.tool_calls.is_empty() {
                break;
            }

            // Execute tools sequentially
            let tool_calls = response.tool_calls.clone();
            for tc in &tool_calls {
                let result = {
                    let registry = self.registries.get(&root_id).unwrap();
                    match registry.get(&tc.name) {
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
                    }
                };

                let agent = self.agents.get_mut(&root_id).unwrap();
                agent.record_tool_result(
                    tc.id.clone(),
                    tc.name.clone(),
                    tc.arguments.clone(),
                    result,
                );
            }

            let agent = self.agents.get_mut(&root_id).unwrap();
            agent.finalize_tool_results()?;
        }

        // Save log and output
        let agent = self.agents.get(&root_id).unwrap();
        if let Some(log) = &agent.conv_log {
            let log_path = self.config.log_dir.join(format!(
                "{}.json",
                log.created_at.format("%Y%m%d_%H%M%S")
            ));
            log.save(&log_path)?;

            match output_format {
                "json" => {
                    let output = serde_json::json!({
                        "log_file": log_path.to_string_lossy(),
                        "entries": log.entries,
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => {
                    for entry in log.entries.iter().rev() {
                        if let quine_core::conversation::Entry::AssistantMessage {
                            content, ..
                        } = entry
                        {
                            if !content.is_empty() {
                                println!("{}", content);
                                break;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn spawn_input_reader(&self) {
        let tx = self.event_tx.clone();
        tokio::task::spawn_blocking(move || {
            let mut rl = rustyline::DefaultEditor::new().unwrap();
            loop {
                match rl.readline("\x1b[1;32m❯\x1b[0m ") {
                    Ok(line) => {
                        let trimmed = line.trim_end().to_string();
                        if !trimmed.is_empty() {
                            let _ = rl.add_history_entry(&trimmed);
                        }
                        if tx.blocking_send(Event::UserInput(trimmed)).is_err() {
                            break;
                        }
                    }
                    Err(rustyline::error::ReadlineError::Interrupted)
                    | Err(rustyline::error::ReadlineError::Eof) => {
                        let _ = tx.blocking_send(Event::Shutdown);
                        break;
                    }
                    Err(_) => {
                        let _ = tx.blocking_send(Event::Shutdown);
                        break;
                    }
                }
            }
        });
    }

    /// Handle user input. Returns true if the loop should exit.
    async fn handle_user_input(&mut self, text: String) -> Result<bool> {
        // If there's a pending AskUserQuestion, route the input there
        if let Some(pending) = self.pending_ask_user.take() {
            let result = ToolOutput {
                success: true,
                output: text,
            };
            self.handle_tool_result(
                pending.agent_id,
                pending.tool_call_id,
                pending.tool_name,
                pending.arguments,
                result,
            )
            .await?;
            return Ok(false);
        }

        if text.is_empty() {
            return Ok(false);
        }

        // Handle slash commands
        let root_id = self.root_id();
        let agent = self.agents.get_mut(&root_id).unwrap();
        if let Some(ref mut log) = agent.conv_log {
            if let Some(result) = commands::handle_command(
                &text,
                log,
                &mut agent.messages,
                &self.session_usage,
                &self.permissions,
            ) {
                match result {
                    CommandResult::Continue | CommandResult::Rewound => return Ok(false),
                    CommandResult::Exit => return Ok(true),
                    CommandResult::Unknown(cmd) => {
                        println!(
                            "\x1b[33mUnknown command: {}. Type /help for available commands.\x1b[0m",
                            cmd
                        );
                        return Ok(false);
                    }
                }
            }
        }

        // Record user input and spawn LLM call
        let agent = self.agents.get_mut(&root_id).unwrap();
        agent.record_user_input(&text)?;
        self.spawn_llm_call(root_id);

        Ok(false)
    }

    async fn handle_llm_response(
        &mut self,
        agent_id: AgentId,
        response: CompletionResponse,
    ) -> Result<()> {
        let has_tool_calls = !response.tool_calls.is_empty();

        // Print content if non-streaming
        if !self.stream && !response.content.is_empty() {
            self.renderer.print_assistant_message(&response.content);
        }

        {
            let agent = self.agents.get_mut(&agent_id).unwrap();
            agent.record_llm_response(&response)?;
        }

        if has_tool_calls {
            self.dispatch_next_tool(agent_id).await?;
        } else {
            self.on_turn_complete(agent_id).await?;
        }

        Ok(())
    }

    async fn handle_stream_chunk(
        &mut self,
        agent_id: AgentId,
        event: StreamEvent,
    ) -> Result<()> {
        // Print content deltas immediately
        if let StreamEvent::ContentDelta(ref text) = event {
            self.renderer.print_delta(text);
        }

        let agent = self.agents.get_mut(&agent_id).unwrap();
        if let Some(response) = agent.apply_stream_event(event) {
            println!(); // Newline after streaming
            // Re-enter as LLM response
            // We need to record it manually since apply_stream_event doesn't record
            let has_tool_calls = !response.tool_calls.is_empty();
            agent.record_llm_response(&response)?;

            if has_tool_calls {
                self.dispatch_next_tool(agent_id).await?;
            } else {
                self.on_turn_complete(agent_id).await?;
            }
        }

        Ok(())
    }

    /// Record a tool result and continue executing remaining tools or finalize.
    fn record_and_check_tools(
        &mut self,
        agent_id: AgentId,
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
        result: ToolOutput,
    ) -> Result<bool> {
        self.renderer.print_tool_result(result.success, &result.output);

        let all_done = {
            let agent = self.agents.get_mut(&agent_id).unwrap();
            agent.record_tool_result(tool_call_id, tool_name, arguments, result)
        };

        if all_done {
            let agent = self.agents.get_mut(&agent_id).unwrap();
            agent.finalize_tool_results()?;
            self.spawn_llm_call(agent_id);
        }
        Ok(all_done)
    }

    async fn handle_tool_result(
        &mut self,
        agent_id: AgentId,
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
        result: ToolOutput,
    ) -> Result<()> {
        let all_done =
            self.record_and_check_tools(agent_id, tool_call_id, tool_name, arguments, result)?;

        if !all_done {
            self.dispatch_next_tool(agent_id).await?;
        }

        Ok(())
    }

    async fn handle_subagent_done(
        &mut self,
        parent_id: AgentId,
        tool_call_id: String,
        result: Result<String, String>,
    ) -> Result<()> {
        let output = match result {
            Ok(text) => ToolOutput {
                success: true,
                output: text,
            },
            Err(e) => ToolOutput {
                success: false,
                output: format!("Subagent error: {}", e),
            },
        };

        let arguments = serde_json::json!({"prompt": "subagent task"});
        self.handle_tool_result(
            parent_id,
            tool_call_id,
            "Subagent".to_string(),
            arguments,
            output,
        )
        .await
    }

    /// Execute tools sequentially in a loop (avoids async recursion).
    async fn dispatch_next_tool(&mut self, agent_id: AgentId) -> Result<()> {
        loop {
            let tc = {
                let agent = self.agents.get_mut(&agent_id).unwrap();
                agent.next_tool_call()
            };

            let tc = match tc {
                Some(tc) => tc,
                None => return Ok(()),
            };

            self.renderer.print_tool_call(&tc.name, &tc.arguments);

            // Handle special tools that the dispatcher intercepts
            if tc.name == "Subagent" {
                return self.dispatch_subagent(agent_id, tc).await;
            }

            if tc.name == "AskUserQuestion" {
                return self.dispatch_ask_user(agent_id, tc);
            }

            // Permission check
            let context = format!("({})", summarize_tool_args(&tc.name, &tc.arguments));
            if !self.permissions.check(&tc.name, &context) {
                let result = ToolOutput {
                    success: false,
                    output: "Permission denied by user.".to_string(),
                };
                let all_done = self.record_and_check_tools(
                    agent_id,
                    tc.id,
                    tc.name,
                    tc.arguments,
                    result,
                )?;
                if all_done {
                    return Ok(());
                }
                continue;
            }

            // Execute tool directly
            let tool_call_id = tc.id.clone();
            let tool_name = tc.name.clone();
            let arguments = tc.arguments.clone();

            let result = {
                let registry = self.registries.get(&agent_id).unwrap();
                match registry.get(&tc.name) {
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
                }
            };

            let all_done =
                self.record_and_check_tools(agent_id, tool_call_id, tool_name, arguments, result)?;
            if all_done {
                return Ok(());
            }
        }
    }

    async fn dispatch_subagent(
        &mut self,
        parent_id: AgentId,
        tc: quine_core::conversation::ToolCall,
    ) -> Result<()> {
        let prompt = tc.arguments["prompt"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let use_worktree = tc.arguments["worktree"].as_bool().unwrap_or(false);

        let sub_id = self.next_agent_id();
        let sub_config = AgentConfig {
            model: self.model.clone(),
            system_prompt: self.system_prompt.clone(),
            max_tokens: 8192,
        };

        let worktree = if use_worktree {
            Some(Worktree::create(&self.config.working_dir)?)
        } else {
            None
        };
        let effective_dir = worktree
            .as_ref()
            .map(|w| w.path.as_path())
            .unwrap_or(&self.config.working_dir);

        let sub_registry =
            ToolRegistry::register_defaults_with_context(effective_dir, &self.global_ctx);

        let sub_agent = Agent::new_subagent(sub_config, prompt);

        self.agents.insert(sub_id, sub_agent);
        self.registries.insert(sub_id, sub_registry);
        self.subagent_info.insert(
            sub_id,
            SubagentInfo {
                parent_id,
                tool_call_id: tc.id,
                _worktree: worktree,
            },
        );

        self.spawn_llm_call(sub_id);
        Ok(())
    }

    fn dispatch_ask_user(
        &mut self,
        agent_id: AgentId,
        tc: quine_core::conversation::ToolCall,
    ) -> Result<()> {
        let question = tc.arguments["question"]
            .as_str()
            .unwrap_or("?");
        println!("\n\x1b[1;33m? {}\x1b[0m", question);

        self.pending_ask_user = Some(PendingAskUser {
            agent_id,
            tool_call_id: tc.id,
            tool_name: tc.name,
            arguments: tc.arguments,
        });

        Ok(())
    }

    fn spawn_llm_call(&self, agent_id: AgentId) {
        let agent = self.agents.get(&agent_id).unwrap();
        let registry = self.registries.get(&agent_id).unwrap();
        let tool_schemas = registry.all_schemas();
        let request = agent.build_request(&tool_schemas);

        let provider = Arc::clone(&self.provider);
        let tx = self.event_tx.clone();
        let stream = self.stream && agent.is_root; // Only stream for root agent

        tokio::spawn(async move {
            if stream {
                match provider.complete_stream(request).await {
                    Ok(mut event_stream) => {
                        while let Some(event_result) = event_stream.next().await {
                            match event_result {
                                Ok(event) => {
                                    if tx
                                        .send(Event::StreamChunk {
                                            agent_id,
                                            event,
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("\x1b[31mStream error: {}\x1b[0m", e);
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("\x1b[31mLLM error: {}\x1b[0m", e);
                    }
                }
            } else {
                match provider.complete(request).await {
                    Ok(response) => {
                        let _ = tx
                            .send(Event::LlmResponse {
                                agent_id,
                                response,
                            })
                            .await;
                    }
                    Err(e) => {
                        eprintln!("\x1b[31mLLM error: {}\x1b[0m", e);
                    }
                }
            }
        });
    }

    async fn on_turn_complete(&mut self, agent_id: AgentId) -> Result<()> {
        if agent_id == self.root_id() {
            // Root agent: print usage and save log
            let agent = self.agents.get(&agent_id).unwrap();
            let tu = &agent.turn_usage;
            self.session_usage.add(
                tu.input_tokens,
                tu.output_tokens,
                tu.cache_creation_input_tokens,
                tu.cache_read_input_tokens,
            );
            if tu.input_tokens > 0 || tu.output_tokens > 0 {
                commands::print_turn_usage(
                    tu.input_tokens,
                    tu.output_tokens,
                    &self.session_usage,
                );
            }

            if let Some(log) = &agent.conv_log {
                let log_path = self.config.log_dir.join(format!(
                    "{}.json",
                    log.created_at.format("%Y%m%d_%H%M%S")
                ));
                log.save(&log_path)?;
            }
        } else {
            // Subagent: notify parent
            if let Some(info) = self.subagent_info.remove(&agent_id) {
                let final_text = if let Some(agent) = self.agents.get(&agent_id) {
                    if let AgentState::Done { ref final_text } = agent.state {
                        final_text.clone()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Clean up subagent
                self.agents.remove(&agent_id);
                self.registries.remove(&agent_id);

                self.handle_subagent_done(
                    info.parent_id,
                    info.tool_call_id,
                    Ok(final_text),
                )
                .await?;
            }
        }

        Ok(())
    }
}

/// Summarize tool arguments for permission prompt context.
fn summarize_tool_args(tool_name: &str, args: &serde_json::Value) -> String {
    match tool_name {
        "Bash" => args["command"]
            .as_str()
            .map(|s| {
                if s.len() > 60 {
                    format!("{}...", &s[..60])
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
        "Write" | "Edit" => args["file_path"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    }
}
