mod lamp;
mod poller;

use std::{
    borrow::Cow,
    convert::Infallible,
    fmt,
    future::IntoFuture,
    ops::{ControlFlow, Not},
    time::Duration,
};

use futures_util::{future::BoxFuture, pin_mut, FutureExt, StreamExt};
use http_api_problem::HttpApiProblem;
use reqwest::{
    header::{HeaderMap, LOCATION},
    Client, Method, RequestBuilder, StatusCode,
};
use serde::{Deserialize, Serialize};
use stable_eyre::eyre::{self, bail, ensure, eyre};
use time::OffsetDateTime;
use tokio::{
    join, select,
    sync::{mpsc, oneshot},
    time::{sleep, timeout},
};
use tracing::{error, info, warn};

use crate::{
    lamp::Brightness,
    td::{
        DataSchemaSubtype, Form, IntegerDataSchema, NumberDataSchema, Operation, ThingDescription,
    },
    tester::lamp::handle_lamp_event_result,
    WotTest,
};

pub(crate) struct Tester {
    host: Cow<'static, str>,
    port: Option<u16>,
    client: Client,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ActionResponse<T> {
    status: ActionResponseStatus,
    output: Option<T>,
    error: Option<HttpApiProblem>,
    href: Option<String>,
    #[serde(with = "time::serde::iso8601::option", default)]
    time_requested: Option<OffsetDateTime>,
    #[serde(with = "time::serde::iso8601::option", default)]
    time_ended: Option<OffsetDateTime>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActionResponseStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl Tester {
    pub(crate) fn new(host: Cow<'static, str>, port: Option<u16>) -> Self {
        let client = Client::new();

        Self { host, port, client }
    }

    pub(crate) async fn test(&self, test: WotTest) -> eyre::Result<()> {
        match test {
            WotTest::Lamp => self.test_lamp().await,
            WotTest::OnOffSwitch => self.test_on_off_switch().await,
            WotTest::LightEmitter => self.test_light_emitter().await,
        }
    }

    pub(crate) async fn test_lamp(&self) -> eyre::Result<()> {
        let (sender, receiver) = mpsc::channel(10);
        let (stopper_sender, stopper_receiver) = oneshot::channel();

        let results = join!(
            self.test_lamp_active(receiver, stopper_sender),
            self.test_lamp_events(sender, stopper_receiver)
        );

        results.0.and(results.1)
    }

    async fn test_lamp_active(
        &self,
        mut receiver: mpsc::Receiver<lamp::Message>,
        stopper_sender: oneshot::Sender<oneshot::Sender<()>>,
    ) -> eyre::Result<()> {
        use lamp::*;

        macro_rules! check_no_incoming_messages {
            ($fut:expr) => {
                check_no_incoming_messages($fut, &mut receiver)
            };
        }

        let (status, _, lamp) = check_no_incoming_messages!(self
            .request(Method::GET, ".well-known/wot")
            .json::<ThingDescription>())
        .await??;
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

        let (status, _, props) =
            check_no_incoming_messages!(self.request(Method::GET, "properties").json::<Props>())
                .await??;
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

        let (status, _, brightness) = check_no_incoming_messages!(self
            .request(Method::GET, "properties/brightness")
            .json::<Brightness>())
        .await??;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        ensure!(
            brightness == 50,
            "Invalid lamp brightness. Expected 50, got {brightness}",
        );
        info!("Lamp brightness is 50 as expected");

        let brightness_set_fut = async {
            let (status, _) = self
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

        let (status, _, brightness) = check_no_incoming_messages!(self
            .request(Method::GET, "properties/brightness")
            .json::<Brightness>())
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

        let (status, headers, output): (_, _, ActionResponse<FadeAction>) = self
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
            self.request(Method::GET, fade_href).json().await?;
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
                        if (25..=OH_FADE.brightness).contains(&brightness).not()
                            || timestamp.is_none()
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
            let expected_range =
                interpolate_brightness_at_t(900)..=interpolate_brightness_at_t(1100);

            let (status, _, brightness) = self
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
        let fade_action_poller = self.lamp_fade_action_poller(timeout_duration, fade_href);

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

        let (status, _) = self
            .request_with_json(Method::PUT, "properties/brightness", &50)
            .await?;
        if status != StatusCode::NO_CONTENT {
            warn!("expected no content when setting the brightness of the lamp");
        }

        send_stop().await;
        Ok(())
    }

    async fn test_lamp_events(
        &self,
        sender: mpsc::Sender<lamp::Message>,
        mut stopper_receiver: oneshot::Receiver<oneshot::Sender<()>>,
    ) -> eyre::Result<()> {
        use ControlFlow::*;

        let event_sources =
            lamp::EventSources::new(self)?.then(|result| handle_lamp_event_result(result, &sender));
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
                            if sender.send(lamp::Message::Stop).await.is_err() {
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
        &self,
        timeout_duration: Duration,
        action_href: &str,
    ) -> eyre::Result<()> {
        let poll_fn = || async move {
            self.request(Method::GET, action_href)
                .json::<ActionResponse<lamp::Fade>>()
                .await
        };

        let poller = poller::poll(Duration::from_millis(100), poll_fn);
        let timeout = tokio::time::sleep(timeout_duration);
        pin_mut!(poller);
        pin_mut!(timeout);

        let mut status = lamp::PolledActionStatus::default();
        let mut polled_next = poller.next();
        loop {
            select! {
                action = &mut polled_next => {
                    lamp::handle_next_polled_action(action, &mut status)?
                }

                _ = &mut timeout => {
                    match status {
                        lamp::PolledActionStatus::Completed { .. } => {
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

    pub(crate) async fn test_on_off_switch(&self) -> eyre::Result<()> {
        let (status, _, thing) = self
            .request(Method::GET, ".well-known/wot")
            .json::<ThingDescription>()
            .await?;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        let on_form = self.get_on_off_form(&thing).await?;

        info!("Got on-off-switch TD from .well-known/wot");

        self.test_on_off_switch_inner(&thing, on_form).await
    }

    async fn test_on_off_switch_inner(
        &self,
        thing: &ThingDescription,
        on_form: &Form,
    ) -> eyre::Result<()> {
        let turn_off = || async {
            let (status, _) = self
                .request_with_json(Method::PUT, &on_form.href, &false)
                .await?;
            ensure!(
                status == StatusCode::NO_CONTENT,
                "Expected NO_CONTENT status code, got {status}",
            );

            let (status, _, is_on) = self
                .request(Method::GET, &on_form.href)
                .json::<bool>()
                .await?;
            ensure!(
                status == StatusCode::OK,
                "Expected OK status code, got {status}",
            );
            ensure!(
                is_on.not(),
                "On-off-switch thing is expected to be off but it is on",
            );
            info!("On-off-switch thing is off as expected");

            Ok(())
        };

        let turn_on = || async {
            let (status, _) = self
                .request_with_json(Method::PUT, &on_form.href, &true)
                .await?;
            ensure!(
                status == StatusCode::NO_CONTENT,
                "Expected NO_CONTENT status code, got {status}",
            );

            let (status, _, is_on) = self
                .request(Method::GET, &on_form.href)
                .json::<bool>()
                .await?;
            ensure!(
                status == StatusCode::OK,
                "Expected OK status code, got {status}",
            );
            ensure!(
                is_on,
                "On-off-switch thing is expected to be on but it is off",
            );
            info!("On-off-switch thing is on as expected");

            Ok(())
        };

        let (status, _, initial_is_on) = self
            .request(Method::GET, &on_form.href)
            .json::<bool>()
            .await?;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );

        if initial_is_on {
            turn_off().await?;
            turn_on().await?;
        } else {
            turn_on().await?;
            turn_off().await?;
        }

        if let Some(toggle_action) = thing
            .actions
            .values()
            .find(|action| &*action.interaction.human_readable.attype == "ToggleAction")
        {
            info!("Found ToggleAction on On-Off-switch thing, testing it");

            let invoke_action = toggle_action
                .interaction
                .interaction
                .forms
                .iter()
                .find(|form| {
                    form.subprotocol.is_none()
                        && form
                            .op
                            .as_ref()
                            .map(|ops| ops.contains(&Operation::InvokeAction))
                            .unwrap_or(true)
                })
                .ok_or(eyre!("Unable to find invoke action form for Toggle Action"))?;
            ensure!(
                toggle_action.synchronous == Some(true),
                "Expected toggle action synchronous to be true",
            );
            ensure!(
                toggle_action.input.is_none(),
                "Expected toggle action input to be null or undefined",
            );
            ensure!(
                toggle_action.output.is_none(),
                "Expected toggle action output to be null or undefined",
            );

            let toggle = || async {
                let (status, _) = self.request(Method::POST, &invoke_action.href).await?;
                ensure!(
                    status == StatusCode::OK,
                    "Expected OK status code, got {status}",
                );

                Ok(())
            };

            toggle().await?;
            let (status, _, is_on) = self
                .request(Method::GET, &on_form.href)
                .json::<bool>()
                .await?;
            ensure!(
                status == StatusCode::OK,
                "Expected OK status code, got {status}",
            );
            ensure!(
                is_on != initial_is_on,
                "Expected toggle action to switch the on/off status"
            );

            toggle().await?;
            let (status, _, is_on) = self
                .request(Method::GET, &on_form.href)
                .json::<bool>()
                .await?;
            ensure!(
                status == StatusCode::OK,
                "Expected OK status code, got {status}",
            );
            ensure!(
                is_on == initial_is_on,
                "Expected toggle action to restore the on/off status"
            );
        }

        Ok(())
    }

    async fn get_on_off_form<'a>(&self, thing: &'a ThingDescription) -> eyre::Result<&'a Form> {
        ensure!(
            thing.attype.contains(&"OnOffSwitch".into()),
            "An on-off switch requires an \"OnOffSwitch\" @type",
        );
        let property = thing
            .properties
            .values()
            .find(|property| &*property.human_readable.attype == "OnOffProperty")
            .ok_or(eyre!("Missing OnOffProperty type from on-off-switch thing"))?;

        let form = property
            .interaction
            .forms
            .iter()
            .find(|form| {
                form.subprotocol.is_none()
                    && form
                        .op
                        .as_ref()
                        .map(|op| {
                            op.contains(&Operation::ReadProperty)
                                && op.contains(&Operation::WriteProperty)
                        })
                        .unwrap_or(true)
            })
            .ok_or(eyre!(
                "Missing ReadProperty+WriteProperty form for on-off switch property"
            ))?;
        ensure!(
            matches!(property.data_schema.subtype, DataSchemaSubtype::Boolean),
            "On-off switch data schema must be a boolean type",
        );
        ensure!(
            property.data_schema.unit.is_empty(),
            "On-off switch data schema must have a nullable/empty unit",
        );
        ensure!(
            matches!(property.data_schema.read_only, None | Some(false)),
            "On-off switch data schema must have a null, undefined or false \"read_only\" field",
        );
        ensure!(
            matches!(property.data_schema.write_only, None | Some(false)),
            "On-off switch data schema must have a null, undefined or false \"write_only\" field",
        );

        Ok(form)
    }

    pub(crate) async fn test_light_emitter(&self) -> eyre::Result<()> {
        let (status, _, thing) = self
            .request(Method::GET, ".well-known/wot")
            .json::<ThingDescription>()
            .await?;
        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        let on_form = self.get_on_off_form(&thing).await?;

        info!("Got on-off-switch TD from .well-known/wot that it could be a light emitter");

        if thing.attype.contains(&"Light".into()) {
            info!("On-off switch is also a Light");
        }

        self.test_on_off_switch_inner(&thing, on_form).await?;

        let brightness_property = thing
            .properties
            .values()
            .find(|property| &*property.human_readable.attype == "BrightnessProperty");

        let brightness_form_and_uses_integers = brightness_property
            .map(|brightness_property| {
                info!("On-off switch has a brightness property");

                let brightness_form = brightness_property
                    .interaction
                    .forms
                    .iter()
                    .find(|form| {
                        form.subprotocol.is_none()
                            && form
                                .op
                                .as_ref()
                                .map(|ops| {
                                    ops.contains(&Operation::ReadProperty)
                                        && ops.contains(&Operation::WriteProperty)
                                })
                                .unwrap_or(true)
                    })
                    .ok_or(eyre!(
                        "Brightness property of an on-off switch must have a \
                     ReadProperty+WriteProperty form operation"
                    ))?;

                let uses_integers = match &brightness_property.data_schema.subtype {
                    DataSchemaSubtype::Integer(IntegerDataSchema {
                        minimum: Some(0),
                        maximum: Some(100),
                    }) => true,

                    &DataSchemaSubtype::Number(NumberDataSchema {
                        minimum: Some(minimum),
                        maximum: Some(maximum),
                    }) if minimum == 0. && maximum == 100. => false,

                    _ => bail!(
                        "Brightness property of an on-off switch must have a valid data schema \
                         integer/number subtype"
                    ),
                };

                ensure!(
                    &*brightness_property.data_schema.unit == "percent",
                    "Brightness property of an on-off switch must have the unit field set to \
                 \"percent\" in data schema",
                );
                ensure!(
                    matches!(
                        brightness_property.data_schema.read_only,
                        None | Some(false)
                    ),
                    "Brightness property of an on-off switch must have a null, undefined or false \
                 \"read_only\" field in data schema",
                );
                ensure!(
                    matches!(
                        brightness_property.data_schema.write_only,
                        None | Some(false)
                    ),
                    "Brightness property of an on-off switch must have a null, undefined or false \
                 \"write_only\" field in data schema",
                );

                Ok((brightness_form, uses_integers))
            })
            .transpose()?;

        let fade_action = thing
            .actions
            .values()
            .find(|action| &*action.interaction.human_readable.attype == "FadeAction");

        let fade_form = fade_action
            .map(|fade_action| {
                info!("On-off switch has a fade action");

                let invoke_form = fade_action
                    .interaction
                    .interaction
                    .forms
                    .iter()
                    .find(|form| {
                        form.subprotocol.is_none()
                            && form
                                .op
                                .as_ref()
                                .map(|ops| ops.contains(&Operation::InvokeAction))
                                .unwrap_or(true)
                    })
                    .ok_or(eyre!(
                        "On-off switch fade action must have an invokable form"
                    ))?;

                let input = fade_action
                    .input
                    .as_ref()
                    .ok_or(eyre!("On-off switch fade action must have an input field"))?;
                ensure!(
                    fade_action.output.is_none(),
                    "On-off switch fade action output must be null or undefined",
                );

                let DataSchemaSubtype::Object(input_subtype) = &input.data_schema.subtype else {
                    bail!(
                        "On-off switch fade action input must have an object type of data schema",
                    );
                };

                let level = input_subtype
                    .properties
                    .iter()
                    .find(|&(name, _)| &**name == "level")
                    .ok_or(eyre!(
                        "On-off switch fade action must have a level property in the input \
                         data schema",
                    ))?
                    .1;

                ensure!(
                    matches!(
                        level.data_schema.subtype,
                        DataSchemaSubtype::Integer(IntegerDataSchema {
                            minimum: Some(0),
                            maximum: Some(100),
                        }),
                    ),
                    "On-off switch fade action level input data schema property has an \
                     invalid data schema subtype",
                );

                ensure!(
                    &*level.data_schema.unit == "percent",
                    "On-off switch fade action level input data schema property must have the \
                     unit field set to \"percent\" in data schema",
                );
                ensure!(
                    matches!(level.data_schema.read_only, None | Some(false)),
                    "On-off switch fade action level input data schema property must have a \
                     null, undefined or false \"read_only\" field in data schema",
                );
                ensure!(
                    matches!(level.data_schema.write_only, None | Some(false)),
                    "On-off switch fade action level input data schema property must have a \
                     null, undefined or false \"write_only\" field in data schema",
                );

                let duration = input_subtype
                    .properties
                    .iter()
                    .find(|&(name, _)| &**name == "duration")
                    .ok_or(eyre!(
                        "On-off switch fade action must have a duration property in the input \
                         data schema",
                    ))?
                    .1;

                ensure!(
                    matches!(
                        duration.data_schema.subtype,
                        DataSchemaSubtype::Integer(IntegerDataSchema {
                            minimum: Some(0),
                            ..
                        }),
                    ),
                    "On-off switch fade action duration input data schema property has an \
                     invalid data schema subtype",
                );

                ensure!(
                    &*duration.data_schema.unit == "seconds",
                    "On-off switch fade action duration input data schema property must have the \
                     unit field set to \"percent\" in data schema",
                );
                ensure!(
                    matches!(duration.data_schema.read_only, None | Some(false)),
                    "On-off switch fade action duration input data schema property must have a \
                     null, undefined or false \"read_only\" field in data schema",
                );
                ensure!(
                    matches!(duration.data_schema.write_only, None | Some(false)),
                    "On-off switch fade action duration input data schema property must have a \
                     null, undefined or false \"write_only\" field in data schema",
                );

                ensure!(
                    input_subtype.required.contains(&"level".into()),
                    "On-off switch fade action input data schema must have \"level\" in its \
                     \"required\" field",
                );
                ensure!(
                    input_subtype.required.contains(&"duration".into()),
                    "On-off switch fade action input data schema must have \"duration\" in its \
                     \"required\" field",
                );

                Ok(invoke_form)
            })
            .transpose()?;

        #[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
        #[serde(untagged)]
        enum Brightness {
            Integer(u8),
            Float(f64),
        }

        impl fmt::Display for Brightness {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    Self::Integer(x) => write!(f, "{x}"),
                    Self::Float(x) => write!(f, "{x}"),
                }
            }
        }

        let brightness_form_and_initial_brightness = match brightness_form_and_uses_integers {
            Some((brightness_form, uses_integers)) => Some(if uses_integers {
                let (brightness_form, brightness) =
                    self.check_brightness_inner::<u8>(brightness_form).await?;
                (brightness_form, Brightness::Integer(brightness))
            } else {
                let (brightness_form, brightness) =
                    self.check_brightness_inner::<f64>(brightness_form).await?;
                (brightness_form, Brightness::Float(brightness))
            }),
            None => None,
        };

        if let Some(fade_form) = fade_form {
            #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
            struct Fade {
                duration: u8,
                level: u8,
            }

            const DURATION_SECS: u8 = 2;
            let level = brightness_form_and_initial_brightness
                .and_then(|(_, initial_brightness)| {
                    (initial_brightness == Brightness::Float(95.)
                        || initial_brightness == Brightness::Integer(95))
                    .then_some(15)
                })
                .unwrap_or(95);

            let fade = Fade {
                duration: DURATION_SECS,
                level,
            };
            let (status, _, output): (_, _, ActionResponse<Fade>) = self
                .request_with_json(Method::POST, &fade_form.href, &fade)
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
                "Invalid on-off switch fade action response. Expected status \"running\" or \
                 \"pending\", received {:?}",
                output.status
            );
            ensure!(
                output.output == Some(fade),
                "Invalid on-off switch fade action response. Expected output {fade:?}, got {:?}",
                output.output
            );

            sleep(Duration::from_secs(DURATION_SECS.into()) + Duration::from_millis(100)).await;

            if let Some((brightness_form, initial_brightness)) =
                brightness_form_and_initial_brightness
            {
                let (status, _, brightness) = self
                    .request(Method::GET, &brightness_form.href)
                    .json::<Brightness>()
                    .await?;

                ensure!(
                    status == StatusCode::OK,
                    "Expected OK status code, got {status}",
                );
                ensure!(
                    matches!(brightness, Brightness::Float(x) if x == f64::from(level))
                    || matches!(brightness, Brightness::Integer(x) if x == level),
                    "On-off switch is expected to have the brightness level expected at the end of \
                     a fade ({level}), found {brightness}.",
                );

                let (status, _) = self
                    .request_with_json(Method::PUT, &brightness_form.href, &initial_brightness)
                    .await?;
                ensure!(
                    status == StatusCode::NO_CONTENT,
                    "Expected NO_CONTENT status code, got {status}",
                );
            }
        }

        Ok(())
    }

    async fn check_brightness_inner<'a, T>(
        &self,
        brightness_form: &'a Form,
    ) -> eyre::Result<(&'a Form, T)>
    where
        T: From<u8> + for<'de> Deserialize<'de> + fmt::Display + PartialEq + PartialOrd + Serialize,
    {
        let (status, _, initial_brightness) = self
            .request(Method::GET, &brightness_form.href)
            .json::<T>()
            .await?;

        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        ensure!(
            (T::from(0)..=T::from(100)).contains(&initial_brightness),
            "On-off switch is expected to have a brightness level between 0 and 100, found \
            {initial_brightness}",
        );

        let new_brightness = if initial_brightness == T::from(25) {
            T::from(40)
        } else {
            T::from(25)
        };

        let (status, _) = self
            .request_with_json(Method::PUT, &brightness_form.href, &new_brightness)
            .await?;
        ensure!(
            status == StatusCode::NO_CONTENT,
            "Expected NO_CONTENT status code, got {status}",
        );

        let (status, _, brightness) = self
            .request(Method::GET, &brightness_form.href)
            .json::<T>()
            .await?;

        ensure!(
            status == StatusCode::OK,
            "Expected OK status code, got {status}",
        );
        ensure!(
            brightness == new_brightness,
            "On-off switch is expected to have the brightness level just set \
            ({new_brightness}), found {brightness}.",
        );

        let (status, _) = self
            .request_with_json(Method::PUT, &brightness_form.href, &initial_brightness)
            .await?;
        ensure!(
            status == StatusCode::NO_CONTENT,
            "Expected NO_CONTENT status code, got {status}",
        );

        Ok((brightness_form, initial_brightness))
    }

    fn request(&self, method: Method, endpoint: &str) -> RequestHandler {
        let request = self.create_request(method, endpoint);
        RequestHandler { request }
    }

    fn request_with_json<T>(&self, method: Method, endpoint: &str, body: &T) -> RequestHandler
    where
        T: Serialize + Sized,
    {
        let request = self.create_request(method, endpoint).json(body);
        RequestHandler { request }
    }

    fn create_request(&self, method: Method, endpoint: &str) -> RequestBuilder {
        use fmt::Write;

        let mut url = format!("http://{}", self.host);
        if let Some(port) = self.port {
            write!(url, ":{}", port).unwrap();
        }
        write!(url, "/{}", endpoint.trim_start_matches('/')).unwrap();

        self.client.request(method, url)
    }
}

async fn check_no_incoming_messages<T, F>(
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

struct RequestHandler {
    request: RequestBuilder,
}

impl RequestHandler {
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
