use async_trait::async_trait;

/// Trait for rendering output to the user.
///
/// Abstracts over the output medium so that the CLI can be tested
/// without writing to an actual terminal.
#[async_trait]
pub trait Renderer: Send + Sync {
    /// Print a streaming text delta (partial token).
    async fn render_delta(&mut self, delta: &str) -> anyhow::Result<()>;

    /// Signal that text generation is complete.
    async fn render_text_complete(&mut self, full_text: &str) -> anyhow::Result<()>;

    /// Render a tool request notification.
    async fn render_tool_request(
        &mut self,
        tool_name: &str,
        tool_use_id: &str,
    ) -> anyhow::Result<()>;

    /// Render an error message.
    async fn render_error(&mut self, message: &str) -> anyhow::Result<()>;

    /// Render the turn-complete marker (e.g., a newline or prompt).
    async fn render_turn_complete(&mut self) -> anyhow::Result<()>;
}

/// Terminal renderer that writes to stdout.
pub struct TerminalRenderer {
    writer: tokio::io::Stdout,
}

impl TerminalRenderer {
    pub fn new() -> Self {
        Self {
            writer: tokio::io::stdout(),
        }
    }
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Renderer for TerminalRenderer {
    async fn render_delta(&mut self, delta: &str) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.writer.write_all(delta.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn render_text_complete(&mut self, _full_text: &str) -> anyhow::Result<()> {
        // Delta rendering already printed the text; just ensure a newline.
        use tokio::io::AsyncWriteExt;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn render_tool_request(
        &mut self,
        tool_name: &str,
        tool_use_id: &str,
    ) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        let msg = format!("[tool request: {tool_name} ({tool_use_id})]\n");
        self.writer.write_all(msg.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn render_error(&mut self, message: &str) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        let msg = format!("Error: {message}\n");
        self.writer.write_all(msg.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn render_turn_complete(&mut self) -> anyhow::Result<()> {
        // No extra output needed; the prompt will be printed by the REPL.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test renderer that collects output into a string buffer.
    struct TestRenderer {
        buffer: String,
    }

    impl TestRenderer {
        fn new() -> Self {
            Self {
                buffer: String::new(),
            }
        }
    }

    #[async_trait]
    impl Renderer for TestRenderer {
        async fn render_delta(&mut self, delta: &str) -> anyhow::Result<()> {
            self.buffer.push_str(delta);
            Ok(())
        }

        async fn render_text_complete(&mut self, _full_text: &str) -> anyhow::Result<()> {
            self.buffer.push('\n');
            Ok(())
        }

        async fn render_tool_request(
            &mut self,
            tool_name: &str,
            _tool_use_id: &str,
        ) -> anyhow::Result<()> {
            self.buffer.push_str(&format!("[tool: {tool_name}]"));
            Ok(())
        }

        async fn render_error(&mut self, message: &str) -> anyhow::Result<()> {
            self.buffer.push_str(&format!("ERR: {message}"));
            Ok(())
        }

        async fn render_turn_complete(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_renderer_collects_output() {
        let mut renderer = TestRenderer::new();
        renderer.render_delta("hello ").await.unwrap();
        renderer.render_delta("world").await.unwrap();
        renderer.render_text_complete("hello world").await.unwrap();
        assert_eq!(renderer.buffer, "hello world\n");
    }
}
