use crate::ast::{Arrow, Shape};
use crate::layout::{Layout, LayoutEdge, LayoutNode};
use std::io::Write;

const SCALE: f32 = 2.0;
const BG: [u8; 4] = [255, 255, 255, 255];
const FG: [u8; 4] = [0, 0, 0, 255];
const EDGE_CLR: [u8; 4] = [60, 60, 60, 255];
const LABEL_CLR: [u8; 4] = [80, 80, 80, 255];

pub fn is_supported() -> bool {
    std::env::var("KITTY_WINDOW_ID").is_ok()
        || std::env::var("TERM").ok().as_deref() == Some("xterm-kitty")
}

pub fn render_to_rgba(layout: &Layout) -> (Vec<u8>, usize, usize) {
    let pw = (layout.width * SCALE).ceil() as usize + 1;
    let ph = (layout.height * SCALE).ceil() as usize + 1;
    let mut canvas = Canvas::new(pw, ph);
    for e in &layout.edges {
        draw_edge(&mut canvas, e);
    }
    for n in &layout.nodes {
        draw_node(&mut canvas, n);
    }
    (canvas.pixels, pw, ph)
}

pub fn display(layout: &Layout) {
    let (pixels, pw, ph) = render_to_rgba(layout);
    let b64 = base64_encode(&pixels);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    transmit(pw, ph, None, &b64, &mut out);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

pub fn display_in_pane(
    rgba: &[u8],
    pixel_w: usize,
    pixel_h: usize,
    cols: u16,
    rows: u16,
    out: &mut impl Write,
) {
    let b64 = base64_encode(rgba);
    transmit(pixel_w, pixel_h, Some((cols, rows)), &b64, out);
    let _ = out.flush();
}

pub fn delete_all(out: &mut impl Write) {
    let _ = out.write_all(b"\x1b_Ga=d,d=A\x1b\\");
    let _ = out.flush();
}

fn draw_node(canvas: &mut Canvas, n: &LayoutNode) {
    let x = (n.x * SCALE) as i32;
    let y = (n.y * SCALE) as i32;
    let w = (n.w * SCALE) as i32;
    let h = (n.h * SCALE) as i32;

    match &n.shape {
        Shape::Diamond => {
            let cx = x + w / 2;
            let cy = y + h / 2;
            draw_diamond(canvas, cx, cy, w / 2, h / 2);
        }
        _ => {
            let radius = match &n.shape {
                Shape::Round | Shape::Stadium => 6,
                _ => 0,
            };
            canvas.fill_rect(x, y, w, h, BG);
            canvas.stroke_rect(x, y, w, h, FG, 2, radius);
            if matches!(&n.shape, Shape::Sub) {
                let inner = (4.0 * SCALE) as i32;
                canvas.draw_vline(x + inner, y, y + h, FG, 1);
                canvas.draw_vline(x + w - inner, y, y + h, FG, 1);
            }
        }
    }

    let label_x = x + w / 2;
    let label_y = y + h / 2 - FONT_H as i32;
    canvas.draw_text_centered(label_x, label_y, &n.label, FG);
}

fn draw_edge(canvas: &mut Canvas, e: &LayoutEdge) {
    if e.points.len() < 2 {
        return;
    }
    let color = match &e.arrow {
        Arrow::Thick => [40, 40, 40, 255],
        Arrow::Dotted => [120, 120, 120, 255],
        _ => EDGE_CLR,
    };
    let thickness = if matches!(&e.arrow, Arrow::Thick) {
        3
    } else {
        1
    };
    let dashed = matches!(&e.arrow, Arrow::Dotted);

    for i in 0..e.points.len() - 1 {
        let (x1, y1) = e.points[i];
        let (x2, y2) = e.points[i + 1];
        let x1 = (x1 * SCALE) as i32;
        let y1 = (y1 * SCALE) as i32;
        let x2 = (x2 * SCALE) as i32;
        let y2 = (y2 * SCALE) as i32;
        if dashed {
            canvas.draw_dashed_line(x1, y1, x2, y2, color, thickness);
        } else {
            canvas.draw_line(x1, y1, x2, y2, color, thickness);
        }
    }

    // Arrowhead at last point
    let n = e.points.len();
    let (ax, ay) = e.points[n - 2];
    let (bx, by) = e.points[n - 1];
    let ax = (ax * SCALE) as i32;
    let ay = (ay * SCALE) as i32;
    let bx = (bx * SCALE) as i32;
    let by = (by * SCALE) as i32;
    draw_arrowhead(canvas, ax, ay, bx, by, &e.arrow, color);

    if let Some(lbl) = &e.label {
        let (lx, ly) = e.label_pos;
        let lx = (lx * SCALE) as i32;
        let ly = (ly * SCALE) as i32;
        canvas.draw_text_centered(lx, ly, lbl, LABEL_CLR);
    }
}

fn draw_arrowhead(
    canvas: &mut Canvas,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    arrow: &Arrow,
    color: [u8; 4],
) {
    match arrow {
        Arrow::Circle => {
            canvas.draw_circle(x2, y2, 5, color);
            return;
        }
        Arrow::Cross => {
            canvas.draw_line(x2 - 4, y2 - 4, x2 + 4, y2 + 4, color, 1);
            canvas.draw_line(x2 + 4, y2 - 4, x2 - 4, y2 + 4, color, 1);
            return;
        }
        _ => {}
    }
    let dx = (x2 - x1) as f32;
    let dy = (y2 - y1) as f32;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / len;
    let uy = dy / len;
    let size = 8.0_f32;
    let px1 = (x2 as f32 - size * ux + size * 0.5 * uy) as i32;
    let py1 = (y2 as f32 - size * uy - size * 0.5 * ux) as i32;
    let px2 = (x2 as f32 - size * ux - size * 0.5 * uy) as i32;
    let py2 = (y2 as f32 - size * uy + size * 0.5 * ux) as i32;
    canvas.draw_line(x2, y2, px1, py1, color, 1);
    canvas.draw_line(x2, y2, px2, py2, color, 1);
}

fn draw_diamond(canvas: &mut Canvas, cx: i32, cy: i32, rx: i32, ry: i32) {
    canvas.draw_line(cx, cy - ry, cx + rx, cy, FG, 1);
    canvas.draw_line(cx + rx, cy, cx, cy + ry, FG, 1);
    canvas.draw_line(cx, cy + ry, cx - rx, cy, FG, 1);
    canvas.draw_line(cx - rx, cy, cx, cy - ry, FG, 1);
}

// ---- Canvas ----------------------------------------------------------------

struct Canvas {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        let mut pixels = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            pixels.extend_from_slice(&BG);
        }
        Canvas {
            pixels,
            width: w,
            height: h,
        }
    }

    fn set(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        let base = (y * self.width + x) * 4;
        self.pixels[base..base + 4].copy_from_slice(&color);
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) {
        for dy in 0..h {
            for dx in 0..w {
                self.set(x + dx, y + dy, color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke_rect(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: [u8; 4],
        thickness: i32,
        radius: i32,
    ) {
        if radius == 0 {
            for t in 0..thickness {
                for dx in 0..w {
                    self.set(x + dx, y + t, color);
                    self.set(x + dx, y + h - 1 - t, color);
                }
                for dy in 0..h {
                    self.set(x + t, y + dy, color);
                    self.set(x + w - 1 - t, y + dy, color);
                }
            }
        } else {
            // Draw with rounded corners using circle arcs
            let r = radius.min(w / 2).min(h / 2);
            for t in 0..thickness {
                let ri = r - t;
                // Straight segments
                for dx in r..w - r {
                    self.set(x + dx, y + t, color);
                    self.set(x + dx, y + h - 1 - t, color);
                }
                for dy in r..h - r {
                    self.set(x + t, y + dy, color);
                    self.set(x + w - 1 - t, y + dy, color);
                }
                // Corner arcs (Bresenham circle, one quadrant each)
                draw_arc(self, x + r, y + r, ri, 3, color); // top-left
                draw_arc(self, x + w - r - 1, y + r, ri, 0, color); // top-right
                draw_arc(self, x + r, y + h - r - 1, ri, 2, color); // bottom-left
                draw_arc(self, x + w - r - 1, y + h - r - 1, ri, 1, color); // bottom-right
            }
        }
    }

    fn draw_line(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, color: [u8; 4], thickness: i32) {
        let dx = (x2 - x1).abs();
        let dy = (y2 - y1).abs();
        let sx: i32 = if x1 < x2 { 1 } else { -1 };
        let sy: i32 = if y1 < y2 { 1 } else { -1 };
        let mut err = dx - dy;
        let mut x = x1;
        let mut y = y1;
        // Spread perpendicular to the major axis
        let spread_y = dx >= dy;
        loop {
            for t in 0..thickness {
                if spread_y {
                    self.set(x, y + t, color);
                } else {
                    self.set(x + t, y, color);
                }
            }
            if x == x2 && y == y2 {
                break;
            }
            let e2 = 2 * err;
            if e2 > -dy {
                err -= dy;
                x += sx;
            }
            if e2 < dx {
                err += dx;
                y += sy;
            }
        }
    }

    fn draw_vline(&mut self, x: i32, y1: i32, y2: i32, color: [u8; 4], thickness: i32) {
        for y in y1..y2 {
            for t in 0..thickness {
                self.set(x + t, y, color);
            }
        }
    }

    fn draw_dashed_line(
        &mut self,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        color: [u8; 4],
        thickness: i32,
    ) {
        let len = (((x2 - x1).pow(2) + (y2 - y1).pow(2)) as f32).sqrt() as i32;
        if len == 0 {
            return;
        }
        let dash = 8;
        let gap = 5;
        let period = dash + gap;
        for t in (0..len).step_by(period as usize) {
            let end = (t + dash).min(len);
            let fx1 = x1 + (x2 - x1) * t / len;
            let fy1 = y1 + (y2 - y1) * t / len;
            let fx2 = x1 + (x2 - x1) * end / len;
            let fy2 = y1 + (y2 - y1) * end / len;
            self.draw_line(fx1, fy1, fx2, fy2, color, thickness);
        }
    }

    fn draw_circle(&mut self, cx: i32, cy: i32, r: i32, color: [u8; 4]) {
        let mut x = r;
        let mut y = 0;
        let mut err: i32 = 0;
        while x >= y {
            for (dx, dy) in [
                (x, y),
                (y, x),
                (-y, x),
                (-x, y),
                (-x, -y),
                (-y, -x),
                (y, -x),
                (x, -y),
            ] {
                self.set(cx + dx, cy + dy, color);
            }
            y += 1;
            if err <= 0 {
                err += 2 * y + 1;
            }
            if err > 0 {
                x -= 1;
                err -= 2 * x + 1;
            }
        }
    }

    fn draw_text_centered(&mut self, cx: i32, cy: i32, text: &str, color: [u8; 4]) {
        let total_w = (text.chars().count() * (FONT_W + 1)) as i32;
        let x = cx - total_w / 2;
        self.draw_text(x, cy, text, color);
    }

    fn draw_text(&mut self, mut x: i32, y: i32, text: &str, color: [u8; 4]) {
        for ch in text.chars() {
            let glyph_idx = (ch as usize).saturating_sub(32).min(95);
            let glyph = FONT[glyph_idx];
            for (row, &bits) in glyph.iter().enumerate() {
                for col in 0..FONT_W {
                    if bits & (1 << (FONT_W - 1 - col)) != 0 {
                        self.set(x + col as i32, y + row as i32, color);
                    }
                }
            }
            x += (FONT_W + 1) as i32;
        }
    }
}

fn draw_arc(canvas: &mut Canvas, cx: i32, cy: i32, r: i32, quadrant: u8, color: [u8; 4]) {
    let mut x = r;
    let mut y = 0;
    let mut err: i32 = 0;
    while x >= y {
        let pts: &[(i32, i32)] = match quadrant {
            0 => &[(x, -y), (y, -x)],   // top-right
            1 => &[(x, y), (y, x)],     // bottom-right
            2 => &[(-x, y), (-y, x)],   // bottom-left
            _ => &[(-x, -y), (-y, -x)], // top-left
        };
        for &(dx, dy) in pts {
            canvas.set(cx + dx, cy + dy, color);
        }
        y += 1;
        if err <= 0 {
            err += 2 * y + 1;
        }
        if err > 0 {
            x -= 1;
            err -= 2 * x + 1;
        }
    }
}

// ---- Kitty protocol --------------------------------------------------------

fn transmit(w: usize, h: usize, cell_size: Option<(u16, u16)>, b64: &[u8], out: &mut impl Write) {
    let chunk_size = 4096;
    let mut iter = b64.chunks(chunk_size).peekable();
    let mut first = true;
    while let Some(chunk) = iter.next() {
        let more = if iter.peek().is_some() { 1 } else { 0 };
        if first {
            let cell_part = match cell_size {
                Some((cols, rows)) => format!(",c={cols},r={rows}"),
                None => String::new(),
            };
            let _ = out.write_all(
                format!("\x1b_Ga=T,f=32,s={w},v={h}{cell_part},q=2,m={more};").as_bytes(),
            );
            first = false;
        } else {
            let _ = out.write_all(format!("\x1b_Gm={more};").as_bytes());
        }
        let _ = out.write_all(chunk);
        let _ = out.write_all(b"\x1b\\");
    }
}

// ---- Base64 ----------------------------------------------------------------

fn base64_encode(data: &[u8]) -> Vec<u8> {
    const C: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        out.push(C[(b0 >> 2) & 0x3f]);
        out.push(C[((b0 << 4) | (b1 >> 4)) & 0x3f]);
        out.push(if chunk.len() > 1 {
            C[((b1 << 2) | (b2 >> 6)) & 0x3f]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 { C[b2 & 0x3f] } else { b'=' });
    }
    out
}

// ---- 5×7 bitmap font (public-domain) ---------------------------------------

const FONT_W: usize = 5;
const FONT_H: usize = 7;

// 96 glyphs starting at ASCII 32 (space), each is 7 bytes (one per row), 5-bit wide
const FONT: [[u8; 7]; 96] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // ' '
    [0x04, 0x04, 0x04, 0x04, 0x00, 0x04, 0x00], // '!'
    [0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00], // '"'
    [0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x00, 0x00], // '#'
    [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04], // '$'
    [0x18, 0x19, 0x02, 0x04, 0x09, 0x03, 0x00], // '%'
    [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D], // '&'
    [0x04, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00], // '\''
    [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02], // '('
    [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08], // ')'
    [0x00, 0x04, 0x15, 0x0E, 0x15, 0x04, 0x00], // '*'
    [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00], // '+'
    [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08], // ','
    [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00], // '-'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00], // '.'
    [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10], // '/'
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E], // '0'
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E], // '1'
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F], // '2'
    [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E], // '3'
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02], // '4'
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E], // '5'
    [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E], // '6'
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08], // '7'
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E], // '8'
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C], // '9'
    [0x00, 0x04, 0x00, 0x00, 0x04, 0x00, 0x00], // ':'
    [0x00, 0x04, 0x00, 0x00, 0x04, 0x04, 0x08], // ';'
    [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02], // '<'
    [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00], // '='
    [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08], // '>'
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04], // '?'
    [0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0E], // '@'
    [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11], // 'A'
    [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E], // 'B'
    [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E], // 'C'
    [0x1E, 0x09, 0x09, 0x09, 0x09, 0x09, 0x1E], // 'D'
    [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F], // 'E'
    [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10], // 'F'
    [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F], // 'G'
    [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11], // 'H'
    [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E], // 'I'
    [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C], // 'J'
    [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11], // 'K'
    [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F], // 'L'
    [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11], // 'M'
    [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11], // 'N'
    [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E], // 'O'
    [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10], // 'P'
    [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D], // 'Q'
    [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11], // 'R'
    [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E], // 'S'
    [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04], // 'T'
    [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E], // 'U'
    [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04], // 'V'
    [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11], // 'W'
    [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11], // 'X'
    [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04], // 'Y'
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F], // 'Z'
    [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E], // '['
    [0x10, 0x08, 0x08, 0x04, 0x02, 0x02, 0x01], // '\\'
    [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E], // ']'
    [0x04, 0x0A, 0x11, 0x00, 0x00, 0x00, 0x00], // '^'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F], // '_'
    [0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00], // '`'
    [0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F], // 'a'
    [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x1E], // 'b'
    [0x00, 0x00, 0x0E, 0x10, 0x10, 0x10, 0x0E], // 'c'
    [0x01, 0x01, 0x0F, 0x11, 0x11, 0x11, 0x0F], // 'd'
    [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E], // 'e'
    [0x06, 0x09, 0x08, 0x1C, 0x08, 0x08, 0x08], // 'f'
    [0x00, 0x00, 0x0F, 0x11, 0x0F, 0x01, 0x0E], // 'g'
    [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x11], // 'h'
    [0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E], // 'i'
    [0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0C], // 'j'
    [0x10, 0x10, 0x11, 0x12, 0x1C, 0x12, 0x11], // 'k'
    [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E], // 'l'
    [0x00, 0x00, 0x1A, 0x15, 0x15, 0x11, 0x11], // 'm'
    [0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x11], // 'n'
    [0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E], // 'o'
    [0x00, 0x00, 0x1E, 0x11, 0x1E, 0x10, 0x10], // 'p'
    [0x00, 0x00, 0x0F, 0x11, 0x0F, 0x01, 0x01], // 'q'
    [0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10], // 'r'
    [0x00, 0x00, 0x0E, 0x10, 0x0E, 0x01, 0x1E], // 's'
    [0x08, 0x08, 0x1C, 0x08, 0x08, 0x09, 0x06], // 't'
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x11, 0x0F], // 'u'
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04], // 'v'
    [0x00, 0x00, 0x11, 0x15, 0x15, 0x15, 0x0A], // 'w'
    [0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11], // 'x'
    [0x00, 0x00, 0x11, 0x11, 0x0F, 0x01, 0x0E], // 'y'
    [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F], // 'z'
    [0x06, 0x04, 0x04, 0x08, 0x04, 0x04, 0x06], // '{'
    [0x04, 0x04, 0x04, 0x00, 0x04, 0x04, 0x04], // '|'
    [0x0C, 0x04, 0x04, 0x02, 0x04, 0x04, 0x0C], // '}'
    [0x00, 0x00, 0x08, 0x15, 0x02, 0x00, 0x00], // '~'
    [0x1F, 0x1F, 0x1F, 0x1F, 0x1F, 0x1F, 0x1F], // DEL (filled block)
];
