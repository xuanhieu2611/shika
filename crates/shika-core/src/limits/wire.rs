//! Tolerant numeric metadata decoding. Malformed quota must never break token
//! receipt collection; unknown fields are skipped without loading content.
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

use super::{QuotaSnapshot, QuotaWindow};

#[derive(Default, Debug)]
pub(crate) struct CodexLimits {
    primary: Fields,
    secondary: Fields,
    bucket: Bucket,
}

#[derive(Default, Debug)]
struct Fields {
    used: Option<f64>,
    minutes: Option<f64>,
    reset: Option<f64>,
}

#[derive(Default)]
struct Number(Option<f64>);

#[derive(Default, Debug)]
struct Bucket(Option<bool>);
impl<'de> Deserialize<'de> for Bucket {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Bucket;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("quota bucket")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Bucket, E> {
                Ok(Bucket(Some(value == "codex")))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Bucket, E> {
                Ok(Bucket(None))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Bucket, E> {
                Ok(Bucket(Some(false)))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Bucket, E> {
                Ok(Bucket(Some(false)))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Bucket, E> {
                Ok(Bucket(Some(false)))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Bucket, E> {
                Ok(Bucket(Some(false)))
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Bucket, M::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Bucket(Some(false)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Bucket, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Bucket(Some(false)))
            }
        }
        d.deserialize_any(V)
    }
}

// A permissive visitor drains invalid arrays/maps as ignored data. This is
// intentionally not serde_json::Value: a bogus quota field can be arbitrarily large.
macro_rules! tolerate {
    () => {
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(Default::default())
        }
    };
}

impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Number;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("quota number")
            }
            fn visit_f64<E: serde::de::Error>(self, n: f64) -> Result<Number, E> {
                Ok(Number(n.is_finite().then_some(n)))
            }
            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<Number, E> {
                self.visit_f64(n as f64)
            }
            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Number, E> {
                self.visit_f64(n as f64)
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Number, M::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Number(None))
            }
            tolerate!();
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Fields;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("quota window")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Fields, M::Error> {
                let mut out = Fields::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "used_percent" | "usedPercent" => out.used = map.next_value::<Number>()?.0,
                        "window_minutes" | "windowDurationMins" => {
                            out.minutes = map.next_value::<Number>()?.0
                        }
                        "resets_at" | "resetsAt" => out.reset = map.next_value::<Number>()?.0,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(out)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Fields, E> {
                Ok(Fields::default())
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Fields, E> {
                Ok(Fields::default())
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Fields, E> {
                Ok(Fields::default())
            }
            tolerate!();
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for CodexLimits {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = CodexLimits;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("Codex quota")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<CodexLimits, M::Error> {
                let mut out = CodexLimits::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "primary" => out.primary = map.next_value()?,
                        "secondary" => out.secondary = map.next_value()?,
                        "limit_id" | "limitId" => {
                            // Bucket names are provider metadata, not conversation text.
                            out.bucket = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(out)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<CodexLimits, E> {
                Ok(CodexLimits::default())
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<CodexLimits, E> {
                Ok(CodexLimits::default())
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<CodexLimits, E> {
                Ok(CodexLimits::default())
            }
            tolerate!();
        }
        d.deserialize_any(V)
    }
}

impl CodexLimits {
    pub(crate) fn snapshot(&self, observed: i64) -> QuotaSnapshot {
        let mut out = QuotaSnapshot::default();
        // Do not label a model-specific/code-review bucket as the account's general allowance.
        if self.bucket.0 == Some(false) {
            return out;
        }
        for fields in [&self.primary, &self.secondary] {
            let Some(window) = window(fields.used, fields.reset, observed) else {
                continue;
            };
            match fields.minutes {
                Some(300.) => out.five_hour = Some(window),
                Some(10080.) => out.weekly = Some(window),
                _ => {}
            }
        }
        out
    }
}

pub(super) fn window(used: Option<f64>, reset: Option<f64>, observed: i64) -> Option<QuotaWindow> {
    let (used, reset) = (used?, reset?);
    if !(0. ..=100.).contains(&used)
        || !reset.is_finite()
        || reset.fract() != 0.
        || !(946684800. ..=4133980800.).contains(&reset)
        || observed <= 0
    {
        return None;
    }
    Some(QuotaWindow {
        used_percent: used,
        resets_at: reset as i64,
        observed_at: observed,
    })
}
