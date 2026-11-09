use p2_math::Vector;
use super::{FluidVertex, FluidSurface};

impl FluidSurface {
    /// Heights between the last two steps, so the water moves at any frame rate, one step behind the simulation.
    pub(super) fn lerp_drawn(&mut self) {
        let (old, new) = (&self.verts[1 - self.latest], &self.verts[self.latest]);
        let t = (self.rollover * self.settings.update_rate).clamp(0.0, 1.0);
        for ((drawn, &old), &new) in self.drawn.iter_mut().zip(old).zip(new) {
            *drawn = old + (new - old) * t;
        }
    }

    fn drawn_at(&self) -> impl Fn(i32, i32) -> f32 + '_ {
        let nx = self.settings.x_size;
        move |x, y| self.drawn[(y * nx + x) as usize]
    }

    pub fn vertices(&mut self) -> Vec<FluidVertex> {
        let mut out = Vec::new();
        self.vertices_into(&mut out);
        out
    }

    pub fn vertices_into(&mut self, out: &mut Vec<FluidVertex>) {
        out.clear();
        let (nx, ny) = (self.settings.x_size, self.settings.y_size);
        if nx < 1 || ny < 1 {
            return;
        }
        out.reserve((nx * ny) as usize);
        self.lerp_drawn();
        if self.settings.hexagonal {
            self.hexagonal_vertices(out);
        } else {
            self.square_vertices(out);
        }
    }

    fn edge_vertex(&self, x: f32, y: f32, uv: [f32; 2]) -> FluidVertex {
        FluidVertex { position: Vector::new(x, y, self.origin.z), normal: Vector::new(0.0, 0.0, 1.0), alpha: 0, uv }
    }

    fn hexagonal_vertices(&self, out: &mut Vec<FluidVertex>) {
        let s = &self.settings;
        let (nx, ny) = (s.x_size, s.y_size);
        let o = self.origin;
        let at = self.drawn_at();
        let hs = s.height_scale;
        let per_u = 1.0 / (nx as f32 - 0.5);
        let row_spacing = s.spacing * s.row_spacing();
        let nz = row_spacing * 4.0 * s.spacing;
        let column = |x: f32| (s.u_tiles * x) * per_u + s.u_offset;
        for x in 0..nx {
            out.push(self.edge_vertex(x as f32 * s.spacing + o.x, o.y, [column(x as f32), s.v_offset]));
        }
        for y in 1..ny - 1 {
            let v = (s.v_tiles * y as f32) / (ny - 1) as f32 + s.v_offset;
            let vy = y as f32 * s.row_spacing() * s.spacing + o.y;
            let odd = y & 1 != 0;
            let shift = if odd { 0.5 } else { 0.0 };
            out.push(self.edge_vertex(shift * s.spacing + o.x, vy, [if odd { column(0.5) } else { s.u_offset }, v]));
            for x in 1..nx - 1 {
                let c = at(x, y);
                let d = |x: i32, y: i32| (at(x, y) - c) * hs;
                let (l, r) = (d(x - 1, y), d(x + 1, y));
                let (up_left, up_right, down_right, down_left) = if y & 1 == 0 {
                    (d(x - 1, y + 1), d(x, y + 1), d(x, y - 1), d(x - 1, y - 1))
                } else {
                    (d(x, y + 1), d(x + 1, y + 1), d(x + 1, y - 1), d(x, y - 1))
                };
                let normal_x = ((((l * 2.0 * row_spacing - r * 2.0 * row_spacing) + up_left * row_spacing) - up_right * row_spacing) + down_left * row_spacing) - down_right * row_spacing;
                let normal_y = ((down_right - (up_right + up_left)) + down_left) * s.spacing * 1.5;
                let fx = x as f32 + shift;
                out.push(FluidVertex {
                    position: Vector::new(fx * s.spacing + o.x, vy, c * hs + o.z),
                    normal: Vector::new(normal_x, normal_y, nz),
                    alpha: self.alpha[(y * nx + x) as usize],
                    uv: [column(fx), v],
                });
            }
            let last = if odd { (nx as f32 - 0.5, s.u_offset + s.u_tiles) } else { ((nx - 1) as f32, column((nx - 1) as f32)) };
            out.push(self.edge_vertex(last.0 * s.spacing + o.x, vy, [last.1, v]));
        }
        let shift = if (ny - 1) & 1 == 0 { 0.0 } else { 0.5 };
        let vy = (ny - 1) as f32 * s.row_spacing() * s.spacing + o.y;
        for x in 0..nx {
            let fx = x as f32 + shift;
            out.push(self.edge_vertex(fx * s.spacing + o.x, vy, [column(fx), s.v_offset + s.v_tiles]));
        }
    }

    fn square_vertices(&self, out: &mut Vec<FluidVertex>) {
        let s = &self.settings;
        let (nx, ny) = (s.x_size, s.y_size);
        let o = self.origin;
        let at = self.drawn_at();
        let hs = s.height_scale;
        let per_u = 1.0 / (nx as f32 - 1.0);
        // Retail's z is negative, which points the normal under the water.
        let nz = s.spacing * s.spacing * 4.0;
        let column = |x: i32| (s.u_tiles * x as f32) * per_u + s.u_offset;
        for x in 0..nx {
            out.push(self.edge_vertex(x as f32 * s.spacing + o.x, o.y, [column(x), s.v_offset]));
        }
        for y in 1..ny - 1 {
            let v = (s.v_tiles * y as f32) / (ny - 1) as f32 + s.v_offset;
            let vy = y as f32 * s.spacing + o.y;
            out.push(self.edge_vertex(o.x, vy, [s.u_offset, v]));
            for x in 1..nx - 1 {
                let c = at(x, y);
                let d = |x: i32, y: i32| (at(x, y) - c) * hs;
                out.push(FluidVertex {
                    position: Vector::new(x as f32 * s.spacing + o.x, vy, c * hs + o.z),
                    normal: Vector::new((d(x - 1, y) - d(x + 1, y)) * s.spacing * 2.0, (d(x, y - 1) - d(x, y + 1)) * s.spacing * 2.0, nz),
                    alpha: self.alpha[(y * nx + x) as usize],
                    uv: [column(x), v],
                });
            }
            out.push(self.edge_vertex((nx - 1) as f32 * s.spacing + o.x, vy, [s.u_tiles + s.u_offset, v]));
        }
        let vy = (ny - 1) as f32 * s.spacing + o.y;
        for x in 0..nx {
            out.push(self.edge_vertex(x as f32 * s.spacing + o.x, vy, [column(x), s.v_offset + s.v_tiles]));
        }
    }

    pub fn indices(&self) -> Vec<u32> {
        let (nx, ny) = (self.settings.x_size, self.settings.y_size);
        let mut out = Vec::with_capacity(((nx - 1).max(0) * (ny - 1).max(0) * 6) as usize);
        for y in 0..ny - 1 {
            for x in 0..nx - 1 {
                let a = (y * nx + x) as u32;
                let b = ((y + 1) * nx + x) as u32;
                if self.settings.hexagonal && y & 1 == 0 {
                    out.extend([a, b, a + 1, b, b + 1, a + 1]);
                } else {
                    out.extend([a, b + 1, a + 1, a, b, b + 1]);
                }
            }
        }
        out
    }
}
