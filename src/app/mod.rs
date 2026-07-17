//! Windows 平台层：命令分派 + 托盘 + 设置窗口 + 开机自启。

pub mod login;
pub mod store;
pub mod widget;

use std::process::{Child, Command};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIconBuilder, TrayIconEvent,
};

/// 入口：按命令行参数分派到不同角色（同一个 exe）。
pub fn run() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "--widget" => widget::run(),
        "--login" => login::run(),
        "--settings" | "--setkey" => widget::run_settings(),
        _ => tray_main(),
    }
}

/// 打开设置窗口（独立进程，可从托盘或小组件调用）。
pub fn open_settings() {
    let _ = spawn("--settings");
}

fn exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| "DeepSeekWidget.exe".into())
}

fn spawn(arg: &str) -> Option<Child> {
    Command::new(exe()).arg(arg).spawn().ok()
}

// ───────── 托盘 ─────────

fn tray_main() {
    // 单实例：用一个命名互斥体。失败则退出。
    if already_running() {
        return;
    }

    let event_loop = EventLoopBuilder::new().build();

    let menu = Menu::new();
    let mi_widget = MenuItem::new("显示小组件", true, None);
    let login_label = if store::has_session() {
        "重新登录 DeepSeek 平台…"
    } else {
        "登录 DeepSeek 平台（同步用量）…"
    };
    let mi_login = MenuItem::new(login_label, true, None);
    let mi_key = MenuItem::new("设置…", true, None);
    let mi_autostart = CheckMenuItem::new("开机自启", true, autostart_enabled(), None);
    let mi_folder = MenuItem::new("打开数据目录", true, None);
    let mi_quit = MenuItem::new("退出", true, None);

    menu.append_items(&[
        &mi_widget,
        &mi_login,
        &mi_key,
        &PredefinedMenuItem::separator(),
        &mi_autostart,
        &mi_folder,
        &PredefinedMenuItem::separator(),
        &mi_quit,
    ])
    .ok();

    let icon = make_icon();
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("DeepSeek 小部件")
        .with_icon(icon)
        .build()
        .expect("tray");

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayIconEvent::receiver();

    let mut widget_child: Option<Child> = None;
    let mut login_child: Option<Child> = None;
    let mut widget_shown = false; // 菜单文字当前对应的状态

    let id_widget = mi_widget.id().clone();
    let id_login = mi_login.id().clone();
    let id_key = mi_key.id().clone();
    let id_autostart = mi_autostart.id().clone();
    let id_folder = mi_folder.id().clone();
    let id_quit = mi_quit.id().clone();

    event_loop.run(move |_event, _, control_flow| {
        // 定时醒来轮询：小组件可能被用户从窗口上直接关闭
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(500),
        );

        // 菜单文字跟随小组件进程存活状态
        let alive = widget_child
            .as_mut()
            .map_or(false, |c| c.try_wait().ok().flatten().is_none());
        if alive != widget_shown {
            widget_shown = alive;
            mi_widget.set_text(if alive { "关闭小组件" } else { "显示小组件" });
        }

        // 双击托盘图标 = 显示/关闭小组件
        if let Ok(ev) = tray_channel.try_recv() {
            if let TrayIconEvent::DoubleClick { .. } = ev {
                toggle_widget(&mut widget_child);
            }
        }

        if let Ok(ev) = menu_channel.try_recv() {
            let id = ev.id;
            if id == id_widget {
                toggle_widget(&mut widget_child);
            } else if id == id_login {
                if login_child.as_mut().map_or(true, |c| c.try_wait().ok().flatten().is_some()) {
                    login_child = spawn("--login");
                }
            } else if id == id_key {
                spawn("--settings");
            } else if id == id_autostart {
                toggle_autostart();
            } else if id == id_folder {
                let _ = Command::new("explorer").arg(store::data_dir()).spawn();
            } else if id == id_quit {
                if let Some(mut c) = widget_child.take() {
                    let _ = c.kill();
                }
                if let Some(mut c) = login_child.take() {
                    let _ = c.kill();
                }
                *control_flow = ControlFlow::Exit;
            }
        }
    });
}

fn toggle_widget(child: &mut Option<Child>) {
    let alive = child
        .as_mut()
        .map_or(false, |c| c.try_wait().ok().flatten().is_none());
    if alive {
        if let Some(mut c) = child.take() {
            let _ = c.kill();
        }
    } else {
        *child = spawn("--widget");
    }
}

/// 托盘图标：编译期嵌入的 32x32 RGBA（DeepSeek 娘头像徽章，见 assets/）。
pub const ICON_32: &[u8] = include_bytes!("../../assets/icon_32.rgba");
pub const ICON_64: &[u8] = include_bytes!("../../assets/icon_64.rgba");

fn make_icon() -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(ICON_32.to_vec(), 32, 32).expect("icon")
}

// ───────── 单实例（命名互斥体） ─────────

fn already_running() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Global\\DeepSeekWidgetSingleton\0".encode_utf16().collect();
    unsafe {
        let h = CreateMutexW(None, false, PCWSTR(name.as_ptr()));
        if h.is_err() {
            return false;
        }
        // 泄漏句柄，让互斥体存活到进程结束
        std::mem::forget(h);
        GetLastError() == ERROR_ALREADY_EXISTS
    }
}

// ───────── 开机自启（注册表 Run） ─────────

fn autostart_cmd() -> String {
    format!("\"{}\"", exe().display())
}

fn autostart_enabled() -> bool {
    winreg_read().is_some()
}

fn toggle_autostart() {
    if autostart_enabled() {
        winreg_delete();
    } else {
        winreg_write(&autostart_cmd());
    }
}

// 用 reg.exe 操作注册表，避免额外 winreg 依赖
fn winreg_read() -> Option<()> {
    let out = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            store::APP_NAME,
        ])
        .output()
        .ok()?;
    if out.status.success() {
        Some(())
    } else {
        None
    }
}

fn winreg_write(cmd: &str) {
    let _ = Command::new("reg")
        .args([
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            store::APP_NAME,
            "/t",
            "REG_SZ",
            "/d",
            cmd,
            "/f",
        ])
        .status();
}

fn winreg_delete() {
    let _ = Command::new("reg")
        .args([
            "delete",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            store::APP_NAME,
            "/f",
        ])
        .status();
}
