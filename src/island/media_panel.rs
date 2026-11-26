use super::{
    ACCENT, BORDER, CARD, INK, Island, MUTED, Tab, action, detailed_preview, empty,
    single_line_preview, status_line,
};
use crate::{
    media::{self, PlaybackAction, VolumeAction},
    preferences::PreviewMode,
};
use gpui_kit::{prelude::FluentBuilder, *};

#[derive(Default)]
pub(super) struct MediaPanel {
    manager: Option<media::SessionManager>,
    playback: Option<media::Playback>,
    playback_error: Option<String>,
    playback_busy: bool,
    loading: bool,
    volume: Option<media::Volume>,
    volume_error: Option<String>,
    pub(super) dragging: bool,
}

impl Island {
    pub(super) fn preview_media(&self) -> Option<(&media::Playback, &'static str)> {
        let player = self.media.playback.as_ref()?;
        Some((player, player.preview_status()?))
    }

    pub(super) fn media_preview(&self) -> Option<Div> {
        let (player, state) = self.preview_media()?;
        match self.preferences.media_preview {
            PreviewMode::SingleLine => Some(single_line_preview(player.title.clone(), state)),
            PreviewMode::MultiLine => {
                let detail = if player.artist.is_empty() {
                    state.to_owned()
                } else {
                    format!("{state} · {}", player.artist)
                };
                Some(detailed_preview("媒体", player.title.clone(), detail))
            }
            PreviewMode::Hidden => None,
        }
    }

    pub(super) fn refresh_media(&mut self, cx: &mut Context<Self>) {
        self.apply_volume(media::volume(), cx);
        if self.media.loading || self.media.playback_busy {
            return;
        }
        self.media.loading = true;
        let manager = self.media.manager.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                let manager = match manager {
                    Some(manager) => manager,
                    None => media::connect().await?,
                };
                let playback = media::playback(&manager).await?;
                Ok::<_, anyhow::Error>((manager, playback))
            }
            .await;
            let _ = this.update(cx, |view, cx| {
                view.media.loading = false;
                match result {
                    Ok((manager, playback)) => {
                        view.media.manager = Some(manager);
                        if view.media.playback.is_none() || playback.is_none() {
                            view.media.playback_error = None;
                        }
                        view.media.playback = playback;
                    }
                    Err(error) => {
                        view.media.manager = None;
                        view.media.playback = None;
                        view.media.playback_error = Some(format!("无法读取媒体会话 · {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_volume(&mut self, result: anyhow::Result<media::Volume>, cx: &mut Context<Self>) {
        let (volume, error) = match result {
            Ok(volume) => (Some(volume), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        if self.media.volume != volume || self.media.volume_error != error {
            self.media.volume = volume;
            self.media.volume_error = error;
            cx.notify();
        }
    }

    fn change_volume(&mut self, action: VolumeAction, cx: &mut Context<Self>) {
        self.apply_volume(media::change_volume(action), cx);
    }

    fn control_playback(&mut self, action: PlaybackAction, cx: &mut Context<Self>) {
        let Some(playback) = self.media.playback.as_ref() else {
            return;
        };
        if self.media.playback_busy || !playback.supports(action) {
            return;
        }
        self.media.playback_busy = true;
        self.media.playback_error = None;
        let session = playback.session();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = media::Playback::control(session, action).await;
            let _ = this.update(cx, |view, cx| {
                view.media.playback_busy = false;
                view.media.playback_error = result.err().map(|error| error.to_string());
                view.refresh_media(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn media_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let playback = if let Some(player) = &self.media.playback {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .bg(rgb(CARD))
                .p_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(ACCENT))
                        .child(if player.playing {
                            "正在播放"
                        } else {
                            "当前媒体"
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(player.title.clone()),
                )
                .when(!player.artist.is_empty(), |card| {
                    card.child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .truncate()
                            .child(player.artist.clone()),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .pt_1()
                        .children(
                            [
                                ("previous", "上一首", PlaybackAction::Previous),
                                (
                                    "toggle",
                                    if player.playing { "暂停" } else { "播放" },
                                    player.primary_action(),
                                ),
                                ("next", "下一首", PlaybackAction::Next),
                            ]
                            .into_iter()
                            .map(|(id, label, control)| {
                                let disabled =
                                    self.media.playback_busy || !player.supports(control);
                                action(id, label, disabled).when(!disabled, |button| {
                                    button.on_click(cx.listener(move |view, _, _, cx| {
                                        view.control_playback(control, cx)
                                    }))
                                })
                            }),
                        ),
                )
        } else if let Some(error) = &self.media.playback_error {
            empty("媒体控制暂不可用", error.clone())
        } else if self.media.loading && self.media.manager.is_none() {
            empty("正在读取媒体", "正在连接 Windows 媒体会话。")
        } else {
            empty(
                "暂无可控媒体",
                "在支持 Windows 媒体控制的应用中播放音频或视频。",
            )
        };

        let volume = if let Some(volume) = self.media.volume {
            div()
                .on_scroll(cx.listener(|view, event: &ScrollEvent, _, cx| {
                    let delta = event.delta.pixel_delta(px(1.0)).y;
                    if delta != px(0.0) {
                        cx.stop_propagation();
                        view.change_volume(
                            VolumeAction::Step(if delta > px(0.0) { 5 } else { -5 }),
                            cx,
                        );
                    }
                }))
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .bg(rgb(CARD))
                .p_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("系统音量"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(ACCENT))
                                .child(format!("{}%", volume.percent)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(
                            action("lower", "-5", volume.percent == 0).on_click(cx.listener(
                                |view, _, _, cx| view.change_volume(VolumeAction::Step(-5), cx),
                            )),
                        )
                        .child(self.volume_track(volume, cx))
                        .child(
                            action("raise", "+5", volume.percent == 100).on_click(cx.listener(
                                |view, _, _, cx| view.change_volume(VolumeAction::Step(5), cx),
                            )),
                        )
                        .child(
                            action(
                                "mute",
                                if volume.muted {
                                    "取消静音"
                                } else {
                                    "静音"
                                },
                                false,
                            )
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.change_volume(VolumeAction::ToggleMute, cx)
                            })),
                        ),
                )
        } else {
            empty(
                "系统音量",
                self.media
                    .volume_error
                    .as_deref()
                    .unwrap_or("正在读取默认输出设备…")
                    .to_owned(),
            )
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(playback)
            .when(self.media.playback.is_some(), |panel| {
                panel.children(
                    self.media
                        .playback_error
                        .iter()
                        .map(|error| status_line(error.clone(), true)),
                )
            })
            .child(volume)
    }

    fn volume_track(&self, volume: media::Volume, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let width = (bounds.size.width - px(12.0)).max(px(1.0));
                let left = bounds.left() + px(6.0);
                let center = bounds.center().y;
                let filled = width * (f32::from(volume.percent) / 100.0);
                let rail = Bounds::new(point(left, center - px(2.0)), size(width, px(4.0)));
                window.paint_quad(fill(rail, rgb(BORDER)));
                let tone = rgb(if volume.muted { MUTED } else { ACCENT });
                if filled > px(0.0) {
                    window.paint_quad(fill(Bounds::new(rail.origin, size(filled, px(4.0))), tone));
                }
                let knob = Bounds::new(
                    point(left + filled - px(5.0), center - px(5.0)),
                    size(px(10.0), px(10.0)),
                );
                let mut knob = fill(knob, rgb(INK));
                knob.corner_radii = Corners::all(px(5.0));
                window.paint_quad(knob);

                let percent = move |x: Pixels| {
                    (((x - left).as_f32() / width.as_f32()).clamp(0.0, 1.0) * 100.0).round() as u8
                };
                let pressed = view.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble
                        && event.button == MouseButton::Left
                        && bounds.contains(&event.position)
                    {
                        let _ = pressed.update(cx, |view, cx| {
                            view.media.dragging = true;
                            view.change_volume(VolumeAction::Level(percent(event.position.x)), cx);
                        });
                    }
                });
                let moved = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = moved.update(cx, |view, cx| {
                            if event.pressed_button != Some(MouseButton::Left) {
                                view.media.dragging = false;
                            }
                            if view.media.dragging && view.frame.expanded && view.tab == Tab::Media
                            {
                                let level = percent(event.position.x);
                                if view
                                    .media
                                    .volume
                                    .is_some_and(|volume| volume.percent != level)
                                {
                                    view.change_volume(VolumeAction::Level(level), cx);
                                }
                            }
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        let _ = view.update(cx, |view, _| view.media.dragging = false);
                    }
                });
            },
        )
        .flex_1()
        .h(px(24.0))
    }
}
