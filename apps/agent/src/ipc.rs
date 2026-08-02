use std::{
    io::{self, BufRead, BufReader, Read, Write},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Listener, ListenerOptions, Stream, prelude::*,
};
use open_hub_protocol::{
    AgentEvent, ClientRequest, ErrorCode, LOCAL_SOCKET_NAME, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
    RequestCommand, ResponseData, ServerMessage,
};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PendingRequest {
    pub request: ClientRequest,
    pub reply: mpsc::SyncSender<ServerMessage>,
}

pub struct IpcHandle {
    requests: mpsc::Receiver<PendingRequest>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<AgentEvent>>>>,
}

impl IpcHandle {
    pub fn try_recv(&self) -> Result<Option<PendingRequest>> {
        match self.requests.try_recv() {
            Ok(request) => Ok(Some(request)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => bail!("IPC listener stopped unexpectedly"),
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<PendingRequest>> {
        match self.requests.recv_timeout(timeout) {
            Ok(request) => Ok(Some(request)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("IPC listener stopped unexpectedly"),
        }
    }

    pub fn publish(&self, event: AgentEvent) {
        let mut subscribers = self
            .subscribers
            .lock()
            .expect("IPC subscriber mutex poisoned");
        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }
}

pub fn start() -> Result<IpcHandle> {
    let listener = create_listener().context("failed to create the Open Hub local socket")?;
    let (request_tx, request_rx) = mpsc::channel();
    let subscribers = Arc::new(Mutex::new(Vec::new()));
    let listener_subscribers = Arc::clone(&subscribers);

    thread::Builder::new()
        .name("open-hub-ipc-listener".to_owned())
        .spawn(move || accept_connections(listener, request_tx, listener_subscribers))
        .context("failed to start the IPC listener thread")?;

    Ok(IpcHandle {
        requests: request_rx,
        subscribers,
    })
}

pub fn send_request(request_json: &str) -> Result<String> {
    if request_json.len() > MAX_MESSAGE_BYTES {
        bail!("request exceeds the {MAX_MESSAGE_BYTES}-byte IPC limit");
    }
    let stream = connect().context("could not connect to the Open Hub agent")?;
    let mut writer = &stream;
    writer.write_all(request_json.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;

    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response)?;
    if response.is_empty() {
        bail!("agent closed the IPC connection without replying");
    }
    Ok(response.trim_end().to_owned())
}

pub fn print_event_stream(max_events: Option<usize>) -> Result<()> {
    let request = ClientRequest {
        id: 1,
        protocol_version: PROTOCOL_VERSION,
        command: RequestCommand::Subscribe,
    };
    let stream = connect().context("could not connect to the Open Hub agent")?;
    let mut writer = &stream;
    serde_json::to_writer(&mut writer, &request)?;
    writer.write_all(b"\n")?;
    writer.flush()?;

    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut event_count = 0;
    for line in BufReader::new(&stream).lines() {
        let line = line?;
        writeln!(output, "{line}")?;
        output.flush()?;
        if matches!(
            serde_json::from_str::<ServerMessage>(&line)?,
            ServerMessage::Event { .. }
        ) {
            event_count += 1;
            if max_events == Some(event_count) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn create_listener() -> io::Result<Listener> {
    if GenericNamespaced::is_supported() {
        let name = LOCAL_SOCKET_NAME.to_ns_name::<GenericNamespaced>()?;
        ListenerOptions::new()
            .name(name)
            .try_overwrite(true)
            .create_sync()
    } else {
        let path = std::env::temp_dir().join(LOCAL_SOCKET_NAME);
        let path = path.to_string_lossy();
        let name = path.as_ref().to_fs_name::<GenericFilePath>()?;
        ListenerOptions::new()
            .name(name)
            .try_overwrite(true)
            .create_sync()
    }
}

fn connect() -> io::Result<Stream> {
    if GenericNamespaced::is_supported() {
        Stream::connect(LOCAL_SOCKET_NAME.to_ns_name::<GenericNamespaced>()?)
    } else {
        let path = std::env::temp_dir().join(LOCAL_SOCKET_NAME);
        let path = path.to_string_lossy();
        Stream::connect(path.as_ref().to_fs_name::<GenericFilePath>()?)
    }
}

fn accept_connections(
    listener: Listener,
    requests: mpsc::Sender<PendingRequest>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<AgentEvent>>>>,
) {
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let requests = requests.clone();
                let subscribers = Arc::clone(&subscribers);
                if let Err(error) = thread::Builder::new()
                    .name("open-hub-ipc-client".to_owned())
                    .spawn(move || {
                        if let Err(error) = handle_client(stream, requests, subscribers) {
                            eprintln!("IPC client disconnected: {error:#}");
                        }
                    })
                {
                    eprintln!("Could not start IPC client thread: {error}");
                }
            }
            Err(error) => eprintln!("IPC accept failed: {error}"),
        }
    }
}

fn handle_client(
    stream: Stream,
    requests: mpsc::Sender<PendingRequest>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<AgentEvent>>>>,
) -> Result<()> {
    let mut reader = BufReader::new(&stream);
    loop {
        let Some(line) = read_line_limited(&mut reader)? else {
            return Ok(());
        };
        let request = match serde_json::from_str::<ClientRequest>(&line) {
            Ok(request) => request,
            Err(error) => {
                write_message(
                    &stream,
                    &ServerMessage::error(0, ErrorCode::InvalidRequest, error.to_string()),
                )?;
                continue;
            }
        };
        if request.protocol_version != PROTOCOL_VERSION {
            write_message(
                &stream,
                &ServerMessage::error(
                    request.id,
                    ErrorCode::VersionMismatch,
                    format!(
                        "client protocol version {} is incompatible with agent version {PROTOCOL_VERSION}",
                        request.protocol_version
                    ),
                ),
            )?;
            continue;
        }

        if matches!(request.command, RequestCommand::Subscribe) {
            let (event_tx, event_rx) = mpsc::channel();
            subscribers
                .lock()
                .expect("IPC subscriber mutex poisoned")
                .push(event_tx);
            write_message(
                &stream,
                &ServerMessage::success(request.id, ResponseData::Subscribed),
            )?;
            for event in event_rx {
                write_message(&stream, &ServerMessage::event(event))?;
            }
            return Ok(());
        }

        let request_id = request.id;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        requests
            .send(PendingRequest {
                request,
                reply: reply_tx,
            })
            .context("agent request loop stopped")?;
        let response = reply_rx.recv_timeout(RESPONSE_TIMEOUT).unwrap_or_else(|_| {
            ServerMessage::error(
                request_id,
                ErrorCode::Internal,
                "agent did not answer before the IPC timeout",
            )
        });
        write_message(&stream, &response)?;
    }
}

fn read_line_limited(reader: &mut impl BufRead) -> Result<Option<String>> {
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
    String::from_utf8(bytes)
        .context("IPC request is not valid UTF-8")
        .map(Some)
}

fn write_message(stream: &Stream, message: &ServerMessage) -> Result<()> {
    let mut writer = stream;
    serde_json::to_writer(&mut writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_one_bounded_json_line() {
        let input = b"{\"id\":1}\r\nremaining";
        let mut reader = BufReader::new(&input[..]);
        assert_eq!(
            read_line_limited(&mut reader).unwrap(),
            Some("{\"id\":1}".to_owned())
        );
    }

    #[test]
    fn rejects_an_oversized_line() {
        let input = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut reader = BufReader::new(input.as_slice());
        assert!(read_line_limited(&mut reader).is_err());
    }
}
