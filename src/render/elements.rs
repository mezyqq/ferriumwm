//! Custom render elements: rounded/clipped surfaces, borders, shadows.

use smithay::{
    backend::renderer::{
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::WaylandSurfaceRenderElement,
            texture::TextureRenderElement,
        },
        gles::{
            GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
            UniformName, UniformType, UniformValue, element::PixelShaderElement,
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Frame,
    },
    render_elements,
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform, user_data::UserDataMap},
};

use crate::style::{Paint, Shadow};

render_elements! {
    pub FeElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Clipped=Clipped<WaylandSurfaceRenderElement<GlesRenderer>>,
    ClippedTex=Clipped<TextureRenderElement<GlesTexture>>,
    Texture=TextureRenderElement<GlesTexture>,
    Pixel=PixelShaderElement,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

pub struct Shaders {
    pub clip: GlesTexProgram,
    pub border: GlesPixelProgram,
    pub shadow: GlesPixelProgram,
}

impl Shaders {
    pub fn compile(renderer: &mut GlesRenderer) -> Result<Shaders, GlesError> {
        let clip = renderer.compile_custom_texture_shader(
            include_str!("shaders/clip.frag"),
            &[
                UniformName::new("frag_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("dim", UniformType::_1f),
            ],
        )?;
        let border = renderer.compile_custom_pixel_shader(
            include_str!("shaders/border.frag"),
            &[
                UniformName::new("border", UniformType::_1f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("color1", UniformType::_4f),
                UniformName::new("color2", UniformType::_4f),
                UniformName::new("angle", UniformType::_1f),
            ],
        )?;
        let shadow = renderer.compile_custom_pixel_shader(
            include_str!("shaders/shadow.frag"),
            &[
                UniformName::new("shadow_rect", UniformType::_4f),
                UniformName::new("win_rect", UniformType::_4f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("win_radius", UniformType::_1f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("color", UniformType::_4f),
            ],
        )?;
        Ok(Shaders { clip, border, shadow })
    }
}

// ------------------------------------------------------------- clipping

/// Wraps an element and renders it through the rounded-clip texture shader.
#[derive(Debug)]
pub struct Clipped<E> {
    inner: E,
    program: GlesTexProgram,
    /// window geometry in output-local physical coordinates
    geo: Rectangle<f64, Physical>,
    radius: f32,
    dim: f32,
}

impl<E: Element> Clipped<E> {
    pub fn new(inner: E, program: GlesTexProgram, geo: Rectangle<f64, Physical>, radius: f32, dim: f32) -> Self {
        Self { inner, program, geo, radius, dim }
    }
}

impl<E: Element> Element for Clipped<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.radius <= 0.0 && self.dim == 0.0 {
            // clip to geometry still applies, so only keep regions inside it
            let geo = self.geo.to_i32_round();
            let loc = self.inner.geometry(scale).loc;
            let local_geo = Rectangle::new(geo.loc - loc, geo.size);
            return OpaqueRegions::from_slice(
                &self
                    .inner
                    .opaque_regions(scale)
                    .iter()
                    .filter_map(|r| r.intersection(local_geo))
                    .collect::<Vec<_>>(),
            );
        }
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

/// Column-major 3x3 helpers.
type M3 = [f32; 9];

fn m3_mul(a: &M3, b: &M3) -> M3 {
    let mut r = [0.0; 9];
    for c in 0..3 {
        for row in 0..3 {
            r[c * 3 + row] = (0..3).map(|k| a[k * 3 + row] * b[c * 3 + k]).sum();
        }
    }
    r
}

fn m3_inv(m: &M3) -> Option<M3> {
    // column-major: element (row, col) = m[col*3+row]
    let e = |r: usize, c: usize| m[c * 3 + r] as f64;
    let det = e(0, 0) * (e(1, 1) * e(2, 2) - e(1, 2) * e(2, 1)) - e(0, 1) * (e(1, 0) * e(2, 2) - e(1, 2) * e(2, 0))
        + e(0, 2) * (e(1, 0) * e(2, 1) - e(1, 1) * e(2, 0));
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = |r: usize, c: usize| -> f32 {
        // cofactor of (c, r) / det
        let (r1, r2) = match c {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let (c1, c2) = match r {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let minor = e(r1, c1) * e(r2, c2) - e(r1, c2) * e(r2, c1);
        let sign = if (r + c) % 2 == 0 { 1.0 } else { -1.0 };
        (sign * minor / det) as f32
    };
    let mut out = [0.0; 9];
    for c in 0..3 {
        for r in 0..3 {
            out[c * 3 + r] = inv(r, c);
        }
    }
    Some(out)
}

/// Matrix mapping gl_FragCoord to coordinates relative to `geo` (output physical space).
pub fn frag_to_local(frame: &GlesFrame<'_, '_>, geo_loc: Point<f64, Physical>) -> M3 {
    let proj: M3 = *frame.projection();
    let size = frame.output_size();
    let fb: Size<i32, Physical> = match frame.transformation() {
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => (size.h, size.w).into(),
        _ => size,
    };
    // fragcoord -> NDC
    let a: M3 = [2.0 / fb.w as f32, 0.0, 0.0, 0.0, 2.0 / fb.h as f32, 0.0, -1.0, -1.0, 1.0];
    let inv = m3_inv(&proj).unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let t: M3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, -geo_loc.x as f32, -geo_loc.y as f32, 1.0];
    m3_mul(&t, &m3_mul(&inv, &a))
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for Clipped<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let m = frag_to_local(frame, self.geo.loc);
        frame.override_default_tex_program(
            self.program.clone(),
            vec![
                Uniform::new("frag_to_geo", UniformValue::Matrix3x3 { matrices: vec![m], transpose: false }),
                Uniform::new("geo_size", (self.geo.size.w as f32, self.geo.size.h as f32)),
                Uniform::new("radius", self.radius),
                Uniform::new("dim", self.dim),
            ],
        );
        let res = self.inner.draw(frame, src, dst, damage, opaque_regions, cache);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// ------------------------------------------------------------- borders & shadows

pub fn border_uniforms(width: f32, radius: f32, paint: &Paint, alpha: f32) -> Vec<Uniform<'static>> {
    let (c1, c2, angle) = paint.stops();
    let pm = |c: crate::style::Color| {
        let [r, g, b, a] = c.premultiplied();
        (r * alpha, g * alpha, b * alpha, a * alpha)
    };
    vec![
        Uniform::new("border", width),
        Uniform::new("radius", radius),
        Uniform::new("color1", pm(c1)),
        Uniform::new("color2", pm(c2)),
        Uniform::new("angle", angle),
    ]
}

/// Area (logical, output-local) covering a shadow for `win` (window+border rect).
pub fn shadow_area(win: Rectangle<f64, smithay::utils::Logical>, s: &Shadow) -> Rectangle<f64, smithay::utils::Logical> {
    let ext = (s.blur * 1.5 + s.spread.abs() + 2.0) as f64;
    let (sx, sy) = (s.x as f64, s.y as f64);
    let mut r = Rectangle::new(
        (win.loc.x + sx - ext, win.loc.y + sy - ext).into(),
        (win.size.w + 2.0 * ext, win.size.h + 2.0 * ext).into(),
    );
    r = r.merge(win);
    r
}

#[allow(clippy::too_many_arguments)]
pub fn shadow_uniforms(
    area: Rectangle<f64, smithay::utils::Logical>,
    win: Rectangle<f64, smithay::utils::Logical>,
    s: &Shadow,
    radius: f32,
    scale: f64,
    alpha: f32,
) -> Vec<Uniform<'static>> {
    let sc = scale as f32;
    let sx = (win.loc.x - area.loc.x) as f32 * sc + (s.x - s.spread) * sc;
    let sy = (win.loc.y - area.loc.y) as f32 * sc + (s.y - s.spread) * sc;
    let sw = win.size.w as f32 * sc + 2.0 * s.spread * sc;
    let sh = win.size.h as f32 * sc + 2.0 * s.spread * sc;
    let wx = (win.loc.x - area.loc.x) as f32 * sc;
    let wy = (win.loc.y - area.loc.y) as f32 * sc;
    let [r, g, b, a] = s.color.premultiplied();
    vec![
        Uniform::new("shadow_rect", (sx, sy, sw, sh)),
        Uniform::new("win_rect", (wx, wy, win.size.w as f32 * sc, win.size.h as f32 * sc)),
        Uniform::new("radius", (radius + s.spread.max(0.0)) * sc),
        Uniform::new("win_radius", radius * sc),
        Uniform::new("sigma", (s.blur / 2.0).max(0.5) * sc),
        Uniform::new("color", (r * alpha, g * alpha, b * alpha, a * alpha)),
    ]
}
