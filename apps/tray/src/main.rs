#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod model;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use gflick_client::EventSubscription;
use gflick_protocol::{AgentEvent, DeviceState, DeviceSummary, RequestCommand, ResponseData};
use model::TrayState;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::WindowId,
};

const REFRESH_MENU_ID: &str = "gflick-refresh";
const QUIT_MENU_ID: &str = "gflick-quit";
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const STOP_REQUEST_POLL: Duration = Duration::from_millis(100);

type DeviceSnapshot = Vec<(DeviceSummary, Option<DeviceState>)>;

enum UserEvent {
    AgentConnected,
    AgentDisconnected(String),
    Snapshot(Result<DeviceSnapshot, String>),
    AgentEvent(AgentEvent),
    QuitCompleted,
    Menu(String),
}

struct TrayApp {
    proxy: EventLoopProxy<UserEvent>,
    tray: Option<TrayIcon>,
    online_icon: Icon,
    offline_icon: Icon,
    state: TrayState,
    last_error: Option<String>,
    worker_started: bool,
}

impl TrayApp {
    fn new(proxy: EventLoopProxy<UserEvent>, online_icon: Icon, offline_icon: Icon) -> Self {
        Self {
            proxy,
            tray: None,
            online_icon,
            offline_icon,
            state: TrayState::default(),
            last_error: None,
            worker_started: false,
        }
    }

    fn create_tray(&mut self) -> Result<()> {
        let menu = build_menu(&self.state, self.last_error.as_deref())?;
        let tray = TrayIconBuilder::new()
            .with_icon(self.offline_icon.clone())
            .with_icon_as_template(cfg!(target_os = "macos"))
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(true)
            .with_tooltip(self.state.tooltip())
            .build()
            .context("failed to create the gflick tray icon")?;
        self.tray = Some(tray);
        Ok(())
    }

    fn redraw(&mut self) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        let icon = if self.state.agent_connected {
            self.online_icon.clone()
        } else {
            self.offline_icon.clone()
        };
        #[cfg(target_os = "macos")]
        let icon_result = tray.set_icon_with_as_template(Some(icon), true);
        #[cfg(not(target_os = "macos"))]
        let icon_result = tray.set_icon(Some(icon));
        if let Err(error) = icon_result {
            self.last_error = Some(format!("Could not update tray icon: {error}"));
        }
        if let Err(error) = tray.set_tooltip(Some(self.state.tooltip())) {
            self.last_error = Some(format!("Could not update tray tooltip: {error}"));
        }
        tray.set_title(Some(self.state.title()));
        match build_menu(&self.state, self.last_error.as_deref()) {
            Ok(menu) => tray.set_menu(Some(Box::new(menu))),
            Err(error) => self.last_error = Some(format!("Could not update tray menu: {error:#}")),
        }
    }

    fn refresh_snapshot(&self) {
        spawn_snapshot(self.proxy.clone());
    }
}

impl ApplicationHandler<UserEvent> for TrayApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.tray.is_none()
            && let Err(error) = self.create_tray()
        {
            eprintln!("gflick tray failed to start: {error:#}");
            event_loop.exit();
            return;
        }
        if !self.worker_started {
            self.worker_started = true;
            spawn_agent_worker(self.proxy.clone());
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::AgentConnected => {
                self.state.agent_connected = true;
                self.last_error = None;
            }
            UserEvent::AgentDisconnected(error) => {
                self.state.disconnect_agent();
                self.last_error = Some(error);
            }
            UserEvent::Snapshot(Ok(snapshot)) => {
                self.state.replace(snapshot);
                self.last_error = None;
            }
            UserEvent::Snapshot(Err(error)) => {
                self.last_error = Some(error);
            }
            UserEvent::AgentEvent(AgentEvent::ApplicationShuttingDown)
            | UserEvent::QuitCompleted => {
                self.tray.take();
                event_loop.exit();
                return;
            }
            UserEvent::AgentEvent(event) => {
                self.state.apply(event);
                self.last_error = None;
            }
            UserEvent::Menu(id) if id == REFRESH_MENU_ID => self.refresh_snapshot(),
            UserEvent::Menu(id) if id == QUIT_MENU_ID => {
                spawn_application_shutdown(self.proxy.clone());
                return;
            }
            UserEvent::Menu(_) => return,
        }
        self.redraw();
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }
}

fn main() -> Result<()> {
    let (online_icon, offline_icon) = load_icons()?;
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    configure_event_loop(&mut builder);
    let event_loop = builder
        .build()
        .context("failed to create tray event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let stop_handshake = TrayStopHandshake::arm_default()?;
    spawn_stop_request_watcher(proxy.clone(), stop_handshake.clone());
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event.id.0));
    }));
    let mut app = TrayApp::new(proxy, online_icon, offline_icon);
    let result = event_loop
        .run_app(&mut app)
        .context("gflick tray event loop failed");
    stop_handshake.disarm()?;
    result
}

#[derive(Clone, Debug)]
struct TrayStopHandshake {
    ready: PathBuf,
    stop: PathBuf,
    generation: String,
}

impl TrayStopHandshake {
    fn arm_default() -> Result<Self> {
        let base =
            BaseDirs::new().context("could not determine the per-user tray data directory")?;
        let root = base.data_local_dir().join("gflick");
        let generation = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before the Unix epoch")?
                .as_nanos()
        );
        Self::arm(&root, &generation)
    }

    fn arm(root: &Path, generation: &str) -> Result<Self> {
        if generation.is_empty() {
            bail!("tray handshake generation must not be empty");
        }
        fs::create_dir_all(root).with_context(|| {
            format!("failed to create tray handshake root `{}`", root.display())
        })?;
        let handshake = Self {
            ready: root.join("tray.ready"),
            stop: root.join("tray.stop"),
            generation: generation.to_owned(),
        };
        remove_if_exists(&handshake.stop)?;
        write_token(&handshake.ready, &handshake.generation)?;
        Ok(handshake)
    }

    fn poll_stop(&self) -> Result<bool> {
        let request = read_token(&self.stop)?;
        match request.as_deref() {
            Some(request) if request == self.generation => {
                remove_if_exists(&self.stop)?;
                remove_if_token_matches(&self.ready, &self.generation)?;
                Ok(true)
            }
            Some(_) => {
                remove_if_exists(&self.stop)?;
                Ok(false)
            }
            None => {
                if self.stop.exists() {
                    remove_if_exists(&self.stop)?;
                }
                Ok(false)
            }
        }
    }

    fn disarm(&self) -> Result<()> {
        remove_if_token_matches(&self.ready, &self.generation)?;
        remove_if_token_matches(&self.stop, &self.generation)
    }
}

fn write_token(path: &Path, token: &str) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("failed to write tray handshake `{}`", path.display()))?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn read_token(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let token = contents.trim();
            if token.is_empty() {
                Ok(None)
            } else {
                Ok(Some(token.to_owned()))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("failed to read tray handshake `{}`", path.display())),
    }
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove tray handshake `{}`", path.display())),
    }
}

fn remove_if_token_matches(path: &Path, generation: &str) -> Result<()> {
    if read_token(path)?.as_deref() == Some(generation) {
        remove_if_exists(path)?;
    }
    Ok(())
}

fn spawn_stop_request_watcher(proxy: EventLoopProxy<UserEvent>, handshake: TrayStopHandshake) {
    thread::Builder::new()
        .name("gflick-tray-stop".to_owned())
        .spawn(move || stop_request_worker(proxy, &handshake))
        .expect("failed to start tray stop-request watcher");
}

fn stop_request_worker(proxy: EventLoopProxy<UserEvent>, handshake: &TrayStopHandshake) {
    loop {
        match handshake.poll_stop() {
            Ok(true) => {
                let _ = proxy.send_event(UserEvent::QuitCompleted);
                return;
            }
            Ok(false) => {}
            Err(error) => eprintln!("Could not consume tray stop request: {error:#}"),
        }
        thread::sleep(STOP_REQUEST_POLL);
    }
}

#[cfg(target_os = "macos")]
fn configure_event_loop(builder: &mut winit::event_loop::EventLoopBuilder<UserEvent>) {
    use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

    builder
        .with_activation_policy(ActivationPolicy::Accessory)
        .with_default_menu(false)
        .with_activate_ignoring_other_apps(false);
}

#[cfg(not(target_os = "macos"))]
fn configure_event_loop(_builder: &mut winit::event_loop::EventLoopBuilder<UserEvent>) {}

fn spawn_agent_worker(proxy: EventLoopProxy<UserEvent>) {
    thread::Builder::new()
        .name("gflick-tray-ipc".to_owned())
        .spawn(move || agent_worker(proxy))
        .expect("failed to start tray IPC worker");
}

fn agent_worker(proxy: EventLoopProxy<UserEvent>) {
    loop {
        match EventSubscription::connect() {
            Ok(mut subscription) => {
                if proxy.send_event(UserEvent::AgentConnected).is_err() {
                    return;
                }
                if proxy
                    .send_event(UserEvent::Snapshot(
                        query_snapshot().map_err(|error| format!("{error:#}")),
                    ))
                    .is_err()
                {
                    return;
                }
                loop {
                    match subscription.recv() {
                        Ok(Some(event)) => {
                            if proxy.send_event(UserEvent::AgentEvent(event)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {
                            let _ = proxy.send_event(UserEvent::AgentDisconnected(
                                "Agent closed the event connection".to_owned(),
                            ));
                            break;
                        }
                        Err(error) => {
                            let _ = proxy.send_event(UserEvent::AgentDisconnected(format!(
                                "Agent connection failed: {error:#}"
                            )));
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                if proxy
                    .send_event(UserEvent::AgentDisconnected(format!(
                        "Could not connect to agent: {error:#}"
                    )))
                    .is_err()
                {
                    return;
                }
            }
        }
        thread::sleep(RECONNECT_DELAY);
    }
}

fn spawn_snapshot(proxy: EventLoopProxy<UserEvent>) {
    let _ = thread::Builder::new()
        .name("gflick-tray-refresh".to_owned())
        .spawn(move || {
            let result = query_snapshot().map_err(|error| format!("{error:#}"));
            let _ = proxy.send_event(UserEvent::Snapshot(result));
        });
}

fn spawn_application_shutdown(proxy: EventLoopProxy<UserEvent>) {
    let _ = thread::Builder::new()
        .name("gflick-tray-quit".to_owned())
        .spawn(move || {
            // A disconnected agent already means there is no background component to stop.
            // Either way, the tray should honor the user's application-wide quit request.
            let _ = gflick_client::request(RequestCommand::Shutdown);
            let _ = proxy.send_event(UserEvent::QuitCompleted);
        });
}

fn query_snapshot() -> Result<DeviceSnapshot> {
    let ResponseData::Devices { devices } = gflick_client::request(RequestCommand::ListDevices)?
    else {
        bail!("agent returned an unexpected device-list response");
    };
    Ok(devices
        .into_iter()
        .map(|summary| {
            let state = if summary.ready {
                match gflick_client::request(RequestCommand::GetDevice {
                    device_id: summary.id.clone(),
                }) {
                    Ok(ResponseData::Device { device }) => Some(*device),
                    _ => None,
                }
            } else {
                None
            };
            (summary, state)
        })
        .collect())
}

fn build_menu(state: &TrayState, last_error: Option<&str>) -> Result<Menu> {
    let menu = Menu::new();
    let connection = MenuItem::new(
        if state.agent_connected {
            "Agent: connected"
        } else {
            "Agent: offline"
        },
        false,
        None,
    );
    menu.append(&connection)?;

    if let Some(error) = last_error {
        let error = MenuItem::new(format!("  {}", truncate(error, 96)), false, None);
        menu.append(&error)?;
    }
    menu.append(&PredefinedMenuItem::separator())?;

    let statuses = state.statuses();
    if statuses.is_empty() {
        let empty = MenuItem::new(
            if state.agent_connected {
                "No mouse connected"
            } else {
                "Waiting for the agent…"
            },
            false,
            None,
        );
        menu.append(&empty)?;
    } else {
        for (index, status) in statuses.iter().enumerate() {
            if index > 0 {
                menu.append(&PredefinedMenuItem::separator())?;
            }
            menu.append(&MenuItem::new(&status.name, false, None))?;
            if let Some(device_status) = status.status.as_ref() {
                menu.append(&MenuItem::new(
                    format!("  Status: {}", truncate(device_status, 88)),
                    false,
                    None,
                ))?;
            }
            menu.append(&MenuItem::new(format!("  {}", status.battery), false, None))?;
            menu.append(&MenuItem::new(format!("  {}", status.dpi), false, None))?;
        }
    }

    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(REFRESH_MENU_ID, "Refresh", true, None))?;
    menu.append(&MenuItem::with_id(QUIT_MENU_ID, "Quit gflick", true, None))?;
    Ok(menu)
}

fn load_icons() -> Result<(Icon, Icon)> {
    #[cfg(target_os = "macos")]
    let (icon_bytes, icon_format) = (
        include_bytes!("../../../assets/tray-template.png").as_slice(),
        image::ImageFormat::Png,
    );
    #[cfg(not(target_os = "macos"))]
    let (icon_bytes, icon_format) = (
        include_bytes!("../../../assets/favicon.ico").as_slice(),
        image::ImageFormat::Ico,
    );
    let image = image::load_from_memory_with_format(icon_bytes, icon_format)
        .context("failed to decode the embedded gflick icon")?
        .into_rgba8();
    let (width, height) = image.dimensions();
    let online = image.into_raw();
    let mut offline = online.clone();
    for pixel in offline.chunks_exact_mut(4) {
        let gray = ((u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])) / 3) as u8;
        pixel[0] = gray;
        pixel[1] = gray;
        pixel[2] = gray;
        pixel[3] = ((u16::from(pixel[3]) * 3) / 5) as u8;
    }
    Ok((
        Icon::from_rgba(online, width, height)?,
        Icon::from_rgba(offline, width, height)?,
    ))
}

fn truncate(value: &str, maximum_characters: usize) -> String {
    if value.chars().count() <= maximum_characters {
        return value.to_owned();
    }
    value
        .chars()
        .take(maximum_characters.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod asset_tests {
    use super::*;

    #[test]
    fn platform_icon_assets_decode_at_expected_sizes() {
        let windows = image::load_from_memory_with_format(
            include_bytes!("../../../assets/favicon.ico"),
            image::ImageFormat::Ico,
        )
        .unwrap();
        assert_eq!((windows.width(), windows.height()), (128, 128));

        let macos = image::load_from_memory_with_format(
            include_bytes!("../../../assets/tray-template.png"),
            image::ImageFormat::Png,
        )
        .unwrap()
        .into_rgba8();
        assert_eq!((macos.width(), macos.height()), (36, 36));
        assert!(macos.pixels().any(|pixel| pixel.0[3] == 0));
        assert!(
            macos
                .pixels()
                .filter(|pixel| pixel.0[3] != 0)
                .all(|pixel| pixel.0[..3] == [0, 0, 0])
        );
    }

    #[test]
    fn matching_generation_is_consumed_and_disarmed() {
        let directory = tempfile::tempdir().unwrap();
        let handshake = TrayStopHandshake::arm(directory.path(), "generation-one").unwrap();
        write_token(&handshake.stop, "generation-one").unwrap();
        assert!(handshake.poll_stop().unwrap());
        assert!(!handshake.ready.exists());
        assert!(!handshake.stop.exists());
    }

    #[test]
    fn mismatched_request_is_ignored_and_cleaned() {
        let directory = tempfile::tempdir().unwrap();
        let handshake = TrayStopHandshake::arm(directory.path(), "generation-new").unwrap();
        write_token(&handshake.stop, "generation-old").unwrap();
        assert!(!handshake.poll_stop().unwrap());
        assert_eq!(
            read_token(&handshake.ready).unwrap().as_deref(),
            Some("generation-new")
        );
        assert!(!handshake.stop.exists());
    }

    #[test]
    fn new_generation_clears_crashed_predecessor_request() {
        let directory = tempfile::tempdir().unwrap();
        let old = TrayStopHandshake::arm(directory.path(), "generation-old").unwrap();
        write_token(&old.stop, "generation-old").unwrap();
        let new = TrayStopHandshake::arm(directory.path(), "generation-new").unwrap();
        assert!(!new.stop.exists());
        assert!(!new.poll_stop().unwrap());
        old.disarm().unwrap();
        assert_eq!(
            read_token(&new.ready).unwrap().as_deref(),
            Some("generation-new")
        );
        new.disarm().unwrap();
        assert!(!new.ready.exists());
    }
}
