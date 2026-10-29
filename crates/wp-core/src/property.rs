use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 属性类型，对齐 Wallpaper Engine 的可调参数模型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PropertyType {
    Bool {
        default: bool,
    },
    Slider {
        min: f32,
        max: f32,
        step: f32,
        default: f32,
    },
    Color {
        default: [u8; 3],
    },
    Combo {
        options: Vec<String>,
        default: usize,
    },
    Text {
        default: String,
    },
    File {
        #[serde(default)]
        filter: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Bool(bool),
    Number(f32),
    Text(String),
    Color([u8; 3]),
    Index(usize),
}

impl PropertyValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::Number(v) => Some(*v),
            Self::Index(v) => Some(*v as f32),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(v) => Some(v.as_str()),
            _ => None,
        }
    }
}

impl From<bool> for PropertyValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<f32> for PropertyValue {
    fn from(v: f32) -> Self {
        Self::Number(v)
    }
}

impl From<String> for PropertyValue {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<&str> for PropertyValue {
    fn from(v: &str) -> Self {
        Self::Text(v.to_string())
    }
}

/// 属性定义集合，键为属性 key。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertySchema {
    pub entries: BTreeMap<String, PropertyEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyEntry {
    pub label: String,
    #[serde(flatten)]
    pub kind: PropertyType,
}

/// 用户设定的属性值集合。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertyValues {
    pub values: BTreeMap<String, PropertyValue>,
}

impl PropertyValues {
    /// 用 schema 的默认值填充缺失项，保证渲染端拿到的值总是完整。
    pub fn with_defaults(&self, schema: &PropertySchema) -> Self {
        let mut out = self.clone();
        for (key, entry) in &schema.entries {
            if out.values.contains_key(key) {
                continue;
            }
            if let Some(v) = default_value(&entry.kind) {
                out.values.insert(key.clone(), v);
            }
        }
        out
    }

    pub fn bool_or(&self, key: &str, fallback: bool) -> bool {
        self.values
            .get(key)
            .and_then(PropertyValue::as_bool)
            .unwrap_or(fallback)
    }
}

pub fn default_value(kind: &PropertyType) -> Option<PropertyValue> {
    Some(match kind {
        PropertyType::Bool { default } => PropertyValue::Bool(*default),
        PropertyType::Slider { default, .. } => PropertyValue::Number(*default),
        PropertyType::Color { default } => PropertyValue::Color(*default),
        PropertyType::Combo { default, .. } => PropertyValue::Index(*default),
        PropertyType::Text { default } => PropertyValue::Text(default.clone()),
        PropertyType::File { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> PropertySchema {
        let mut s = PropertySchema::default();
        s.entries.insert(
            "speed".into(),
            PropertyEntry {
                label: "速度".into(),
                kind: PropertyType::Slider {
                    min: 0.0,
                    max: 2.0,
                    step: 0.1,
                    default: 1.0,
                },
            },
        );
        s.entries.insert(
            "glow".into(),
            PropertyEntry {
                label: "发光".into(),
                kind: PropertyType::Bool { default: true },
            },
        );
        s
    }

    #[test]
    fn defaults_fill_missing() {
        let filled = PropertyValues::default().with_defaults(&schema());
        assert_eq!(filled.values.len(), 2);
        assert!(filled.bool_or("glow", false));
        assert_eq!(filled.values["speed"], PropertyValue::Number(1.0));
    }

    #[test]
    fn existing_values_win() {
        let mut v = PropertyValues::default();
        v.values.insert("glow".into(), PropertyValue::Bool(false));
        let filled = v.with_defaults(&schema());
        assert!(!filled.bool_or("glow", true));
    }
}
