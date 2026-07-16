//! Windows 平台层：命令分派 + 托盘 + API Key 对话框 + 开机自启。

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
        "--setkey" => setkey_dialog(),
        _ => tray_main(),
    }
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
    let mi_key = MenuItem::new("设置 API Key…", true, None);
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

    let id_widget = mi_widget.id().clone();
    let id_login = mi_login.id().clone();
    let id_key = mi_key.id().clone();
    let id_autostart = mi_autostart.id().clone();
    let id_folder = mi_folder.id().clone();
    let id_quit = mi_quit.id().clone();

    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

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
                spawn("--setkey");
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

// ───────── API Key 对话框（egui 小窗） ─────────

fn setkey_dialog() {
    use eframe::egui;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 180.0])
            .with_resizable(false)
            .with_always_on_top(),
        ..Default::default()
    };

    struct KeyApp {
        input: String,
        show: bool,
        msg: (String, egui::Color32),
        cur_masked: String,
    }

    let cfg = store::load_config();
    let cur = cfg.api_key.clone();
    let masked = if cur.len() > 12 {
        format!("{}…{}", &cur[..6], &cur[cur.len() - 4..])
    } else if cur.is_empty() {
        "（未设置）".into()
    } else {
        cur.clone()
    };

    let _ = eframe::run_native(
        "DeepSeek 设置",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(KeyApp {
                input: String::new(),
                show: false,
                msg: ("粘贴 sk- 开头的密钥后点击保存".into(), egui::Color32::GRAY),
                cur_masked: masked.clone(),
            }) as Box<dyn eframe::App>)
        }),
    );

    impl eframe::App for KeyApp {
        fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("API Key（用于查询余额，可留空）").strong());
                ui.label(egui::RichText::new(format!("当前：{}", self.cur_masked)).weak());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let edit = egui::TextEdit::singleline(&mut self.input)
                        .password(!self.show)
                        .desired_width(280.0);
                    ui.add(edit);
                    if ui.button(if self.show { "隐藏" } else { "显示" }).clicked() {
                        self.show = !self.show;
                    }
                });
                ui.add_space(6.0);
                ui.colored_label(self.msg.1, &self.msg.0);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        let key = self.input.trim().to_string();
                        if key.is_empty() {
                            self.msg = ("输入为空，未修改".into(), egui::Color32::from_rgb(240, 169, 93));
                        } else if !key.starts_with("sk-") {
                            self.msg = ("看起来不是有效的 Key（应以 sk- 开头）".into(), egui::Color32::from_rgb(240, 106, 93));
                        } else {
                            let mut c = store::load_config();
                            c.api_key = key;
                            store::save_config(&c);
                            self.msg = ("✓ 已保存，小组件下次刷新即生效".into(), egui::Color32::from_rgb(52, 199, 123));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    if ui.button("取消").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        }
    }
}
