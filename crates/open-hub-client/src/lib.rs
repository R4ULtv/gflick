//! Typed synchronous client for the Open Hub local IPC protocol.

use std::{
    io::{BufRead, BufReader, Read, Write},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, Stream, prelude::*};
use open_hub_protocol::{
    AgentEvent, ClientRequest, LOCAL_SOCKET_NAME, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
    RequestCommand, ResponseData, ResponseResult, ServerMessage,
};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Sends one typed request over a short-lived local connection.
pub fn request(command: RequestCommand) -> Result<ResponseData> {
    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let request = ClientRequest {
        id: request_id,
        protocol_version: PROTOCOL_VERSION,
        command,
    };
    let stream = connect().context("could not connect to the Open Hub agent")?;
    write_message(&stream, &request)?;
    let mut reader = BufReader::new(stream);
    match read_message(&mut reader)? {
        Some(ServerMessage::Response {
            id,
            protocol_version,
            result,
        }) => {
            validate_response_header(request_id, id, protocol_version)?;
            response_data(result)
        }
        Some(ServerMessage::Event { .. }) => {
            bail!("agent sent an event before the request response")
        }
        None => bail!("agent closed the IPC connection without replying"),
    }
}

/// Blocking subscription to agent lifecycle, settings, and battery events.
pub struct EventSubscription {
    reader: BufReader<Stream>,
}

impl EventSubscription {
    /// Connects and verifies the subscription acknowledgement.
    pub fn connect() -> Result<Self> {
        let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let request = ClientRequest {
            id: request_id,
            protocol_version: PROTOCOL_VERSION,
            command: RequestCommand::Subscribe,
        };
        let stream = connect().context("could not connect to the Open Hub agent")?;
        write_message(&stream, &request)?;
        let mut reader = BufReader::new(stream);
        match read_message(&mut reader)? {
            Some(ServerMessage::Response {
                id,
                protocol_version,
                result,
            }) => {
                validate_response_header(request_id, id, protocol_version)?;
                if !matches!(response_data(result)?, ResponseData::Subscribed) {
                    bail!("agent returned an unexpected subscription response");
                }
            }
            Some(ServerMessage::Event { .. }) => {
                bail!("agent sent an event before acknowledging the subscription")
            }
            None => bail!("agent closed the IPC connection before subscribing"),
        }
        Ok(Self { reader })
    }

    /// Blocks until the next event. `None` means the agent closed the connection.
    pub fn recv(&mut self) -> Result<Option<AgentEvent>> {
        match read_message(&mut self.reader)? {
            Some(ServerMessage::Event {
                protocol_version,
                event,
            }) => {
                validate_protocol_version(protocol_version)?;
                Ok(Some(event))
            }
            Some(ServerMessage::Response { .. }) => {
                bail!("agent sent an unexpected response on the event subscription")
            }
            None => Ok(None),
        }
    }
}

fn connect() -> std::io::Result<Stream> {
    if GenericNamespaced::is_supported() {
        Stream::connect(LOCAL_SOCKET_NAME.to_ns_name::<GenericNamespaced>()?)
    } else {
        let path = std::env::temp_dir().join(LOCAL_SOCKET_NAME);
        let path = path.to_string_lossy();
        Stream::connect(path.as_ref().to_fs_name::<GenericFilePath>()?)
    }
}

fn write_message(stream: &Stream, request: &ClientRequest) -> Result<()> {
    let mut writer = stream;
    serde_json::to_writer(&mut writer, request)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<ServerMessage>> {
    let mut bytes = Vec::new();
    let read = Read::by_ref(reader)
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > MAX_MESSAGE_BYTES {
        bail!("IPC message exceeds the {MAX_MESSAGE_BYTES}-byte limit");
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    serde_json::from_slice(&bytes)
        .context("agent returned invalid IPC JSON")
        .map(Some)
}

fn validate_response_header(expected_id: u64, id: u64, protocol_version: u16) -> Result<()> {
    validate_protocol_version(protocol_version)?;
    if id != expected_id {
        bail!("agent response ID {id} does not match request ID {expected_id}");
    }
    Ok(())
}

fn validate_protocol_version(protocol_version: u16) -> Result<()> {
    if protocol_version != PROTOCOL_VERSION {
        bail!(
            "agent protocol version {protocol_version} is incompatible with client version {PROTOCOL_VERSION}"
        );
    }
    Ok(())
}

fn response_data(result: ResponseResult) -> Result<ResponseData> {
    match result {
        ResponseResult::Success { data } => Ok(data),
        ResponseResult::Error { code, message } => {
            bail!("agent request failed ({code:?}): {message}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_one_bounded_message() {
        let input = b"{\"message\":\"response\",\"id\":1,\"protocol_version\":1,\"result\":{\"status\":\"success\",\"data\":{\"type\":\"pong\"}}}\r\n";
        let message = read_message(&mut BufReader::new(&input[..]))
            .unwrap()
            .unwrap();
        assert!(matches!(
            message,
            ServerMessage::Response {
                result: ResponseResult::Success {
                    data: ResponseData::Pong
                },
                ..
            }
        ));
    }

    #[test]
    fn rejects_oversized_messages() {
        let input = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        assert!(read_message(&mut BufReader::new(input.as_slice())).is_err());
    }

    #[test]
    fn reports_agent_errors() {
        let result = response_data(ResponseResult::Error {
            code: open_hub_protocol::ErrorCode::DeviceNotFound,
            message: "missing".to_owned(),
        });
        assert!(result.unwrap_err().to_string().contains("DeviceNotFound"));
    }
}
