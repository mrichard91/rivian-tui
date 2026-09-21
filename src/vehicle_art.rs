//! An original, front three-quarter R1T illustration made from terminal block glyphs.
//!
//! Geometry is in a 160 × 72 drawing space. A terminal cell is roughly twice as
//! tall as it is wide; the renderer preserves that proportion at every size.
//! No image protocol, downloaded asset, font ligature, or graphics dependency is
//! needed. Thin LEDs receive extra weight so they survive at dashboard sizes.

use std::cell::RefCell;

use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

type Rgb = [u8; 3];
const BG: Rgb = [16, 21, 24];
const SHADOW: Rgb = [23, 32, 35];
const TIRE: Rgb = [32, 39, 42];
const TREAD: Rgb = [50, 59, 62];
const RIM: Rgb = [104, 118, 123];
const RIM_HI: Rgb = [163, 176, 176];
const HUB: Rgb = [53, 65, 70];
const GLASS: Rgb = [36, 57, 63];
const GLASS_HI: Rgb = [71, 97, 104];
const GLASS_DARK: Rgb = [25, 39, 45];
const PAINT: Rgb = [103, 135, 121];
const SHADE: Rgb = [60, 90, 80];
const DEEP: Rgb = [44, 66, 59];
const LIGHT: Rgb = [142, 170, 149];
const HIGHLIGHT: Rgb = [191, 206, 192];
const TRIM: Rgb = [38, 53, 51];
const WHITE: Rgb = [239, 255, 254];
const LED: Rgb = [214, 244, 237];
const GOLD: Rgb = [212, 173, 88];

pub const BACKGROUND: Color = Color::Rgb(BG[0], BG[1], BG[2]);

/// Space reserved for the complete illustration, excluding the panel border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct R1tArt {
    pub width: u16,
    pub height: u16,
}

impl R1tArt {
    /// Fit without squashing or clipping. Below 32 × 8 the lamps and pickup bed
    /// no longer read clearly, so leave that space to vehicle information.
    pub fn fit(width: u16, height: u16) -> Option<Self> {
        let width = width.min(76).min(height.saturating_mul(40) / 9);
        let height = (width * 9).div_ceil(40);
        (width >= 32 && height >= 8).then_some(Self { width, height })
    }
}

thread_local! {
    // Only the most recent size is retained. The 200 ms redraw path copies
    // terminal cells; geometry and glyph fitting run only after a resize.
    static CACHE: RefCell<Option<Buffer>> = const { RefCell::new(None) };
}

impl Widget for R1tArt {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area);
        // Widget callers may supply a smaller area than the requested size.
        // Refit instead of writing beyond the allocation, including at zero.
        let Some(size) = Self::fit(area.width.min(self.width), area.height.min(self.height)) else {
            return;
        };
        CACHE.with_borrow_mut(|cache| {
            let bounds = Rect::new(0, 0, size.width, size.height);
            if cache.as_ref().is_none_or(|art| art.area != bounds) {
                *cache = Some(illustration(size));
            }
            let art = cache.as_ref().expect("art was initialized above");
            let x = area.x + (area.width - size.width) / 2;
            let y = area.y + (area.height - size.height) / 2;
            for row in 0..size.height {
                for col in 0..size.width {
                    if let Some(cell) = buf.cell_mut((x + col, y + row)) {
                        *cell = art[(col, row)].clone();
                    }
                }
            }
        });
    }
}

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Rgb>,
}

impl Canvas {
    fn new(width: u16, height: u16) -> Self {
        let (width, height) = (width as usize * 8, height as usize * 16);
        Self {
            width,
            height,
            pixels: vec![BG; width * height],
        }
    }

    fn paint(&mut self, bounds: [f32; 4], color: Rgb, contains: impl Fn(f32, f32) -> bool) {
        let sx = self.width as f32 / 160.0;
        let sy = self.height as f32 / 72.0;
        let x0 = (bounds[0] * sx).floor().max(0.0) as usize;
        let x1 = (bounds[2] * sx).ceil().min(self.width as f32) as usize;
        let y0 = (bounds[1] * sy).floor().max(0.0) as usize;
        let y1 = (bounds[3] * sy).ceil().min(self.height as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                if contains((x as f32 + 0.5) / sx, (y as f32 + 0.5) / sy) {
                    self.pixels[y * self.width + x] = color;
                }
            }
        }
    }

    fn polygon(&mut self, points: &[(f32, f32)], color: Rgb) {
        let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &(x, y) in points {
            bounds = [
                bounds[0].min(x),
                bounds[1].min(y),
                bounds[2].max(x),
                bounds[3].max(y),
            ];
        }
        self.paint(bounds, color, |x, y| {
            let mut inside = false;
            let mut previous = points[points.len() - 1];
            for &(px, py) in points {
                let (qx, qy) = previous;
                if (py > y) != (qy > y) && x < (qx - px) * (y - py) / (qy - py) + px {
                    inside = !inside;
                }
                previous = (px, py);
            }
            inside
        });
    }

    fn line(&mut self, points: &[(f32, f32)], color: Rgb, width: f32) {
        let radius = width / 2.0;
        for pair in points.windows(2) {
            let ((ax, ay), (bx, by)) = (pair[0], pair[1]);
            let (dx, dy) = (bx - ax, by - ay);
            let length_squared = dx * dx + dy * dy;
            self.paint(
                [
                    ax.min(bx) - radius,
                    ay.min(by) - radius,
                    ax.max(bx) + radius,
                    ay.max(by) + radius,
                ],
                color,
                |x, y| {
                    let t = if length_squared > 0.0 {
                        (((x - ax) * dx + (y - ay) * dy) / length_squared).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    (x - ax - t * dx).powi(2) + (y - ay - t * dy).powi(2) <= radius * radius
                },
            );
        }
    }

    fn ellipse(&mut self, bounds: [f32; 4], color: Rgb) {
        let (rx, ry) = ((bounds[2] - bounds[0]) / 2.0, (bounds[3] - bounds[1]) / 2.0);
        let (cx, cy) = (bounds[0] + rx, bounds[1] + ry);
        self.paint(bounds, color, |x, y| {
            ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0
        });
    }

    fn wheel(&mut self, cx: f32, cy: f32, rx: f32, ry: f32) {
        for (inset, color) in [
            (0.0, TIRE),
            (1.0, TREAD),
            (2.0, TIRE),
            (3.0, RIM),
            (4.0, GLASS_DARK),
        ] {
            self.ellipse(
                [
                    cx - rx + inset,
                    cy - ry + inset,
                    cx + rx - inset,
                    cy + ry - inset,
                ],
                color,
            );
        }
        for spoke in 0..5 {
            let angle = spoke as f32 * std::f32::consts::TAU / 5.0 - std::f32::consts::FRAC_PI_2;
            self.line(
                &[
                    (cx, cy),
                    (cx + angle.cos() * (rx - 3.6), cy + angle.sin() * (ry - 3.6)),
                ],
                RIM_HI,
                1.0,
            );
        }
        self.ellipse([cx - 1.6, cy - 2.0, cx + 1.6, cy + 2.0], HUB);
    }
}

fn draw_truck(c: &mut Canvas) {
    // Ground contact, the far front tire, and the underbody.
    c.ellipse([12.0, 59.0, 152.0, 69.0], SHADOW);
    c.ellipse([13.0, 44.0, 29.0, 64.0], TIRE);
    c.polygon(
        &[
            (17.0, 48.0),
            (144.0, 34.0),
            (149.0, 45.0),
            (64.0, 63.0),
            (22.0, 56.0),
        ],
        GLASS_DARK,
    );
    // Short, open pickup bed: a dark inset bounded by a separate bright rail.
    c.polygon(
        &[
            (50.0, 36.0),
            (69.0, 28.0),
            (116.0, 23.0),
            (150.0, 18.0),
            (155.0, 21.0),
            (155.0, 42.0),
            (150.0, 46.0),
            (52.0, 62.0),
        ],
        SHADE,
    );
    c.polygon(
        &[(115.0, 23.0), (149.0, 18.0), (155.0, 21.0), (121.0, 28.0)],
        TRIM,
    );
    c.polygon(
        &[(120.0, 23.0), (149.0, 19.5), (152.0, 21.0), (123.0, 25.5)],
        GLASS_DARK,
    );
    c.line(
        &[(119.0, 22.5), (150.0, 18.5), (154.0, 21.0)],
        HIGHLIGHT,
        1.0,
    );
    // Crew cab with a long windshield, two side windows and a substantial C pillar.
    c.polygon(
        &[
            (31.0, 30.0),
            (47.0, 11.0),
            (50.0, 9.0),
            (80.0, 8.0),
            (106.0, 5.0),
            (112.0, 7.0),
            (121.0, 26.0),
            (69.0, 34.0),
        ],
        PAINT,
    );
    c.polygon(
        &[
            (48.0, 10.0),
            (80.0, 8.0),
            (106.0, 5.0),
            (112.0, 7.0),
            (80.0, 11.0),
            (45.0, 13.0),
        ],
        LIGHT,
    );
    c.line(
        &[(50.0, 9.0), (80.0, 8.0), (106.0, 5.0), (111.0, 6.5)],
        HIGHLIGHT,
        0.75,
    );
    c.polygon(
        &[(48.0, 13.0), (77.0, 12.0), (67.0, 29.0), (33.0, 29.0)],
        GLASS,
    );
    c.polygon(
        &[(49.0, 13.0), (75.0, 12.0), (67.0, 16.0), (42.0, 23.0)],
        GLASS_HI,
    );
    c.polygon(
        &[(48.0, 15.0), (71.0, 13.0), (64.0, 28.0), (35.0, 28.0)],
        GLASS,
    );
    c.line(&[(37.0, 27.5), (64.0, 27.5)], GLASS_DARK, 1.0);
    c.polygon(
        &[(81.0, 12.0), (94.0, 10.5), (98.0, 26.0), (72.0, 30.0)],
        GLASS_DARK,
    );
    c.polygon(
        &[(82.0, 13.0), (92.0, 12.0), (93.0, 25.0), (74.0, 28.5)],
        GLASS,
    );
    c.polygon(
        &[(84.0, 13.0), (90.0, 12.5), (91.0, 24.0), (78.0, 26.0)],
        GLASS_HI,
    );
    c.polygon(
        &[(98.0, 10.0), (109.0, 9.0), (115.0, 24.0), (101.0, 26.0)],
        GLASS_DARK,
    );
    c.polygon(
        &[(100.0, 11.0), (108.0, 10.5), (112.0, 22.5), (102.0, 24.0)],
        GLASS,
    );
    c.polygon(
        &[(101.0, 11.5), (106.0, 11.0), (108.0, 17.0), (102.0, 20.0)],
        GLASS_HI,
    );
    // Shoulder reflection, flush handles, door seams, and the gear tunnel.
    c.polygon(
        &[
            (51.0, 38.0),
            (68.0, 29.0),
            (120.0, 24.0),
            (154.0, 21.5),
            (154.0, 28.0),
            (115.0, 32.0),
            (70.0, 38.0),
            (54.0, 45.0),
        ],
        PAINT,
    );
    c.polygon(
        &[
            (69.0, 30.0),
            (119.0, 25.0),
            (153.0, 22.0),
            (153.0, 24.0),
            (74.0, 35.0),
            (57.0, 41.0),
        ],
        LIGHT,
    );
    c.line(&[(72.0, 37.0), (117.0, 30.0), (151.0, 27.0)], SHADE, 0.75);
    c.polygon(
        &[
            (75.0, 38.0),
            (118.0, 31.0),
            (129.0, 30.0),
            (126.0, 43.0),
            (74.0, 55.0),
        ],
        SHADE,
    );
    c.polygon(
        &[(73.0, 53.0), (126.0, 42.0), (124.0, 46.0), (74.0, 58.0)],
        DEEP,
    );
    c.polygon(
        &[(70.0, 58.0), (149.0, 41.0), (151.0, 44.0), (71.0, 63.0)],
        TRIM,
    );
    c.line(&[(75.0, 56.0), (128.0, 44.0), (150.0, 40.0)], RIM, 1.0);
    c.line(&[(95.0, 27.0), (98.0, 38.0), (98.0, 49.0)], DEEP, 0.75);
    c.line(
        &[(117.0, 25.0), (121.0, 35.0), (117.0, 45.0), (105.0, 49.0)],
        DEEP,
        0.8,
    );
    c.line(&[(80.0, 32.0), (83.0, 44.0), (80.0, 53.0)], DEEP, 0.6);
    c.line(&[(86.0, 33.0), (90.0, 32.3)], HIGHLIGHT, 1.0);
    c.line(&[(104.0, 30.0), (108.0, 29.4)], HIGHLIGHT, 1.0);
    c.polygon(
        &[(120.0, 31.0), (128.0, 29.0), (126.0, 40.0), (120.0, 43.0)],
        DEEP,
    );
    c.line(
        &[(120.0, 31.0), (127.0, 29.5), (125.0, 40.0), (120.0, 42.0)],
        SHADE,
        0.75,
    );
    // Squared wheel arches around tall all-terrain tires and five-spoke wheels.
    c.polygon(
        &[
            (49.0, 53.0),
            (51.0, 44.0),
            (55.0, 38.0),
            (62.0, 36.0),
            (68.0, 37.0),
            (73.0, 41.0),
            (77.0, 55.0),
            (73.0, 59.0),
            (50.0, 62.0),
        ],
        TRIM,
    );
    c.line(
        &[
            (50.0, 53.0),
            (53.0, 44.0),
            (57.0, 39.0),
            (64.0, 38.0),
            (68.0, 40.0),
            (71.0, 46.0),
        ],
        RIM,
        0.8,
    );
    c.polygon(
        &[
            (127.0, 44.0),
            (128.0, 34.0),
            (132.0, 29.0),
            (138.0, 28.0),
            (143.0, 29.0),
            (146.0, 33.0),
            (149.0, 41.0),
            (145.0, 47.0),
        ],
        TRIM,
    );
    c.line(
        &[
            (128.0, 42.0),
            (129.0, 35.0),
            (133.0, 30.0),
            (138.0, 29.0),
            (143.0, 31.0),
            (146.0, 36.0),
        ],
        RIM,
        0.7,
    );
    c.wheel(62.0, 54.0, 10.0, 13.0);
    c.wheel(137.0, 43.0, 8.0, 11.0);
    c.line(&[(70.0, 44.0), (71.0, 48.0)], GOLD, 1.0);
    // Flat hood, upright nose, recessed front panel, bumper and silver skid plate.
    c.polygon(
        &[
            (5.0, 34.0),
            (31.0, 28.0),
            (68.0, 29.0),
            (61.0, 34.0),
            (51.0, 39.0),
            (7.0, 39.0),
        ],
        LIGHT,
    );
    c.polygon(
        &[
            (8.0, 34.0),
            (32.0, 29.0),
            (66.0, 29.5),
            (55.0, 33.0),
            (13.0, 35.0),
        ],
        PAINT,
    );
    c.line(&[(7.0, 34.0), (33.0, 28.5), (66.0, 29.0)], HIGHLIGHT, 0.7);
    c.polygon(
        &[
            (6.0, 35.0),
            (50.0, 38.0),
            (51.0, 51.0),
            (47.0, 56.0),
            (9.0, 55.0),
            (4.0, 51.0),
            (4.0, 40.0),
        ],
        PAINT,
    );
    c.polygon(
        &[(8.0, 43.0), (46.0, 45.0), (46.0, 50.0), (10.0, 49.0)],
        SHADE,
    );
    c.line(&[(8.0, 48.0), (45.0, 50.0)], LIGHT, 0.6);
    c.polygon(
        &[
            (5.0, 49.0),
            (49.0, 51.0),
            (49.0, 57.0),
            (43.0, 60.0),
            (8.0, 57.0),
            (4.0, 53.0),
        ],
        TIRE,
    );
    c.line(&[(8.0, 50.0), (44.0, 52.0)], GLASS_DARK, 2.0);
    c.line(&[(9.0, 52.0), (44.0, 54.0)], RIM, 0.75);
    c.polygon(
        &[(13.0, 55.0), (40.0, 57.0), (44.0, 60.0), (11.0, 58.0)],
        RIM,
    );
    c.line(&[(13.0, 55.0), (38.0, 57.0)], RIM_HI, 0.75);
    c.line(&[(10.0, 53.0), (14.0, 53.3)], GLASS_DARK, 1.0);
    c.line(&[(37.0, 54.4), (41.0, 54.7)], GLASS_DARK, 1.0);
    // Heavier light outlines keep the hollow stadium lamps visible at small sizes.
    c.line(&[(6.0, 38.2), (47.0, 40.5), (49.0, 40.0)], TRIM, 3.0);
    c.line(&[(7.0, 38.2), (46.5, 40.5), (48.0, 40.0)], WHITE, 1.8);
    for (x, y, rx, ry) in [(11.5, 41.0, 2.8, 5.5), (42.0, 42.5, 3.2, 6.0)] {
        c.ellipse(
            [x - rx - 0.7, y - ry - 0.65, x + rx + 0.7, y + ry + 0.65],
            TRIM,
        );
        c.ellipse([x - rx, y - ry, x + rx, y + ry], WHITE);
        c.ellipse(
            [x - rx + 1.4, y - ry + 1.25, x + rx - 1.4, y + ry - 1.25],
            GLASS_DARK,
        );
        c.line(&[(x, y - 0.7), (x, y + 0.8)], RIM, 0.55);
    }
    c.polygon(
        &[(27.0, 32.0), (28.0, 32.7), (27.0, 33.5), (26.0, 32.8)],
        GOLD,
    );
    c.polygon(
        &[
            (69.0, 27.0),
            (71.0, 24.0),
            (77.0, 23.0),
            (81.0, 25.0),
            (80.0, 28.0),
            (73.0, 29.0),
        ],
        TRIM,
    );
    c.polygon(
        &[(70.0, 25.8), (72.0, 24.0), (77.0, 23.8), (80.0, 25.8)],
        PAINT,
    );
    c.line(&[(72.0, 27.2), (79.0, 26.8)], LED, 0.65);
    c.line(&[(152.0, 27.0), (153.0, 31.0)], [200, 138, 115], 1.0);
    c.line(&[(81.0, 42.0), (86.0, 41.2)], GOLD, 0.65);
}

/// Masks are 8 × 8; each mask row covers two samples vertically. These are
/// ordinary Unicode block elements, supported by standard terminal fonts.
const GLYPHS: [(char, u64); 25] = [
    (' ', 0),
    ('▁', 0xff00_0000_0000_0000),
    ('▂', 0xffff_0000_0000_0000),
    ('▃', 0xffff_ff00_0000_0000),
    ('▄', 0xffff_ffff_0000_0000),
    ('▅', 0xffff_ffff_ff00_0000),
    ('▆', 0xffff_ffff_ffff_0000),
    ('▇', 0xffff_ffff_ffff_ff00),
    ('▏', 0x0101_0101_0101_0101),
    ('▎', 0x0303_0303_0303_0303),
    ('▍', 0x0707_0707_0707_0707),
    ('▌', 0x0f0f_0f0f_0f0f_0f0f),
    ('▋', 0x1f1f_1f1f_1f1f_1f1f),
    ('▊', 0x3f3f_3f3f_3f3f_3f3f),
    ('▉', 0x7f7f_7f7f_7f7f_7f7f),
    ('▘', 0x0000_0000_0f0f_0f0f),
    ('▝', 0x0000_0000_f0f0_f0f0),
    ('▖', 0x0f0f_0f0f_0000_0000),
    ('▗', 0xf0f0_f0f0_0000_0000),
    ('▚', 0xf0f0_f0f0_0f0f_0f0f),
    ('▞', 0x0f0f_0f0f_f0f0_f0f0),
    ('▛', 0x0f0f_0f0f_ffff_ffff),
    ('▜', 0xf0f0_f0f0_ffff_ffff),
    ('▙', 0xffff_ffff_0f0f_0f0f),
    ('▟', 0xffff_ffff_f0f0_f0f0),
];

#[derive(Clone, Copy, Default)]
struct Ink {
    weight: f32,
    sum: [f32; 3],
}

impl Ink {
    fn add(&mut self, other: Self) {
        self.weight += other.weight;
        for (sum, value) in self.sum.iter_mut().zip(other.sum) {
            *sum += value;
        }
    }

    fn without(self, other: Self) -> Self {
        Self {
            weight: self.weight - other.weight,
            sum: std::array::from_fn(|i| self.sum[i] - other.sum[i]),
        }
    }

    fn score(self) -> f32 {
        self.sum.iter().map(|v| v * v).sum::<f32>() / self.weight.max(1.0)
    }

    fn color(self) -> Color {
        let rgb: Rgb = std::array::from_fn(|i| (self.sum[i] / self.weight.max(1.0)).round() as u8);
        Color::Rgb(rgb[0], rgb[1], rgb[2])
    }
}

fn illustration(size: R1tArt) -> Buffer {
    let mut canvas = Canvas::new(size.width, size.height);
    draw_truck(&mut canvas);
    let mut buffer = Buffer::empty(Rect::new(0, 0, size.width, size.height));
    for y in 0..size.height as usize {
        for x in 0..size.width as usize {
            let mut samples = [Ink::default(); 64];
            let mut total = Ink::default();
            for (i, sample) in samples.iter_mut().enumerate() {
                for row in 0..2 {
                    let pixel =
                        canvas.pixels[(y * 16 + i / 8 * 2 + row) * canvas.width + x * 8 + i % 8];
                    // Preserve the light bar and hollow stadium lamps. Without
                    // this optical correction the small white outlines average
                    // into the bodywork at common 120-column terminal sizes.
                    let weight = if pixel.iter().map(|v| *v as u16).sum::<u16>() > 615 {
                        8.0
                    } else {
                        1.0
                    };
                    sample.add(Ink {
                        weight,
                        sum: pixel.map(|v| v as f32 * weight),
                    });
                }
                total.add(*sample);
            }
            let mut best = (f32::MIN, ' ', Ink::default(), total);
            for (symbol, mask) in GLYPHS {
                let mut foreground = Ink::default();
                for (i, sample) in samples.iter().enumerate() {
                    if mask & (1 << i) != 0 {
                        foreground.add(*sample);
                    }
                }
                let background = total.without(foreground);
                // Maximizing the explained color energy minimizes squared
                // error against the drawing, using one foreground and one
                // background color per terminal cell.
                let score = foreground.score() + background.score();
                if score > best.0 {
                    best = (score, symbol, foreground, background);
                }
            }
            buffer[(x as u16, y as u16)]
                .set_char(best.1)
                .set_fg(best.2.color())
                .set_bg(best.3.color());
        }
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_preserves_proportions_and_fits_available_space() {
        for width in 0..200 {
            for height in 0..60 {
                if let Some(art) = R1tArt::fit(width, height) {
                    assert!(art.width <= width && art.height <= height);
                    assert!((32..=76).contains(&art.width));
                    assert_eq!(art.height, (art.width * 9).div_ceil(40));
                }
            }
        }
    }

    #[test]
    fn resizing_never_paints_outside_the_widget() {
        for area in [
            Rect::new(7, 3, 40, 10),
            Rect::new(7, 3, 32, 8),
            Rect::new(7, 3, 5, 2),
            Rect::new(55, 18, 20, 10),
            Rect::new(7, 3, 0, 0),
        ] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 60, 20));
            for cell in &mut buffer.content {
                cell.set_char('x');
            }
            R1tArt {
                width: 76,
                height: 18,
            }
            .render(area, &mut buffer);
            for y in 0..20 {
                for x in 0..60 {
                    if !area.contains((x, y).into()) {
                        assert_eq!(buffer[(x, y)].symbol(), "x");
                    }
                }
            }
        }
    }
}
