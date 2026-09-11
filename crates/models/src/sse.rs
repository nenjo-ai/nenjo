//! Shared byte framing for provider server-sent events.
//!
//! Providers interpret completion and error events; this module preserves UTF-8
//! and data lines across arbitrary HTTP chunk boundaries.

use anyhow::Context;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub(crate) name: Option<String>,
    pub(crate) data: String,
}

pub(crate) fn take_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    let (index, delimiter_len) = match (lf, crlf) {
        (Some(left), Some(right)) => {
            if left.0 <= right.0 {
                left
            } else {
                right
            }
        }
        (Some(found), None) | (None, Some(found)) => found,
        (None, None) => return None,
    };
    let remaining = buffer.split_off(index + delimiter_len);
    let mut frame = std::mem::replace(buffer, remaining);
    frame.truncate(index);
    Some(frame)
}

pub(crate) fn decode_event(frame: &[u8]) -> anyhow::Result<Option<SseEvent>> {
    let frame = std::str::from_utf8(frame).context("provider SSE event contained invalid UTF-8")?;
    let mut name = None;
    let mut lines = Vec::new();
    for line in frame.lines() {
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => name = Some(value.to_string()),
            "data" => lines.push(value),
            _ => {}
        }
    }
    let data = lines.join("\n");
    Ok((!data.trim().is_empty()).then_some(SseEvent { name, data }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_preserve_utf8_and_crlf_when_every_byte_arrives_separately() {
        let input = concat!(
            ": heartbeat\r\n\r\n",
            "event: response.output_text.delta\r\n",
            "data: {\"delta\":\"caf\u{e9} \u{1f600}\"}\r\n\r\n",
            "data: [DONE]\n\n"
        );
        let mut buffer = Vec::new();
        let mut events = Vec::new();
        for byte in input.bytes() {
            buffer.push(byte);
            while let Some(frame) = take_frame(&mut buffer) {
                if let Some(event) = decode_event(&frame).unwrap() {
                    events.push(event);
                }
            }
        }
        assert!(buffer.is_empty());
        assert_eq!(
            events,
            vec![
                SseEvent {
                    name: Some("response.output_text.delta".into()),
                    data: "{\"delta\":\"caf\u{e9} \u{1f600}\"}".into(),
                },
                SseEvent {
                    name: None,
                    data: "[DONE]".into()
                },
            ]
        );
    }

    #[test]
    fn multiline_data_and_comments_follow_sse_framing() {
        let event = decode_event(b"event: response.completed\ndata: {\n: comment\ndata: \"type\":\"response.completed\"\ndata: }").unwrap().unwrap();
        assert_eq!(event.name.as_deref(), Some("response.completed"));
        assert_eq!(event.data, "{\n\"type\":\"response.completed\"\n}");
        assert!(decode_event(b": heartbeat").unwrap().is_none());
        assert!(decode_event(b"data: \xff").is_err());
    }
}
