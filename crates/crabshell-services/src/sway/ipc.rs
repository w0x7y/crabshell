//! Sway's IPC framing, from sway-ipc(7): the magic string `i3-ipc`, the
//! payload's length and the message type as u32s in native byte order,
//! then the JSON payload.

/// Asks for the workspaces.
pub(super) const GET_WORKSPACES: u32 = 1;
/// Subscribes the connection to the events named in the payload.
pub(super) const SUBSCRIBE: u32 = 2;
/// Sent on a workspace change. Events have the high bit set.
pub(super) const WORKSPACE_EVENT: u32 = 0x8000_0000;

const MAGIC: &[u8; 6] = b"i3-ipc";
const HEADER: usize = 14;

/// The longest payload accepted either way. A workspace list is a few KiB
/// and an event carries one workspace's window tree, so a longer one means
/// the stream is broken.
pub(super) const MAX_PAYLOAD: usize = 4 << 20;

/// Appends one message to `out`.
pub(super) fn encode(kind: u32, payload: &[u8], out: &mut Vec<u8>) {
    assert!(
        payload.len() <= MAX_PAYLOAD,
        "invariant: callers send at most MAX_PAYLOAD bytes"
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    out.extend_from_slice(payload);
}

/// Takes the first whole message off the front of `inbox`: its type and
/// payload. `Ok(None)` while part of it is still to come. An error means
/// the stream can't be read any further.
pub(super) fn decode(inbox: &mut Vec<u8>) -> Result<Option<(u32, Vec<u8>)>, String> {
    let Some(header) = inbox.get(..HEADER) else {
        return Ok(None);
    };
    if header[..MAGIC.len()] != MAGIC[..] {
        return Err("a message without the i3-ipc magic".to_owned());
    }
    let word = |at: usize| {
        let bytes = header[at..at + 4].try_into();
        u32::from_ne_bytes(bytes.expect("invariant: a 4-byte slice"))
    };
    let (length, kind) = (word(6) as usize, word(10));
    if length > MAX_PAYLOAD {
        return Err(format!(
            "a {length}-byte message, over the {MAX_PAYLOAD}-byte cap"
        ));
    }
    if inbox.len() < HEADER + length {
        return Ok(None);
    }
    let payload = inbox[HEADER..HEADER + length].to_vec();
    inbox.drain(..HEADER + length);
    Ok(Some((kind, payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_endian = "little")]
    fn encode_matches_the_man_page() {
        // sway-ipc(7)'s hexdump of the `exit` command, a RUN_COMMAND (type 0).
        let mut out = Vec::new();
        encode(0, b"exit", &mut out);
        assert_eq!(
            out,
            [
                0x69, 0x33, 0x2d, 0x69, 0x70, 0x63, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x65, 0x78, 0x69, 0x74
            ]
        );
    }

    #[test]
    fn decode_waits_for_a_whole_message_and_leaves_the_next() {
        let mut stream = Vec::new();
        encode(GET_WORKSPACES, b"[]", &mut stream);
        encode(WORKSPACE_EVENT, b"{}", &mut stream);
        let mut inbox = Vec::new();
        for &byte in &stream[..15] {
            assert_eq!(decode(&mut inbox), Ok(None));
            inbox.push(byte);
        }
        assert_eq!(decode(&mut inbox), Ok(None), "one payload byte of two");
        inbox.extend_from_slice(&stream[15..]);
        assert_eq!(
            decode(&mut inbox),
            Ok(Some((GET_WORKSPACES, b"[]".to_vec())))
        );
        assert_eq!(
            decode(&mut inbox),
            Ok(Some((WORKSPACE_EVENT, b"{}".to_vec())))
        );
        assert_eq!(decode(&mut inbox), Ok(None));
        assert!(inbox.is_empty());
    }

    #[test]
    fn decode_refuses_a_bad_magic_and_an_oversize_length() {
        let mut inbox = b"i3-ipX\0\0\0\0\0\0\0\0".to_vec();
        assert!(decode(&mut inbox).is_err());
        let mut inbox = b"i3-ipc".to_vec();
        inbox.extend_from_slice(&(MAX_PAYLOAD as u32 + 1).to_ne_bytes());
        inbox.extend_from_slice(&GET_WORKSPACES.to_ne_bytes());
        let error = decode(&mut inbox).expect_err("over the cap");
        assert!(error.contains("cap"), "{error}");
        // The cap itself is fine, once it has all arrived.
        let mut inbox = b"i3-ipc".to_vec();
        inbox.extend_from_slice(&(MAX_PAYLOAD as u32).to_ne_bytes());
        inbox.extend_from_slice(&GET_WORKSPACES.to_ne_bytes());
        assert_eq!(decode(&mut inbox), Ok(None));
    }
}
