#![cfg_attr(windows, windows_subsystem = "windows")]

mod apimock;
mod manager;
mod server;

use manager::AppManager;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    // Mark the process as Per-Monitor-V2 DPI-aware BEFORE any GUI / dialog
    // code runs. Without this, native pickers spawned by `rfd` (Open Folder,
    // Open File) are bitmap-stretched by Windows on high-DPI displays and
    // render visibly blurry. Must be done before the first dialog opens.
    #[cfg(windows)]
    enable_dpi_awareness();

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let rt_handle = rt.handle().clone();

    let manager = Arc::new(AppManager::new(rt_handle.clone()));
    manager.load();

    // Pre-warm `npm prefix -g` on a background thread so the first start_app
    // call doesn't have to block the tokio executor on a synchronous npm
    // shell-out (it can take 100–500 ms cold).
    manager::prewarm_npm_prefix();

    let mgr = manager.clone();
    std::thread::spawn(move || {
        rt.block_on(server::run(mgr));
    });

    let mgr = manager.clone();
    rt_handle.spawn(async move {
        mgr.auto_start_all().await;
    });

    // Give the server a brief moment to bind before launching the browser.
    std::thread::sleep(Duration::from_millis(250));
    // The in-app updater restarts us with APPNEST_NO_OPEN=1 so we don't
    // pop a second browser tab on top of the one the user already has open
    // (which is polling and will reload itself once we're reachable).
    if std::env::var_os("APPNEST_NO_OPEN").is_none() {
        let _ = open::that("http://localhost:1234");
    }

    #[cfg(target_os = "windows")]
    run_with_tray(manager, rt_handle);

    #[cfg(not(target_os = "windows"))]
    run_headless(manager);
}

// ───────────────────────────── Windows: system tray ─────────────────────────────
#[cfg(windows)]
fn enable_dpi_awareness() {
    // Prefer Per-Monitor-V2 (Win10 1703+). Falls back silently on older
    // builds; in that case the dialog remains slightly soft but at least
    // doesn't crash. We deliberately don't ship a side-by-side manifest
    // for this — calling SetProcessDpiAwarenessContext at startup is
    // equivalent and avoids the build-system complexity.
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
#[cfg(target_os = "windows")]
fn run_with_tray(manager: Arc<AppManager>, rt_handle: tokio::runtime::Handle) {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::TrayIconBuilder;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    let menu = Menu::new();
    let mi_open = MenuItem::new("Open Dashboard", true, None);
    let mi_start_all = MenuItem::new("Start All Apps", true, None);
    let mi_stop_all = MenuItem::new("Stop All Apps", true, None);
    let mi_quit = MenuItem::new("Quit AppNest", true, None);
    menu.append_items(&[
        &mi_open,
        &PredefinedMenuItem::separator(),
        &mi_start_all,
        &mi_stop_all,
        &PredefinedMenuItem::separator(),
        &mi_quit,
    ])
    .unwrap();

    let icon = create_tray_icon();
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("AppNest — Local Dev Manager")
        .with_icon(icon)
        .build()
        .expect("Failed to create tray icon");

    let menu_channel = MenuEvent::receiver();

    loop {
        unsafe {
            let mut msg = std::mem::zeroed();
            let ret = PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE);
            if ret != 0 {
                if msg.message == WM_QUIT {
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        while let Ok(event) = menu_channel.try_recv() {
            if event.id == mi_open.id().clone() {
                let _ = open::that("http://localhost:1234");
            } else if event.id == mi_start_all.id().clone() {
                let mgr = manager.clone();
                rt_handle.spawn(async move { mgr.start_all().await });
            } else if event.id == mi_stop_all.id().clone() {
                manager.stop_all();
            } else if event.id == mi_quit.id().clone() {
                manager.stop_all();
                unsafe {
                    PostQuitMessage(0);
                }
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "windows")]
fn create_tray_icon() -> tray_icon::Icon {
    // Renders the same Feather "settings" gear used as the web favicon
    // (outlined gear + center circle, indigo #6366f1, transparent bg) so
    // the tray / launcher icon matches the in-app branding.
    let s: u32 = 32;
    let center = (s as f32 - 1.0) / 2.0;
    let mut rgba = vec![0u8; (s * s * 4) as usize];

    // Gear geometry (in pixel units, 32×32 canvas):
    //   r(θ) = base + amp * pulse(8θ)   — 8 teeth, smoothed square wave
    let base_r = 10.5f32;       // outer radius between teeth
    let tooth_amp = 2.0f32;     // tooth height
    let stroke_half = 0.9f32;   // half stroke width
    let hub_r = 4.0f32;         // center circle radius (3/24 * 32 ≈ 4)

    // 4×4 supersampling for smooth edges.
    const SS: u32 = 4;
    let ss_f = SS as f32;
    let samples = (SS * SS) as f32;

    for y in 0..s {
        for x in 0..s {
            let mut coverage = 0.0f32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / ss_f - 0.5 - center;
                    let py = y as f32 + (sy as f32 + 0.5) / ss_f - 0.5 - center;
                    let dist = (px * px + py * py).sqrt();
                    let angle = py.atan2(px);

                    // Smoothed square wave with 8 cycles (8 teeth), in [0, 1].
                    let raw = (angle * 8.0).cos();
                    let pulse = smoothstep(-0.25, 0.25, raw);
                    let curve_r = base_r + tooth_amp * pulse;

                    // Distance from the gear outline (signed → unsigned).
                    let d_gear = (dist - curve_r).abs();
                    // Distance from the center circle outline.
                    let d_hub = (dist - hub_r).abs();
                    let d = d_gear.min(d_hub);

                    if d <= stroke_half + 0.5 {
                        // Linear edge falloff for AA.
                        let cov = (stroke_half + 0.5 - d).clamp(0.0, 1.0);
                        coverage += cov;
                    }
                }
            }
            let alpha = (coverage / samples * 255.0).clamp(0.0, 255.0) as u8;
            if alpha > 0 {
                let i = ((y * s + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[99, 102, 241, alpha]);
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, s, s).expect("Failed to create icon")
}

#[cfg(target_os = "windows")]
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ───────────────────────────── macOS / Linux: headless ─────────────────────────────

#[cfg(not(target_os = "windows"))]
fn run_headless(manager: Arc<AppManager>) {
    // Block until SIGINT / SIGTERM, then stop all managed processes so we don't
    // leave dev servers orphaned.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create signal runtime");

    rt.block_on(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
            let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            tokio::select! {
                _ = sigint.recv() => {}
                _ = sigterm.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    });

    eprintln!("AppNest: shutting down, stopping managed apps…");
    manager.stop_all();
}
