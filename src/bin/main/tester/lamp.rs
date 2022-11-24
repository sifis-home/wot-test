use std::{
    convert::Infallible,
    ops::{ControlFlow, Not},
    pin::Pin,
    task::{self, Poll},
    time::Duration,
};

use futures_util::{
    pin_mut,
    stream::{self, FusedStream},
    Stream, StreamExt,
};
use http_api_problem::HttpApiProblem;
use reqwest::{
    header::{HeaderMap, LOCATION},
    Method, StatusCode,
};
use reqwest_eventsource::EventSource;
use serde::{Deserialize, Serialize};
use stable_eyre::eyre::{self, bail, ensure, eyre, Context};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::{
    join, select,
    sync::{mpsc, oneshot},
    time::{sleep, timeout},
};
use tracing::{error, info, warn};
use wot_test::{
    check_no_incoming_messages,
    td::ThingDescription,
    tester::{ActionResponse, ActionResponseStatus, Requester},
};

use crate::lamp::Brightness;

use super::Tester;

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
    fn new(tester: &Tester) -> eyre::Result<Self> {
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

type ActionToHandle = Option<Result<(StatusCode, HeaderMap, ActionResponse<Fade>), reqwest::Error>>;

pub(super) fn handle_next_polled_action(
    action: ActionToHandle,
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

pub(crate) async fn run_test(tester: &Tester) -> eyre::Result<()> {
    let (sender, receiver) = mpsc::channel(10);
    let (stopper_sender, stopper_receiver) = oneshot::channel();

    let results = join!(
        test_lamp_active(tester, receiver, stopper_sender),
        test_lamp_events(tester, sender, stopper_receiver)
    );

    results.0.and(results.1)
}

async fn test_lamp_active(
    tester: &Tester,
    mut receiver: mpsc::Receiver<Message>,
    stopper_sender: oneshot::Sender<oneshot::Sender<()>>,
) -> eyre::Result<()> {
    let no_messages_checks = async {
        let (status, _, lamp) = tester
            .request(Method::GET, ".well-known/wot")
            .json::<ThingDescription>()
            .await?;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        ensure!(
            lamp == *crate::lamp::DESCRIPTION,
            "Invalid lamp. Expected:\n{:#?}\nObtained:\n{:#?}",
            *crate::lamp::DESCRIPTION,
            lamp,
        );
        info!("Got lamp TD from .well-known/wot");

        #[derive(Debug, Eq, PartialEq, Deserialize)]
        struct Props {
            brightness: crate::lamp::Brightness,
            on: crate::lamp::Status,
        }

        let (status, _, props) = tester
            .request(Method::GET, "properties")
            .json::<Props>()
            .await?;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        const EXPECTED_PROPS: Props = Props {
            brightness: 50,
            on: true,
        };
        ensure!(
            props == EXPECTED_PROPS,
            "Invalid lamp props. Expected:\n{:#?}\nObtained:\n{:#?}",
            EXPECTED_PROPS,
            props,
        );
        info!("Lamp is correctly on with brightness 50");

        let (status, _, brightness) = tester
            .request(Method::GET, "properties/brightness")
            .json::<Brightness>()
            .await?;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        ensure!(
            brightness == 50,
            "Invalid lamp brightness. Expected 50, got {brightness}",
        );
        info!("Lamp brightness is 50 as expected");

        Ok(())
    };
    check_no_incoming_messages(no_messages_checks, &mut receiver).await??;

    let brightness_set_fut = async {
        let (status, _) = tester
            .request_with_json(Method::PUT, "properties/brightness", &25)
            .await?;
        ensure!(
            status == StatusCode::NO_CONTENT,
            "Expected NO_CONTENT status code, got {status}",
        );
        Ok(())
    };

    let mut got_brightness_event = false;
    let brightness_event_fut = async {
        loop {
            match receiver.recv().await {
                Some(Message::Brightness {
                    brightness,
                    timestamp,
                    ..
                }) => {
                    if brightness != 25 || timestamp.is_none() {
                        break Err::<Infallible, eyre::Report>(eyre!(
                            "invalid lamp brightness status event"
                        ));
                    }
                    got_brightness_event = true;
                }
                Some(message) => {
                    bail!("unexpected event from lamp when setting brightness: {message:?}")
                }
                None => bail!("lamp events stream closed unexpectedly"),
            }
        }
    };

    match join!(
        brightness_set_fut,
        timeout(Duration::from_millis(50), brightness_event_fut)
    ) {
        (Ok(()), Err(_)) => {}
        (Err(err), _) | (Ok(_), Ok(Err(err))) => return Err(err),
        (Ok(()), Ok(Ok(_infallible))) => unreachable!(),
    }

    if got_brightness_event.not() {
        bail!("one or more brightness event is expected after the value has been set");
    }

    info!("Lamp brightness successfully set to 25 and relative events have been received");

    let (status, _, brightness) = check_no_incoming_messages(
        tester
            .request(Method::GET, "properties/brightness")
            .json::<Brightness>(),
        &mut receiver,
    )
    .await??;
    ensure!(
        status == StatusCode::OK,
        "Expected OK status code, got {status}",
    );
    ensure!(
        brightness == 25,
        "Invalid lamp brightness. Expected 25, got {brightness}",
    );
    info!("Lamp brightness is 25 as expected");

    #[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct FadeAction {
        brightness: u32,
        duration: u32,
    }
    const OH_FADE: FadeAction = FadeAction {
        brightness: 100,
        duration: 2000,
    };

    let (status, headers, output): (_, _, ActionResponse<FadeAction>) = tester
        .request_with_json(Method::POST, "actions/fade", &OH_FADE)
        .json()
        .await?;
    ensure!(
        status == StatusCode::CREATED,
        "Expected CREATED status code, got {status}",
    );
    ensure!(
        output.error.is_none(),
        "Lamp fade action unexpectedly returned an error"
    );
    ensure!(
        matches!(
            output.status,
            ActionResponseStatus::Running | ActionResponseStatus::Pending
        ),
        "Invalid lamp fade action response. Expected status \"running\" or \"pending\", received {:?}",
        output.status
    );
    ensure!(
        output.output == Some(OH_FADE),
        "Invalid lamp fade action response. Expected output {OH_FADE:?}, got {:?}",
        output.output
    );
    let fade_href = headers
        .get(LOCATION)
        .ok_or(eyre!(
            "Lamp fade action should respond with a 'Location' header"
        ))?
        .to_str()?;
    ensure!(
        fade_href.starts_with("/actions/fade/"),
        "Lamp fade action location should start with '/actions/fade/'",
    );
    if let Some(body_href) = output.href.as_deref() {
        ensure!(
            body_href == fade_href,
            "Lamp fade action href in body must be coherent with location header",
        );
    }
    info!("Lamp fade action started successfully");

    let (status, _, output): (_, _, ActionResponse<FadeAction>) =
        tester.request(Method::GET, fade_href).json().await?;
    ensure!(
        status == StatusCode::OK,
        "Expected OK status code, got {status}",
    );
    ensure!(
        matches!(
            output.status,
            ActionResponseStatus::Running | ActionResponseStatus::Pending
        ),
        "Fade action should be running or pending after being created",
    );
    info!("Lamp fade action is pending as expected");

    let receive_events_fut = async {
        let mut received_overheating = false;
        let mut received_overheating_global = false;
        let mut last_global_brightness = None;
        let mut last_brightness = None;

        loop {
            match receiver.recv().await {
                Some(Message::Overheated {
                    global,
                    data: Overheated(data),
                    timestamp,
                }) => {
                    if data <= 100 || timestamp.is_none() {
                        break Err(eyre!("invalid lamp overheated event"));
                    }

                    if global {
                        received_overheating_global = true;
                    } else {
                        received_overheating = true;
                    }
                }
                Some(Message::Brightness {
                    global,
                    brightness,
                    timestamp,
                }) => {
                    if (25..=OH_FADE.brightness).contains(&brightness).not() || timestamp.is_none()
                    {
                        break Err(eyre!("invalid lamp brightness status event"));
                    }

                    if global {
                        last_global_brightness = Some(brightness);
                    } else {
                        last_brightness = Some(brightness);
                    }
                }
                None | Some(Message::Stop) => {
                    break Err(eyre!("lamp event handler closed prematurely"))
                }
            }

            if received_overheating
                && received_overheating_global
                && last_brightness == Some(OH_FADE.brightness)
                && last_global_brightness == Some(OH_FADE.brightness)
            {
                break Ok(());
            }
        }
    };

    let check_brightness_get_fut = async {
        sleep(Duration::from_secs(1)).await;
        let interpolate_brightness_at_t =
            |millis| (i64::from(OH_FADE.brightness) - 25) * millis / 2000 + 25;
        let expected_range = interpolate_brightness_at_t(900)..=interpolate_brightness_at_t(1100);

        let (status, _, brightness) = tester
            .request(Method::GET, "properties/brightness")
            .json::<Brightness>()
            .await?;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        ensure!(
            expected_range.contains(&brightness),
            "Invalid lamp brightness after 1s of fade. Expected between {} and {}, got {brightness}",
            expected_range.start(), expected_range.end()
        );
        Ok(())
    };

    let timeout_duration = Duration::from_millis(2100);
    let fade_action_poller = lamp_fade_action_poller(tester, timeout_duration, fade_href);

    let send_stop = || async {
        if stopper_sender.is_closed().not() {
            let (sync_sender, sync_receiver) = oneshot::channel();
            match stopper_sender.send(sync_sender) {
                Ok(()) => {
                    if let Err(err) = sync_receiver.await {
                        warn!("unable to receive sync message back from stop: {err}");
                    }
                }
                Err(_) => warn!("trying to send a stop message to an already closed channel"),
            }
        }
    };

    match join!(
        fade_action_poller,
        check_brightness_get_fut,
        timeout(timeout_duration, receive_events_fut)
    ) {
        (Ok(()), Ok(()), Ok(Ok(()))) => {}
        (Err(err), _, _) | (_, Err(err), _) | (_, _, Ok(Err(err))) => {
            error!("error while waiting for lamp events: {err}");
            send_stop().await;
            bail!("error while waiting for lamp events: {err}");
        }
        (Ok(()), Ok(()), Err(_)) => {
            error!("not all the expected events are received from the lamp");
            send_stop().await;
            bail!("not all the expected events are received from the lamp");
        }
    }

    info!("Restoring initial lamp state");

    let (status, _) = tester
        .request_with_json(Method::PUT, "properties/brightness", &50)
        .await?;
    if status != StatusCode::NO_CONTENT {
        warn!("expected no content when setting the brightness of the lamp");
    }

    send_stop().await;
    Ok(())
}

async fn test_lamp_events(
    tester: &Tester,
    sender: mpsc::Sender<Message>,
    mut stopper_receiver: oneshot::Receiver<oneshot::Sender<()>>,
) -> eyre::Result<()> {
    use ControlFlow::*;

    let event_sources =
        EventSources::new(tester)?.then(|result| handle_lamp_event_result(result, &sender));
    pin_mut!(event_sources);

    // Cancel safety for underlying EventSource is unknown
    let mut next_event_future = event_sources.next();

    loop {
        select! {
            maybe_control_flow = &mut next_event_future => {
                match maybe_control_flow {
                    Some(Continue(())) => {
                        next_event_future = event_sources.next();
                    }
                    Some(Break(result)) => {
                        break result;
                    },
                    None => {
                        error!("events stream closed prematurely");
                        if sender.send(Message::Stop).await.is_err() {
                            warn!("Trying to send a stop message to a closed channel");
                        }
                        break Err(eyre!("events stream closed prematurely"));
                    }
                }
            },
            sync_sender_result = &mut stopper_receiver => {
                match sync_sender_result {
                    Ok(sync_sender) => if sync_sender.send(()).is_err() {
                        warn!("unable to send sync message back through stop channel");
                    },
                    Err(_) => warn!("stop channel closed unexpectedly"),
                }
                break Ok(());
            }
        };
    }
}

async fn lamp_fade_action_poller(
    tester: &Tester,
    timeout_duration: Duration,
    action_href: &str,
) -> eyre::Result<()> {
    let poll_fn = || async move {
        tester
            .request(Method::GET, action_href)
            .json::<ActionResponse<Fade>>()
            .await
    };

    let poller = wot_test::poll(Duration::from_millis(100), poll_fn);
    let timeout = tokio::time::sleep(timeout_duration);
    pin_mut!(poller);
    pin_mut!(timeout);

    let mut status = PolledActionStatus::default();
    let mut polled_next = poller.next();
    loop {
        select! {
            action = &mut polled_next => {
                handle_next_polled_action(action, &mut status)?
            }

            _ = &mut timeout => {
                match status {
                    PolledActionStatus::Completed { .. } => {
                        break Ok(())
                    },
                    _ => {
                        error!("lamp fade action not finished before timeout");
                        break Err(eyre!("lamp fade action not finished before timeout"))
                    }
                }
            }
        }
    }
}
