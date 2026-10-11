//! Golden fixtures: `proto/golden/<name>.json` holds `{type, message}` in proto3 JSON,
//! `<name>.bin` the encoding this crate produces for it. Both repos decode them.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use prost::Message;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::v1::{
    BridgeMessage, Chunk, FetchSourceRequest, GenerationRequest, JudgeMessage, UploadChunk, UploadReceipt,
};

#[derive(Debug, Deserialize)]
pub struct Fixture {
    #[serde(rename = "type")]
    pub type_name: String,
    pub message: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum GoldenError {
    #[error("unknown fixture type {0}")]
    UnknownType(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/golden")
}

/// Fixture names (file stems), sorted.
pub fn fixture_names() -> Result<Vec<String>, GoldenError> {
    let mut names: Vec<String> = fs::read_dir(golden_dir())?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter_map(|file| file.strip_suffix(".json").map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

pub fn read_fixture(name: &str) -> Result<Fixture, GoldenError> {
    let text = fs::read_to_string(golden_dir().join(format!("{name}.json")))?;
    Ok(serde_json::from_str(&text)?)
}

/// The binary encoding of a fixture's JSON message.
pub fn encode(fixture: &Fixture) -> Result<Vec<u8>, GoldenError> {
    fn of<M: Message + DeserializeOwned>(json: &Value) -> Result<Vec<u8>, GoldenError> {
        Ok(serde_json::from_value::<M>(json.clone())?.encode_to_vec())
    }
    match fixture.type_name.as_str() {
        "JudgeMessage" => of::<JudgeMessage>(&fixture.message),
        "BridgeMessage" => of::<BridgeMessage>(&fixture.message),
        "FetchSourceRequest" => of::<FetchSourceRequest>(&fixture.message),
        "Chunk" => of::<Chunk>(&fixture.message),
        "GenerationRequest" => of::<GenerationRequest>(&fixture.message),
        "UploadChunk" => of::<UploadChunk>(&fixture.message),
        "UploadReceipt" => of::<UploadReceipt>(&fixture.message),
        other => Err(GoldenError::UnknownType(other.to_string())),
    }
}

/// `bytes` decoded as `type_name`, then printed as proto3 JSON.
pub fn decode_to_json(type_name: &str, bytes: &[u8]) -> Result<Value, GoldenError> {
    fn of<M: Message + Default + Serialize>(bytes: &[u8]) -> Result<Value, GoldenError> {
        Ok(serde_json::to_value(M::decode(bytes)?)?)
    }
    match type_name {
        "JudgeMessage" => of::<JudgeMessage>(bytes),
        "BridgeMessage" => of::<BridgeMessage>(bytes),
        "FetchSourceRequest" => of::<FetchSourceRequest>(bytes),
        "Chunk" => of::<Chunk>(bytes),
        "GenerationRequest" => of::<GenerationRequest>(bytes),
        "UploadChunk" => of::<UploadChunk>(bytes),
        "UploadReceipt" => of::<UploadReceipt>(bytes),
        other => Err(GoldenError::UnknownType(other.to_string())),
    }
}
