//! The workflow-rpc message frame of the node's Borsh wRPC (the same code as the executor's `rpc::borsh`, framing only): a request
//! is `borsh(Option<u64> id, RpcApiOps op)` followed by the body as a Borsh byte vector, an answer
//! `borsh(Option<u64> id, ServerMessageKind kind, Option<RpcApiOps> op)` followed by the Borsh `Result` of the call (a
//! `ServerError` when the kind is `Error`).

use ::borsh::{BorshDeserialize, BorshSerialize};
use kaspa_rpc_core::api::ops::RpcApiOps;

/// workflow-rpc `ServerMessageKind`.
const KIND_SUCCESS: u8 = 0;
const KIND_ERROR: u8 = 1;
const KIND_NOTIFICATION: u8 = 0xff;

/// A request frame of an already encoded body: `borsh(Option<u64> id, op)`, then the body as a Borsh byte vector
/// (workflow-serializer `Payload`: u32 length, then the bytes).
pub fn frame_bytes(id: u64, op: RpcApiOps, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + payload.len());
    BorshSerialize::serialize(&(Some(id), op), &mut out).expect("a Vec never fails to grow");
    BorshSerialize::serialize(payload, &mut out).expect("a Vec never fails to grow");
    out
}

/// The bytes of a workflow-serializer `Payload` (a Borsh byte vector: u32 length, then the bytes), without copying them.
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
    /// A notification (never subscribed to; skipped).
    Notification,
}

/// Split an answer frame into header and body: `borsh(Option<u64> id, u8 kind, Option<RpcApiOps> op)`, then for a success
/// the Borsh `Result` of the call (Ok: the body, a byte vector holding the answer), for an error the `ServerError`.
pub fn parse_reply(buf: &[u8]) -> Result<Reply<'_>, String> {
    let mut rest = buf;
    let id = <Option<u64> as BorshDeserialize>::deserialize(&mut rest).map_err(|e| format!("Borsh answer header: {e}"))?;
    let (&kind, after) = rest.split_first().ok_or("Borsh answer header: no message kind")?;
    rest = after;
    if kind == KIND_NOTIFICATION {
        return Ok(Reply::Notification);
    }
    // the op the answer is about: not needed (the id says which request it answers), but it must be read past
    <Option<RpcApiOps> as BorshDeserialize>::deserialize(&mut rest).map_err(|e| format!("Borsh answer header: {e}"))?;
    match kind {
        // a success carries a Borsh `Result<body, ServerError>` (workflow-rpc `ServerResult`): tag 1 Ok, 0 Err
        KIND_SUCCESS => match rest.split_first() {
            Some((1, body)) => Ok(Reply::Ok { id, body }),
            Some((0, err)) => Ok(Reply::Err { id, message: server_error_text(err) }),
            _ => Err("Borsh answer: no result tag".into()),
        },
        KIND_ERROR => Ok(Reply::Err { id, message: server_error_text(rest) }),
        k => Err(format!("Borsh answer header: unknown message kind {k}")),
    }
}

/// The text of a workflow-rpc `ServerError` (a Borsh enum; the node's RPC errors travel as its `Text` variant).
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
        // NotificationDeserialize, RespDeserialize, Text, WebSocketError: one string
        10 | 11 | 13 | 14 => {
            <String as BorshDeserialize>::deserialize(&mut rest).unwrap_or_else(|_| format!("node error (variant {tag})"))
        }
        // Data: bytes
        12 => <Vec<u8> as BorshDeserialize>::deserialize(&mut rest)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|_| "node error (data)".into()),
        t => format!("node error (variant {t})"),
    }
}

/// An answer frame as a node sends it (tests, mock nodes): `id`, the call's `op`, and the response body already serialised by
/// workflow-serializer (`Serializer::serialize`).
#[cfg(test)]
pub fn answer_frame(id: u64, op: RpcApiOps, response: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    BorshSerialize::serialize(&(Some(id), KIND_SUCCESS, Some(op)), &mut out).unwrap();
    out.push(1);
    BorshSerialize::serialize(response, &mut out).unwrap();
    out
}

/// A refusal frame (tests, mock nodes): a `ServerError::Text`.
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
