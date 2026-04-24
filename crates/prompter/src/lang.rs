use std::fmt;

use oxilangtag::{LanguageTag as RawLanguageTag, LanguageTagParseError};
use serde::{Deserialize, Deserializer};

/// A validated BCP 47 language tag (RFC 5646). Carries no domain-specific
/// semantics beyond "this string is a well-formed language tag" — the model
/// does the actual interpretation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageTag(RawLanguageTag<String>);

impl LanguageTag {
    /// Parse a language tag, validating it against RFC 5646.
    pub fn parse(input: &str) -> Result<Self, LanguageTagError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(LanguageTagError::Empty);
        }
        let raw = RawLanguageTag::parse(trimmed.to_string()).map_err(LanguageTagError::Invalid)?;
        Ok(Self(raw))
    }

    /// The tag as originally written, e.g. `"fr-FR"`.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// A sentence suitable for appending to a system preamble, e.g.
    /// `"Always reply in French, even when the user writes in a different language. Do not include translations in any other language."`
    /// for `fr-FR`. Falls back to the raw tag when the primary subtag isn't
    /// in the built-in name table; frontier models handle BCP 47 tags
    /// directly. The instruction is phrased as a hard constraint so the
    /// model doesn't helpfully mirror the user's language or append
    /// parenthetical translations.
    pub fn instruction(&self) -> String {
        let name = display_name(self.0.primary_language()).unwrap_or_else(|| self.as_str());
        format!(
            "Always reply in {name}, even when the user writes in a different language. Do not include translations in any other language."
        )
    }
}

impl fmt::Display for LanguageTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LanguageTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug)]
pub enum LanguageTagError {
    Empty,
    Invalid(LanguageTagParseError),
}

impl fmt::Display for LanguageTagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("language tag must not be empty"),
            Self::Invalid(err) => write!(f, "invalid language tag: {err}"),
        }
    }
}

impl std::error::Error for LanguageTagError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Empty => None,
            Self::Invalid(err) => Some(err),
        }
    }
}

/// English display name for common primary language subtags. Kept short on
/// purpose: unknown tags pass through verbatim, which models handle fine.
/// Alphabetically by tag.
fn display_name(primary: &str) -> Option<&'static str> {
    Some(match primary.to_ascii_lowercase().as_str() {
        "ar" => "Arabic",
        "de" => "German",
        "en" => "English",
        "es" => "Spanish",
        "fr" => "French",
        "hi" => "Hindi",
        "it" => "Italian",
        "ja" => "Japanese",
        "ko" => "Korean",
        "nl" => "Dutch",
        "pl" => "Polish",
        "pt" => "Portuguese",
        "ru" => "Russian",
        "sv" => "Swedish",
        "tr" => "Turkish",
        "zh" => "Chinese",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_simple_tag() {
        let tag = LanguageTag::parse("fr").unwrap();
        assert_eq!(tag.as_str(), "fr");
    }

    #[test]
    fn parse_accepts_language_region() {
        let tag = LanguageTag::parse("fr-FR").unwrap();
        assert_eq!(tag.as_str(), "fr-FR");
    }

    #[test]
    fn parse_trims_whitespace() {
        let tag = LanguageTag::parse("  en-US  ").unwrap();
        assert_eq!(tag.as_str(), "en-US");
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(matches!(
            LanguageTag::parse(""),
            Err(LanguageTagError::Empty)
        ));
        assert!(matches!(
            LanguageTag::parse("   "),
            Err(LanguageTagError::Empty)
        ));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(matches!(
            LanguageTag::parse("not a tag!"),
            Err(LanguageTagError::Invalid(_))
        ));
    }

    #[test]
    fn instruction_uses_english_name_for_known_tags() {
        assert_eq!(
            LanguageTag::parse("fr").unwrap().instruction(),
            "Always reply in French, even when the user writes in a different language. Do not include translations in any other language."
        );
        assert_eq!(
            LanguageTag::parse("ja-JP").unwrap().instruction(),
            "Always reply in Japanese, even when the user writes in a different language. Do not include translations in any other language."
        );
    }

    #[test]
    fn instruction_falls_back_to_raw_tag_when_unknown() {
        assert_eq!(
            LanguageTag::parse("cy").unwrap().instruction(),
            "Always reply in cy, even when the user writes in a different language. Do not include translations in any other language."
        );
    }

    #[test]
    fn instruction_preserves_region_in_fallback() {
        let tag = LanguageTag::parse("br-FR").unwrap();
        assert_eq!(
            tag.instruction(),
            "Always reply in br-FR, even when the user writes in a different language. Do not include translations in any other language."
        );
    }
}
