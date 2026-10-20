//! `refusal-classes.txt` lists every `RefusalClass` serde spelling, one per
//! line in declaration order; each host's tests hold its own closed set to
//! that list.

use appa_runtime_api::RefusalClass;
use serde::Deserializer;
use serde::de::{Error as _, Visitor, value::Error};

const LISTED: &str = include_str!("../refusal-classes.txt");

/// A deserializer that answers nothing and records the variant names
/// the derived `Deserialize` hands `deserialize_enum` — the enum's
/// serde spellings, read off the derive itself.
struct Variants<'a>(&'a mut &'static [&'static str]);

impl<'de> Deserializer<'de> for Variants<'_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(Error::custom("not an enum"))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Error> {
        *self.0 = variants;
        Err(Error::custom("recorded"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option
        unit unit_struct newtype_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

#[test]
fn the_listed_spellings_are_the_enums_exactly() {
    let mut variants: &'static [&'static str] = &[];
    assert!(<RefusalClass as serde::Deserialize>::deserialize(Variants(&mut variants)).is_err());
    let listed: Vec<&str> = LISTED.lines().collect();
    assert_eq!(listed, variants);
    for spelling in listed {
        let class: RefusalClass = serde_json::from_value(serde_json::Value::from(spelling)).unwrap();
        assert_eq!(serde_json::to_value(class).unwrap(), spelling);
    }
}
