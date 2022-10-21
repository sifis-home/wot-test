use std::{
    ops::{ControlFlow, Not},
    pin::Pin,
    task::{self, Poll},
};

use futures_util::{
    stream::{self, FusedStream},
    Stream, StreamExt,
};
use http_api_problem::HttpApiProblem;
use reqwest::{header::HeaderMap, Method, StatusCode};
use reqwest_eventsource::EventSource;
use serde::Deserialize;
use stable_eyre::eyre::{self, bail, ensure, eyre, Context};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::tester::ActionResponseStatus;

use super::{ActionResponse, Tester};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Stop,
    Overheated {
        global: bool,
        data: Overheated,
        timestamp: Option<OffsetDateTime>,
    },
    Brightness {
        global: bool,
        brightness: u32,
        timestamp: Option<OffsetDateTime>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PropertyStatus {
    Brightness(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionStatus {
    Fade(Fade),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", tag = "event")]
pub enum EventStatus {
    Overheated(Overheated),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fade {
    pub brightness: u32,
    pub duration: u32,
    #[serde(with = "time::serde::iso8601::option", default)]
    pub time_requested: Option<OffsetDateTime>,
    #[serde(with = "time::serde::iso8601::option", default)]
    pub time_completed: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct Overheated(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub inner: EventInner,
    pub timestamp: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventInner {
    Property(PropertyStatus),
    Event(EventStatus),
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct InvalidEvent;

impl TryFrom<&eventsource_stream::Event> for Event {
    type Error = InvalidEvent;

    fn try_from(event: &eventsource_stream::Event) -> Result<Self, Self::Error> {
        let timestamp = event
            .id
            .is_empty()
            .not()
            .then(|| match OffsetDateTime::parse(&event.id, &Rfc3339) {
                Ok(x) => Some(x),
                Err(_) => {
                    warn!(
                        "event id for \"{}\" is not an RFC3339 timestamp",
                        event.event
                    );
                    None
                }
            })
            .flatten();

        let inner = match &*event.event {
            "brightness" => {
                let data = serde_json::from_str(&event.data).map_err(|_| InvalidEvent)?;
                EventInner::Property(PropertyStatus::Brightness(data))
            }
            "overheated" => {
                let data = serde_json::from_str(&event.data).map_err(|_| InvalidEvent)?;
                EventInner::Event(EventStatus::Overheated(Overheated(data)))
            }
            _ => return Err(InvalidEvent),
        };

        Ok(Self { inner, timestamp })
    }
}

impl TryFrom<eventsource_stream::Event> for Event {
    type Error = InvalidEvent;

    #[inline]
    fn try_from(event: eventsource_stream::Event) -> Result<Self, Self::Error> {
        Self::try_from(&event)
    }
}

pub struct EventSources {
    events: stream::Fuse<EventSource>,
    overheat: stream::Fuse<EventSource>,
    properties: stream::Fuse<EventSource>,
    brightness: stream::Fuse<EventSource>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum EventType {
    Event,
    Overheat,
    Property,
    Brightness,
}

impl EventSources {
    pub(crate) fn new(tester: &Tester) -> eyre::Result<Self> {
        let events = EventSource::new(tester.create_request(Method::GET, "events"))
            .context("Unable to create events source for lamp")?
            .fuse();
        let overheat = EventSource::new(tester.create_request(Method::GET, "events/overheated"))
            .context("Unable to create overheated events source for lamp")?
            .fuse();
        let properties = EventSource::new(tester.create_request(Method::GET, "properties/observe"))
            .context("Unable to create properties events source for lamp")?
            .fuse();
        let brightness =
            EventSource::new(tester.create_request(Method::GET, "properties/brightness/observe"))
                .context("Unable to create brightness events source for lamp")?
                .fuse();

        Ok(Self {
            events,
            overheat,
            properties,
            brightness,
        })
    }
}

impl Stream for EventSources {
    type Item = Result<(EventType, reqwest_eventsource::Event), reqwest_eventsource::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut pending = false;

        macro_rules! ready {
            ($poll:expr) => {
                match $poll {
                    Poll::Ready(Some(x)) => return Poll::Ready(Some(x)),
                    Poll::Pending => pending = true,
                    Poll::Ready(None) => {}
                }
            };
        }

        ready!(Pin::new(&mut this.events)
            .poll_next(cx)
            .map_ok(|event| (EventType::Event, event)));

        ready!(Pin::new(&mut this.overheat)
            .poll_next(cx)
            .map_ok(|event| (EventType::Overheat, event)));

        ready!(Pin::new(&mut this.properties)
            .poll_next(cx)
            .map_ok(|event| (EventType::Property, event)));

        ready!(Pin::new(&mut this.brightness)
            .poll_next(cx)
            .map_ok(|event| (EventType::Brightness, event)));

        if pending {
            Poll::Pending
        } else {
            Poll::Ready(None)
        }
    }
}

impl FusedStream for EventSources {
    fn is_terminated(&self) -> bool {
        self.events.is_terminated()
            && self.overheat.is_terminated()
            && self.properties.is_terminated()
            && self.brightness.is_terminated()
    }
}

pub async fn handle_lamp_event_result<'a>(
    result: Result<(EventType, reqwest_eventsource::Event), reqwest_eventsource::Error>,
    sender: &mpsc::Sender<Message>,
) -> ControlFlow<eyre::Result<()>> {
    use ControlFlow::*;

    let (ty, event) = match result {
        Ok(event) => event,
        Err(err) => {
            error!("events stream closed prematurely: {err}");
            if sender.send(Message::Stop).await.is_err() {
                warn!("Trying to send a stop message to a closed channel");
            }

            return Break(Err(eyre!("events stream closed prematurely: {}", err)));
        }
    };

    match event {
        reqwest_eventsource::Event::Open => {
            info!("Event source connection opened for {ty:?}")
        }
        reqwest_eventsource::Event::Message(event) => {
            let event: Event = match event.try_into() {
                Ok(x) => x,
                Err(_) => {
                    error!("invalid lamp event");
                    return Break(Err(eyre!("invalid lamp event")));
                }
            };
            let timestamp = event.timestamp;
            let message = match (ty, event.inner) {
                (
                    EventType::Property | EventType::Brightness,
                    EventInner::Property(PropertyStatus::Brightness(brightness)),
                ) => {
                    let global = matches!(ty, EventType::Property);
                    Message::Brightness {
                        global,
                        brightness,
                        timestamp,
                    }
                }

                (
                    EventType::Event | EventType::Overheat,
                    EventInner::Event(EventStatus::Overheated(overheat)),
                ) => {
                    let global = matches!(ty, EventType::Event);
                    Message::Overheated {
                        global,
                        data: overheat,
                        timestamp,
                    }
                }

                _ => return Break(Err(eyre!("invalid lamp event type/message combination"))),
            };

            if sender.send(message).await.is_err() {
                return Break(Err(eyre!("lamp handler channel closed unexpectedly")));
            }
        }
    }

    Continue(())
}

#[derive(Clone, Debug, Default)]
pub enum PolledActionStatus {
    #[default]
    None,
    Pending {
        time_requested: Option<OffsetDateTime>,
    },
    Running {
        time_requested: Option<OffsetDateTime>,
    },
    Completed {
        output: Option<Fade>,
        time_requested: Option<OffsetDateTime>,
        time_ended: Option<OffsetDateTime>,
    },
    Failed {
        error: Option<HttpApiProblem>,
        time_requested: Option<OffsetDateTime>,
        time_ended: Option<OffsetDateTime>,
    },
}

impl PolledActionStatus {
    fn transition_to_pending(
        &mut self,
        time_requested: Option<OffsetDateTime>,
    ) -> eyre::Result<()> {
        match &*self {
            Self::None => {
                *self = Self::Pending { time_requested };
                Ok(())
            }

            &Self::Pending {
                time_requested: Some(old_time_requested),
            } if time_requested
                .map(|time_requested| old_time_requested == time_requested)
                .unwrap_or(true) =>
            {
                Ok(())
            }

            Self::Pending {
                time_requested: None,
            } => Ok(()),

            Self::Pending { .. } => {
                bail!("invalid Pending non-transition with a changing request time")
            }

            _ => bail!("invalid transition from {:?} to Pending", *self),
        }
    }

    fn transition_to_running(
        &mut self,
        time_requested: Option<OffsetDateTime>,
    ) -> eyre::Result<()> {
        match &*self {
            Self::None => {
                *self = Self::Running { time_requested };
                Ok(())
            }

            &Self::Pending {
                time_requested: Some(old_time_requested),
            }
            | &Self::Running {
                time_requested: Some(old_time_requested),
            } if time_requested
                .map(|time_requested| old_time_requested == time_requested)
                .unwrap_or(true) =>
            {
                *self = Self::Running { time_requested };
                Ok(())
            }

            Self::Pending {
                time_requested: None,
            }
            | Self::Running {
                time_requested: None,
            } => {
                *self = Self::Running { time_requested };
                Ok(())
            }

            Self::Pending { .. } => {
                bail!("invalid transition from Pending to Running with a changing request time")
            }

            Self::Running { .. } => {
                bail!("invalid Running non-transition with a changing request time")
            }

            _ => bail!("invalid transition from {:?} to Running", *self),
        }
    }

    fn transition_to_completed(
        &mut self,
        output: Option<Fade>,
        time_requested: Option<OffsetDateTime>,
        time_ended: Option<OffsetDateTime>,
    ) -> eyre::Result<()> {
        match &*self {
            Self::None => {
                *self = Self::Completed {
                    output,
                    time_requested,
                    time_ended,
                };
                Ok(())
            }

            &Self::Pending {
                time_requested: Some(old_time_requested),
            }
            | &Self::Running {
                time_requested: Some(old_time_requested),
            } if time_requested
                .map(|time_requested| old_time_requested == time_requested)
                .unwrap_or(true) =>
            {
                *self = Self::Completed {
                    output,
                    time_requested,
                    time_ended,
                };
                Ok(())
            }

            Self::Pending {
                time_requested: None,
            }
            | Self::Running {
                time_requested: None,
            } => {
                *self = Self::Completed {
                    output,
                    time_requested,
                    time_ended,
                };
                Ok(())
            }

            Self::Pending { .. } | Self::Running { .. } => {
                bail!("invalid transition from Pending/Running to Completed with a changing request time");
            }

            Self::Completed {
                output: old_output,
                time_requested: old_time_requested,
                time_ended: old_time_ended,
            } if old_output == &output
                && old_time_requested == &time_requested
                && old_time_ended == &time_ended =>
            {
                Ok(())
            }

            Self::Completed { .. } => {
                bail!("invalid Completed non-transition with changing fields")
            }

            Self::Failed { .. } => bail!("invalid transition from Failed to Completed"),
        }
    }

    fn transition_to_failed(
        &mut self,
        error: Option<HttpApiProblem>,
        time_requested: Option<OffsetDateTime>,
        time_ended: Option<OffsetDateTime>,
    ) -> eyre::Result<()> {
        match &*self {
            Self::None => {
                *self = Self::Failed {
                    error,
                    time_requested,
                    time_ended,
                };
                Ok(())
            }

            &Self::Pending {
                time_requested: Some(old_time_requested),
            }
            | &Self::Running {
                time_requested: Some(old_time_requested),
            } if time_requested
                .map(|time_requested| old_time_requested == time_requested)
                .unwrap_or(true) =>
            {
                *self = Self::Failed {
                    error,
                    time_requested,
                    time_ended,
                };
                Ok(())
            }

            Self::Pending {
                time_requested: None,
            }
            | Self::Running {
                time_requested: None,
            } => {
                *self = Self::Failed {
                    error,
                    time_requested,
                    time_ended,
                };
                Ok(())
            }

            Self::Pending { .. } | Self::Running { .. } => {
                bail!("invalid transition from Pending/Running to Failed with a changing request time");
            }

            Self::Failed {
                error: old_error,
                time_requested: old_time_requested,
                time_ended: old_time_ended,
            } if eq_opt_http_api_problem(old_error, &error)
                && old_time_requested == &time_requested
                && old_time_ended == &time_ended =>
            {
                Ok(())
            }

            Self::Failed { .. } => {
                bail!("invalid Failed non-transition with changing fields")
            }

            Self::Completed { .. } => bail!("invalid transition from Completed to Failed"),
        }
    }
}

fn eq_opt_http_api_problem(a: &Option<HttpApiProblem>, b: &Option<HttpApiProblem>) -> bool {
    match (a, b) {
        (None, None) => true,

        (Some(a), Some(b)) => {
            a.type_url == b.type_url
                && a.status == b.status
                && a.title == b.title
                && a.detail == b.detail
                && a.instance == b.instance
                && a.additional_fields() == b.additional_fields()
        }

        _ => false,
    }
}

pub(super) fn handle_next_polled_action(
    action: Option<eyre::Result<(StatusCode, HeaderMap, ActionResponse<Fade>)>>,
    action_status: &mut PolledActionStatus,
) -> eyre::Result<()> {
    let (status, _, action_response) = match action {
        Some(action) => action.context("error while polling fade action")?,
        None => bail!("lamp fade action poller finished unexpectedly"),
    };
    ensure!(
        status == StatusCode::OK,
        "lamp fade action poller got the status code {status} instead of OK"
    );

    match action_response.status {
        ActionResponseStatus::Pending => {
            ensure!(
                action_response.time_ended.is_none(),
                "lamp fade action in a pending state cannot have a time_ended field"
            );
            ensure!(
                action_response.error.is_none(),
                "lamp fade action in a pending state cannot have an error field"
            );
            ensure!(
                action_response.output.is_none(),
                "lamp fade action in a pending state cannot have an output field"
            );
            action_status.transition_to_pending(action_response.time_requested)
        }
        ActionResponseStatus::Running => {
            ensure!(
                action_response.time_ended.is_none(),
                "lamp fade action in a running state cannot have a time_ended field"
            );
            ensure!(
                action_response.error.is_none(),
                "lamp fade action in a running state cannot have an error field"
            );
            ensure!(
                action_response.output.is_none(),
                "lamp fade action in a running state cannot have an output field"
            );
            action_status.transition_to_running(action_response.time_requested)
        }
        ActionResponseStatus::Completed => {
            ensure!(
                action_response.error.is_none(),
                "lamp fade action in a completed state cannot have an error field"
            );
            action_status.transition_to_completed(
                action_response.output,
                action_response.time_requested,
                action_response.time_ended,
            )
        }
        ActionResponseStatus::Failed => {
            ensure!(
                action_response.output.is_none(),
                "lamp fade action in a running state cannot have an output field"
            );
            action_status.transition_to_failed(
                action_response.error,
                action_response.time_requested,
                action_response.time_ended,
            )
        }
    }
}
