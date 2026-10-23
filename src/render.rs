use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub fn premultiplied(self) -> [f32; 4] {
        [self.r * self.a, self.g * self.a, self.b * self.a, self.a]
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
    pub u: f32,
    pub v: f32,
}

pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub data: Vec<u8>,
}

pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Surface {
    pub fn new(width: u32, height: u32) -> Self {
        Surface { width, height, pixels: vec![0; (width * height * 4) as usize] }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.pixels = vec![0; (width * height * 4) as usize];
        }
    }

    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    fn blend(&mut self, index: usize, src: [f32; 4]) {
        let dst = &mut self.pixels[index..index + 4];
        let inv = 1.0 - src[3];
        let r = src[0] + (dst[0] as f32 / 255.0) * inv;
        let g = src[1] + (dst[1] as f32 / 255.0) * inv;
        let b = src[2] + (dst[2] as f32 / 255.0) * inv;
        let a = src[3] + (dst[3] as f32 / 255.0) * inv;
        dst[0] = (r.clamp(0.0, 1.0) * 255.0) as u8;
        dst[1] = (g.clamp(0.0, 1.0) * 255.0) as u8;
        dst[2] = (b.clamp(0.0, 1.0) * 255.0) as u8;
        dst[3] = (a.clamp(0.0, 1.0) * 255.0) as u8;
    }
}

pub struct Paint {
    pub xform: [f32; 6],
    pub extent: [f32; 2],
    pub inner: Color,
    pub outer: Color,
    pub image: i32,
}

impl Default for Paint {
    fn default() -> Self {
        Paint {
            xform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            extent: [0.0, 0.0],
            inner: Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
            outer: Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
            image: 0,
        }
    }
}

pub struct Scissor {
    pub xform: [f32; 6],
    pub extent: [f32; 2],
    pub use_clip: bool,
}

impl Default for Scissor {
    fn default() -> Self {
        Scissor { xform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], extent: [0.0, 0.0], use_clip: false }
    }
}

pub struct Renderer {
    pub surface: Surface,
    pub textures: HashMap<i32, Texture>,
    next_texture: i32,
    pub frame_serial: u64,
    pub draw_calls: u32,
}

impl Renderer {
    pub fn new(width: u32, height: u32) -> Self {
        Renderer {
            surface: Surface::new(width.max(1), height.max(1)),
            textures: HashMap::new(),
            next_texture: 1,
            frame_serial: 0,
            draw_calls: 0,
        }
    }

    pub fn create_texture(&mut self, alpha: bool, width: u32, height: u32, data: Option<&[u8]>) -> i32 {
        let id = self.next_texture;
        self.next_texture += 1;
        let size = (width * height) as usize * if alpha { 1 } else { 4 };
        let mut pixels = vec![0u8; size];
        if let Some(src) = data {
            let n = src.len().min(size);
            pixels[..n].copy_from_slice(&src[..n]);
        }
        self.textures.insert(id, Texture { width, height, alpha, data: pixels });
        id
    }

    pub fn update_texture(&mut self, id: i32, x: i32, y: i32, w: i32, h: i32, data: &[u8]) -> bool {
        let Some(tex) = self.textures.get_mut(&id) else { return false };
        let bpp = if tex.alpha { 1 } else { 4 };
        let mut offset = 0usize;
        for row in 0..h {
            let ty = y + row;
            if ty < 0 || ty >= tex.height as i32 {
                offset += (w as usize) * bpp;
                continue;
            }
            for col in 0..w {
                let tx = x + col;
                if tx < 0 || tx >= tex.width as i32 {
                    offset += bpp;
                    continue;
                }
                let dst = (ty as usize * tex.width as usize + tx as usize) * bpp;
                let n = bpp.min(data.len().saturating_sub(offset));
                if n > 0 && dst + n <= tex.data.len() {
                    tex.data[dst..dst + n].copy_from_slice(&data[offset..offset + n]);
                }
                offset += bpp;
            }
        }
        true
    }

    pub fn begin_frame(&mut self) {
        self.surface.clear();
        self.draw_calls = 0;
        self.frame_serial += 1;
    }

    pub fn end_frame(&mut self) {
        self.draw_calls = 0;
    }

    fn sample(&self, paint: &Paint, u: f32, v: f32) -> [f32; 4] {
        let Some(tex) = self.textures.get(&paint.image) else {
            return paint.inner.premultiplied();
        };
        let x = (u.clamp(0.0, 1.0) * (tex.width.saturating_sub(1)) as f32) as usize;
        let y = (v.clamp(0.0, 1.0) * (tex.height.saturating_sub(1)) as f32) as usize;
        let idx = y * tex.width as usize + x;
        let tint = paint.inner;
        if tex.alpha {
            let a = *tex.data.get(idx).unwrap_or(&0) as f32 / 255.0;
            [tint.r * a, tint.g * a, tint.b * a, tint.a * a]
        } else {
            let base = idx * 4;
            let c = |i: usize| *tex.data.get(base + i).unwrap_or(&0) as f32 / 255.0;
            let r = c(0) * tint.r;
            let g = c(1) * tint.g;
            let b = c(2) * tint.b;
            let a = c(3) * tint.a;
            [r * a, g * a, b * a, a]
        }
    }

    pub fn triangles(&mut self, paint: &Paint, scissor: &Scissor, verts: &[Vertex]) {
        self.draw_calls += 1;
        for tri in verts.chunks_exact(3) {
            self.raster_triangle(paint, scissor, tri[0], tri[1], tri[2]);
        }
    }

    pub fn stroke(&mut self, paint: &Paint, scissor: &Scissor, strips: &[Vec<Vertex>]) {
        self.draw_calls += 1;
        for strip in strips {
            for i in 0..strip.len().saturating_sub(2) {
                let (a, b, c) = (strip[i], strip[i + 1], strip[i + 2]);
                self.raster_triangle(paint, scissor, a, b, c);
            }
        }
    }

    pub fn fill(&mut self, paint: &Paint, scissor: &Scissor, rings: &[Vec<Vertex>]) {
        self.draw_calls += 1;
        let color = paint.inner.premultiplied();
        for ring in rings {
            if ring.len() < 3 {
                continue;
            }
            self.raster_polygon(scissor, ring, color);
        }
    }

    fn clip_contains(&self, scissor: &Scissor, x: f32, y: f32) -> bool {
        if !scissor.use_clip || scissor.extent[0] < -0.5 || scissor.extent[1] < -0.5 {
            return true;
        }
        let m = scissor.xform;
        let px = m[0] * x + m[2] * y + m[4];
        let py = m[1] * x + m[3] * y + m[5];
        px.abs() <= scissor.extent[0] && py.abs() <= scissor.extent[1]
    }

    pub fn fill_path(&mut self, paint: &Paint, scissor: &Scissor, fan: &[Vertex], fringe: &[Vertex]) {
        self.draw_calls += 1;
        if std::env::var("MSFSRUNNER_TRACE_RENDER").is_ok() {
            let minx = fan.iter().map(|v| v.x).fold(f32::MAX, f32::min);
            let maxx = fan.iter().map(|v| v.x).fold(f32::MIN, f32::max);
            let miny = fan.iter().map(|v| v.y).fold(f32::MAX, f32::min);
            let maxy = fan.iter().map(|v| v.y).fold(f32::MIN, f32::max);
            eprintln!(
                "    fill fan={} fringe={} bounds=({minx:.1},{miny:.1})-({maxx:.1},{maxy:.1}) color={:?}",
                fan.len(), fringe.len(), paint.inner
            );
        }
        if paint.image != 0 {
            if fan.len() >= 3 {
                for i in 1..fan.len() - 1 {
                    self.raster_triangle(paint, scissor, fan[0], fan[i], fan[i + 1]);
                }
            }
        } else {
            let color = paint.inner.premultiplied();
            if fan.len() >= 3 {
                self.raster_polygon(scissor, fan, color);
            }
        }
        if !fringe.is_empty() {
            for i in 0..fringe.len().saturating_sub(2) {
                self.raster_triangle(paint, scissor, fringe[i], fringe[i + 1], fringe[i + 2]);
            }
        }
    }

    fn raster_triangle_opaque(&mut self, color: [f32; 4], a: Vertex, b: Vertex, c: Vertex) {
        let (w, h) = (self.surface.width as i32, self.surface.height as i32);
        let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
        let max_x = a.x.max(b.x).max(c.x).ceil().min(w as f32) as i32;
        let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
        let max_y = a.y.max(b.y).max(c.y).ceil().min(h as f32) as i32;
        let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
        if area.abs() < 1e-6 || min_x >= max_x || min_y >= max_y {
            return;
        }
        let inv = 1.0 / area;
        let px = [
            (color[0].clamp(0.0, 1.0) * 255.0) as u8,
            (color[1].clamp(0.0, 1.0) * 255.0) as u8,
            (color[2].clamp(0.0, 1.0) * 255.0) as u8,
            255u8,
        ];
        for py in min_y..max_y {
            let sy = py as f32 + 0.5;
            let row = (py * w) as usize * 4;
            for sx_i in min_x..max_x {
                let sx = sx_i as f32 + 0.5;
                let w0 = ((b.x - sx) * (c.y - sy) - (c.x - sx) * (b.y - sy)) * inv;
                let w1 = ((c.x - sx) * (a.y - sy) - (a.x - sx) * (c.y - sy)) * inv;
                if w0 < -0.0001 || w1 < -0.0001 || w0 + w1 > 1.0001 {
                    continue;
                }
                let idx = row + sx_i as usize * 4;
                self.surface.pixels[idx..idx + 4].copy_from_slice(&px);
            }
        }
    }

    fn raster_polygon(&mut self, scissor: &Scissor, ring: &[Vertex], color: [f32; 4]) {
        let (w, h) = (self.surface.width as i32, self.surface.height as i32);
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        for v in ring {
            min_y = min_y.min(v.y);
            max_y = max_y.max(v.y);
        }
        let y0 = (min_y.floor() as i32).max(0);
        let y1 = (max_y.ceil() as i32).min(h);
        let mut xs: Vec<f32> = Vec::new();
        for py in y0..y1 {
            let sy = py as f32 + 0.5;
            xs.clear();
            for i in 0..ring.len() {
                let a = ring[i];
                let b = ring[(i + 1) % ring.len()];
                if (a.y <= sy && b.y > sy) || (b.y <= sy && a.y > sy) {
                    let t = (sy - a.y) / (b.y - a.y);
                    xs.push(a.x + t * (b.x - a.x));
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            for pair in xs.chunks_exact(2) {
                let start = (pair[0].ceil() as i32).max(0);
                let end = (pair[1].floor() as i32).min(w);
                for px in start..end {
                    if !self.clip_contains(scissor, px as f32 + 0.5, sy) {
                        continue;
                    }
                    let idx = ((py * w + px) * 4) as usize;
                    if idx + 4 <= self.surface.pixels.len() {
                        self.surface.blend(idx, color);
                    }
                }
            }
        }
    }

    fn raster_triangle(&mut self, paint: &Paint, scissor: &Scissor, a: Vertex, b: Vertex, c: Vertex) {
        let solid = paint.image == 0;
        let clipped = scissor.use_clip && scissor.extent[0] >= -0.5 && scissor.extent[1] >= -0.5;
        if solid && !clipped {
            let color = paint.inner.premultiplied();
            if color[3] >= 0.999 {
                self.raster_triangle_opaque(color, a, b, c);
                return;
            }
        }
        let (w, h) = (self.surface.width as i32, self.surface.height as i32);
        let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
        let max_x = a.x.max(b.x).max(c.x).ceil().min(w as f32) as i32;
        let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
        let max_y = a.y.max(b.y).max(c.y).ceil().min(h as f32) as i32;
        let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
        if area.abs() < 1e-6 {
            return;
        }
        let inv = 1.0 / area;
        for py in min_y..max_y {
            let sy = py as f32 + 0.5;
            for px in min_x..max_x {
                let sx = px as f32 + 0.5;
                let w0 = ((b.x - sx) * (c.y - sy) - (c.x - sx) * (b.y - sy)) * inv;
                let w1 = ((c.x - sx) * (a.y - sy) - (a.x - sx) * (c.y - sy)) * inv;
                let w2 = 1.0 - w0 - w1;
                if w0 < -0.0001 || w1 < -0.0001 || w2 < -0.0001 {
                    continue;
                }
                if !self.clip_contains(scissor, sx, sy) {
                    continue;
                }
                let src = if paint.image != 0 {
                    let u = a.u * w0 + b.u * w1 + c.u * w2;
                    let v = a.v * w0 + b.v * w1 + c.v * w2;
                    self.sample(paint, u, v)
                } else {
                    paint.inner.premultiplied()
                };
                if src[3] <= 0.0001 {
                    continue;
                }
                let idx = ((py * w + px) * 4) as usize;
                if idx + 4 <= self.surface.pixels.len() {
                    self.surface.blend(idx, src);
                }
            }
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore]
    fn raster_throughput() {
        let mut r = Renderer::new(1024, 768);
        let paint = Paint::default();
        let scissor = Scissor::default();
        let quad = vec![
            Vertex { x: 0.0, y: 0.0, u: 0.0, v: 0.0 },
            Vertex { x: 1024.0, y: 0.0, u: 0.0, v: 0.0 },
            Vertex { x: 1024.0, y: 768.0, u: 0.0, v: 0.0 },
            Vertex { x: 0.0, y: 0.0, u: 0.0, v: 0.0 },
            Vertex { x: 1024.0, y: 768.0, u: 0.0, v: 0.0 },
            Vertex { x: 0.0, y: 768.0, u: 0.0, v: 0.0 },
        ];
        let start = std::time::Instant::now();
        for _ in 0..10 {
            r.triangles(&paint, &scissor, &quad);
        }
        println!("10 fullscreen tris: {:?}", start.elapsed());

        let ring: Vec<Vertex> = (0..200)
            .map(|i| {
                let a = i as f32 / 200.0 * std::f32::consts::TAU;
                Vertex { x: 512.0 + 500.0 * a.cos(), y: 384.0 + 380.0 * a.sin(), u: 0.0, v: 0.0 }
            })
            .collect();
        let start = std::time::Instant::now();
        for _ in 0..10 {
            r.fill_path(&paint, &scissor, &ring, &[]);
        }
        println!("10 polygon fills: {:?}", start.elapsed());
    }
}
