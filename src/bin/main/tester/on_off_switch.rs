use http_api_problem::StatusCode;
use reqwest::Method;
use stable_eyre::{
    eyre::{self, ensure, eyre},
    Report,
};
use tracing::info;
use wot_test::{
    td::{DataSchemaSubtype, Form, Operation, ThingDescription},
    test_property, Requester,
};

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
    let on_form = get_on_off_form(&thing)?;

    info!("Got on-off-switch TD from .well-known/wot");

    test_switch(tester, &thing, on_form).await
}

pub(crate) async fn test_switch(
    tester: &Tester,
    thing: &ThingDescription,
    on_form: &Form,
) -> eyre::Result<()> {
    let turn_off = || async {
        test_property(tester, on_form, &false).await?;

        info!("On-off-switch thing is off as expected");

        Ok::<_, Report>(())
    };

    let turn_on = || async {
        test_property(tester, on_form, &true).await?;

        info!("On-off-switch thing is on as expected");

        Ok::<_, Report>(())
    };

    let (status, _, initial_is_on) = tester
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
            let (status, _) = tester.request(Method::POST, &invoke_action.href).await?;
            ensure!(
                status == StatusCode::OK,
                "Expected OK status code, got {status}",
            );

            Ok(())
        };

        toggle().await?;
        let (status, _, is_on) = tester
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
        let (status, _, is_on) = tester
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

pub(crate) fn get_on_off_form(thing: &ThingDescription) -> eyre::Result<&Form> {
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
