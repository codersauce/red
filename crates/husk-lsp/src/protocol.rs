//! Bounded JSON-RPC framing for LSP stdio transport.

use std::io::{BufRead, Write};

use anyhow::Context;
use serde_json::Value;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn read_message(reader: &mut impl BufRead) -> anyhow::Result<Option<Value>> {
    let mut content_length = None;
    let mut header_bytes = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).context("read LSP header")?;
        if read == 0 {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err(anyhow::anyhow!("truncated LSP header"))
            };
        }
        header_bytes = header_bytes.saturating_add(read);
        anyhow::ensure!(
            header_bytes <= MAX_HEADER_BYTES,
            "LSP header exceeds {MAX_HEADER_BYTES} bytes"
        );
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("Content-Length:")
        {
            anyhow::ensure!(content_length.is_none(), "duplicate Content-Length header");
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("parse LSP Content-Length")?,
            );
        }
    }
    let length = content_length.context("LSP message omitted Content-Length")?;
    anyhow::ensure!(
        length <= MAX_MESSAGE_BYTES,
        "LSP message is {length} bytes; maximum is {MAX_MESSAGE_BYTES}"
    );
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .context("read LSP message body")?;
    serde_json::from_slice(&body)
        .context("parse LSP JSON message")
        .map(Some)
}

pub(crate) fn write_message(writer: &mut impl Write, message: &Value) -> anyhow::Result<()> {
    let body = serde_json::to_vec(message).context("serialize LSP JSON message")?;
    anyhow::ensure!(
        body.len() <= MAX_MESSAGE_BYTES,
        "LSP response is {} bytes; maximum is {MAX_MESSAGE_BYTES}",
        body.len()
    );
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())
        .context("write LSP response header")?;
    writer.write_all(&body).context("write LSP response body")?;
    writer.flush().context("flush LSP response")
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::json;

    use super::*;

    #[test]
    fn round_trips_a_framed_message() {
        let expected = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut output = Vec::new();
        write_message(&mut output, &expected).expect("write message");
        let mut reader = BufReader::new(Cursor::new(output));

        assert_eq!(
            read_message(&mut reader).expect("read message"),
            Some(expected)
        );
    }

    #[test]
    fn rejects_duplicate_or_oversized_headers() {
        let duplicate = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        let mut reader = BufReader::new(Cursor::new(duplicate));
        assert!(
            read_message(&mut reader)
                .expect_err("duplicate must fail")
                .to_string()
                .contains("duplicate")
        );
    }
}
