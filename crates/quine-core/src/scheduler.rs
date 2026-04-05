use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, VecDeque};

use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant};

use crate::channel::CoreInput;
use crate::permission::PermissionPromptBehavior;
use crate::session::{InheritanceFlags, SessionId};

#[derive(Debug)]
pub(crate) struct SchedulerHandle {
    tx: mpsc::Sender<SchedulerCommand>,
}

impl SchedulerHandle {
    pub(crate) async fn schedule_user_message(
        &self,
        session_id: SessionId,
        content: String,
        delay: Duration,
    ) -> Result<(), SchedulerError> {
        self.send(SchedulerCommand::Schedule {
            action: ScheduledAction::UserMessage {
                session_id,
                content,
            },
            delay,
        })
        .await
    }

    pub(crate) async fn schedule_spawn_session(
        &self,
        parent_id: SessionId,
        task: String,
        system_prompt: Option<String>,
        delay: Duration,
        cadence: Option<Duration>,
    ) -> Result<(), SchedulerError> {
        self.send(SchedulerCommand::Schedule {
            action: ScheduledAction::SpawnSession {
                parent_id,
                task,
                system_prompt,
                cadence,
            },
            delay,
        })
        .await
    }

    pub(crate) async fn send_ipc_message(
        &self,
        target: String,
        content: String,
    ) -> Result<(), SchedulerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send(SchedulerCommand::SendIpcMessage {
            target,
            content,
            reply: reply_tx,
        })
        .await?;
        reply_rx.await.map_err(|_| SchedulerError::Closed)?
    }

    pub(crate) async fn recv_ipc_message(
        &self,
        source: String,
        non_blocking: bool,
    ) -> Result<Option<String>, SchedulerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send(SchedulerCommand::RecvIpcMessage {
            source,
            _non_blocking: non_blocking,
            reply: reply_tx,
        })
        .await?;
        reply_rx.await.map_err(|_| SchedulerError::Closed)
    }

    pub(crate) async fn shutdown(&self) -> Result<(), SchedulerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send(SchedulerCommand::Shutdown { reply: reply_tx })
            .await?;
        reply_rx.await.map_err(|_| SchedulerError::Closed)
    }

    async fn send(&self, command: SchedulerCommand) -> Result<(), SchedulerError> {
        self.tx
            .send(command)
            .await
            .map_err(|_| SchedulerError::Closed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerError {
    Closed,
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchedulerError::Closed => write!(f, "scheduler closed"),
        }
    }
}

impl std::error::Error for SchedulerError {}

#[derive(Debug)]
enum SchedulerCommand {
    Schedule {
        action: ScheduledAction,
        delay: Duration,
    },
    SendIpcMessage {
        target: String,
        content: String,
        reply: oneshot::Sender<Result<(), SchedulerError>>,
    },
    RecvIpcMessage {
        source: String,
        _non_blocking: bool,
        reply: oneshot::Sender<Option<String>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone)]
enum ScheduledAction {
    UserMessage {
        session_id: SessionId,
        content: String,
    },
    SpawnSession {
        parent_id: SessionId,
        task: String,
        system_prompt: Option<String>,
        cadence: Option<Duration>,
    },
}

struct ScheduledCommandAt {
    execute_at: Instant,
    action: ScheduledAction,
}

impl ScheduledCommandAt {
    fn new(action: ScheduledAction, delay: Duration) -> Self {
        Self {
            execute_at: Instant::now() + delay,
            action,
        }
    }
}

struct QueuedCommand {
    execute_at: Instant,
    sequence: u64,
    action: ScheduledAction,
}

impl QueuedCommand {
    fn new(command: ScheduledCommandAt, sequence: u64) -> Self {
        Self {
            execute_at: command.execute_at,
            sequence,
            action: command.action,
        }
    }
}

impl PartialEq for QueuedCommand {
    fn eq(&self, other: &Self) -> bool {
        self.execute_at == other.execute_at && self.sequence == other.sequence
    }
}

impl Eq for QueuedCommand {}

impl Ord for QueuedCommand {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .execute_at
            .cmp(&self.execute_at)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for QueuedCommand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn spawn_scheduler(
    input_tx: mpsc::Sender<CoreInput>,
) -> (SchedulerHandle, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(256);
    let task = tokio::spawn(scheduler_loop(input_tx, rx));
    (SchedulerHandle { tx }, task)
}

async fn scheduler_loop(
    input_tx: mpsc::Sender<CoreInput>,
    mut command_rx: mpsc::Receiver<SchedulerCommand>,
) {
    let mut pending = BinaryHeap::new();
    let mut next_sequence = 0_u64;
    let mut channel_closed = false;
    let mut ipc_mailboxes: HashMap<String, VecDeque<String>> = HashMap::new();

    loop {
        let now = Instant::now();
        while pending
            .peek()
            .is_some_and(|command: &QueuedCommand| command.execute_at <= now)
        {
            let queued = pending.pop().expect("pending heap is not empty");
            if dispatch_scheduled_action(&input_tx, &mut pending, &mut next_sequence, queued)
                .await
                .is_err()
            {
                return;
            }
        }

        if channel_closed && pending.is_empty() {
            return;
        }

        if pending.is_empty() {
            match command_rx.recv().await {
                Some(command) => handle_scheduler_command(
                    command,
                    &mut pending,
                    &mut next_sequence,
                    &mut ipc_mailboxes,
                    &mut channel_closed,
                ),
                None => channel_closed = true,
            }
            continue;
        }

        let next_deadline = pending
            .peek()
            .map(|command| command.execute_at)
            .expect("pending heap is not empty");

        if channel_closed {
            tokio::time::sleep_until(next_deadline).await;
            continue;
        }

        tokio::select! {
            maybe_command = command_rx.recv() => {
                match maybe_command {
                    Some(command) => handle_scheduler_command(
                        command,
                        &mut pending,
                        &mut next_sequence,
                        &mut ipc_mailboxes,
                        &mut channel_closed,
                    ),
                    None => channel_closed = true,
                }
            }
            _ = tokio::time::sleep_until(next_deadline) => {}
        }
    }
}

fn handle_scheduler_command(
    command: SchedulerCommand,
    pending: &mut BinaryHeap<QueuedCommand>,
    next_sequence: &mut u64,
    ipc_mailboxes: &mut HashMap<String, VecDeque<String>>,
    channel_closed: &mut bool,
) {
    match command {
        SchedulerCommand::Schedule { action, delay } => {
            pending.push(QueuedCommand::new(
                ScheduledCommandAt::new(action, delay),
                *next_sequence,
            ));
            *next_sequence += 1;
        }
        SchedulerCommand::SendIpcMessage {
            target,
            content,
            reply,
        } => {
            ipc_mailboxes.entry(target).or_default().push_back(content);
            let _ = reply.send(Ok(()));
        }
        SchedulerCommand::RecvIpcMessage {
            source,
            _non_blocking: _,
            reply,
        } => {
            let message = ipc_mailboxes
                .get_mut(&source)
                .and_then(|messages| messages.pop_front());
            let _ = reply.send(message);
        }
        SchedulerCommand::Shutdown { reply } => {
            pending.clear();
            *channel_closed = true;
            let _ = reply.send(());
        }
    }
}

async fn dispatch_scheduled_action(
    input_tx: &mpsc::Sender<CoreInput>,
    pending: &mut BinaryHeap<QueuedCommand>,
    next_sequence: &mut u64,
    queued: QueuedCommand,
) -> Result<(), SchedulerError> {
    match queued.action {
        ScheduledAction::UserMessage {
            session_id,
            content,
        } => input_tx
            .send(CoreInput::UserMessage {
                session_id,
                content,
            })
            .await
            .map_err(|_| SchedulerError::Closed),
        ScheduledAction::SpawnSession {
            parent_id,
            task,
            system_prompt,
            cadence,
        } => {
            if let Some(cadence) = cadence {
                pending.push(QueuedCommand {
                    execute_at: queued.execute_at + cadence,
                    sequence: *next_sequence,
                    action: ScheduledAction::SpawnSession {
                        parent_id,
                        task: task.clone(),
                        system_prompt: system_prompt.clone(),
                        cadence: Some(cadence),
                    },
                });
                *next_sequence += 1;
            }

            let (reply_tx, _reply_rx) = oneshot::channel();
            input_tx
                .send(CoreInput::SpawnSession {
                    parent_id,
                    child_id: SessionId::new(),
                    task,
                    system_prompt,
                    prompt_behavior: PermissionPromptBehavior::Background,
                    inheritance: InheritanceFlags::default(),
                    reply: reply_tx,
                })
                .await
                .map_err(|_| SchedulerError::Closed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn scheduler_orders_by_deadline_and_fifo() {
        let (input_tx, mut input_rx) = mpsc::channel(16);
        let (handle, task) = spawn_scheduler(input_tx);
        let session_one = SessionId::new();
        let session_two = SessionId::new();
        let session_three = SessionId::new();

        handle
            .schedule_user_message(session_two, "later".into(), Duration::from_secs(30))
            .await
            .unwrap();
        handle
            .schedule_user_message(session_one, "first".into(), Duration::from_secs(10))
            .await
            .unwrap();
        handle
            .schedule_user_message(session_three, "second".into(), Duration::from_secs(10))
            .await
            .unwrap();

        tokio::time::advance(Duration::from_secs(10)).await;
        let first = input_rx.recv().await.unwrap();
        let second = input_rx.recv().await.unwrap();

        match first {
            CoreInput::UserMessage {
                session_id,
                content,
            } => {
                assert_eq!(session_id, session_one);
                assert_eq!(content, "first");
            }
            _ => panic!("expected first scheduled user message"),
        }
        match second {
            CoreInput::UserMessage {
                session_id,
                content,
            } => {
                assert_eq!(session_id, session_three);
                assert_eq!(content, "second");
            }
            _ => panic!("expected second scheduled user message"),
        }

        tokio::time::advance(Duration::from_secs(20)).await;
        match input_rx.recv().await.unwrap() {
            CoreInput::UserMessage {
                session_id,
                content,
            } => {
                assert_eq!(session_id, session_two);
                assert_eq!(content, "later");
            }
            _ => panic!("expected later scheduled user message"),
        }

        handle.shutdown().await.unwrap();
        let _ = task.await;
    }

    #[tokio::test(start_paused = true)]
    async fn recurring_spawn_reschedules_from_launch_time() {
        let (input_tx, mut input_rx) = mpsc::channel(16);
        let (handle, task) = spawn_scheduler(input_tx);
        let parent_id = SessionId::new();

        handle
            .schedule_spawn_session(
                parent_id,
                "recurring".into(),
                Some("system".into()),
                Duration::from_secs(10),
                Some(Duration::from_secs(5)),
            )
            .await
            .unwrap();

        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(matches!(
            input_rx.recv().await.unwrap(),
            CoreInput::SpawnSession {
                prompt_behavior: PermissionPromptBehavior::Background,
                ..
            }
        ));

        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(matches!(
            input_rx.recv().await.unwrap(),
            CoreInput::SpawnSession {
                prompt_behavior: PermissionPromptBehavior::Background,
                ..
            }
        ));

        handle.shutdown().await.unwrap();
        let _ = task.await;
    }

    #[tokio::test]
    async fn ipc_mailbox_is_fifo() {
        let (input_tx, _input_rx) = mpsc::channel(4);
        let (handle, task) = spawn_scheduler(input_tx);

        handle
            .send_ipc_message("worker".into(), "one".into())
            .await
            .unwrap();
        handle
            .send_ipc_message("worker".into(), "two".into())
            .await
            .unwrap();

        assert_eq!(
            handle
                .recv_ipc_message("worker".into(), false)
                .await
                .unwrap(),
            Some("one".into())
        );
        assert_eq!(
            handle
                .recv_ipc_message("worker".into(), false)
                .await
                .unwrap(),
            Some("two".into())
        );
        assert_eq!(
            handle
                .recv_ipc_message("worker".into(), false)
                .await
                .unwrap(),
            None
        );

        handle.shutdown().await.unwrap();
        let _ = task.await;
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_drops_pending_scheduled_work() {
        let (input_tx, mut input_rx) = mpsc::channel(4);
        let (handle, task) = spawn_scheduler(input_tx);
        let session_id = SessionId::new();

        handle
            .schedule_user_message(session_id, "later".into(), Duration::from_secs(30))
            .await
            .unwrap();
        handle.shutdown().await.unwrap();

        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        assert!(input_rx.try_recv().is_err());

        let _ = task.await;
    }
}
