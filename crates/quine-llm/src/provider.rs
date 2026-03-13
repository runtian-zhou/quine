use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::types::{CompletionRequest, CompletionResponse, StreamEvent};

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse>;

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>>;
}
