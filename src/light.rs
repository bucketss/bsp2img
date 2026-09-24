#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightParams {
    pub gamma: f64,
    pub texgamma: f64,
    pub lightgamma: f64,
    pub brightness: f64,
    pub scale: f64,
    pub all_styles: bool,
}

impl Default for LightParams {
    fn default() -> Self {
        LightParams { gamma: 2.5, texgamma: 2.0, lightgamma: 2.5, brightness: 1.0, scale: 1.0, all_styles: false }
    }
}

impl LightParams {
    pub fn tex_lut(&self) -> [u8; 256] {
        let g = self.gamma.min(3.0);
        let mut lut = [0u8; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            *v = (255.0 * (i as f64 / 255.0).powf(self.texgamma / g) + 0.5).clamp(0.0, 255.0) as u8;
        }
        lut
    }

    pub fn light_to_screen(&self, linear: f64) -> u8 {
        let b = self.brightness;
        let g3 = if b <= 0.0 {
            0.125
        } else if b > 1.0 {
            0.05
        } else {
            0.125 - b * b * 0.075
        };
        let mut f = linear * self.scale;
        if b > 1.0 {
            f *= b;
        }
        f = if f <= g3 { f / g3 * 0.125 } else { 0.125 + (f - g3) / (1.0 - g3) * 0.875 };
        f = f.clamp(0.0, 1.0);
        (255.0 * f.powf(1.0 / self.gamma.min(3.0)) + 0.5).clamp(0.0, 255.0) as u8
    }
}
