# DeepSeek 桌面小部件

基于 Rust 开发的轻量级 DeepSeek 用量监控桌面小部件，上着班烧token懒得切网页刷就写了一个。

## 功能

- **原生桌面组件**：基于 `egui/eframe` 绘制，占用资源低。
- **登录状态获取**：内置 WebView2，可直接登录 DeepSeek 自动获取用量信息。
- **自动更新数据**：自动获取最新用量信息，支持 API Key 配置。

## 构建

### 环境要求

- Rust（推荐 `stable-x86_64-pc-windows-msvc`）
- Visual Studio Build Tools（包含 C++ 桌面开发工具）

### 编译

在项目根目录执行：

```bash
cargo build --release
```

编译完成后，可执行文件位于：

```text
target/release/DeepSeekWidget.exe
```

## 计划

- [x] 优化 UI，~~现在的有点丑了我说白了~~
- [ ] 支持自定义窗口透明度
- [ ] 支持深色 / 浅色主题
