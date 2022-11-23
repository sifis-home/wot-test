mod poller;

pub use poller::*;

use std::{fmt, future::IntoFuture, ops::Not};

use futures_util::{future::BoxFuture, FutureExt};
use http_api_problem::HttpApiProblem;
use reqwest::{header::HeaderMap, Client, Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::{select, sync::mpsc};

use crate::td::Form;

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
) -> Result<F::Output, IncomingMessageError<T>>
where
    F: IntoFuture,
{
    let fut = fut.into_future();
    select! {
        out = fut => {
            Ok(out)
        },

        received = receiver.recv() => {
            match received {
                Some(message) => Err(IncomingMessageError::UnexpectedMessage(message)),
                None => Err(IncomingMessageError::ChannelClosed),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum IncomingMessageError<T> {
    UnexpectedMessage(T),
    ChannelClosed,
}

impl<T> fmt::Display for IncomingMessageError<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedMessage(message) => write!(
                f,
                "Message from event handler received when nothing was expected: {message:?}"
            ),
            Self::ChannelClosed => f.write_str("Channel from event handler closed unexpectedly"),
        }
    }
}

impl<T> std::error::Error for IncomingMessageError<T> where T: fmt::Debug {}

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

    pub async fn json<T>(self) -> Result<(StatusCode, HeaderMap, T), reqwest::Error>
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
    type Output = Result<(StatusCode, HeaderMap), IntoFutureError>;
    type IntoFuture = BoxFuture<'static, Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        async move {
            let response = self.request.send().await?;
            let status = response.status();
            let headers = response.headers().clone();
            let output = response.bytes().await?;

            if output.is_empty().not() {
                let output = String::from_utf8_lossy(&output);
                return Err(IntoFutureError::NotEmpty(output.into_owned()));
            }
            Ok((status, headers))
        }
        .boxed()
    }
}

#[derive(Debug)]
pub enum IntoFutureError {
    Reqwest(reqwest::Error),
    NotEmpty(String),
}

impl From<reqwest::Error> for IntoFutureError {
    fn from(error: reqwest::Error) -> Self {
        Self::Reqwest(error)
    }
}

impl fmt::Display for IntoFutureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reqwest(reqwest) => write!(f, "Reqwest error: {reqwest}"),
            Self::NotEmpty(s) => write!(f, "Expected empty response, obtained: {s}"),
        }
    }
}

impl std::error::Error for IntoFutureError {}

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

pub async fn test_property<'a, R, T>(
    requester: &R,
    form: &Form,
    value: &'a T,
) -> Result<(), TestPropertyError<'a, T>>
where
    R: Requester,
    T: Serialize + Sized + PartialEq + fmt::Debug + for<'de> Deserialize<'de>,
{
    let (status, _) = requester
        .request_with_json(Method::PUT, &form.href, value)
        .await?;
    if status != StatusCode::NO_CONTENT {
        return Err(TestPropertyError::IncorrectPutStatus(status));
    }

    let (status, _, read) = requester
        .request(Method::GET, &form.href)
        .json::<T>()
        .await?;
    if status != StatusCode::OK {
        return Err(TestPropertyError::IncorrectGetStatus(status));
    }

    if read == *value {
        Ok(())
    } else {
        Err(TestPropertyError::IncorrectValue {
            expected: value,
            found: read,
        })
    }
}

#[derive(Debug)]
pub enum TestPropertyError<'a, T> {
    IncorrectPutStatus(StatusCode),
    IncorrectGetStatus(StatusCode),
    IncorrectValue { expected: &'a T, found: T },
    Reqwest(reqwest::Error),
    NotEmpty(String),
}

impl<T> fmt::Display for TestPropertyError<'_, T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncorrectPutStatus(status) => {
                write!(
                    f,
                    "Expected NO_CONTENT status code from PUT request, got {status}"
                )
            }
            Self::IncorrectGetStatus(status) => {
                write!(f, "Expected OK status code from GET request, got {status}")
            }
            Self::IncorrectValue { expected, found } => {
                write!(
                    f,
                    "Value not set correctly, got {found:?} instead of {expected:?}"
                )
            }
            Self::Reqwest(error) => write!(f, "Reqwest error: {error}"),
            Self::NotEmpty(s) => {
                write!(f, "Expected empty response from PUT request, obtained: {s}")
            }
        }
    }
}

impl<T> std::error::Error for TestPropertyError<'_, T> where T: fmt::Debug {}

impl<T> From<reqwest::Error> for TestPropertyError<'_, T> {
    fn from(error: reqwest::Error) -> Self {
        Self::Reqwest(error)
    }
}

impl<T> From<IntoFutureError> for TestPropertyError<'_, T> {
    fn from(error: IntoFutureError) -> Self {
        match error {
            IntoFutureError::Reqwest(x) => Self::Reqwest(x),
            IntoFutureError::NotEmpty(x) => Self::NotEmpty(x),
        }
    }
}
