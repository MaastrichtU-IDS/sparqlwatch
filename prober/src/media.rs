//! The one place a `Content-Type` header becomes an RDF format.
//!
//! There were two, and they drifted, which is the whole reason this module
//! exists. `client.rs` stripped the header's parameters before matching, so
//! `application/rdf+xml; charset=utf-8` classified as RDF; `declare.rs` handed
//! `RdfFormat::from_media_type` the full header, got `None` for the very same
//! response, and fell back to Turtle. An RDF/XML or JSON-LD description served
//! with a charset parameter (routine) therefore parsed as Turtle, yielded zero
//! triples, and the run published `service-description = verified` with
//! `Level(0)`, which means "none served", next to `declarationsRead = false`
//! for a document we had demonstrably read, and lost every declaration in it.
//!
//! This module decides nothing about whether a media type is trustworthy: that
//! is a separate question, and `client.rs` answers it with its own allowlist
//! on top of what is here. Picking a parser for a body we are going to read
//! anyway is a weaker claim than classifying that body as RDF, and the two
//! must not be conflated again.

use oxrdfio::RdfFormat;

/// A media type's essence: the type and subtype with every parameter
/// (`; charset=utf-8`, `; q=0.9`) removed, trimmed and lowercased.
///
/// `Content-Type` parameters are part of the header's grammar, not part of the
/// media type, and no caller here varies its behaviour by charset: oxrdfio's
/// parsers read UTF-8 and the body has already been decoded to a `str` by the
/// time either caller runs.
pub fn essence(content_type: &str) -> String {
    content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

/// The RDF format `content_type` announces, ignoring its parameters, or `None`
/// if `oxrdfio` knows no format for it.
///
/// `RdfFormat::from_media_type` is deliberately given the essence and never
/// the raw header: handed a full header it returns `None` for any parameter it
/// does not itself recognise, including `charset=iso-8859-1` and any parameter
/// with no `=` in it.
pub fn rdf_format_of(content_type: &str) -> Option<RdfFormat> {
    RdfFormat::from_media_type(&essence(content_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_charset_parameter_does_not_hide_the_format() {
        // The bug this module exists for: every one of these is a routine
        // header from a real server, and `from_media_type` on the raw string
        // answers `None` for the ones with a non-UTF-8 or malformed parameter.
        for (header, want) in [
            ("application/rdf+xml", RdfFormat::RdfXml),
            ("application/rdf+xml; charset=utf-8", RdfFormat::RdfXml),
            ("application/rdf+xml;charset=UTF-8", RdfFormat::RdfXml),
            ("application/rdf+xml; charset=iso-8859-1", RdfFormat::RdfXml),
            ("APPLICATION/RDF+XML", RdfFormat::RdfXml),
            ("  text/turtle ; charset=utf-8", RdfFormat::Turtle),
            ("text/turtle; qs=0.9; charset=utf-8", RdfFormat::Turtle),
            // A parameter with no `=` at all, which `from_media_type` rejects.
            ("text/turtle; utf-8", RdfFormat::Turtle),
        ] {
            assert_eq!(rdf_format_of(header), Some(want), "{header}");
        }
        // JSON-LD carries a profile set, so it is matched by shape rather than
        // by an equality its constructor makes awkward to write.
        assert!(
            matches!(rdf_format_of("application/ld+json; charset=utf-8"), Some(RdfFormat::JsonLd { .. })),
            "a charset on JSON-LD must not hide the format either"
        );
    }

    #[test]
    fn a_media_type_no_rdf_parser_knows_is_none() {
        for header in ["application/sparql-results+json", "text/html; charset=utf-8", "", "   "] {
            assert_eq!(rdf_format_of(header), None, "{header}");
        }
    }

    #[test]
    fn the_essence_is_the_type_and_subtype_alone() {
        assert_eq!(essence("Text/Turtle; charset=utf-8"), "text/turtle");
        assert_eq!(essence("text/turtle"), "text/turtle");
        assert_eq!(essence(""), "");
    }
}

