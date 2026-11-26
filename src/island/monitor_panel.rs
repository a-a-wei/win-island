use super::{ACCENT, BORDER, CARD, Island, MUTED, action, empty};
use crate::monitor::Sample;
use gpui_kit::{prelude::FluentBuilder, *};
use std::collections::VecDeque;

const BLUE: u32 = 0x59afff;
const GREEN: u32 = 0x52db80;
const AMBER: u32 = 0xf5b44c;

#[derive(Default)]
pub(super) struct MonitorPanel {
    sample: Option<Sample>,
    history: VecDeque<(f32, u64, u64)>,
    by_memory: bool,
}

impl MonitorPanel {
    pub(super) fn push(&mut self, sample: Sample) {
        self.history
            .push_back((sample.cpu, sample.download, sample.upload));
        if self.history.len() > 24 {
            self.history.pop_front();
        }
        self.sample = Some(sample);
    }
}

impl Island {
    pub(super) fn monitor_tab(&self, cx: &mut Context<Self>) -> Div {
        let Some(sample) = &self.monitor.sample else {
            return empty(
                "正在采集系统状态",
                "CPU、显卡、内存、网络和进程会在这里更新。",
            );
        };
        let gigabytes = 1024.0_f64.powi(3);
        let mem_percent = if sample.total_memory == 0 {
            0.0
        } else {
            100.0 * sample.memory as f32 / sample.total_memory as f32
        };
        let max_down = self
            .monitor
            .history
            .iter()
            .map(|entry| entry.1)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let max_up = self
            .monitor
            .history
            .iter()
            .map(|entry| entry.2)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let mut processes: Vec<_> = sample.processes.iter().collect();
        processes.sort_by(|a, b| {
            if self.monitor.by_memory {
                b.memory.cmp(&a.memory)
            } else {
                b.cpu.total_cmp(&a.cpu)
            }
            .then_with(|| a.name.cmp(&b.name))
        });
        div()
            .flex()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        metric_panel()
                            .child(metric_title("CPU", format!("{:.0}%", sample.cpu), BLUE))
                            .child(history_bars(
                                self.monitor.history.iter().map(|entry| entry.0),
                                BLUE,
                                31.0,
                            )),
                    )
                    .child(
                        metric_panel()
                            .child(metric_title(
                                "显卡",
                                sample.gpu.map_or_else(
                                    || "暂无数据".into(),
                                    |gpu| format!("{:.0}%", gpu.usage),
                                ),
                                GREEN,
                            ))
                            .when_some(sample.gpu, |panel, gpu| {
                                panel.child(
                                    div().flex().gap_2().children(
                                        ["3D", "复制", "解码", "编码"]
                                            .into_iter()
                                            .zip(gpu.engines)
                                            .map(|(label, percent)| {
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(progress_bar(percent, GREEN))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_center()
                                                            .text_color(rgb(MUTED))
                                                            .child(label),
                                                    )
                                            }),
                                    ),
                                )
                            }),
                    )
                    .child(
                        metric_panel()
                            .child(metric_title(
                                "内存",
                                format!(
                                    "{:.1} / {:.1} GB",
                                    sample.memory as f64 / gigabytes,
                                    sample.total_memory as f64 / gigabytes
                                ),
                                BLUE,
                            ))
                            .child(progress_bar(mem_percent, BLUE)),
                    )
                    .child(
                        metric_panel().child(
                            div()
                                .flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(metric_title(
                                            "↓ 下载",
                                            super::wifi::format_rate(sample.download),
                                            BLUE,
                                        ))
                                        .child(history_bars(
                                            self.monitor
                                                .history
                                                .iter()
                                                .map(|entry| 100.0 * entry.1 as f32 / max_down),
                                            BLUE,
                                            18.0,
                                        )),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(metric_title(
                                            "↑ 上传",
                                            super::wifi::format_rate(sample.upload),
                                            AMBER,
                                        ))
                                        .child(history_bars(
                                            self.monitor
                                                .history
                                                .iter()
                                                .map(|entry| 100.0 * entry.2 as f32 / max_up),
                                            AMBER,
                                            18.0,
                                        )),
                                ),
                        ),
                    ),
            )
            .child(
                div()
                    .w(px(234.0))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(CARD))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("进程"),
                            )
                            .child(div().flex().gap_1().children(
                                [("CPU", false), ("内存", true)].into_iter().map(
                                    |(label, memory)| {
                                        action(("process-sort", usize::from(memory)), label, false)
                                            .when(self.monitor.by_memory == memory, |button| {
                                                button.text_color(rgb(ACCENT))
                                            })
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.monitor.by_memory = memory;
                                                cx.notify();
                                            }))
                                    },
                                ),
                            )),
                    )
                    .children(processes.into_iter().take(7).map(|process| {
                        let abbreviation = process
                            .name
                            .chars()
                            .next()
                            .map(|value| value.to_uppercase().to_string())
                            .unwrap_or_default();
                        let value = if self.monitor.by_memory {
                            format!("{:.0} MB", process.memory as f64 / 1048576.0)
                        } else {
                            format!("{:.1}%", process.cpu)
                        };
                        let name = process.name.strip_suffix(".exe").unwrap_or(&process.name);
                        let label = if process.instances > 1 {
                            format!("{name} ×{}", process.instances)
                        } else {
                            name.to_owned()
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(px(24.0))
                                    .flex_shrink_0()
                                    .rounded_md()
                                    .bg(rgb(BORDER))
                                    .text_xs()
                                    .text_center()
                                    .child(abbreviation),
                            )
                            .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
                            .child(div().flex_shrink_0().text_xs().child(value))
                    })),
            )
    }
}

fn metric_panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(rgb(CARD))
}

fn metric_title(label: &'static str, value: String, color: u32) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_1()
        .child(div().text_xs().text_color(rgb(MUTED)).child(label))
        .child(
            div()
                .text_xs()
                .text_color(rgb(color))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
}

fn progress_bar(percent: f32, color: u32) -> Div {
    div()
        .w_full()
        .h(px(5.0))
        .rounded_full()
        .bg(rgb(BORDER))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(percent.clamp(0.0, 100.0) / 100.0))
                .bg(rgb(color)),
        )
}

fn history_bars(values: impl Iterator<Item = f32>, color: u32, height: f32) -> Div {
    div()
        .flex()
        .items_end()
        .gap_0p5()
        .h(px(height))
        .children(values.map(move |value| {
            div()
                .flex_1()
                .h(px((height * value.clamp(0.0, 100.0) / 100.0).max(1.0)))
                .rounded_sm()
                .bg(rgb(color))
        }))
}
