use async_trait::async_trait;

/// Approximate end-user location used to localize web results.
#[derive(Debug, Clone, Default)]
pub struct WebUserLocation {
    pub country: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    pub timezone: Option<String>,
}

/// A citation returned from a web-backed answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebCitation {
    pub title: Option<String>,
    pub url: String,
    pub start_index: Option<u64>,
    pub end_index: Option<u64>,
}

/// A retrieved source URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSource {
    pub title: Option<String>,
    pub url: String,
}

/// Normalized result returned by a web provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebResult {
    pub text: String,
    pub citations: Vec<WebCitation>,
    pub sources: Vec<WebSource>,
}

/// A web search request.
#[derive(Debug, Clone)]
pub struct WebSearchRequest {
    pub query: String,
    pub allowed_domains: Vec<String>,
    pub user_location: Option<WebUserLocation>,
    pub external_web_access: bool,
}

/// A request to inspect a specific URL.
#[derive(Debug, Clone)]
pub struct WebOpenRequest {
    pub url: String,
    pub prompt: Option<String>,
    pub external_web_access: bool,
}

/// Trait abstracting the backing service for web-enabled tools.
#[async_trait]
pub trait WebProvider: Send + Sync {
    async fn search(&self, request: WebSearchRequest) -> anyhow::Result<WebResult>;

    async fn open(&self, request: WebOpenRequest) -> anyhow::Result<WebResult>;
}

/// Default placeholder provider for contexts that do not enable web access.
pub struct NoopWebProvider;

#[async_trait]
impl WebProvider for NoopWebProvider {
    async fn search(&self, _request: WebSearchRequest) -> anyhow::Result<WebResult> {
        anyhow::bail!("web tools are not configured")
    }

    async fn open(&self, _request: WebOpenRequest) -> anyhow::Result<WebResult> {
        anyhow::bail!("web tools are not configured")
    }
}
