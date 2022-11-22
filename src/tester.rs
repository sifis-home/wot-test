mod poller;

pub use poller::*;

use std::{fmt, future::IntoFuture, ops::Not};

use futures_util::{future::BoxFuture, FutureExt};
use http_api_problem::HttpApiProblem;
use reqwest::{header::HeaderMap, Client, Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use stable_eyre::eyre::{self, bail, eyre};
use time::OffsetDateTime;
use tokio::{select, sync::mpsc};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResponse<T> {
    pub status: ActionResponseStatus,
    pub output: Option<T>,
    pub error: Option<HttpApiProblem>,
    pub href: Option<String>,
    #[serde(with = "time::serde::iso8601::option", default)]
    pub time_requested: Option<OffsetDateTime>,
    #[serde(with = "time::serde::iso8601::option", default)]
    pub time_ended: Option<OffsetDateTime>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionResponseStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

pub async fn check_no_incoming_messages<T, F>(
    fut: F,
    receiver: &mut mpsc::Receiver<T>,
) -> eyre::Result<F::Output>
where
    T: fmt::Debug,
    F: IntoFuture,
{
    let fut = fut.into_future();
    select! {
        out = fut => {
            Ok(out)
        },

        received = receiver.recv() => {
            match received {
                Some(message) => Err(eyre!(
                    "Message from event handler received when nothing was expected: {message:?}"
                )),
                None => Err(eyre!("Channel from event handler closed unexpectedly")),
            }
        }
    }
}

pub struct RequestHandler {
    request: RequestBuilder,
}

impl RequestHandler {
    #[inline]
    pub fn new(request_builder: RequestBuilder) -> Self {
        Self {
            request: request_builder,
        }
    }

    pub async fn json<T>(self) -> eyre::Result<(StatusCode, HeaderMap, T)>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self.request.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let response = response.json().await?;

        Ok((status, headers, response))
    }
}

impl IntoFuture for RequestHandler {
    type Output = eyre::Result<(StatusCode, HeaderMap)>;
    type IntoFuture = BoxFuture<'static, Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        async move {
            let response = self.request.send().await?;
            let status = response.status();
            let headers = response.headers().clone();
            let output = response.bytes().await?;

            if output.is_empty().not() {
                let output = String::from_utf8_lossy(&output);
                bail!("expected empty response, obtained: {output}");
            }
            Ok((status, headers))
        }
        .boxed()
    }
}

pub trait Requester {
    fn host(&self) -> &str;
    fn port(&self) -> Option<u16>;
    fn client(&self) -> &Client;

    #[inline]
    fn request(&self, method: Method, endpoint: &str) -> RequestHandler {
        let request = self.create_request(method, endpoint);
        RequestHandler::new(request)
    }

    #[inline]
    fn request_with_json<T>(&self, method: Method, endpoint: &str, body: &T) -> RequestHandler
    where
        T: Serialize + Sized,
    {
        let request = self.create_request(method, endpoint).json(body);
        RequestHandler::new(request)
    }

    fn create_request(&self, method: Method, endpoint: &str) -> RequestBuilder {
        use fmt::Write;

        let mut url = format!("http://{}", self.host());
        if let Some(port) = self.port() {
            write!(url, ":{}", port).unwrap();
        }
        write!(url, "/{}", endpoint.trim_start_matches('/')).unwrap();

        self.client().request(method, url)
    }
}
