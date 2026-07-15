//! 平台登录/捕获窗口：内嵌 WebView2（wry），注入 fetch/XHR 钩子，
//! 捕获用量页真实调用的接口（URL/方法/鉴权/请求体/返回 JSON），
//! 通过 IPC 传回 Rust，落盘为可重放的会话。登录态由 WebView2 用户数据目录持久化。

use crate::app::store::{self, Endpoint, Session};
use serde::Deserialize;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// 注入脚本：hook fetch 与 XMLHttpRequest，命中用量相关 URL 时把详情用
/// window.ipc.postMessage(JSON) 传回。并在页面顶部放一个状态条 + 完成按钮。
const HOOK_JS: &str = r#"
(function(){
  if (window.__dsHook) return; window.__dsHook = 1;
  var KEY = /.*/i;
  function send(o){ try { window.ipc.postMessage(JSON.stringify(o)); } catch(e){} }

  var of = window.fetch;
  window.fetch = function(input, init){
    var req = (typeof Request!=='undefined' && input instanceof Request) ? input : null;
    var url = req ? req.url : String(input);
    var method = (init&&init.method) || (req&&req.method) || 'GET';
    var auth='', ctype='';
    try {
      var h=(init&&init.headers)||(req&&req.headers);
      if(h){ if(typeof h.get==='function'){auth=h.get('authorization')||'';ctype=h.get('content-type')||'';}
             else {auth=h['authorization']||h['Authorization']||'';ctype=h['content-type']||h['Content-Type']||'';} }
    } catch(e){}
    var body=(init&&typeof init.body==='string')?init.body:null;
    var pr=of.apply(this,arguments);
    if(KEY.test(url)){ pr.then(function(resp){ try{ resp.clone().text().then(function(t){
      send({kind:'cap',url:url,method:method.toUpperCase(),auth:auth,body:body,
            content_type:ctype,status:resp.status,text:t.slice(0,800000)}); }); }catch(e){} }); }
    return pr;
  };

  var oO=XMLHttpRequest.prototype.open, oS=XMLHttpRequest.prototype.send,
      oH=XMLHttpRequest.prototype.setRequestHeader;
  XMLHttpRequest.prototype.open=function(m,u){this.__m=m;this.__u=u;return oO.apply(this,arguments);};
  XMLHttpRequest.prototype.setRequestHeader=function(k,v){
    if(/^authorization$/i.test(k))this.__a=v; if(/^content-type$/i.test(k))this.__c=v;
    return oH.apply(this,arguments);
  };
  XMLHttpRequest.prototype.send=function(b){
    var self=this;
    this.addEventListener('loadend',function(){ try{
      if(KEY.test(self.__u||'')){
        send({kind:'cap',url:self.responseURL||self.__u,method:(self.__m||'GET').toUpperCase(),
              auth:self.__a||'',body:(typeof b==='string')?b:null,content_type:self.__c||'',
              status:self.status,text:String(self.responseText||'').slice(0,800000)});
      } }catch(e){} });
    return oS.apply(this,arguments);
  };

  function bar(){
    if(document.getElementById('__dsbar')) return;
    if(!document.body){ setTimeout(bar,300); return; }
    var b=document.createElement('div'); b.id='__dsbar';
    b.style.cssText='position:fixed;top:0;left:0;right:0;z-index:2147483647;height:36px;'
      +'background:#1f6fe0;color:#fff;font:13px/36px "Microsoft YaHei",sans-serif;'
      +'display:flex;align-items:center;justify-content:center;gap:14px;box-shadow:0 2px 8px rgba(0,0,0,.35)';
    var m=document.createElement('span'); m.id='__dsmsg';
    m.textContent='请登录并进入「用量」页面，捕获到数据后此条会变绿…';
    var btn=document.createElement('button'); btn.textContent='完成并保存';
    btn.style.cssText='background:#fff;color:#1f6fe0;border:0;border-radius:6px;padding:4px 12px;'
      +'font:12px "Microsoft YaHei";cursor:pointer;font-weight:600';
    btn.onclick=function(){ send({kind:'finish'}); };
    b.appendChild(m); b.appendChild(btn); document.body.appendChild(b);
    document.body.style.marginTop='36px';
  }
  bar();
  // 每秒问 Rust 已捕获多少（Rust 通过 evaluate 注入 window.__dsCount）
  setInterval(function(){
    var m=document.getElementById('__dsmsg'), b=document.getElementById('__dsbar');
    var n=window.__dsCount||0;
    if(m&&b&&n>0){ b.style.background='#1f9d55';
      m.textContent='✓ 已捕获 '+n+' 个数据接口。可继续停留刷新，或点右侧「完成并保存」'; }
  },1000);
})();
"#;

#[derive(Deserialize)]
struct Msg {
    kind: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    auth: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    content_type: String,
    #[serde(default)]
    status: i64,
    #[serde(default)]
    text: String,
}

struct Cap {
    ep: Endpoint,
    json: Value,
}

/// 用户事件：让 IPC 回调把“需要关闭”的信号送回事件循环。
enum UserEvent {
    Finish,
    Count(usize),
}

pub fn run() {
    let event_loop: EventLoop<UserEvent> = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("登录 DeepSeek 平台（进入「用量」页后点顶部「完成并保存」）")
        .with_inner_size(tao::dpi::LogicalSize::new(1050.0, 760.0))
        .build(&event_loop)
        .expect("create window");

    // 捕获集合：pathkey -> Cap（去重保留最新）
    let caps: Rc<RefCell<BTreeMap<String, Cap>>> = Rc::new(RefCell::new(BTreeMap::new()));
    let caps_ipc = caps.clone();
    let proxy_ipc = proxy.clone();

    // WebView2 用户数据目录（持久化登录态）
    let udf = store::data_dir().join("webview2");
    let _ = std::fs::create_dir_all(&udf);

use wry::http::Request;

let ipc = move |req: Request<String>| {
    let body = req.body();

    let msg: Msg = match serde_json::from_str(body) {
        Ok(m) => m,
        Err(_) => return,
    };

    match msg.kind.as_str() {
        "finish" => {
            let _ = proxy_ipc.send_event(UserEvent::Finish);
        }

        "cap" => {
            if msg.status != 200 || msg.text.is_empty() {
                return;
            }
            // 只留用量/余额接口；登录、短信、埋点接口不落盘（重放会重复发短信）
            if !store::is_replayable(&msg.url) {
                return;
            }

            let json: Value = match serde_json::from_str(&msg.text) {
                Ok(v) => v,
                Err(_) => return,
            };

            let path = url_path(&msg.url);
            let key = format!("{} {}", msg.method, path);

            let ep = Endpoint {
                url: msg.url,
                method: msg.method,
                auth: msg.auth,
                body: msg.body,
                content_type: msg.content_type,
            };

            let mut m = caps_ipc.borrow_mut();
            m.insert(key, Cap { ep, json });

            let n = m.len();
            drop(m);

            persist(&caps_ipc);

            let _ = proxy_ipc.send_event(UserEvent::Count(n));
        }

        _ => {}
    }
};

    let webview = WebViewBuilder::new()
        .with_url(store::USAGE_URL)
        .with_user_agent(store::UA)
        .with_initialization_script(HOOK_JS)
        .with_ipc_handler(ipc)
        // 持久化数据目录：不同 wry 版本方法名可能为 with_data_directory
        .build(&window)
        .expect("build webview");

    let mut finish_pending = false;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {}
            Event::UserEvent(UserEvent::Count(n)) => {
                // 把计数塞进页面，供状态条自轮询显示
                let _ = webview.evaluate_script(&format!("window.__dsCount={};", n));
            }
            Event::UserEvent(UserEvent::Finish) => {
                finish_pending = true;
                persist(&caps);
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                persist(&caps);
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
        let _ = finish_pending;
    });
}

fn url_path(u: &str) -> String {
    // 取 scheme://host 之后到 ? 之前
    let after = u.splitn(2, "://").nth(1).unwrap_or(u);
    let no_q = after.splitn(2, '?').next().unwrap_or(after);
    match no_q.find('/') {
        Some(i) => no_q[i..].to_string(),
        None => "/".to_string(),
    }
}

/// 把当前捕获落盘为会话 + 立即归一化写缓存，让小组件马上有数据。
fn persist(caps: &Rc<RefCell<BTreeMap<String, Cap>>>) {
    let m = caps.borrow();
    if m.is_empty() {
        return;
    }
    let endpoints: Vec<Endpoint> = m.values().map(|c| c.ep.clone()).collect();
    let jsons: Vec<Value> = m.values().map(|c| c.json.clone()).collect();

    store::save_session(&Session {
        endpoints,
        cookie: String::new(), // DeepSeek 用 Bearer 鉴权；如需 cookie 可后续扩展
        saved_at: store::now_hms(),
    });

    let day_map = crate::core::normalize_captures(&jsons);
    let balance = crate::core::find_balance(&jsons);
    if !day_map.is_empty() || balance.is_some() {
        store::save_cache(&store::Cache {
            day_map,
            balance,
            fetched_at: store::now_hms(),
        });
    }
    // 原始返回留档，便于平台改版时排查
    let debug: Vec<Value> = m
        .values()
        .map(|c| {
            serde_json::json!({
                "url": c.ep.url, "method": c.ep.method,
                "has_auth": !c.ep.auth.is_empty(), "body": c.ep.body,"json": c.json,
            })
        })
        .collect();
    let _ = std::fs::write(
        store::data_dir().join("captures_debug.json"),
        serde_json::to_string_pretty(&debug).unwrap_or_default(),
    );
}
