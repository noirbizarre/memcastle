//! SurrealDB's WebSocket wire formats: how one RPC message becomes bytes and back.
//!
//! The codecs are SurrealDB's own (`surrealdb-core` for JSON and CBOR,
//! `surrealdb-types` for the binary `flatbuffers` format the Rust SDK speaks),
//! because a client only understands the encoding it asked for byte-for-byte.
//! Nothing here interprets a request; that is [`super::connection`]'s job.

use axum::extract::ws::Message;
use surrealdb::types::{Error as TypesError, SurrealValue, Value};
use surrealdb_core::rpc::format::{cbor, json};
use surrealdb_rpc::{DbResponse, Request, error::invalid_request, error::parse_error};

/// The subprotocols offered during the WebSocket handshake, in preference
/// order. These are the names SurrealDB's own server and clients use.
pub(super) const PROTOCOLS: [&str; 3] = ["flatbuffers", "cbor", "json"];

/// How deeply nested a decoded value may be. Bounds the recursion a hostile
/// message can force; SurrealDB's own server applies the same kind of limit.
const NESTING_LIMIT: usize = 100;

/// The encoding a connection negotiated for its whole lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    /// Text frames carrying JSON.
    Json,
    /// Binary frames carrying CBOR.
    Cbor,
    /// Binary frames carrying SurrealDB's flatbuffers encoding.
    Flatbuffers,
}

impl Format {
    /// The format for the subprotocol the handshake settled on.
    ///
    /// A client that offered none (a `websocat` session, a hand-written test)
    /// gets JSON: it is the only encoding a person can type, and refusing the
    /// connection would make the endpoint harder to probe for no safety gain.
    pub(super) fn negotiated(protocol: Option<&str>) -> Self {
        match protocol {
            Some("flatbuffers") => Self::Flatbuffers,
            Some("cbor") => Self::Cbor,
            _ => Self::Json,
        }
    }

    /// Decode one incoming message into a request.
    ///
    /// # Errors
    ///
    /// A wire-shaped error (parse or invalid request) to send back to the
    /// client; the connection stays open, as SurrealDB's server does.
    pub(super) fn decode_request(self, bytes: &[u8]) -> Result<Request, TypesError> {
        let value = match self {
            Self::Json => json::decode(bytes, NESTING_LIMIT),
            Self::Cbor => cbor::decode(bytes, NESTING_LIMIT),
            Self::Flatbuffers => surrealdb::types::decode::<Value>(bytes),
        }
        .map_err(|_| parse_error())?;
        match value {
            Value::Object(object) => Request::from_object(object),
            _ => Err(invalid_request()),
        }
    }

    /// Encode a response as the frame this format travels in.
    ///
    /// # Errors
    ///
    /// The encoder's error; callers log it and drop the connection, because a
    /// response that cannot be encoded cannot be reported to the client either.
    pub(super) fn encode_response(self, response: DbResponse) -> Result<Message, String> {
        let value = response.into_value();
        // The codecs report `anyhow` errors; only the message matters here.
        let message = match self {
            Self::Json => json::encode_str(value).map(|text| Message::Text(text.into())),
            Self::Cbor => cbor::encode(value).map(|bytes| Message::Binary(bytes.into())),
            Self::Flatbuffers => {
                surrealdb::types::encode(&value).map(|bytes| Message::Binary(bytes.into()))
            }
        };
        message.map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surrealdb::types::object;
    use surrealdb_rpc::{DbResult, Method};

    fn ping_request() -> Value {
        Value::Object(object! {
            id: 7,
            method: "ping",
            params: Value::Array(surrealdb::types::Array::new()),
        })
    }

    #[test]
    fn a_connection_without_a_subprotocol_speaks_json() {
        assert_eq!(Format::negotiated(None), Format::Json);
        assert_eq!(Format::negotiated(Some("nonsense")), Format::Json);
    }

    #[test]
    fn each_offered_subprotocol_maps_to_its_own_format() {
        assert_eq!(Format::negotiated(Some("cbor")), Format::Cbor);
        assert_eq!(Format::negotiated(Some("flatbuffers")), Format::Flatbuffers);
        assert_eq!(Format::negotiated(Some("json")), Format::Json);
    }

    #[test]
    fn a_ping_request_decodes_the_same_from_every_format() {
        let value = ping_request();
        let encoded = [
            (Format::Json, json::encode(value.clone()).unwrap()),
            (Format::Cbor, cbor::encode(value.clone()).unwrap()),
            (
                Format::Flatbuffers,
                surrealdb::types::encode(&value).unwrap(),
            ),
        ];
        for (format, bytes) in encoded {
            let request = format.decode_request(&bytes).unwrap();
            assert_eq!(request.method, Method::Ping, "{format:?}");
        }
    }

    #[test]
    fn garbage_is_a_parse_error_not_a_panic() {
        for format in [Format::Json, Format::Cbor, Format::Flatbuffers] {
            assert!(format.decode_request(b"\xff\x00not a message").is_err());
        }
    }

    #[test]
    fn a_message_that_is_not_an_object_is_an_invalid_request() {
        let bytes = json::encode(Value::Array(surrealdb::types::Array::new())).unwrap();
        assert!(Format::Json.decode_request(&bytes).is_err());
    }

    #[test]
    fn a_response_encodes_to_the_frame_type_its_format_uses() {
        let response = || DbResponse::success(None, None, DbResult::Other(Value::None));
        assert!(matches!(
            Format::Json.encode_response(response()).unwrap(),
            Message::Text(_)
        ));
        assert!(matches!(
            Format::Cbor.encode_response(response()).unwrap(),
            Message::Binary(_)
        ));
        assert!(matches!(
            Format::Flatbuffers.encode_response(response()).unwrap(),
            Message::Binary(_)
        ));
    }
}
