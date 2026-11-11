// One quad per cell background and one textured quad per glyph. No
// atlas and no damage tracking; 80x24 is about 2k quads.

use std::collections::HashMap;

use alacritty_terminal::vte::ansi::Rgb;
use crossfont::{
    BitmapBuffer, FontDesc, FontKey, GlyphKey, Rasterize, Rasterizer, Size, Slant, Style, Weight,
};
use glow::HasContext;

use crate::Result;

const VERT: &str = r#"#version 330 core
layout(location = 0) in vec2 v;
uniform vec2 u_screen;
uniform vec4 u_rect;
out vec2 uv;
void main() {
    vec2 p = u_rect.xy + v * u_rect.zw;
    gl_Position = vec4(p.x / u_screen.x * 2.0 - 1.0, 1.0 - p.y / u_screen.y * 2.0, 0.0, 1.0);
    uv = v;
}"#;

// u_mode: 0 = solid, 1 = RGB coverage mask, 2 = RGBA bitmap (emoji).
const FRAG: &str = r#"#version 330 core
in vec2 uv;
uniform sampler2D u_tex;
uniform vec3 u_color;
uniform int u_mode;
out vec4 color;
void main() {
    if (u_mode == 0) {
        color = vec4(u_color, 1.0);
    } else if (u_mode == 1) {
        vec3 m = texture(u_tex, uv).rgb;
        color = vec4(u_color, (m.r + m.g + m.b) / 3.0);
    } else {
        color = texture(u_tex, uv);
    }
}"#;

struct Glyph {
    tex: Option<glow::Texture>,
    rgba: bool,
    w: i32,
    h: i32,
    left: i32,
    top: i32,
}

const NO_GLYPH: Glyph = Glyph {
    tex: None,
    rgba: false,
    w: 0,
    h: 0,
    left: 0,
    top: 0,
};

pub struct Renderer {
    gl: glow::Context,
    program: glow::Program,
    u_screen: glow::UniformLocation,
    u_rect: glow::UniformLocation,
    u_color: glow::UniformLocation,
    u_mode: glow::UniformLocation,
    rast: Rasterizer,
    regular: FontKey,
    bold: FontKey,
    size: Size,
    glyphs: HashMap<(char, bool), Glyph>,
    pub cell_w: u32,
    pub cell_h: u32,
    ascent: i32,
}

fn err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

impl Renderer {
    // 'font_px' is in pixels with the scale factor already applied.
    pub fn new(gl: glow::Context, font_px: f32) -> Result<Renderer> {
        // crossfont takes points and converts at 96 dpi.
        let size = Size::new(font_px * 72.0 / 96.0);
        let mut rast = Rasterizer::new().map_err(err)?;
        let desc = |weight| {
            let style = Style::Description {
                slant: Slant::Normal,
                weight,
            };
            FontDesc::new("JetBrains Mono", style)
        };
        let regular = rast.load_font(&desc(Weight::Normal), size).map_err(err)?;
        let bold = rast.load_font(&desc(Weight::Bold), size).map_err(err)?;
        // metrics() returns zeros until one glyph has been rasterized.
        let _ = rast.get_glyph(GlyphKey {
            character: 'm',
            font_key: regular,
            size,
        });
        let m = rast.metrics(regular, size).map_err(err)?;
        let cell_w = m.average_advance.round().max(1.0) as u32;
        let cell_h = m.line_height.round().max(1.0) as u32;
        // 'descent' is negative.
        let ascent = (m.line_height + m.descent as f64).round() as i32;

        let program;
        let loc;
        unsafe {
            program = gl.create_program().map_err(err)?;
            for (kind, src) in [(glow::VERTEX_SHADER, VERT), (glow::FRAGMENT_SHADER, FRAG)] {
                let sh = gl.create_shader(kind).map_err(err)?;
                gl.shader_source(sh, src);
                gl.compile_shader(sh);
                if !gl.get_shader_compile_status(sh) {
                    return Err(err(gl.get_shader_info_log(sh)));
                }
                gl.attach_shader(program, sh);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(err(gl.get_program_info_log(program)));
            }
            gl.use_program(Some(program));
            loc = |n: &str| {
                gl.get_uniform_location(program, n)
                    .ok_or_else(|| err(format!("no uniform {n}")))
            };

            // A unit square; u_rect places and scales it.
            let vao = gl.create_vertex_array().map_err(err)?;
            gl.bind_vertex_array(Some(vao));
            let vbo = gl.create_buffer().map_err(err)?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let quad: [f32; 12] = [0., 0., 1., 0., 0., 1., 1., 0., 1., 1., 0., 1.];
            let bytes: Vec<u8> = quad.iter().flat_map(|f| f.to_ne_bytes()).collect();
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            // Glyph bitmaps are tightly packed RGB, 3 bytes per pixel.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        }

        Ok(Renderer {
            u_screen: loc("u_screen")?,
            u_rect: loc("u_rect")?,
            u_color: loc("u_color")?,
            u_mode: loc("u_mode")?,
            gl,
            program,
            rast,
            regular,
            bold,
            size,
            glyphs: HashMap::new(),
            cell_w,
            cell_h,
            ascent,
        })
    }

    pub fn resize(&self, w: u32, h: u32) {
        unsafe {
            self.gl.viewport(0, 0, w as i32, h as i32);
            self.gl.use_program(Some(self.program));
            self.gl
                .uniform_2_f32(Some(&self.u_screen), w as f32, h as f32);
        }
    }

    pub fn clear(&self, c: Rgb) {
        unsafe {
            self.gl.disable(glow::BLEND);
            let (r, g, b) = rgbf(c);
            self.gl.clear_color(r, g, b, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    fn quad(&self, mode: i32, x: i32, y: i32, w: i32, h: i32, c: Rgb) {
        let (r, g, b) = rgbf(c);
        unsafe {
            self.gl.uniform_1_i32(Some(&self.u_mode), mode);
            self.gl
                .uniform_4_f32(Some(&self.u_rect), x as f32, y as f32, w as f32, h as f32);
            self.gl.uniform_3_f32(Some(&self.u_color), r, g, b);
            self.gl.draw_arrays(glow::TRIANGLES, 0, 6);
        }
    }

    // Draws backgrounds. Call before begin_glyphs().
    pub fn fill(&self, x: i32, y: i32, w: i32, h: i32, c: Rgb) {
        self.quad(0, x, y, w, h, c);
    }

    // Glyphs are blended so one that overhangs its cell doesn't erase
    // the neighbor.
    pub fn begin_glyphs(&self) {
        unsafe { self.gl.enable(glow::BLEND) }
    }

    // Draws 'c' in the cell whose top-left corner is (x, y).
    pub fn glyph(&mut self, c: char, bold: bool, x: i32, y: i32, fg: Rgb) {
        if c == ' ' {
            return;
        }
        if !self.glyphs.contains_key(&(c, bold)) {
            let g = self.rasterize(c, bold);
            self.glyphs.insert((c, bold), g);
        }
        let g = &self.glyphs[&(c, bold)];
        let Some(tex) = g.tex else { return };
        let mode = if g.rgba { 2 } else { 1 };
        let (gx, gy, gw, gh) = (x + g.left, y + self.ascent - g.top, g.w, g.h);
        unsafe { self.gl.bind_texture(glow::TEXTURE_2D, Some(tex)) }
        self.quad(mode, gx, gy, gw, gh, fg);
    }

    fn rasterize(&mut self, c: char, bold: bool) -> Glyph {
        let font_key = if bold { self.bold } else { self.regular };
        let key = GlyphKey {
            character: c,
            font_key,
            size: self.size,
        };
        let Ok(r) = self.rast.get_glyph(key) else {
            return NO_GLYPH;
        };
        if r.width == 0 || r.height == 0 {
            return NO_GLYPH;
        }
        let (fmt, rgba, data) = match &r.buffer {
            BitmapBuffer::Rgb(d) => (glow::RGB, false, d.as_slice()),
            BitmapBuffer::Rgba(d) => (glow::RGBA, true, d.as_slice()),
        };
        let tex = unsafe {
            let gl = &self.gl;
            let t = gl.create_texture().ok();
            gl.bind_texture(glow::TEXTURE_2D, t);
            for (k, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
            }
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                fmt as i32,
                r.width,
                r.height,
                0,
                fmt,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(data)),
            );
            t
        };
        Glyph {
            tex,
            rgba,
            w: r.width,
            h: r.height,
            left: r.left,
            top: r.top,
        }
    }
}

fn rgbf(c: Rgb) -> (f32, f32, f32) {
    (c.r as f32 / 255., c.g as f32 / 255., c.b as f32 / 255.)
}
