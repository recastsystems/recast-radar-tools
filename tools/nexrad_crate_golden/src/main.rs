//! Prints, as one JSON document, every accessor of the first message 3
//! (Performance/Maintenance Data) and message 18 (RDA Adaptation Data) that
//! the danielway nexrad crates decode from each Level II file named on the
//! command line (`<id>=<path>` arguments). Floats are printed as their bit
//! patterns so that nothing is rounded. Run through
//! tools/nexrad_crate_golden.py, which generates `accessors.rs`.

use nexrad_data::volume::File;
use nexrad_decode::messages::MessageContents;
use serde_json::{json, Map, Value};

mod accessors;

/// An accessor's value as golden JSON.
trait Golden {
    fn golden(self) -> Value;
}

impl Golden for u16 {
    fn golden(self) -> Value {
        json!(self)
    }
}

impl Golden for u32 {
    fn golden(self) -> Value {
        json!(self)
    }
}

impl Golden for i16 {
    fn golden(self) -> Value {
        json!(self)
    }
}

impl Golden for f32 {
    fn golden(self) -> Value {
        json!({ "f32_bits": self.to_bits() })
    }
}

impl<const N: usize> Golden for [u16; N] {
    fn golden(self) -> Value {
        json!(self.to_vec())
    }
}

impl<const N: usize> Golden for &[u8; N] {
    fn golden(self) -> Value {
        json!({ "bytes": self.to_vec() })
    }
}

impl Golden for Vec<f32> {
    fn golden(self) -> Value {
        Value::Array(self.into_iter().map(Golden::golden).collect())
    }
}

impl Golden for Option<f32> {
    fn golden(self) -> Value {
        self.map_or(Value::Null, Golden::golden)
    }
}

impl Golden for Option<f64> {
    fn golden(self) -> Value {
        self.map_or(Value::Null, |v| json!({ "f64_bits": v.to_bits().to_string() }))
    }
}

impl Golden for Option<u32> {
    fn golden(self) -> Value {
        self.map_or(Value::Null, |v| json!(v))
    }
}

impl Golden for Option<i32> {
    fn golden(self) -> Value {
        self.map_or(Value::Null, |v| json!(v))
    }
}

impl Golden for Option<String> {
    fn golden(self) -> Value {
        self.map_or(Value::Null, |v| json!({ "text": v }))
    }
}

fn file_messages(path: &str) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let file = File::new(bytes)
        .decompress()
        .map_err(|e| format!("{path}: decompress: {e}"))?;
    let records = file.records().map_err(|e| format!("{path}: records: {e}"))?;
    let mut performance: Option<Map<String, Value>> = None;
    let mut adaptation: Option<Map<String, Value>> = None;
    for record in records {
        let record = if record.compressed() {
            match record.decompress() {
                Ok(record) => record,
                Err(e) => return Err(format!("{path}: record: {e}")),
            }
        } else {
            record
        };
        let Ok(messages) = record.messages() else {
            continue;
        };
        for message in &messages {
            match message.contents() {
                MessageContents::PerformanceMaintenanceData(m) if performance.is_none() => {
                    performance = Some(accessors::performance(m));
                }
                MessageContents::RDAAdaptationData(m) if adaptation.is_none() => {
                    adaptation = Some(accessors::adaptation(m));
                }
                _ => {}
            }
        }
        if performance.is_some() && adaptation.is_some() {
            break;
        }
    }
    Ok(json!({
        "message_3": performance.map_or(Value::Null, Value::Object),
        "message_18": adaptation.map_or(Value::Null, Value::Object),
    }))
}

fn main() {
    let mut out = Map::new();
    for arg in std::env::args().skip(1) {
        let (id, path) = arg.split_once('=').expect("<id>=<path>");
        let value = file_messages(path).unwrap_or_else(|e| json!({ "error": e }));
        out.insert(id.to_owned(), value);
    }
    println!("{}", Value::Object(out));
}
