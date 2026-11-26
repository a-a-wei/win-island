use super::{ACCENT, CARD, Island, Tab, action, status_line};
use crate::preferences::{self, Preferences, Preview, PreviewMode};
use crate::startup;
use gpui_kit::{prelude::FluentBuilder, *};

const TAB_PREVIEW_MODES: &[(&str, PreviewMode)] = &[
    ("一行简略", PreviewMode::SingleLine),
    ("多行详细", PreviewMode::MultiLine),
    ("隐藏", PreviewMode::Hidden),
];

impl Island {
    fn save_preferences(&mut self, next: Preferences, cx: &mut Context<Self>) {
        match preferences::save(next) {
            Ok(()) => {
                if self.preferences.visible_tabs != next.visible_tabs {
                    self.tab_slide = None;
                }
                self.preferences = next;
                self.clipboard.items.truncate(next.clipboard_max_items);
                self.preferences_error = None;
                if !self.tab_visible(self.tab) {
                    let first = self.visible_tabs().next();
                    if let Some(tab) = first {
                        self.select_tab(tab, cx);
                    }
                }
            }
            Err(error) => self.preferences_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    pub(super) fn preferences_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(CARD))
                    .child(div().text_sm().child("登录后自动启动"))
                    .child(
                        div().flex().items_center().gap_1().children(
                            [
                                ("开启", true, startup::State::Current),
                                ("关闭", false, startup::State::Disabled),
                            ]
                            .into_iter()
                            .map(|(label, enabled, state)| {
                                action(("startup", usize::from(enabled)), label, false)
                                    .when(
                                        self.startup_state.as_ref().ok() == Some(&state),
                                        |button| {
                                            button.border_color(rgb(ACCENT)).text_color(rgb(ACCENT))
                                        },
                                    )
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.startup_state = startup::set_enabled(enabled)
                                            .map_err(|error| format!("{error:#}"));
                                        cx.notify();
                                    }))
                            }),
                        ),
                    ),
            )
            .when(
                self.startup_state.as_ref().ok() == Some(&startup::State::AnotherLocation),
                |panel| {
                    panel.child(status_line(
                        "自动启动仍指向旧位置，点击开启可更新为当前便携版。".into(),
                        false,
                    ))
                },
            )
            .when(self.startup_state.is_err(), |panel| {
                panel.children(
                    self.startup_state
                        .as_ref()
                        .err()
                        .map(|error| status_line(error.clone(), true)),
                )
            })
            .child(preference_row(
                "hover-delay",
                "悬停展开",
                self.preferences.hover_delay_ms,
                &[("立即", 0), ("80 ms", 80), ("150 ms", 150), ("250 ms", 250)],
                |choices, value| choices.hover_delay_ms = value,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(CARD))
                    .child(div().text_sm().child("显示标签页"))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(Tab::ALL.into_iter().map(|tab| {
                                action(("tab-visible", tab as usize), tab.name(), false)
                                    .when(self.tab_visible(tab), |button| {
                                        button.border_color(rgb(ACCENT)).text_color(rgb(ACCENT))
                                    })
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let mut next = view.preferences;
                                        next.visible_tabs ^= tab.bit();
                                        view.save_preferences(next, cx);
                                    }))
                            })),
                    ),
            )
            .child(preference_row(
                "collapse-delay",
                "离开后收起",
                self.preferences.collapse_delay_ms,
                &[
                    ("立即", 0),
                    ("100 ms", 100),
                    ("250 ms", 250),
                    ("500 ms", 500),
                ],
                |choices, value| choices.collapse_delay_ms = value,
                cx,
            ))
            .child(preference_row(
                "animate",
                "收放动画",
                self.preferences.animate,
                &[("开启", true), ("关闭", false)],
                |choices, value| choices.animate = value,
                cx,
            ))
            .child(preference_row(
                "preview",
                "收起时的通知预览",
                self.preferences.preview,
                &[
                    ("详情", Preview::Details),
                    ("仅条数", Preview::Count),
                    ("隐藏", Preview::Hidden),
                ],
                |choices, value| choices.preview = value,
                cx,
            ))
            .child(preference_row(
                "media-preview",
                "收起时的媒体预览",
                self.preferences.media_preview,
                TAB_PREVIEW_MODES,
                |choices, value| choices.media_preview = value,
                cx,
            ))
            .child(preference_row(
                "wifi-preview",
                "收起时的 Wi‑Fi 预览",
                self.preferences.wifi_preview,
                TAB_PREVIEW_MODES,
                |choices, value| choices.wifi_preview = value,
                cx,
            ))
            .child(preference_row(
                "bluetooth-preview",
                "收起时的蓝牙预览",
                self.preferences.bluetooth_preview,
                TAB_PREVIEW_MODES,
                |choices, value| choices.bluetooth_preview = value,
                cx,
            ))
            .children(
                self.preferences_error
                    .iter()
                    .map(|error| status_line(error.clone(), true)),
            )
            .child(preference_row(
                "clipboard-limit",
                "剪贴历史最大条数",
                self.preferences.clipboard_max_items,
                &[
                    ("10", 10),
                    ("30", 30),
                    ("50", 50),
                    ("100", 100),
                    ("200", 200),
                ],
                |choices, value| choices.clipboard_max_items = value,
                cx,
            ))
            .child(div().flex().justify_end().child(
                action("reset-preferences", "恢复显示默认", false).on_click(
                    cx.listener(|view, _, _, cx| view.save_preferences(Preferences::default(), cx)),
                ),
            ))
    }
}

fn preference_row<T: Copy + PartialEq + 'static>(
    id: &'static str,
    title: &'static str,
    current: T,
    choices: &[(&'static str, T)],
    set: fn(&mut Preferences, T),
    cx: &mut Context<Island>,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(rgb(CARD))
        .child(div().text_sm().child(title))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .children(
                    choices
                        .iter()
                        .copied()
                        .enumerate()
                        .map(|(index, (label, value))| {
                            action((id, index), label, false)
                                .when(current == value, |button| {
                                    button.border_color(rgb(ACCENT)).text_color(rgb(ACCENT))
                                })
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    let mut next = view.preferences;
                                    set(&mut next, value);
                                    view.save_preferences(next, cx);
                                }))
                        }),
                ),
        )
}
