//! The workflow-rpc message frame of the Borsh wRPC of the node.
//!
//! This is the framing of `rpc::borsh` of the executor, and nothing else.
//! - A request is `borsh(Option<u64> id, RpcApiOps op)` followed by the body as a Borsh byte
//!   vector.
//! - An answer is `borsh(Option<u64> id, ServerMessageKind kind, Option<RpcApiOps> op)` followed
//!   by the Borsh `Result` of the call, which is a `ServerError` when the kind is `Error`.

use ::borsh::{BorshDeserialize, BorshSerialize};
use kaspa_rpc_core::api::ops::RpcApiOps;

/// The workflow-rpc `ServerMessageKind` values.
const KIND_SUCCESS: u8 = 0;
const KIND_ERROR: u8 = 1;
const KIND_NOTIFICATION: u8 = 0xff;

/// A request frame of an already encoded body.
///
/// It is `borsh(Option<u64> id, op)`, then the body as a Borsh byte vector, which is the
/// workflow-serializer `Payload` of a u32 length and the bytes.
pub fn frame_bytes(id: u64, op: RpcApiOps, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + payload.len());
    BorshSerialize::serialize(&(Some(id), op), &mut out).expect("a Vec never fails to grow");
    BorshSerialize::serialize(payload, &mut out).expect("a Vec never fails to grow");
    out
}

/// The bytes of a workflow-serializer `Payload`, without copying them.
///
/// A `Payload` is a Borsh byte vector: a u32 length, then the bytes.
pub fn payload(body: &[u8]) -> Result<&[u8], String> {
    let (len, rest) = body.split_at_checked(4).ok_or("Borsh answer body shorter than its length")?;
    let len = u32::from_le_bytes(len.try_into().expect("four bytes")) as usize;
    rest.get(..len).ok_or_else(|| format!("Borsh answer body of {} bytes, its length says {len}", rest.len()))
}

/// One answer frame of the node.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply<'a> {
    /// The answer to request `id`.
    Ok { id: Option<u64>, body: &'a [u8] },
    /// The node refused request `id`, with its message.
    Err { id: Option<u64>, message: String },
    /// A notification, which the client never subscribed to and skips.
    Notification,
}

/// Splits an answer frame into its header and body.
///
/// The header is `borsh(Option<u64> id, u8 kind, Option<RpcApiOps> op)`. A success then carries
/// the Borsh `Result` of the call, whose `Ok` holds the answer as a byte vector. An error
/// carries the `ServerError`.
pub fn parse_reply(buf: &[u8]) -> Result<Reply<'_>, String> {
    let mut rest = buf;
    let id = <Option<u64> as BorshDeserialize>::deserialize(&mut rest).map_err(|e| format!("Borsh answer header: {e}"))?;
    let (&kind, after) = rest.split_first().ok_or("Borsh answer header: no message kind")?;
    rest = after;
    if kind == KIND_NOTIFICATION {
        return Ok(Reply::Notification);
    }
    // The op of the answer is not needed, because the id says which request it answers, but it
    // must be read past.
    <Option<RpcApiOps> as BorshDeserialize>::deserialize(&mut rest).map_err(|e| format!("Borsh answer header: {e}"))?;
    match kind {
        // A success carries a Borsh `Result<body, ServerError>` (workflow-rpc `ServerResult`).
        // The tag is 1 for `Ok` and 0 for `Err`.
        KIND_SUCCESS => match rest.split_first() {
            Some((1, body)) => Ok(Reply::Ok { id, body }),
            Some((0, err)) => Ok(Reply::Err { id, message: server_error_text(err) }),
            _ => Err("Borsh answer: no result tag".into()),
        },
        KIND_ERROR => Ok(Reply::Err { id, message: server_error_text(rest) }),
        k => Err(format!("Borsh answer header: unknown message kind {k}")),
    }
}

/// The text of a workflow-rpc `ServerError`, which is a Borsh enum.
///
/// RPC errors of the node travel as its `Text` variant.
pub fn server_error_text(body: &[u8]) -> String {
    const UNIT: [&str; 10] = [
        "connection is closed",
        "RPC call timed out",
        "no data",
        "RPC method not found",
        "resource lock error",
        "not a borsh request",
        "not a serde request",
        "request serialization error",
        "request deserialization error",
        "response serialization error",
    ];
    let Some((&tag, mut rest)) = body.split_first() else {
        return "node error (empty)".into();
    };
    match tag {
        t if (t as usize) < UNIT.len() => UNIT[t as usize].into(),
        // NotificationDeserialize, RespDeserialize, Text and WebSocketError hold one string.
        10 | 11 | 13 | 14 => {
            <String as BorshDeserialize>::deserialize(&mut rest).unwrap_or_else(|_| format!("node error (variant {tag})"))
        }
        // Data holds bytes.
        12 => <Vec<u8> as BorshDeserialize>::deserialize(&mut rest)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|_| "node error (data)".into()),
        t => format!("node error (variant {t})"),
    }
}

/// An answer frame as a node sends it, for tests and mock nodes.
///
/// `response` is the response body already serialised by workflow-serializer
/// (`Serializer::serialize`).
#[cfg(test)]
pub fn answer_frame(id: u64, op: RpcApiOps, response: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    BorshSerialize::serialize(&(Some(id), KIND_SUCCESS, Some(op)), &mut out).unwrap();
    out.push(1);
    BorshSerialize::serialize(response, &mut out).unwrap();
    out
}

/// A refusal frame as a node sends it, for tests and mock nodes: a `ServerError::Text`.
#[cfg(test)]
pub fn error_frame(id: u64, op: RpcApiOps, text: &str) -> Vec<u8> {
    let mut out = vec![];
    BorshSerialize::serialize(&(Some(id), KIND_SUCCESS, Some(op)), &mut out).unwrap();
    out.push(0);
    out.push(13);
    BorshSerialize::serialize(&text.to_string(), &mut out).unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_answers() {
        let f = frame_bytes(7, RpcApiOps::GetBlockDagInfo, &[1, 2, 3]);
        let mut r = f.as_slice();
        let (id, op) = <(Option<u64>, RpcApiOps) as BorshDeserialize>::deserialize(&mut r).unwrap();
        assert_eq!((id, op), (Some(7), RpcApiOps::GetBlockDagInfo));
        assert_eq!(<Vec<u8> as BorshDeserialize>::deserialize(&mut r).unwrap(), vec![1, 2, 3]);

        let a = answer_frame(7, RpcApiOps::GetBlockDagInfo, &[9, 9]);
        match parse_reply(&a).unwrap() {
            Reply::Ok { id: Some(7), body } => assert_eq!(payload(body).unwrap(), &[9, 9]),
            other => panic!("{other:?}"),
        }
        match parse_reply(&error_frame(8, RpcApiOps::SubmitTransaction, "orphan transaction")).unwrap() {
            Reply::Err { id: Some(8), message } => assert_eq!(message, "orphan transaction"),
            other => panic!("{other:?}"),
        }
        assert!(parse_reply(&[]).is_err());
        assert!(payload(&[9, 0, 0, 0, 1]).is_err());
        assert_eq!(server_error_text(&[1]), "RPC call timed out");
        assert_eq!(server_error_text(&[]), "node error (empty)");
    }
}
