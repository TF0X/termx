#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tao::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{Event, StartCause, WindowEvent},
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
const WEBGL_JS: &str = include_str!("vendor/addon-webgl.js");
const PROMPT_PS1: &str = include_str!("prompt.ps1");
const PROMPT_BASH: &str = include_str!("prompt.bash");

const DEFAULT_W: f64 = 880.0;
const DEFAULT_H: f64 = 520.0;

/// How long the window must sit unfocused before the webview is asked to
/// drop caches. Invisible to the user; shells keep running.
const IDLE: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_secs(60);
/// While a wobbly drag is live, how often to check whether the mouse is up.
const DRAG_POLL: Duration = Duration::from_millis(25);

enum Ev {
    Ready,
    Spawn {
        id: u32,
        cols: u16,
        rows: u16,
        profile: usize,
    },
    Input(u32, Vec<u8>),
    PtyResize {
        id: u32,
        cols: u16,
        rows: u16,
    },
    Kill(u32),
    Output(u32, Vec<u8>),
    Exited(u32),
    Profiles(Vec<Profile>),
    Copy(String),
    Paste(u32),
    Open(String),
    Glass(u8),
    Font(u8),
    Wobble(bool),
    Theme(bool),
    Drag { wobble: bool },
    Settled,
    Resize(ResizeDirection),
    Maximize,
    Minimize,
    Close,
}

/// Something that can run in a pane: what the dropdown lists.
#[derive(Clone)]
struct Profile {
    name: String,
    /// Picks the badge the UI draws; not used on this side.
    kind: String,
    cmd: String,
    args: Vec<String>,
}

#[derive(Clone)]
struct State {
    glass: u8,
    font: u8,
    wobble: bool,
    /// Built-in robbyrussell-style prompt for PowerShell, cmd and Git Bash.
    theme: bool,
    /// A user-set shell from state.json, listed first when present.
    custom: Option<(String, Vec<String>)>,
    /// Last position and size in physical pixels: x, y, w, h.
    win: Option<(i32, i32, u32, u32)>,
}

/// One pane's pseudoconsole. Dropping the master closes the ConPTY, which is
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

fn profile(name: &str, kind: &str, cmd: &str, args: &[&str]) -> Profile {
    Profile {
        name: name.to_owned(),
        kind: kind.to_owned(),
        cmd: cmd.to_owned(),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Shells that can be found with a few file checks, so the first tab never
/// waits on detection. WSL is slower to ask and is appended later.
fn base_profiles(state: &State) -> Vec<Profile> {
    let mut v = Vec::new();
    if let Some((cmd, args)) = &state.custom {
        v.push(Profile {
            name: "Custom".into(),
            kind: "custom".into(),
            cmd: cmd.clone(),
            args: args.clone(),
        });
    }
    if on_path("pwsh.exe") {
        v.push(profile("PowerShell", "pwsh", "pwsh.exe", &["-NoLogo"]));
    }
    v.push(profile(
        "Windows PowerShell",
        "powershell",
        "powershell.exe",
        &["-NoLogo"],
    ));
    v.push(profile("Command Prompt", "cmd", "cmd.exe", &[]));

    let git_bash = [
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        dirs::data_local_dir().map(|d| d.join("Programs")),
    ]
    .into_iter()
    .flatten()
    .map(|d| d.join("Git").join("bin").join("bash.exe"))
    .find(|p| p.is_file());
    if let Some(bash) = git_bash {
        v.push(profile(
            "Git Bash",
            "gitbash",
            &bash.to_string_lossy(),
            &["--login", "-i"],
        ));
    }
    v
}

/// One profile per installed WSL distro, skipping Docker Desktop's internal
/// ones the way Windows Terminal does.
#[cfg(target_os = "windows")]
fn wsl_profiles() -> Vec<Profile> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    if !on_path("wsl.exe") {
        return Vec::new();
    }
    let Ok(out) = std::process::Command::new("wsl.exe")
        .args(["-l", "-q"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    // wsl.exe writes UTF-16LE regardless of the console code page.
    let wide: Vec<u16> = out
        .stdout
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&wide)
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
        .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
        .map(|name| profile(name, "wsl", "wsl.exe", &["-d", name, "--cd", "~"]))
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn wsl_profiles() -> Vec<Profile> {
    Vec::new()
}

fn profiles_json(profiles: &[Profile]) -> String {
    serde_json::Value::Array(
        profiles
            .iter()
            .map(|p| serde_json::json!({ "name": p.name, "kind": p.kind }))
            .collect(),
    )
    .to_string()
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

    let custom = v["shell"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| {
            let args = v["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            (s.to_owned(), args)
        });

    State {
        glass: v["glass"].as_u64().unwrap_or(1).min(2) as u8,
        font: v["font"].as_u64().unwrap_or(14).clamp(9, 32) as u8,
        wobble: v["wobble"].as_bool().unwrap_or(true),
        theme: v["theme"].as_bool().unwrap_or(true),
        custom,
        win,
    }
}

fn save_state(state: &State) {
    let mut doc = serde_json::json!({
        "glass": state.glass,
        "font": state.font,
        "wobble": state.wobble,
        "theme": state.theme,
    });
    if let Some((shell, args)) = &state.custom {
        doc["shell"] = shell.as_str().into();
        doc["args"] = args.clone().into();
    }
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
        "/addon-webgl.js" => (WEBGL_JS, "text/javascript; charset=utf-8"),
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
    profile: &Profile,
    theme: bool,
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

    let mut cmd = CommandBuilder::new(&profile.cmd);
    cmd.args(&profile.args);
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if theme {
        apply_theme(&mut cmd, &profile.kind);
    }

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

/// Dresses the shell up with the built-in prompt. WSL and custom shells are
/// left exactly as their own config has them.
fn apply_theme(cmd: &mut CommandBuilder, kind: &str) {
    match kind {
        "pwsh" | "powershell" => {
            // -EncodedCommand (UTF-16LE, base64) sidesteps every quoting rule
            // between here, CreateProcess and PowerShell's own parser.
            let utf16: Vec<u8> = PROMPT_PS1
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            cmd.args(["-NoExit", "-EncodedCommand", &B64.encode(utf16)]);
        }
        "cmd" => {
            cmd.env(
                "PROMPT",
                "$E]0;$P$E\\$E[1;32m\u{279C}$S$S$E[1;36m$P$E[0m$S",
            );
        }
        "gitbash" => {
            cmd.env("PROMPT_COMMAND", PROMPT_BASH);
        }
        _ => {}
    }
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
///
/// `on = false` drops back to a plain transparent window. The blur is always
/// the full window rectangle, so it has to go while the content wobbles or a
/// rigid frosted box would show around the warped page.
#[cfg(target_os = "windows")]
fn backdrop(win: &Window, on: bool) {
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
    const ACCENT_DISABLED: u32 = 0;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

    let mut accent = AccentPolicy {
        state: if on {
            ACCENT_ENABLE_ACRYLICBLURBEHIND
        } else {
            ACCENT_DISABLED
        },
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

#[cfg(not(target_os = "windows"))]
fn backdrop(_: &Window, _: bool) {}

/// Frosted look on or off: the blur, and the frame shadow that would otherwise
/// outline the transparent margin the wobble needs.
fn glass_frame(window: &Window, on: bool) {
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::WindowExtWindows;
        window.set_undecorated_shadow(on);
    }
    backdrop(window, on);
}

#[cfg(target_os = "windows")]
fn mouse_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000 != 0 }
}

#[cfg(not(target_os = "windows"))]
fn mouse_down() -> bool {
    false
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

#[cfg(target_os = "windows")]
fn set_memory(wv: &wry::WebView, low: bool) {
    use wry::{MemoryUsageLevel, WebViewExtWindows};
    let _ = wv.set_memory_usage_level(if low {
        MemoryUsageLevel::Low
    } else {
        MemoryUsageLevel::Normal
    });
}

#[cfg(not(target_os = "windows"))]
fn set_memory(_: &wry::WebView, _: bool) {}

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
                profile: msg["profile"].as_u64().unwrap_or(0) as usize,
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
        "wobble" => Ev::Wobble(msg["on"].as_bool()?),
        "theme" => Ev::Theme(msg["on"].as_bool()?),
        "drag" => Ev::Drag {
            wobble: msg["wobble"].as_bool().unwrap_or(false),
        },
        "settled" => Ev::Settled,
        "resize" => Ev::Resize(direction(msg["dir"].as_str()?)?),
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
    let mut profiles = base_profiles(&state);

    let wsl = proxy.clone();
    std::thread::spawn(move || {
        let found = wsl_profiles();
        if !found.is_empty() {
            let _ = wsl.send_event(Ev::Profiles(found));
        }
    });

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

    glass_frame(&window, true);

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
        // Otherwise WebView2 eats Ctrl+R (reload, which would kill every pane),
        // Ctrl+F, Ctrl+P, F5 and friends before the terminal ever sees them.
        wb = wb.with_browser_accelerator_keys(false);
    }

    let webview = wb.build(&window)?;
    let mut webview = Some(webview);

    let mut sessions: HashMap<u32, Session> = HashMap::new();
    // Output is coalesced per pane and flushed once per loop turn, so a flood
    // of small reads becomes one script call instead of thousands.
    let mut pending: HashMap<u32, Vec<u8>> = HashMap::new();
    // Clipboard goes through Rust: navigator.clipboard.readText() in WebView2
    // pops a permission prompt.
    let mut clipboard = arboard::Clipboard::new().ok();

    let mut dragging = false;
    let mut focused = true;
    let mut blurred_at = Instant::now();
    let mut trimmed = false;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = if dragging {
            ControlFlow::WaitUntil(Instant::now() + DRAG_POLL)
        } else {
            ControlFlow::WaitUntil(Instant::now() + POLL)
        };

        let js = |s: String| {
            if let Some(w) = webview.as_ref() {
                let _ = w.evaluate_script(&s);
            }
        };

        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                // Moving the window runs Windows' own modal loop, which tao
                // gives no end event for; the button coming up is the signal.
                if dragging && !mouse_down() {
                    dragging = false;
                    js("window.__dragEnd()".into());
                }
                if !focused
                    && !trimmed
                    && blurred_at.elapsed() >= IDLE
                    && let Some(w) = webview.as_ref()
                {
                    set_memory(w, true);
                    trimmed = true;
                }
            }
            Event::UserEvent(Ev::Ready) => js(format!(
                "window.__init({{glass:{},font:{},wobble:{},theme:{},profiles:{}}})",
                state.glass,
                state.font,
                state.wobble,
                state.theme,
                profiles_json(&profiles)
            )),
            Event::UserEvent(Ev::Profiles(more)) => {
                profiles.extend(more);
                js(format!("window.__profiles({})", profiles_json(&profiles)));
            }
            Event::UserEvent(Ev::Spawn {
                id,
                cols,
                rows,
                profile,
            }) => {
                let p = profiles.get(profile).unwrap_or(&profiles[0]);
                match spawn(id, cols, rows, p, state.theme, &proxy) {
                    Ok(s) => {
                        sessions.insert(id, s);
                    }
                    Err(e) => js(format!(
                        "window.__fail({id},{})",
                        serde_json::Value::String(format!("could not start {}: {e}", p.cmd))
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
            Event::UserEvent(Ev::Wobble(on)) => {
                state.wobble = on;
                save_state(&state);
            }
            Event::UserEvent(Ev::Theme(on)) => {
                state.theme = on;
                save_state(&state);
            }
            Event::UserEvent(Ev::Drag { wobble }) => {
                if wobble && window.is_maximized() {
                    // Dragging un-maximizes; not worth warping through that.
                    js("window.__dragEnd()".into());
                } else if wobble {
                    glass_frame(&window, false);
                    dragging = true;
                    *control_flow = ControlFlow::WaitUntil(Instant::now() + DRAG_POLL);
                }
                let _ = window.drag_window();
            }
            // The page has sprung back to rest; bring the frost back.
            Event::UserEvent(Ev::Settled) => glass_frame(&window, true),
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
                event: WindowEvent::Focused(has_focus),
                ..
            } => {
                focused = has_focus;
                if has_focus {
                    if trimmed && let Some(w) = webview.as_ref() {
                        set_memory(w, false);
                        trimmed = false;
                    }
                    js("window.__focus && window.__focus()".into());
                } else {
                    blurred_at = Instant::now();
                }
            }
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
                if dragging {
                    js(format!("window.__moved({},{})", pos.x, pos.y));
                }
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
