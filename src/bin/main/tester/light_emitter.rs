use std::{fmt, time::Duration};

use http_api_problem::StatusCode;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use stable_eyre::eyre::{self, bail, ensure, eyre};
use tokio::time::sleep;
use tracing::info;
use wot_test::{
    td::{
        DataSchemaSubtype, Form, IntegerDataSchema, NumberDataSchema, Operation, ThingDescription,
    },
    tester::{ActionResponse, ActionResponseStatus},
    Requester,
};

use crate::tester::on_off_switch;

use super::Tester;

pub(crate) async fn run_test(tester: &Tester) -> eyre::Result<()> {
    let (status, _, thing) = tester
        .request(Method::GET, ".well-known/wot")
        .json::<ThingDescription>()
        .await?;
    ensure!(
        status == StatusCode::OK,
        "Expected OK status code, got {status}",
    );
    let on_form = on_off_switch::get_on_off_form(&thing)?;

    info!("Got on-off-switch TD from .well-known/wot that it could be a light emitter");

    if thing.attype.contains(&"Light".into()) {
        info!("On-off switch is also a Light");
    }

    on_off_switch::test_switch(tester, &thing, on_form).await?;

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
                check_brightness_inner::<u8>(tester, brightness_form).await?;
            (brightness_form, Brightness::Integer(brightness))
        } else {
            let (brightness_form, brightness) =
                check_brightness_inner::<f64>(tester, brightness_form).await?;
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
        let (status, _, output): (_, _, ActionResponse<Fade>) = tester
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

        if let Some((brightness_form, initial_brightness)) = brightness_form_and_initial_brightness
        {
            let (status, _, brightness) = tester
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

            let (status, _) = tester
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
    tester: &Tester,
    brightness_form: &'a Form,
) -> eyre::Result<(&'a Form, T)>
where
    T: From<u8> + for<'de> Deserialize<'de> + fmt::Display + PartialEq + PartialOrd + Serialize,
{
    let (status, _, initial_brightness) = tester
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

    let (status, _) = tester
        .request_with_json(Method::PUT, &brightness_form.href, &new_brightness)
        .await?;
    ensure!(
        status == StatusCode::NO_CONTENT,
        "Expected NO_CONTENT status code, got {status}",
    );

    let (status, _, brightness) = tester
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

    let (status, _) = tester
        .request_with_json(Method::PUT, &brightness_form.href, &initial_brightness)
        .await?;
    ensure!(
        status == StatusCode::NO_CONTENT,
        "Expected NO_CONTENT status code, got {status}",
    );

    Ok((brightness_form, initial_brightness))
}
