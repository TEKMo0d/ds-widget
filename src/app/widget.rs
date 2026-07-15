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
    source: String,
    note: String,
    updated: String,
}

pub fn run() {
    let cfg = store::load_config();
    let (w, h) = (cfg.window_width, cfg.window_height);

    let viewport = egui::ViewportBuilder::default()
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
    tab: Tab,
    snap: Arc<Mutex<Snapshot>>,
    trigger: mpsc::Sender<()>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        setup_style(&cc.egui_ctx);
        
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
            let cfg2 = cfg.clone();
            let interval = cfg.refresh_seconds.max(10);
            thread::spawn(move || loop {
                let res = store::get_usage(days);
                let bal = store::fetch_balance(&cfg2);
                {
                    let mut s = snap.lock().unwrap();
                    s.series = res.series;
                    s.source = res.source.to_string();
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
                    s.updated = store::now_hms();
                }
                ctx.request_repaint();
                // 等间隔或被手动唤醒
                let _ = rx.recv_timeout(Duration::from_secs(interval));
            });
        }
        App {
            cfg,
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
        let snap = self.snap.lock().unwrap().clone();

        let frame = egui::Frame::new()
            .fill(C_BG)
            .corner_radius(12)
            .stroke(egui::Stroke::new(1.0, C_LINE));

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            self.titlebar(ui, &snap, ctx);
            if !snap.note.is_empty() {
                note_bar(ui, &snap.note);
            }
            ui.add_space(2.0);
            match self.tab {
                Tab::Balance => self.page_balance(ui, &snap),
                Tab::Requests => self.page_requests(ui, &snap),
                Tab::Tokens => self.page_tokens(ui, &snap),
            }
            // 底部标签栏固定在底部
            self.tabs(ui);
        });
    }
}

// ───────── 顶栏 ─────────

impl App {
    fn titlebar(&mut self, ui: &mut egui::Ui, snap: &Snapshot, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            // 拖动区（点住空白拖动窗口）
            let logo = egui::Color32::from_rgb(79, 142, 247);
            ui.painter().circle_filled(
                ui.cursor().min + egui::vec2(6.0, 10.0),
                4.0,
                logo,
            );
            ui.add_space(12.0);
            ui.label(egui::RichText::new("DeepSeek").strong().size(14.0).color(C_TEXT));
            let src = if snap.source == "web" { " · 平台" } else { "" };
            ui.label(
                egui::RichText::new(format!("{}{}", snap.updated, src))
                    .size(10.5)
                    .color(C_DIM),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_btn(ui, "✕", C_DIM).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if icon_btn(ui, "—", C_DIM).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                if icon_btn(ui, "⟳", C_DIM).clicked() {
                    let _ = self.trigger.send(());
                }
            });
        });
        // 让整条顶栏可拖动
        let bar = ui.interact(
            egui::Rect::from_min_size(ui.min_rect().min, egui::vec2(ui.available_width(), 0.0)),
            ui.id().with("drag"),
            egui::Sense::drag(),
        );
        if bar.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let w = ui.available_width() / 3.0;
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
            ui.separator();
        });
    }
}

// ───────── 余额页 ─────────

impl App {
    fn page_balance(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.add_space(8.0);
        if let Some(err) = &snap.balance_err {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(egui::RichText::new("⚠️ 余额获取失败").color(C_DIM));
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
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(&b.total)
                    .size(34.0)
                    .strong()
                    .color(C_TEXT),
            );
            ui.label(egui::RichText::new(&b.currency).size(14.0).color(C_DIM));
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let (col, txt) = if b.is_available {
                (C_GREEN, "余额可用")
            } else {
                (C_RED, "余额不足")
            };
            pill(ui, col, txt);
            if b.from_web {
                pill(ui, C_DIM, "来自平台网页");
            }
        });
        ui.add_space(14.0);
        row(ui, "充值余额", &format!("{} {}", b.topped_up, b.currency));
        row(ui, "赠送余额", &format!("{} {}", b.granted, b.currency));

        // 今日两块统计
        let today = snap.series.last().cloned().unwrap_or_default().1;
        let tok = today.hit + today.miss + today.output;
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            stat(ui, "今日请求", &fmt(today.requests));
            ui.add_space(8.0);
            stat(ui, "今日 Tokens", &fmt(tok));
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
        let max = snap.series.iter().map(|(_, d)| d.requests).max().unwrap_or(1).max(1);
        bar_chart(ui, snap, max, |d| vec![(d.requests, C_B2)], false);
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
        let max = snap.series.iter().map(|(_, d)| tot(d)).max().unwrap_or(1).max(1);
        // 堆叠：命中 / 未命中 / 输出（从下到上）
        bar_chart(
            ui,
            snap,
            max,
            |d| vec![(d.hit, C_B1), (d.miss, C_B2), (d.output, C_B3)],
            true,
        );
        axis(ui, snap);
        legend(ui);
    }
}

/// 画柱状图，segs 返回每个柱子的分段 (值, 颜色)（从底到顶堆叠）。
/// tok=true 时悬停显示三项明细，否则只显示请求数。
fn bar_chart(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    max: i64,
    segs: impl Fn(&Day) -> Vec<(i64, egui::Color32)>,
    tok: bool,
) {
    let n = snap.series.len().max(1);
    let avail = ui.available_size();
    let chart_h = (avail.y - 60.0).max(80.0);
    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), chart_h), egui::Sense::hover());
    let painter = ui.painter();
    painter.with_clip_rect(rect);

    let gap = 1.0f32;
    let bw = ((rect.width() - gap * (n as f32 - 1.0)) / n as f32).max(1.0);
    let pointer = ui.input(|i| i.pointer.hover_pos());

    // 网格上限标签
    painter.text(
        rect.left_top() + egui::vec2(2.0, 0.0),
        egui::Align2::LEFT_TOP,
        fmt(max),
        egui::FontId::monospace(10.0),
        C_DIM,
    );

    let mut hover_idx: Option<usize> = None;
    for (i, (_date, d)) in snap.series.iter().enumerate() {
        let x0 = rect.left() + i as f32 * (bw + gap);
        let col_rect = egui::Rect::from_min_size(
            egui::pos2(x0, rect.top()),
            egui::vec2(bw, rect.height()),
        );
        // 堆叠
        let mut y = rect.bottom();
        for (val, color) in segs(d) {
            if val <= 0 {
                continue;
            }
            let seg_h = (val as f32 / max as f32) * rect.height();
            let seg = egui::Rect::from_min_max(egui::pos2(x0, y - seg_h), egui::pos2(x0 + bw, y));
            painter.rect_filled(seg, 0, color);
            y -= seg_h;
        }
        if let Some(p) = pointer {
            if col_rect.contains(p) {
                hover_idx = Some(i);
                painter.rect_filled(col_rect, 0, egui::Color32::from_white_alpha(16));
            }
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
    let w = 190.0;
    let line_h = 18.0;
    let h = 10.0 + line_h * lines.len() as f32;
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
        egui::Color32::from_rgb(44, 44, 48),
        egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 58, 62)),
        egui::StrokeKind::Inside,
    );
    let mut y = rect.top() + 6.0;
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
            if strong { C_TEXT } else { egui::Color32::from_rgb(201, 201, 206) },
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

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "msyh".into(),
        egui::FontData::from_owned(
            std::fs::read("C:\\Windows\\Fonts\\msyh.ttc")
                .expect("无法加载微软雅黑字体")
        ).into(),
    );

    fonts.families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .insert(0, "msyh".into());

    fonts.families
        .get_mut(&egui::FontFamily::Monospace)
        .unwrap()
        .insert(0, "msyh".into());

    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();

    style.visuals.override_text_color = Some(C_TEXT);

    ctx.set_style(style);
}

fn icon_btn(ui: &mut egui::Ui, txt: &str, col: egui::Color32) -> egui::Response {
    let btn = egui::Button::new(egui::RichText::new(txt).size(13.0).color(col))
        .fill(egui::Color32::TRANSPARENT)
        .min_size(egui::vec2(22.0, 22.0));
    ui.add(btn)
}

fn tab_btn(ui: &mut egui::Ui, w: f32, txt: &str, active: bool, mut on: impl FnMut()) {
    let col = if active { C_TEXT } else { C_DIM };
    let resp = ui.add_sized(
        egui::vec2(w, 26.0),
        egui::Button::new(egui::RichText::new(txt).size(12.0).color(col))
            .fill(egui::Color32::TRANSPARENT),
    );
    if active {
        let r = resp.rect;
        ui.painter().line_segment(
            [egui::pos2(r.left(), r.top()), egui::pos2(r.right(), r.top())],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(79, 142, 247)),
        );
    }
    if resp.clicked() {
        on();
    }
}

fn pill(ui: &mut egui::Ui, dot: egui::Color32, txt: &str) {
    egui::Frame::new()
        .fill(C_CARD)
        .stroke(egui::Stroke::new(1.0, C_LINE))
        .corner_radius(99)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.painter().circle_filled(
                    ui.cursor().min + egui::vec2(3.0, 8.0),
                    3.0,
                    dot,
                );
                ui.add_space(8.0);
                ui.label(egui::RichText::new(txt).size(11.0).color(C_DIM));
            });
        });
}

fn row(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(k).size(12.5).color(C_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(v).size(12.5).monospace().color(C_TEXT));
        });
    });
}

fn stat(ui: &mut egui::Ui, k: &str, v: &str) {
    let w = (ui.available_width() - 8.0) / 2.0;
    egui::Frame::new()
        .fill(C_CARD)
        .stroke(egui::Stroke::new(1.0, C_LINE))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(w);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(k).size(11.0).color(C_DIM));
                ui.label(egui::RichText::new(v).size(16.0).strong().monospace().color(C_TEXT));
            });
        });
}

fn chart_head(ui: &mut egui::Ui, title: &str, total: i64) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).size(12.5).strong().color(C_DIM));
        ui.label(egui::RichText::new(fmt(total)).size(17.0).strong().monospace().color(C_TEXT));
    });
    ui.add_space(4.0);
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
        for (c, t) in [
            (C_B1, "输入（命中缓存）"),
            (C_B2, "输入（未命中缓存）"),
            (C_B3, "输出"),
        ] {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(ui.cursor().min + egui::vec2(0.0, 4.0), egui::vec2(8.0, 8.0)),
                2,
                c,
            );
            ui.add_space(12.0);
            ui.label(egui::RichText::new(t).size(10.5).color(C_DIM));
            ui.add_space(6.0);
        }
    });
}

fn empty_hint(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(egui::RichText::new("暂无用量数据").color(C_DIM));
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

// 配色
const C_BG: egui::Color32 = egui::Color32::from_rgb(20, 20, 20);
const C_CARD: egui::Color32 = egui::Color32::from_rgb(28, 28, 30);
const C_LINE: egui::Color32 = egui::Color32::from_rgb(42, 42, 45);
const C_TEXT: egui::Color32 = egui::Color32::from_rgb(236, 236, 238);
const C_DIM: egui::Color32 = egui::Color32::from_rgb(142, 142, 147);
const C_B1: egui::Color32 = egui::Color32::from_rgb(167, 205, 245);
const C_B2: egui::Color32 = egui::Color32::from_rgb(91, 156, 240);
const C_B3: egui::Color32 = egui::Color32::from_rgb(31, 111, 224);
const C_GREEN: egui::Color32 = egui::Color32::from_rgb(52, 199, 123);
const C_RED: egui::Color32 = egui::Color32::from_rgb(240, 106, 93);
const C_AMBER: egui::Color32 = egui::Color32::from_rgb(240, 169, 93);
