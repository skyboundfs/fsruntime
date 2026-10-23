use crate::render::{Color, Paint, Renderer, Scissor, Vertex};
use std::collections::HashMap;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl FsColor {
    pub fn to_color(self) -> Color {
        Color { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FsPaint {
    pub xform: [f32; 6],
    pub extent: [f32; 2],
    pub radius: f32,
    pub feather: f32,
    pub inner_color: FsColor,
    pub outer_color: FsColor,
    pub image: i32,
}

impl Default for FsPaint {
    fn default() -> Self {
        FsPaint {
            xform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            extent: [0.0, 0.0],
            radius: 0.0,
            feather: 1.0,
            inner_color: FsColor { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
            outer_color: FsColor { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
            image: 0,
        }
    }
}

impl FsPaint {
    pub fn to_paint(self) -> Paint {
        Paint {
            xform: self.xform,
            extent: self.extent,
            inner: self.inner_color.to_color(),
            outer: self.outer_color.to_color(),
            image: self.image,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FsCompositeOperationState {
    pub src_rgb: i32,
    pub dst_rgb: i32,
    pub src_alpha: i32,
    pub dst_alpha: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FsScissor {
    pub xform: [f32; 6],
    pub extent: [f32; 2],
    pub use_clip: u8,
    pub set: u8,
    pub mode: i32,
}

impl Default for FsScissor {
    fn default() -> Self {
        FsScissor {
            xform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            extent: [0.0, 0.0],
            use_clip: 0,
            set: 0,
            mode: 0,
        }
    }
}

impl FsScissor {
    pub fn to_scissor(self) -> Scissor {
        Scissor { xform: self.xform, extent: self.extent, use_clip: self.use_clip != 0 }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsVertex {
    pub x: f32,
    pub y: f32,
    pub u: f32,
    pub v: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FsPath {
    pub first: i32,
    pub count: i32,
    pub closed: u8,
    pub nbevel: i32,
    pub fill: u32,
    pub nfill: i32,
    pub stroke: u32,
    pub nstroke: i32,
    pub winding: i32,
    pub convex: i32,
}

#[derive(Clone, Debug, Default)]
pub struct DrawFrame {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub commands: u32,
}

pub struct GaugeRender {
    pub renderer: Renderer,
    pub viewport: [f32; 3],
    pub last_serial: u64,
}

impl GaugeRender {
    pub fn new(width: u32, height: u32) -> Self {
        GaugeRender {
            renderer: Renderer::new(width, height),
            viewport: [width as f32, height as f32, 1.0],
            last_serial: 0,
        }
    }
}

#[derive(Default)]
pub struct RenderRegistry {
    gauges: HashMap<u64, GaugeRender>,
    next_context: u64,
}

impl RenderRegistry {
    pub fn context_for(&mut self, ctx: u64, width: u32, height: u32) -> &mut GaugeRender {
        let entry = self
            .gauges
            .entry(ctx)
            .or_insert_with(|| GaugeRender::new(width, height));
        entry.renderer.surface.resize(width.max(1), height.max(1));
        entry
    }

    pub fn get(&mut self, ctx: u64) -> Option<&mut GaugeRender> {
        self.gauges.get_mut(&ctx)
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_context += 1;
        self.next_context
    }

    pub fn remove(&mut self, ctx: u64) {
        self.gauges.remove(&ctx);
    }

    pub fn frames(&self) -> impl Iterator<Item = (u64, &GaugeRender)> {
        self.gauges.iter().map(|(k, v)| (*k, v))
    }
}

pub fn verts_from_ptr(vertices: &[FsVertex]) -> Vec<Vertex> {
    vertices.iter().map(|v| Vertex { x: v.x, y: v.y, u: v.u, v: v.v }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_layout_matches_sdk_header() {
        assert_eq!(std::mem::size_of::<FsPaint>(), 24 + 8 + 4 + 4 + 16 + 16 + 4);
        assert_eq!(std::mem::size_of::<FsScissor>(), 24 + 8 + 1 + 1 + 2 + 4);
        assert_eq!(std::mem::size_of::<FsVertex>(), 16);
    }

    #[test]
    fn vertex_conversion_preserves_coordinates() {
        let src = [FsVertex { x: 1.5, y: 2.5, u: 0.25, v: 0.75 }];
        let out = verts_from_ptr(&src);
        assert_eq!(out[0].x, 1.5);
        assert_eq!(out[0].v, 0.75);
    }
}
