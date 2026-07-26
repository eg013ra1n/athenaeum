//! Tray mode: a tao event loop on the main thread, the supervisor + web server on
//! a background tokio runtime. The icon is drawn programmatically (a filled
//! circle per state) — no image assets, no decoder dependency.
//!
//! Only compiled with `--features tray` (the module is gated in `lib.rs`), so a
//! headless build pulls none of the GUI crates. The event loop owns the whole
//! process: `run_tray` blocks until Quit, which shuts the supervisor down
//! gracefully and exits.

use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::supervisor::AgentState;

/// User-events forwarded onto the tao event loop from off-thread sources: menu
/// clicks (tray-icon's global handler), lifecycle-state changes (the
/// supervisor's `watch` channel, bridged by a tokio task), and the deferred
/// start-at-login probe result (see [`run_tray`]).
enum UserEvent {
    Menu(MenuEvent),
    State(AgentState),
    AutoLaunch(bool),
}

/// 32×32 RGBA filled circle. macOS gets a black template glyph (with a hollow
/// ring variant for `NeedsSetup`) that the system tints for light/dark menu
/// bars; other platforms get state colors. Returns `(rgba, is_template)`.
fn icon_rgba(state: &AgentState) -> (Vec<u8>, bool) {
    let template = cfg!(target_os = "macos");
    let (r, g, b) = if template {
        (0, 0, 0)
    } else {
        match state {
            AgentState::NeedsSetup { .. } => (128, 128, 128),
            AgentState::Starting => (66, 133, 244),
            AgentState::Running { in_flight } if *in_flight > 0 => (66, 133, 244),
            AgentState::Running { .. } => (52, 168, 83),
            AgentState::Failed { .. } => (217, 48, 37),
        }
    };
    let hollow = matches!(state, AgentState::NeedsSetup { .. });
    let (w, c) = (32i32, 15.5f32);
    let mut buf = vec![0u8; (w * w * 4) as usize];
    for y in 0..w {
        for x in 0..w {
            let d = (((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt() - 13.0).max(0.0);
            let mut a = (1.0 - d).clamp(0.0, 1.0); // 1px anti-aliased edge
            if hollow {
                // Ring: carve the inner disc out (anti-aliased inner edge too).
                let inner = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
                if inner < 9.0 {
                    a = 0.0;
                } else if inner < 10.0 {
                    a *= inner - 9.0;
                }
            }
            let i = ((y * w + x) * 4) as usize;
            buf[i] = r;
            buf[i + 1] = g;
            buf[i + 2] = b;
            buf[i + 3] = (a * 255.0) as u8;
        }
    }
    (buf, template)
}

/// The status line shown (disabled) at the top of the tray menu.
fn state_line(state: &AgentState, dirs: usize) -> String {
    match state {
        AgentState::NeedsSetup { needs } => format!("Setup required: {}", needs.join(", ")),
        AgentState::Starting => "Starting…".into(),
        AgentState::Running { in_flight: 0 } => format!("Watching {dirs} folder(s)"),
        AgentState::Running { in_flight } => format!("Syncing {in_flight} package(s)"),
        AgentState::Failed { error } => format!("Error: {error}"),
    }
}

/// Run the tray: build the tao event loop, spin up the supervisor + web server on
/// a background tokio runtime, and drive the menu-bar icon until Quit. Owns its
/// own runtime — `main` must NOT be `#[tokio::main]` on this path. Blocks (never
/// returns on macOS; the process exits on Quit).
pub fn run_tray(config_path: std::path::PathBuf) -> anyhow::Result<()> {
    // `mut` is only exercised by the macOS activation-policy call below;
    // Windows/Linux builds would otherwise warn `unused_mut`.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // Menu-bar only: no Dock icon, no app window.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    // tray-icon delivers menu clicks through a single global handler; forward
    // them onto the tao loop as user-events.
    MenuEvent::set_event_handler(Some({
        let proxy = proxy.clone();
        move |e| {
            let _ = proxy.send_event(UserEvent::Menu(e));
        }
    }));

    // Probe the current start-at-login state OFF the UI thread. On macOS
    // `is_enabled()` shells out to `osascript` (System Events), which can block
    // for the AppleEvent timeout (~120s, measured) when Automation permission is
    // absent — never on the event-loop thread, or the tray icon would freeze
    // before it appears. The checkbox starts unchecked and is corrected via
    // `UserEvent::AutoLaunch` when the probe returns.
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            if let Some(al) = build_autolaunch() {
                let enabled = al.is_enabled().unwrap_or(false);
                let _ = proxy.send_event(UserEvent::AutoLaunch(enabled));
            }
        });
    }

    // Background tokio runtime hosting the supervisor + web server.
    let rt = tokio::runtime::Runtime::new()?;
    let handle = rt.block_on(crate::supervisor::start_supervised(config_path.clone()))?;
    let mut state_rx = handle.state.clone();
    // Bridge the supervisor's lifecycle-state channel onto the tao loop.
    rt.spawn({
        let proxy = proxy.clone();
        async move {
            while state_rx.changed().await.is_ok() {
                let state = state_rx.borrow().clone();
                if proxy.send_event(UserEvent::State(state)).is_err() {
                    break; // event loop gone
                }
            }
        }
    });

    let web_url = {
        // A broken config must not kill the tray — the supervisor already
        // surfaces the parse error (red icon + web banner). Fall back to the
        // default loopback bind so "Open Web UI" still points somewhere useful.
        let bind: std::net::SocketAddr = crate::config::Config::load_lenient_for_boot(&config_path)
            .ok()
            .map(|c| c.web_bind)
            .filter(|b| !b.is_empty())
            .and_then(|b| b.parse().ok())
            .unwrap_or_else(|| crate::config::DEFAULT_WEB_BIND.parse().unwrap());
        let host = if bind.ip().is_unspecified() {
            "127.0.0.1".to_string()
        } else {
            bind.ip().to_string()
        };
        format!("http://{host}:{}", bind.port())
    };
    tracing::info!(web_url = %web_url, "tray mode starting");

    // Menu: a disabled status line, then the actions.
    let status_item = MenuItem::new("Starting…", false, None);
    let open_item = MenuItem::new("Open Web UI", true, None);
    let autolaunch = build_autolaunch();
    // Unchecked initially; the background probe above corrects it via
    // `UserEvent::AutoLaunch` once `is_enabled()` returns (may be slow on macOS).
    let start_login = CheckMenuItem::new("Start at login", true, false, None);
    let quit_item = MenuItem::new("Quit Perseus", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &status_item,
        &PredefinedMenuItem::separator(),
        &open_item,
        &start_login,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])?;

    let mut tray = None; // created on Init (tao's Linux gtk backend requires it there)
    let mut handle = Some(handle);
    // Guards a start-at-login toggle in flight (see `spawn_toggle_autolaunch`):
    // a click while `true` is ignored so a slow osascript round-trip can't be
    // raced by a second click.
    let mut autolaunch_pending = false;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            tao::event::Event::NewEvents(tao::event::StartCause::Init) => {
                let (rgba, template) = icon_rgba(&AgentState::Starting);
                let icon = Icon::from_rgba(rgba, 32, 32).expect("static icon dims");
                let builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu.clone()))
                    .with_tooltip("Perseus")
                    .with_icon(icon);
                #[cfg(target_os = "macos")]
                let builder = builder.with_icon_as_template(template);
                #[cfg(not(target_os = "macos"))]
                let _ = template;
                tray = Some(builder.build().expect("create tray icon"));
                tracing::info!("tray icon created");
            }
            tao::event::Event::UserEvent(UserEvent::State(state)) => {
                let dirs = crate::config::Config::load_lenient_for_boot(&config_path)
                    .map(|c| c.capture_dirs_resolved().len())
                    .unwrap_or(0);
                status_item.set_text(state_line(&state, dirs));
                if let Some(t) = &tray {
                    let (rgba, template) = icon_rgba(&state);
                    if let Ok(icon) = Icon::from_rgba(rgba, 32, 32) {
                        #[cfg(target_os = "macos")]
                        let _ = t.set_icon_with_as_template(Some(icon), template);
                        #[cfg(not(target_os = "macos"))]
                        {
                            let _ = template;
                            let _ = t.set_icon(Some(icon));
                        }
                    }
                }
            }
            tao::event::Event::UserEvent(UserEvent::AutoLaunch(enabled)) => {
                // Covers both the startup probe and a completed toggle: either
                // way nothing is in flight anymore, so re-enable the item.
                autolaunch_pending = false;
                start_login.set_checked(enabled);
                start_login.set_enabled(true);
            }
            tao::event::Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id() == open_item.id() {
                    if let Err(error) = open::that(&web_url) {
                        tracing::error!(%error, url = %web_url, "open web ui failed");
                    }
                } else if e.id() == start_login.id() {
                    if autolaunch_pending {
                        // A toggle is already in flight; ignore the double-click
                        // rather than racing a second osascript call.
                    } else if let Some(al) = autolaunch.clone() {
                        autolaunch_pending = true;
                        start_login.set_enabled(false);
                        spawn_toggle_autolaunch(al, proxy.clone());
                    } else {
                        start_login.set_checked(false);
                    }
                } else if e.id() == quit_item.id() {
                    tracing::info!("quit requested; shutting down supervisor");
                    if let Some(h) = handle.take() {
                        rt.block_on(h.shutdown());
                    }
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}

/// Build an `AutoLaunch` for the current executable, or `None` (logged) when the
/// platform can't resolve one — the "Start at login" item then reads unchecked
/// and toggling is a no-op.
fn build_autolaunch() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("Perseus")
        .set_app_path(&exe.to_string_lossy())
        .build()
        .map_err(|error| tracing::warn!(%error, "autolaunch unavailable"))
        .ok()
}

/// Flip the start-at-login state OFF the UI thread. Like the startup probe,
/// `is_enabled`/`enable`/`disable` can shell out to `osascript` (System Events)
/// on macOS and block for the AppleEvent timeout (~120s, measured) — never on
/// the event-loop thread, or a single click would freeze the whole tray. The
/// caller has already disabled the menu item and set the in-flight guard;
/// delivers the resulting checked state back via `UserEvent::AutoLaunch`, which
/// clears the guard and re-enables the item.
fn spawn_toggle_autolaunch(al: auto_launch::AutoLaunch, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let was_enabled = al.is_enabled().unwrap_or(false);
        let result = if was_enabled { al.disable() } else { al.enable() };
        let new_state = match result {
            Ok(()) => !was_enabled,
            Err(error) => {
                tracing::error!(%error, "toggle start-at-login failed");
                // Best-effort: report the real current state; fall back to the
                // pre-click value if even that probe fails.
                al.is_enabled().unwrap_or(was_enabled)
            }
        };
        let _ = proxy.send_event(UserEvent::AutoLaunch(new_state));
    });
}
