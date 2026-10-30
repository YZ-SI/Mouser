//! Device and app images, embedded in the exe and decoded only when shown (one device image
//! lives in memory at a time). Artwork from Mouser (MIT) `images/`; see assets/MOUSER-LICENSE.txt.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

static ASSETS: &[(&str, &[u8])] = &[
    ("mouse.png", include_bytes!("../assets/images/mouse.png")),
    ("mouse_mx_anywhere_3s.png", include_bytes!("../assets/images/mouse_mx_anywhere_3s.png")),
    ("mx_vertical.png", include_bytes!("../assets/images/mx_vertical.png")),
    ("logitech-mice/m585_m590/mouse.png", include_bytes!("../assets/images/logitech-mice/m585_m590/mouse.png")),
    ("logitech-mice/m720_triathlon/mouse.svg", include_bytes!("../assets/images/logitech-mice/m720_triathlon/mouse.svg")),
    ("logitech-mice/mx_anywhere_2s/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_anywhere_2s/mouse.png")),
    ("logitech-mice/mx_anywhere_3/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_anywhere_3/mouse.png")),
    ("logitech-mice/mx_anywhere_3s/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_anywhere_3s/mouse.png")),
    ("logitech-mice/mx_master/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_master/mouse.png")),
    ("logitech-mice/mx_master_2s/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_master_2s/mouse.png")),
    ("logitech-mice/mx_master_3/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_master_3/mouse.png")),
    ("logitech-mice/mx_master_3s/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_master_3s/mouse.png")),
    ("logitech-mice/mx_master_4/mouse.png", include_bytes!("../assets/images/logitech-mice/mx_master_4/mouse.png")),
    ("icons/mouse-simple.svg", include_bytes!("../assets/icons/mouse-simple.svg")),
    ("chrom.png", include_bytes!("../assets/images/chrom.png")),
    // Mouser's `media.webp` (legacy icon of Windows Media Player), converted once to PNG.
    ("media.png", include_bytes!("../assets/images/media.png")),
    ("VLC.png", include_bytes!("../assets/images/VLC.png")),
    ("VSCODE.png", include_bytes!("../assets/images/VSCODE.png")),
];

/// Largest side kept in memory; the view shows images at <= 460 px so 2x covers HiDPI.
const MAX_SIDE: u32 = 960;

/// The catalog names the legacy `.webp` icon; it ships as PNG.
fn find(asset: &str) -> Option<&'static [u8]> {
    let asset = asset.strip_suffix(".webp").map_or_else(|| asset.to_string(), |a| format!("{a}.png"));
    ASSETS.iter().find(|(n, _)| n.eq_ignore_ascii_case(&asset)).map(|(_, b)| *b)
}

/// Decode `asset` (a path relative to Mouser's `images/`), downscaling big photos.
pub fn load(asset: &str) -> Option<Image> {
    let bytes = find(asset)?;
    if asset.ends_with(".svg") {
        return Image::load_from_svg_data(bytes).ok();
    }
    decode_png(bytes, MAX_SIDE)
}

/// 48 px-bounded decode for list icons (profile rows, app picker).
pub fn load_icon(asset: &str) -> Option<Image> {
    decode_png(find(asset)?, 48)
}

/// Decode a PNG to RGBA8, box-averaging down by an integer factor so no side exceeds `max_side`.
pub fn decode_png(bytes: &[u8], max_side: u32) -> Option<Image> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut raw = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut raw).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let ch = info.color_type.samples();
    let px = |x: usize, y: usize| -> [u32; 4] {
        let i = (y * w + x) * ch;
        match info.color_type {
            png::ColorType::Rgba => [raw[i] as u32, raw[i + 1] as u32, raw[i + 2] as u32, raw[i + 3] as u32],
            png::ColorType::Rgb => [raw[i] as u32, raw[i + 1] as u32, raw[i + 2] as u32, 255],
            png::ColorType::GrayscaleAlpha => [raw[i] as u32, raw[i] as u32, raw[i] as u32, raw[i + 1] as u32],
            _ => [raw[i] as u32, raw[i] as u32, raw[i] as u32, 255],
        }
    };
    let f = (w.max(h) as u32).div_ceil(max_side).max(1) as usize;
    let (ow, oh) = (w / f, h / f);
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(ow as u32, oh as u32);
    for (o, dst) in buf.make_mut_slice().iter_mut().enumerate() {
        let (ox, oy) = (o % ow, o / ow);
        let mut acc = [0u32; 4];
        for dy in 0..f {
            for dx in 0..f {
                let p = px(ox * f + dx, oy * f + dy);
                // premultiplied accumulation so transparent pixels do not bleed colour
                for c in 0..3 {
                    acc[c] += p[c] * p[3];
                }
                acc[3] += p[3];
            }
        }
        let n = (f * f) as u32;
        let a = acc[3];
        *dst = match (acc[0].checked_div(a), acc[1].checked_div(a), acc[2].checked_div(a)) {
            (Some(r), Some(g), Some(bl)) => Rgba8Pixel::new(r as u8, g as u8, bl as u8, (a / n) as u8),
            _ => Rgba8Pixel::new(0, 0, 0, 0),
        };
    }
    Some(Image::from_rgba8(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_asset_decodes() {
        for (name, _) in ASSETS {
            let img = load(name).unwrap_or_else(|| panic!("cannot decode {name}"));
            let s = img.size();
            assert!(s.width > 0 && s.height > 0, "{name}");
            assert!(s.width.max(s.height) <= MAX_SIDE || name.ends_with(".svg"), "{name} {s:?}");
        }
    }

    #[test]
    fn legacy_webp_name_maps_to_the_png() {
        assert!(load_icon("media.webp").is_some());
    }

    #[test]
    fn unknown_asset_is_none() {
        assert!(load("nope.png").is_none());
    }

    #[test]
    fn every_layout_asset_is_embedded() {
        for l in mouser_core::devices::DEVICE_LAYOUTS.iter() {
            assert!(ASSETS.iter().any(|(n, _)| *n == l.image_asset), "layout {} asset {}", l.key, l.image_asset);
        }
    }
}
