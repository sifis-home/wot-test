use once_cell::sync::Lazy;

use crate::td::{
    ActionAffordance, DataSchema, DataSchemaSubtype, EventAffordance, Form, HumanReadable,
    IntegerDataSchema, InteractionAffordance, ObjectDataSchema, OneOrMany, Operation,
    PartialDataSchema, PartialInteractionAffordance, PropertyAffordance, SecurityScheme,
    SecuritySchemeDefinition, ThingDescription,
};

pub(crate) static DESCRIPTION: Lazy<ThingDescription> = Lazy::new(|| ThingDescription {
    id: "urn:dev:ops:my-lamp-1234".into(),
    title: "My Lamp".into(),
    security: "nosec_sc".into(),
    security_definitions: [(
        "nosec_sc".into(),
        SecuritySchemeDefinition {
            scheme: SecurityScheme::NoSecurityScheme,
        },
    )]
    .into_iter()
    .collect(),
    attype: vec!["OnOffSwitch".into(), "Light".into()].into(),
    description: "A web connected lamp".into(),
    properties: [
        (
            "on".into(),
            PropertyAffordance {
                interaction: PartialInteractionAffordance {
                    forms: vec![Form {
                        href: "/properties/on".into(),
                        op: Default::default(),
                        subprotocol: None,
                    }],
                },
                human_readable: HumanReadable {
                    attype: "OnOffProperty".into(),
                    title: "On/Off".into(),
                    description: "Whether the lamp is turned on".into(),
                },
                data_schema: PartialDataSchema {
                    subtype: DataSchemaSubtype::Boolean,
                    unit: Default::default(),
                    read_only: Default::default(),
                    write_only: Default::default(),
                    observable: Default::default(),
                },
            },
        ),
        (
            "brightness".into(),
            PropertyAffordance {
                interaction: PartialInteractionAffordance {
                    forms: vec![
                        Form {
                            href: "/properties/brightness".into(),
                            op: Some(OneOrMany(vec![
                                Operation::ReadProperty,
                                Operation::WriteProperty,
                            ])),
                            subprotocol: None,
                        },
                        Form {
                            href: "/properties/brightness/observe".into(),
                            op: Some(OneOrMany(vec![
                                Operation::ObserveProperty,
                                Operation::UnobserveProperty,
                            ])),
                            subprotocol: Some("sse".into()),
                        },
                    ],
                },
                human_readable: HumanReadable {
                    attype: "BrightnessProperty".into(),
                    title: "Brightness".into(),
                    description: "The level of light from 0-100".into(),
                },
                data_schema: PartialDataSchema {
                    subtype: DataSchemaSubtype::Integer(IntegerDataSchema {
                        minimum: Some(0.into()),
                        maximum: Some(100.into()),
                    }),
                    unit: "percent".into(),
                    read_only: Default::default(),
                    write_only: Default::default(),
                    observable: true,
                },
            },
        ),
    ]
    .into_iter()
    .collect(),
    actions: [(
        "fade".into(),
        ActionAffordance {
            interaction: InteractionAffordance {
                interaction: PartialInteractionAffordance {
                    forms: vec![
                        Form {
                            href: "/actions/fade".into(),
                            op: Some(Operation::InvokeAction.into()),
                            subprotocol: None,
                        },
                        Form {
                            href: "/actions/fade/{action_id}".into(),
                            op: Some(vec![Operation::QueryAction, Operation::CancelAction].into()),
                            subprotocol: None,
                        },
                    ],
                },
                human_readable: HumanReadable {
                    title: "Fade".into(),
                    description: "Fade the lamp to a given level".into(),
                    ..Default::default()
                },
            },
            input: Some(DataSchema {
                data_schema: PartialDataSchema {
                    subtype: DataSchemaSubtype::Object(ObjectDataSchema {
                        properties: [
                            (
                                "brightness".into(),
                                DataSchema {
                                    data_schema: PartialDataSchema {
                                        subtype: DataSchemaSubtype::Integer(IntegerDataSchema {
                                            minimum: Some(0),
                                            maximum: Some(100),
                                        }),
                                        unit: "percent".into(),
                                        read_only: Default::default(),
                                        write_only: Default::default(),
                                        observable: Default::default(),
                                    },
                                    human_readable: Default::default(),
                                },
                            ),
                            (
                                "duration".into(),
                                DataSchema {
                                    data_schema: PartialDataSchema {
                                        subtype: DataSchemaSubtype::Integer(IntegerDataSchema {
                                            minimum: Some(1),
                                            ..Default::default()
                                        }),
                                        unit: "milliseconds".into(),
                                        read_only: Default::default(),
                                        write_only: Default::default(),
                                        observable: Default::default(),
                                    },
                                    human_readable: Default::default(),
                                },
                            ),
                        ]
                        .into_iter()
                        .collect(),
                        required: vec!["brightness".into(), "duration".into()],
                    }),
                    unit: Default::default(),
                    read_only: Default::default(),
                    write_only: Default::default(),
                    observable: Default::default(),
                },
                human_readable: Default::default(),
            }),
            ..Default::default()
        },
    )]
    .into_iter()
    .collect(),
    events: [(
        "overheated".into(),
        EventAffordance {
            interaction: InteractionAffordance {
                interaction: PartialInteractionAffordance {
                    forms: vec![Form {
                        href: "/events/overheated".into(),
                        op: Default::default(),
                        subprotocol: Some("sse".into()),
                    }],
                },
                human_readable: HumanReadable {
                    description: "The lamp has exceeded its safe operating temperature".into(),
                    ..Default::default()
                },
            },
            data: Some(DataSchema {
                data_schema: PartialDataSchema {
                    subtype: DataSchemaSubtype::Number,
                    unit: "degree celsius".into(),
                    read_only: Default::default(),
                    write_only: Default::default(),
                    observable: Default::default(),
                },
                human_readable: Default::default(),
            }),
        },
    )]
    .into_iter()
    .collect(),
    forms: vec![
        Form {
            href: "/properties".into(),
            op: Some(OneOrMany(vec![Operation::ReadAllProperties])),
            subprotocol: None,
        },
        Form {
            href: "/properties/observe".into(),
            op: Some(OneOrMany(vec![
                Operation::ObserveAllProperties,
                Operation::UnobserveAllProperties,
            ])),
            subprotocol: Some("sse".into()),
        },
        Form {
            href: "/actions".into(),
            op: Some(OneOrMany(vec![Operation::QueryAllActions])),
            subprotocol: None,
        },
        Form {
            href: "/events".into(),
            op: Some(OneOrMany(vec![
                Operation::SubscribeAllEvents,
                Operation::UnsubscribeAllEvents,
            ])),
            subprotocol: Some("sse".into()),
        },
    ],
});

pub(crate) type Brightness = i64;
pub(crate) type Status = bool;
