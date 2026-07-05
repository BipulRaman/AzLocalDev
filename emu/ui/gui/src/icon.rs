//! Generates the app's icon by rasterizing an embedded SVG via `resvg`, so we don't need to
//! ship/maintain a separate .ico/.png asset file, and the design itself is a normal,
//! easy-to-edit SVG rather than hand-rolled pixel geometry. Used for the system tray icon.
//!
//! The SVG lives at `emu/ui/web/assets/icon.svg` (not a local copy) so the tray icon and the
//! web dashboard's favicon/brand mark are always rendering the exact same artwork - edit
//! that one file to change the design everywhere at once.

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{self, Tree};

/// The icon's source SVG (viewBox `0 0 100 100`). See the module doc comment for why this
/// reaches into the `emu-web` crate's asset folder instead of keeping a local copy.
const SVG_SOURCE: &str = include_str!("../../web/assets/icon.svg");

/// Renders the icon as raw RGBA8 pixels (straight, non-premultiplied alpha - what
/// `tray_icon::Icon::from_rgba` expects). Returns `(rgba, size)`.
pub fn render_rgba(size: u32) -> (Vec<u8>, u32) {
    let tree = Tree::from_str(SVG_SOURCE, &usvg::Options::default()).expect("valid icon.svg");
    let tree_size = tree.size();

    let mut pixmap = Pixmap::new(size, size).expect("non-zero icon size");
    let transform = Transform::from_scale(
        size as f32 / tree_size.width(),
        size as f32 / tree_size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    (unpremultiply(pixmap.data()), size)
}

/// `tiny_skia::Pixmap` stores premultiplied alpha; convert to straight alpha so downstream
/// consumers (tray/window icon APIs) get correct, unmodified colors at semi-transparent
/// (anti-aliased) edge pixels.
fn unpremultiply(premultiplied: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(premultiplied.len());
    for px in premultiplied.chunks_exact(4) {
        let a = px[3];
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let unmul = |c: u8| ((c as u32 * 255) / a as u32).min(255) as u8;
            out.push(unmul(px[0]));
            out.push(unmul(px[1]));
            out.push(unmul(px[2]));
            out.push(a);
        }
    }
    out
}
