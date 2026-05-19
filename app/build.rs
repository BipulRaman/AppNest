// Generates an .ico file containing the same Feather "settings" gear used
// as the in-app favicon/tray icon, then (on Windows) embeds it into the
// executable so Explorer / Start menu / taskbar show the matching icon.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico_path = out_dir.join("appnest.ico");

    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in &[16u32, 32, 48, 64, 128, 256] {
        let rgba = render_gear_rgba(size);
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        icon_dir.add_entry(
            ico::IconDirEntry::encode(&image).expect("encode ico entry"),
        );
    }
    let file = std::fs::File::create(&ico_path).expect("create ico file");
    icon_dir.write(file).expect("write ico file");

    #[cfg(target_os = "windows")]
    {
        let rc_path = out_dir.join("appnest.rc");
        // Use forward slashes — the RC compiler accepts them and it
        // avoids escaping headaches on Windows paths.
        let ico_for_rc = ico_path.to_string_lossy().replace('\\', "/");
        std::fs::write(
            &rc_path,
            format!("IDI_ICON1 ICON \"{}\"\n", ico_for_rc),
        )
        .expect("write rc file");
        embed_resource::compile(&rc_path, embed_resource::NONE);
    }
}

/// Renders the gear (matches `create_tray_icon` in src/main.rs) at the
/// requested square size, with 4×4 supersampled antialiasing. Indigo
/// (#6366f1) strokes on a fully transparent background.
fn render_gear_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let center = (s - 1.0) / 2.0;
    let scale = s / 32.0;

    let base_r = 10.5 * scale;
    let tooth_amp = 2.0 * scale;
    let stroke_half = 0.9 * scale;
    let hub_r = 4.0 * scale;

    const SS: u32 = 4;
    let ss_f = SS as f32;
    let samples = (SS * SS) as f32;

    let mut rgba = vec![0u8; (size * size * 4) as usize];

    for y in 0..size {
        for x in 0..size {
            let mut coverage = 0.0f32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / ss_f - 0.5 - center;
                    let py = y as f32 + (sy as f32 + 0.5) / ss_f - 0.5 - center;
                    let dist = (px * px + py * py).sqrt();
                    let angle = py.atan2(px);

                    let raw = (angle * 8.0).cos();
                    let pulse = smoothstep(-0.25, 0.25, raw);
                    let curve_r = base_r + tooth_amp * pulse;

                    let d_gear = (dist - curve_r).abs();
                    let d_hub = (dist - hub_r).abs();
                    let d = d_gear.min(d_hub);

                    if d <= stroke_half + 0.5 {
                        coverage += (stroke_half + 0.5 - d).clamp(0.0, 1.0);
                    }
                }
            }
            let alpha = (coverage / samples * 255.0).clamp(0.0, 255.0) as u8;
            if alpha > 0 {
                let i = ((y * size + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[99, 102, 241, alpha]);
            }
        }
    }
    rgba
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
