//! How requests reach a model service: a trait, so the tests answer from a
//! script, and reqwest behind it in the apps.

use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{StreamExt as _, stream};

/// A request to a model service. Its `Debug` leaves out the headers, which
/// hold the key.
#[derive(Clone)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("body", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl HttpRequest {
    /// The body as JSON, for the tests that look at what was sent.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A model service's answer: its status, and its body as it comes.
pub struct HttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    /// From `retry-after-ms` or `retry-after` (seconds), when sent.
    pub retry_after: Option<Duration>,
    pub body: BoxStream<'static, Result<Vec<u8>, TransportError>>,
}

/// Why a request got no answer, or its answer broke off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The service could not be reached, or the connection broke.
    Network(String),
    /// The service took too long.
    Timeout,
}

pub trait Transport: Send + Sync {
    fn post(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, Result<HttpResponse, TransportError>>;

    /// The same without a body (`request.body` is not sent): a service's
    /// list of its models.
    fn get(&self, request: HttpRequest)
    -> BoxFuture<'static, Result<HttpResponse, TransportError>>;
}

/// Requests over reqwest: the system's certificates and proxy, no
/// redirects (reqwest carries an `x-api-key` header to another host), and
/// time limits for the connection and for each read.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

/// How long a connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a read may wait: the first event of a reasoning model can be
/// a long time coming.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

impl ReqwestTransport {
    /// With the app's `User-Agent`, which this crate never builds.
    pub fn new(user_agent: &str) -> Result<Self, String> {
        reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map(|client| Self { client })
            .map_err(|error| error.to_string())
    }
}

fn transport_error(error: reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Network(error.to_string())
    }
}

impl Transport for ReqwestTransport {
    fn post(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        let builder = self.client.post(&request.url).body(request.body);
        send(builder, &request.headers)
    }

    fn get(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        send(self.client.get(&request.url), &request.headers)
    }
}

fn send(
    mut builder: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    Box::pin(async move {
        let response = builder.send().await.map_err(transport_error)?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let retry_after = header("retry-after-ms")
            .and_then(|ms| ms.trim().parse::<f64>().ok())
            .map(|ms| Duration::from_secs_f64(ms.max(0.0) / 1000.0))
            .or_else(|| {
                header("retry-after")
                    .and_then(|seconds| seconds.trim().parse::<f64>().ok())
                    .map(|seconds| Duration::from_secs_f64(seconds.max(0.0)))
            });
        let status = response.status().as_u16();
        let content_type = header("content-type");
        let body = stream::unfold(Some(response), |response| async move {
            let mut response = response?;
            match response.chunk().await {
                Ok(Some(bytes)) => Some((Ok(bytes.to_vec()), Some(response))),
                Ok(None) => None,
                Err(error) => Some((Err(transport_error(error)), None)),
            }
        })
        .boxed();
        Ok(HttpResponse {
            status,
            content_type,
            retry_after,
            body,
        })
    })
}
