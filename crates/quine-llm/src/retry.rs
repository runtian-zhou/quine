use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};

use crate::error::LlmError;

const MAX_ATTEMPTS: u32 = 4;
const INITIAL_BACKOFF_MS: u64 = 250;
const MAX_BACKOFF_MS: u64 = 5_000;

pub(crate) async fn send_with_retry(
    request: RequestBuilder,
    debug_label: &str,
) -> Result<Response, LlmError> {
    let mut next_delay = Duration::from_millis(INITIAL_BACKOFF_MS);
    let debug = std::env::var("QUINE_DEBUG").is_ok();

    for attempt in 1..=MAX_ATTEMPTS {
        let request = request.try_clone().ok_or_else(|| LlmError::InvalidConfig {
            message: "request body could not be cloned for retry".into(),
        })?;

        match request.send().await {
            Ok(response) => {
                if !should_retry_status(response.status()) || attempt == MAX_ATTEMPTS {
                    return Ok(response);
                }

                let delay = retry_delay_for_status(&response, next_delay);
                if debug {
                    eprintln!(
                        "[{debug_label}] retrying request after HTTP {} (attempt {attempt}/{MAX_ATTEMPTS}, delay={}ms)",
                        response.status().as_u16(),
                        delay.as_millis()
                    );
                }
                tokio::time::sleep(delay).await;
                next_delay = next_backoff(next_delay);
            }
            Err(error) => {
                if !should_retry_error(&error) || attempt == MAX_ATTEMPTS {
                    return Err(LlmError::from(error));
                }

                let delay = jitter_delay(next_delay, attempt);
                if debug {
                    eprintln!(
                        "[{debug_label}] retrying request after transport error (attempt {attempt}/{MAX_ATTEMPTS}, delay={}ms): {error}",
                        delay.as_millis()
                    );
                }
                tokio::time::sleep(delay).await;
                next_delay = next_backoff(next_delay);
            }
        }
    }

    Err(LlmError::InvalidConfig {
        message: "retry loop exhausted unexpectedly".into(),
    })
}

fn retry_delay_for_status(response: &Response, fallback: Duration) -> Duration {
    if let Some(retry_after) = response.headers().get(reqwest::header::RETRY_AFTER) {
        if let Ok(value) = retry_after.to_str() {
            if let Some(delay) = parse_retry_after(value) {
                return delay;
            }
        }
    }

    jitter_delay(fallback, 1)
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    let seconds = value.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds.min(MAX_BACKOFF_MS / 1000)))
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn should_retry_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request() || error.is_body()
}

fn next_backoff(current: Duration) -> Duration {
    let next_ms = (current.as_millis() as u64)
        .saturating_mul(2)
        .min(MAX_BACKOFF_MS);
    Duration::from_millis(next_ms)
}

fn jitter_delay(base: Duration, attempt: u32) -> Duration {
    let base_ms = base.as_millis() as u64;
    let jitter_window = (base_ms / 4).max(1);
    let offset = ((attempt as u64 * 97) % (jitter_window * 2 + 1)).saturating_sub(jitter_window);
    let jittered = base_ms.saturating_add_signed(offset as i64);
    Duration::from_millis(jittered.clamp(1, MAX_BACKOFF_MS))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::StatusCode;

    use super::{jitter_delay, next_backoff, parse_retry_after, should_retry_status};

    #[test]
    fn retries_rate_limits_and_server_errors() {
        assert!(should_retry_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry_status(StatusCode::BAD_GATEWAY));
        assert!(!should_retry_status(StatusCode::BAD_REQUEST));
        assert!(!should_retry_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn parses_retry_after_seconds() {
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("abc"), None);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(
            next_backoff(Duration::from_millis(250)),
            Duration::from_millis(500)
        );
        assert_eq!(
            next_backoff(Duration::from_millis(3_000)),
            Duration::from_millis(5_000)
        );
        assert_eq!(
            next_backoff(Duration::from_millis(5_000)),
            Duration::from_millis(5_000)
        );
    }

    #[test]
    fn jitter_stays_bounded() {
        let delay = jitter_delay(Duration::from_millis(400), 2);
        assert!(delay >= Duration::from_millis(300));
        assert!(delay <= Duration::from_millis(500));
    }
}
