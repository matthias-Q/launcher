//! A blurred copy of the desktop to draw behind the launcher (X11), captured exactly like the
//! Freya launch. The capture is drawn under the panel and the panel's `backdrop_blur` blurs it
//! on the GPU; the transparent window lets the real desktop stay sharp around the panel. The
//! original's CPU box-blur is not ported because rune blurs for free.
//!
//! `capture` must run before the window maps, or it captures the launcher itself.

use std::error::Error;

use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{ConnectionExt as _, ImageFormat},
    },
};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// The capture is shrunk by this factor; the blur fills in the detail it loses.
pub const DOWNSCALE: u32 = 4;

/// A monitor in physical root window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Monitor {
    fn contains(&self, x: i32, y: i32) -> bool {
        (self.x..self.x + self.width as i32).contains(&x)
            && (self.y..self.y + self.height as i32).contains(&y)
    }
}

/// One monitor's desktop as RGBA pixels at 1/[`DOWNSCALE`] of its size.
pub struct Backdrop {
    pub monitor: Monitor,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The monitor under the pointer, where window managers usually open new windows.
pub fn pointer_monitor() -> Result<Monitor> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let pointer = conn.query_pointer(root)?.reply()?;
    let monitors: Vec<Monitor> = conn
        .randr_get_monitors(root, true)?
        .reply()?
        .monitors
        .iter()
        .map(|monitor| Monitor {
            x: monitor.x as i32,
            y: monitor.y as i32,
            width: monitor.width as u32,
            height: monitor.height as u32,
        })
        .collect();
    monitors
        .iter()
        .find(|monitor| monitor.contains(pointer.root_x as i32, pointer.root_y as i32))
        .or(monitors.first())
        .copied()
        .ok_or_else(|| "no monitors".into())
}

/// Captures the monitor. Must run before our window is mapped, or it captures itself.
pub fn capture(monitor: Monitor) -> Result<Backdrop> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            root,
            monitor.x as i16,
            monitor.y as i16,
            monitor.width as u16,
            monitor.height as u16,
            u32::MAX,
        )?
        .reply()?;
    // 24 and 32 bit visuals store pixels as little endian BGRX.
    if !matches!(reply.depth, 24 | 32)
        || reply.data.len() < (monitor.width * monitor.height * 4) as usize
    {
        return Err(format!("unsupported root image of depth {}", reply.depth).into());
    }
    let (width, height, rgba) =
        downscale_bgrx(&reply.data, monitor.width, monitor.height, DOWNSCALE);
    Ok(Backdrop {
        monitor,
        width,
        height,
        rgba,
    })
}

/// Averages `factor`×`factor` blocks of BGRX pixels into opaque RGBA.
fn downscale_bgrx(data: &[u8], width: u32, height: u32, factor: u32) -> (u32, u32, Vec<u8>) {
    let factor = factor.max(1);
    let (small_width, small_height) = (width.div_ceil(factor), height.div_ceil(factor));
    let mut rgba = Vec::with_capacity((small_width * small_height * 4) as usize);
    for sy in 0..small_height {
        for sx in 0..small_width {
            let mut sum = [0u32; 3];
            let mut count = 0;
            for y in sy * factor..((sy + 1) * factor).min(height) {
                for x in sx * factor..((sx + 1) * factor).min(width) {
                    let offset = ((y * width + x) * 4) as usize;
                    for (channel, value) in sum.iter_mut().zip(&data[offset..offset + 3]) {
                        *channel += *value as u32;
                    }
                    count += 1;
                }
            }
            let [b, g, r] = sum.map(|channel| (channel / count) as u8);
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    (small_width, small_height, rgba)
}

/// A synthetic desktop for the `glass` scene: deterministic, so `--scene glass` renders
/// identically on every machine (headless runs never capture the real desktop).
pub fn synthetic() -> (u32, u32, Vec<u8>) {
    let (width, height) = (480u32, 270u32);
    // Three "desktop windows" over a diagonal gradient, so the blur is clearly visible.
    let windows: [((u32, u32), (u32, u32), (f32, f32, f32)); 3] = [
        ((40, 40), (150, 90), (0.85, 0.35, 0.33)),
        ((260, 70), (180, 120), (0.32, 0.62, 0.40)),
        ((90, 170), (340, 80), (0.35, 0.45, 0.82)),
    ];
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let t = (x + y) as f32 / (width + height) as f32;
            let mut px = [
                (30. + 90. * t) as u8,
                (40. + 60. * t) as u8,
                (80. + 100. * t) as u8,
            ];
            for (origin, size, (r, g, b)) in windows {
                if x >= origin.0 && x < origin.0 + size.0 && y >= origin.1 && y < origin.1 + size.1
                {
                    px = [(r * 255.) as u8, (g * 255.) as u8, (b * 255.) as u8];
                }
            }
            rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    (width, height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgrx_downscales_to_opaque_rgba() {
        // 8x4 BGRX that is red in the top half and blue in the bottom half.
        let width = 8u32;
        let height = 4u32;
        let mut bgrx = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                let (b, g, r) = if y < 2 { (0, 0, 255) } else { (255, 0, 0) };
                bgrx[i..i + 4].copy_from_slice(&[b, g, r, 0]);
            }
        }
        let (w, h, rgba) = downscale_bgrx(&bgrx, width, height, 2);
        assert_eq!((w, h), (4, 2));
        let top: Vec<_> = rgba[..w as usize * 4].to_vec();
        let bottom = &rgba[w as usize * 4..];
        assert!(top.chunks(4).all(|p| p == [255, 0, 0, 255]), "blue half becomes red: {top:?}");
        assert!(bottom.chunks(4).all(|p| p == [0, 0, 255, 255]), "red half becomes blue: {bottom:?}");
    }

    #[test]
    fn synthetic_is_sized_and_colorful() {
        let (w, h, rgba) = synthetic();
        assert_eq!((w, h), (480, 270));
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        let mut distinct = std::collections::HashSet::new();
        for px in rgba.chunks(4) {
            distinct.insert([px[0], px[1], px[2], px[3]]);
        }
        assert!(distinct.len() > 10, "all one flat color: {}", distinct.len());
    }
}
