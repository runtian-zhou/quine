use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::sync::Arc;

use reqwest::Client;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::client::IpcClient;
use crate::context_debug::{fetch_session_context, SessionContextSnapshot};
use crate::run::{self, InteractionNeededOutput, OneshotOutput, RunOneshotOptions};
use crate::session::{create_session, create_slash_skill_session};
use crate::slash_command::{parse_slash_command, SlashCommand};
use quine_harness::protocol::methods;

#[derive(Debug, Clone, Default)]
struct TelegramChatState {
    session_id: Option<String>,
    plan_mode: bool,
    model_profile: Option<String>,
    state_tab: SessionStateTab,
    session_picker_page: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SessionStateTab {
    #[default]
    Overview,
    History,
    Tools,
    Skills,
    Plans,
    Memory,
    Permissions,
}

#[derive(Debug, Clone)]
enum TelegramSlashAction {
    Quit,
    ShowSessions {
        tree: bool,
    },
    ShowState {
        tab: Option<SessionStateTab>,
    },
    Compact,
    Context,
    Clear,
    SetModelProfile {
        model_profile: String,
    },
    Switch {
        session_id: String,
    },
    SendPlan {
        request: String,
    },
    ScheduleLoop {
        request: String,
        delay_secs: u64,
        cadence_secs: Option<u64>,
    },
    StartSkill {
        skill_name: String,
        request: String,
    },
    SendMessage(String),
}

#[derive(Debug, Deserialize)]
struct TelegramPluginConfig {
    #[serde(default)]
    bot_token: Option<String>,
    #[serde(default = "default_bot_token_env")]
    bot_token_env: String,
    #[serde(default)]
    allowed_user_ids: Vec<i64>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    auto_approve: bool,
    #[serde(default)]
    model_profile: Option<String>,
}

fn default_bot_token_env() -> String {
    "TELEGRAM_BOT_TOKEN".to_string()
}

impl TelegramPluginConfig {
    fn load(config_path: Option<&Path>) -> anyhow::Result<(Self, PathBuf)> {
        let path = resolve_config_path(config_path)?;
        let content = std::fs::read_to_string(&path).map_err(|error| {
            anyhow::anyhow!(
                "failed to read Telegram plugin config at {}: {error}",
                path.display()
            )
        })?;
        let config: Self = toml::from_str(&content).map_err(|error| {
            anyhow::anyhow!(
                "failed to parse Telegram plugin config at {}: {error}",
                path.display()
            )
        })?;
        Ok((config, path))
    }

    fn bot_token(&self) -> anyhow::Result<String> {
        if let Some(token) = &self.bot_token {
            return Ok(token.clone());
        }

        std::env::var(&self.bot_token_env).map_err(|error| {
            anyhow::anyhow!(
                "missing Telegram bot token: set `bot_token` in the config or export {} ({error})",
                self.bot_token_env
            )
        })
    }

    fn allowed_users(&self) -> HashSet<i64> {
        self.allowed_user_ids.iter().copied().collect()
    }
}

impl SessionSummary {
    fn summary_text(&self) -> Option<&str> {
        self.summary
            .as_deref()
            .or(self.title.as_deref())
            .filter(|value| !value.trim().is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct SessionSummary {
    session_id: String,
    #[serde(default)]
    parent_id: Option<String>,
    status: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    plan_mode: bool,
}

#[derive(Debug, Deserialize)]
struct TelegramApiEnvelope<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
    #[serde(default)]
    callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    chat: TelegramChat,
    #[serde(default)]
    from: Option<TelegramUser>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramCallbackQuery {
    id: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    message: Option<TelegramCallbackMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramCallbackMessage {
    message_id: i64,
    chat: TelegramChat,
}

#[derive(Debug, Deserialize)]
struct TelegramSentMessage {
    message_id: i64,
}

#[derive(Clone)]
struct TelegramBot {
    client: Client,
    api_base: String,
    allowed_user_ids: HashSet<i64>,
    socket_path: PathBuf,
    skills: Vec<String>,
    auto_approve: bool,
    default_model_profile: Option<String>,
    chat_state: Arc<Mutex<HashMap<i64, TelegramChatState>>>,
}

impl TelegramBot {
    fn new(
        bot_token: String,
        allowed_user_ids: HashSet<i64>,
        socket_path: PathBuf,
        skills: Vec<String>,
        auto_approve: bool,
        model_profile: Option<String>,
    ) -> anyhow::Result<Self> {
        let client = Client::builder().build()?;
        Ok(Self {
            client,
            api_base: format!("https://api.telegram.org/bot{bot_token}"),
            allowed_user_ids,
            socket_path,
            skills,
            auto_approve,
            default_model_profile: model_profile,
            chat_state: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn validate_token(&self) -> anyhow::Result<()> {
        let url = format!("{}/getMe", self.api_base);
        let response = self.client.get(url).send().await?;
        let envelope: TelegramApiEnvelope<serde_json::Value> = response.json().await?;
        if envelope.ok {
            Ok(())
        } else {
            anyhow::bail!(
                "Telegram bot token validation failed: {}",
                envelope
                    .description
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
    }

    async fn connect_client(&self) -> anyhow::Result<IpcClient> {
        run::connect_existing_client(&self.socket_path).await
    }

    async fn current_session_id(&self, chat_id: i64) -> anyhow::Result<Option<String>> {
        Ok(self
            .chat_state
            .lock()
            .await
            .get(&chat_id)
            .and_then(|state| state.session_id.clone()))
    }

    async fn current_state_tab(&self, chat_id: i64) -> anyhow::Result<Option<SessionStateTab>> {
        Ok(self
            .chat_state
            .lock()
            .await
            .get(&chat_id)
            .map(|state| state.state_tab))
    }

    async fn current_session_picker_page(&self, chat_id: i64) -> anyhow::Result<Option<usize>> {
        Ok(self
            .chat_state
            .lock()
            .await
            .get(&chat_id)
            .map(|state| state.session_picker_page))
    }

    async fn set_session_picker_page(&self, chat_id: i64, page: usize) {
        if let Some(state) = self.chat_state.lock().await.get_mut(&chat_id) {
            state.session_picker_page = page;
        }
    }

    async fn resolve_status_session_id(&self, chat_id: i64) -> anyhow::Result<Option<String>> {
        if let Some(session_id) = self.current_session_id(chat_id).await? {
            return Ok(Some(session_id));
        }

        let mut client = self.connect_client().await?;
        let sessions = self.fetch_sessions(&mut client).await?;
        let Some(session) = sessions.first() else {
            return Ok(None);
        };
        let session_id = session.session_id.clone();
        eprintln!(
            "[telegram] resolve_status_session_id chat_id={chat_id} falling back to latest session_id={session_id}"
        );
        self.chat_state.lock().await.insert(
            chat_id,
            TelegramChatState {
                session_id: Some(session_id.clone()),
                plan_mode: session.plan_mode,
                model_profile: self.default_model_profile.clone(),
                state_tab: SessionStateTab::Overview,
                session_picker_page: 0,
            },
        );
        Ok(Some(session_id))
    }

    async fn send_reply(
        &self,
        chat_id: i64,
        reply_to_message_id: i64,
        text: &str,
        reply_markup: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        eprintln!(
            "[telegram] send_reply chat_id={chat_id} reply_to={reply_to_message_id} len={} has_markup={}",
            text.chars().count(),
            reply_markup.is_some()
        );
        for (index, chunk) in chunk_message(text, 3900).into_iter().enumerate() {
            let reply_target = if index == 0 {
                Some(reply_to_message_id)
            } else {
                None
            };
            let markup = if index == 0 {
                reply_markup.clone()
            } else {
                None
            };
            if self
                .send_message(chat_id, &chunk, reply_target, markup.clone())
                .await
                .is_err()
            {
                self.send_message(chat_id, &chunk, None, None).await?;
            }
        }
        Ok(())
    }

    async fn load_or_create_session(
        &self,
        chat_id: i64,
        plan_mode: bool,
    ) -> anyhow::Result<String> {
        if let Some(session_id) = self.current_session_id(chat_id).await? {
            return Ok(session_id);
        }

        let mut client = self.connect_client().await?;
        let sessions = self.fetch_sessions(&mut client).await?;
        if let Some(session) = sessions.first() {
            let session_id = session.session_id.clone();
            eprintln!(
                "[telegram] load_or_create_session chat_id={chat_id} reusing latest session_id={session_id}"
            );
            self.chat_state.lock().await.insert(
                chat_id,
                TelegramChatState {
                    session_id: Some(session_id.clone()),
                    plan_mode: session.plan_mode,
                    model_profile: self.default_model_profile.clone(),
                    state_tab: SessionStateTab::Overview,
                    session_picker_page: 0,
                },
            );
            return Ok(session_id);
        }

        let created = create_session(
            &mut client,
            &self.skills,
            plan_mode,
            self.default_model_profile.as_deref(),
        )
        .await?;
        self.chat_state.lock().await.insert(
            chat_id,
            TelegramChatState {
                session_id: Some(created.session_id.clone()),
                plan_mode,
                model_profile: self.default_model_profile.clone(),
                state_tab: SessionStateTab::Overview,
                session_picker_page: 0,
            },
        );
        Ok(created.session_id)
    }

    async fn send_chat_message(
        &self,
        chat_id: i64,
        reply_to_message_id: i64,
        content: String,
    ) -> anyhow::Result<()> {
        eprintln!(
            "[telegram] send_chat_message chat_id={chat_id} reply_to={reply_to_message_id} len={}",
            content.chars().count()
        );
        let session_id = self.load_or_create_session(chat_id, false).await?;
        let status_message_id = self
            .send_message(
                chat_id,
                "Quine is thinking...",
                Some(reply_to_message_id),
                None,
            )
            .await?;
        let output = match run::execute_oneshot(
            &self.socket_path,
            &content,
            RunOneshotOptions {
                session_id: Some(&session_id),
                resume_checkpoint: None,
                json_output: false,
                skills: &self.skills,
                auto_approve: self.auto_approve,
                model_profile: self.default_model_profile.as_deref(),
            },
            false,
        )
        .await
        {
            Ok(output) => output,
            Err(error) => {
                let message = format!("Quine error: {error}");
                if self
                    .edit_message(chat_id, status_message_id, &message, None)
                    .await
                    .is_err()
                {
                    let _ = self
                        .send_reply(chat_id, reply_to_message_id, &message, None)
                        .await;
                }
                return Ok(());
            }
        };
        let reply = format_reply(&output);
        match self.edit_message(chat_id, status_message_id, &reply, None).await {
            Ok(()) => Ok(()),
            Err(_) => self.send_reply(chat_id, reply_to_message_id, &reply, None).await,
        }
    }

    async fn send_plan_request(
        &self,
        chat_id: i64,
        reply_to_message_id: i64,
        request: String,
    ) -> anyhow::Result<()> {
        let mut client = self.connect_client().await?;
        let session_id = {
            let state = self.chat_state.lock().await.clone();
            match state.get(&chat_id) {
                Some(chat_state) if chat_state.plan_mode => {
                    chat_state.session_id.clone().unwrap_or_default()
                }
                _ => String::new(),
            }
        };
        let session_id = if session_id.is_empty() {
            let created = create_session(
                &mut client,
                &self.skills,
                true,
                self.default_model_profile.as_deref(),
            )
            .await?;
            self.chat_state.lock().await.insert(
                chat_id,
                TelegramChatState {
                    session_id: Some(created.session_id.clone()),
                    plan_mode: true,
                    model_profile: self.default_model_profile.clone(),
                    state_tab: SessionStateTab::Overview,
                    session_picker_page: 0,
                },
            );
            created.session_id
        } else {
            session_id
        };

        let output = run::execute_oneshot(
            &self.socket_path,
            &request,
            RunOneshotOptions {
                session_id: Some(&session_id),
                resume_checkpoint: None,
                json_output: false,
                skills: &self.skills,
                auto_approve: self.auto_approve,
                model_profile: self.default_model_profile.as_deref(),
            },
            false,
        )
        .await?;
        let reply = format_reply(&output);
        self.send_reply(
            chat_id,
            reply_to_message_id,
            &reply,
            Some(default_slash_reply_markup()),
        )
        .await
    }

    async fn send_skill_request(
        &self,
        chat_id: i64,
        reply_to_message_id: i64,
        skill_name: String,
        request: String,
    ) -> anyhow::Result<()> {
        let mut client = self.connect_client().await?;
        let available_skills = run::fetch_available_skills(&mut client)
            .await
            .unwrap_or_default();
        if !available_skills
            .iter()
            .any(|candidate| candidate == &skill_name)
        {
            self.send_reply(
                chat_id,
                reply_to_message_id,
                &format!("Unknown slash command: /{skill_name}"),
                Some(default_slash_reply_markup()),
            )
            .await?;
            return Ok(());
        }

        let created = create_slash_skill_session(&mut client, &skill_name, &request).await?;
        let output = run::execute_oneshot(
            &self.socket_path,
            &request,
            RunOneshotOptions {
                session_id: Some(&created.session_id),
                resume_checkpoint: None,
                json_output: false,
                skills: &self.skills,
                auto_approve: self.auto_approve,
                model_profile: self.default_model_profile.as_deref(),
            },
            false,
        )
        .await?;
        self.chat_state.lock().await.insert(
            chat_id,
            TelegramChatState {
                session_id: Some(created.session_id),
                plan_mode: false,
                model_profile: self.default_model_profile.clone(),
                state_tab: SessionStateTab::Overview,
                session_picker_page: 0,
            },
        );
        let reply = format_reply(&output);
        self.send_reply(
            chat_id,
            reply_to_message_id,
            &reply,
            Some(default_slash_reply_markup()),
        )
        .await
    }

    async fn fetch_sessions(&self, client: &mut IpcClient) -> anyhow::Result<Vec<SessionSummary>> {
        let result = client.call(methods::LIST_SESSIONS, None).await?;
        let value = result.map_err(|message| anyhow::anyhow!(message))?;
        let sessions: Vec<SessionSummary> = serde_json::from_value(value)?;
        eprintln!("[telegram] fetch_sessions count={}", sessions.len());
        Ok(sessions)
    }

    fn parse_action(&self, input: &str) -> anyhow::Result<TelegramSlashAction> {
        if let Some(command) = parse_slash_command(input) {
            let action = match command {
                SlashCommand::BuiltIn { name, arguments } => match name.as_str() {
                    "quit" => TelegramSlashAction::Quit,
                    "ps" => TelegramSlashAction::ShowSessions {
                        tree: matches!(arguments.trim(), "tree"),
                    },
                    "state" => TelegramSlashAction::ShowState {
                        tab: parse_state_tab(arguments.as_str()),
                    },
                    "compact" => TelegramSlashAction::Compact,
                    "context" => TelegramSlashAction::Context,
                    "clear" => TelegramSlashAction::Clear,
                    "switch" => {
                        let target = arguments.trim();
                        if target.is_empty() {
                            return Ok(TelegramSlashAction::SendMessage(
                                "Usage: /switch <session-id>".to_string(),
                            ));
                        }
                        TelegramSlashAction::Switch {
                            session_id: target.to_string(),
                        }
                    }
                    "model" => {
                        let target = arguments.trim();
                        if target.is_empty() {
                            return Ok(TelegramSlashAction::SendMessage(
                                "Usage: /model <profile>".to_string(),
                            ));
                        }
                        TelegramSlashAction::SetModelProfile {
                            model_profile: target.to_string(),
                        }
                    }
                    "plan" => {
                        if arguments.trim().is_empty() {
                            return Ok(TelegramSlashAction::SendMessage(
                                "Usage: /plan <request>".to_string(),
                            ));
                        }
                        TelegramSlashAction::SendPlan { request: arguments }
                    }
                    "loop" => match parse_loop_arguments(&arguments) {
                        Ok((request, delay_secs, cadence_secs)) => {
                            TelegramSlashAction::ScheduleLoop {
                                request,
                                delay_secs: delay_secs.as_secs(),
                                cadence_secs: cadence_secs.map(|value| value.as_secs()),
                            }
                        }
                        Err(message) => TelegramSlashAction::SendMessage(message),
                    },
                    other => {
                        TelegramSlashAction::SendMessage(format!("Unknown slash command: /{other}"))
                    }
                },
                SlashCommand::Skill { name, arguments } => TelegramSlashAction::StartSkill {
                    skill_name: name,
                    request: arguments,
                },
            };
            Ok(action)
        } else {
            Ok(TelegramSlashAction::SendMessage(input.to_string()))
        }
    }

    async fn poll_updates(&self, offset: i64) -> anyhow::Result<Vec<TelegramUpdate>> {
        let url = format!("{}/getUpdates", self.api_base);
        let offset = offset.to_string();
        let response = self
            .client
            .get(url)
            .query(&[("timeout", "30"), ("offset", offset.as_str())])
            .send()
            .await?;
        let envelope: TelegramApiEnvelope<Vec<TelegramUpdate>> = response.json().await?;
        if envelope.ok {
            Ok(envelope.result.unwrap_or_default())
        } else {
            anyhow::bail!(
                "Telegram getUpdates failed: {}",
                envelope
                    .description
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
    }

    async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_to_message_id: Option<i64>,
        reply_markup: Option<serde_json::Value>,
    ) -> anyhow::Result<i64> {
        eprintln!(
            "[telegram] api sendMessage chat_id={chat_id} reply_to={reply_to_message_id:?} len={} has_markup={}",
            text.chars().count(),
            reply_markup.is_some()
        );
        let url = format!("{}/sendMessage", self.api_base);
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if let Some(reply_to_message_id) = reply_to_message_id {
            payload["reply_to_message_id"] = serde_json::json!(reply_to_message_id);
        }
        if let Some(reply_markup) = reply_markup {
            payload["reply_markup"] = reply_markup;
        }
        let response = self.client.post(url).json(&payload).send().await?;
        let envelope: TelegramApiEnvelope<TelegramSentMessage> = response.json().await?;
        if envelope.ok {
            envelope
                .result
                .map(|message| message.message_id)
                .ok_or_else(|| anyhow::anyhow!("Telegram sendMessage returned no message id"))
        } else {
            eprintln!(
                "[telegram] api sendMessage failed chat_id={chat_id} reply_to={reply_to_message_id:?}: {}",
                envelope
                    .description
                    .as_deref()
                    .unwrap_or("unknown error")
            );
            anyhow::bail!(
                "Telegram sendMessage failed: {}",
                envelope
                    .description
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
    }

    async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> anyhow::Result<()> {
        let url = format!("{}/answerCallbackQuery", self.api_base);
        let mut payload = serde_json::json!({
            "callback_query_id": callback_query_id,
        });
        if let Some(text) = text {
            payload["text"] = serde_json::json!(text);
            payload["show_alert"] = serde_json::json!(false);
        }
        let response = self.client.post(url).json(&payload).send().await?;
        let envelope: TelegramApiEnvelope<serde_json::Value> = response.json().await?;
        if envelope.ok {
            Ok(())
        } else {
            anyhow::bail!(
                "Telegram answerCallbackQuery failed: {}",
                envelope
                    .description
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
    }

    async fn edit_message(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        eprintln!(
            "[telegram] api editMessageText chat_id={chat_id} message_id={message_id} len={} has_markup={}",
            text.chars().count(),
            reply_markup.is_some()
        );
        let url = format!("{}/editMessageText", self.api_base);
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if let Some(reply_markup) = reply_markup {
            payload["reply_markup"] = reply_markup;
        }
        let response = self.client.post(url).json(&payload).send().await?;
        let envelope: TelegramApiEnvelope<serde_json::Value> = response.json().await?;
        if envelope.ok {
            Ok(())
        } else {
            eprintln!(
                "[telegram] api editMessageText failed chat_id={chat_id} message_id={message_id}: {}",
                envelope
                    .description
                    .as_deref()
                    .unwrap_or("unknown error")
            );
            anyhow::bail!(
                "Telegram editMessageText failed: {}",
                envelope
                    .description
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
    }

    async fn handle_command(
        &self,
        chat_id: i64,
        reply_to_message_id: i64,
        trimmed: &str,
        from_slash_command: bool,
        edit_in_place: bool,
    ) -> anyhow::Result<()> {
        let action = self.parse_action(trimmed)?;
        eprintln!(
            "[telegram] handle_command chat_id={chat_id} reply_to={reply_to_message_id} from_slash={from_slash_command} edit_in_place={edit_in_place} input={trimmed:?} action={action:?}"
        );
        match action {
            TelegramSlashAction::Quit => {
                self.chat_state.lock().await.remove(&chat_id);
                self.send_reply(
                    chat_id,
                    reply_to_message_id,
                    "Session closed.",
                    Some(default_slash_reply_markup()),
                )
                .await
            }
            TelegramSlashAction::ShowSessions { tree } => {
                let mut client = self.connect_client().await?;
                eprintln!("[telegram] /ps fetching sessions chat_id={chat_id} tree={tree}");
                let sessions = self.fetch_sessions(&mut client).await?;
                eprintln!(
                    "[telegram] /ps chat_id={chat_id} tree={tree} sessions={}",
                    sessions.len()
                );
                let (reply, markup) = if tree {
                    (
                        render_session_tree(&sessions),
                        session_list_reply_markup(tree, &sessions),
                    )
                } else {
                    let (reply, page, total_pages) = render_session_picker_page(
                        &sessions,
                        self.current_session_picker_page(chat_id).await?.unwrap_or(0),
                    );
                    self.set_session_picker_page(chat_id, page).await;
                    (
                        reply,
                        session_picker_reply_markup(&sessions, page, total_pages),
                    )
                };
                if edit_in_place {
                    match self
                        .edit_message(chat_id, reply_to_message_id, &reply, Some(markup.clone()))
                        .await
                    {
                        Ok(()) => Ok(()),
                        Err(_) => self
                            .send_reply(chat_id, reply_to_message_id, &reply, Some(markup))
                            .await,
                    }
                } else {
                    self.send_reply(chat_id, reply_to_message_id, &reply, Some(markup))
                        .await
                }
            }
            TelegramSlashAction::ShowState { tab } => {
                let tab = tab
                    .or(self.current_state_tab(chat_id).await?)
                    .unwrap_or_default();
                let mut client = self.connect_client().await?;
                let Some(session_id) = self.resolve_status_session_id(chat_id).await? else {
                    eprintln!("[telegram] /state chat_id={chat_id} no session available");
                    self.send_reply(
                        chat_id,
                        reply_to_message_id,
                        "No session found. Send a message first.",
                        Some(default_slash_reply_markup()),
                    )
                    .await?;
                    return Ok(());
                };
                eprintln!(
                    "[telegram] /state chat_id={chat_id} session_id={session_id} tab={tab:?}"
                );
                eprintln!(
                    "[telegram] /state fetching context chat_id={chat_id} session_id={session_id}"
                );
                {
                    let mut state = self.chat_state.lock().await;
                    let entry = state.entry(chat_id).or_default();
                    entry.session_id = Some(session_id.clone());
                    entry.session_picker_page = 0;
                }
                let snapshot = fetch_session_context(&mut client, &session_id).await?;
                eprintln!(
                    "[telegram] /state fetched context chat_id={chat_id} session_id={session_id} plan_mode={}",
                    snapshot.plan_mode
                );
                let session_picker_page = 0;
                self.chat_state.lock().await.insert(
                    chat_id,
                    TelegramChatState {
                        session_id: Some(session_id),
                        plan_mode: snapshot.plan_mode,
                        model_profile: self.default_model_profile.clone(),
                        state_tab: tab,
                        session_picker_page,
                    },
                );
                let reply = render_state_view(&snapshot, tab);
                let markup = state_view_reply_markup(tab);
                if edit_in_place {
                    match self
                        .edit_message(chat_id, reply_to_message_id, &reply, Some(markup.clone()))
                        .await
                    {
                        Ok(()) => Ok(()),
                        Err(_) => self
                            .send_reply(chat_id, reply_to_message_id, &reply, Some(markup))
                            .await,
                    }
                } else {
                    self.send_reply(chat_id, reply_to_message_id, &reply, Some(markup))
                        .await
                }
            }
            TelegramSlashAction::Compact => {
                let mut client = self.connect_client().await?;
                let Some(session_id) = self.current_session_id(chat_id).await? else {
                    self.send_reply(
                        chat_id,
                        reply_to_message_id,
                        "No active session. Send a message or use /clear first.",
                        Some(default_slash_reply_markup()),
                    )
                    .await?;
                    return Ok(());
                };
                let result = client
                    .call(
                        methods::COMPACT_SESSION,
                        Some(serde_json::json!({ "session_id": session_id })),
                    )
                    .await?;
                match result {
                    Ok(_) => {
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            "Context compacted.",
                            Some(context_reply_markup(SessionStateTab::Overview)),
                        )
                        .await
                    }
                    Err(error) => {
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            &format!("Failed to compact context: {error}"),
                            Some(default_slash_reply_markup()),
                        )
                        .await
                    }
                }
            }
            TelegramSlashAction::Context => {
                let mut client = self.connect_client().await?;
                let Some(session_id) = self.resolve_status_session_id(chat_id).await? else {
                    eprintln!("[telegram] /context chat_id={chat_id} no session available");
                    self.send_reply(
                        chat_id,
                        reply_to_message_id,
                        "No session found. Send a message first.",
                        Some(default_slash_reply_markup()),
                    )
                    .await?;
                    return Ok(());
                };
                eprintln!("[telegram] /context chat_id={chat_id} session_id={session_id}");
                let params = serde_json::json!({ "session_id": session_id });
                eprintln!("[telegram] /context fetching context chat_id={chat_id}");
                let result = client
                    .call(methods::GET_SESSION_CONTEXT, Some(params))
                    .await?;
                match result {
                    Ok(value) => {
                        let snapshot: SessionContextSnapshot = serde_json::from_value(value)?;
                        eprintln!(
                            "[telegram] /context fetched context chat_id={chat_id} session_id={session_id}"
                        );
                        let reply = render_context_view(&snapshot);
                        let markup = context_reply_markup(SessionStateTab::Overview);
                        if edit_in_place {
                            match self
                                .edit_message(chat_id, reply_to_message_id, &reply, Some(markup.clone()))
                                .await
                            {
                                Ok(()) => Ok(()),
                                Err(_) => self
                                    .send_reply(
                                        chat_id,
                                        reply_to_message_id,
                                        &reply,
                                        Some(markup),
                                    )
                                    .await,
                            }
                        } else {
                            self.send_reply(chat_id, reply_to_message_id, &reply, Some(markup))
                                .await
                        }
                    }
                    Err(error) => {
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            &format!("Failed to load context: {error}"),
                            Some(default_slash_reply_markup()),
                        )
                        .await
                    }
                }
            }
            TelegramSlashAction::Clear => {
                let mut client = self.connect_client().await?;
                let created = create_session(
                    &mut client,
                    &self.skills,
                    false,
                    self.default_model_profile.as_deref(),
                )
                .await?;
                self.chat_state.lock().await.insert(
                    chat_id,
                    TelegramChatState {
                        session_id: Some(created.session_id.clone()),
                        plan_mode: false,
                        model_profile: self.default_model_profile.clone(),
                        state_tab: SessionStateTab::Overview,
                        session_picker_page: 0,
                    },
                );
                self.send_reply(
                    chat_id,
                    reply_to_message_id,
                    &format!("Started new session {}.", created.session_id),
                    Some(context_reply_markup(SessionStateTab::Overview)),
                )
                .await
            }
            TelegramSlashAction::SetModelProfile { model_profile } => {
                let mut client = self.connect_client().await?;
                let Some(session_id) = self.current_session_id(chat_id).await? else {
                    self.send_reply(
                        chat_id,
                        reply_to_message_id,
                        "No active session. Send a message or use /clear first.",
                        Some(default_slash_reply_markup()),
                    )
                    .await?;
                    return Ok(());
                };
                match crate::session::set_session_model_profile(
                    &mut client,
                    &session_id,
                    &model_profile,
                )
                .await
                {
                    Ok(()) => {
                        if let Some(state) = self.chat_state.lock().await.get_mut(&chat_id) {
                            state.model_profile = Some(model_profile.clone());
                        }
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            &format!("Switched model profile to `{model_profile}`."),
                            Some(default_slash_reply_markup()),
                        )
                        .await
                    }
                    Err(error) => {
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            &format!("Failed to switch model profile: {error}"),
                            Some(default_slash_reply_markup()),
                        )
                        .await
                    }
                }
            }
            TelegramSlashAction::Switch { session_id } => {
                let mut client = self.connect_client().await?;
                eprintln!("[telegram] /switch chat_id={chat_id} session_id={session_id}");
                match fetch_session_context(&mut client, &session_id).await {
                    Ok(snapshot) => {
                        self.chat_state.lock().await.insert(
                            chat_id,
                            TelegramChatState {
                                session_id: Some(session_id.clone()),
                                plan_mode: snapshot.plan_mode,
                                model_profile: None,
                                state_tab: SessionStateTab::Overview,
                                session_picker_page: 0,
                            },
                        );
                        let reply = format!("Switched to session {session_id}.");
                        let markup = context_reply_markup(SessionStateTab::Overview);
                        if edit_in_place {
                            match self
                                .edit_message(chat_id, reply_to_message_id, &reply, Some(markup.clone()))
                                .await
                            {
                                Ok(()) => Ok(()),
                                Err(_) => self
                                    .send_reply(
                                        chat_id,
                                        reply_to_message_id,
                                        &reply,
                                        Some(markup),
                                    )
                                    .await,
                            }
                        } else {
                            self.send_reply(chat_id, reply_to_message_id, &reply, Some(markup))
                                .await
                        }
                    }
                    Err(error) => {
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            &format!("Failed to switch session: {error}"),
                            Some(default_slash_reply_markup()),
                        )
                        .await
                    }
                }
            }
            TelegramSlashAction::SendPlan { request } => {
                self.send_plan_request(chat_id, reply_to_message_id, request)
                    .await
            }
            TelegramSlashAction::ScheduleLoop {
                request,
                delay_secs,
                cadence_secs,
            } => {
                let mut client = self.connect_client().await?;
                let Some(session_id) = self.current_session_id(chat_id).await? else {
                    self.send_reply(
                        chat_id,
                        reply_to_message_id,
                        "No active session. Send a message or use /clear first.",
                        Some(default_slash_reply_markup()),
                    )
                    .await?;
                    return Ok(());
                };
                let params = serde_json::json!({
                    "parent_id": session_id,
                    "task": request,
                    "delay_secs": delay_secs,
                    "cadence_secs": cadence_secs,
                });
                let result = client.call(methods::SCHEDULE_AGENT, Some(params)).await?;
                match result {
                    Ok(_) => {
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            "Loop scheduled.",
                            Some(default_slash_reply_markup()),
                        )
                        .await
                    }
                    Err(error) => {
                        self.send_reply(
                            chat_id,
                            reply_to_message_id,
                            &format!("Failed to schedule loop: {error}"),
                            Some(default_slash_reply_markup()),
                        )
                        .await
                    }
                }
            }
            TelegramSlashAction::StartSkill {
                skill_name,
                request,
            } => {
                self.send_skill_request(chat_id, reply_to_message_id, skill_name, request)
                    .await
            }
            TelegramSlashAction::SendMessage(content) => {
                if from_slash_command && trimmed.starts_with('/') {
                    self.send_reply(
                        chat_id,
                        reply_to_message_id,
                        &content,
                        Some(default_slash_reply_markup()),
                    )
                    .await
                } else {
                    match self
                        .send_chat_message(chat_id, reply_to_message_id, content)
                        .await
                    {
                        Ok(()) => Ok(()),
                        Err(error) => self
                            .send_reply(
                                chat_id,
                                reply_to_message_id,
                                &format!("Quine error: {error}"),
                                Some(default_slash_reply_markup()),
                            )
                            .await,
                    }
                }
            }
        }
    }

    async fn handle_message(&self, message: &TelegramMessage) -> anyhow::Result<()> {
        let Some(user) = &message.from else {
            return Ok(());
        };
        if !self.allowed_user_ids.contains(&user.id) {
            return Ok(());
        }

        let Some(text) = &message.text else {
            return Ok(());
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        self.handle_command(message.chat.id, message.message_id, trimmed, true, false)
            .await
    }

    async fn handle_callback_query(&self, query: &TelegramCallbackQuery) -> anyhow::Result<()> {
        let Some(message) = &query.message else {
            self.answer_callback_query(&query.id, Some("Nothing to update."))
                .await?;
            return Ok(());
        };
        let Some(data) = &query.data else {
            self.answer_callback_query(&query.id, Some("Empty action."))
                .await?;
            return Ok(());
        };

        let data = data.strip_prefix("cmd:").unwrap_or(data);

        if let Some(page_text) = data.strip_prefix("ps:page:") {
            let page = page_text.parse::<usize>().unwrap_or(0);
            let mut client = self.connect_client().await?;
            let sessions = self.fetch_sessions(&mut client).await?;
            let (reply, page, total_pages) = render_session_picker_page(&sessions, page);
            self.set_session_picker_page(message.chat.id, page).await;
            let markup = session_picker_reply_markup(&sessions, page, total_pages);
            if self
                .edit_message(message.chat.id, message.message_id, &reply, Some(markup.clone()))
                .await
                .is_err()
            {
                self.send_reply(message.chat.id, message.message_id, &reply, Some(markup))
                    .await?;
            }
            self.answer_callback_query(&query.id, None).await?;
            return Ok(());
        }

        if let Some(session_id) = data.strip_prefix("ps:switch:") {
            let command = format!("/switch {session_id}");
            self.answer_callback_query(&query.id, None).await?;
            return self
                .handle_command(message.chat.id, message.message_id, &command, true, true)
                .await;
        }

        self.answer_callback_query(&query.id, None).await?;
        self.handle_command(message.chat.id, message.message_id, data, true, true)
            .await
    }

    async fn run_foreground(&self) -> anyhow::Result<()> {
        self.validate_token().await?;
        eprintln!("Telegram plugin started. Waiting for updates...");

        let mut offset = 0_i64;
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("Telegram plugin stopped.");
                    return Ok(());
                }
                updates = self.poll_updates(offset) => {
                    match updates {
                        Ok(updates) => {
                            for update in updates {
                                offset = update.update_id + 1;
                                self.spawn_update_task(update);
                            }
                        }
                        Err(error) => {
                            eprintln!("Telegram polling error: {error}");
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }
                }
            }
        }
    }

    async fn run_background(&self) -> anyhow::Result<()> {
        self.validate_token().await?;
        eprintln!("Telegram plugin autostarted. Waiting for updates...");

        let mut offset = 0_i64;
        loop {
            match self.poll_updates(offset).await {
                Ok(updates) => {
                    for update in updates {
                        offset = update.update_id + 1;
                        self.spawn_update_task(update);
                    }
                }
                Err(error) => {
                    eprintln!("Telegram polling error: {error}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    fn spawn_update_task(&self, update: TelegramUpdate) {
        let bot = self.clone();
        tokio::spawn(async move {
            if let Some(message) = update.message {
                if let Err(error) = bot.handle_message(&message).await {
                    eprintln!("Telegram plugin error: {error}");
                    let _ = bot
                        .send_message(
                            message.chat.id,
                            &format!("Quine error: {error}"),
                            Some(message.message_id),
                            None,
                        )
                        .await;
                }
            }
            if let Some(callback_query) = update.callback_query {
                if let Err(error) = bot.handle_callback_query(&callback_query).await {
                    eprintln!("Telegram plugin error: {error}");
                    if let Some(message) = callback_query.message {
                        let _ = bot
                            .send_message(
                                message.chat.id,
                                &format!("Quine error: {error}"),
                                Some(message.message_id),
                                None,
                            )
                            .await;
                    }
                }
            }
        });
    }
}

fn resolve_config_path(config_path: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(path) = config_path {
        return Ok(path.to_path_buf());
    }

    let candidates = config_candidates();
    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    anyhow::bail!(
        "Telegram plugin config not found. Checked: {}",
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn config_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from(".quine/plugins/telegram.toml")];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".quine").join("plugins").join("telegram.toml"));
    }
    candidates
}

fn autostart_config_path() -> Option<PathBuf> {
    config_candidates().into_iter().find(|path| path.is_file())
}

fn parse_loop_arguments(arguments: &str) -> Result<(String, Duration, Option<Duration>), String> {
    let trimmed = arguments.trim();
    let mut parts = trimmed.split_whitespace();
    let mode = parts.next().ok_or_else(|| {
        "Usage: /loop every <duration> <message> | /loop in <duration> <message>".to_string()
    })?;
    let duration_text = parts.next().ok_or_else(|| {
        "Usage: /loop every <duration> <message> | /loop in <duration> <message>".to_string()
    })?;
    let request = parts.collect::<Vec<_>>().join(" ");
    if request.is_empty() {
        return Err(
            "Usage: /loop every <duration> <message> | /loop in <duration> <message>".into(),
        );
    }

    let duration = parse_duration(duration_text)?;
    match mode {
        "every" => Ok((request, Duration::ZERO, Some(duration))),
        "in" => Ok((request, duration, None)),
        _ => Err("Usage: /loop every <duration> <message> | /loop in <duration> <message>".into()),
    }
}

fn parse_duration(input: &str) -> Result<Duration, String> {
    if input.is_empty() {
        return Err("Duration cannot be empty".into());
    }
    let split_at = input
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "Duration must include a unit like s, m, h, or d".to_string())?;
    let (value, unit) = input.split_at(split_at);
    let amount = value
        .parse::<u64>()
        .map_err(|_| format!("Invalid duration value: {input}"))?;
    match unit {
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount * 60)),
        "h" => Ok(Duration::from_secs(amount * 60 * 60)),
        "d" => Ok(Duration::from_secs(amount * 60 * 60 * 24)),
        _ => Err(format!("Unsupported duration unit: {unit}")),
    }
}

const SESSION_PICKER_PAGE_SIZE: usize = 6;

fn summarize_session(session: &SessionSummary) -> Option<String> {
    session.summary_text().map(|summary| {
        format!(
            "{} {} - {}",
            short_session_id(&session.session_id),
            if session.plan_mode { "[plan]" } else { "[chat]" },
            truncate_text(summary, 32)
        )
    })
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut output = String::new();
    for _ in 0..max_chars {
        if let Some(ch) = chars.next() {
            output.push(ch);
        } else {
            return output;
        }
    }
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

fn render_session_picker_page(sessions: &[SessionSummary], page: usize) -> (String, usize, usize) {
    let summarized: Vec<&SessionSummary> =
        sessions.iter().filter(|session| session.summary_text().is_some()).collect();
    if summarized.is_empty() {
        return ("No sessions with summaries found.".to_string(), 0, 0);
    }

    let total_pages = summarized.len().div_ceil(SESSION_PICKER_PAGE_SIZE);
    let page = page.min(total_pages.saturating_sub(1));
    let start = page * SESSION_PICKER_PAGE_SIZE;
    let end = (start + SESSION_PICKER_PAGE_SIZE).min(summarized.len());
    let visible = &summarized[start..end];

    let mut lines = vec![
        format!(
            "Sessions with summaries: {} | page {}/{}",
            summarized.len(),
            page + 1,
            total_pages
        ),
        "Tap a session to switch.".to_string(),
        String::new(),
    ];

    for session in visible {
        if let Some(summary) = summarize_session(session) {
            lines.push(summary);
        }
    }

    (lines.join("\n"), page, total_pages)
}

fn render_session_tree(sessions: &[SessionSummary]) -> String {
    if sessions.is_empty() {
        return "No sessions found.".to_string();
    }

    let mut by_id = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots = Vec::new();
    for session in sessions {
        let id = session.session_id.clone();
        if let Some(parent_id) = session.parent_id.clone() {
            children.entry(parent_id).or_default().push(id.clone());
        } else {
            roots.push(id.clone());
        }
        by_id.insert(id, session);
    }
    roots.sort();
    for child_list in children.values_mut() {
        child_list.sort();
    }

    fn walk(
        output: &mut Vec<String>,
        by_id: &HashMap<String, &SessionSummary>,
        children: &HashMap<String, Vec<String>>,
        session_id: &str,
        prefix: &str,
        is_last: bool,
    ) {
        let Some(session) = by_id.get(session_id) else {
            return;
        };
        let branch = if prefix.is_empty() {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };
        let summary = session
            .summary
            .as_deref()
            .or(session.title.as_deref())
            .unwrap_or("");
        output.push(format!(
            "{}{}{} [{}]{}{}",
            prefix,
            branch,
            session.session_id,
            session.status,
            if summary.is_empty() {
                String::new()
            } else {
                format!(" - {summary}")
            },
            if session.plan_mode { " [plan]" } else { "" },
        ));

        let next_prefix = if prefix.is_empty() {
            String::new()
        } else if is_last {
            format!("{prefix}   ")
        } else {
            format!("{prefix}│  ")
        };

        if let Some(child_ids) = children.get(session_id) {
            for (index, child_id) in child_ids.iter().enumerate() {
                walk(
                    output,
                    by_id,
                    children,
                    child_id,
                    &next_prefix,
                    index + 1 == child_ids.len(),
                );
            }
        }
    }

    let mut output = Vec::new();
    for (index, root_id) in roots.iter().enumerate() {
        walk(
            &mut output,
            &by_id,
            &children,
            root_id,
            "",
            index + 1 == roots.len(),
        );
    }
    output.join("\n")
}

fn format_reply(output: &OneshotOutput) -> String {
    if let Some(interaction) = &output.interaction_needed {
        let mut lines = vec![format!("Interaction needed: {}", interaction.prompt)];
        if let Some(label) = &interaction.source_label {
            lines.push(format!("Source: {label}"));
        }
        if !interaction.response.trim().is_empty() {
            lines.push(String::new());
            lines.push(interaction.response.clone());
        }
        lines.join("\n")
    } else if output.response.trim().is_empty() {
        "No response.".to_string()
    } else {
        output.response.clone()
    }
}

fn chunk_message(message: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for ch in message.chars() {
        if current_len >= max_chars {
            chunks.push(current);
            current = String::new();
            current_len = 0;
        }
        current.push(ch);
        current_len += 1;
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

fn parse_state_tab(input: &str) -> Option<SessionStateTab> {
    match input.trim() {
        "" => None,
        "overview" => Some(SessionStateTab::Overview),
        "history" => Some(SessionStateTab::History),
        "tools" => Some(SessionStateTab::Tools),
        "skills" => Some(SessionStateTab::Skills),
        "plans" => Some(SessionStateTab::Plans),
        "memory" => Some(SessionStateTab::Memory),
        "permissions" => Some(SessionStateTab::Permissions),
        _ => None,
    }
}

impl SessionStateTab {
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::History => "history",
            Self::Tools => "tools",
            Self::Skills => "skills",
            Self::Plans => "plans",
            Self::Memory => "memory",
            Self::Permissions => "permissions",
        }
    }

    fn command(self) -> String {
        match self {
            Self::Overview => "/state overview".to_string(),
            _ => format!("/state {}", self.label()),
        }
    }
}

fn command_button(label: impl Into<String>, command: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "text": label.into(),
        "callback_data": format!("cmd:{}", command.into()),
    })
}

fn command_keyboard(rows: Vec<Vec<(String, String)>>) -> serde_json::Value {
    serde_json::json!({
        "inline_keyboard": rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(label, command)| command_button(label, command))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    })
}

fn default_slash_reply_markup() -> serde_json::Value {
    command_keyboard(vec![
        vec![
            ("State".to_string(), "/state".to_string()),
            ("Sessions".to_string(), "/ps".to_string()),
            ("Tree".to_string(), "/ps tree".to_string()),
        ],
        vec![
            ("Context".to_string(), "/context".to_string()),
            ("Compact".to_string(), "/compact".to_string()),
            ("Clear".to_string(), "/clear".to_string()),
        ],
        vec![("Quit".to_string(), "/quit".to_string())],
    ])
}

fn session_picker_reply_markup(
    sessions: &[SessionSummary],
    page: usize,
    total_pages: usize,
) -> serde_json::Value {
    let summarized: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|session| session.summary_text().is_some())
        .collect();
    if summarized.is_empty() {
        return command_keyboard(vec![vec![(
            "Refresh".to_string(),
            "/ps".to_string(),
        )]]);
    }

    let page = page.min(total_pages.saturating_sub(1));
    let start = page * SESSION_PICKER_PAGE_SIZE;
    let end = (start + SESSION_PICKER_PAGE_SIZE).min(summarized.len());

    let mut rows: Vec<Vec<(String, String)>> = summarized[start..end]
        .iter()
        .map(|session| {
            vec![
                (
                    session_picker_button_label(session),
                    format!("/switch {}", session.session_id),
                ),
            ]
        })
        .collect();

    let prev_page = page.saturating_sub(1);
    let next_page = (page + 1).min(total_pages.saturating_sub(1));
    let mut nav_row = Vec::new();
    if page > 0 {
        nav_row.push(("Prev".to_string(), format!("ps:page:{prev_page}")));
    }
    if total_pages > 1 && page + 1 < total_pages {
        nav_row.push(("Next".to_string(), format!("ps:page:{next_page}")));
    }
    nav_row.push(("Refresh".to_string(), format!("ps:page:{page}")));
    rows.push(nav_row);
    rows.push(vec![
        ("State".to_string(), "/state".to_string()),
        ("Context".to_string(), "/context".to_string()),
    ]);

    command_keyboard(rows)
}

fn session_list_reply_markup(tree: bool, sessions: &[SessionSummary]) -> serde_json::Value {
    let toggle = if tree {
        "/ps".to_string()
    } else {
        "/ps tree".to_string()
    };
    let mut rows = vec![
        vec![
            ("State".to_string(), "/state".to_string()),
            if tree {
                ("List".to_string(), toggle)
            } else {
                ("Tree".to_string(), toggle)
            },
            ("Context".to_string(), "/context".to_string()),
        ],
        vec![
            ("Compact".to_string(), "/compact".to_string()),
            ("Clear".to_string(), "/clear".to_string()),
        ],
    ];
    rows.extend(session_buttons_rows(sessions));
    command_keyboard(rows)
}

fn context_reply_markup(active_tab: SessionStateTab) -> serde_json::Value {
    command_keyboard(vec![
        vec![
            ("Overview".to_string(), SessionStateTab::Overview.command()),
            ("History".to_string(), SessionStateTab::History.command()),
            ("Tools".to_string(), SessionStateTab::Tools.command()),
        ],
        vec![
            ("Skills".to_string(), SessionStateTab::Skills.command()),
            ("Plans".to_string(), SessionStateTab::Plans.command()),
            ("Memory".to_string(), SessionStateTab::Memory.command()),
        ],
        vec![
            ("Permissions".to_string(), SessionStateTab::Permissions.command()),
            ("Refresh".to_string(), active_tab.command()),
            ("Sessions".to_string(), "/ps".to_string()),
        ],
    ])
}

fn state_view_reply_markup(tab: SessionStateTab) -> serde_json::Value {
    command_keyboard(vec![
        vec![
            ("Overview".to_string(), SessionStateTab::Overview.command()),
            ("History".to_string(), SessionStateTab::History.command()),
            ("Tools".to_string(), SessionStateTab::Tools.command()),
        ],
        vec![
            ("Skills".to_string(), SessionStateTab::Skills.command()),
            ("Plans".to_string(), SessionStateTab::Plans.command()),
            ("Memory".to_string(), SessionStateTab::Memory.command()),
        ],
        vec![
            ("Permissions".to_string(), SessionStateTab::Permissions.command()),
            ("Refresh".to_string(), tab.command()),
            ("Context".to_string(), "/context".to_string()),
        ],
    ])
}

fn session_button_label(session: &SessionSummary) -> String {
    let short_id = short_session_id(&session.session_id);
    let summary = session.summary_text().unwrap_or("");
    if summary.is_empty() {
        format!("{short_id} {}", if session.plan_mode { "[plan]" } else { "[chat]" })
    } else {
        format!(
            "{short_id} {} - {summary}",
            if session.plan_mode { "[plan]" } else { "[chat]" }
        )
    }
}

fn session_buttons_rows(sessions: &[SessionSummary]) -> Vec<Vec<(String, String)>> {
    let mut rows = Vec::new();
    for session in sessions
        .iter()
        .filter(|session| session.summary_text().is_some())
        .take(6)
    {
        let label = session_button_label(session);
        rows.push(vec![(label, format!("/switch {}", session.session_id))]);
    }
    rows
}

fn session_picker_button_label(session: &SessionSummary) -> String {
    let summary = session.summary_text().unwrap_or("");
    format!(
        "{} {} - {}",
        short_session_id(&session.session_id),
        if session.plan_mode { "[plan]" } else { "[chat]" },
        truncate_text(summary, 24)
    )
}

fn short_session_id(session_id: &str) -> String {
    if session_id.len() <= 12 {
        session_id.to_string()
    } else {
        format!("{}…", &session_id[..11])
    }
}

fn render_context_view(snapshot: &SessionContextSnapshot) -> String {
    let mut lines = vec![
        format!("Session: {}", snapshot.session_id),
        format!(
            "Mode: {} | State: {} | Plan: {}",
            if snapshot.plan_mode { "plan" } else { "chat" },
            snapshot.state,
            if snapshot.plan_mode { "on" } else { "off" }
        ),
        format!("Created: {}", snapshot.created_at),
        format!("Cwd: {}", snapshot.working_directory.display()),
        format!(
            "Lineage: root {} | parent {} | depth {} | children {}",
            snapshot.lineage.root_id,
            snapshot.lineage.parent_id.as_deref().unwrap_or("<root>"),
            snapshot.lineage.depth,
            snapshot.lineage.child_ids.len()
        ),
        format!(
            "Skills: {} | Tools: {} | Plans: {}",
            snapshot.loaded_skills.len(),
            snapshot.available_tools.len(),
            snapshot.plans.len()
        ),
    ];

    lines.push(String::new());
    lines.push("Summary:".to_string());
    match snapshot.compact_memory_summary_markdown.as_deref() {
        Some(summary) => {
            for line in summary
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    !trimmed.is_empty() && trimmed != "# Session Summary"
                })
                .take(4)
            {
                lines.push(format!("  {line}"));
            }
        }
        None => lines.push("  <none>".to_string()),
    }

    lines.push(String::new());
    lines.push("Recent activity:".to_string());
    if snapshot.history.is_empty() {
        lines.push("  <empty>".to_string());
    } else {
        for (index, entry) in snapshot.history.iter().rev().take(6).enumerate() {
            lines.push(format!("  {}", format_history_entry(index, entry)));
        }
    }

    lines.join("\n")
}

fn render_state_view(snapshot: &SessionContextSnapshot, tab: SessionStateTab) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Session: {}", snapshot.session_id));
    lines.push(format!(
        "State: {} | Mode: {} | Plan: {}",
        snapshot.state,
        if snapshot.plan_mode { "plan" } else { "chat" },
        if snapshot.plan_mode { "on" } else { "off" }
    ));
    lines.push(format!("Created: {}", snapshot.created_at));
    lines.push(format!("Cwd: {}", snapshot.working_directory.display()));
    lines.push(format!(
        "Lineage: root {} | parent {} | depth {} | children {}",
        snapshot.lineage.root_id,
        snapshot.lineage.parent_id.as_deref().unwrap_or("<root>"),
        snapshot.lineage.depth,
        snapshot.lineage.child_ids.len()
    ));
    lines.push(format!(
        "System prompt: {}",
        snapshot
            .system_prompt
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("<none>")
    ));
    lines.push(format!(
        "Tools: {} | Loaded skills: {} | Plans: {}",
        snapshot.available_tools.len(),
        snapshot.loaded_skills.len(),
        snapshot.plans.len()
    ));

    match tab {
        SessionStateTab::Overview => {
            lines.push(String::new());
            lines.push("Session summary:".to_string());
            match snapshot.compact_memory_summary_markdown.as_deref() {
                Some(summary) => {
                    for line in summary
                        .lines()
                        .filter(|line| {
                            let trimmed = line.trim();
                            !trimmed.is_empty() && trimmed != "# Session Summary"
                        })
                        .take(4)
                    {
                        lines.push(format!("  {line}"));
                    }
                }
                None => lines.push("  <none>".to_string()),
            }
        }
        SessionStateTab::History => {
            lines.push(String::new());
            lines.push("History:".to_string());
            let mut count = 0usize;
            for (index, entry) in snapshot.history.iter().rev().take(10).enumerate() {
                count += 1;
                lines.push(format!("  {}", format_history_entry(index, entry)));
            }
            if count == 0 {
                lines.push("  <empty>".to_string());
            }
        }
        SessionStateTab::Tools => {
            lines.push(String::new());
            lines.push("Available tools:".to_string());
            if snapshot.available_tools.is_empty() {
                lines.push("  <none>".to_string());
            } else {
                for tool in snapshot.available_tools.iter().take(12) {
                    let description = tool.description.lines().next().unwrap_or("").trim();
                    if description.is_empty() {
                        lines.push(format!("  {}", tool.name));
                    } else {
                        lines.push(format!("  {} - {}", tool.name, description));
                    }
                }
            }
        }
        SessionStateTab::Skills => {
            lines.push(String::new());
            lines.push("Loaded skills:".to_string());
            if snapshot.loaded_skills.is_empty() {
                lines.push("  <none>".to_string());
            } else {
                for skill in snapshot.loaded_skills.iter().take(12) {
                    lines.push(format!(
                        "  {} v{} - {}",
                        skill.name,
                        skill.version,
                        skill.source_path.display()
                    ));
                }
            }
        }
        SessionStateTab::Plans => {
            lines.push(String::new());
            lines.push("Plans:".to_string());
            if snapshot.plans.is_empty() {
                lines.push("  <none>".to_string());
            } else {
                for plan in snapshot.plans.iter().take(8) {
                    lines.push(format!(
                        "  {} - {} ({} actions)",
                        plan.plan_id,
                        plan.title,
                        plan.actions.len()
                    ));
                    for action in plan.actions.iter().take(6) {
                        lines.push(format!(
                            "    - {} [{}]",
                            action.title,
                            action.status.label()
                        ));
                    }
                }
            }
        }
        SessionStateTab::Memory => {
            lines.push(String::new());
            lines.push("Memory:".to_string());
            if let Some(diagnostics) = &snapshot.memory_diagnostics {
                lines.push(format!(
                    "  Session memory: {}",
                    memory_status_label(diagnostics.session_memory.refresh.status.clone())
                ));
                lines.push(format!(
                    "  Prompt memory: {}",
                    prompt_memory_label(&diagnostics.prompt_memory.mode)
                ));
                lines.push(format!(
                    "  Persistent memory: {}",
                    memory_status_label(diagnostics.persistent_memory.write_status.clone())
                ));
                if let Some(summary_path) = &diagnostics.session_memory.summary_path {
                    lines.push(format!("  Summary: {}", summary_path.display()));
                }
                if let Some(metadata_path) = &diagnostics.session_memory.metadata_path {
                    lines.push(format!("  Metadata: {}", metadata_path.display()));
                }
                if let Some(project_root) = &diagnostics.persistent_memory.project_root {
                    lines.push(format!("  Project root: {}", project_root.display()));
                }
            } else {
                lines.push("  <none>".to_string());
            }
        }
        SessionStateTab::Permissions => {
            lines.push(String::new());
            lines.push("Permissions:".to_string());
            if let Some(diagnostics) = &snapshot.permission_diagnostics {
                lines.push(format!(
                    "  Mode: {}",
                    permission_mode_label(&diagnostics.mode)
                ));
                if let Some(pre_plan_mode) = &diagnostics.pre_plan_mode {
                    lines.push(format!(
                        "  Pre-plan mode: {}",
                        permission_mode_label(pre_plan_mode)
                    ));
                }
                lines.push(format!("  Prompt: {:?}", diagnostics.prompt_behavior));
                lines.push(format!(
                    "  Workspace: {}",
                    diagnostics.workspace_root.display()
                ));
                lines.push(format!(
                    "  Additional roots: {}",
                    diagnostics.additional_allowed_roots.len()
                ));
                if let Some(last_decision) = &diagnostics.last_decision {
                    lines.push(format!("  Last decision: {}", last_decision.reason));
                }
                if let Some(pending) = &diagnostics.pending_approval {
                    lines.push(format!("  Pending: {}", pending.request_id));
                }
            } else {
                lines.push("  <none>".to_string());
            }
        }
    }

    lines.join("\n")
}

fn format_history_entry(index: usize, entry: &crate::context_debug::HistoryEntry) -> String {
    let entry_number = index + 1;
    match entry {
        crate::context_debug::HistoryEntry::Text { role, text } => {
            let first_line = text.lines().next().unwrap_or("").trim();
            if first_line.is_empty() {
                format!("{entry_number:>3}. {role}: <blank>")
            } else {
                format!("{entry_number:>3}. {role}: {first_line}")
            }
        }
        crate::context_debug::HistoryEntry::ToolUse {
            role,
            text,
            tool_calls,
        } => {
            let suffix = text
                .as_deref()
                .and_then(|value| value.lines().next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("");
            if tool_calls.len() > 1 {
                let names = tool_calls
                    .iter()
                    .map(|call| call.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if suffix.is_empty() {
                    format!(
                        "{entry_number:>3}. {role}: tool batch ({}) [{names}]",
                        tool_calls.len()
                    )
                } else {
                    format!(
                        "{entry_number:>3}. {role}: batch ({}) {suffix}",
                        tool_calls.len()
                    )
                }
            } else {
                let tool_summary = tool_calls
                    .first()
                    .map(|call| call.tool_name.as_str())
                    .unwrap_or("tool");
                if suffix.is_empty() {
                    format!("{entry_number:>3}. {role}: tool {tool_summary}")
                } else {
                    format!("{entry_number:>3}. {role}: {suffix}")
                }
            }
        }
        crate::context_debug::HistoryEntry::ToolResult {
            tool_use_id,
            is_error,
            ..
        } => {
            let status = if *is_error { "error" } else { "ok" };
            format!("{entry_number:>3}. tool result {tool_use_id} ({status})")
        }
    }
}

fn memory_status_label(status: crate::context_debug::MemoryStatusSnapshot) -> &'static str {
    match status {
        crate::context_debug::MemoryStatusSnapshot::NotRun => "not run",
        crate::context_debug::MemoryStatusSnapshot::Succeeded => "succeeded",
        crate::context_debug::MemoryStatusSnapshot::Skipped => "skipped",
        crate::context_debug::MemoryStatusSnapshot::FailedBestEffort => "failed best-effort",
    }
}

fn prompt_memory_label(mode: &crate::context_debug::PromptMemoryMode) -> &'static str {
    match mode {
        crate::context_debug::PromptMemoryMode::Disabled => "disabled",
        crate::context_debug::PromptMemoryMode::IndexOnly => "index-only",
        crate::context_debug::PromptMemoryMode::TargetedRecall => "targeted-recall",
    }
}

fn permission_mode_label(mode: &crate::context_debug::PermissionModeSnapshot) -> &'static str {
    match mode {
        crate::context_debug::PermissionModeSnapshot::Default => "default",
        crate::context_debug::PermissionModeSnapshot::AcceptEdits => "accept-edits",
        crate::context_debug::PermissionModeSnapshot::Plan => "plan",
        crate::context_debug::PermissionModeSnapshot::Bypass => "bypass",
    }
}

pub async fn serve(socket_path: &Path, config_path: Option<&Path>) -> anyhow::Result<()> {
    let (config, path) = TelegramPluginConfig::load(config_path)?;
    if config.allowed_user_ids.is_empty() {
        anyhow::bail!(
            "Telegram plugin config at {} must define at least one allowed_user_id",
            path.display()
        );
    }

    let bot_token = config.bot_token()?;
    let bot = TelegramBot::new(
        bot_token,
        config.allowed_users(),
        socket_path.to_path_buf(),
        config.skills,
        config.auto_approve,
        config.model_profile,
    )?;

    eprintln!("Loaded Telegram plugin config from {}", path.display());
    bot.run_foreground().await
}

pub async fn run_autostart(socket_path: &Path) -> anyhow::Result<()> {
    let Some(config_path) = autostart_config_path() else {
        return Ok(());
    };

    let (config, path) = TelegramPluginConfig::load(Some(&config_path))?;
    if config.allowed_user_ids.is_empty() {
        eprintln!(
            "Telegram plugin not autostarted because {} has no allowed_user_ids",
            path.display()
        );
        return Ok(());
    }

    let bot = TelegramBot::new(
        config.bot_token()?,
        config.allowed_users(),
        socket_path.to_path_buf(),
        config.skills,
        config.auto_approve,
        config.model_profile,
    )?;

    eprintln!("Autostarted Telegram plugin from {}", path.display());
    bot.run_background().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_message_splits_on_character_boundaries() {
        let chunks = chunk_message("abcdef", 2);
        assert_eq!(chunks, vec!["ab", "cd", "ef"]);
    }

    #[test]
    fn format_reply_prefers_interaction_prompt() {
        let output = OneshotOutput {
            session_id: "s".into(),
            response: "partial".into(),
            tool_calls: Vec::new(),
            duration_us: None,
            usage: None,
            interaction_needed: Some(InteractionNeededOutput {
                prompt: "Need approval".into(),
                source_label: Some("permission:1".into()),
                response: "partial".into(),
                tool_calls: Vec::new(),
            }),
        };

        let reply = format_reply(&output);
        assert!(reply.contains("Need approval"));
        assert!(reply.contains("partial"));
    }
}
