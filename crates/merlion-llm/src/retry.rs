//! Retry the initial chat-completion POST when the provider hands back a
//! transient error (429 rate limit, 5xx server error).
//!
//! We can only retry the **setup** of a stream — once we start consuming
//! SSE chunks, the model has begun emitting tokens and we can't safely
//! restart from where we left off. So this helper wraps the
//! `http.post(...).send().await` step plus the status check, and returns a
//! `reqwest::Response` only once a 2xx body is in hand.

use std::time::Duration;

use merlion_core::{Error, Result};
use tracing::warn;

const MAX_ATTEMPTS: u32 = 3;
const BASE_BACKOFF_MS: u64 = 500;

/// Repeatedly call `build_request().send()` until it returns a 2xx response
/// or we've exhausted [`MAX_ATTEMPTS`]. Retries on transport errors and on
/// 429/5xx responses (with exponential backoff).
///
/// `build_request` is called fresh each attempt because a `reqwest::Request`
/// can't be cloned cheaply once a streaming body is attached.
pub async fn send_with_retry<F>(mut build_request: F) -> Result<reqwest::Response>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let mut attempt = 0u32;
    loop {
        let outcome = build_request().send().await;
        let response = match outcome {
            Ok(resp) => resp,
            Err(e) if attempt + 1 < MAX_ATTEMPTS => {
                let delay = backoff_for(attempt);
                warn!(attempt, ?delay, error = %e, "llm request transport error; retrying");
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            Err(e) => return Err(Error::Llm(format!("request: {e}"))),
        };

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let should_retry = status.as_u16() == 429 || status.is_server_error();
        if !should_retry || attempt + 1 >= MAX_ATTEMPTS {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Llm(format!("http {status}: {text}")));
        }

        // Drain the body so the connection can be reused.
        let _ = response.text().await;
        let delay = backoff_for(attempt);
        warn!(attempt, status = %status, ?delay, "llm request returned retriable status; retrying");
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

fn backoff_for(attempt: u32) -> Duration {
    Duration::from_millis(BASE_BACKOFF_MS * 2u64.pow(attempt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(backoff_for(0), Duration::from_millis(500));
        assert_eq!(backoff_for(1), Duration::from_millis(1000));
        assert_eq!(backoff_for(2), Duration::from_millis(2000));
    }
}
