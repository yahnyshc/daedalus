use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::error::{DdlError, Result};

pub fn write_pairs(path: &Path, pairs: &[(&str, String)]) -> Result<()> {
    let mut content = String::new();
    for (key, value) in pairs {
        content.push_str(key);
        content.push(':');
        content.push_str(&hex_encode(value.as_bytes()));
        content.push('\n');
    }

    fs::write(path, content)?;
    Ok(())
}

pub fn read_pairs(path: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let content = fs::read_to_string(path)?;
    let mut result = BTreeMap::new();

    for (index, raw_line) in content.lines().enumerate() {
        if raw_line.is_empty() {
            continue;
        }

        let (key, value) = raw_line.split_once(':').ok_or_else(|| {
            DdlError::InvalidState(format!(
                "invalid metadata record in {} on line {}",
                path.display(),
                index + 1
            ))
        })?;

        let decoded = hex_decode(value).ok_or_else(|| {
            DdlError::InvalidState(format!(
                "invalid hex payload in {} on line {}",
                path.display(),
                index + 1
            ))
        })?;

        result
            .entry(key.to_string())
            .or_insert_with(Vec::new)
            .push(decoded);
    }

    Ok(result)
}

pub fn required_value(map: &BTreeMap<String, Vec<String>>, key: &str) -> Result<String> {
    let Some(values) = map.get(key) else {
        return Err(DdlError::InvalidState(format!(
            "required metadata field `{key}` is missing"
        )));
    };

    values
        .first()
        .cloned()
        .ok_or_else(|| DdlError::InvalidState(format!("required metadata field `{key}` is empty")))
}

pub fn optional_value(map: &BTreeMap<String, Vec<String>>, key: &str) -> Option<String> {
    map.get(key).and_then(|values| values.first().cloned())
}

pub fn repeated_values(map: &BTreeMap<String, Vec<String>>, key: &str) -> Vec<String> {
    map.get(key).cloned().unwrap_or_default()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(nibble_to_hex(byte >> 4));
        out.push(nibble_to_hex(byte & 0x0f));
    }
    out
}

fn hex_decode(value: &str) -> Option<String> {
    if value.len() % 2 != 0 {
        return None;
    }

    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut chars = value.as_bytes().iter();
    while let (Some(high), Some(low)) = (chars.next(), chars.next()) {
        let high = hex_to_nibble(*high)?;
        let low = hex_to_nibble(*low)?;
        bytes.push((high << 4) | low);
    }

    String::from_utf8(bytes).ok()
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!("nibble outside hex range"),
    }
}

fn hex_to_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{read_pairs, repeated_values, required_value, write_pairs};

    #[test]
    fn metadata_roundtrip_preserves_repeated_fields() {
        let path = unique_temp_path("kv-roundtrip.txt");
        write_pairs(
            &path,
            &[
                ("id", "cp_1".to_string()),
                ("arg", "codex".to_string()),
                ("arg", "run --dangerous".to_string()),
            ],
        )
        .expect("write metadata");

        let pairs = read_pairs(&path).expect("read metadata");
        assert_eq!(
            required_value(&pairs, "id").expect("required value"),
            "cp_1"
        );
        assert_eq!(
            repeated_values(&pairs, "arg"),
            vec!["codex".to_string(), "run --dangerous".to_string()]
        );

        fs::remove_file(path).expect("remove temp file");
    }

    fn unique_temp_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "ddl-test-{}-{name}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }
}
