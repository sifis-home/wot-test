use std::{borrow::Cow, collections::HashMap, ops::Deref};

use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize)]
pub(crate) struct CowStr(Cow<'static, str>);

impl From<&'static str> for CowStr {
    fn from(s: &'static str) -> Self {
        CowStr(s.into())
    }
}

impl From<String> for CowStr {
    fn from(s: String) -> Self {
        CowStr(s.into())
    }
}

impl Deref for CowStr {
    type Target = Cow<'static, str>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Eq, PartialEq, Deserialize)]
pub(crate) enum SecurityScheme {
    #[serde(rename = "nosec")]
    NoSecurityScheme,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThingDescription {
    #[serde(default)]
    pub(crate) id: CowStr,
    #[serde(default)]
    pub(crate) title: CowStr,
    #[serde(default)]
    pub(crate) security: CowStr,
    pub(crate) security_definitions: HashMap<CowStr, SecuritySchemeDefinition>,
    #[serde(rename = "@type")]
    pub(crate) attype: OneOrMany<CowStr>,
    #[serde(default)]
    pub(crate) description: CowStr,
    #[serde(default)]
    pub(crate) properties: HashMap<CowStr, PropertyAffordance>,
    #[serde(default)]
    pub(crate) actions: HashMap<CowStr, ActionAffordance>,
    #[serde(default)]
    pub(crate) events: HashMap<CowStr, EventAffordance>,
    #[serde(default)]
    pub(crate) forms: Vec<Form>,
}

#[derive(Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SecuritySchemeDefinition {
    pub(crate) scheme: SecurityScheme,
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub(crate) struct OneOrMany<T>(pub(crate) Vec<T>);

impl<T> AsRef<[T]> for OneOrMany<T> {
    fn as_ref(&self) -> &[T] {
        self.0.as_ref()
    }
}

impl<T> Deref for OneOrMany<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de, T> Deserialize<'de> for OneOrMany<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Inner<T> {
            One(T),
            Many(Vec<T>),
        }

        let inner = Inner::deserialize(deserializer)?;
        Ok(match inner {
            Inner::One(x) => Self(vec![x]),
            Inner::Many(x) => Self(x),
        })
    }
}

impl<T> From<Vec<T>> for OneOrMany<T> {
    fn from(vec: Vec<T>) -> Self {
        Self(vec)
    }
}

impl<T> From<T> for OneOrMany<T> {
    fn from(elem: T) -> Self {
        Self(vec![elem])
    }
}

#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub(crate) struct PartialInteractionAffordance {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) forms: Vec<Form>,
}

#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub(crate) struct InteractionAffordance {
    #[serde(flatten)]
    pub(crate) interaction: PartialInteractionAffordance,
    #[serde(flatten)]
    pub(crate) human_readable: HumanReadable,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PropertyAffordance {
    #[serde(flatten)]
    pub(crate) interaction: PartialInteractionAffordance,
    #[serde(flatten)]
    pub(crate) data_schema: PartialDataSchema,
    #[serde(flatten)]
    pub(crate) human_readable: HumanReadable,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ActionAffordance {
    #[serde(flatten)]
    pub(crate) interaction: InteractionAffordance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<DataSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<DataSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) synchronous: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct EventAffordance {
    #[serde(flatten)]
    pub(crate) interaction: InteractionAffordance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<DataSchema>,
}

impl Form {
    fn is_equivalent(
        &self,
        other: &Self,
        default_op_self: &[Operation],
        default_op_other: &[Operation],
    ) -> bool {
        self.href == other.href
            && self.op.as_deref().unwrap_or(default_op_self)
                == other.op.as_deref().unwrap_or(default_op_other)
    }
}

fn are_equivalent_interaction_affordance<'a, F, DS>(
    left_affordance: &'a PartialInteractionAffordance,
    left_data_schema: DS,
    right_affordance: &'a PartialInteractionAffordance,
    right_data_schema: DS,
    get_operation: F,
) -> bool
where
    DS: Copy + 'a,
    F: Fn(DS) -> &'static [Operation],
{
    left_affordance
        .forms
        .iter()
        .zip(&right_affordance.forms)
        .all(|(a, b)| {
            a.is_equivalent(
                b,
                get_operation(left_data_schema),
                get_operation(right_data_schema),
            )
        })
}

impl InteractionAffordance {
    #[inline]
    fn is_equivalent(&self, other: &Self, default_op: &'static [Operation]) -> bool {
        self.human_readable == other.human_readable
            && are_equivalent_interaction_affordance(
                &self.interaction,
                (),
                &other.interaction,
                (),
                move |_data| default_op,
            )
    }
}

impl PartialEq for PropertyAffordance {
    fn eq(&self, other: &Self) -> bool {
        const DATA_SCHEMA_DEFAULTS: PartialDataSchemaDefaults = PartialDataSchemaDefaults {
            read_only: &Some(false),
            write_only: &Some(false),
        };

        are_equivalent_interaction_affordance(
            &self.interaction,
            &self.data_schema,
            &other.interaction,
            &other.data_schema,
            |data| match (data.read_only, data.write_only) {
                (None | Some(false), None | Some(false)) => {
                    [Operation::ReadProperty, Operation::WriteProperty].as_slice()
                }
                (Some(true), _) => &[Operation::ReadProperty],
                (_, Some(true)) => &[Operation::WriteProperty],
            },
        ) && self
            .data_schema
            .is_equivalent(&other.data_schema, Some(DATA_SCHEMA_DEFAULTS))
            && self.human_readable == other.human_readable
    }
}

impl PartialEq for ActionAffordance {
    fn eq(&self, other: &Self) -> bool {
        // read_only and write_only are `None` by default on ActionAffordance, no need to use
        // DataSchema::is_equivalent
        self.interaction
            .is_equivalent(&other.interaction, &[Operation::InvokeAction])
            && are_equivalent_opt_data_schema(&self.input, &other.input)
            && are_equivalent_opt_data_schema(&self.output, &other.output)
            && self.synchronous == other.synchronous
    }
}

impl PartialEq for EventAffordance {
    fn eq(&self, other: &Self) -> bool {
        // read_only and write_only are `None` by default on EventAffordance, no need to use
        // DataSchema::is_equivalent
        self.interaction.is_equivalent(
            &other.interaction,
            [Operation::SubscribeEvent, Operation::UnsubscribeEvent].as_slice(),
        ) && are_equivalent_opt_data_schema(&self.data, &other.data)
    }
}

#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub(crate) struct HumanReadable {
    #[serde(rename = "@type", skip_serializing_if = "CowStr::is_empty", default)]
    pub(crate) attype: CowStr,
    #[serde(skip_serializing_if = "CowStr::is_empty", default)]
    pub(crate) title: CowStr,
    #[serde(skip_serializing_if = "CowStr::is_empty", default)]
    pub(crate) description: CowStr,
}

#[derive(Debug, Eq, PartialEq, Deserialize)]
pub(crate) struct Form {
    pub(crate) href: CowStr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) op: Option<OneOrMany<Operation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subprotocol: Option<CowStr>,
}

#[derive(Debug, PartialEq, Deserialize)]
pub(crate) struct DataSchema {
    #[serde(flatten)]
    pub(crate) data_schema: PartialDataSchema,
    #[serde(flatten)]
    pub(crate) human_readable: HumanReadable,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialDataSchema {
    #[serde(flatten)]
    pub(crate) subtype: DataSchemaSubtype,
    #[serde(skip_serializing_if = "CowStr::is_empty", default)]
    pub(crate) unit: CowStr,
    pub(crate) read_only: Option<bool>,
    pub(crate) write_only: Option<bool>,
    #[serde(default)]
    pub(crate) observable: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PartialDataSchemaDefaults {
    pub(crate) read_only: &'static Option<bool>,
    pub(crate) write_only: &'static Option<bool>,
}

impl Default for PartialDataSchemaDefaults {
    fn default() -> Self {
        Self {
            read_only: &None,
            write_only: &None,
        }
    }
}

impl PartialDataSchema {
    pub(crate) fn is_equivalent(
        &self,
        other: &Self,
        defaults: Option<PartialDataSchemaDefaults>,
    ) -> bool {
        self.subtype.is_equivalent(&other.subtype)
            && self.unit == other.unit
            && defaults
                .map(|defaults| {
                    self.read_only.or(*defaults.read_only)
                        == other.read_only.or(*defaults.read_only)
                        && self.write_only.or(*defaults.write_only)
                            == other.write_only.or(*defaults.write_only)
                })
                .unwrap_or(true)
    }
}

impl DataSchema {
    pub(crate) fn is_equivalent(&self, other: &Self) -> bool {
        self.data_schema.is_equivalent(&other.data_schema, None)
            && self.human_readable == other.human_readable
    }
}

fn are_equivalent_opt_data_schema(left: &Option<DataSchema>, right: &Option<DataSchema>) -> bool {
    match (left.as_ref(), right.as_ref()) {
        (None, None) => true,
        (Some(left), Some(right)) => left.is_equivalent(right),
        _ => false,
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum DataSchemaSubtype {
    Object(ObjectDataSchema),
    Array,
    String,
    Number(NumberDataSchema),
    Integer(IntegerDataSchema),
    Boolean,
    Null,
}

impl DataSchemaSubtype {
    fn is_equivalent(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Object(a), Self::Object(b)) => a.is_equivalent(b),
            (Self::Integer(a), Self::Integer(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::Array, Self::Array)
            | (Self::String, Self::String)
            | (Self::Boolean, Self::Boolean)
            | (Self::Null, Self::Null) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub(crate) struct IntegerDataSchema {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) minimum: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) maximum: Option<i64>,
}

#[derive(Debug, Default, PartialEq, Deserialize)]
pub(crate) struct NumberDataSchema {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) minimum: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) maximum: Option<f64>,
}

#[derive(Debug, Default, PartialEq, Deserialize)]
pub(crate) struct ObjectDataSchema {
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub(crate) properties: HashMap<CowStr, DataSchema>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) required: Vec<CowStr>,
}

impl ObjectDataSchema {
    fn is_equivalent(&self, other: &Self) -> bool {
        self.properties.len() == other.properties.len()
            && self.properties.iter().all(|(name, data_schema)| {
                other
                    .properties
                    .get(name)
                    .map_or(false, |other_data_schema| {
                        data_schema.is_equivalent(other_data_schema)
                    })
            })
            && self.required.iter().all(|req| other.required.contains(req))
            && other.required.iter().all(|req| self.required.contains(req))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Operation {
    ReadProperty,
    WriteProperty,
    ObserveProperty,
    UnobserveProperty,
    InvokeAction,
    QueryAction,
    CancelAction,
    SubscribeEvent,
    UnsubscribeEvent,
    ReadAllProperties,
    WriteAllProperties,
    ReadMultipleProperties,
    WriteMultipleProperties,
    ObserveAllProperties,
    UnobserveAllProperties,
    SubscribeAllEvents,
    UnsubscribeAllEvents,
    QueryAllActions,
}
