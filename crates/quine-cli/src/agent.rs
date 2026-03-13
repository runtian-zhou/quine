use std::collections::VecDeque;

use quine_llm::types::*;
use serde_json::Value;

use quine_core::conversation::{Entry, ToolCall, ToolOutput};
use quine_core::log::ConversationLog;

/// Unique identifier for an agent instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentId(pub u64);

/// Per-tool-call result collected while executing a batch of tool calls.
#[derive(Debug, Clone)]
pub struct ToolResultEntry {
    pub tool_call_id: String,
    pub result: ToolOutput,
}

/// Explicit agent states.
pub enum AgentState {
    /// Waiting for user input (root agent only).
    WaitingUserInput,
    /// An LLM request is in flight.
    WaitingLlmResponse,
    /// Executing tool calls sequentially.
    ExecutingTools {
        pending: VecDeque<ToolCall>,
        results: Vec<ToolResultEntry>,
    },
    /// Paused during tool execution, waiting for user input to complete a tool
    /// (e.g. AskUserQuestion). The current tool call info is stored here so the
    /// dispatcher knows which tool to resume when input arrives.
    WaitingToolInput {
        pending: VecDeque<ToolCall>,
        results: Vec<ToolResultEntry>,
        tool_call_id: String,
        tool_name: String,
        arguments: Value,
    },
    /// Agent has finished (subagents reach this state).
    Done {
        final_text: String,
    },
}

/// Accumulated token usage for the current turn.
#[derive(Debug, Clone, Default)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl TurnUsage {
    pub fn add_usage(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        self.cache_read_input_tokens += usage.cache_read_input_tokens;
    }
}

/// Configuration for a single agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
}

/// Accumulator that builds a `CompletionResponse` from streaming events.
#[derive(Debug, Clone, Default)]
pub struct StreamAccumulator {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    current_tool_id: String,
    current_tool_name: String,
    current_tool_args: String,
    pub stop_reason: Option<StopReason>,
    pub usage: Usage,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a stream event. Returns `Some(response)` when the stream is done.
    pub fn apply(&mut self, event: StreamEvent) -> Option<CompletionResponse> {
        match event {
            StreamEvent::ContentDelta(text) => {
                self.content.push_str(&text);
                None
            }
            StreamEvent::ToolCallStart { id, name } => {
                self.current_tool_id = id;
                self.current_tool_name = name;
                self.current_tool_args.clear();
                None
            }
            StreamEvent::ToolCallDelta(json) => {
                self.current_tool_args.push_str(&json);
                None
            }
            StreamEvent::ToolCallEnd => {
                if !self.current_tool_id.is_empty() {
                    let args: Value = serde_json::from_str(&self.current_tool_args)
                        .unwrap_or(Value::Null);
                    self.tool_calls.push(ToolCall {
                        id: std::mem::take(&mut self.current_tool_id),
                        name: std::mem::take(&mut self.current_tool_name),
                        arguments: args,
                    });
                    self.current_tool_args.clear();
                }
                None
            }
            StreamEvent::Usage(u) => {
                self.usage.input_tokens += u.input_tokens;
                self.usage.output_tokens += u.output_tokens;
                self.usage.cache_creation_input_tokens += u.cache_creation_input_tokens;
                self.usage.cache_read_input_tokens += u.cache_read_input_tokens;
                None
            }
            StreamEvent::Done(reason) => {
                self.stop_reason = Some(reason.clone());
                // Flush any remaining tool call
                if !self.current_tool_id.is_empty() {
                    let args: Value = serde_json::from_str(&self.current_tool_args)
                        .unwrap_or(Value::Null);
                    self.tool_calls.push(ToolCall {
                        id: std::mem::take(&mut self.current_tool_id),
                        name: std::mem::take(&mut self.current_tool_name),
                        arguments: args,
                    });
                    self.current_tool_args.clear();
                }
                Some(CompletionResponse {
                    content: std::mem::take(&mut self.content),
                    tool_calls: std::mem::take(&mut self.tool_calls),
                    stop_reason: reason,
                    usage: self.usage.clone(),
                })
            }
        }
    }
}

/// An agent in the event-driven system.
pub struct Agent {
    pub state: AgentState,
    pub messages: Vec<ChatMessage>,
    pub conv_log: Option<ConversationLog>,
    pub turn_usage: TurnUsage,
    pub config: AgentConfig,
    pub is_root: bool,
    pub stream_acc: Option<StreamAccumulator>,
}

impl Agent {
    pub fn new_root(
        config: AgentConfig,
        conv_log: ConversationLog,
        messages: Vec<ChatMessage>,
    ) -> Self {
        Self {
            state: AgentState::WaitingUserInput,
            messages,
            conv_log: Some(conv_log),
            turn_usage: TurnUsage::default(),
            config,
            is_root: true,
            stream_acc: None,
        }
    }

    pub fn new_subagent(config: AgentConfig, prompt: String) -> Self {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Text(prompt),
        }];
        Self {
            state: AgentState::WaitingLlmResponse,
            messages,
            conv_log: None,
            turn_usage: TurnUsage::default(),
            config,
            is_root: false,
            stream_acc: None,
        }
    }

    /// Record user input, transition WaitingUserInput → WaitingLlmResponse.
    pub fn record_user_input(&mut self, text: &str) -> anyhow::Result<()> {
        if !matches!(self.state, AgentState::WaitingUserInput) {
            anyhow::bail!("record_user_input called in invalid state");
        }

        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Text(text.to_string()),
        });
        if let Some(log) = &mut self.conv_log {
            log.push(Entry::UserMessage {
                content: text.to_string(),
            });
        }
        self.turn_usage = TurnUsage::default();
        self.state = AgentState::WaitingLlmResponse;
        Ok(())
    }

    /// Build an LLM completion request from current state.
    pub fn build_request(&self, tool_schemas: &[Value]) -> CompletionRequest {
        CompletionRequest {
            model: self.config.model.clone(),
            system: self.config.system_prompt.clone(),
            messages: self.messages.clone(),
            tools: tool_schemas.to_vec(),
            max_tokens: self.config.max_tokens,
        }
    }

    /// Record an LLM response. Transitions to ExecutingTools or WaitingUserInput/Done.
    pub fn record_llm_response(&mut self, response: &CompletionResponse) -> anyhow::Result<()> {
        if !matches!(self.state, AgentState::WaitingLlmResponse) {
            anyhow::bail!("record_llm_response called in invalid state");
        }

        self.turn_usage.add_usage(&response.usage);

        // Log assistant message
        let tool_calls_for_log: Vec<ToolCall> = response
            .tool_calls
            .iter()
            .map(|tc| ToolCall {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
            })
            .collect();
        if let Some(log) = &mut self.conv_log {
            log.push(Entry::AssistantMessage {
                content: response.content.clone(),
                tool_calls: tool_calls_for_log.clone(),
            });
        }

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
        self.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: if assistant_blocks.len() == 1
                && matches!(&assistant_blocks[0], ContentBlock::Text { .. })
            {
                ChatContent::Text(response.content.clone())
            } else {
                ChatContent::Blocks(assistant_blocks)
            },
        });

        // Transition based on whether there are tool calls
        if response.tool_calls.is_empty() {
            if self.is_root {
                self.state = AgentState::WaitingUserInput;
            } else {
                self.state = AgentState::Done {
                    final_text: response.content.clone(),
                };
            }
        } else {
            self.state = AgentState::ExecutingTools {
                pending: VecDeque::from(tool_calls_for_log),
                results: Vec::new(),
            };
        }

        Ok(())
    }

    /// Pop the next tool call from the pending queue.
    pub fn next_tool_call(&mut self) -> Option<ToolCall> {
        if let AgentState::ExecutingTools { pending, .. } = &mut self.state {
            pending.pop_front()
        } else {
            None
        }
    }

    /// Transition from ExecutingTools → WaitingToolInput.
    /// Called when a tool (e.g. AskUserQuestion) needs user input to complete.
    /// The popped tool call info is stored in the state so the dispatcher can
    /// resume the right tool when user input arrives.
    pub fn enter_waiting_tool_input(&mut self, tc: ToolCall) -> anyhow::Result<()> {
        let (pending, results) =
            if let AgentState::ExecutingTools { pending, results } = &mut self.state {
                (std::mem::take(pending), std::mem::take(results))
            } else {
                anyhow::bail!("enter_waiting_tool_input called in invalid state");
            };

        self.state = AgentState::WaitingToolInput {
            pending,
            results,
            tool_call_id: tc.id,
            tool_name: tc.name,
            arguments: tc.arguments,
        };
        Ok(())
    }

    /// Transition from WaitingToolInput → ExecutingTools by providing the user's
    /// response as the tool result. Returns true if all tools are now done.
    pub fn resolve_tool_input(&mut self, user_response: ToolOutput) -> anyhow::Result<bool> {
        let (pending, mut results, tool_call_id, tool_name, arguments) =
            if let AgentState::WaitingToolInput {
                pending,
                results,
                tool_call_id,
                tool_name,
                arguments,
            } = &mut self.state
            {
                (
                    std::mem::take(pending),
                    std::mem::take(results),
                    std::mem::take(tool_call_id),
                    std::mem::take(tool_name),
                    std::mem::take(arguments),
                )
            } else {
                anyhow::bail!("resolve_tool_input called in invalid state");
            };

        // Log tool execution
        if let Some(log) = &mut self.conv_log {
            log.push(Entry::ToolExecution {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                arguments: arguments.clone(),
                result: user_response.clone(),
            });
        }
        results.push(ToolResultEntry {
            tool_call_id,
            result: user_response,
        });

        let all_done = pending.is_empty();
        self.state = AgentState::ExecutingTools { pending, results };
        Ok(all_done)
    }

    /// Record a tool result. Returns true if all tools are done.
    pub fn record_tool_result(
        &mut self,
        tool_call_id: String,
        tool_name: String,
        arguments: Value,
        result: ToolOutput,
    ) -> bool {
        if let AgentState::ExecutingTools {
            pending, results, ..
        } = &mut self.state
        {
            // Log tool execution
            if let Some(log) = &mut self.conv_log {
                log.push(Entry::ToolExecution {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments: arguments.clone(),
                    result: result.clone(),
                });
            }
            results.push(ToolResultEntry {
                tool_call_id,
                result,
            });
            pending.is_empty()
        } else {
            false
        }
    }

    /// Finalize tool results: add the tool result message and transition to WaitingLlmResponse.
    pub fn finalize_tool_results(&mut self) -> anyhow::Result<()> {
        let results = if let AgentState::ExecutingTools { results, .. } = &mut self.state {
            std::mem::take(results)
        } else {
            anyhow::bail!("finalize_tool_results called in invalid state");
        };

        let tool_result_blocks: Vec<ContentBlock> = results
            .into_iter()
            .map(|r| ContentBlock::ToolResult {
                tool_use_id: r.tool_call_id,
                content: r.result.output,
            })
            .collect();

        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Blocks(tool_result_blocks),
        });

        self.state = AgentState::WaitingLlmResponse;
        Ok(())
    }

    /// Check if this agent is currently waiting for tool input from the user.
    pub fn is_waiting_tool_input(&self) -> bool {
        matches!(self.state, AgentState::WaitingToolInput { .. })
    }

    /// Apply a streaming event. Returns `Some(response)` when stream is done.
    pub fn apply_stream_event(&mut self, event: StreamEvent) -> Option<CompletionResponse> {
        let acc = self.stream_acc.get_or_insert_with(StreamAccumulator::new);
        let result = acc.apply(event);
        if result.is_some() {
            self.stream_acc = None;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> AgentConfig {
        AgentConfig {
            model: "test-model".to_string(),
            system_prompt: "test-system".to_string(),
            max_tokens: 4096,
        }
    }

    fn make_conv_log() -> ConversationLog {
        ConversationLog::new(
            "test-model".to_string(),
            "test-provider".to_string(),
            "test-system".to_string(),
        )
    }

    #[test]
    fn record_user_input_transitions_to_waiting_llm() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        assert!(
            matches!(agent.state, AgentState::WaitingUserInput),
            "initial state should be WaitingUserInput"
        );

        agent.record_user_input("hello").unwrap();
        assert!(
            matches!(agent.state, AgentState::WaitingLlmResponse),
            "state should transition to WaitingLlmResponse after user input"
        );
        assert_eq!(agent.messages.len(), 1, "should have 1 message after user input");
        assert_eq!(
            agent.messages[0].content.as_text(),
            "hello",
            "message content should be 'hello'"
        );
    }

    #[test]
    fn record_user_input_invalid_state_returns_error() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::WaitingLlmResponse;
        let result = agent.record_user_input("hello");
        assert!(result.is_err(), "should error when called in WaitingLlmResponse state");
    }

    #[test]
    fn record_llm_response_no_tools_root_goes_to_waiting_user() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::WaitingLlmResponse;
        let response = CompletionResponse {
            content: "Hello back!".to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        };
        agent.record_llm_response(&response).unwrap();
        assert!(
            matches!(agent.state, AgentState::WaitingUserInput),
            "root agent with no tools should go to WaitingUserInput"
        );
    }

    #[test]
    fn record_llm_response_no_tools_subagent_goes_to_done() {
        let mut agent = Agent::new_subagent(
            make_config(),
            "do something".to_string(),
        );
        let response = CompletionResponse {
            content: "Result".to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        };
        agent.record_llm_response(&response).unwrap();
        assert!(
            matches!(agent.state, AgentState::Done { ref final_text } if final_text == "Result"),
            "subagent with no tools should go to Done with final_text='Result'"
        );
    }

    #[test]
    fn record_llm_response_with_tools_goes_to_executing() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::WaitingLlmResponse;
        let response = CompletionResponse {
            content: "".to_string(),
            tool_calls: vec![
                ToolCall {
                    id: "tc1".to_string(),
                    name: "Read".to_string(),
                    arguments: serde_json::json!({"file_path": "test.txt"}),
                },
                ToolCall {
                    id: "tc2".to_string(),
                    name: "Glob".to_string(),
                    arguments: serde_json::json!({"pattern": "*.rs"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        };
        agent.record_llm_response(&response).unwrap();
        assert!(
            matches!(agent.state, AgentState::ExecutingTools { ref pending, .. } if pending.len() == 2),
            "should go to ExecutingTools with 2 pending tool calls"
        );
    }

    #[test]
    fn next_tool_call_pops_from_pending() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::ExecutingTools {
            pending: VecDeque::from(vec![
                ToolCall {
                    id: "tc1".to_string(),
                    name: "Read".to_string(),
                    arguments: serde_json::json!({}),
                },
                ToolCall {
                    id: "tc2".to_string(),
                    name: "Glob".to_string(),
                    arguments: serde_json::json!({}),
                },
            ]),
            results: vec![],
        };

        let first = agent.next_tool_call();
        assert_eq!(first.as_ref().map(|tc| tc.id.as_str()), Some("tc1"), "first pop should be tc1");

        let second = agent.next_tool_call();
        assert_eq!(second.as_ref().map(|tc| tc.id.as_str()), Some("tc2"), "second pop should be tc2");

        let third = agent.next_tool_call();
        assert!(third.is_none(), "third pop should be None");
    }

    #[test]
    fn record_tool_result_returns_true_when_all_done() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::ExecutingTools {
            pending: VecDeque::new(),
            results: vec![],
        };

        let all_done = agent.record_tool_result(
            "tc1".to_string(),
            "Read".to_string(),
            serde_json::json!({}),
            ToolOutput {
                success: true,
                output: "content".to_string(),
            },
        );
        assert!(all_done, "should return true when pending is empty");
    }

    #[test]
    fn record_tool_result_returns_false_when_pending_remain() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::ExecutingTools {
            pending: VecDeque::from(vec![ToolCall {
                id: "tc2".to_string(),
                name: "Glob".to_string(),
                arguments: serde_json::json!({}),
            }]),
            results: vec![],
        };

        let all_done = agent.record_tool_result(
            "tc1".to_string(),
            "Read".to_string(),
            serde_json::json!({}),
            ToolOutput {
                success: true,
                output: "content".to_string(),
            },
        );
        assert!(!all_done, "should return false when pending tools remain");
    }

    #[test]
    fn finalize_tool_results_transitions_to_waiting_llm() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::ExecutingTools {
            pending: VecDeque::new(),
            results: vec![ToolResultEntry {
                tool_call_id: "tc1".to_string(),
                result: ToolOutput {
                    success: true,
                    output: "file content".to_string(),
                },
            }],
        };

        agent.finalize_tool_results().unwrap();
        assert!(
            matches!(agent.state, AgentState::WaitingLlmResponse),
            "should transition to WaitingLlmResponse after finalizing tool results"
        );
        // The last message should be a user message with tool results
        let last = agent.messages.last().unwrap();
        assert_eq!(last.role, "user", "last message should be from user (tool results)");
    }

    #[test]
    fn stream_accumulator_basic_flow() {
        let mut acc = StreamAccumulator::new();

        assert!(acc.apply(StreamEvent::ContentDelta("Hello ".to_string())).is_none());
        assert!(acc.apply(StreamEvent::ContentDelta("world".to_string())).is_none());
        assert!(acc.apply(StreamEvent::Usage(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        })).is_none());

        let response = acc.apply(StreamEvent::Done(StopReason::EndTurn));
        assert!(response.is_some(), "should return response on Done");
        let response = response.unwrap();
        assert_eq!(response.content, "Hello world", "content should be 'Hello world'");
        assert_eq!(response.tool_calls.len(), 0, "should have 0 tool calls");
        assert_eq!(response.usage.input_tokens, 10, "input_tokens should be 10");
        assert_eq!(response.usage.output_tokens, 5, "output_tokens should be 5");
    }

    #[test]
    fn stream_accumulator_with_tool_calls() {
        let mut acc = StreamAccumulator::new();

        assert!(acc.apply(StreamEvent::ToolCallStart {
            id: "tc1".to_string(),
            name: "Read".to_string(),
        }).is_none());
        assert!(acc.apply(StreamEvent::ToolCallDelta("{\"file".to_string())).is_none());
        assert!(acc.apply(StreamEvent::ToolCallDelta("_path\":\"test.txt\"}".to_string())).is_none());
        assert!(acc.apply(StreamEvent::ToolCallEnd).is_none());

        let response = acc.apply(StreamEvent::Done(StopReason::ToolUse)).unwrap();
        assert_eq!(response.tool_calls.len(), 1, "should have 1 tool call");
        assert_eq!(response.tool_calls[0].name, "Read", "tool name should be 'Read'");
        assert_eq!(
            response.tool_calls[0].arguments["file_path"].as_str(),
            Some("test.txt"),
            "tool argument file_path should be 'test.txt'"
        );
    }

    #[test]
    fn apply_stream_event_on_agent_clears_accumulator_on_done() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::WaitingLlmResponse;

        assert!(agent.apply_stream_event(StreamEvent::ContentDelta("Hi".to_string())).is_none());
        assert!(agent.stream_acc.is_some(), "accumulator should exist during streaming");

        let response = agent.apply_stream_event(StreamEvent::Done(StopReason::EndTurn));
        assert!(response.is_some(), "should return response on Done");
        assert!(agent.stream_acc.is_none(), "accumulator should be cleared after Done");
    }

    #[test]
    fn enter_waiting_tool_input_transitions_from_executing() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::ExecutingTools {
            pending: VecDeque::from(vec![ToolCall {
                id: "tc2".to_string(),
                name: "Glob".to_string(),
                arguments: serde_json::json!({}),
            }]),
            results: vec![ToolResultEntry {
                tool_call_id: "tc0".to_string(),
                result: ToolOutput { success: true, output: "done".to_string() },
            }],
        };

        let ask_tc = ToolCall {
            id: "tc1".to_string(),
            name: "AskUserQuestion".to_string(),
            arguments: serde_json::json!({"question": "What file?"}),
        };
        agent.enter_waiting_tool_input(ask_tc).unwrap();

        assert!(
            agent.is_waiting_tool_input(),
            "agent should be in WaitingToolInput state"
        );
        if let AgentState::WaitingToolInput {
            ref pending,
            ref results,
            ref tool_call_id,
            ref tool_name,
            ..
        } = agent.state
        {
            assert_eq!(pending.len(), 1, "pending should carry over the remaining tool call");
            assert_eq!(results.len(), 1, "results should carry over previous results");
            assert_eq!(tool_call_id, "tc1", "tool_call_id should be the ask tool");
            assert_eq!(tool_name, "AskUserQuestion", "tool_name should be AskUserQuestion");
        } else {
            panic!("expected WaitingToolInput state");
        }
    }

    #[test]
    fn enter_waiting_tool_input_invalid_state_returns_error() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        // WaitingUserInput is not a valid state for this transition
        let tc = ToolCall {
            id: "tc1".to_string(),
            name: "AskUserQuestion".to_string(),
            arguments: serde_json::json!({}),
        };
        let result = agent.enter_waiting_tool_input(tc);
        assert!(result.is_err(), "should error when called in WaitingUserInput state");
    }

    #[test]
    fn resolve_tool_input_transitions_back_to_executing() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::WaitingToolInput {
            pending: VecDeque::from(vec![ToolCall {
                id: "tc2".to_string(),
                name: "Glob".to_string(),
                arguments: serde_json::json!({}),
            }]),
            results: vec![],
            tool_call_id: "tc1".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            arguments: serde_json::json!({"question": "What file?"}),
        };

        let user_response = ToolOutput {
            success: true,
            output: "test.rs".to_string(),
        };
        let all_done = agent.resolve_tool_input(user_response).unwrap();
        assert!(!all_done, "should not be all done when pending tools remain");
        assert!(
            matches!(agent.state, AgentState::ExecutingTools { ref pending, ref results, .. }
                if pending.len() == 1 && results.len() == 1),
            "should transition back to ExecutingTools with 1 pending and 1 result"
        );
    }

    #[test]
    fn resolve_tool_input_all_done_when_no_pending() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::WaitingToolInput {
            pending: VecDeque::new(),
            results: vec![],
            tool_call_id: "tc1".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            arguments: serde_json::json!({"question": "What file?"}),
        };

        let user_response = ToolOutput {
            success: true,
            output: "test.rs".to_string(),
        };
        let all_done = agent.resolve_tool_input(user_response).unwrap();
        assert!(all_done, "should be all done when no pending tools remain");
    }

    #[test]
    fn resolve_tool_input_invalid_state_returns_error() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        // WaitingUserInput is not a valid state
        let result = agent.resolve_tool_input(ToolOutput {
            success: true,
            output: "test".to_string(),
        });
        assert!(result.is_err(), "should error when called in WaitingUserInput state");
    }

    #[test]
    fn resolve_tool_input_logs_tool_execution() {
        let mut agent = Agent::new_root(
            make_config(),
            make_conv_log(),
            vec![],
        );
        agent.state = AgentState::WaitingToolInput {
            pending: VecDeque::new(),
            results: vec![],
            tool_call_id: "tc1".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            arguments: serde_json::json!({"question": "What file?"}),
        };

        agent.resolve_tool_input(ToolOutput {
            success: true,
            output: "test.rs".to_string(),
        }).unwrap();

        let log = agent.conv_log.as_ref().unwrap();
        assert_eq!(log.entries.len(), 1, "should have logged one tool execution entry");
        if let Entry::ToolExecution { ref tool_name, ref result, .. } = log.entries[0] {
            assert_eq!(tool_name, "AskUserQuestion", "logged tool name should be AskUserQuestion");
            assert_eq!(result.output, "test.rs", "logged output should be 'test.rs'");
        } else {
            panic!("expected ToolExecution entry");
        }
    }
}
