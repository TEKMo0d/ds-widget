// 入口：Windows 上分派到托盘/登录/小组件；非 Windows 仅用于跑核心单元测试。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod core;

#[cfg(windows)]
mod app; // 平台相关（托盘、登录捕获、原生小组件、重放网络）

#[cfg(windows)]
fn main() {
    app::run();
}

#[cfg(not(windows))]
fn main() {
    println!("此二进制的 GUI 仅在 Windows 上运行；当前平台仅用于编译与测试核心逻辑。");
}
