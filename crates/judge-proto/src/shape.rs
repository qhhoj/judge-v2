//! The wire shape of the contract (messages, field numbers, types, `optional`, oneof
//! membership, enum values, services
//! RPCs included) and the v1 rule: a newer shape may only add to an older one.
use std::collections::BTreeMap;

use prost::Message;
use prost_types::{DescriptorProto, EnumDescriptorProto, FileDescriptorSet, ServiceDescriptorProto};
use serde::{Deserialize, Serialize};

/// Version of this file's layout. Format 2 adds `optional`, `oneof` and `services`;
/// a shape of an older format is not compared on those.
pub const SHAPE_FORMAT: u32 = 2;

fn legacy_format() -> u32 {
    1
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldShape {
    pub name: String,
    /// `type_name` for messages and enums, else the scalar type's name (`Uint32`).
    pub kind: String,
    pub repeated: bool,
    /// Declared with proto3 `optional` (explicit presence).
    #[serde(default)]
    pub optional: bool,
    /// The `oneof` the field belongs to, if any.
    #[serde(default)]
    pub oneof: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MethodShape {
    pub input: String,
    pub output: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageShape {
    pub fields: BTreeMap<i32, FieldShape>,
    pub reserved_numbers: Vec<i32>,
    pub reserved_names: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    #[serde(default = "legacy_format")]
    pub format: u32,
    /// `proto/version.json` minor the shape was blessed at.
    #[serde(default)]
    pub minor: u32,
    pub messages: BTreeMap<String, MessageShape>,
    pub enums: BTreeMap<String, BTreeMap<i32, String>>,
    /// Service name -> method name -> shape.
    #[serde(default)]
    pub services: BTreeMap<String, BTreeMap<String, MethodShape>>,
}

/// The shape of every message and enum in `descriptor_set` (an encoded `FileDescriptorSet`).
pub fn shape_of(descriptor_set: &[u8]) -> Result<Shape, prost::DecodeError> {
    let set = FileDescriptorSet::decode(descriptor_set)?;
    let mut shape = Shape { format: SHAPE_FORMAT, minor: crate::PROTOCOL_MINOR, ..Shape::default() };
    for file in &set.file {
        let prefix = format!(".{}", file.package());
        file.message_type.iter().for_each(|m| add_message(&mut shape, &prefix, m));
        file.enum_type.iter().for_each(|e| add_enum(&mut shape, &prefix, e));
        file.service.iter().for_each(|service| add_service(&mut shape, &prefix, service));
    }
    Ok(shape)
}

fn add_message(shape: &mut Shape, prefix: &str, message: &DescriptorProto) {
    let name = format!("{prefix}.{}", message.name());
    let fields = message.field.iter().map(|field| {
        let kind = match field.type_name() {
            "" => format!("{:?}", field.r#type()),
            type_name => type_name.to_string(),
        };
        let repeated = field.label() == prost_types::field_descriptor_proto::Label::Repeated;
        let optional = field.proto3_optional();
        // a proto3 `optional` field sits in a synthetic oneof that is not a real group
        let oneof = field.oneof_index.filter(|_| !optional).map(|index| {
            message.oneof_decl.get(index as usize).map(|decl| decl.name().to_string()).unwrap_or_default()
        });
        (field.number(), FieldShape { name: field.name().to_string(), kind, repeated, optional, oneof })
    });
    let reserved_numbers =
        message.reserved_range.iter().flat_map(|range| range.start()..range.end()).collect();
    let entry = MessageShape {
        fields: fields.collect(),
        reserved_numbers,
        reserved_names: message.reserved_name.clone(),
    };
    shape.messages.insert(name.clone(), entry);
    message.nested_type.iter().for_each(|m| add_message(shape, &name, m));
    message.enum_type.iter().for_each(|e| add_enum(shape, &name, e));
}

fn add_enum(shape: &mut Shape, prefix: &str, value: &EnumDescriptorProto) {
    let values = value.value.iter().map(|v| (v.number(), v.name().to_string()));
    shape.enums.insert(format!("{prefix}.{}", value.name()), values.collect());
}

fn add_service(shape: &mut Shape, prefix: &str, service: &ServiceDescriptorProto) {
    let methods = service.method.iter().map(|method| {
        let method_shape = MethodShape {
            input: method.input_type().to_string(),
            output: method.output_type().to_string(),
            client_streaming: method.client_streaming(),
            server_streaming: method.server_streaming(),
        };
        (method.name().to_string(), method_shape)
    });
    shape.services.insert(format!("{prefix}.{}", service.name()), methods.collect());
}

/// Every way `new` breaks a reader built from `old`; empty means `new` is additive.
pub fn breaking_changes(old: &Shape, new: &Shape) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, old_message) in &old.messages {
        match new.messages.get(name) {
            None => problems.push(format!("message {name} was removed")),
            Some(new_message) => {
                compare_message(name, (old_message, new_message), old.format >= 2, &mut problems)
            }
        }
    }
    compare_services(old, new, &mut problems);
    for (name, old_values) in &old.enums {
        let new_values = new.enums.get(name);
        for (number, value) in old_values {
            if new_values.and_then(|values| values.get(number)) != Some(value) {
                problems.push(format!("enum value {name}.{value} = {number} changed or removed"));
            }
        }
    }
    problems
}

fn compare_services(old: &Shape, new: &Shape, problems: &mut Vec<String>) {
    for (service, old_methods) in &old.services {
        for (method, old_method) in old_methods {
            let new_method = new.services.get(service).and_then(|methods| methods.get(method));
            if new_method != Some(old_method) {
                problems
                    .push(format!("rpc {service}.{method} was removed or changed its types or streaming"));
            }
        }
    }
}

/// Fields are the same on the wire and in meaning; `strict` also compares `optional` and the oneof.
fn same_field(old: &FieldShape, new: &FieldShape, strict: bool) -> bool {
    let wire = old.name == new.name && old.kind == new.kind && old.repeated == new.repeated;
    wire && (!strict || (old.optional == new.optional && old.oneof == new.oneof))
}

fn compare_message(
    name: &str,
    (old, new): (&MessageShape, &MessageShape),
    strict: bool,
    problems: &mut Vec<String>,
) {
    for (number, old_field) in &old.fields {
        match new.fields.get(number) {
            Some(new_field) if same_field(old_field, new_field, strict) => {}
            Some(_) => {
                problems.push(format!("{name} field {number} changed name, type, label, optional or oneof"))
            }
            None if new.reserved_numbers.contains(number) && new.reserved_names.contains(&old_field.name) => {
            }
            None => problems.push(format!(
                "{name} field {number} ({}) removed without reserving its number and name",
                old_field.name
            )),
        }
    }
    for (number, field) in &new.fields {
        if old.reserved_numbers.contains(number) || old.reserved_names.contains(&field.name) {
            problems.push(format!("{name} field {number} reuses a reserved number or name"));
        }
    }
}

/// Fields (oneof members included) must be declared in increasing number order.
pub fn declaration_order_problems(descriptor_set: &[u8]) -> Result<Vec<String>, prost::DecodeError> {
    let set = FileDescriptorSet::decode(descriptor_set)?;
    let mut problems = Vec::new();
    for file in &set.file {
        file.message_type.iter().for_each(|m| check_order(m, &mut problems));
    }
    Ok(problems)
}

fn check_order(message: &DescriptorProto, problems: &mut Vec<String>) {
    let numbers: Vec<i32> = message.field.iter().map(|field| field.number()).collect();
    if numbers.windows(2).any(|pair| pair[0] >= pair[1]) {
        problems.push(format!("{}: fields out of number order {numbers:?}", message.name()));
    }
    message.nested_type.iter().for_each(|m| check_order(m, problems));
}

/// Whether a changed contract may be written to the shape file: the version must have
/// been bumped, and a non-additive change also needs the explicit `--allow-breaking`.
pub fn bless_problems(stored: &Shape, current: &Shape, allow_breaking: bool) -> Result<(), String> {
    if stored == current {
        return Ok(());
    }
    if current.minor <= stored.minor {
        return Err(format!(
            "the contract changed but the minor is still {}: bump proto/version.json and PROTOCOL_MINOR first",
            current.minor
        ));
    }
    let breaking = breaking_changes(stored, current);
    if !breaking.is_empty() && !allow_breaking {
        return Err(format!("breaking changes need --allow-breaking:\n{}", breaking.join("\n")));
    }
    Ok(())
}
