use p2_math::Vector;
use super::{FluidSurface, ROW, MIN_PLING_RADIUS};

impl FluidSurface {
    pub fn pling_vertex(&mut self, x: i32, y: i32, strength: f32) {
        if (1..self.settings.x_size - 1).contains(&x) && (1..self.settings.y_size - 1).contains(&y) && self.settings.update_rate > 0.0 {
            let i = self.index(x, y);
            if !self.is_clamped(i) {
                let height = self.verts[self.latest][i] + (1.0 / self.settings.update_rate) * strength;
                self.verts[self.latest][i] = height.clamp(-self.height_limit, self.height_limit);
            }
        }
    }

    pub fn pling(&mut self, position: Vector, strength: f32, radius: f32) {
        let wide = self.settings.spacing * MIN_PLING_RADIUS;
        let (reach, scale) = if radius >= wide {
            (radius, strength)
        } else {
            let total = self.pling_weight_total(position, wide);
            if total <= 0.0 {
                return;
            }
            (wide, strength / total)
        };
        let (x0, y0) = self.nearest_index(position.x - reach, position.y - reach);
        let (x1, y1) = self.nearest_index(position.x + reach, position.y + reach);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let weight = self.pling_weight(x, y, position, reach);
                if weight > 0.0 {
                    self.pling_vertex(x, y, weight * scale);
                }
            }
        }
    }

    fn pling_weight_total(&self, position: Vector, reach: f32) -> f32 {
        let (x0, y0) = self.nearest_index(position.x - reach, position.y - reach);
        let (x1, y1) = self.nearest_index(position.x + reach, position.y + reach);
        let mut total = 0.0;
        for y in y0..=y1 {
            for x in x0..=x1 {
                total += self.pling_weight(x, y, position, reach);
            }
        }
        total
    }

    fn pling_weight(&self, x: i32, y: i32, position: Vector, reach: f32) -> f32 {
        let s = &self.settings;
        if !(1..s.x_size - 1).contains(&x) || !(1..s.y_size - 1).contains(&y) || self.is_clamped(self.index(x, y)) {
            return 0.0;
        }
        let (vx, vy) = self.vertex_xy(x, y);
        let d2 = (vx - position.x) * (vx - position.x) + (vy - position.y) * (vy - position.y);
        if d2 < reach * reach { (reach * reach - d2).sqrt() / reach } else { 0.0 }
    }

    fn nearest_index(&self, x: f32, y: f32) -> (i32, i32) {
        let s = &self.settings;
        let row = if s.hexagonal { s.spacing * ROW } else { s.spacing };
        let ix = ((x - self.origin.x) / s.spacing).round_ties_even() as i32;
        let iy = ((y - self.origin.y) / row).round_ties_even() as i32;
        let hold = |v: i32, count: i32| if v < 0 { 0 } else { v.min(count - 1) };
        (hold(ix, s.x_size), hold(iy, s.y_size))
    }

    pub(super) fn vertex_xy(&self, x: i32, y: i32) -> (f32, f32) {
        let s = &self.settings;
        if s.hexagonal {
            let shift = if y & 1 != 0 { 0.5 } else { 0.0 };
            ((x as f32 + shift) * s.spacing + self.origin.x, y as f32 * ROW * s.spacing + self.origin.y)
        } else {
            (x as f32 * s.spacing + self.origin.x, y as f32 * s.spacing + self.origin.y)
        }
    }
}
