//! Text zoom ladder: browser-style size steps from 50% to 300%, applied on top
//! of the desktop text scale.

pub const STEPS: &[f64] = &[
    0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];

#[derive(Debug, Clone, Copy)]
pub struct Zoom {
    pub zoom: f64,
    pub desktop: f64,
}

impl Zoom {
    pub fn new(desktop: f64, zoom: f64) -> Self {
        Self {
            zoom: zoom.clamp(STEPS[0], STEPS[STEPS.len() - 1]),
            desktop,
        }
    }
    pub fn scale(&self) -> f64 {
        self.desktop * self.zoom
    }
    fn closest(&self) -> usize {
        let mut best = 0;
        for (i, &s) in STEPS.iter().enumerate() {
            if (s - self.zoom).abs() < (STEPS[best] - self.zoom).abs() {
                best = i;
            }
        }
        best
    }
    pub fn step(&self, dir: i32) -> f64 {
        let c = self.closest() as i32;
        let n = (c + dir).clamp(0, STEPS.len() as i32 - 1) as usize;
        STEPS[n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ladder() {
        let z = Zoom::new(1.5, 1.0);
        assert!((z.scale() - 1.5).abs() < 1e-9);
        assert!((Zoom::new(1.5, 1.0).step(1) - 1.1).abs() < 1e-9);
    }
}
