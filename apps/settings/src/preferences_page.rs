use crate::*;
use gpui_kit::component::{WindowExt, dialog::DialogButtonProps, switch::Switch};

/// What a service switch does when it is moved. Boxed because the two rows hand
/// the same builder two different listeners.
type SwitchAction = Box<dyn Fn(&bool, &mut Window, &mut gpui_kit::App)>;

#[derive(Clone, Copy)]
enum Preference {
    Apply,
    Discard,
    Profiles,
}
impl SettingsView {
    pub(crate) fn open_preferences(&mut self, cx: &mut Context<Self>) {
        self.page = Page::Preferences;
        if self.service_busy {
            cx.notify();
            return;
        }
        self.service_busy = true;
        self.startup_enabled = None;
        self.tray_enabled = None;
        self.agent_online = None;
        let task = cx.background_executor().spawn(async {
            (
                services::startup_status(),
                services::tray_status(),
                services::ping(),
                preferences::Preferences::load(),
            )
        });
        cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            let (startup, tray, online, preferences) = task.await;
            view.update(cx, |view, cx| {
                view.service_busy = false;
                view.agent_online = Some(online);
                view.preferences_loaded = preferences.is_ok();
                match preferences {
                    Ok(preferences) => {
                        view.preferences = preferences;
                        view.preference_error = None;
                    }
                    Err(e) => view.preference_error = Some(format!("{e:#}")),
                }
                match tray {
                    Ok(enabled) => view.tray_enabled = Some(enabled),
                    Err(error) => {
                        view.tray_enabled = None;
                        view.preference_error = Some(format!("{error:#}"));
                    }
                }
                match startup {
                    Ok(value) => view.startup_enabled = Some(value),
                    Err(e) => {
                        view.startup_enabled = None;
                        view.preference_error = Some(format!("{e:#}"));
                    }
                }
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach();
        cx.notify();
    }
    /// Turns both login items on the first time the app runs.
    ///
    /// They are on by default rather than opt-in, because a mouse whose owner
    /// installed this app expects it to hold its settings across a reboot. The
    /// flag is written whatever the outcome, so switching either one off later
    /// is not undone on the next launch.
    pub(crate) fn apply_startup_defaults(&mut self, cx: &mut Context<Self>) {
        if !self.preferences_loaded || self.preferences.startup_defaults_applied {
            return;
        }
        self.service_busy = true;
        let task = cx.background_executor().spawn(async {
            // Failures are left to the switches to report. This runs before
            // anyone has asked for it, so it is not the moment to raise an
            // error over a login item.
            let _ = services::set_startup(true);
            let _ = services::set_tray(true);
            (services::startup_status(), services::tray_status())
        });
        cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            let (startup, tray) = task.await;
            view.update(cx, |view, cx| {
                view.service_busy = false;
                view.startup_enabled = startup.ok();
                view.tray_enabled = tray.ok();
                let mut next = view.preferences.clone();
                next.startup_defaults_applied = true;
                if next.save().is_ok() {
                    view.preferences = next;
                }
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach();
    }
    fn set_preference(&mut self, key: Preference, value: bool, cx: &mut Context<Self>) {
        if !self.preferences_loaded {
            return;
        }
        let mut next = self.preferences.clone();
        match key {
            Preference::Apply => next.confirm_apply = value,
            Preference::Discard => next.confirm_discard = value,
            Preference::Profiles => next.confirm_profile_writes = value,
        }
        match next.save() {
            Ok(()) => {
                self.preferences = next;
                self.preference_error = None;
            }
            Err(error) => self.preference_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }
    fn set_startup(&mut self, value: bool, cx: &mut Context<Self>) {
        if self.service_busy {
            return;
        }
        self.service_busy = true;
        let task = cx.background_executor().spawn(async move {
            services::set_startup(value).and_then(|_| services::startup_status())
        });
        cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            let result = task.await;
            view.update(cx, |view, cx| {
                view.service_busy = false;
                match result {
                    Ok(enabled) => {
                        view.startup_enabled = Some(enabled);
                        view.preference_error = None;
                    }
                    Err(e) => view.preference_error = Some(format!("{e:#}")),
                }
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach();
        cx.notify();
    }
    fn restart_agent(&mut self, cx: &mut Context<Self>) {
        if self.service_busy
            || self.loading
            || self.working(cx)
            || self
                .editors
                .values()
                .any(|e| !e.read(cx).dirty(cx).is_empty())
        {
            return;
        }
        self.service_busy = true;
        self.agent_online = None;
        self.loading = true;
        let task = cx.background_executor().spawn(async {
            let result = services::restart().and_then(|_| agent::load_snapshot());
            let online = services::ping();
            (result, online)
        });
        cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            let (result, online) = task.await;
            view.update(cx, |view, cx| {
                view.agent_online = Some(online);
                view.service_busy = false;
                view.loading = false;
                match result {
                    Ok(snapshot) => {
                        view.adopt(snapshot);
                        view.agent_online = Some(true);
                        view.error = None;
                        view.preference_error = None;
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        view.preference_error = Some(message.clone());
                        view.error = Some(message);
                    }
                }
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn confirm_device_action(
        &mut self,
        editor: Entity<editor::Editor>,
        discard: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading || self.working(cx) {
            return;
        }
        let current = editor.read(cx);
        let dirty = current.dirty(cx);
        if dirty.is_empty() {
            return;
        }
        let values = current.values(cx);
        let onboard = values
            .get(&settings::Key::Source)
            .is_some_and(|v| !v.is_empty() && v != "host")
            && dirty
                .keys()
                .any(|key| !matches!(key, settings::Key::Appearance | settings::Key::Nickname));
        if !self.preferences.needs_confirmation(discard, onboard) {
            editor.update(cx, |editor, cx| {
                if discard {
                    editor.discard(window, cx)
                } else {
                    editor.apply(window, cx)
                }
            });
            return;
        }
        let summary = settings::specs(&current.baseline)
            .into_iter()
            .filter_map(|spec| {
                dirty.get(&spec.key).map(|value| {
                    let value = spec
                        .choices
                        .iter()
                        .find(|(v, _)| v == value)
                        .map(|(_, label)| label.as_str())
                        .unwrap_or(value);
                    format!(
                        "{}: {}",
                        spec.label,
                        if value.is_empty() { "Default" } else { value }
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        let description = format!(
            "{} {}?\n\n{}",
            if discard {
                "Discard pending changes for"
            } else {
                "Apply these changes to"
            },
            device_label(&current.baseline.device),
            summary
        );
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let editor = editor.clone();
            let values = values.clone();
            dialog
                .title(if discard {
                    "Discard changes?"
                } else {
                    "Apply changes?"
                })
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .show_cancel(true)
                        .cancel_text("Cancel")
                        .ok_text(if discard { "Discard" } else { "Apply" }),
                )
                .on_ok(move |_, window, cx| {
                    if editor.read(cx).values(cx) == values {
                        editor.update(cx, |editor, cx| {
                            if discard {
                                editor.discard(window, cx)
                            } else {
                                editor.apply(window, cx)
                            }
                        });
                    }
                    true
                })
        });
    }
    fn set_tray(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.service_busy {
            return;
        }
        self.service_busy = true;
        self.tray_enabled = None;
        let task = cx.background_executor().spawn(async move {
            let result = services::set_tray(enabled);
            (result, services::tray_status())
        });
        cx.spawn(async move |view: gpui_kit::WeakEntity<Self>, cx| {
            let (result, status) = task.await;
            view.update(cx, |view, cx| {
                view.service_busy = false;
                view.tray_enabled = status.as_ref().ok().copied();
                view.preference_error = result
                    .and(status.map(|_| ()))
                    .err()
                    .map(|error| format!("{error:#}"));
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach();
        cx.notify();
    }
    /// The page. Laid out like a device page — same rail, same header shape —
    /// because it is one more page in the same window, not a dialog.
    pub(crate) fn render_preferences(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // A switch whose state the app owns: it is saved the moment it moves.
        let preference = |id: &'static str,
                          label: &'static str,
                          help: &'static str,
                          checked: bool,
                          key: Preference| {
            ui::setting_row()
                .child(ui::row_copy(label, help, false))
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Switch::new(id)
                                .checked(checked)
                                .disabled(!self.preferences_loaded)
                                .on_click(cx.listener(move |view, checked, _, cx| {
                                    view.set_preference(key, *checked, cx)
                                })),
                        )
                        .child(ui::switch_state(checked)),
                )
                .into_any_element()
        };
        // A switch standing for something outside the app — a login item, a
        // running process. Its state has to be read before it can be shown, so
        // the switch stays in place and says why it cannot be moved yet.
        let service = |id: &'static str,
                       label: &'static str,
                       help: &'static str,
                       tag: Option<Div>,
                       state: Option<bool>,
                       on_click: SwitchAction| {
            ui::setting_row()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(theme::text::BODY)
                                .font_semibold()
                                .text_color(rgb(TEXT))
                                .child(label)
                                .children(tag),
                        )
                        .child(
                            div()
                                .max_w(px(420.0))
                                .text_size(theme::text::TINY)
                                .text_color(rgb(MUTED_2))
                                .child(help),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_2()
                        .when(state.is_none(), |el| {
                            el.child(ui::chip(if self.service_busy {
                                "Checking\u{2026}"
                            } else {
                                "Unavailable"
                            }))
                        })
                        .child(
                            Switch::new(id)
                                .checked(state.unwrap_or(false))
                                .disabled(self.service_busy || state.is_none())
                                .on_click(on_click),
                        )
                        // No word while the state is unknown: the chip beside it
                        // is already saying that, and "Off" would be a guess.
                        .children(state.map(ui::switch_state)),
                )
                .into_any_element()
        };

        let agent_busy = self.service_busy
            || self.loading
            || self.working(cx)
            || self
                .editors
                .values()
                .any(|e| !e.read(cx).dirty(cx).is_empty());
        let (agent_state, agent_tone) = match self.agent_online {
            Some(true) => ("Connected", SUCCESS),
            Some(false) => ("Disconnected", DANGER),
            None => ("Checking\u{2026}", MUTED_2),
        };

        div().size_full().overflow_y_scrollbar().child(
            div()
                .w_full()
                .max_w(px(1120.0))
                .px(px(28.0))
                .pt(px(32.0))
                .pb(px(56.0))
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(7.0))
                        .mb(px(10.0))
                        .child(ui::eyebrow("THIS APP"))
                        .child(
                            div()
                                .text_size(theme::text::DISPLAY)
                                .font_semibold()
                                .text_color(rgb(TEXT))
                                .child("App preferences"),
                        )
                        .child(
                            div()
                                .max_w(px(560.0))
                                .text_size(theme::text::BODY)
                                .text_color(rgb(MUTED))
                                .child(
                                    "These stay on this computer. Every switch saves the \
                                         moment you move it.",
                                ),
                        ),
                )
                .when_some(self.preference_error.clone(), |el, message| {
                    el.child(ui::notice(IconName::TriangleAlert, DANGER, message))
                })
                .child(
                    ui::card()
                        .child(ui::card_header(
                            IconName::Settings,
                            "Startup",
                            "What GFlick runs when you sign in.",
                        ))
                        .child(ui::card_rows(vec![
                            service(
                                "launch-at-sign-in",
                                "Launch at sign in",
                                "Starts the agent with your session, so your saved settings \
                                     are on the mouse before you reach for it.",
                                Some(ui::chip_accent("RECOMMENDED")),
                                self.startup_enabled,
                                Box::new(cx.listener(|view, checked, _, cx| {
                                    view.set_startup(*checked, cx)
                                })),
                            ),
                            service(
                                "show-tray-icon",
                                "Show the tray icon",
                                "A menu bar readout of battery and connection, and the quick \
                                     switches, without opening this window.",
                                Some(ui::chip("OPTIONAL")),
                                self.tray_enabled,
                                Box::new(
                                    cx.listener(|view, checked, _, cx| view.set_tray(*checked, cx)),
                                ),
                            ),
                        ])),
                )
                .when(self.startup_enabled == Some(false), |el| {
                    el.child(ui::notice(
                        IconName::TriangleAlert,
                        theme::WARNING,
                        "With this off, nothing GFlick holds for your mouse is written after a \
                         restart — you have to open this app for your settings to reach it. The \
                         agent idles between changes, so leaving it on is the same as not \
                         running it as far as your machine is concerned.",
                    ))
                })
                .child(
                    ui::card()
                        .child(ui::card_header(
                            IconName::ListChecks,
                            "Confirmations",
                            "When to ask before a change reaches the mouse.",
                        ))
                        .child(ui::card_rows(vec![
                            preference(
                                "confirm-apply",
                                "Confirm Apply",
                                "Review the pending changes before they are written.",
                                self.preferences.confirm_apply,
                                Preference::Apply,
                            ),
                            preference(
                                "confirm-discard",
                                "Confirm Discard",
                                "Ask before pending changes are thrown away.",
                                self.preferences.confirm_discard,
                                Preference::Discard,
                            ),
                            preference(
                                "confirm-profile",
                                "Confirm profile writes",
                                "Ask when a change would be written while an onboard \
                                     profile is in control.",
                                self.preferences.confirm_profile_writes,
                                Preference::Profiles,
                            ),
                        ])),
                )
                .child(
                    ui::card()
                        .child(ui::card_header(
                            IconName::Plug,
                            "Agent",
                            "The service this window talks to.",
                        ))
                        .child(ui::card_rows(vec![
                            ui::setting_row()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .text_size(theme::text::BODY)
                                                .font_semibold()
                                                .text_color(rgb(TEXT))
                                                .child(ui::dot(agent_tone))
                                                .child("GFlick Agent"),
                                        )
                                        .child(
                                            div()
                                                .text_size(theme::text::TINY)
                                                .text_color(rgb(MUTED_2))
                                                .child(format!(
                                                    "{agent_state} \u{b7} Local socket \
                                                         \u{b7} Protocol v{}",
                                                    gflick_protocol::PROTOCOL_VERSION
                                                )),
                                        ),
                                )
                                .child(
                                    Button::new("restart-agent")
                                        .icon(IconName::RotateCw)
                                        .accessibility_label("Restart the agent")
                                        .child(ui::button_label(
                                            if self.service_busy {
                                                "Working\u{2026}"
                                            } else {
                                                "Restart"
                                            },
                                            theme::text::BODY,
                                        ))
                                        .disabled(agent_busy)
                                        .on_click(
                                            cx.listener(|view, _, _, cx| view.restart_agent(cx)),
                                        ),
                                )
                                .into_any_element(),
                        ])),
                ),
        )
    }
}
