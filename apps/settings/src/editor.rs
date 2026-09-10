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
    ActiveTheme, Disableable, Icon, Sizable as _,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Subscription, Window,
    div, prelude::*, px, rgb, rgba,
};
use std::collections::BTreeMap;

pub enum EditorEvent {
    Changed,
    Updated(Box<DeviceState>),
}
struct Field {
    spec: Spec,
    input: Entity<InputState>,
}
pub struct Editor {
    pub baseline: DeviceState,
    fields: Vec<Field>,
    subscriptions: Vec<Subscription>,
    pub busy: bool,
    pub details: bool,
    pub message: Option<String>,
    pub failed: bool,
    /// Visible polling row; both connections retain their drafts and apply together.
    connection: Option<Key>,
}
impl EventEmitter<EditorEvent> for Editor {}
impl Editor {
    pub fn new(state: DeviceState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut editor = Self {
            baseline: state,
            fields: Vec::new(),
            subscriptions: Vec::new(),
            busy: false,
            details: false,
            message: None,
            failed: false,
            connection: None,
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
        for spec in settings::specs(&self.baseline) {
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(spec.value.clone(), window, cx);
                input
            });
            self.subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.message = None;
                        this.failed = false;
                        cx.emit(EditorEvent::Changed);
                        cx.notify();
                    }
                },
            ));
            self.fields.push(Field { spec, input });
        }
        cx.notify();
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
            // DPI values are points on a scale rather than named modes, so they
            // stay chips however few of them a mouse supports.
            Key::Dpi | Key::DpiY => Self::Chips,
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
            "How this mouse is named and pictured in GFlick. Neither is written to the device."
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
            let choices = spec.choices.clone();
            let selected = value.to_owned();
            let label = choices
                .iter()
                .find(|(v, _)| v == value)
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| "Not reported".into());
            let editor = cx.entity().downgrade();
            return Button::new(SharedString::from(format!("{key:?}-mapping")))
                .label(label)
                .outline()
                .dropdown_caret(true)
                .disabled(self.busy)
                .dropdown_menu(move |menu, _, _| {
                    choices
                        .iter()
                        .fold(menu, |menu, (value, label)| {
                            let editor = editor.clone();
                            let action = value.clone();
                            menu.item(
                                PopupMenuItem::new(label.clone())
                                    .checked(value == &selected)
                                    .on_click(move |_, window, cx| {
                                        let _ = editor.update(cx, |editor, cx| {
                                            editor.set(key, action.clone(), window, cx)
                                        });
                                    }),
                            )
                        })
                        .scrollable(true)
                })
                .into_any_element();
        }

        if spec.text {
            let entry = div().w(px(input_width(key))).flex_shrink_0().child(
                Input::new(&field.input)
                    .disabled(self.busy)
                    .h(px(36.0))
                    // Override Kit's translucent fill to keep inputs visually recessed.
                    .bg(rgb(DEEP))
                    .border_color(rgb(LINE_STRONG))
                    .when(!spec.unit.is_empty(), |input| {
                        input.suffix(
                            div()
                                .pr_2()
                                .text_size(text::MICRO)
                                .text_color(rgb(MUTED_2))
                                .child(spec.unit),
                        )
                    }),
            );
            // A free-form value can still have common ones worth one click, as
            // DPI does. They sit under the field they fill in.
            if spec.choices.is_empty() {
                return entry.into_any_element();
            }
            return div()
                .min_w_0()
                .flex()
                .flex_col()
                .items_end()
                .gap_2()
                .child(entry)
                .child(self.render_chips(spec, value, cx))
                .into_any_element();
        }

        match Control::for_spec(spec) {
            Control::Toggle => self.render_toggle(spec, value, cx).into_any_element(),
            Control::Segmented => self.render_segmented(spec, value, cx).into_any_element(),
            Control::Chips => self.render_chips(spec, value, cx).into_any_element(),
            Control::Swatches => self.render_swatches(spec, value, cx).into_any_element(),
        }
    }

    /// A flag, as a switch, with the state spelled out next to it. Carries the
    /// accent when on: unlike a segmented control, there is nothing else on it
    /// to read the state from.
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
        for field in &self.fields {
            // The nickname and the finish describe the mouse rather than tune
            // it, so they live on the details page.
            if matches!(field.spec.key, Key::Nickname | Key::Appearance) != self.details {
                continue;
            }
            // The rate rows are one control between them, drawn below.
            if matches!(field.spec.key, Key::Wired | Key::Wireless) {
                polling.push(field);
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

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .children(cards.into_values())
    }
}
