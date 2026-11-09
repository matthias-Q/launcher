//! A blurred copy of the desktop to draw behind the launcher (X11).

use std::error::Error;

use freya::prelude::Bytes;
use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{ConnectionExt as _, ImageFormat},
    },
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// The capture is shrunk by this factor; it gets blurred anyway.
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

impl Backdrop {
    /// Gaussian-like blur with `sigma` in physical pixels.
    pub fn blur(&mut self, sigma: f32) {
        blur_rgba(
            &mut self.rgba,
            self.width as usize,
            self.height as usize,
            sigma / DOWNSCALE as f32,
        );
    }

    pub fn bytes(&self) -> Bytes {
        Bytes::from(self.rgba.clone())
    }
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

/// Three box blurs per axis, which closely approximate a Gaussian blur.
fn blur_rgba(rgba: &mut [u8], width: usize, height: usize, sigma: f32) {
    if sigma <= 0. || width == 0 || height == 0 {
        return;
    }
    let mut scratch = rgba.to_vec();
    for radius in box_radii(sigma) {
        box_blur(rgba, &mut scratch, width, height, radius, 4, width * 4);
        box_blur(&scratch, rgba, height, width, radius, width * 4, 4);
    }
}

/// Box radii for three passes whose combined blur matches `sigma`.
fn box_radii(sigma: f32) -> [usize; 3] {
    let passes = 3.;
    let ideal = (12. * sigma * sigma / passes + 1.).sqrt();
    let mut lower = ideal.floor() as i32;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let upper = lower + 2;
    let lower_f = lower as f32;
    let lower_count =
        ((12. * sigma * sigma - passes * lower_f * lower_f - 4. * passes * lower_f - 3. * passes)
            / (-4. * lower_f - 4.))
            .round() as i32;
    std::array::from_fn(|pass| {
        let size = if (pass as i32) < lower_count {
            lower
        } else {
            upper
        };
        (size.max(1) as usize - 1) / 2
    })
}

/// Blurs `lines` lines of `length` pixels each, reading `src` and writing `dst`. `step` is the
/// byte distance between neighbors on a line and `stride` between line starts, so the same
/// code blurs rows and columns. Edges repeat the outermost pixel.
fn box_blur(
    src: &[u8],
    dst: &mut [u8],
    length: usize,
    lines: usize,
    radius: usize,
    step: usize,
    stride: usize,
) {
    let window = (2 * radius + 1) as u32;
    let last = length as isize - 1;
    for line in 0..lines {
        let base = line * stride;
        let at = |index: isize, channel: usize| {
            src[base + index.clamp(0, last) as usize * step + channel] as u32
        };
        for channel in 0..3 {
            let mut sum: u32 = (-(radius as isize)..=radius as isize)
                .map(|index| at(index, channel))
                .sum();
            for index in 0..length as isize {
                dst[base + index as usize * step + channel] = ((sum + window / 2) / window) as u8;
                sum = sum + at(index + radius as isize + 1, channel)
                    - at(index - radius as isize, channel);
            }
        }
        for index in 0..length {
            dst[base + index * step + 3] = src[base + index * step + 3];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscales_bgrx_to_rgba_by_averaging_blocks() {
        // 3x2 BGRX pixels: a 2x2 block of blue-ish pixels, then a red column.
        let blue = [200, 0, 0, 0];
        let red = [0, 0, 100, 0];
        let data: Vec<u8> = [blue, blue, red, blue, blue, red].concat();
        let (width, height, rgba) = downscale_bgrx(&data, 3, 2, 2);
        assert_eq!((width, height), (2, 1));
        assert_eq!(rgba, [0, 0, 200, 255, 100, 0, 0, 255]);
    }

    fn red_channel(rgba: &[u8]) -> Vec<u8> {
        rgba.chunks(4).map(|pixel| pixel[0]).collect()
    }

    #[test]
    fn blur_keeps_flat_areas_and_softens_edges() {
        let (width, height) = (16, 3);
        let mut rgba: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                if i % width < width / 2 {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                }
            })
            .collect();
        blur_rgba(&mut rgba, width, height, 1.5);
        let row = &red_channel(&rgba)[width..2 * width];
        assert_eq!(row[0], 0);
        assert_eq!(row[width - 1], 255);
        assert!(row[width / 2 - 1] > 40 && row[width / 2] < 215, "{row:?}");
        assert!(row.windows(2).all(|pair| pair[0] <= pair[1]), "{row:?}");
        assert!(rgba.chunks(4).all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn box_radii_grow_with_sigma() {
        assert_eq!(box_radii(0.5), [0, 0, 0]);
        let small: usize = box_radii(2.).iter().sum();
        let large: usize = box_radii(8.).iter().sum();
        assert!(small < large);
    }
}
