use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

pub struct Palette([Rgb; COUNT]);

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

fn dim(c: Rgb) -> Rgb {
    c * 0.66
}

impl Default for Palette {
    fn default() -> Self {
        let mut p = [rgb(0, 0, 0); COUNT];
        let normal = [
            rgb(0x1d, 0x1f, 0x21),
            rgb(0xcc, 0x66, 0x66),
            rgb(0xb5, 0xbd, 0x68),
            rgb(0xf0, 0xc6, 0x74),
            rgb(0x81, 0xa2, 0xbe),
            rgb(0xb2, 0x94, 0xbb),
            rgb(0x8a, 0xbe, 0xb7),
            rgb(0xc5, 0xc8, 0xc6),
        ];
        let bright = [
            rgb(0x66, 0x66, 0x66),
            rgb(0xd5, 0x4e, 0x53),
            rgb(0xb9, 0xca, 0x4a),
            rgb(0xe7, 0xc5, 0x47),
            rgb(0x7a, 0xa6, 0xda),
            rgb(0xc3, 0x97, 0xd8),
            rgb(0x70, 0xc0, 0xb1),
            rgb(0xea, 0xea, 0xea),
        ];
        p[..8].copy_from_slice(&normal);
        p[8..16].copy_from_slice(&bright);
        // 6x6x6 cube, then 24 grays, as xterm lays them out.
        for i in 0..216u8 {
            let lvl = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            p[16 + i as usize] = rgb(lvl(i / 36), lvl(i / 6 % 6), lvl(i % 6));
        }
        for i in 0..24u8 {
            let v = 8 + 10 * i;
            p[232 + i as usize] = rgb(v, v, v);
        }
        p[NamedColor::Foreground as usize] = rgb(0xc5, 0xc8, 0xc6);
        p[NamedColor::Background as usize] = rgb(0x14, 0x17, 0x1c);
        p[NamedColor::Cursor as usize] = rgb(0xc5, 0xc8, 0xc6);
        for i in 0..8 {
            p[NamedColor::DimBlack as usize + i] = dim(normal[i]);
        }
        p[NamedColor::BrightForeground as usize] = rgb(0xea, 0xea, 0xea);
        p[NamedColor::DimForeground as usize] = dim(p[NamedColor::Foreground as usize]);
        Palette(p)
    }
}

impl Palette {
    pub fn get(&self, c: NamedColor) -> Rgb {
        self.0[c as usize]
    }
}

fn lookup(idx: usize, palette: &Palette, overrides: &Colors) -> Rgb {
    overrides[idx].unwrap_or(palette.0[idx])
}

fn to_dim(c: NamedColor) -> Option<NamedColor> {
    Some(match c {
        NamedColor::Black => NamedColor::DimBlack,
        NamedColor::Red => NamedColor::DimRed,
        NamedColor::Green => NamedColor::DimGreen,
        NamedColor::Yellow => NamedColor::DimYellow,
        NamedColor::Blue => NamedColor::DimBlue,
        NamedColor::Magenta => NamedColor::DimMagenta,
        NamedColor::Cyan => NamedColor::DimCyan,
        NamedColor::White => NamedColor::DimWhite,
        NamedColor::Foreground => NamedColor::DimForeground,
        _ => return None,
    })
}

fn resolve_one(c: Color, dimmed: bool, palette: &Palette, overrides: &Colors) -> Rgb {
    match c {
        Color::Spec(rgb) => {
            if dimmed {
                dim(rgb)
            } else {
                rgb
            }
        }
        Color::Named(n) => match (dimmed, to_dim(n)) {
            (true, Some(d)) => lookup(d as usize, palette, overrides),
            _ => lookup(n as usize, palette, overrides),
        },
        Color::Indexed(i) if dimmed && i < 8 => lookup(
            NamedColor::DimBlack as usize + i as usize,
            palette,
            overrides,
        ),
        Color::Indexed(i) => lookup(i as usize, palette, overrides),
    }
}

/// Returns (fg, bg) for a cell. DIM applies to the foreground only;                                           
/// INVERSE swaps after resolution; HIDDEN paints the glyph in bg.                                             
pub fn resolve(
    fg: Color,
    bg: Color,
    flags: Flags,
    palette: &Palette,
    overrides: &Colors,
) -> (Rgb, Rgb) {
    let mut f = resolve_one(fg, flags.contains(Flags::DIM), palette, overrides);
    let mut b = resolve_one(bg, false, palette, overrides);
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut f, &mut b);
    }
    if flags.contains(Flags::HIDDEN) {
        f = b;
    }
    (f, b)
}
