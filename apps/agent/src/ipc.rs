use std::{
    io::{self, BufRead, BufReader, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use gflick_protocol::{
    AgentEvent, ClientRequest, ErrorCode, LOCAL_SOCKET_NAME, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
    RequestCommand, ResponseData, ServerMessage,
};
use interprocess::TryClone;
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Listener, ListenerOptions, Stream, prelude::*,
};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Identifies subscribers so a disconnect can remove the right one.
static NEXT_SUBSCRIBER_ID: AtomicU64 = AtomicU64::new(1);

/// True when an I/O failure just means the peer hung up.
fn is_client_hangup(error: &anyhow::Error) -> bool {
    /// Windows reports a half-closed pipe as this, not as a broken pipe.
    const ERROR_NO_DATA: i32 = 232;

    error.chain().any(|cause| {
        cause.downcast_ref::<io::Error>().is_some_and(|io_error| {
            matches!(
                io_error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::UnexpectedEof
            ) || io_error.raw_os_error() == Some(ERROR_NO_DATA)
        })
    })
}

pub struct PendingRequest {
    pub request: ClientRequest,
    pub reply: mpsc::SyncSender<ServerMessage>,
}

/// Keyed so a disconnecting client can remove its own slot.
struct Subscriber {
    id: u64,
    events: mpsc::Sender<AgentEvent>,
}

type Subscribers = Arc<Mutex<Vec<Subscriber>>>;

/// Removes a subscriber if it has not already been removed by another cleanup path.
fn remove_subscriber(subscribers: &Subscribers, subscriber_id: u64) {
    subscribers
        .lock()
        .expect("IPC subscriber mutex poisoned")
        .retain(|subscriber| subscriber.id != subscriber_id);
}

/// Rolls back a newly inserted subscriber until the hangup watcher owns its cleanup.
struct SubscriberRegistration {
    subscribers: Subscribers,
    subscriber_id: u64,
    armed: bool,
}

impl SubscriberRegistration {
    fn new(subscribers: Subscribers, subscriber_id: u64, events: mpsc::Sender<AgentEvent>) -> Self {
        subscribers
            .lock()
            .expect("IPC subscriber mutex poisoned")
            .push(Subscriber {
                id: subscriber_id,
                events,
            });

        Self {
            subscribers,
            subscriber_id,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for SubscriberRegistration {
    fn drop(&mut self) {
        if self.armed {
            remove_subscriber(&self.subscribers, self.subscriber_id);
        }
    }
}

fn complete_subscription_registration(
    registration: SubscriberRegistration,
    acknowledge: impl FnOnce() -> Result<()>,
    start_watcher: impl FnOnce() -> Result<()>,
) -> Result<()> {
    acknowledge()?;
    start_watcher()?;
    registration.disarm();
    Ok(())
}

pub struct IpcHandle {
    requests: mpsc::Receiver<PendingRequest>,
    subscribers: Subscribers,
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
        subscribers.retain(|subscriber| subscriber.events.send(event.clone()).is_ok());
    }
}

pub fn start() -> Result<IpcHandle> {
    let listener = create_listener().context("failed to create the GFlick local socket")?;
    let (request_tx, request_rx) = mpsc::channel();
    let subscribers = Arc::new(Mutex::new(Vec::new()));
    let listener_subscribers = Arc::clone(&subscribers);

    thread::Builder::new()
        .name("gflick-ipc-listener".to_owned())
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
    let stream = connect().context("could not connect to the GFlick agent")?;
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
    let stream = connect().context("could not connect to the GFlick agent")?;
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
    subscribers: Subscribers,
) {
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let requests = requests.clone();
                let subscribers = Arc::clone(&subscribers);
                if let Err(error) = thread::Builder::new()
                    .name("gflick-ipc-client".to_owned())
                    .spawn(move || {
                        // A hangup is routine; only real faults are printed.
                        if let Err(error) = handle_client(stream, requests, subscribers)
                            && !is_client_hangup(&error)
                        {
                            eprintln!("IPC client failed: {error:#}");
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
    subscribers: Subscribers,
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
            let subscriber_id = NEXT_SUBSCRIBER_ID.fetch_add(1, Ordering::Relaxed);
            let registration =
                SubscriberRegistration::new(Arc::clone(&subscribers), subscriber_id, event_tx);
            complete_subscription_registration(
                registration,
                || {
                    write_message(
                        &stream,
                        &ServerMessage::success(request.id, ResponseData::Subscribed),
                    )
                },
                || {
                    // Without this, an idle subscriber only notices a hangup when the
                    // next event finally fails to write.
                    spawn_hangup_watcher(&stream, subscriber_id, Arc::clone(&subscribers))
                },
            )?;

            loop {
                match event_rx.recv() {
                    Ok(event) => write_message(&stream, &ServerMessage::event(event))?,
                    // Pruned by the watcher: the client is gone.
                    Err(mpsc::RecvError) => return Ok(()),
                }
            }
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

/// Drops a subscriber once its read half reports EOF.
///
/// A write probe is not an option: clients parse every line, so even a bare
/// newline would be a parse error on a healthy connection.
fn spawn_hangup_watcher(
    stream: &Stream,
    subscriber_id: u64,
    subscribers: Subscribers,
) -> Result<()> {
    let watched = stream
        .try_clone()
        .context("could not watch the subscriber connection for disconnect")?;

    thread::Builder::new()
        .name("gflick-ipc-hangup".to_owned())
        .spawn(move || {
            let mut reader = &watched;
            let mut discard = [0_u8; 64];
            // Subscribers send nothing; only the end of the stream matters.
            while matches!(reader.read(&mut discard), Ok(1..)) {}
            remove_subscriber(&subscribers, subscriber_id);
        })
        .with_context(|| format!("failed to watch subscription {subscriber_id} for disconnect"))?;

    Ok(())
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

    #[test]
    fn treats_peer_closure_as_a_hangup() {
        for kind in [
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::UnexpectedEof,
        ] {
            let error = anyhow::Error::from(io::Error::new(kind, "peer left"));
            assert!(is_client_hangup(&error), "{kind:?} should be a hangup");
        }
    }

    #[test]
    fn treats_windows_pipe_closing_as_a_hangup() {
        let error = anyhow::Error::from(io::Error::from_raw_os_error(232));
        assert!(is_client_hangup(&error));
    }

    #[test]
    fn finds_a_hangup_through_added_context() {
        let error = anyhow::Error::from(io::Error::new(io::ErrorKind::BrokenPipe, "peer left"))
            .context("while publishing an event");
        assert!(is_client_hangup(&error));
    }

    #[test]
    fn dropping_a_subscriber_ends_its_parked_recv() {
        // Stands in for the watcher pruning its slot on EOF.
        let (event_tx, event_rx) = mpsc::channel();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let registration = SubscriberRegistration::new(Arc::clone(&subscribers), 7, event_tx);

        remove_subscriber(&subscribers, 7);
        registration.disarm();

        assert!(subscribers.lock().expect("subscriber mutex").is_empty());
        assert!(matches!(event_rx.recv(), Err(mpsc::RecvError)));
    }

    #[test]
    fn pruning_one_subscriber_leaves_the_others_connected() {
        let (first_tx, first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let subscribers: Subscribers = Arc::new(Mutex::new(vec![
            Subscriber {
                id: 1,
                events: first_tx,
            },
            Subscriber {
                id: 2,
                events: second_tx,
            },
        ]));

        remove_subscriber(&subscribers, 1);

        assert!(matches!(first_rx.recv(), Err(mpsc::RecvError)));
        let survivors = subscribers.lock().expect("subscriber mutex");
        assert_eq!(survivors.len(), 1);
        assert!(
            survivors[0]
                .events
                .send(AgentEvent::ApplicationShuttingDown)
                .is_ok()
        );
        assert!(second_rx.recv().is_ok());
    }

    #[test]
    fn removing_a_subscriber_twice_is_harmless() {
        let (event_tx, event_rx) = mpsc::channel();
        let (survivor_tx, _survivor_rx) = mpsc::channel();
        let subscribers: Subscribers = Arc::new(Mutex::new(vec![Subscriber {
            id: 8,
            events: survivor_tx,
        }]));
        let registration = SubscriberRegistration::new(Arc::clone(&subscribers), 7, event_tx);

        // Stands in for the watcher winning the race before setup rolls back.
        remove_subscriber(&subscribers, 7);
        drop(registration);
        remove_subscriber(&subscribers, 999);

        let survivors = subscribers.lock().expect("subscriber mutex");
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].id, 8);
        assert!(matches!(event_rx.recv(), Err(mpsc::RecvError)));
    }

    #[test]
    fn acknowledgement_failure_rolls_back_subscription() {
        let (event_tx, event_rx) = mpsc::channel();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let registration = SubscriberRegistration::new(Arc::clone(&subscribers), 7, event_tx);

        let error = complete_subscription_registration(
            registration,
            || bail!("acknowledgement failed"),
            || Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "acknowledgement failed");
        assert!(subscribers.lock().expect("subscriber mutex").is_empty());
        assert!(matches!(event_rx.recv(), Err(mpsc::RecvError)));
    }

    #[test]
    fn watcher_spawn_failure_rolls_back_subscription() {
        let (event_tx, event_rx) = mpsc::channel();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let registration = SubscriberRegistration::new(Arc::clone(&subscribers), 7, event_tx);

        let error =
            complete_subscription_registration(registration, || Ok(()), || bail!("watcher failed"))
                .unwrap_err();

        assert_eq!(error.to_string(), "watcher failed");
        assert!(subscribers.lock().expect("subscriber mutex").is_empty());
        assert!(matches!(event_rx.recv(), Err(mpsc::RecvError)));
    }

    #[test]
    fn keeps_reporting_real_faults() {
        let oversized = anyhow::anyhow!("IPC message exceeds the limit");
        assert!(!is_client_hangup(&oversized));

        let denied = anyhow::Error::from(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        assert!(!is_client_hangup(&denied));
    }
}
