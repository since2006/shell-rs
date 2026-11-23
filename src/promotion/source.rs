//! Where the promotions come from: shellrs.com over HTTPS, or in the UI
//! tests a fake.

use std::io::Read as _;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::Client;
use url::Url;

use crate::update::build_info::VERSION;
use crate::update::platform::user_agent;

/// What a request got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Body(Vec<u8>),
    /// Nothing there: the site took the document down.
    NotFound,
}

/// Why a request got nothing. Never shown: the start page just shows what
/// it had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    Network(String),
    Status(u16),
    /// Longer than `limit`.
    TooLarge,
}

/// Fetches on the promotions' worker thread, so it may block.
pub trait PromotionSource: Send + Sync {
    fn get(&self, url: &Url, limit: u64) -> Result<Answer, FetchError>;
}

/// The web, as the update check reaches it: only the User-Agent says
/// anything (version, system, processor), through the system's proxy.
pub struct HttpSource;

impl PromotionSource for HttpSource {
    fn get(&self, url: &Url, limit: u64) -> Result<Answer, FetchError> {
        let network = |error: reqwest::Error| FetchError::Network(error.to_string());
        // Per request, on the worker: the blocking client runs a runtime
        // of its own, which must not be made or dropped in an async context.
        let client = Client::builder()
            .user_agent(user_agent(VERSION))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(network)?;
        let response = client.get(url.as_str()).send().map_err(network)?;
        match response.status() {
            StatusCode::NOT_FOUND => return Ok(Answer::NotFound),
            status if !status.is_success() => return Err(FetchError::Status(status.as_u16())),
            _ => {}
        }
        let mut body = Vec::new();
        response
            .take(limit + 1)
            .read_to_end(&mut body)
            .map_err(|error| FetchError::Network(error.to_string()))?;
        if body.len() as u64 > limit {
            return Err(FetchError::TooLarge);
        }
        Ok(Answer::Body(body))
    }
}
