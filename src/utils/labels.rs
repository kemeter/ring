use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

/// Deserialize labels from YAML that can be in multiple formats:
/// - Empty array: []
/// - Array of objects: [{"key": "value"}, {"key2": "value2"}]
/// - Object: {"key": "value", "key2": "value2"}
/// - Null/missing
pub fn deserialize_labels<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_yaml::Value;
    use serde::de::Error;

    let value = Value::deserialize(deserializer)?;
    let mut labels = HashMap::new();

    match value {
        Value::Sequence(seq) if seq.is_empty() => {
            // Empty array returns empty HashMap
        }
        Value::Sequence(seq) => {
            for item in seq {
                if let Value::Mapping(map) = item {
                    for (k, v) in map {
                        if let (Some(key), Some(value)) = (k.as_str(), v.as_str()) {
                            labels.insert(key.to_string(), value.to_string());
                        }
                    }
                }
            }
        }
        Value::Mapping(map) => {
            for (k, v) in map {
                if let (Some(key), Some(value)) = (k.as_str(), v.as_str()) {
                    labels.insert(key.to_string(), value.to_string());
                }
            }
        }
        Value::Null => {
            // Null returns empty HashMap
        }
        _ => {
            return Err(D::Error::custom("labels must be an array or object"));
        }
    }

    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    
    #[derive(Deserialize)]
    struct TestStruct {
        #[serde(default, deserialize_with = "deserialize_labels")]
        labels: HashMap<String, String>,
    }

    #[test]
    fn test_empty_array_labels() {
        let yaml = "labels: []";
        let test: TestStruct = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(test.labels.len(), 0);
    }

    #[test]
    fn test_object_labels() {
        let yaml = r#"
labels:
  app: "my-app"
  version: "1.0"
"#;
        let test: TestStruct = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(test.labels.len(), 2);
        assert_eq!(test.labels.get("app"), Some(&"my-app".to_string()));
        assert_eq!(test.labels.get("version"), Some(&"1.0".to_string()));
    }

    #[test]
    fn test_array_labels() {
        let yaml = r#"
labels:
  - app: "my-app"
  - version: "1.0"
"#;
        let test: TestStruct = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(test.labels.len(), 2);
        assert_eq!(test.labels.get("app"), Some(&"my-app".to_string()));
        assert_eq!(test.labels.get("version"), Some(&"1.0".to_string()));
    }

    #[test]
    fn test_missing_labels() {
        let yaml = "other: value";
        let test: TestStruct = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(test.labels.len(), 0);
    }
}