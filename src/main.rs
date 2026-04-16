#![windows_subsystem = "windows"]

mod manager;
mod server;

use manager::AppManager;
use std::sync::Arc;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let rt_handle = rt.handle().clone();

    let manager = Arc::new(AppManager::new(rt_handle.clone()));
    manager.load();

    let mgr = manager.clone();
    std::thread::spawn(move || {
        rt.block_on(server::run(mgr));
    });

    let mgr = manager.clone();
    rt_handle.spawn(async move {
        mgr.auto_start_all().await;
    });

    let menu = Menu::new();
    let mi_open = MenuItem::new("Open Dashboard", true, None);
    let mi_start_all = MenuItem::new("Start All Apps", true, None);
    let mi_stop_all = MenuItem::new("Stop All Apps", true, None);
    let mi_quit = MenuItem::new("Quit MyServers", true, None);
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
        .with_tooltip("MyServers — Local CI/CD")
        .with_icon(icon)
        .build()
        .expect("Failed to create tray icon");

    let _ = open::that("http://localhost:1234");

    let menu_channel = MenuEvent::receiver();

    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
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

    #[cfg(not(target_os = "windows"))]
    {
        loop {
            match menu_channel.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    if event.id == mi_open.id().clone() {
                        let _ = open::that("http://localhost:1234");
                    } else if event.id == mi_start_all.id().clone() {
                        let mgr = manager.clone();
                        rt_handle.spawn(async move { mgr.start_all().await });
                    } else if event.id == mi_stop_all.id().clone() {
                        manager.stop_all();
                    } else if event.id == mi_quit.id().clone() {
                        manager.stop_all();
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
    }
}

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
