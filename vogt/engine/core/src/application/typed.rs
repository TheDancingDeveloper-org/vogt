//! Typed operations: `fn(&AppContext, P) -> Result<R, VogtError>`.
//!
//! The registry still holds one `ServiceFn` shape (`fn(&Built, Value)`), so a
//! typed handler is registered through the `typed_op!` wrapper, which is that
//! shape. Families migrate one at a time. Design: `docs/rust-port/typed-operations.md`.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::errors::VogtError;

/// Decode the transport's JSON into the operation's parameter type. A failure
/// is `InvalidRequest` with serde's message, the same mapping the hand-parsed
/// services used.
pub fn decode<P: DeserializeOwned>(params: Value) -> Result<P, VogtError> {
    serde_json::from_value(params).map_err(|err| VogtError::InvalidRequest(err.to_string()))
}

/// As `decode`, but a `null` body (a bare call with no arguments) reads as `{}`
/// so an all-defaults parameter type works.
pub fn decode_or_empty<P: DeserializeOwned>(params: Value) -> Result<P, VogtError> {
    decode(if params.is_null() {
        Value::Object(Default::default())
    } else {
        params
    })
}

/// Encode a handler's result for the transport.
pub fn encode<R: Serialize>(result: R) -> Result<Value, VogtError> {
    serde_json::to_value(result).map_err(|err| VogtError::InvalidRequest(err.to_string()))
}

// Define `pub fn $name(&Built, Value) -> Result<Value, VogtError>` from a
// typed handler `fn<C, I>(&AppContext<C, I>, P) -> Result<R, VogtError>`.
// `null_is_empty` makes a `null` body read as `{}`.
#[macro_export]
macro_rules! typed_op {
    ($name:ident, $params:ty, $handler:path) => {
        pub fn $name(
            built: &$crate::application::context::Built,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, $crate::errors::VogtError> {
            $crate::with_ctx!(built, |ctx| {
                let typed: $params = $crate::application::typed::decode(params)?;
                $crate::application::typed::encode($handler(ctx, typed)?)
            })
        }
    };
    ($name:ident, $params:ty, $handler:path, null_is_empty) => {
        pub fn $name(
            built: &$crate::application::context::Built,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, $crate::errors::VogtError> {
            $crate::with_ctx!(built, |ctx| {
                let typed: $params = $crate::application::typed::decode_or_empty(params)?;
                $crate::application::typed::encode($handler(ctx, typed)?)
            })
        }
    };
}
