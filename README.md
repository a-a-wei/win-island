# 灵动岛 · Win Island

Windows 10 x64 悬浮面板，使用 Rust 和 GPUI Kit。窗口始终贴紧主屏幕顶部，圆角外侧透明。媒体、Wi‑Fi 和蓝牙各自可以选择「一行简略」「多行详细」或「隐藏」。一行样式的摘要靠右并优先占用所需宽度，名称靠左，超长文字以省略号收尾。多行样式沿用 v0.6.0 的状态圆点和三行卡片。有通知时优先显示通知，通知可以单独选择详情、仅条数或隐藏。当前标签页没有可预览的内容时，收成一条小黑条。鼠标移入后展开，离开约 100 毫秒后开始收起。可点击的通知预览会留出约 300 毫秒供点击。展开后可在右上角退出。

## 功能

| 面板 | 操作 |
| --- | --- |
| 通知 | 监听 Windows 通知中心保留的系统和应用通知。点击通知列表中的卡片会打开来源应用，并在启动成功后移除该条通知。Windows 通知读取接口只提供来源应用和显示内容，通知原本的跳转参数由系统传给来源应用。「全部删除」逐条清空当前列表，并检查 Windows 通知中心是否仍有残留。通知内容只保留在内存里。 |
| 媒体 | 控制 Windows 当前媒体会话的播放、暂停、上一首和下一首。播放和暂停时都可预览标题和状态，多行样式也显示艺术家。可在「系统音量」上滚动鼠标滚轮，或通过滑条、加减按钮和静音键调节默认音频输出。 |
| Wi‑Fi | 查看附近网络和信号强度，扫描、连接已保存或开放的网络，或断开当前连接。每秒按网卡的实际收发量更新上传和下载速率，收起后也可查看。首次加入需要凭据的网络时，在系统 Wi‑Fi 设置中完成。 |
| 蓝牙 | 支持的经典蓝牙耳机和音箱可通过 Windows 音频端点直接连接或断开。支持 GATT 的低功耗设备也可在岛内连接，并可释放灵动岛建立的会话。已连接设备会显示可读取的电量，预览选择已知电量最低的设备。 |
| 剪贴板 | 查看本次运行录入的内容，每条记录显示文本、文件或图片类型。图片数据直接预览，PNG、JPEG、WebP、GIF、BMP、TIFF 和 ICO 文件显示缩略图及文件名，普通文件显示文件名。可重新复制、暂停记录和清空历史。可在设置中调整最大条数。 |
| 文件中转 | 将文件或目录拖到灵动岛上，自动展开中转站并暂存原路径。拖出时按 Ctrl 复制，按 Shift 移动。移动成功后清除中转条目，复制后仍可继续使用。也可用「复制全部」在资源管理器中粘贴。 |
| 快捷启动 | 添加应用程序或快捷方式，点击「启动」打开。清单会保存到下次启动。 |
| 系统监控 | 每秒刷新 CPU、GPU、内存、上下行速率和进程排行。进程可按 CPU 或内存排序。GPU 数据取决于 Windows 显卡性能计数器是否可用。 |
| 设置 | 可配置标签页显示、剪贴历史最大条数、登录后自动启动、悬停延时、收放动画和收起预览。在标签栏上滚动鼠标滚轮可循环切换可见标签页。 |

## 构建和运行

需要 Windows 10 2004（内部版本 19041）或更高版本、x64 Rust MSVC 工具链、Visual Studio 的 C++ 桌面开发组件、Windows SDK 和 CMake。普通 PowerShell 中执行以下命令。首次运行 Cargo 会下载项目指定的 Rust 版本及依赖。

```powershell
cargo run --locked
```

直接运行即可开发和验证悬浮窗、媒体、无线和通知功能。

## 启用便携版通知

打开 `WinIsland-Portable.exe`，展开「通知」页即可查看已授权的通知。如果 Windows 需要授权，请点击「配置通知权限」。如果权限已关闭，可通过「通知隐私设置」恢复授权。

如需随 Windows 登录启动，展开悬浮岛，进入「设置」，将「登录后自动启动」设为开启。移动 EXE 后可以在同一位置更新启动路径。

## 构建便携发布版

```powershell
cargo build --locked --release --target x86_64-pc-windows-msvc
```

将编译生成的 `target\x86_64-pc-windows-msvc\release\win-island.exe` 复制为 `dist\WinIsland-Portable.exe` 即可发布。

## 实现参考

- [GPUI Kit 安装要求](https://gpui-kit.com/docs/installation/)
- [Windows 通知侦听器](https://learn.microsoft.com/en-us/windows/uwp/design/shell/tiles-and-notifications/notification-listener)
- [Windows 媒体会话](https://learn.microsoft.com/en-us/uwp/api/windows.media.control.globalsystemmediatransportcontrolssessionmanager)
- [Windows Native Wi‑Fi](https://learn.microsoft.com/en-us/windows/win32/nativewifi/native-wifi-functions)
- [Windows 蓝牙 GATT 客户端](https://learn.microsoft.com/en-us/windows/uwp/devices-sensors/gatt-client)
- [Windows 蓝牙音频连接控制](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/kspropsetid-btaudio)
