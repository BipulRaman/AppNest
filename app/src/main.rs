#![cfg_attr(windows, windows_subsystem = "windows")]

mod manager;
mod server;

use manager::AppManager;
use std::sync::Arc;
use std::time::Duration;

fn main() {
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
    let _ = open::that("http://localhost:1234");

    #[cfg(target_os = "windows")]
    run_with_tray(manager, rt_handle);

    #[cfg(not(target_os = "windows"))]
    run_headless(manager);
}

// ───────────────────────────── Windows: system tray ─────────────────────────────

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
    let s = 32u32;
    let mut rgba = vec![0u8; (s * s * 4) as usize];
    let c = 15.5f32;

    for y in 0..s {
        for x in 0..s {
            let i = ((y * s + x) * 4) as usize;
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let dist = (dx * dx + dy * dy).sqrt();
            let angle = dy.atan2(dx);

            // Gear: ring with 8 teeth + center hub
            let tooth = (angle * 4.0).cos(); // 8 teeth
            let outer = 13.0 + tooth.max(0.0) * 2.5;
            let inner = 8.0;
            let hub = 4.0;

            let in_shape = (dist <= outer && dist >= inner) || dist <= hub;
            if in_shape {
                let edge = if dist >= inner {
                    (outer - dist).min(dist - inner)
                } else {
                    hub - dist
                };
                let a = (edge.clamp(0.0, 1.5) / 1.5 * 255.0) as u8;
                rgba[i..i + 4].copy_from_slice(&[99, 102, 241, a]);
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, s, s).expect("Failed to create icon")
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
