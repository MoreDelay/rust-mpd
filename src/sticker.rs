use crate::error::ParseError;
use std::str::FromStr;

#[allow(dead_code)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Sticker {
    pub name: String,
    pub value: String,
}

impl FromStr for Sticker {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Sticker, ParseError> {
        let mut parts = s.splitn(2, '=');
        match (parts.next(), parts.next()) {
            (Some(name), Some(value)) => Ok(Sticker { name: name.to_owned(), value: value.to_owned() }),
            _ => Err(ParseError::BadValue(s.to_owned())),
        }
    }
}
