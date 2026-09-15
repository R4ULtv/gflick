//! Native settings editor. Drafts are isolated per device and never write on edit.
use crate::icons::IconName;
use crate::{
    settings::{self, Key, Spec, Values},
    theme::{
        ACCENT_TEXT, DEEP, LINE_SOFT, LINE_STRONG, MUTED, MUTED_2, NOTICE_ICON, SUCCESS, SURFACE_2,
        SURFACE_3, TEXT, WARNING, text,
    },
    ui,
};
use gflick_protocol::{DeviceConnection, DeviceState};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Side, Sizable as _,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    slider::{Slider, SliderEvent, SliderScale, SliderState},
    switch::Switch,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, IntoElement, Pixels, Render, SharedString, Subscription,
    Window, div, prelude::*, px, relative, rgb, rgba,
};
use std::cell::Cell;
use std::collections::BTreeMap;

pub enum EditorEvent {
    Changed,
    Updated(Box<DeviceState>),
}

/// Which device page owns the controls currently drawn by the editor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorPage {
    Performance,
    Buttons,
    Details,
}

/// One physical control, drawn by the page that owns the device artwork.
pub struct Callout {
    /// What the control is called on the enclosure, such as "Wheel click".
    pub label: SharedString,
    /// The action still on the device, while an edit is waiting to be applied.
    pub was: Option<SharedString>,
    pub picker: gpui_kit::AnyElement,
}

/// How tall a mapping menu runs before it scrolls, and the air above and below
/// each of its rows. Kit's own rows are tight enough to read as one block.
const MENU_HEIGHT: Pixels = px(322.0);
const MENU_ROW_PAD: Pixels = px(3.0);

/// What a mapping value sends to this computer.
fn action_label(spec: &Spec, value: &str) -> SharedString {
    spec.choices
        .iter()
        .find(|(choice, _)| choice == value)
        .map(|(_, label)| label.clone().into())
        .unwrap_or(SharedString::new_static("Not reported"))
}

/// A glyph for the action, so a callout reads before its label does.
fn action_icon(value: &str) -> IconName {
    match value {
        "0" => IconName::Ban,
        "1" => IconName::MousePointerClick,
        "2" => IconName::MousePointer2,
        "3" => IconName::CircleDot,
        "4" => IconName::ArrowLeft,
        "5" => IconName::ArrowRight,
        _ => IconName::Mouse,
    }
}

struct Field {
    spec: Spec,
    input: Entity<InputState>,
    /// Sensitivity is a point on a range, so its field carries a slider. The
    /// number and the slider are two views of the one draft value.
    slider: Option<Entity<SliderState>>,
    /// Last measured track width, used to refit the scale after layout.
    scale_width: Cell<f32>,
}
pub struct Editor {
    pub baseline: DeviceState,
    fields: Vec<Field>,
    subscriptions: Vec<Subscription>,
    pub busy: bool,
    pub page: EditorPage,
    pub columns: usize,
    /// Whether the buttons page is drawing its own callouts, which leaves the
    /// editor nothing to list for those fields.
    pub callouts_drawn: bool,
    pub message: Option<String>,
    pub failed: bool,
    /// Visible polling row; both connections retain their drafts and apply together.
    connection: Option<Key>,
    /// Whether the two sensitivity axes are edited apart. A mouse that reports
    /// a Y axis still writes both on apply; this only splits the controls.
    separate_axes: bool,
}
impl EventEmitter<EditorEvent> for Editor {}
impl Editor {
    pub fn new(state: DeviceState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut editor = Self {
            baseline: state,
            fields: Vec::new(),
            subscriptions: Vec::new(),
            busy: false,
            page: EditorPage::Performance,
            columns: 1,
            callouts_drawn: false,
            message: None,
            failed: false,
            connection: None,
            separate_axes: false,
        };
        editor.reset(window, cx);
        editor
    }
    pub fn values(&self, cx: &App) -> Values {
        self.fields
            .iter()
            .map(|f| (f.spec.key, f.input.read(cx).value().to_string()))
            .collect()
    }
    pub fn dirty(&self, cx: &App) -> Values {
        self.fields
            .iter()
            .filter_map(|f| {
                let value = f.input.read(cx).value().to_string();
                (value != f.spec.value).then_some((f.spec.key, value))
            })
            .collect()
    }
    /// The edits as a person made them. Linked axes are one control, so the
    /// pair of writes it produces counts and reads as one edit.
    pub fn shown_edits(&self, cx: &App) -> Values {
        let mut edits = self.dirty(cx);
        if self.axes_linked() && edits.contains_key(&Key::Dpi) {
            edits.remove(&Key::DpiY);
        }
        edits
    }

    /// Whether both sensitivity axes are driven by the one control.
    pub fn axes_linked(&self) -> bool {
        !self.separate_axes
    }
    pub fn sync(&mut self, state: DeviceState, window: &mut Window, cx: &mut Context<Self>) {
        // Telemetry must not rebuild inputs or steal focus, even during a draft.
        self.baseline.settings.battery = state.settings.battery.clone();
        if self.busy || !self.dirty(cx).is_empty() || self.baseline == state {
            return;
        }
        self.baseline = state;
        self.reset(window, cx);
    }
    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.message = None;
        self.failed = false;
        self.reset(window, cx);
        cx.emit(EditorEvent::Changed);
    }
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.subscriptions.clear();
        self.fields.clear();
        // Axes open apart only when the mouse is already running them apart.
        self.separate_axes = self
            .baseline
            .settings
            .dpi
            .is_some_and(|dpi| dpi.current_y.is_some_and(|y| y != dpi.current_x));
        let range = dpi_range(&self.baseline);
        for spec in settings::specs(&self.baseline) {
            let key = spec.key;
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(spec.value.clone(), window, cx);
                input
            });
            self.subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.message = None;
                        this.failed = false;
                        this.sync_sliders(window, cx);
                        cx.emit(EditorEvent::Changed);
                        cx.notify();
                    }
                    // A typed sensitivity settles on an increment the sensor
                    // has, rather than failing validation on apply.
                    InputEvent::Blur | InputEvent::PressEnter { .. } if is_dpi(key) => {
                        this.settle_dpi(key, window, cx)
                    }
                    _ => {}
                },
            ));
            let slider = range.filter(|_| is_dpi(key)).map(|(min, max)| {
                let track = Track::new(&spec, min, max);
                let value = spec.value.parse::<f32>().unwrap_or(min);
                // The slider runs the length of the track rather than over the
                // sensor's numbers, since the track is not an even scale.
                let slider = cx.new(|_| {
                    SliderState::new()
                        .min(0.0)
                        .max(1.0)
                        .step(0.001)
                        .scale(SliderScale::Linear)
                        .default_value(track.at(value))
                });
                self.subscriptions.push(cx.subscribe_in(
                    &slider,
                    window,
                    move |this, _, event: &SliderEvent, window, cx| {
                        if let SliderEvent::Change(at) = event {
                            let dpi = this.snap_dpi(round_dpi(track.dpi(at.end())));
                            this.set_dpi(key, dpi, window, cx);
                        }
                    },
                ));
                slider
            });
            self.fields.push(Field {
                spec,
                input,
                slider,
                scale_width: Cell::default(),
            });
        }
        cx.notify();
    }

    /// Moves every slider back onto the value its field now holds.
    fn sync_sliders(&self, window: &mut Window, cx: &mut Context<Self>) {
        for field in &self.fields {
            let Some((slider, track)) = field.slider.clone().zip(self.track(field)) else {
                continue;
            };
            let Ok(value) = field.input.read(cx).value().parse::<f32>() else {
                continue;
            };
            slider.update(cx, |slider, cx| {
                slider.set_value(track.at(value), window, cx)
            });
        }
    }

    /// How this field's track lays the sensor's range out.
    fn track(&self, field: &Field) -> Option<Track> {
        dpi_range(&self.baseline).map(|(min, max)| Track::new(&field.spec, min, max))
    }

    /// The nearest sensitivity this sensor can actually run at.
    fn snap_dpi(&self, value: f32) -> u16 {
        nearest_dpi(self.baseline.capabilities.supported_dpi.as_deref(), value)
    }

    /// Rewrites a typed sensitivity as an increment the sensor reports.
    fn settle_dpi(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.fields.iter().find(|field| field.spec.key == key) else {
            return;
        };
        let Ok(typed) = field.input.read(cx).value().parse::<f32>() else {
            return;
        };
        let settled = self.snap_dpi(typed);
        if f32::from(settled) != typed {
            self.set_dpi(key, settled, window, cx);
        }
    }

    /// Writes one axis, and the other with it while the axes are linked.
    fn set_dpi(&mut self, key: Key, dpi: u16, window: &mut Window, cx: &mut Context<Self>) {
        self.set(key, dpi.to_string(), window, cx);
        if self.separate_axes {
            return;
        }
        let other = if key == Key::Dpi { Key::DpiY } else { Key::Dpi };
        if self.fields.iter().any(|field| field.spec.key == other) {
            self.set(other, dpi.to_string(), window, cx);
        }
    }

    /// The polling row on show: whichever was picked, else the one the mouse is
    /// reporting over now.
    fn shown_connection(&self) -> Key {
        self.connection
            .filter(|key| self.fields.iter().any(|field| field.spec.key == *key))
            .unwrap_or_else(|| self.live_connection())
    }

    /// The polling row to open on: the one the mouse is reporting over now.
    fn live_connection(&self) -> Key {
        let wireless = self.fields.iter().any(|f| f.spec.key == Key::Wireless);
        match self.baseline.device.connection {
            DeviceConnection::Receiver if wireless => Key::Wireless,
            _ => Key::Wired,
        }
    }
    pub fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        // A value typed and applied without leaving the field still goes to the
        // device as an increment it has.
        for key in [Key::Dpi, Key::DpiY] {
            self.settle_dpi(key, window, cx);
        }
        let dirty = self.dirty(cx);
        if dirty.is_empty() {
            return;
        }
        let values = self.values(cx);
        let changes = match settings::plan(&self.baseline, &values, &dirty) {
            Ok(changes) => changes,
            Err(error) => {
                self.message = Some(error.to_string());
                self.failed = true;
                cx.emit(EditorEvent::Changed);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.message = None;
        self.failed = false;
        cx.emit(EditorEvent::Changed);
        cx.notify();
        let baseline = self.baseline.clone();
        let task = cx
            .background_executor()
            .spawn(async move { crate::agent::apply_changes(baseline, changes) });
        cx.spawn_in(
            window,
            async move |editor: gpui_kit::WeakEntity<Self>, cx| {
                let outcome = task.await;
                editor.update_in(cx, |editor, window, cx| {
                    editor.busy = false;
                    if let Some(state) = outcome.state {
                        editor.baseline = state.clone();
                        editor.reset(window, cx);
                        let (initial, draft) =
                            settings::reconcile(&state, &values, &dirty, &outcome.applied);
                        for field in &mut editor.fields {
                            let key = field.spec.key;
                            if let Some(value) = initial.get(&key) {
                                field.spec.value = value.clone();
                            }
                            if let Some(value) = draft.get(&key) {
                                field.input.update(cx, |input, cx| {
                                    input.set_value(value.clone(), window, cx)
                                });
                            }
                        }
                        cx.emit(EditorEvent::Updated(Box::new(state)));
                    }
                    editor.failed = outcome.error.is_some();
                    editor.message = Some(
                        outcome
                            .error
                            .unwrap_or_else(|| "Changes applied. Device values refreshed.".into()),
                    );
                    cx.emit(EditorEvent::Changed);
                    cx.notify();
                })?;
                anyhow::Ok(())
            },
        )
        .detach();
    }
    fn set(&mut self, key: Key, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(field) = self.fields.iter().find(|f| f.spec.key == key) {
            field
                .input
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        // A programmatic write emits no input event, so the sliders are moved here.
        self.sync_sliders(window, cx);
        self.message = None;
        self.failed = false;
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }
}

/// Presentation chosen from each setting's shape and number of options.
enum Control {
    /// A single flag, which reads as a switch rather than as two words to
    /// choose between.
    Toggle,
    Segmented,
    Chips,
    /// The mouse's finish, which is a colour and so shows as one.
    Swatches,
}

impl Control {
    fn for_spec(spec: &Spec) -> Self {
        match spec.key {
            Key::Appearance => Self::Swatches,
            _ if spec.choices.len() == 2
                && spec.choices[0].0 == "off"
                && spec.choices[1].0 == "on" =>
            {
                Self::Toggle
            }
            _ if spec.choices.len() <= 4
                && spec.choices.iter().all(|(_, label)| label.len() <= 14) =>
            {
                Self::Segmented
            }
            _ => Self::Chips,
        }
    }
}

/// Half of a note as it is drawn, in pixels. The scale is set in one size, so
/// its labels are measured from the digits they carry.
fn note_half_width(dpi: u16) -> f32 {
    dpi_text(dpi).chars().count() as f32 * 3.0
}

/// The air left between two notes, so the scale reads as separate numbers.
const NOTE_GAP: f32 = 8.0;

/// A note's track position, with endpoint labels kept inside the bounds.
fn note_centre(dpi: u16, track: Track, width: f32) -> f32 {
    match f32::from(dpi) {
        dpi if dpi <= track.min => note_half_width(track.min as u16),
        dpi if dpi >= track.max => width - note_half_width(track.max as u16),
        dpi => track.at(dpi) * width,
    }
}

/// Fits labels without overlap, prioritizing the current value, endpoints, and presets.
fn fitted_notes(spec: &Spec, value: &str, track: Track, width: f32) -> Vec<u16> {
    let centre = |dpi: u16| note_centre(dpi, track, width);
    // What is read first is kept first: the value in force, then the two ends,
    // then the rest from the top down, so the roundest settings survive.
    let mut order = scale_notes(spec, track.min, track.max);
    order.sort_by_key(|dpi| {
        (
            dpi.to_string() != value,
            !(f32::from(*dpi) <= track.min || f32::from(*dpi) >= track.max),
            std::cmp::Reverse(*dpi),
        )
    });
    let mut notes: Vec<u16> = Vec::new();
    for dpi in order {
        // Two notes clear one another when their labels do, with a gap between
        // so the scale still reads as separate numbers.
        let clear = notes.iter().all(|taken| {
            (centre(dpi) - centre(*taken)).abs()
                >= note_half_width(dpi) + note_half_width(*taken) + NOTE_GAP
        });
        if clear {
            notes.push(dpi);
        }
    }
    notes.sort_unstable();
    notes
}

/// Whether a setting is one of the sensitivity axes.
fn is_dpi(key: Key) -> bool {
    matches!(key, Key::Dpi | Key::DpiY)
}

/// The ends of the sensitivity slider, or none when the mouse reports no range
/// to slide through.
fn dpi_range(state: &DeviceState) -> Option<(f32, f32)> {
    let values = state.capabilities.supported_dpi.as_ref()?;
    let min = f32::from(*values.iter().min()?);
    let max = f32::from(*values.iter().max()?);
    (min > 0.0 && max > min).then_some((min, max))
}

/// Track shares reserved for uncommon low and high DPI ranges.
const TRACK_HEAD: f32 = 0.1;
const TRACK_TAIL: f32 = 0.2;

/// Piecewise-logarithmic DPI track with compressed outer ranges.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Track {
    min: f32,
    max: f32,
    /// The lowest and highest setting the scale calls common.
    low: f32,
    high: f32,
    head: f32,
    tail: f32,
}

impl Track {
    /// Lays a range out around the common settings a spec offers. A range with
    /// no room on one side of them simply has no tail there.
    fn new(spec: &Spec, min: f32, max: f32) -> Self {
        let common: Vec<f32> = spec
            .choices
            .iter()
            .filter_map(|(choice, _)| choice.parse::<f32>().ok())
            .filter(|dpi| *dpi > min && *dpi < max)
            .collect();
        let low = common.iter().copied().fold(f32::MAX, f32::min);
        let high = common.iter().copied().fold(0.0, f32::max);
        // The band has to be worth setting apart: a hair above the floor or
        // below the ceiling is not, and neither is an empty one.
        let head = if low > min * 1.2 { TRACK_HEAD } else { 0.0 };
        let tail = if high < max / 1.2 { TRACK_TAIL } else { 0.0 };
        match low < high {
            true => Self {
                min,
                max,
                low: if head > 0.0 { low } else { min },
                high: if tail > 0.0 { high } else { max },
                head,
                tail,
            },
            // Nothing to centre on, so the whole range reads as one stretch.
            false => Self {
                min,
                max,
                low: min,
                high: max,
                head: 0.0,
                tail: 0.0,
            },
        }
    }

    /// Where a sensitivity sits along the track, from 0 at its low end to 1.
    fn at(self, dpi: f32) -> f32 {
        let across = |value: f32, from: f32, to: f32| (value / from).ln() / (to / from).ln();
        let band = 1.0 - self.head - self.tail;
        match dpi.clamp(self.min, self.max) {
            dpi if dpi < self.low => self.head * across(dpi, self.min, self.low),
            dpi if dpi > self.high => {
                self.head + band + self.tail * across(dpi, self.high, self.max)
            }
            dpi => self.head + band * across(dpi, self.low, self.high),
        }
    }

    /// The sensitivity a point along the track stands for.
    fn dpi(self, at: f32) -> f32 {
        let along = |share: f32, from: f32, to: f32| from * (to / from).powf(share);
        let band = 1.0 - self.head - self.tail;
        match at.clamp(0.0, 1.0) {
            at if at < self.head => along(at / self.head, self.min, self.low),
            at if at > self.head + band => {
                along((at - self.head - band) / self.tail, self.high, self.max)
            }
            at => along((at - self.head) / band, self.low, self.high),
        }
    }
}

/// The increment closest to a value, of those the sensor reports. Without a
/// reported list the value stands as typed, and validation catches it.
fn nearest_dpi(supported: Option<&[u16]>, value: f32) -> u16 {
    let rounded = value.round().clamp(0.0, f32::from(u16::MAX)) as u16;
    supported
        .and_then(|values| {
            values.iter().copied().min_by(|a, b| {
                let distance = |dpi: u16| (f32::from(dpi) - value).abs();
                distance(*a).total_cmp(&distance(*b))
            })
        })
        .unwrap_or(rounded)
}

/// Dragging lands on a round number rather than on whatever the pointer was
/// over: the sensor's increments are far finer than a hand on a track.
fn round_dpi(value: f32) -> f32 {
    let step = match value {
        value if value < 5000.0 => 50.0,
        value if value < 10000.0 => 100.0,
        _ => 500.0,
    };
    (value / step).round() * step
}

/// A sensitivity as it is written down. Four digits are read as one number in
/// this context, so only five carry a separator.
fn dpi_text(value: u16) -> String {
    let digits = value.to_string();
    if digits.len() < 5 {
        return digits;
    }
    let mut grouped = String::with_capacity(digits.len() + 1);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Slider scale labels: endpoints plus available common presets.
fn scale_notes(spec: &Spec, min: f32, max: f32) -> Vec<u16> {
    let mut notes = vec![min.round() as u16];
    notes.extend(
        spec.choices
            .iter()
            .filter_map(|(choice, _)| choice.parse::<u16>().ok())
            .filter(|dpi| f32::from(*dpi) > min && f32::from(*dpi) < max),
    );
    notes.push(max.round() as u16);
    notes.sort_unstable();
    notes.dedup();
    notes
}

/// How a connection is drawn wherever it is named.
fn connection_icon(wireless: bool) -> IconName {
    if wireless {
        IconName::Wifi
    } else {
        IconName::Plug
    }
}

/// A rate as a person says it: 8000 Hz is "8K".
fn rate_name(hz: u32) -> String {
    if hz >= 1000 && hz.is_multiple_of(1000) {
        format!("{}K", hz / 1000)
    } else {
        hz.to_string()
    }
}

/// What a rate means once it reaches the hand: the gap between two reports,
/// and what holding that gap open costs.
fn rate_readout(hz: Option<u32>, on_battery: bool) -> gpui_kit::Div {
    let interval = hz
        .filter(|hz| *hz > 0)
        .map(|hz| {
            let ms = format!("{:.3}", 1000.0 / hz as f64);
            let ms = ms.trim_end_matches('0').trim_end_matches('.').to_owned();
            SharedString::from(format!("{ms} ms"))
        })
        .unwrap_or(SharedString::new_static("—"));

    // A cabled mouse runs on the host's power, so its rate costs nothing to
    // hold; only the radio pays for a shorter interval.
    let (cost, tone) = match (on_battery, hz) {
        (false, _) => (SharedString::new_static("None on the cable"), MUTED),
        (true, None) => (SharedString::new_static("—"), MUTED),
        (true, Some(hz)) if hz >= 8000 => (SharedString::new_static("Very high"), WARNING),
        (true, Some(hz)) if hz >= 4000 => (SharedString::new_static("High"), WARNING),
        (true, Some(hz)) if hz >= 1000 => (SharedString::new_static("Moderate"), TEXT),
        (true, Some(_)) => (SharedString::new_static("Low"), SUCCESS),
    };

    ui::readout(vec![
        ("REPORT INTERVAL", interval, TEXT),
        ("BATTERY COST", cost, tone),
    ])
}

/// What each group of settings is for, shown under its title.
fn group_help(group: &str) -> &'static str {
    match group {
        "Sensitivity" => "How far the pointer travels for a given hand movement.",
        "Polling rate" => "How often the mouse reports its position to this computer.",
        "Buttons" => "Choose the host-visible action for each physical mouse button.",
        "Sensor" => "Tracking behaviour on the surface and just above it.",
        "Bunny hopping" => {
            "Keeps the sensor reporting through a short lift, so re-placing the mouse mid-motion \
             does not cut the aim."
        }
        "Configuration" => "Whether the mouse follows this app or its own onboard profile.",
        "Lighting" => "Written when you apply. The agent cannot read current lighting back.",
        "Presentation" => {
            "How this mouse is named and pictured in gflick. Neither is written to the device."
        }
        _ => "",
    }
}

/// The glyph that introduces each card.
fn group_icon(group: &str) -> IconName {
    match group {
        "Sensitivity" => IconName::MousePointer,
        "Polling rate" => IconName::Gauge,
        "Buttons" => IconName::Mouse,
        "Sensor" => IconName::Radar,
        "Bunny hopping" => IconName::Mouse,
        // The card is about which brain the mouse listens to, its own or ours.
        "Configuration" => IconName::Cpu,
        "Lighting" => IconName::Lightbulb,
        "Presentation" => IconName::Image,
        _ => IconName::Settings,
    }
}

/// Cards read top to bottom in the order a person tunes a mouse.
fn group_order(group: &str) -> usize {
    match group {
        "Sensitivity" => 0,
        "Polling rate" => 1,
        "Sensor" => 2,
        "Bunny hopping" => 3,
        "Configuration" => 4,
        "Lighting" => 5,
        "Presentation" => 6,
        "Buttons" => 7,
        _ => 8,
    }
}

/// Enough room for the value plus its unit, and no more: a full-width field for
/// a four-digit number reads as a text area.
fn input_width(key: Key) -> f32 {
    match key {
        Key::Nickname => 220.0,
        Key::Color => 150.0,
        _ => 128.0,
    }
}

impl Editor {
    /// A typed value, recessed so it reads as a field rather than a label.
    fn render_entry(&self, field: &Field, width: Pixels) -> gpui_kit::Div {
        let unit = field.spec.unit;
        div().w(width).flex_shrink_0().child(
            Input::new(&field.input)
                .disabled(self.busy)
                .h(px(36.0))
                // Override Kit's translucent fill to keep inputs visually recessed.
                .bg(rgb(DEEP))
                .border_color(rgb(LINE_STRONG))
                .when(!unit.is_empty(), |input| {
                    input.suffix(
                        div()
                            .pr_2()
                            .text_size(text::MICRO)
                            .text_color(rgb(MUTED_2))
                            .child(unit),
                    )
                }),
        )
    }

    /// The control on the right of a settings row.
    fn render_control(
        &self,
        field: &Field,
        value: &str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let spec = &field.spec;
        let key = spec.key;

        if matches!(key, Key::Button(_)) {
            return self.render_mapping(spec, value, None, cx);
        }

        if spec.text {
            return self
                .render_entry(field, px(input_width(key)))
                .into_any_element();
        }

        match Control::for_spec(spec) {
            Control::Toggle => self.render_toggle(spec, value, cx).into_any_element(),
            Control::Segmented => self.render_segmented(spec, value, cx).into_any_element(),
            Control::Chips => self.render_chips(spec, value, cx).into_any_element(),
            Control::Swatches => self.render_swatches(spec, value, cx).into_any_element(),
        }
    }

    /// A physical-button picker, optionally sized to fill a callout card.
    fn render_mapping(
        &self,
        spec: &Spec,
        value: &str,
        width: Option<Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let key = spec.key;
        let choices = spec.choices.clone();
        let selected = value.to_owned();
        let label = action_label(spec, value);
        let editor = cx.entity().downgrade();
        Button::new(SharedString::from(format!("{key:?}-mapping")))
            .outline()
            .dropdown_caret(true)
            .disabled(self.busy)
            .accessibility_label(format!("{}: {label}", spec.label))
            .when_some(width, |button, width| button.w(width).h(px(34.0)))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .child(
                        Icon::new(action_icon(value))
                            .with_size(px(14.0))
                            .text_color(rgb(MUTED_2)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(text::BODY)
                            .child(label),
                    ),
            )
            .dropdown_menu(move |menu, _, _| {
                choices
                    .iter()
                    // Put checks opposite action icons and constrain the menu to its control.
                    .fold(
                        menu.check_side(Side::Right)
                            .max_h(MENU_HEIGHT)
                            .when_some(width, |menu, width| menu.min_w(width)),
                        |menu, (value, label)| {
                            let editor = editor.clone();
                            let action = value.clone();
                            let label = SharedString::from(label.clone());
                            // Separate named actions from raw button numbers.
                            menu.when(value == "6", PopupMenu::separator).item(
                                // Element rows allow more padding than Kit's fixed plain rows.
                                PopupMenuItem::element(move |_, _| {
                                    // Flexible, so the check stays on the far
                                    // edge rather than trailing the label.
                                    div().flex_1().py(MENU_ROW_PAD).child(label.clone())
                                })
                                .icon(action_icon(value))
                                .checked(value == &selected)
                                .on_click(move |_, window, cx| {
                                    let _ = editor.update(cx, |editor, cx| {
                                        editor.set(key, action.clone(), window, cx)
                                    });
                                }),
                            )
                        },
                    )
                    .scrollable(true)
            })
            .into_any_element()
    }

    /// How many physical controls this device's draft assigns.
    pub fn mapped_buttons(&self) -> usize {
        self.fields
            .iter()
            .filter(|field| matches!(field.spec.key, Key::Button(_)))
            .count()
    }

    /// The buttons page's callouts: the artwork belongs to the page, the draft
    /// behind each picker belongs here.
    pub fn callouts(&self, picker: Pixels, cx: &mut Context<Self>) -> Vec<Callout> {
        let mut callouts = Vec::new();
        for field in &self.fields {
            if !matches!(field.spec.key, Key::Button(_)) {
                continue;
            }
            let value = field.input.read(cx).value().to_string();
            callouts.push(Callout {
                label: field.spec.label.into(),
                was: (value != field.spec.value)
                    .then(|| action_label(&field.spec, &field.spec.value)),
                picker: self.render_mapping(&field.spec, &value, Some(picker), cx),
            });
        }
        callouts
    }

    /// A switch with an explicit state label and accent when enabled.
    fn render_toggle(&self, spec: &Spec, value: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let key = spec.key;
        let on = value == "on";
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Switch::new(SharedString::from(format!("{key:?}-switch")))
                    .checked(on)
                    .disabled(self.busy)
                    .accessibility_label(spec.label)
                    .on_click(cx.listener(move |editor, on: &bool, window, cx| {
                        editor.set(key, if *on { "on" } else { "off" }.to_owned(), window, cx)
                    })),
            )
            .child(ui::switch_state(on))
    }

    /// One control with one lit segment. Reads by position, so the selection is
    /// a neutral fill rather than the accent, which is reserved for values.
    fn render_segmented(
        &self,
        spec: &Spec,
        value: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = spec.key;
        let segments = spec.choices.iter().map(|(choice, label)| {
            let selected = value == choice;
            let choice = choice.clone();
            let style = ButtonCustomVariant::new(cx)
                .color(if selected {
                    rgb(SURFACE_3).into()
                } else {
                    cx.theme().transparent
                })
                .foreground(rgb(if selected { TEXT } else { MUTED_2 }).into())
                .hover(rgb(SURFACE_2).into())
                .active(rgb(SURFACE_3).into())
                .shadow(false);
            Button::new(SharedString::from(format!("{key:?}-{choice}")))
                .custom(style)
                .compact()
                .rounded(px(6.0))
                .accessibility_label(label.clone())
                .child(ui::button_label(label.clone(), text::SMALL))
                .disabled(self.busy)
                .on_click(cx.listener(move |editor, _, window, cx| {
                    editor.set(key, choice.clone(), window, cx)
                }))
        });
        div()
            .flex()
            .flex_shrink_0()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded_lg()
            .bg(rgb(DEEP))
            .children(segments)
    }

    /// A grid of values. The selection carries the accent because which value is
    /// live is the whole point of the row.
    fn render_chips(&self, spec: &Spec, value: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let key = spec.key;
        let chips = spec.choices.iter().map(|(choice, label)| {
            let selected = value == choice;
            let choice = choice.clone();
            Button::new(SharedString::from(format!("{key:?}-{choice}")))
                .map(|button| {
                    if selected {
                        button.primary()
                    } else {
                        button.outline()
                    }
                })
                .compact()
                .accessibility_label(label.clone())
                .child(ui::button_label(label.clone(), text::BODY))
                .disabled(self.busy)
                .on_click(cx.listener(move |editor, _, window, cx| {
                    editor.set(key, choice.clone(), window, cx)
                }))
        });
        div()
            .min_w_0()
            .max_w(px(348.0))
            .flex()
            .flex_wrap()
            .justify_end()
            .gap(px(6.0))
            .children(chips)
    }

    /// The sensitivity card: a track per axis, the value it lands on, and the
    /// settings people actually pick.
    fn render_sensitivity(&self, rows: &[&Field], cx: &mut Context<Self>) -> impl IntoElement {
        let x = rows
            .iter()
            .find(|field| field.spec.key == Key::Dpi)
            .or(rows.first())
            .expect("the sensitivity card is only built for a device that reports DPI");
        let y = rows.iter().find(|field| field.spec.key == Key::DpiY);
        let help = group_help("Sensitivity");
        let header = match y {
            Some(_) => ui::card_header_aside(
                group_icon("Sensitivity"),
                "Sensitivity",
                help,
                self.render_axis_link(cx),
            )
            .into_any_element(),
            None => {
                ui::card_header(group_icon("Sensitivity"), "Sensitivity", help).into_any_element()
            }
        };
        // Linked axes are one sensitivity with one track; the vertical one is
        // written to match on every edit.
        let split = y.filter(|_| self.separate_axes);
        ui::card().child(header).child(
            ui::card_body()
                .flex()
                .flex_col()
                .pt_4()
                .pb_4()
                .child(self.render_axis(
                    x,
                    if split.is_some() {
                        "HORIZONTAL · X"
                    } else {
                        "CHOOSE DPI"
                    },
                    false,
                    cx,
                ))
                .children(split.map(|y| self.render_axis(y, "VERTICAL · Y", true, cx))),
        )
    }

    /// One sensitivity: what it is called, the number, the track, the presets.
    fn render_axis(
        &self,
        field: &Field,
        axis: &'static str,
        divided: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let spec = &field.spec;
        let value = field.input.read(cx).value().to_string();
        let pending = (value != spec.value && !spec.value.is_empty())
            .then(|| format!("{} DPI on the device", spec.value));
        let track = self.track(field);
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .when(divided, |el| {
                el.mt(px(18.0))
                    .pt(px(18.0))
                    .border_t_1()
                    .border_color(rgb(LINE_SOFT))
            })
            .child(
                div()
                    .min_w_0()
                    .flex()
                    // Fixed, so the line the draft note appears on does not
                    // grow with it and shift the page under the pointer.
                    .h(px(16.0))
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(ui::caption(axis))
                    .when_some(pending, |el, was| {
                        el.child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(ui::dot(ACCENT_TEXT))
                                .child(
                                    div()
                                        .text_size(text::TINY)
                                        .text_color(rgb(ACCENT_TEXT))
                                        .child(was),
                                ),
                        )
                    }),
            )
            .child(
                // The track and the number it lands on stand side by side, with
                // the scale reading under the track it belongs to.
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_4()
                    .when_some(field.slider.clone().zip(track), |el, (slider, track)| {
                        el.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(Slider::new(&slider).disabled(self.busy))
                                .child(self.render_scale(field, &value, track, cx)),
                        )
                    })
                    .child(self.render_value(field)),
            )
    }

    /// The number the track lands on, typed directly. The block is titled with
    /// the unit, so the field carries digits alone.
    fn render_value(&self, field: &Field) -> gpui_kit::Div {
        div().w(px(88.0)).flex_shrink_0().child(
            Input::new(&field.input)
                .disabled(self.busy)
                .h(px(36.0))
                // A number with no unit beside it reads from its middle.
                .text_center()
                // Override Kit's translucent fill to keep inputs visually recessed.
                .bg(rgb(DEEP))
                .border_color(rgb(LINE_STRONG)),
        )
    }

    /// The scale under a track: the two ends, and the settings people reach
    /// for. Each note stands where the thumb stands for it, and sets it.
    fn render_scale(
        &self,
        field: &Field,
        value: &str,
        track: Track,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let key = field.spec.key;
        // The track measures itself as it paints. Before the first paint a
        // comfortable width is assumed, which the next frame corrects.
        let width = field
            .slider
            .as_ref()
            .map(|slider| slider.read(cx).bounds().size.width.as_f32())
            .filter(|width| *width > 0.0)
            .unwrap_or(600.0);
        if (field.scale_width.get() - width).abs() > 0.5 {
            // The guess was wrong, or the window changed width: fit again with
            // what the track turned out to be.
            field.scale_width.set(width);
            cx.notify();
        }
        div().relative().w_full().h(px(19.0)).children(
            fitted_notes(&field.spec, value, track, width)
                .into_iter()
                .map(|dpi| {
                    let selected = value == dpi.to_string();
                    let note = div()
                        .id(SharedString::from(format!("{key:?}-note-{dpi}")))
                        .absolute()
                        .top_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .cursor_pointer()
                        .when(!self.busy, |el| {
                            el.on_click(cx.listener(move |editor, _, window, cx| {
                                editor.set_dpi(key, dpi, window, cx)
                            }))
                        })
                        .child(div().w(px(1.0)).h(px(4.0)).bg(rgb(if selected {
                            ACCENT_TEXT
                        } else {
                            LINE_STRONG
                        })))
                        .child(
                            div()
                                .text_size(text::MICRO)
                                .when(selected, |el| el.font_semibold())
                                .text_color(rgb(if selected { ACCENT_TEXT } else { MUTED_2 }))
                                .hover(|el| el.text_color(rgb(TEXT)))
                                .child(dpi_text(dpi)),
                        );
                    // The ends hang inside the track; the notes between centre on it.
                    match f32::from(dpi) {
                        dpi if dpi <= track.min => note.left_0().items_start(),
                        dpi if dpi >= track.max => note.right_0().items_end(),
                        dpi => note
                            .left(relative(track.at(dpi)))
                            .w(px(40.0))
                            .ml(px(-20.0))
                            .items_center(),
                    }
                }),
        )
    }

    /// The tie between the two axes. Off, one sensitivity drives both.
    fn render_axis_link(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(9.0))
            .child(
                div()
                    .text_size(text::SMALL)
                    .text_color(rgb(MUTED))
                    .child("Separate X / Y"),
            )
            .child(
                Switch::new("dpi-axes")
                    .checked(self.separate_axes)
                    .disabled(self.busy)
                    .accessibility_label("Separate X and Y sensitivity")
                    .on_click(cx.listener(|editor, separate: &bool, window, cx| {
                        editor.separate_axes = *separate;
                        // Tying the axes back together pulls the vertical one
                        // onto the horizontal, which is the one on show.
                        let linked = (!*separate)
                            .then(|| {
                                editor
                                    .fields
                                    .iter()
                                    .find(|field| field.spec.key == Key::Dpi)
                                    .map(|field| field.input.read(cx).value().to_string())
                            })
                            .flatten();
                        if let Some(value) = linked {
                            editor.set(Key::DpiY, value, window, cx);
                        }
                        cx.notify();
                    })),
            )
    }

    /// One polling-rate picker with a wired/wireless switch and cost readout.
    fn render_polling(&self, rows: &[&Field], cx: &mut Context<Self>) -> impl IntoElement {
        let showing = self.shown_connection();
        let shown = rows
            .iter()
            .find(|field| field.spec.key == showing)
            .or(rows.first())
            .expect("the polling card is only built for a device that reports rates");
        let value = shown.input.read(cx).value().to_string();
        let hz: Option<u32> = value.parse().ok();
        // A shared rate draws battery only while the current connection is wireless.
        let on_battery = match shown.spec.key {
            Key::Wireless => true,
            _ if rows.len() == 1 => self.baseline.device.connection == DeviceConnection::Receiver,
            _ => false,
        };
        let help = group_help("Polling rate");

        // A rate for the connection the mouse is not on is still worth setting,
        // but it will not be felt until the mouse is on that connection.
        let mut notices: Vec<(IconName, u32, SharedString)> = Vec::new();
        if let Some(hz) = hz.filter(|hz| *hz >= 4000) {
            notices.push((
                if on_battery {
                    IconName::Zap
                } else {
                    IconName::Info
                },
                if on_battery { WARNING } else { NOTICE_ICON },
                if on_battery {
                    format!(
                        "{} gives the lowest latency this mouse can reach, at its highest battery \
                         cost. The difference shows on a high-refresh display.",
                        rate_name(hz)
                    )
                } else {
                    format!(
                        "{} gives the lowest latency this mouse can reach. The difference shows \
                         on a high-refresh display.",
                        rate_name(hz)
                    )
                }
                .into(),
            ));
        }
        if rows.len() > 1 && shown.spec.key != self.live_connection() {
            notices.push((
                connection_icon(shown.spec.key == Key::Wireless),
                NOTICE_ICON,
                if shown.spec.key == Key::Wireless {
                    "The mouse is on the cable now. This rate takes over when it goes wireless."
                } else {
                    "The mouse is on the receiver now. This rate takes over when it is plugged in."
                }
                .into(),
            ));
        }

        let header = if rows.len() > 1 {
            ui::card_header_aside(
                group_icon("Polling rate"),
                "Polling rate",
                help,
                self.render_connection_switch(rows, cx),
            )
            .into_any_element()
        } else {
            ui::card_header(group_icon("Polling rate"), "Polling rate", help).into_any_element()
        };

        ui::card().child(header).child(
            ui::card_body()
                .flex()
                .flex_col()
                .gap_4()
                .pt_4()
                .pb_4()
                .child(self.render_rates(&shown.spec, &value, cx))
                .child(rate_readout(hz, on_battery))
                .children(
                    notices
                        .into_iter()
                        .map(|(icon, tone, body)| ui::notice(icon, tone, body)),
                ),
        )
    }

    /// Connection selector; a dot keeps hidden, unapplied edits visible.
    fn render_connection_switch(
        &self,
        rows: &[&Field],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let showing = self.shown_connection();
        let segments = rows.iter().map(|field| {
            let key = field.spec.key;
            let selected = key == showing;
            let pending = field.input.read(cx).value() != field.spec.value;
            let style = ButtonCustomVariant::new(cx)
                .color(if selected {
                    rgb(SURFACE_3).into()
                } else {
                    cx.theme().transparent
                })
                .foreground(rgb(if selected { TEXT } else { MUTED_2 }).into())
                .hover(rgb(SURFACE_2).into())
                .active(rgb(SURFACE_3).into())
                .shadow(false);
            Button::new(SharedString::from(format!("connection-{key:?}")))
                .custom(style)
                .compact()
                .rounded(px(6.0))
                .accessibility_label(field.spec.label)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .text_size(text::SMALL)
                        .child(Icon::new(connection_icon(key == Key::Wireless)).with_size(px(13.0)))
                        .child(field.spec.label)
                        .when(pending, |el| el.child(ui::dot(ACCENT_TEXT))),
                )
                .on_click(cx.listener(move |editor, _, _, cx| {
                    editor.connection = Some(key);
                    cx.notify();
                }))
        });
        div()
            .flex()
            .flex_shrink_0()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded_lg()
            .bg(rgb(DEEP))
            .children(segments)
    }

    /// The rates themselves: the number, and the unit on the same line.
    fn render_rates(&self, spec: &Spec, value: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let key = spec.key;
        let buttons = spec.choices.iter().map(|(choice, label)| {
            let selected = value == choice;
            let hz: u32 = choice.parse().unwrap_or_default();
            let choice = choice.clone();
            Button::new(SharedString::from(format!("{key:?}-{choice}")))
                .map(|button| {
                    if selected {
                        button.primary()
                    } else {
                        button.outline()
                    }
                })
                .w(px(76.0))
                .h(px(38.0))
                .rounded(px(8.0))
                .disabled(self.busy)
                .accessibility_label(label.clone())
                .child(
                    div()
                        .flex()
                        // The unit sits on the number's baseline rather than
                        // centred on it, the way a unit is set next to a figure.
                        .items_baseline()
                        .justify_center()
                        .gap(px(3.0))
                        .child(
                            div()
                                .text_size(text::BODY)
                                .font_semibold()
                                .child(rate_name(hz)),
                        )
                        .child(
                            div()
                                .text_size(text::MICRO)
                                .text_color(if selected {
                                    rgba(0xffffffb0)
                                } else {
                                    rgb(MUTED_2)
                                })
                                .child("Hz"),
                        ),
                )
                .on_click(cx.listener(move |editor, _, window, cx| {
                    editor.set(key, choice.clone(), window, cx)
                }))
        });
        div().flex().flex_wrap().gap(px(7.0)).children(buttons)
    }

    fn render_swatches(
        &self,
        spec: &Spec,
        value: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = spec.key;
        let swatches = spec.choices.iter().map(|(choice, label)| {
            let selected = value == choice;
            let color = self
                .baseline
                .device
                .available_colors()
                .iter()
                .find(|color| color.as_str() == choice)
                .and_then(|color| self.baseline.device.model()?.swatch(*color))
                .expect("appearance choices have verified swatches");
            let choice = choice.clone();
            let style = ButtonCustomVariant::new(cx)
                .foreground(rgb(TEXT).into())
                .hover(rgb(SURFACE_2).into())
                .active(rgb(SURFACE_3).into())
                .shadow(false);
            Button::new(SharedString::from(format!("{key:?}-{choice}")))
                .custom(style)
                .size(px(36.0))
                .p_0()
                .rounded(px(10.0))
                .disabled(self.busy)
                .tooltip(label.clone())
                .accessibility_label(format!(
                    "{}{}",
                    label,
                    if selected { ", selected" } else { "" }
                ))
                .child(
                    // A subtle ring marks selection without overpowering the finish.
                    div()
                        .size(px(28.0))
                        .flex_shrink_0()
                        .p(px(2.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(if selected { rgba(0xffffff8c) } else { rgba(0) })
                        .child(
                            div()
                                .size_full()
                                .rounded(px(5.0))
                                .border_1()
                                .border_color(rgba(0xffffff26))
                                .bg(rgb(color)),
                        ),
                )
                .on_click(cx.listener(move |editor, _, window, cx| {
                    editor.set(key, choice.clone(), window, cx)
                }))
        });
        div().flex().flex_shrink_0().gap_2().children(swatches)
    }
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut groups: BTreeMap<usize, (&str, Vec<gpui_kit::AnyElement>)> = BTreeMap::new();
        let mut polling: Vec<&Field> = Vec::new();
        let mut sensitivity: Vec<&Field> = Vec::new();
        for field in &self.fields {
            // Device tuning, physical button assignments, and presentation
            // each have their own page even though one editor keeps the draft.
            let field_page = match field.spec.key {
                Key::Nickname | Key::Appearance => EditorPage::Details,
                Key::Button(_) => EditorPage::Buttons,
                _ => EditorPage::Performance,
            };
            if field_page != self.page {
                continue;
            }
            if self.callouts_drawn && matches!(field.spec.key, Key::Button(_)) {
                continue;
            }
            // The rate rows are one control between them, drawn below.
            if matches!(field.spec.key, Key::Wired | Key::Wireless) {
                polling.push(field);
                continue;
            }
            // Both axes share one card, which draws its own tracks.
            if is_dpi(field.spec.key) {
                sensitivity.push(field);
                continue;
            }
            let spec = &field.spec;
            let value = field.input.read(cx).value().to_string();
            let dirty = value != spec.value;
            let pending = dirty
                .then(|| {
                    spec.choices
                        .iter()
                        .find(|(choice, _)| choice == &spec.value)
                        .map(|(_, label)| label.clone())
                        .or_else(|| (!spec.value.is_empty()).then(|| spec.value.clone()))
                })
                .flatten();

            let row = ui::setting_row()
                .child(
                    ui::row_copy(spec.label, &spec.help, dirty).when_some(pending, |el, was| {
                        el.child(
                            div()
                                .text_size(text::TINY)
                                .text_color(rgb(ACCENT_TEXT))
                                .child(format!("On the device: {was}")),
                        )
                    }),
                )
                .child(self.render_control(field, &value, cx))
                .into_any_element();

            groups
                .entry(group_order(spec.group))
                .or_insert((spec.group, Vec::new()))
                .1
                .push(row);
        }

        // Cards are keyed by group order so the polling card lands in the
        // reading order its settings would have had.
        let mut cards: BTreeMap<usize, gpui_kit::AnyElement> = groups
            .into_iter()
            .map(|(order, (group, rows))| {
                let card = ui::card()
                    .child(ui::card_header(group_icon(group), group, group_help(group)))
                    .child(ui::card_body().children(rows.into_iter().enumerate().map(
                        |(index, row)| {
                            // The header already rules the first row off, so
                            // only the rows after it carry a divider.
                            div()
                                .min_w_0()
                                .when(index > 0, |el| el.border_t_1().border_color(rgb(LINE_SOFT)))
                                .child(row)
                        },
                    )));
                (order, card.into_any_element())
            })
            .collect();
        if !polling.is_empty() {
            cards.insert(
                group_order("Polling rate"),
                self.render_polling(&polling, cx).into_any_element(),
            );
        }
        if !sensitivity.is_empty() {
            cards.insert(
                group_order("Sensitivity"),
                self.render_sensitivity(&sensitivity, cx).into_any_element(),
            );
        }

        let cards: Vec<gpui_kit::AnyElement> = cards.into_values().collect();
        if cards.is_empty() && self.page == EditorPage::Buttons {
            return ui::card()
                .child(ui::card_header(
                    IconName::Mouse,
                    "Button assignments",
                    "This mouse does not report host-visible button remapping.",
                ))
                .child(ui::card_body().child(ui::notice(
                    IconName::Info,
                    NOTICE_ICON,
                    "No remappable buttons were reported for this device.",
                )))
                .into_any_element();
        }
        if self.columns > 1 && cards.len() > 1 {
            let mut split: Vec<Vec<gpui_kit::AnyElement>> = vec![Vec::new(), Vec::new()];
            for (index, card) in cards.into_iter().enumerate() {
                split[index % 2].push(card);
            }
            return div()
                .w_full()
                .min_w_0()
                .flex()
                .items_start()
                .gap_4()
                .children(split.into_iter().map(|column| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .children(column)
                }))
                .into_any_element();
        }
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .children(cards)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drag lands on a value a person would name, and then on the increment
    /// the sensor has closest to it.
    #[test]
    fn dragging_settles_on_round_supported_values() {
        let supported: Vec<u16> = (100..=1990).step_by(10).collect();
        assert_eq!(round_dpi(1603.0), 1600.0);
        assert_eq!(round_dpi(1626.0), 1650.0);
        assert_eq!(round_dpi(7340.0), 7300.0);
        assert_eq!(round_dpi(23_800.0), 24_000.0);
        assert_eq!(nearest_dpi(Some(&supported), round_dpi(1603.0)), 1600);
        assert_eq!(nearest_dpi(Some(&supported), 1324.0), 1320);
        // Outside the reported range, the nearest end still wins.
        assert_eq!(nearest_dpi(Some(&supported), 40_000.0), 1990);
        assert_eq!(nearest_dpi(Some(&[]), 1324.0), 1324);
        assert_eq!(nearest_dpi(None, 1324.0), 1324);
    }

    fn common(values: &[u16]) -> Spec {
        Spec {
            key: Key::Dpi,
            group: "Sensitivity",
            label: "DPI",
            help: String::new(),
            unit: "DPI",
            value: String::new(),
            choices: values
                .iter()
                .map(|dpi| (dpi.to_string(), dpi.to_string()))
                .collect(),
            text: true,
        }
    }

    /// The settings people use take the middle of the track, and the range
    /// beyond them folds into short tails at either end.
    #[test]
    fn the_track_gives_its_middle_to_the_settings_people_use() {
        let track = Track::new(
            &common(&[400, 600, 800, 1200, 1600, 2400, 3200]),
            100.0,
            44_000.0,
        );
        assert_eq!(track.at(100.0), 0.0);
        assert_eq!(track.at(44_000.0), 1.0);
        assert_eq!(track.at(400.0), TRACK_HEAD);
        assert_eq!(track.at(3200.0), 1.0 - TRACK_TAIL);
        // Every doubling inside the band covers the same distance.
        let step = track.at(800.0) - track.at(400.0);
        for (lower, upper) in [(800.0, 1600.0), (1600.0, 3200.0)] {
            assert!((track.at(upper) - track.at(lower) - step).abs() < 0.001);
        }
        // A point on the track and the value it stands for agree both ways.
        for dpi in [100.0, 137.0, 400.0, 900.0, 1600.0, 5000.0, 44_000.0] {
            assert!(
                (track.dpi(track.at(dpi)) - dpi).abs() < 0.5,
                "{dpi} round trips"
            );
        }
    }

    /// A range with nothing worth setting apart reads as one even stretch.
    #[test]
    fn a_track_without_a_band_is_one_stretch() {
        let track = Track::new(&common(&[]), 400.0, 3200.0);
        assert_eq!(track.head, 0.0);
        assert_eq!(track.tail, 0.0);
        assert!((track.at(1131.0) - 0.5).abs() < 0.01);
    }

    /// Scale labels thin without losing endpoints/current value or overlapping.
    #[test]
    fn the_scale_thins_to_the_width_it_is_given() {
        let spec = common(&[400, 800, 1200, 1600, 2400, 3200]);
        let track = Track::new(&spec, 100.0, 44_000.0);
        let wide = fitted_notes(&spec, "1600", track, 700.0);
        assert_eq!(wide, [100, 400, 800, 1200, 1600, 2400, 3200, 44_000]);

        for width in [200.0, 280.0, 330.0, 480.0, 700.0] {
            let notes = fitted_notes(&spec, "1200", track, width);
            assert!(notes.contains(&100), "the low end is written at {width}");
            assert!(
                notes.contains(&44_000),
                "the high end is written at {width}"
            );
            assert!(
                notes.contains(&1200),
                "the live value is written at {width}"
            );
            for pair in notes.windows(2) {
                let (left, right) = (pair[0], pair[1]);
                let gap = note_centre(right, track, width) - note_centre(left, track, width);
                assert!(
                    gap >= note_half_width(left) + note_half_width(right),
                    "{left} and {right} collide on a {width}pt track"
                );
            }
        }
    }

    #[test]
    fn only_five_digit_sensitivities_carry_a_separator() {
        assert_eq!(dpi_text(100), "100");
        assert_eq!(dpi_text(1600), "1600");
        assert_eq!(dpi_text(44_000), "44,000");
    }
}
