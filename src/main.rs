#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tao::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{ResizeDirection, Window, WindowBuilder},
};
use wry::{
    WebContext, WebViewBuilder,
    http::{Request, Response},
};

const INDEX_HTML: &str = include_str!("index.html");
const TERM_JS: &str = include_str!("term.js");
const XTERM_JS: &str = include_str!("vendor/xterm.js");
const XTERM_CSS: &str = include_str!("vendor/xterm.css");
const FIT_JS: &str = include_str!("vendor/addon-fit.js");
const LINKS_JS: &str = include_str!("vendor/addon-web-links.js");

const DEFAULT_W: f64 = 880.0;
const DEFAULT_H: f64 = 520.0;

enum Ev {
    Ready,
    Spawn { id: u32, cols: u16, rows: u16 },
    Input(u32, Vec<u8>),
    PtyResize { id: u32, cols: u16, rows: u16 },
    Kill(u32),
    Output(u32, Vec<u8>),
    Exited(u32),
    Copy(String),
    Paste(u32),
    Open(String),
    Glass(u8),
    Font(u8),
    Drag,
    Resize(ResizeDirection),
    Maximize,
    Minimize,
    Close,
}

#[derive(Clone)]
struct State {
    glass: u8,
    font: u8,
    /// Program to run in each tab, and its arguments.
    shell: String,
    args: Vec<String>,
    /// Last position and size in physical pixels: x, y, w, h.
    win: Option<(i32, i32, u32, u32)>,
}

/// One tab's pseudoconsole. Dropping the master closes the ConPTY, which is
/// also what finally ends the reader thread's blocking read.
struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

fn state_dir() -> PathBuf {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("glassterm");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
}

/// PowerShell 7 when it is installed, Windows PowerShell otherwise.
fn default_shell() -> (String, Vec<String>) {
    let exe = if on_path("pwsh.exe") {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    (exe.to_owned(), vec!["-NoLogo".to_owned()])
}

fn load_state() -> State {
    let v = std::fs::read_to_string(state_dir().join("state.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_default();
    let win = match (&v["x"], &v["y"], &v["w"], &v["h"]) {
        (x, y, w, h) if w.is_number() && h.is_number() => Some((
            x.as_i64().unwrap_or(0) as i32,
            y.as_i64().unwrap_or(0) as i32,
            w.as_u64().unwrap_or(0) as u32,
            h.as_u64().unwrap_or(0) as u32,
        )),
        _ => None,
    }
    .filter(|(_, _, w, h)| *w >= 360 && *h >= 220);

    let (shell, args) = match v["shell"].as_str() {
        Some(s) if !s.is_empty() => (
            s.to_owned(),
            v["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => default_shell(),
    };

    State {
        glass: v["glass"].as_u64().unwrap_or(1).min(2) as u8,
        font: v["font"].as_u64().unwrap_or(14).clamp(9, 32) as u8,
        shell,
        args,
        win,
    }
}

fn save_state(state: &State) {
    let mut doc = serde_json::json!({
        "glass": state.glass,
        "font": state.font,
        "shell": state.shell,
        "args": state.args,
    });
    if let Some((x, y, w, h)) = state.win {
        doc["x"] = x.into();
        doc["y"] = y.into();
        doc["w"] = w.into();
        doc["h"] = h.into();
    }
    let _ = std::fs::write(state_dir().join("state.json"), doc.to_string());
}

fn direction(tag: &str) -> Option<ResizeDirection> {
    Some(match tag {
        "n" => ResizeDirection::North,
        "s" => ResizeDirection::South,
        "w" => ResizeDirection::West,
        "e" => ResizeDirection::East,
        "nw" => ResizeDirection::NorthWest,
        "ne" => ResizeDirection::NorthEast,
        "sw" => ResizeDirection::SouthWest,
        "se" => ResizeDirection::SouthEast,
        _ => return None,
    })
}

fn serve(req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let (body, mime): (&'static str, &str) = match req.uri().path() {
        "/" | "/index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
        "/term.js" => (TERM_JS, "text/javascript; charset=utf-8"),
        "/xterm.js" => (XTERM_JS, "text/javascript; charset=utf-8"),
        "/xterm.css" => (XTERM_CSS, "text/css; charset=utf-8"),
        "/addon-fit.js" => (FIT_JS, "text/javascript; charset=utf-8"),
        "/addon-web-links.js" => (LINKS_JS, "text/javascript; charset=utf-8"),
        _ => {
            return Response::builder()
                .status(404)
                .body(Cow::Borrowed(&b"not found"[..]))
                .unwrap();
        }
    };
    Response::builder()
        .header("Content-Type", mime)
        .body(Cow::Borrowed(body.as_bytes()))
        .unwrap()
}

fn spawn(
    id: u32,
    cols: u16,
    rows: u16,
    state: &State,
    proxy: &EventLoopProxy<Ev>,
) -> Result<Session, String> {
    let size = PtySize {
        rows: rows.max(2),
        cols: cols.max(10),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| e.to_string())?;

    let mut cmd = CommandBuilder::new(&state.shell);
    cmd.args(&state.args);
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    let out = proxy.clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.send_event(Ev::Output(id, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // ConPTY does not hand the reader an EOF when the shell exits -- only when
    // the pseudoconsole itself is closed -- so exit is watched separately.
    let done = proxy.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = done.send_event(Ev::Exited(id));
    });

    Ok(Session {
        master: pair.master,
        writer,
        killer,
    })
}

/// Park a fresh window in the middle of the primary monitor.
fn place_center(window: &Window) {
    let Some(monitor) = window
        .primary_monitor()
        .or_else(|| window.current_monitor())
    else {
        return;
    };
    let area = monitor.size();
    let origin = monitor.position();
    let win = window.outer_size();
    window.set_outer_position(PhysicalPosition::new(
        origin.x + (area.width as i32 - win.width as i32) / 2,
        origin.y + (area.height as i32 - win.height as i32) / 2,
    ));
}

/// A real Gaussian blur of the desktop behind the window, with a tint we pick
/// (so it is *not* forced dark the way Win11's DWM acrylic is). Same approach
/// as prism and anion: window-vibrancy's apply_acrylic switches to the fixed
/// dark DWM backdrop on build 22621+, so the accent state is set directly.
/// gradient_color is 0xAABBGGRR.
#[cfg(target_os = "windows")]
fn backdrop(win: &Window) {
    use std::ffi::c_void;
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::core::{s, w};

    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }
    #[repr(C)]
    struct CompAttrData {
        attribute: u32,
        data: *mut c_void,
        size: usize,
    }

    type SetWca = unsafe extern "system" fn(HWND, *mut CompAttrData) -> i32;
    let set_wca: SetWca = unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else {
            return;
        };
        match GetProcAddress(user32, s!("SetWindowCompositionAttribute")) {
            Some(p) => std::mem::transmute::<unsafe extern "system" fn() -> isize, SetWca>(p),
            None => return,
        }
    };

    const WCA_ACCENT_POLICY: u32 = 19;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

    let mut accent = AccentPolicy {
        state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        flags: 2,
        gradient_color: 0x0D_1E_1A_16, // light, low-alpha frost (~0.05)
        animation_id: 0,
    };
    let mut data = CompAttrData {
        attribute: WCA_ACCENT_POLICY,
        data: &mut accent as *mut _ as *mut c_void,
        size: std::mem::size_of::<AccentPolicy>(),
    };
    unsafe {
        let hwnd = HWND(win.hwnd() as *mut c_void);
        set_wca(hwnd, &mut data);
    }
}

/// Hands a link to the default browser. Only web URLs: anything else coming
/// out of a terminal (file:, ms-settings:, ...) is not worth trusting.
#[cfg(target_os = "windows")]
fn open_url(url: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};

    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn open_url(_: &str) {}

fn parse(body: &str) -> Option<Ev> {
    let msg = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let id = || msg["id"].as_u64().map(|n| n as u32);
    let dims = || {
        Some((
            msg["cols"].as_u64()?.min(u16::MAX as u64) as u16,
            msg["rows"].as_u64()?.min(u16::MAX as u64) as u16,
        ))
    };
    Some(match msg["action"].as_str()? {
        "ready" => Ev::Ready,
        "spawn" => {
            let (cols, rows) = dims()?;
            Ev::Spawn {
                id: id()?,
                cols,
                rows,
            }
        }
        "input" => {
            // Keystrokes arrive as text; mouse reports in some modes are raw
            // bytes, which JSON cannot carry, so those come base64'd.
            let bytes = match (msg["data"].as_str(), msg["bin"].as_str()) {
                (Some(s), _) => s.as_bytes().to_vec(),
                (_, Some(b)) => B64.decode(b).ok()?,
                _ => return None,
            };
            Ev::Input(id()?, bytes)
        }
        "pty_resize" => {
            let (cols, rows) = dims()?;
            Ev::PtyResize {
                id: id()?,
                cols,
                rows,
            }
        }
        "kill" => Ev::Kill(id()?),
        "copy" => Ev::Copy(msg["text"].as_str()?.to_owned()),
        "paste" => Ev::Paste(id()?),
        "open" => Ev::Open(msg["url"].as_str()?.to_owned()),
        "glass" => Ev::Glass(msg["level"].as_u64()?.min(2) as u8),
        "font" => Ev::Font(msg["size"].as_u64()?.clamp(9, 32) as u8),
        "resize" => Ev::Resize(direction(msg["dir"].as_str()?)?),
        "drag" => Ev::Drag,
        "maximize" => Ev::Maximize,
        "minimize" => Ev::Minimize,
        "close" => Ev::Close,
        _ => return None,
    })
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let mut state = load_state();

    let mut builder = WindowBuilder::new()
        .with_title("glassterm")
        .with_min_inner_size(LogicalSize::new(360.0, 220.0))
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(true);

    builder = match state.win {
        Some((_, _, w, h)) => builder.with_inner_size(PhysicalSize::new(w, h)),
        None => builder.with_inner_size(LogicalSize::new(DEFAULT_W, DEFAULT_H)),
    };

    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::WindowBuilderExtWindows;
        builder = builder.with_undecorated_shadow(false);
    }

    let window = builder.build(&event_loop).unwrap();

    // A saved position can point at a monitor that no longer exists. There is
    // no titlebar to drag the window back with, so verify before trusting it.
    let visible = state.win.is_some_and(|(x, y, _, _)| {
        window.available_monitors().any(|m| {
            let (p, s) = (m.position(), m.size());
            x + 80 >= p.x
                && y + 40 >= p.y
                && x + 80 <= p.x + s.width as i32
                && y + 40 <= p.y + s.height as i32
        })
    });

    match state.win {
        Some((x, y, _, _)) if visible => window.set_outer_position(PhysicalPosition::new(x, y)),
        _ => place_center(&window),
    }

    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::WindowExtWindows;
        // Restores the drop shadow + rounded corners Windows gives decorated windows.
        window.set_undecorated_shadow(true);
        backdrop(&window);
    }

    let ctx: &'static mut WebContext =
        Box::leak(Box::new(WebContext::new(Some(state_dir().join("webview")))));

    let ipc = proxy.clone();
    let mut wb = WebViewBuilder::new_with_web_context(ctx)
        .with_transparent(true)
        .with_custom_protocol("glassterm".into(), |_id, req| serve(&req))
        .with_url("glassterm://localhost/index.html")
        .with_ipc_handler(move |req: Request<String>| {
            if let Some(ev) = parse(req.body()) {
                let _ = ipc.send_event(ev);
            }
        });

    #[cfg(target_os = "windows")]
    {
        use wry::WebViewBuilderExtWindows;
        // Otherwise WebView2 eats Ctrl+R (reload, which would kill every tab),
        // Ctrl+F, Ctrl+P, F5 and friends before the terminal ever sees them.
        wb = wb.with_browser_accelerator_keys(false);
    }

    let webview = wb.build(&window)?;
    let mut webview = Some(webview);

    let mut sessions: HashMap<u32, Session> = HashMap::new();
    // Output is coalesced per tab and flushed once per loop turn, so a flood
    // of small reads becomes one script call instead of thousands.
    let mut pending: HashMap<u32, Vec<u8>> = HashMap::new();
    // Clipboard goes through Rust: navigator.clipboard.readText() in WebView2
    // pops a permission prompt.
    let mut clipboard = arboard::Clipboard::new().ok();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        let js = |s: String| {
            if let Some(w) = webview.as_ref() {
                let _ = w.evaluate_script(&s);
            }
        };

        match event {
            Event::UserEvent(Ev::Ready) => js(format!(
                "window.__init({{glass:{},font:{}}})",
                state.glass, state.font
            )),
            Event::UserEvent(Ev::Spawn { id, cols, rows }) => {
                match spawn(id, cols, rows, &state, &proxy) {
                    Ok(s) => {
                        sessions.insert(id, s);
                    }
                    Err(e) => js(format!(
                        "window.__fail({id},{})",
                        serde_json::Value::String(format!(
                            "could not start {}: {e}",
                            state.shell
                        ))
                    )),
                }
            }
            Event::UserEvent(Ev::Input(id, bytes)) => {
                if let Some(s) = sessions.get_mut(&id) {
                    let _ = s.writer.write_all(&bytes);
                    let _ = s.writer.flush();
                }
            }
            Event::UserEvent(Ev::PtyResize { id, cols, rows }) => {
                if let Some(s) = sessions.get(&id) {
                    let _ = s.master.resize(PtySize {
                        rows: rows.max(2),
                        cols: cols.max(10),
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
            Event::UserEvent(Ev::Output(id, bytes)) => {
                pending.entry(id).or_default().extend_from_slice(&bytes);
            }
            Event::MainEventsCleared => {
                for (id, bytes) in pending.drain() {
                    js(format!("window.__out({id},\"{}\")", B64.encode(bytes)));
                }
            }
            Event::UserEvent(Ev::Exited(id)) => {
                if let Some(bytes) = pending.remove(&id) {
                    js(format!("window.__out({id},\"{}\")", B64.encode(bytes)));
                }
                if sessions.remove(&id).is_some() {
                    js(format!("window.__exit({id})"));
                }
            }
            Event::UserEvent(Ev::Kill(id)) => {
                pending.remove(&id);
                if let Some(mut s) = sessions.remove(&id) {
                    let _ = s.killer.kill();
                }
            }
            Event::UserEvent(Ev::Copy(text)) => {
                if let Some(c) = clipboard.as_mut() {
                    let _ = c.set_text(text);
                }
            }
            Event::UserEvent(Ev::Paste(id)) => {
                if let Some(text) = clipboard.as_mut().and_then(|c| c.get_text().ok()) {
                    js(format!(
                        "window.__paste({id},{})",
                        serde_json::Value::String(text)
                    ));
                }
            }
            Event::UserEvent(Ev::Open(url)) => open_url(&url),
            Event::UserEvent(Ev::Glass(level)) => {
                state.glass = level;
                save_state(&state);
            }
            Event::UserEvent(Ev::Font(size)) => {
                state.font = size;
                save_state(&state);
            }
            Event::UserEvent(Ev::Drag) => {
                let _ = window.drag_window();
            }
            Event::UserEvent(Ev::Resize(dir)) => {
                let _ = window.drag_resize_window(dir);
            }
            Event::UserEvent(Ev::Maximize) => {
                window.set_maximized(!window.is_maximized());
            }
            Event::UserEvent(Ev::Minimize) => {
                window.set_minimized(true);
            }
            Event::WindowEvent {
                event: WindowEvent::Focused(true),
                ..
            } => js("window.__focus && window.__focus()".into()),
            // Remember geometry so a resize survives a restart. Minimized and
            // maximized windows report sizes we do not want to restore to.
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                if size.width > 0
                    && size.height > 0
                    && !window.is_minimized()
                    && !window.is_maximized()
                    && let Ok(pos) = window.outer_position()
                {
                    state.win = Some((pos.x, pos.y, size.width, size.height));
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Moved(pos),
                ..
            } => {
                if !window.is_minimized()
                    && !window.is_maximized()
                    && pos.x > -30000
                    && pos.y > -30000
                {
                    let size = window.inner_size();
                    state.win = Some((pos.x, pos.y, size.width, size.height));
                }
            }
            Event::UserEvent(Ev::Close)
            | Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                save_state(&state);
                for (_, mut s) in sessions.drain() {
                    let _ = s.killer.kill();
                }
                let _ = webview.take();
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
