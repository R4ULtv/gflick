mod agent;
mod editor;
/// Baked into the photos by `build.rs`; compiled here only to keep its test.
#[cfg(test)]
mod filter;
mod history;
mod icons;
mod live;
mod preferences;
use preferences::PreferenceStore as _;
mod preferences_page;
mod preview;
mod services;
mod settings;
#[cfg(test)]
mod test_support;
mod theme;
mod ui;

use agent::Snapshot;
use gflick_protocol::{
    ConfigurationSource, DeviceAvailability, DeviceConnection, DeviceState, DeviceSummary,
};
use gpui_fps::fps_monitor;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Root, Selectable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    scroll::ScrollableElement,
};
use gpui_kit::{
    Anchor, App, AppContext, Bounds, Context, DevicePixels, Div, DragMoveEvent, Entity,
    IntoElement, KeyBinding, MouseButton, MouseUpEvent, Pixels, Render, RenderImage, SharedString,
    SvgSize, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, rgba, size,
};
use icons::IconName;
use theme::{
    ACCENT, BG, DANGER, DEEP, LINE, LINE_STRONG, MUTED, MUTED_2, SUCCESS, SURFACE, SURFACE_3, TEXT,
};

/// Width of the device list. The content pages need it to know their own.
const SIDEBAR_WIDTH: Pixels = px(272.0);
/// The sidebar's own padding. The dragged card is pinned to it, so the two have
/// to agree.
const SIDEBAR_PAD: Pixels = px(14.0);
/// The selected sidebar row's fill, which the dragged card also stands on.
const SELECTED_ROW: u32 = 0x30343f;

/// Logical edge of the sidebar logo. Keep in step with its `size_8()` below.
const LOGO_SIZE: f32 = 32.0;
const LOGO_SVG: &[u8] = include_bytes!("../../../website/public/favicon.svg");

actions!(gflick_settings, [ToggleFps]);

/// Set to any non-empty value to open with the performance HUD already up.
const FPS_ENV: &str = "GFLICK_FPS";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Performance,
    Details,
    Preferences,
}

struct SettingsView {
    preferences: preferences::Preferences,
    preferences_loaded: bool,
    preference_error: Option<String>,
    startup_enabled: Option<bool>,
    agent_online: Option<bool>,
    service_busy: bool,
    tray_enabled: Option<bool>,
    disconnected: std::collections::BTreeSet<String>,
    live_task: Option<gpui_kit::Task<()>>,
    pending_live: Vec<live::Update>,
    refreshing: bool,
    editors: std::collections::BTreeMap<String, Entity<editor::Editor>>,
    subscriptions: Vec<gpui_kit::Subscription>,
    logo: Option<std::sync::Arc<RenderImage>>,
    logo_pixels: i32,
    page: Page,
    preview: Entity<preview::MousePreview>,
    devices: Vec<DeviceSummary>,
    states: Vec<DeviceState>,
    selected_id: Option<String>,
    loading: bool,
    error: Option<String>,
    /// The drag in flight, if there is one. It is what draws the drop line, and
    /// what the release commits.
    drop_hint: Option<DropHint>,
    show_fps: bool,
}

/// One device as the sidebar draws it: name, link, and charge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkState {
    Ready,
    Offline,
    Disconnected,
    Checking,
    Unknown,
    Error,
}
impl LinkState {
    fn of(device: &DeviceSummary, disconnected: bool) -> Self {
        if disconnected {
            return Self::Disconnected;
        }
        match device.availability.as_ref() {
            Some(DeviceAvailability::Initializing) => Self::Checking,
            Some(DeviceAvailability::Unavailable {
                reason: gflick_protocol::DeviceUnavailableReason::CommunicationError,
                ..
            }) => Self::Error,
            _ if device.ready => Self::Ready,
            _ => Self::Offline,
        }
    }
    fn label(self, device: &DeviceSummary) -> &'static str {
        match self {
            Self::Ready => connection_label(device),
            Self::Offline if device.connection == DeviceConnection::Receiver => {
                "Mouse offline · Receiver connected"
            }
            Self::Offline => "USB connected · not responding",
            Self::Disconnected => "USB disconnected",
            Self::Checking => "Checking connection…",
            Self::Unknown => "Connection unverified",
            Self::Error => "USB connected · read error",
        }
    }
    fn icon(self, device: &DeviceSummary) -> IconName {
        match self {
            Self::Disconnected => IconName::Unplug,
            Self::Offline => offline_icon(device),
            Self::Checking => IconName::RotateCw,
            Self::Unknown | Self::Error => IconName::TriangleAlert,
            Self::Ready => connection_icon(device),
        }
    }
}

fn device_card(
    device: &DeviceSummary,
    battery: Option<&gflick_protocol::BatteryState>,
    backdrop: u32,
    status: LinkState,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .p(px(11.0))
        .rounded_lg()
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(theme::text::BODY)
                        .font_semibold()
                        .text_color(rgb(TEXT))
                        .truncate()
                        .child(device_label(device)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_size(theme::text::MICRO)
                        .font_semibold()
                        .text_color(rgb(MUTED_2))
                        .child(
                            Icon::new(status.icon(device))
                                .with_size(px(11.0))
                                .flex_shrink_0(),
                        )
                        .child(status.label(device)),
                ),
        )
        // The bolt is edged in the card's own colour, so the card has to say
        // what it is painted.
        .when(status == LinkState::Ready, |row| {
            row.child(sidebar_battery(battery, device, backdrop))
        })
}

/// A dragged device and the full card drawn under the pointer.
#[derive(Clone)]
struct DeviceDrag {
    status: LinkState,
    device: DeviceSummary,
    battery: Option<gflick_protocol::BatteryState>,
}

/// Active drag, keyed by device ID so a refresh cannot change what is moving.
#[derive(Clone, PartialEq, Eq)]
struct DropHint {
    device_id: String,
    over: usize,
}

/// The dragged card, and where in it the pointer took hold.
struct DeviceDragCard {
    drag: DeviceDrag,
    grab_x: Pixels,
}

impl Render for DeviceDragCard {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Counter the pointer offset so the preview stays pinned to the sidebar rail.
        let origin_x = window.mouse_position().x - self.grab_x;
        div().child(
            device_card(
                &self.drag.device,
                self.drag.battery.as_ref(),
                SELECTED_ROW,
                self.drag.status,
            )
            .ml(SIDEBAR_PAD - origin_x)
            // The card is off the sidebar now, so it carries its own surface
            // and the width the rail gave it.
            .w(SIDEBAR_WIDTH - SIDEBAR_PAD - SIDEBAR_PAD)
            .bg(rgb(SELECTED_ROW))
            .border_1()
            .border_color(rgb(0x545a69))
            .shadow_lg(),
        )
    }
}

impl SettingsView {
    fn new(cx: &mut Context<Self>) -> Self {
        let loaded = preferences::Preferences::load();
        let preference_error = loaded.as_ref().err().map(|e| format!("{e:#}"));
        let mut view = Self {
            preferences_loaded: loaded.is_ok(),
            preferences: loaded.unwrap_or_default(),
            preference_error,
            startup_enabled: None,
            agent_online: None,
            service_busy: false,
            tray_enabled: None,
            disconnected: Default::default(),
            live_task: None,
            pending_live: Vec::new(),
            refreshing: false,
            editors: Default::default(),
            subscriptions: Vec::new(),
            logo: None,
            logo_pixels: 0,
            page: Page::Performance,
            preview: cx.new(|_| preview::MousePreview::new(preview::MouseModel::Superlight2)),
            devices: Vec::new(),
            states: Vec::new(),
            selected_id: None,
            loading: false,
            error: None,
            drop_hint: None,
            show_fps: std::env::var_os(FPS_ENV).is_some_and(|value| !value.is_empty()),
        };
        view.start_live(cx);
        view.apply_startup_defaults(cx);
        view
    }

    fn start_live(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        let (updates, listener) = live::subscribe();
        self.live_task = Some(cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            let _listener = listener;
            while let Ok(update) = updates.recv().await {
                if view
                    .update(cx, |view, cx| view.handle_live(update, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn handle_live(&mut self, update: live::Update, cx: &mut Context<Self>) {
        if self.refreshing {
            self.pending_live.push(update);
            return;
        }
        match update {
            live::Update::Snapshot(snapshot) => {
                self.adopt(snapshot);
                self.loading = false;
                self.error = None;
                self.agent_online = Some(true);
            }
            live::Update::Offline(message) => {
                self.agent_online = Some(false);
                self.loading = false;
                self.error = Some(format!("Agent disconnected. Reconnecting… {message}"));
            }
            live::Update::Event(gflick_protocol::AgentEvent::ApplicationShuttingDown) => {
                self.agent_online = Some(false);
                self.loading = false;
                self.error = Some("Agent stopped. Reconnecting…".into());
            }
            live::Update::Event(event) => {
                live::apply(
                    &mut self.devices,
                    &mut self.states,
                    &mut self.disconnected,
                    event,
                );
            }
        }
        // Drafts belong to hardware identities, not transient USB routing IDs.
        for device in &self.devices {
            if !self.editors.contains_key(&device.id) && device.hardware_id.is_some() {
                let previous = self
                    .editors
                    .iter()
                    .find(|(_, editor)| {
                        editor.read(cx).baseline.device.hardware_id == device.hardware_id
                    })
                    .map(|(id, _)| id.clone());
                if let Some(previous) = previous {
                    let editor = self.editors.remove(&previous).unwrap();
                    editor.update(cx, |editor, _| {
                        editor.baseline.device.id = device.id.clone()
                    });
                    if self.selected_id.as_ref() == Some(&previous) {
                        self.selected_id = Some(device.id.clone());
                    }
                    self.editors.insert(device.id.clone(), editor);
                }
            }
        }
        if self
            .selected_id
            .as_ref()
            .is_none_or(|id| !self.devices.iter().any(|d| &d.id == id))
        {
            self.selected_id = self.devices.first().map(|d| d.id.clone());
        }
        cx.notify();
    }

    /// Rasterize the logo at its display size to avoid downsampling aliases.
    fn prepare_logo(&mut self, window: &Window, cx: &App) {
        let edge = (LOGO_SIZE * window.scale_factor()).round().max(1.0) as i32;
        if self.logo_pixels == edge {
            return;
        }
        let renderer = cx.svg_renderer();
        let edge = DevicePixels(edge);
        match renderer
            .parse_svg(LOGO_SVG)
            .and_then(|svg| renderer.render_parsed(&svg, SvgSize::ExactSize(size(edge, edge))))
        {
            Ok(logo) => {
                self.logo = Some(logo);
                self.logo_pixels = edge.0;
            }
            Err(error) => eprintln!("could not rasterize the logo: {error}"),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.working(cx) || self.selected_dirty(cx) > 0 {
            return;
        }

        self.loading = true;
        self.refreshing = true;
        self.error = None;
        cx.notify();

        let load = cx
            .background_executor()
            .spawn(async { agent::load_snapshot() });
        cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            let result = load.await;
            view.update(cx, |view, cx| {
                view.loading = false;
                view.refreshing = false;
                match result {
                    Ok(snapshot) => view.adopt(snapshot),
                    Err(error) => view.error = Some(format!("{error:#}")),
                }
                for update in std::mem::take(&mut view.pending_live) {
                    view.handle_live(update, cx);
                }
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach();
    }

    fn adopt(&mut self, snapshot: Snapshot) {
        let selected_hardware = self.selected_summary().and_then(|d| d.hardware_id.clone());
        (self.devices, self.disconnected) = history::merge(&snapshot.saved, snapshot.devices);
        if let Some(hardware) = selected_hardware
            && let Some(device) = self
                .devices
                .iter()
                .find(|d| d.hardware_id.as_ref() == Some(&hardware))
        {
            self.selected_id = Some(device.id.clone());
        }
        self.states = snapshot.states;

        let selection_is_valid = self
            .selected_id
            .as_ref()
            .is_some_and(|id| self.devices.iter().any(|device| &device.id == id));
        if !selection_is_valid {
            self.selected_id = self.devices.first().map(|device| device.id.clone());
        }
    }

    /// Never present an old snapshot as the result of a pending or failed read.
    fn snapshot_visible(&self) -> bool {
        !self.loading && self.error.is_none()
    }

    /// Tracks the row under a sidebar drag; horizontal movement does not affect it.
    fn hover_drop(&mut self, device_id: &str, over: usize, inside: bool, cx: &mut Context<Self>) {
        let hint = DropHint {
            device_id: device_id.to_owned(),
            over,
        };
        let next = if inside {
            Some(hint)
        } else if self.drop_hint.as_ref() == Some(&hint) {
            // The drag has left the row that last claimed it. Rows report in
            // list order, so only the row holding the hint clears it.
            None
        } else {
            self.drop_hint.clone()
        };
        if next != self.drop_hint {
            self.drop_hint = next;
            cx.notify();
        }
    }

    /// Commits the active drop at the row currently claiming it.
    fn commit_drop(&mut self, cx: &mut Context<Self>) {
        let Some(hint) = self.drop_hint.take() else {
            return;
        };
        cx.notify();
        self.drop_device(&hint.device_id, hint.over, cx);
    }

    /// Puts the dragged device where it was dropped and saves the new order.
    fn drop_device(&mut self, device_id: &str, target: usize, cx: &mut Context<Self>) {
        let Some(from) = self
            .devices
            .iter()
            .position(|device| device.id == device_id)
        else {
            return;
        };
        if from == target || target >= self.devices.len() {
            return;
        }
        let device = self.devices.remove(from);
        self.devices.insert(target, device);
        self.save_order(cx);
    }

    /// Saves the optimistically updated sidebar order and surfaces only failures.
    fn save_order(&mut self, cx: &mut Context<Self>) {
        // Only a device the agent has identified can hold a saved position; the
        // rest keep sorting behind the ordered ones.
        let hardware_ids: Vec<String> = self
            .devices
            .iter()
            .filter_map(|device| device.hardware_id.clone())
            .collect();
        self.error = None;
        cx.notify();

        let save = cx
            .background_executor()
            .spawn(async move { agent::reorder(hardware_ids) });
        cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            if let Err(error) = save.await {
                view.update(cx, |view, cx| {
                    view.error = Some(format!("The new device order was not saved. {error:#}"));
                    cx.notify();
                })?;
            }
            anyhow::Ok(())
        })
        .detach();
    }

    fn select(&mut self, device_id: String, cx: &mut Context<Self>) {
        if self.working(cx) || !self.snapshot_visible() {
            return;
        }
        self.selected_id = Some(device_id);
        if self.page == Page::Preferences {
            self.page = Page::Performance;
        }
        cx.notify();
    }

    fn active_editor(&self) -> Option<Entity<editor::Editor>> {
        self.selected_id
            .as_ref()
            .and_then(|id| self.editors.get(id))
            .cloned()
    }
    fn working(&self, cx: &App) -> bool {
        self.editors.values().any(|editor| editor.read(cx).busy)
    }
    fn selected_dirty(&self, cx: &App) -> usize {
        self.active_editor()
            .map(|editor| editor.read(cx).dirty(cx).len())
            .unwrap_or(0)
    }
    fn ensure_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.selected_state().cloned() else {
            return;
        };
        let id = state.device.id.clone();
        if let Some(editor) = self.editors.get(&id) {
            editor.update(cx, |editor, cx| editor.sync(state, window, cx));
        } else {
            let editor = cx.new(|cx| editor::Editor::new(state, window, cx));
            self.subscriptions.push(cx.subscribe(
                &editor,
                |view, _, event: &editor::EditorEvent, cx| {
                    if let editor::EditorEvent::Updated(state) = event {
                        if let Some(existing) = view
                            .states
                            .iter_mut()
                            .find(|s| s.device.id == state.device.id)
                        {
                            *existing = *state.clone();
                        }
                        if let Some(existing) =
                            view.devices.iter_mut().find(|s| s.id == state.device.id)
                        {
                            *existing = state.device.clone();
                        }
                    }
                    cx.notify();
                },
            ));
            self.editors.insert(id, editor);
        }
    }

    fn selected_summary(&self) -> Option<&DeviceSummary> {
        let selected_id = self.selected_id.as_deref()?;
        self.devices.iter().find(|device| device.id == selected_id)
    }

    fn selected_state(&self) -> Option<&DeviceState> {
        let selected_id = self.selected_id.as_deref()?;
        self.states
            .iter()
            .find(|state| state.device.id == selected_id)
    }
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.devices.iter().enumerate().map(|(index, device)| {
            let status = if self.loading {
                LinkState::Checking
            } else if self.error.is_some() {
                LinkState::Unknown
            } else {
                LinkState::of(device, self.disconnected.contains(&device.id))
            };
            let battery = self
                .states
                .iter()
                .find(|state| state.device.id == device.id)
                .and_then(|state| state.settings.battery.as_ref());
            let id = device.id.clone();
            let selected = self.selected_id.as_deref() == Some(device.id.as_str());
            // A device the agent has not identified has no position to save, so
            // it cannot be dragged — but it can still be dragged past.
            let drag =
                (device.hardware_id.is_some() && self.snapshot_visible()).then(|| DeviceDrag {
                    status,
                    device: device.clone(),
                    battery: battery.cloned(),
                });
            // Where this row would take the drop, if anywhere: above it for a
            // device coming up from below, below it for one coming down.
            let landing = self
                .drop_hint
                .as_ref()
                .filter(|hint| hint.over == index)
                .and_then(|hint| self.devices.iter().position(|d| d.id == hint.device_id))
                .filter(|from| *from != index)
                .map(|from| from > index);
            div()
                .id(SharedString::from(format!("device-{}", device.id)))
                .relative()
                .rounded_lg()
                .border_1()
                .border_color(if selected { rgb(0x484d5a) } else { rgba(0) })
                .bg(if selected { rgb(SELECTED_ROW) } else { rgba(0) })
                .cursor_pointer()
                .when(!selected, |el| el.hover(|style| style.bg(rgb(0x292d36))))
                .on_click(cx.listener(move |view, _, _, cx| view.select(id.clone(), cx)))
                .when_some(drag, |el, drag| {
                    el.on_drag(drag, |drag, grab, _, cx| {
                        let drag = drag.clone();
                        cx.new(move |_| DeviceDragCard {
                            drag,
                            grab_x: grab.x,
                        })
                    })
                })
                .on_drag_move(
                    cx.listener(move |view, event: &DragMoveEvent<DeviceDrag>, _, cx| {
                        let id = event.drag(cx).device.id.clone();
                        let y = event.event.position.y;
                        let inside = y >= event.bounds.top() && y < event.bounds.bottom();
                        view.hover_drop(&id, index, inside, cx);
                    }),
                )
                // Draw the drop line in the gap so the dragged card cannot cover it.
                .when_some(landing, |el, above| {
                    el.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .h(px(2.0))
                            .rounded_full()
                            .bg(rgb(ACCENT))
                            .map(|line| {
                                if above {
                                    line.top(px(-4.0))
                                } else {
                                    line.bottom(px(-4.0))
                                }
                            }),
                    )
                })
                .child(device_card(
                    device,
                    battery,
                    if selected { SELECTED_ROW } else { DEEP },
                    status,
                ))
        });
        let release =
            |view: &mut Self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>| {
                view.commit_drop(cx);
            };
        div()
            .w(SIDEBAR_WIDTH)
            .flex_shrink_0()
            .h_full()
            // A release ends the drag wherever it lands, so both the sidebar and
            // everywhere else have to answer for it.
            .on_mouse_up(MouseButton::Left, cx.listener(release))
            .on_mouse_up_out(MouseButton::Left, cx.listener(release))
            .flex()
            .flex_col()
            .pt(px(22.0))
            .px(SIDEBAR_PAD)
            .pb_4()
            .bg(rgb(DEEP))
            .border_r_1()
            .border_color(rgb(0x353944))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px_2()
                    .pb(px(26.0))
                    .when_some(self.logo.clone(), |el, logo| {
                        el.child(gpui_kit::img(logo).size_8())
                    })
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(18.0))
                            .font_semibold()
                            .text_color(rgb(TEXT))
                            .child("GFlick"),
                    )
                    .child(
                        div()
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x3b3f49))
                            .text_size(theme::text::MICRO)
                            .text_color(rgb(MUTED_2))
                            .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .pb(px(4.0))
                    .child(
                        div()
                            .text_size(theme::text::MICRO)
                            .font_semibold()
                            .text_color(rgb(MUTED_2))
                            .child("CONNECTED DEVICES"),
                    )
                    .child(
                        div()
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(rgb(0x30343e))
                            .text_size(theme::text::MICRO)
                            .text_color(rgb(MUTED))
                            .child(if self.snapshot_visible() {
                                self.devices.len().to_string()
                            } else {
                                "—".to_owned()
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    // The list scrolls, and a scroll box clips what leaves it.
                    // This is the room a drop line above the first row needs.
                    .pt(px(6.0))
                    .children(rows)
                    .when(self.loading, |el| {
                        el.child(
                            div()
                                .p_3()
                                .text_size(theme::text::SMALL)
                                .text_color(rgb(MUTED_2))
                                .child("Reading connected devices…"),
                        )
                    })
                    .when(self.snapshot_visible() && self.devices.is_empty(), |el| {
                        el.child(
                            div()
                                .p_3()
                                .text_size(theme::text::SMALL)
                                .text_color(rgb(MUTED_2))
                                .child("Your mouse will appear here."),
                        )
                    })
                    .overflow_y_scrollbar(),
            )
            .child(
                Button::new("app-preferences")
                    .icon(IconName::Settings)
                    .ghost()
                    .compact()
                    .accessibility_label("App preferences")
                    // Kit centres a button's content in a box that fills it, so
                    // the label takes the leftover width to sit against the icon
                    // rather than being centred with it.
                    .child(ui::button_label("App preferences", theme::text::BODY).flex_1())
                    .selected(self.page == Page::Preferences)
                    .on_click(cx.listener(|view, _, _, cx| view.open_preferences(cx))),
            )
            .child(
                // Re-reading the mouse belongs to the agent, not to the page,
                // so the control sits with the connection it acts on.
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .mx(px(6.0))
                    .mt_3()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(0x333741))
                    .child(ui::dot(if self.error.is_some() {
                        DANGER
                    } else if self.loading {
                        MUTED_2
                    } else {
                        SUCCESS
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(theme::text::TINY)
                                    .font_semibold()
                                    .text_color(rgb(TEXT))
                                    .child("GFlick Agent"),
                            )
                            .child(
                                div()
                                    .text_size(theme::text::MICRO)
                                    .text_color(rgb(MUTED_2))
                                    .child(if self.error.is_some() {
                                        "Unable to connect"
                                    } else if self.loading {
                                        "Reading devices…"
                                    } else {
                                        "Connected locally"
                                    }),
                            ),
                    )
                    .child(
                        Button::new("refresh")
                            .icon(IconName::RotateCw)
                            .ghost()
                            .small()
                            .compact()
                            .tooltip("Read the mouse again")
                            .accessibility_label("Refresh")
                            .loading(self.loading)
                            .disabled(
                                self.loading || self.working(cx) || self.selected_dirty(cx) > 0,
                            )
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                    ),
            )
    }

    /// Renders content at an explicit width because scroll children are unconstrained.
    fn render_content(&self, width: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = width < px(808.0);
        let status = self
            .active_editor()
            .filter(|_| {
                self.snapshot_visible()
                    && self.page != Page::Preferences
                    && self.selected_state().is_some()
            })
            .and_then(|editor| {
                let editor = editor.read(cx);
                editor
                    .message
                    .clone()
                    .map(|message| (message, editor.failed))
            });
        let body = if self.page == Page::Preferences {
            self.render_preferences(cx).into_any_element()
        } else if self.loading {
            self.render_loading().into_any_element()
        } else if self.error.is_some() {
            self.render_empty().into_any_element()
        } else if let Some(state) = self.selected_state() {
            self.render_device(state, width, compact).into_any_element()
        } else if let Some(device) = self.selected_summary() {
            self.render_unavailable(device).into_any_element()
        } else {
            self.render_empty().into_any_element()
        };
        // The bar is the device's pages and the changes to them, so it stands
        // only where those exist: not over preferences, and not over a device
        // that is loading, missing or offline, which has nothing to switch
        // between and nothing to apply.
        let bar = (self.page != Page::Preferences && self.selected_state().is_some()).then(|| {
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_4()
                .px(px(28.0))
                .py_2()
                // One row, at one height, whatever the window is doing: the
                // pages and the actions belong on the same line. Wrapping is
                // only the last resort, for a window too narrow to hold both.
                .min_h(px(58.0))
                .flex_wrap()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(self.render_tabs(cx))
                .child(div().flex_1())
                .child(self.render_actions(cx))
        });
        div()
            .min_w_0()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .children(bar)
            .when_some(status, |el, (message, failed)| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px(px(24.0))
                        .py(px(10.0))
                        .bg(rgb(if failed { 0x38282d } else { 0x25352e }))
                        .border_b_1()
                        .border_color(rgb(if failed { 0x5a3a41 } else { 0x38584a }))
                        .text_size(theme::text::SMALL)
                        .text_color(rgb(if failed { DANGER } else { SUCCESS }))
                        .child(ui::dot(if failed { DANGER } else { SUCCESS }))
                        .child(message),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
    }

    /// The device's pages, as one segmented group.
    ///
    /// The same control the polling card uses for wired/wireless, because this
    /// is the same kind of choice: two views of one device, not two
    /// destinations. An underline had to be held a bar's height away from its
    /// own label to sit on the rule; a group carries the selection on the word.
    ///
    /// The preferences page has none: the tabs are a device's pages, and it is
    /// not one. Picking a device in the sidebar is the way back to them.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.page == Page::Preferences {
            return div().into_any_element();
        }
        let tabs = [
            (Page::Performance, "Performance", IconName::Gauge),
            (Page::Details, "Device details", IconName::IdCard),
        ]
        .into_iter()
        .map(|(page, label, icon)| {
            let active = self.page == page;
            div()
                .id(label)
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(11.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .text_size(theme::text::SMALL)
                .font_semibold()
                .cursor_pointer()
                .when(active, |el| el.bg(rgb(SURFACE_3)).text_color(rgb(TEXT)))
                .when(!active, |el| {
                    el.text_color(rgb(MUTED_2))
                        .hover(|style| style.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
                })
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.page = page;
                    cx.notify();
                }))
                .child(Icon::new(icon).with_size(px(14.0)))
                .child(label)
        });
        div()
            .flex()
            .flex_shrink_0()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded_lg()
            .bg(rgb(DEEP))
            .children(tabs)
            .into_any_element()
    }

    /// The right of the top bar: what to do about what is pending.
    ///
    /// There is no readout beside the buttons, because the buttons are the
    /// readout: the count rides on Apply, both go quiet when the mouse already
    /// matches, and Apply says so itself while it is writing.
    fn render_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.working(cx);
        let dirty = self.selected_dirty(cx);
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .when_some(
                self.active_editor().filter(|_| {
                    self.snapshot_visible()
                        && self.page != Page::Preferences
                        && self.selected_state().is_some()
                }),
                |bar, editor| {
                    let discard = editor.clone();
                    bar.child(
                        Button::new("discard")
                            .outline()
                            .compact()
                            // An undo arrow rather than a cross: the edits are
                            // rolled back to what the mouse already holds, not
                            // dismissed.
                            .icon(IconName::Undo2)
                            .accessibility_label("Discard")
                            .child(ui::button_label("Discard", theme::text::BODY))
                            .disabled(busy || dirty == 0)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.confirm_device_action(discard.clone(), true, window, cx)
                            })),
                    )
                    .child(
                        Button::new("apply")
                            .primary()
                            .compact()
                            .icon(IconName::Check)
                            .accessibility_label("Apply")
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    // The size goes on the label itself: Kit sets
                                    // its own on the element wrapping this one.
                                    .child(ui::button_label(
                                        if busy { "Applying…" } else { "Apply" },
                                        theme::text::BODY,
                                    ))
                                    .when(!busy && dirty > 0, |el| {
                                        el.child(ui::count_badge(dirty))
                                    }),
                            )
                            .loading(busy)
                            .disabled(
                                busy || dirty == 0
                                    || self.loading
                                    || self.selected_state().is_none(),
                            )
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.confirm_device_action(editor.clone(), false, window, cx)
                            })),
                    )
                },
            )
    }

    fn render_device(&self, state: &DeviceState, width: Pixels, compact: bool) -> impl IntoElement {
        let details = self.page == Page::Details;
        // Use explicit widths because the compact column's alignment disables stretching.
        let inner = width.min(px(1120.0)) - px(56.0);
        let rail = px(300.0);
        let column = if details && !compact {
            (inner - rail - px(16.0)).max(px(280.0))
        } else {
            inner
        };
        let settings = div()
            .w(column)
            .when_some(self.active_editor(), |el, editor| el.child(editor))
            .when(details, |el| {
                el.child(
                    ui::card()
                        .mt_4()
                        .child(ui::card_header(
                            IconName::IdCard,
                            "Identity",
                            "What the mouse reports about itself.",
                        ))
                        .child(ui::card_body().child(ui::info_table(vec![
                                ("Model", reported_label(&state.device).into()),
                                (
                                    "USB identity",
                                    format!(
                                        "{:04x}:{:04x}",
                                        state.device.vendor_id, state.device.product_id
                                    )
                                    .into(),
                                ),
                                (
                                    "Serial",
                                    state
                                        .device
                                        .serial_number
                                        .clone()
                                        .map(Into::into)
                                        .unwrap_or(SharedString::new_static("Not reported")),
                                ),
                                (
                                    "Hardware ID",
                                    state
                                        .device
                                        .hardware_id
                                        .clone()
                                        .map(Into::into)
                                        .unwrap_or(SharedString::new_static("Pending")),
                                ),
                            ]))),
                )
                .child(
                    ui::card()
                        .mt_4()
                        .child(ui::card_header(
                            IconName::ListChecks,
                            "Capabilities",
                            "What this model lets the agent change.",
                        ))
                        .child(ui::card_body().child(ui::info_table(vec![
                            (
                                "Onboard profiles",
                                yes_no(state.capabilities.onboard_profiles).into(),
                            ),
                            (
                                "Lighting",
                                yes_no(state.capabilities.color_led_effects).into(),
                            ),
                            (
                                "Lift-off distance",
                                yes_no(state.capabilities.lift_off_distance).into(),
                            ),
                            (
                                "Surface mode",
                                yes_no(state.capabilities.surface_mode).into(),
                            ),
                        ]))),
                )
            });

        let page = div()
            .w(width)
            .max_w(px(1120.0))
            .px(px(28.0))
            .pt(px(32.0))
            .pb(px(56.0))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_6()
                    .mb(px(26.0))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(7.0))
                            .child(ui::eyebrow("YOUR DEVICE"))
                            .child(
                                div()
                                    .text_size(theme::text::DISPLAY)
                                    .font_semibold()
                                    .text_color(rgb(TEXT))
                                    .child(device_label(&state.device)),
                            )
                            .child(
                                div()
                                    .max_w(px(560.0))
                                    .text_size(theme::text::BODY)
                                    .text_color(rgb(MUTED))
                                    .child(
                                        "Make it yours. Changes reach the mouse when you apply.",
                                    ),
                            ),
                    ),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .mb_5()
                        .flex()
                        .items_start()
                        .gap_2()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0x5a3a41))
                        .bg(rgb(0x38282d))
                        .text_size(theme::text::SMALL)
                        .text_color(rgb(DANGER))
                        .child(ui::dot(DANGER).mt_1())
                        .child(format!("Showing the last successful read. {error}")),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_4()
                    .when(compact, |el| el.flex_col_reverse())
                    .when(!compact, |el| el.items_start())
                    .child(settings)
                    .when(details, |el| {
                        el.child(self.render_rail(state, if compact { inner } else { rail }))
                    }),
            );

        // A column that centres its child lets the page keep a comfortable
        // maximum width on a wide window and still shrink on a narrow one.
        div()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .child(page)
            .overflow_y_scrollbar()
    }

    /// The details page's right column: what the mouse is, not what it does.
    fn render_rail(&self, state: &DeviceState, width: Pixels) -> impl IntoElement {
        let model = preview::MouseModel::for_device(&state.device);
        let battery = state.settings.battery.as_ref();
        div()
            .w(width)
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(SURFACE))
                    .overflow_hidden()
                    .pb_4()
                    .when_some(model, |el, model| {
                        el.child(self.preview.clone()).child(
                            div()
                                .mt_2()
                                .text_center()
                                .text_size(theme::text::SMALL)
                                .text_color(rgb(MUTED))
                                .child(model.label()),
                        )
                    })
                    .when(model.is_none(), |el| {
                        el.child(
                            div()
                                .h(px(280.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(theme::text::SMALL)
                                .text_color(rgb(MUTED_2))
                                .child("No product artwork yet"),
                        )
                    }),
            )
            .child(
                ui::card()
                    .child(ui::card_header(
                        IconName::BatteryFull,
                        "Battery",
                        "As last reported by the mouse.",
                    ))
                    .child(
                        ui::card_body()
                            .child(ui::info_table(vec![
                                (
                                    "Charge",
                                    battery
                                        .map(|b| SharedString::from(format!("{}%", b.percentage)))
                                        .unwrap_or(SharedString::new_static("—")),
                                ),
                                (
                                    "Status",
                                    battery
                                        .map(|b| SharedString::from(sentence_case(&b.status)))
                                        .unwrap_or(SharedString::new_static("Not reported")),
                                ),
                            ]))
                            .when(charge(battery) == Charge::Low, |el| {
                                el.pb_4().child(ui::notice(
                                    IconName::BatteryLow,
                                    DANGER,
                                    format!(
                                        "Under {LOW_BATTERY}%. Worth putting the mouse on the \
                                         cable before your next session."
                                    ),
                                ))
                            })
                            .when(charge(battery) == Charge::Charging, |el| {
                                el.pb_4().child(ui::notice(
                                    IconName::Zap,
                                    SUCCESS,
                                    "On the cable and filling up.",
                                ))
                            }),
                    ),
            )
            .child(
                ui::well()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .p_4()
                    .child(ui::caption("SETTINGS SOURCE"))
                    .child(ui::chip_accent(source_label(
                        state.settings.configuration_source,
                    ))),
            )
    }

    fn render_unavailable(&self, device: &DeviceSummary) -> impl IntoElement {
        let disconnected = self.disconnected.contains(&device.id);
        let detail = if disconnected {
            "USB disconnected. Reconnect the mouse or its receiver; it will appear online automatically."
        } else {
            match device.availability.as_ref() {
                Some(DeviceAvailability::Initializing) => "The agent is reading this device.",
                Some(DeviceAvailability::Unavailable {
                    reason: gflick_protocol::DeviceUnavailableReason::NotResponding,
                    ..
                }) if device.connection == DeviceConnection::Receiver => {
                    "Receiver connected, mouse offline. The mouse may be switched off, asleep, or out of range."
                }
                Some(DeviceAvailability::Unavailable { detail, .. }) => detail.as_str(),
                _ => "The device is not ready yet.",
            }
        };
        // A device still being read is not a device that has gone quiet, so
        // only the second of those two earns the glyph.
        let silent = disconnected
            || matches!(
                device.availability.as_ref(),
                Some(DeviceAvailability::Unavailable { .. })
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            // The photo stands whichever page was open when the mouse went
            // quiet: it is what the page is about, and there is nothing else
            // here to look at.
            .when(preview::MouseModel::for_device(device).is_some(), |el| {
                el.child(div().w(px(320.0)).child(self.preview.clone()))
            })
            .when(silent, |el| {
                el.child(
                    Icon::new(if disconnected {
                        IconName::Unplug
                    } else {
                        offline_icon(device)
                    })
                    .with_size(px(30.0))
                    .text_color(rgb(MUTED_2)),
                )
            })
            .child(centered_message(device_label(device), detail))
    }

    fn render_loading(&self) -> impl IntoElement {
        // Neutral shapes preserve the page structure without inventing settings,
        // battery levels, artwork, or a device count while HID++ reads finish.
        div()
            .size_full()
            .p_8()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .text_size(theme::text::TITLE)
                    .text_color(rgb(TEXT))
                    .child("Reading device settings…"),
            )
            .child(
                div()
                    .text_size(theme::text::SMALL)
                    .text_color(rgb(MUTED))
                    .child("Waiting for current values from your mouse."),
            )
            .children((0..2).map(|_| {
                ui::card()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(div().w(px(144.0)).h(px(14.0)).rounded_md().bg(rgb(LINE)))
                    .child(div().w_full().h(px(36.0)).rounded_lg().bg(rgb(DEEP)))
                    .child(div().w_full().h(px(36.0)).rounded_lg().bg(rgb(DEEP)))
            }))
    }

    fn render_empty(&self) -> impl IntoElement {
        let (title, detail) = if let Some(error) = self.error.as_deref() {
            ("Could not read device settings", error)
        } else if self.loading {
            ("Reading devices", "Connecting to the local GFlick agent…")
        } else {
            ("No devices", "Connect a supported Logitech mouse.")
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                div()
                    .w_full()
                    .max_w(px(460.0))
                    .py(px(56.0))
                    .px_6()
                    .rounded_xl()
                    .border_1()
                    .border_dashed()
                    .border_color(rgb(LINE_STRONG))
                    .child(centered_message(title, detail)),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_editor(window, cx);
        self.prepare_logo(window, cx);
        let model = self
            .selected_summary()
            .and_then(preview::MouseModel::for_device)
            .unwrap_or(preview::MouseModel::Superlight2);
        let mut color = self.selected_summary().map(|d| d.color).unwrap_or_default();
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| {
                let details = self.page == Page::Details;
                if editor.details != details {
                    editor.details = details;
                    cx.notify();
                }
            });
            if let Some(value) = editor.read(cx).values(cx).get(&settings::Key::Appearance) {
                color = editor
                    .read(cx)
                    .baseline
                    .device
                    .available_colors()
                    .iter()
                    .copied()
                    .find(|color| color.as_str() == value)
                    .unwrap_or_default();
            }
        }
        self.preview
            .update(cx, |preview, cx| preview.set_model(model, color, cx));
        div()
            // `fps_monitor` pins itself absolutely, so its parent must be relative.
            .relative()
            .size_full()
            .flex()
            .font_family(".SystemUIFont")
            .bg(rgb(BG))
            .on_action(cx.listener(|view: &mut Self, _: &ToggleFps, _, cx| {
                view.show_fps = !view.show_fps;
                cx.notify();
            }))
            .child(self.render_sidebar(cx))
            .child(self.render_content(
                (window.viewport_size().width - SIDEBAR_WIDTH).max(px(0.0)),
                cx,
            ))
            .children(Root::render_dialog_layer(window, cx))
            .when(self.show_fps, |el| {
                // Bottom right: the HUD's own top-right default sits over the
                // header's Discard and Apply buttons.
                el.child(fps_monitor(window, cx).anchor(Anchor::BottomRight))
            })
    }
}

/// Charge below this is worth pointing at rather than just reporting.
const LOW_BATTERY: u8 = 30;

/// The one thing about the charge worth a glyph of its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Charge {
    /// On the cable and filling up.
    Charging,
    /// Running down, and close enough to empty to say so.
    Low,
    /// Running down with room to spare: the level itself is the whole story.
    Steady,
}

fn charge(battery: Option<&gflick_protocol::BatteryState>) -> Charge {
    let Some(battery) = battery else {
        return Charge::Steady;
    };
    // Every status the mouse reports other than "discharging" is one it can
    // only be in on the cable, including having finished.
    match battery.status_code {
        0x01..=0x04 => Charge::Charging,
        _ if battery.percentage < LOW_BATTERY => Charge::Low,
        _ => Charge::Steady,
    }
}

/// Sidebar charge level, with a bolt only for the otherwise invisible charging state.
fn sidebar_battery(
    battery: Option<&gflick_protocol::BatteryState>,
    device: &DeviceSummary,
    backdrop: u32,
) -> impl IntoElement {
    let percentage = battery.map(|battery| battery.percentage.min(100));
    let color = match (charge(battery), percentage) {
        _ if !device.ready => MUTED_2,
        (Charge::Charging, _) => SUCCESS,
        (Charge::Low, _) => DANGER,
        (_, Some(0..=20)) => DANGER,
        (_, Some(_)) => SUCCESS,
        (_, None) => MUTED_2,
    };
    // A mouse that is not answering has no charge to report, so the readout
    // says why it is silent instead of drawing a cell it cannot fill.
    let mark = if device.ready {
        battery_cell(
            percentage,
            color,
            backdrop,
            charge(battery) == Charge::Charging,
        )
        .into_any_element()
    } else {
        Icon::new(offline_icon(device))
            .with_size(px(14.0))
            .text_color(rgb(MUTED_2))
            .into_any_element()
    };
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .flex_shrink_0()
        .child(
            div()
                .text_size(theme::text::TINY)
                .font_semibold()
                .text_color(rgb(color))
                .child(percentage.map(|p| format!("{p}%")).unwrap_or_else(|| {
                    if device.ready { "\u{2014}" } else { "Offline" }.to_owned()
                })),
        )
        .child(div().flex().items_center().h(px(14.0)).child(mark))
}

/// The colour of a battery cell's shell: its border and its terminal.
const CELL_SHELL: u32 = 0x777e8d;
/// The cell's outer size, and the inner width left for the fill once its border
/// and padding are taken out.
const CELL_WIDTH: f32 = 22.0;
const CELL_HEIGHT: f32 = 10.0;
const CELL_FILL_WIDTH: f32 = 18.0;
/// Bolt and backdrop sizes, slightly taller than the battery cell.
const BOLT_SIZE: f32 = 14.0;
const BOLT_EDGE: f32 = 16.5;

/// A charge cell with an overlaid bolt rendered as a sibling above GPUI's border.
/// A larger backdrop-colored bolt separates it from both fill and shell.
fn battery_cell(
    percentage: Option<u8>,
    color: u32,
    backdrop: u32,
    charging: bool,
) -> impl IntoElement {
    let bolt = |size: f32, color: u32| {
        icons::bolt()
            .with_size(px(size))
            .text_color(rgb(color))
            .absolute()
    };
    div()
        .relative()
        .flex()
        .items_center()
        .gap(px(1.0))
        .child(
            div()
                .w(px(CELL_WIDTH))
                .h(px(CELL_HEIGHT))
                .p(px(1.0))
                .rounded_sm()
                .border_1()
                .border_color(rgb(CELL_SHELL))
                .child(
                    div()
                        .h_full()
                        .w(px(
                            CELL_FILL_WIDTH * f32::from(percentage.unwrap_or(0)) / 100.0
                        ))
                        .rounded(px(1.0))
                        .bg(rgb(color)),
                ),
        )
        .child(
            div()
                .w(px(2.0))
                .h(px(4.0))
                .rounded_r(px(1.0))
                .bg(rgb(CELL_SHELL)),
        )
        .when(charging, |el| {
            // Centred on the cell, not on the readout: the terminal beside it
            // would pull the bolt off centre.
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(CELL_WIDTH))
                    .h(px(CELL_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(bolt(BOLT_EDGE, backdrop))
                    .child(bolt(BOLT_SIZE, color)),
            )
        })
}

fn centered_message(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(
            div()
                .text_size(theme::text::TITLE)
                .font_semibold()
                .text_color(rgb(TEXT))
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(400.0))
                .text_center()
                .text_size(theme::text::BODY)
                .text_color(rgb(MUTED))
                .child(detail.into()),
        )
}

fn device_label(device: &DeviceSummary) -> String {
    device
        .nickname
        .as_ref()
        .or(device.display_name.as_ref())
        .or(device.product_name.as_ref())
        .cloned()
        .unwrap_or_else(|| "Logitech mouse".to_owned())
}

fn reported_label(device: &DeviceSummary) -> String {
    device
        .display_name
        .as_ref()
        .or(device.product_name.as_ref())
        .cloned()
        .unwrap_or_else(|| "Not reported".to_owned())
}

/// How a connection is drawn wherever it is named. Kept in step with the
/// polling switch, which asks the same question of the same two words.
fn connection_icon(device: &DeviceSummary) -> IconName {
    match device.connection {
        DeviceConnection::DirectUsb => IconName::Plug,
        DeviceConnection::Receiver => IconName::Wifi,
    }
}

/// The crossed-out counterpart to [`connection_icon`] for an offline link.
fn offline_icon(device: &DeviceSummary) -> IconName {
    match device.connection {
        DeviceConnection::DirectUsb => IconName::Unplug,
        DeviceConnection::Receiver => IconName::WifiOff,
    }
}

fn connection_label(device: &DeviceSummary) -> &'static str {
    match device.connection {
        DeviceConnection::DirectUsb => "DIRECT USB",
        DeviceConnection::Receiver => "WIRELESS RECEIVER",
    }
}

fn source_label(source: Option<ConfigurationSource>) -> &'static str {
    match source {
        Some(ConfigurationSource::Host) => "HOST CONTROL",
        Some(ConfigurationSource::Onboard { .. }) => "ONBOARD PROFILE",
        Some(ConfigurationSource::Unknown { .. }) => "UNKNOWN MODE",
        None => "SOURCE UNAVAILABLE",
    }
}

/// The agent reports battery status in lower case; here it is a value on a
/// label, so it starts like one.
fn sentence_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "Supported" } else { "Unavailable" }
}

fn main() {
    gpui_kit::application()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            theme::apply(cx);
            let toggle_fps = if cfg!(target_os = "macos") {
                "cmd-alt-f"
            } else {
                "ctrl-alt-f"
            };
            cx.bind_keys([
                KeyBinding::new(toggle_fps, ToggleFps, None),
                // Dialogs default to Cancel: Enter must never accidentally send
                // hardware writes or throw away a draft. Explicit buttons act.
                KeyBinding::new("enter", gpui_kit::component::dialog::Cancel, Some("Dialog")),
            ]);

            let bounds = Bounds::centered(None, size(px(1220.0), px(820.0)), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(840.0), px(640.0))),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("GFlick Settings");
                    let view: Entity<SettingsView> = cx.new(SettingsView::new);
                    cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
                },
            );

            if let Err(error) = window {
                eprintln!("failed to open the GFlick settings window: {error:#}");
                cx.quit();
                return;
            }

            cx.activate(true);
        });
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    #[test]
    fn distinguishes_missing_usb_from_a_silent_receiver_and_read_errors() {
        let mut device = test_support::device().device;
        device.ready = false;
        device.connection = DeviceConnection::Receiver;
        device.availability = Some(DeviceAvailability::Unavailable {
            reason: gflick_protocol::DeviceUnavailableReason::NotResponding,
            detail: "Timed out".into(),
        });
        assert!(LinkState::of(&device, false) == LinkState::Offline);
        assert!(LinkState::of(&device, true) == LinkState::Disconnected);
        device.availability = Some(DeviceAvailability::Initializing);
        assert!(LinkState::of(&device, false) == LinkState::Checking);
        device.availability = Some(DeviceAvailability::Unavailable {
            reason: gflick_protocol::DeviceUnavailableReason::CommunicationError,
            detail: "Read failed".into(),
        });
        assert!(LinkState::of(&device, false) == LinkState::Error);
    }
}
