use super::{ACCENT, BORDER, CARD, Island, MUTED, action, empty, status_line};
use crate::desktop_tools::{
    self, Clip, ClipboardThumbnail, DesktopLists,
    file_drag::{self, DragOutcome},
};
use gpui_kit::{prelude::FluentBuilder, *};
use std::path::{Path, PathBuf};

impl Island {
    fn store_desktop(&mut self, next: DesktopLists, cx: &mut Context<Self>) {
        match next.save() {
            Ok(()) => {
                self.desktop_lists = next;
                self.desktop_status = None;
            }
            Err(error) => self.desktop_status = Some(format!("保存失败 · {error}")),
        }
        cx.notify();
    }

    pub(super) fn add_desktop_paths(
        &mut self,
        paths: Vec<PathBuf>,
        shortcuts: bool,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.desktop_lists.clone();
        let list = if shortcuts {
            &mut next.shortcuts
        } else {
            &mut next.files
        };
        for path in paths {
            if path.is_absolute() && !list.contains(&path) {
                list.push(path);
            }
        }
        self.store_desktop(next, cx);
    }

    fn choose_desktop_paths(&mut self, shortcuts: bool, directories: bool, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: true,
            prompt: Some(
                if shortcuts {
                    "选择应用或快捷方式"
                } else {
                    "添加到文件中转站"
                }
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            let result = pick.await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(Ok(Some(paths))) => view.add_desktop_paths(paths, shortcuts, cx),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    view.desktop_status = Some(format!("无法选择项目 · {error}"));
                    cx.notify();
                }
                Err(_) => {}
            });
        })
        .detach();
    }

    fn open_desktop_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { desktop_tools::open_path(&path) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.desktop_status = result.err().map(|error| error.to_string());
                view.collapse_after_launch = view.desktop_status.is_none();
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_clip(&mut self, clip: &Clip, window: &Window, cx: &mut Context<Self>) {
        self.desktop_status = Some(match clip.copy(window, cx) {
            Ok(()) => "已复制到剪贴板".into(),
            Err(error) => format!("复制失败 · {error}"),
        });
        cx.notify();
    }

    pub(super) fn clipboard_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("本次运行的剪贴记录"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                action(
                                    "pause-clipboard",
                                    if self.clipboard.paused {
                                        "继续记录"
                                    } else {
                                        "暂停记录"
                                    },
                                    false,
                                )
                                .on_click(cx.listener(
                                    |view, _, _, cx| {
                                        view.clipboard.paused = !view.clipboard.paused;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                action(
                                    "clear-clipboard-history",
                                    "清空历史",
                                    self.clipboard.items.is_empty(),
                                )
                                .on_click(cx.listener(
                                    |view, _, _, cx| {
                                        view.clipboard.clear(cx);
                                        view.desktop_status = None;
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            )
            .children(
                self.desktop_status
                    .iter()
                    .cloned()
                    .map(|status| status_line(status, false)),
            )
            .when(self.clipboard.items.is_empty(), |panel| {
                panel.child(empty(
                    "暂无剪贴记录",
                    "复制文本、图片、文件或目录后会显示在这里。",
                ))
            })
            .children(
                self.clipboard
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let content = match item {
                            Clip::Text(text) => div()
                                .text_sm()
                                .line_clamp(3)
                                .child(text.chars().take(400).collect::<String>())
                                .into_any_element(),
                            Clip::Image(image) => img(image.clone())
                                .w(px(160.0))
                                .h(px(84.0))
                                .object_fit(ObjectFit::Contain)
                                .into_any_element(),
                            Clip::Files(paths) => div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(paths.iter().map(|path| {
                                    let thumbnail = if ClipboardThumbnail::supports(path) {
                                        window.use_asset::<ClipboardThumbnail>(path, cx).flatten()
                                    } else {
                                        None
                                    };
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .children(thumbnail.map(|image| {
                                            img(image)
                                                .w(px(80.0))
                                                .h(px(52.0))
                                                .flex_shrink_0()
                                                .object_fit(ObjectFit::Contain)
                                        }))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .text_sm()
                                                .truncate()
                                                .child(path_name(path)),
                                        )
                                }))
                                .into_any_element(),
                        };
                        let clip = item.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_lg()
                            .p_3()
                            .bg(rgb(CARD))
                            .child(
                                div()
                                    .w(px(38.0))
                                    .flex_shrink_0()
                                    .rounded_sm()
                                    .bg(rgb(BORDER))
                                    .py_0p5()
                                    .text_xs()
                                    .text_center()
                                    .text_color(rgb(ACCENT))
                                    .child(item.label()),
                            )
                            .child(div().flex_1().min_w_0().child(content))
                            .child(
                                action(("recopy", index), "复制", false).on_click(cx.listener(
                                    move |view, _, window, cx| {
                                        view.copy_clip(&clip, window, cx);
                                    },
                                )),
                            )
                    }),
            )
    }

    pub(super) fn files_tab(&self, cx: &mut Context<Self>) -> Div {
        let all_files = Clip::Files(self.desktop_lists.files.clone());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        action("pick-shelf-files", "添加文件", false).on_click(cx.listener(
                            |view, _, _, cx| {
                                view.choose_desktop_paths(false, false, cx);
                            },
                        )),
                    )
                    .child(
                        action("pick-shelf-folders", "添加目录", false).on_click(cx.listener(
                            |view, _, _, cx| {
                                view.choose_desktop_paths(false, true, cx);
                            },
                        )),
                    )
                    .child(
                        action(
                            "copy-shelf",
                            "复制全部",
                            self.desktop_lists.files.is_empty(),
                        )
                        .when(
                            !self.desktop_lists.files.is_empty(),
                            |button| {
                                button.on_click(cx.listener(move |view, _, window, cx| {
                                    view.copy_clip(&all_files, window, cx)
                                }))
                            },
                        ),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(if self.dragging_files {
                        "松开鼠标，将文件或目录放入中转站"
                    } else {
                        "拖入文件或目录。拖出时按 Ctrl 复制，按 Shift 移动。"
                    }),
            )
            .children(
                self.desktop_status
                    .iter()
                    .cloned()
                    .map(|status| status_line(status, false)),
            )
            .children(
                self.desktop_lists
                    .files
                    .iter()
                    .enumerate()
                    .map(|(index, path)| self.path_row(index, path, false, cx)),
            )
    }

    pub(super) fn launcher_tab(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().justify_end().child(
                action("add-launcher", "添加应用", false).on_click(
                    cx.listener(|view, _, _, cx| view.choose_desktop_paths(true, false, cx)),
                ),
            ))
            .children(
                self.desktop_status
                    .iter()
                    .cloned()
                    .map(|status| status_line(status, false)),
            )
            .when(self.desktop_lists.shortcuts.is_empty(), |panel| {
                panel.child(empty(
                    "暂无快捷启动项",
                    "添加应用的可执行文件或 Windows 快捷方式。",
                ))
            })
            .children(
                self.desktop_lists
                    .shortcuts
                    .iter()
                    .enumerate()
                    .map(|(index, path)| self.path_row(index, path, true, cx)),
            )
    }

    fn path_row(&self, index: usize, path: &Path, shortcut: bool, cx: &mut Context<Self>) -> Div {
        let name = path_name(path);
        let target = path.to_owned();
        let remove_target = target.clone();
        let details = div()
            .id(("desktop-path", index))
            .flex_1()
            .min_w_0()
            .child(div().text_sm().truncate().child(name.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .truncate()
                    .child(path.to_string_lossy().into_owned()),
            )
            .when(!shortcut, |row| {
                row.cursor_grab()
                    .on_drag(path.to_owned(), move |path, _, window, cx| {
                        let owner = desktop_tools::window_handle(window);
                        let handle = window.window_handle();
                        let file = path.clone();
                        cx.spawn(async move |cx| {
                            // Run the OLE drag loop outside GPUI's input callback.
                            let result = owner.and_then(|owner| file_drag::drag_file(&file, owner));
                            let _ = handle.update(cx, |view, window, cx| {
                                window.dispatch_event(
                                    PlatformInput::FileDrop(FileDropEvent::Exited),
                                    cx,
                                );
                                if let Ok(view) = view.downcast::<Island>() {
                                    view.update(cx, |view, cx| match result {
                                        Ok(DragOutcome::Moved) => {
                                            let mut next = view.desktop_lists.clone();
                                            next.files.retain(|path| path != &file);
                                            view.store_desktop(next, cx);
                                        }
                                        Ok(DragOutcome::Copied) => {
                                            view.desktop_status = Some("已复制文件".into());
                                            cx.notify();
                                        }
                                        Ok(DragOutcome::Cancelled) => {}
                                        Err(error) => {
                                            view.desktop_status =
                                                Some(format!("无法完成拖放 · {error}"));
                                            cx.notify();
                                        }
                                    });
                                }
                            });
                        })
                        .detach();
                        cx.new(|_| DragLabel(name.clone()))
                    })
            });
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
            .child(details)
            .child(
                action(
                    ("open-desktop", index),
                    if shortcut { "启动" } else { "打开" },
                    false,
                )
                .on_click(
                    cx.listener(move |view, _, _, cx| view.open_desktop_path(target.clone(), cx)),
                ),
            )
            .child(
                action(("remove-desktop", index), "移除", false).on_click(cx.listener(
                    move |view, _, _, cx| {
                        let mut next = view.desktop_lists.clone();
                        let list = if shortcut {
                            &mut next.shortcuts
                        } else {
                            &mut next.files
                        };
                        list.retain(|path| path != &remove_target);
                        view.store_desktop(next, cx);
                    },
                )),
            )
    }
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

struct DragLabel(String);

impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .bg(rgb(CARD))
            .p_2()
            .rounded_md()
            .text_sm()
            .child(self.0.clone())
    }
}
