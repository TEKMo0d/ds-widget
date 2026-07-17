//! 原生小组件（eframe/egui）。置顶、无边框、不显示在任务栏。
//! 三个分页：余额 / 请求次数 / Tokens（柱状图悬停显示明细）。
//! 取数在后台线程进行，UI 只读快照，绝不卡。

use crate::app::store::{self, Config};
use crate::core::{Balance, Day};
use eframe::egui;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Default)]
struct Snapshot {
    series: Vec<(String, Day)>,
    balance: Option<Balance>,
    balance_err: Option<String>,
    note: String,
}

pub fn run() {
    let cfg = store::load_config();
    let (w, h) = (cfg.window_width, cfg.window_height);

    let icon = egui::IconData {
        rgba: crate::app::ICON_64.to_vec(),
        width: 64,
        height: 64,
    };
    let viewport = egui::ViewportBuilder::default()
        .with_icon(icon)
        .with_inner_size([w, h])
        .with_min_inner_size([300.0, 360.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false) // ← 原生不上任务栏
        .with_resizable(true);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let _ = eframe::run_native(
        "DeepSeek Widget",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, cfg)))),
    );
}

enum Tab {
    Balance,
    Requests,
    Tokens,
}

struct App {
    cfg: Config,
    shared_cfg: Arc<Mutex<Config>>,
    last_cfg_reload: std::time::Instant,
    applied_passthrough: Option<bool>,
    tab: Tab,
    snap: Arc<Mutex<Snapshot>>,
    trigger: mpsc::Sender<()>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        setup_style(&cc.egui_ctx);

        let shared_cfg = Arc::new(Mutex::new(cfg.clone()));
        let snap = Arc::new(Mutex::new(Snapshot {
            balance_err: Some("加载中…".into()),
            ..Default::default()
        }));
        let (tx, rx) = mpsc::channel::<()>();
        // 后台取数线程
        {
            let snap = snap.clone();
            let ctx = cc.egui_ctx.clone();
            let days = cfg.days;
            let shared = shared_cfg.clone();
            let interval = cfg.refresh_seconds.max(10);
            thread::spawn(move || loop {
                let cfg_now = shared.lock().unwrap().clone();
                let res = store::get_usage(days);
                let bal = store::fetch_balance(&cfg_now);
                {
                    let mut s = snap.lock().unwrap();
                    s.series = res.series;
                    s.note = res.note;
                    match bal {
                        Ok(b) => {
                            s.balance = Some(b);
                            s.balance_err = None;
                        }
                        Err(e) => {
                            s.balance = res.balance.clone().or(s.balance.clone());
                            if s.balance.is_none() {
                                s.balance_err = Some(e);
                            } else {
                                s.balance_err = None;
                            }
                        }
                    }
                }
                ctx.request_repaint();
                // 等间隔或被手动唤醒
                let _ = rx.recv_timeout(Duration::from_secs(interval));
            });
        }
        App {
            cfg,
            shared_cfg,
            last_cfg_reload: std::time::Instant::now(),
            applied_passthrough: None,
            tab: Tab::Balance,
            snap,
            trigger: tx,
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // 透明，靠圆角面板填色
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 每秒重读配置：设置窗口保存后即时生效（透明度/穿透/API Key）
        if self.last_cfg_reload.elapsed() > Duration::from_secs(1) {
            self.last_cfg_reload = std::time::Instant::now();
            let c = store::load_config();
            if c.api_key != self.cfg.api_key {
                *self.shared_cfg.lock().unwrap() = c.clone();
                let _ = self.trigger.send(()); // 密钥变了立即刷新
            }
            self.cfg = c;
        }
        // 鼠标穿透开关（只在变化时下发）
        if self.applied_passthrough != Some(self.cfg.click_through) {
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(
                self.cfg.click_through,
            ));
            self.applied_passthrough = Some(self.cfg.click_through);
        }
        ctx.request_repaint_after(Duration::from_secs(1));

        let snap = self.snap.lock().unwrap().clone();
        let op = self.cfg.opacity.clamp(0.2, 1.0);

        let frame = egui::Frame::new()
            .fill(fade(C_BG, op))
            .corner_radius(14)
            .stroke(egui::Stroke::new(1.0, fade(C_LINE, op)))
            .inner_margin(egui::Margin {
                left: 14,
                right: 14,
                top: 10,
                bottom: 12,
            });

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // 固定底部标签栏高度，内容区与其互不侵占，避免被裁切
            let full = ui.max_rect();
            let tab_h = 38.0;
            let content_rect =
                egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, full.max.y - tab_h));
            let tabs_rect = egui::Rect::from_min_max(
                egui::pos2(full.min.x, full.max.y - tab_h),
                full.max,
            );

            ui.scope_builder(egui::UiBuilder::new().max_rect(content_rect), |ui| {
                ui.set_clip_rect(content_rect.expand(2.0));
                self.titlebar(ui, ctx);
                if !snap.note.is_empty() {
                    ui.add_space(4.0);
                    note_bar(ui, &snap.note);
                }
                ui.add_space(2.0);
                match self.tab {
                    Tab::Balance => self.page_balance(ui, &snap),
                    Tab::Requests => self.page_requests(ui, &snap),
                    Tab::Tokens => self.page_tokens(ui, &snap),
                }
            });

            ui.scope_builder(egui::UiBuilder::new().max_rect(tabs_rect), |ui| {
                self.tabs(ui);
            });
        });
    }
}

// ───────── 顶栏 ─────────

impl App {
    fn titlebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let bar_h = 28.0;
        let bar_rect =
            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), bar_h));

        // 拖动区避开右侧按钮（egui 允许点击与拖拽命中不同控件，
        // 若覆盖按钮，点按钮会同时触发窗口拖动的模态循环，导致 UI 卡顿）
        let drag_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2((ui.available_width() - 118.0).max(0.0), bar_h),
        );
        let drag = ui.interact(
            drag_rect,
            ui.id().with("drag"),
            egui::Sense::click_and_drag(),
        );
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        ui.scope_builder(egui::UiBuilder::new().max_rect(bar_rect), |ui| {
            ui.horizontal_centered(|ui| {
                // Logo 圆点
                let c = ui.cursor().min;
                ui.painter().circle_filled(
                    egui::pos2(c.x + 5.0, bar_rect.center().y),
                    4.5,
                    C_ACCENT,
                );
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new("DeepSeek")
                        .strong()
                        .size(14.0)
                        .color(C_TEXT),
                );
                ui.add_space(2.0);
                // 实时时钟（update 里已有每秒重绘）
                ui.label(
                    egui::RichText::new(store::now_hms())
                        .size(10.5)
                        .color(C_DIM),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_btn(ui, "×").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if icon_btn(ui, "—").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    if icon_btn(ui, "⚙").clicked() {
                        crate::app::open_settings();
                    }
                    if icon_btn(ui, "⟳").clicked() {
                        let _ = self.trigger.send(());
                    }
                });
            });
        });
        ui.advance_cursor_after_rect(bar_rect);
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        let op = self.cfg.opacity.clamp(0.2, 1.0);
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(fade(C_CARD, op))
            .stroke(egui::Stroke::new(1.0, fade(C_LINE, op)))
            .corner_radius(9)
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 6.0) / 3.0;
                    tab_btn(ui, w, "余额", matches!(self.tab, Tab::Balance), || {
                        self.tab = Tab::Balance
                    });
                    tab_btn(ui, w, "请求次数", matches!(self.tab, Tab::Requests), || {
                        self.tab = Tab::Requests
                    });
                    tab_btn(ui, w, "Tokens", matches!(self.tab, Tab::Tokens), || {
                        self.tab = Tab::Tokens
                    });
                });
            });
    }
}

// ───────── 余额页 ─────────

impl App {
    fn page_balance(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        let op = self.cfg.opacity.clamp(0.2, 1.0);
        ui.add_space(10.0);
        if let Some(err) = &snap.balance_err {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(egui::RichText::new("⚠ 余额获取失败").size(13.0).color(C_DIM));
                ui.add_space(6.0);
                ui.label(egui::RichText::new(err).color(C_RED).size(11.0));
            });
            return;
        }
        let b = match &snap.balance {
            Some(b) => b,
            None => return,
        };
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(money(&b.total))
                    .size(36.0)
                    .strong()
                    .color(C_TEXT),
            );
            ui.vertical(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new(&b.currency).size(13.0).color(C_DIM));
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            let (col, txt) = if b.is_available {
                (C_GREEN, "余额可用")
            } else {
                (C_RED, "余额不足")
            };
            pill(ui, op, col, txt);
            if b.from_web {
                pill(ui, op, C_DIM, "来自平台网页");
            }
        });

        ui.add_space(14.0);
        // 明细卡片
        egui::Frame::new()
            .fill(fade(C_CARD, op))
            .stroke(egui::Stroke::new(1.0, fade(C_LINE, op)))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(12, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                row(ui, "充值余额", &format!("{} {}", money(&b.topped_up), b.currency));
                sep(ui);
                row(ui, "赠送余额", &format!("{} {}", money(&b.granted), b.currency));
            });

        // 今日两块统计
        let today = snap.series.last().cloned().unwrap_or_default().1;
        let tok = today.hit + today.miss + today.output;
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let w = (ui.available_width() - 8.0) / 2.0;
            stat(ui, op, w, "今日请求", &fmt(today.requests));
            stat(ui, op, w, "今日 Tokens", &fmt(tok));
        });
    }
}

// ───────── 图表页 ─────────

impl App {
    fn page_requests(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        let total: i64 = snap.series.iter().map(|(_, d)| d.requests).sum();
        chart_head(ui, "API 请求次数", total);
        if total == 0 {
            empty_hint(ui);
            return;
        }
        let max = snap
            .series
            .iter()
            .map(|(_, d)| d.requests)
            .max()
            .unwrap_or(1)
            .max(1);
        bar_chart(ui, snap, max, |d| vec![(d.requests, C_B2)], false, 30.0);
        axis(ui, snap);
    }

    fn page_tokens(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        let tot = |d: &Day| d.hit + d.miss + d.output;
        let total: i64 = snap.series.iter().map(|(_, d)| tot(d)).sum();
        chart_head(ui, "Tokens", total);
        if total == 0 {
            empty_hint(ui);
            return;
        }
        let max = snap
            .series
            .iter()
            .map(|(_, d)| tot(d))
            .max()
            .unwrap_or(1)
            .max(1);
        // 堆叠：命中 / 未命中 / 输出（从下到上）
        bar_chart(
            ui,
            snap,
            max,
            |d| vec![(d.hit, C_B1), (d.miss, C_B2), (d.output, C_B3)],
            true,
            60.0,
        );
        axis(ui, snap);
        ui.add_space(4.0);
        legend(ui);
    }
}

/// 画柱状图，segs 返回每个柱子的分段 (值, 颜色)（从底到顶堆叠）。
/// tok=true 时悬停显示三项明细；reserve 为图下方需要保留的高度。
fn bar_chart(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    max: i64,
    segs: impl Fn(&Day) -> Vec<(i64, egui::Color32)>,
    tok: bool,
    reserve: f32,
) {
    let n = snap.series.len().max(1);
    let avail = ui.available_size();
    let chart_h = (avail.y - reserve).max(80.0);
    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), chart_h), egui::Sense::hover());
    let painter = ui.painter_at(rect.expand(1.0));

    // 网格线：顶（max）、中（max/2）、底基线
    for (frac, val) in [(0.0f32, max), (0.5, max / 2)] {
        let y = rect.top() + frac * (rect.height() - 14.0) + 12.0;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, C_GRID),
        );
        painter.text(
            egui::pos2(rect.left(), y - 12.0),
            egui::Align2::LEFT_TOP,
            fmt(val),
            egui::FontId::monospace(9.5),
            C_DIM,
        );
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, C_LINE),
    );

    // 柱区顶部让出网格标签的空间
    let plot_top = rect.top() + 12.0;
    let plot_h = rect.bottom() - plot_top;

    let gap = if n > 60 { 0.0f32 } else { 1.0 };
    let bw = ((rect.width() - gap * (n as f32 - 1.0)) / n as f32).max(1.0);
    let pointer = ui.input(|i| i.pointer.hover_pos());

    let mut hover_idx: Option<usize> = None;
    for (i, (_date, d)) in snap.series.iter().enumerate() {
        let x0 = rect.left() + i as f32 * (bw + gap);
        let col_rect = egui::Rect::from_min_size(
            egui::pos2(x0, rect.top()),
            egui::vec2(bw, rect.height()),
        );
        let hovered = pointer.map_or(false, |p| col_rect.contains(p));
        if hovered {
            hover_idx = Some(i);
            painter.rect_filled(col_rect, 3, egui::Color32::from_white_alpha(10));
        }
        // 堆叠，最顶段圆角收头
        let parts: Vec<(i64, egui::Color32)> =
            segs(d).into_iter().filter(|(v, _)| *v > 0).collect();
        let mut y = rect.bottom();
        for (j, (val, color)) in parts.iter().enumerate() {
            let seg_h = (*val as f32 / max as f32) * plot_h;
            let seg =
                egui::Rect::from_min_max(egui::pos2(x0, y - seg_h), egui::pos2(x0 + bw, y));
            let top = j + 1 == parts.len() && bw >= 3.0;
            let cr = if top {
                egui::CornerRadius {
                    nw: 2,
                    ne: 2,
                    sw: 0,
                    se: 0,
                }
            } else {
                egui::CornerRadius::ZERO
            };
            let c = if hovered {
                color.gamma_multiply(1.15)
            } else {
                *color
            };
            painter.rect_filled(seg, cr, c);
            y -= seg_h;
        }
    }

    if let (Some(i), Some(p)) = (hover_idx, pointer) {
        let (date, d) = &snap.series[i];
        tooltip(ui, p, date, d, tok);
    }
}

fn tooltip(ui: &egui::Ui, pos: egui::Pos2, date: &str, d: &Day, tok: bool) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("ds_tip"),
    ));
    let lines: Vec<(String, String, Option<egui::Color32>)> = if tok {
        let total = d.hit + d.miss + d.output;
        vec![
            (date.to_string(), fmt(total), None),
            ("输入（命中缓存）".into(), fmt(d.hit), Some(C_B1)),
            ("输入（未命中缓存）".into(), fmt(d.miss), Some(C_B2)),
            ("输出".into(), fmt(d.output), Some(C_B3)),
        ]
    } else {
        vec![
            (date.to_string(), String::new(), None),
            ("请求次数".into(), fmt(d.requests), None),
        ]
    };
    // 按最长一行实测宽度自适应
    let w = ui.fonts(|f| {
        let mut w = 150.0f32;
        for (i, (k, v, col)) in lines.iter().enumerate() {
            let size = if i == 0 { 13.0 } else { 12.0 };
            let kw = f
                .layout_no_wrap(k.clone(), egui::FontId::proportional(size), C_TEXT)
                .rect
                .width();
            let vw = f
                .layout_no_wrap(v.clone(), egui::FontId::monospace(size), C_TEXT)
                .rect
                .width();
            let swatch = if col.is_some() { 14.0 } else { 0.0 };
            w = w.max(10.0 + swatch + kw + 18.0 + vw + 10.0);
        }
        w
    });
    let line_h = 18.0;
    let h = 12.0 + line_h * lines.len() as f32;
    let mut origin = pos + egui::vec2(12.0, -h - 10.0);
    let screen = ui.ctx().screen_rect();
    if origin.x + w > screen.right() - 6.0 {
        origin.x = pos.x - w - 12.0;
    }
    if origin.y < screen.top() + 6.0 {
        origin.y = pos.y + 14.0;
    }
    let rect = egui::Rect::from_min_size(origin, egui::vec2(w, h));
    painter.rect(
        rect,
        8,
        egui::Color32::from_rgb(40, 40, 44),
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 66)),
        egui::StrokeKind::Inside,
    );
    let mut y = rect.top() + 7.0;
    for (i, (k, v, col)) in lines.iter().enumerate() {
        let strong = i == 0;
        let kx = rect.left() + 10.0;
        if let Some(c) = col {
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(kx, y + 4.0), egui::vec2(8.0, 8.0)),
                2,
                *c,
            );
        }
        let kx2 = if col.is_some() { kx + 14.0 } else { kx };
        painter.text(
            egui::pos2(kx2, y),
            egui::Align2::LEFT_TOP,
            k,
            egui::FontId::proportional(if strong { 13.0 } else { 12.0 }),
            if strong {
                C_TEXT
            } else {
                egui::Color32::from_rgb(201, 201, 206)
            },
        );
        painter.text(
            egui::pos2(rect.right() - 10.0, y),
            egui::Align2::RIGHT_TOP,
            v,
            egui::FontId::monospace(if strong { 13.0 } else { 12.0 }),
            C_TEXT,
        );
        y += line_h;
    }
}

// ───────── 小组件通用绘制辅助 ─────────

pub(crate) fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "msyh".into(),
        egui::FontData::from_owned(
            std::fs::read("C:\\Windows\\Fonts\\msyh.ttc").expect("无法加载微软雅黑字体"),
        )
        .into(),
    );

    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .insert(0, "msyh".into());

    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .unwrap()
        .insert(0, "msyh".into());

    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    style.visuals.override_text_color = Some(C_TEXT);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    ctx.set_style(style);
}

fn icon_btn(ui: &mut egui::Ui, txt: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 6, egui::Color32::from_white_alpha(14));
    }
    let col = if resp.hovered() { C_TEXT } else { C_DIM };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        txt,
        egui::FontId::proportional(if txt == "×" { 16.0 } else { 13.0 }),
        col,
    );
    resp
}

fn tab_btn(ui: &mut egui::Ui, w: f32, txt: &str, active: bool, mut on: impl FnMut()) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 7, C_ACCENT_BG);
    } else if resp.hovered() {
        painter.rect_filled(rect, 7, egui::Color32::from_white_alpha(8));
    }
    let col = if active {
        C_ACCENT_TEXT
    } else if resp.hovered() {
        C_TEXT
    } else {
        C_DIM
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        txt,
        egui::FontId::proportional(12.0),
        col,
    );
    if resp.clicked() {
        on();
    }
}

fn pill(ui: &mut egui::Ui, op: f32, dot: egui::Color32, txt: &str) {
    egui::Frame::new()
        .fill(fade(C_CARD, op))
        .stroke(egui::Stroke::new(1.0, fade(C_LINE, op)))
        .corner_radius(99)
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.horizontal(|ui| {
                let c = ui.cursor().min;
                ui.painter()
                    .circle_filled(egui::pos2(c.x + 3.0, c.y + 8.0), 3.0, dot);
                ui.add_space(8.0);
                ui.label(egui::RichText::new(txt).size(11.0).color(C_DIM));
            });
        });
}

fn row(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.horizontal(|ui| {
        ui.set_height(26.0);
        ui.label(egui::RichText::new(k).size(12.5).color(C_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(v).size(12.5).monospace().color(C_TEXT));
        });
    });
}

fn sep(ui: &mut egui::Ui) {
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            egui::pos2(r.left(), r.top()),
            egui::pos2(r.right(), r.top()),
        ],
        egui::Stroke::new(1.0, C_LINE),
    );
}

fn stat(ui: &mut egui::Ui, op: f32, w: f32, k: &str, v: &str) {
    egui::Frame::new()
        .fill(fade(C_CARD, op))
        .stroke(egui::Stroke::new(1.0, fade(C_LINE, op)))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(w - 24.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(k).size(11.0).color(C_DIM));
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(v)
                        .size(17.0)
                        .strong()
                        .monospace()
                        .color(C_TEXT),
                );
            });
        });
}

fn chart_head(ui: &mut egui::Ui, title: &str, total: i64) {
    ui.add_space(8.0);
    // 手绘保证标题与数字基线对齐
    let h = 22.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
    let baseline = rect.bottom() - 3.0;
    let painter = ui.painter();
    let t = painter.text(
        egui::pos2(rect.left(), baseline),
        egui::Align2::LEFT_BOTTOM,
        title,
        egui::FontId::proportional(12.5),
        C_DIM,
    );
    painter.text(
        egui::pos2(t.right() + 8.0, baseline + 2.0),
        egui::Align2::LEFT_BOTTOM,
        fmt(total),
        egui::FontId::monospace(18.0),
        C_TEXT,
    );
    ui.add_space(6.0);
}

fn axis(ui: &mut egui::Ui, snap: &Snapshot) {
    if snap.series.is_empty() {
        return;
    }
    let f = |d: &str| {
        let m: i64 = d[5..7].parse().unwrap_or(0);
        let day: i64 = d[8..10].parse().unwrap_or(0);
        format!("{}/{}", m, day)
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(f(&snap.series[0].0)).size(10.5).color(C_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(f(&snap.series[snap.series.len() - 1].0))
                    .size(10.5)
                    .color(C_DIM),
            );
        });
    });
}

fn legend(ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (c, t) in [
            (C_B1, "命中缓存"),
            (C_B2, "未命中"),
            (C_B3, "输出"),
        ] {
            let cur = ui.cursor().min;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(cur + egui::vec2(0.0, 4.0), egui::vec2(8.0, 8.0)),
                2,
                c,
            );
            ui.add_space(11.0);
            ui.label(egui::RichText::new(t).size(10.5).color(C_DIM));
            ui.add_space(8.0);
        }
    });
}

fn empty_hint(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(egui::RichText::new("暂无用量数据").size(13.0).color(C_DIM));
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("右键托盘图标 →「登录 DeepSeek 平台…」\n登录一次即可自动同步官方用量")
                .size(11.0)
                .color(C_DIM),
        );
    });
}

fn note_bar(ui: &mut egui::Ui, note: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(48, 40, 24))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(note).size(10.5).color(C_AMBER));
        });
}

/// 金额显示：能解析成数字就固定两位小数，否则原样（如 "-"）
fn money(s: &str) -> String {
    s.parse::<f64>()
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|_| s.to_string())
}

/// 按不透明度衰减颜色（用于背景类颜色，文字保持不透明以保证可读）
fn fade(c: egui::Color32, op: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * op) as u8)
}

fn fmt(n: i64) -> String {
    // 千分位
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let mut r: String = out.chars().rev().collect();
    if n < 0 {
        r.insert(0, '-');
    }
    r
}

// ───────── 设置窗口（与小组件同风格：无边框深色圆角） ─────────

pub fn run_settings() {
    let icon = egui::IconData {
        rgba: crate::app::ICON_64.to_vec(),
        width: 64,
        height: 64,
    };
    let viewport = egui::ViewportBuilder::default()
        .with_icon(icon)
        .with_inner_size([340.0, 440.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_resizable(false);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let cfg = store::load_config();
    let masked = mask_key(&cfg.api_key);
    let (op0, ct0) = (cfg.opacity.clamp(0.2, 1.0), cfg.click_through);

    let _ = eframe::run_native(
        "DeepSeek 小组件设置",
        options,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            form_style(&cc.egui_ctx);
            Ok(Box::new(SettingsApp {
                key_input: String::new(),
                show_key: false,
                cur_masked: masked,
                opacity: op0,
                click_through: ct0,
                msg: String::new(),
            }) as Box<dyn eframe::App>)
        }),
    );
}

fn mask_key(cur: &str) -> String {
    if cur.len() > 12 {
        format!("{}…{}", &cur[..6], &cur[cur.len() - 4..])
    } else if cur.is_empty() {
        "（未设置）".into()
    } else {
        cur.to_string()
    }
}

/// 表单控件（输入框/滑条/勾选框）的深色风格
fn form_style(ctx: &egui::Context) {
    ctx.style_mut(|s| {
        let v = &mut s.visuals;
        v.extreme_bg_color = egui::Color32::from_rgb(15, 15, 17); // TextEdit/滑条槽底色
        v.selection.bg_fill = C_ACCENT;
        v.selection.stroke = egui::Stroke::new(1.0, C_ACCENT_TEXT);
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.bg_fill = C_CARD;
            w.weak_bg_fill = C_CARD;
            w.bg_stroke = egui::Stroke::new(1.0, C_LINE);
            w.corner_radius = egui::CornerRadius::same(7);
            w.fg_stroke = egui::Stroke::new(1.0, C_TEXT);
        }
        v.widgets.hovered.bg_fill = egui::Color32::from_rgb(40, 40, 44);
        v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(40, 40, 44);
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, C_ACCENT);
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, C_ACCENT);
        s.spacing.slider_width = 130.0;
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}

struct SettingsApp {
    key_input: String,
    show_key: bool,
    cur_masked: String,
    opacity: f32,
    click_through: bool,
    msg: String,
}

impl eframe::App for SettingsApp {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        // 背景手动按窗口实际大小绘制：Frame 会随内容高度扩展，
        // 内容略超出窗口时圆角下半截会被画到窗口外，导致底部变直角。
        let frame = egui::Frame::new().inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 10,
            bottom: 14,
        });

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.painter().rect(
                ctx.screen_rect().shrink(0.5),
                14,
                C_BG,
                egui::Stroke::new(1.0, C_LINE),
                egui::StrokeKind::Inside,
            );
            // 顶栏（可拖动）
            let bar_h = 28.0;
            let bar_rect = egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(ui.available_width(), bar_h),
            );
            // 拖动区避开右侧 × 按钮（原因见小组件 titlebar 注释）
            let drag_rect = egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2((ui.available_width() - 34.0).max(0.0), bar_h),
            );
            let drag = ui.interact(
                drag_rect,
                ui.id().with("drag"),
                egui::Sense::click_and_drag(),
            );
            if drag.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            ui.scope_builder(egui::UiBuilder::new().max_rect(bar_rect), |ui| {
                ui.horizontal_centered(|ui| {
                    let c = ui.cursor().min;
                    ui.painter().circle_filled(
                        egui::pos2(c.x + 5.0, bar_rect.center().y),
                        4.5,
                        C_ACCENT,
                    );
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("小组件设置")
                            .strong()
                            .size(14.0)
                            .color(C_TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_btn(ui, "×").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });
            ui.advance_cursor_after_rect(bar_rect);

            ui.add_space(12.0);
            section(ui, "API KEY");
            card(ui, |ui| {
                row(ui, "当前", &self.cur_masked);
                sep(ui);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 52.0;
                    ui.add_sized(
                        [w, 26.0],
                        egui::TextEdit::singleline(&mut self.key_input)
                            .password(!self.show_key)
                            .hint_text("sk-…（留空则不修改）")
                            .font(egui::FontId::monospace(12.0)),
                    );
                    let t = if self.show_key { "隐藏" } else { "显示" };
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(t).size(11.5).color(C_DIM))
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, C_LINE))
                                .corner_radius(7)
                                .min_size(egui::vec2(44.0, 26.0)),
                        )
                        .clicked()
                    {
                        self.show_key = !self.show_key;
                    }
                });
                ui.add_space(4.0);
            });

            ui.add_space(14.0);
            section(ui, "外观");
            card(ui, |ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.set_height(28.0);
                    ui.label(egui::RichText::new("背景不透明度").size(12.5).color(C_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Slider::new(&mut self.opacity, 0.2..=1.0)
                                .trailing_fill(true)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                .custom_parser(|s| {
                                    s.trim_end_matches('%')
                                        .parse::<f64>()
                                        .ok()
                                        .map(|v| v / 100.0)
                                }),
                        );
                    });
                });
                ui.add_space(6.0);
                sep(ui);
                ui.add_space(8.0);
                ui.checkbox(
                    &mut self.click_through,
                    egui::RichText::new("鼠标穿透（不遮挡点击）")
                        .size(12.5)
                        .color(C_TEXT),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "开启后小组件完全不响应鼠标（无法拖动/点击），\n如需恢复请从托盘菜单进入「设置…」关闭本选项。",
                    )
                    .size(10.5)
                    .color(C_DIM),
                );
                ui.add_space(2.0);
            });

            // 底部按钮（锚定底部）
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let save = ui.add(
                        egui::Button::new(
                            egui::RichText::new("保存")
                                .size(13.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(C_ACCENT)
                        .corner_radius(8)
                        .min_size(egui::vec2(92.0, 30.0)),
                    );
                    let cancel = ui.add(
                        egui::Button::new(egui::RichText::new("取消").size(13.0).color(C_TEXT))
                            .fill(C_CARD)
                            .stroke(egui::Stroke::new(1.0, C_LINE))
                            .corner_radius(8)
                            .min_size(egui::vec2(68.0, 30.0)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("保存后 1 秒内生效")
                                .size(10.5)
                                .color(C_DIM),
                        );
                    });
                    if save.clicked() {
                        let key = self.key_input.trim().to_string();
                        if !key.is_empty() && !key.starts_with("sk-") {
                            self.msg = "Key 看起来无效（应以 sk- 开头），未保存".into();
                        } else {
                            let mut c = store::load_config();
                            if !key.is_empty() {
                                c.api_key = key;
                            }
                            c.opacity = self.opacity;
                            c.click_through = self.click_through;
                            store::save_config(&c);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    if cancel.clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                if !self.msg.is_empty() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(&self.msg).size(11.0).color(C_RED));
                }
            });
        });
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(11.0)
            .strong()
            .color(C_DIM),
    );
    ui.add_space(6.0);
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(C_CARD)
        .stroke(egui::Stroke::new(1.0, C_LINE))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

// 配色
const C_BG: egui::Color32 = egui::Color32::from_rgb(19, 19, 21);
const C_CARD: egui::Color32 = egui::Color32::from_rgb(28, 28, 31);
const C_LINE: egui::Color32 = egui::Color32::from_rgb(44, 44, 48);
const C_GRID: egui::Color32 = egui::Color32::from_rgb(33, 33, 36);
const C_TEXT: egui::Color32 = egui::Color32::from_rgb(236, 236, 238);
const C_DIM: egui::Color32 = egui::Color32::from_rgb(142, 142, 147);
const C_ACCENT: egui::Color32 = egui::Color32::from_rgb(79, 142, 247);
const C_ACCENT_BG: egui::Color32 = egui::Color32::from_rgb(36, 53, 88);
const C_ACCENT_TEXT: egui::Color32 = egui::Color32::from_rgb(148, 186, 255);
const C_B1: egui::Color32 = egui::Color32::from_rgb(167, 205, 245);
const C_B2: egui::Color32 = egui::Color32::from_rgb(91, 156, 240);
const C_B3: egui::Color32 = egui::Color32::from_rgb(31, 111, 224);
const C_GREEN: egui::Color32 = egui::Color32::from_rgb(52, 199, 123);
const C_RED: egui::Color32 = egui::Color32::from_rgb(240, 106, 93);
const C_AMBER: egui::Color32 = egui::Color32::from_rgb(240, 169, 93);
