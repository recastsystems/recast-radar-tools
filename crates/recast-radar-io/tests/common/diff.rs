//! Structural diff of two volumes (through their JSON form), for fixed
//! points: a volume that must read back exactly.

use recast_radar_core::model::Volume;
use serde_json::Value as Json;

/// The first path where two JSON trees differ.
pub fn first_difference(a: &Json, b: &Json, path: &str) -> Option<String> {
    match (a, b) {
        (Json::Object(x), Json::Object(y)) => {
            for (key, value) in x {
                let child = format!("{path}.{key}");
                match y.get(key) {
                    Some(other) => {
                        if let Some(found) = first_difference(value, other, &child) {
                            return Some(found);
                        }
                    }
                    None => return Some(format!("{child}: missing on the right")),
                }
            }
            y.keys()
                .find(|key| !x.contains_key(*key))
                .map(|key| format!("{path}.{key}: missing on the left"))
        }
        (Json::Array(x), Json::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: length {} != {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(index, (p, q))| first_difference(p, q, &format!("{path}[{index}]")))
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {a} != {b}")),
    }
}

/// The first difference between two volumes (ray times to a microsecond),
/// `None` when they are equal.
pub fn volume_difference(first: &Volume, second: &Volume) -> Option<String> {
    let mut second = second.clone();
    if first.sweeps.len() == second.sweeps.len() {
        for (a, b) in first.sweeps.iter().zip(&mut second.sweeps) {
            if a.rays.time_s.len() == b.rays.time_s.len()
                && a.rays
                    .time_s
                    .iter()
                    .zip(&b.rays.time_s)
                    .all(|(x, y)| (x - y).abs() <= 1e-6 || (x.is_nan() && y.is_nan()))
            {
                b.rays.time_s.clone_from(&a.rays.time_s);
            }
        }
    }
    if *first == second {
        return None;
    }
    // NaN != NaN: two volumes whose JSON (NaN written as null) agrees differ
    // only where both hold NaN.
    let a = serde_json::to_value(first).expect("serialize");
    let b = serde_json::to_value(&second).expect("serialize");
    first_difference(&a, &b, "volume")
}

/// Assert two volumes are equal, reporting the first difference.
pub fn assert_same_volume(first: &Volume, second: &Volume, what: &str) {
    if first != second {
        let a = serde_json::to_value(first).expect("serialize");
        let b = serde_json::to_value(second).expect("serialize");
        panic!(
            "{what}: the volume read back differs at {}",
            first_difference(&a, &b, "volume").unwrap_or_else(|| "(no JSON difference)".into())
        );
    }
}
