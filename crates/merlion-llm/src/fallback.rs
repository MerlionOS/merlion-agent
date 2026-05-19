//! Multi-provider fallback wrapper.
//!
//! [`FallbackLlmClient`] wraps a primary [`LlmClient`] and an ordered chain of
//! backup clients. When the primary returns a retriable LLM error (HTTP 429
//! or 5xx — recognized via the message produced by `retry::send_with_retry`),
//! the wrapper transparently falls through to the next client in the chain.
//! The first successful stream wins; if every client fails, the last error
//! is returned.
//!
//! Fallthrough only triggers on `Error::Llm` whose message contains
//! `"http 429"` or `"http 5"` — i.e. only after the primary's own
//! per-provider retry budget has been exhausted. Transport errors, auth
//! errors, schema errors, and non-retriable 4xx all surface immediately
//! (no fallthrough) so misconfigurations don't silently mask themselves.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use merlion_core::{Error, LlmClient, LlmRequest, LlmStreamEvent, Result};
use tracing::warn;

pub struct FallbackLlmClient {
    primary: Arc<dyn LlmClient>,
    primary_name: String,
    chain: Vec<Arc<dyn LlmClient>>,
    names: Vec<String>,
}

impl FallbackLlmClient {
    /// `primary` is the first client tried. `chain` is the ordered list of
    /// fallbacks. `names` labels each entry (primary first, then the chain)
    /// for tracing — its length must be `1 + chain.len()`.
    pub fn new(
        primary: Arc<dyn LlmClient>,
        chain: Vec<Arc<dyn LlmClient>>,
        names: Vec<String>,
    ) -> Self {
        assert_eq!(
            names.len(),
            chain.len() + 1,
            "FallbackLlmClient::new: `names` must label the primary plus every fallback ({} expected, got {})",
            chain.len() + 1,
            names.len()
        );
        let mut names_iter = names.into_iter();
        let primary_name = names_iter.next().unwrap();
        let names: Vec<String> = names_iter.collect();
        Self {
            primary,
            primary_name,
            chain,
            names,
        }
    }
}

fn is_retriable(err: &Error) -> bool {
    match err {
        Error::Llm(msg) => {
            let lower = msg.to_ascii_lowercase();
            lower.contains("http 429") || lower.contains("http 5")
        }
        _ => false,
    }
}

#[async_trait]
impl LlmClient for FallbackLlmClient {
    async fn stream(&self, req: LlmRequest) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let mut last_err = match self.primary.stream(req.clone()).await {
            Ok(s) => return Ok(s),
            Err(e) if is_retriable(&e) => {
                warn!(
                    provider = %self.primary_name,
                    error = %e,
                    "primary llm returned retriable error; falling through to next provider"
                );
                e
            }
            Err(e) => return Err(e),
        };

        for (idx, client) in self.chain.iter().enumerate() {
            let name = self.names.get(idx).map(String::as_str).unwrap_or("?");
            match client.stream(req.clone()).await {
                Ok(s) => {
                    warn!(provider = %name, "fallback llm succeeded");
                    return Ok(s);
                }
                Err(e) if is_retriable(&e) => {
                    warn!(
                        provider = %name,
                        error = %e,
                        "fallback llm returned retriable error; trying next provider"
                    );
                    last_err = e;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use futures::StreamExt;
    use merlion_core::{LlmClient, LlmRequest, LlmStreamEvent};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Scripted mock LlmClient. Each call pops the next outcome off `script`.
    /// Outcomes are either an `Err(Error::Llm(...))` or a `Vec` of stream
    /// events to yield in order. We track call count so tests can assert
    /// which providers were exercised. Modeled after `ScriptedLlm` in
    /// `crates/merlion-core/tests/approval.rs`.
    struct MockLlmClient {
        script: std::sync::Mutex<Vec<Outcome>>,
        calls: AtomicUsize,
    }
    enum Outcome {
        Err(String),
        Ok(Vec<LlmStreamEvent>),
    }
    impl MockLlmClient {
        fn new(script: Vec<Outcome>) -> Arc<Self> {
            Arc::new(Self {
                script: std::sync::Mutex::new(script),
                calls: AtomicUsize::new(0),
            })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }
    #[async_trait]
    impl LlmClient for MockLlmClient {
        async fn stream(
            &self,
            _req: LlmRequest,
        ) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let outcome = self
                .script
                .lock()
                .unwrap()
                .pop()
                .expect("MockLlmClient called more times than scripted");
            match outcome {
                Outcome::Err(msg) => Err(Error::Llm(msg)),
                Outcome::Ok(events) => {
                    let events: Vec<Result<LlmStreamEvent>> = events.into_iter().map(Ok).collect();
                    Ok(futures::stream::iter(events).boxed())
                }
            }
        }
    }

    fn req() -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            messages: vec![],
            tools: vec![],
            temperature: None,
            max_tokens: None,
        }
    }

    async fn collect(mut s: BoxStream<'static, Result<LlmStreamEvent>>) -> Vec<LlmStreamEvent> {
        let mut out = Vec::new();
        while let Some(ev) = s.next().await {
            out.push(ev.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn primary_success_skips_fallbacks() {
        let primary = MockLlmClient::new(vec![Outcome::Ok(vec![
            LlmStreamEvent::Delta("hi".into()),
            LlmStreamEvent::Done(Some("stop".into())),
        ])]);
        let fb = MockLlmClient::new(vec![Outcome::Ok(vec![])]);
        let wrapped = FallbackLlmClient::new(
            primary.clone() as Arc<dyn LlmClient>,
            vec![fb.clone() as Arc<dyn LlmClient>],
            vec!["primary".into(), "fb".into()],
        );

        let events = collect(wrapped.stream(req()).await.unwrap()).await;
        assert!(matches!(events[0], LlmStreamEvent::Delta(ref s) if s == "hi"));
        assert_eq!(primary.calls(), 1);
        assert_eq!(fb.calls(), 0);
    }

    #[tokio::test]
    async fn falls_through_on_429() {
        let primary = MockLlmClient::new(vec![Outcome::Err("http 429: rate limited".into())]);
        let fb = MockLlmClient::new(vec![Outcome::Ok(vec![
            LlmStreamEvent::Delta("from-fb".into()),
            LlmStreamEvent::Done(None),
        ])]);
        let wrapped = FallbackLlmClient::new(
            primary.clone() as Arc<dyn LlmClient>,
            vec![fb.clone() as Arc<dyn LlmClient>],
            vec!["primary".into(), "fb".into()],
        );

        let events = collect(wrapped.stream(req()).await.unwrap()).await;
        assert!(matches!(events[0], LlmStreamEvent::Delta(ref s) if s == "from-fb"));
        assert_eq!(primary.calls(), 1);
        assert_eq!(fb.calls(), 1);
    }

    #[tokio::test]
    async fn falls_through_on_500() {
        let primary = MockLlmClient::new(vec![Outcome::Err("http 503: oops".into())]);
        let fb = MockLlmClient::new(vec![Outcome::Ok(vec![LlmStreamEvent::Done(None)])]);
        let wrapped = FallbackLlmClient::new(
            primary as Arc<dyn LlmClient>,
            vec![fb.clone() as Arc<dyn LlmClient>],
            vec!["primary".into(), "fb".into()],
        );

        let _ = wrapped.stream(req()).await.unwrap();
        assert_eq!(fb.calls(), 1);
    }

    #[tokio::test]
    async fn non_retriable_error_surfaces_immediately() {
        let primary = MockLlmClient::new(vec![Outcome::Err("http 401: bad key".into())]);
        let fb = MockLlmClient::new(vec![Outcome::Ok(vec![])]);
        let wrapped = FallbackLlmClient::new(
            primary as Arc<dyn LlmClient>,
            vec![fb.clone() as Arc<dyn LlmClient>],
            vec!["primary".into(), "fb".into()],
        );

        let err = wrapped.stream(req()).await.err().expect("should err");
        assert!(matches!(err, Error::Llm(_)));
        assert_eq!(fb.calls(), 0, "fallback must not run on non-retriable 4xx");
    }

    #[tokio::test]
    async fn returns_last_error_when_every_provider_fails() {
        let primary = MockLlmClient::new(vec![Outcome::Err("http 429: p".into())]);
        let fb1 = MockLlmClient::new(vec![Outcome::Err("http 502: f1".into())]);
        let fb2 = MockLlmClient::new(vec![Outcome::Err("http 504: f2-final".into())]);
        let wrapped = FallbackLlmClient::new(
            primary.clone() as Arc<dyn LlmClient>,
            vec![
                fb1.clone() as Arc<dyn LlmClient>,
                fb2.clone() as Arc<dyn LlmClient>,
            ],
            vec!["primary".into(), "fb1".into(), "fb2".into()],
        );

        let err = wrapped.stream(req()).await.err().expect("all failed");
        let msg = err.to_string();
        assert!(msg.contains("f2-final"), "got: {msg}");
        assert_eq!(primary.calls(), 1);
        assert_eq!(fb1.calls(), 1);
        assert_eq!(fb2.calls(), 1);
    }

    #[tokio::test]
    async fn empty_chain_propagates_primary_error() {
        let primary = MockLlmClient::new(vec![Outcome::Err("http 429: alone".into())]);
        let wrapped = FallbackLlmClient::new(
            primary as Arc<dyn LlmClient>,
            vec![],
            vec!["primary".into()],
        );
        let err = wrapped.stream(req()).await.err().expect("should err");
        assert!(err.to_string().contains("alone"));
    }
}
