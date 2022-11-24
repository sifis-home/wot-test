//! An stand-alone implementation of a Thing Description.
//!
//! This is a limited implementation to not rely on [`wot-td`] while testing it, use it to
//! implement Things instead!
//!
//! Note: the implementation is incomplete.

use std::{borrow::Cow, collections::HashMap, ops::Deref};

use serde::{Deserialize, Deserializer};

/// A simple wrapper to a `Cow<'static, str>`.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize)]
pub struct CowStr(pub Cow<'static, str>);

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

/// Possible security schemes.
#[derive(Debug, Eq, PartialEq, Deserialize)]
pub enum SecurityScheme {
    /// No security scheme.
    #[serde(rename = "nosec")]
    NoSecurityScheme,
}

/// A Thing Description.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThingDescription {
    /// The id.
    #[serde(default)]
    pub id: CowStr,

    /// The title.
    #[serde(default)]
    pub title: CowStr,

    /// The security string.
    #[serde(default)]
    pub security: CowStr,

    /// The security definitions.
    pub security_definitions: HashMap<CowStr, SecuritySchemeDefinition>,

    /// The JSON-LD @type.
    #[serde(rename = "@type")]
    pub attype: OneOrMany<CowStr>,

    /// The description.
    #[serde(default)]
    pub description: CowStr,

    /// The properties.
    #[serde(default)]
    pub properties: HashMap<CowStr, PropertyAffordance>,

    /// The actions.
    #[serde(default)]
    pub actions: HashMap<CowStr, ActionAffordance>,

    /// The events.
    #[serde(default)]
    pub events: HashMap<CowStr, EventAffordance>,

    /// The Thing-level forms.
    #[serde(default)]
    pub forms: Vec<Form>,
}

/// The security scheme definitions.
#[derive(Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecuritySchemeDefinition {
    /// The security scheme.
    pub scheme: SecurityScheme,
}

/// A wrapper to represent one or many elements using a [`Vec`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct OneOrMany<T>(pub Vec<T>);

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

/// A partial interaction affordance.
///
/// It does not contain the _human readable_ part.
#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub struct PartialInteractionAffordance {
    /// The affordance forms.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub forms: Vec<Form>,
}

/// A full interaction affordance.
#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub struct InteractionAffordance {
    /// The _partial_ interaction affordance.
    #[serde(flatten)]
    pub interaction: PartialInteractionAffordance,

    /// The human readable fields.
    #[serde(flatten)]
    pub human_readable: HumanReadable,
}

/// A property affordance.
#[derive(Debug, Deserialize)]
pub struct PropertyAffordance {
    /// The partial interaction affordance.
    #[serde(flatten)]
    pub interaction: PartialInteractionAffordance,

    /// A partial data schema.
    #[serde(flatten)]
    pub data_schema: PartialDataSchema,

    /// The human readable fields.
    #[serde(flatten)]
    pub human_readable: HumanReadable,
}

/// An action affordance.
#[derive(Debug, Default, Deserialize)]
pub struct ActionAffordance {
    /// The interaction affordance.
    #[serde(flatten)]
    pub interaction: InteractionAffordance,

    /// The data schema for input fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<DataSchema>,

    /// The data schema for output fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<DataSchema>,

    /// Is it synchronous?
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synchronous: Option<bool>,
}

/// An event affordance.
#[derive(Debug, Default, Deserialize)]
pub struct EventAffordance {
    /// The interaction affordance.
    #[serde(flatten)]
    pub interaction: InteractionAffordance,

    /// The data schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<DataSchema>,
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

/// Set of human readable fields.
#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub struct HumanReadable {
    /// The JSON-LD @type.
    #[serde(rename = "@type", skip_serializing_if = "CowStr::is_empty", default)]
    pub attype: CowStr,

    /// The title.
    #[serde(skip_serializing_if = "CowStr::is_empty", default)]
    pub title: CowStr,

    /// The description.
    #[serde(skip_serializing_if = "CowStr::is_empty", default)]
    pub description: CowStr,
}

/// A form component.
#[derive(Debug, Eq, PartialEq, Deserialize)]
pub struct Form {
    /// The hyper-reference of the form.
    pub href: CowStr,

    /// The operation(s) of the form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<OneOrMany<Operation>>,

    /// The subprotocol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subprotocol: Option<CowStr>,
}

/// A data schema.
#[derive(Debug, PartialEq, Deserialize)]
pub struct DataSchema {
    /// The partial data schema.
    #[serde(flatten)]
    pub data_schema: PartialDataSchema,

    /// The human readable fields.
    #[serde(flatten)]
    pub human_readable: HumanReadable,
}

/// A partial data schema.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialDataSchema {
    /// The data schema subtype.
    #[serde(flatten)]
    pub subtype: DataSchemaSubtype,

    /// The unit.
    #[serde(skip_serializing_if = "CowStr::is_empty", default)]
    pub unit: CowStr,

    /// Is it read only?
    pub read_only: Option<bool>,

    /// Is it write only?
    pub write_only: Option<bool>,

    /// Is it observable?
    #[serde(default)]
    pub observable: bool,
}

/// A helper structure for default data schema fields.
#[derive(Debug, Clone, Copy)]
pub struct PartialDataSchemaDefaults {
    /// Is it read only?
    pub read_only: &'static Option<bool>,

    /// Is it write only?
    pub write_only: &'static Option<bool>,
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
    /// Checks whether two partial data schemas are equivalent.
    pub fn is_equivalent(&self, other: &Self, defaults: Option<PartialDataSchemaDefaults>) -> bool {
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
    /// Checks whether two data schemas are equivalent.
    pub fn is_equivalent(&self, other: &Self) -> bool {
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

/// Data schema subtypes.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DataSchemaSubtype {
    /// An object data schema.
    Object(ObjectDataSchema),

    /// An array data schema.
    Array,

    /// A string data schema.
    String,

    /// A number data schema.
    Number(NumberDataSchema),

    /// An integer data schema.
    Integer(IntegerDataSchema),

    /// A boolean data schema.
    Boolean,

    /// A null data schema.
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

/// An integer data schema.
#[derive(Debug, Default, Eq, PartialEq, Deserialize)]
pub struct IntegerDataSchema {
    /// The minimum value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,

    /// The maximum value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
}

/// A number data schema.
#[derive(Debug, Default, PartialEq, Deserialize)]
pub struct NumberDataSchema {
    /// The minimum value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,

    /// The maximum value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
}

/// An object data schema.
#[derive(Debug, Default, PartialEq, Deserialize)]
pub struct ObjectDataSchema {
    /// The properties of the object.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub properties: HashMap<CowStr, DataSchema>,

    /// The set of required properties, by key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<CowStr>,
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

/// A form operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Read a property.
    ReadProperty,

    /// Write a property.
    WriteProperty,

    /// Observe a property.
    ObserveProperty,

    /// Unobserve a property.
    UnobserveProperty,

    /// Invoke an action.
    InvokeAction,

    /// Query an action.
    QueryAction,

    /// Cancel an action.
    CancelAction,

    /// Subscribe to an event.
    SubscribeEvent,

    /// Unsubscribe from an event.
    UnsubscribeEvent,

    /// Read all properties.
    ReadAllProperties,

    /// Write to all properties.
    WriteAllProperties,

    /// Read multiple properties.
    ReadMultipleProperties,

    /// Write to multiple properties.
    WriteMultipleProperties,

    /// Observer all properties.
    ObserveAllProperties,

    /// Unobserve all properties.
    UnobserveAllProperties,

    /// Subscribe to all events.
    SubscribeAllEvents,

    /// Unsubscribe from all events.
    UnsubscribeAllEvents,

    /// Query all actions.
    QueryAllActions,
}
