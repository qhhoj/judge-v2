//! The wire shape of the contract (messages, field numbers, types, enum values)
//! and the v1 rule: a newer shape may only add to an older one.
use std::collections::BTreeMap;

use prost::Message;
use prost_types::{DescriptorProto, EnumDescriptorProto, FileDescriptorSet};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldShape {
    pub name: String,
    /// `type_name` for messages and enums, else the scalar type's name (`Uint32`).
    pub kind: String,
    pub repeated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageShape {
    pub fields: BTreeMap<i32, FieldShape>,
    pub reserved_numbers: Vec<i32>,
    pub reserved_names: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub messages: BTreeMap<String, MessageShape>,
    pub enums: BTreeMap<String, BTreeMap<i32, String>>,
}

/// The shape of every message and enum in `descriptor_set` (an encoded `FileDescriptorSet`).
pub fn shape_of(descriptor_set: &[u8]) -> Result<Shape, prost::DecodeError> {
    let set = FileDescriptorSet::decode(descriptor_set)?;
    let mut shape = Shape::default();
    for file in &set.file {
        let prefix = format!(".{}", file.package());
        file.message_type.iter().for_each(|m| add_message(&mut shape, &prefix, m));
        file.enum_type.iter().for_each(|e| add_enum(&mut shape, &prefix, e));
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
        (field.number(), FieldShape { name: field.name().to_string(), kind, repeated })
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

/// Every way `new` breaks a reader built from `old`; empty means `new` is additive.
pub fn breaking_changes(old: &Shape, new: &Shape) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, old_message) in &old.messages {
        match new.messages.get(name) {
            None => problems.push(format!("message {name} was removed")),
            Some(new_message) => compare_message(name, old_message, new_message, &mut problems),
        }
    }
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

fn compare_message(name: &str, old: &MessageShape, new: &MessageShape, problems: &mut Vec<String>) {
    for (number, old_field) in &old.fields {
        match new.fields.get(number) {
            Some(new_field) if new_field == old_field => {}
            Some(_) => problems.push(format!("{name} field {number} changed name, type or label")),
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
