use anyhow::Result;
use cairo::{Context, Format, ImageSurface, LinearGradient};

fn ease(t: f64) -> f64 {
    let t = t.clamp(0., 1.);
    if t < 0.5 {
        4. * t * t * t
    } else {
        1. - (-2. * t + 2.).powi(3) / 2.
    }
}

pub fn shell(
    width: i32,
    height: i32,
    color: [u8; 3],
    open: f64,
    level: f64,
    tick: f64,
) -> Result<Vec<u8>> {
    let mut surface = ImageSurface::create(Format::ARgb32, width, height)?;
    {
        let c = Context::new(&surface)?;
        let eased = ease(open);
        let rgb = color.map(|v| v as f64 / 255.);
        let h = height as f64;

        // Rock-solid fixed dimensions: the outer shell never wobbles or breathes
        let current_w = 88. + eased * 76.;
        let current_h = 14. + eased * 24.;
        let left = (width as f64 - current_w) / 2.;
        let top = h - current_h;

        // Fixed outer boundary (strictly inside carapace, zero rectangular shadow)
        carapace(&c, left, left + current_w, h, top);
        let g = LinearGradient::new(0., top, 0., h);
        g.add_color_stop_rgba(0., 0.12, 0.14, 0.18, 0.98);
        g.add_color_stop_rgba(1., 0.05, 0.06, 0.09, 0.98);
        c.set_source(&g)?;
        c.fill_preserve()?;

        // Subtle specular highlight on top edge
        let edge = LinearGradient::new(0., top, 0., top + 8.);
        edge.add_color_stop_rgba(0., 1., 1., 1., 0.28);
        edge.add_color_stop_rgba(1., 1., 1., 1., 0.);
        c.set_source(&edge)?;
        c.fill_preserve()?;

        // Outer glow/border
        c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.7);
        c.set_line_width(1.2);
        c.stroke()?;

        if open > 0.05 {
            // Voice Activity Indicator / Dynamic Waveform Display
            let inner_w = current_w - 24.;
            let inner_h = 18.;
            let inner_x = left + 12.;
            let inner_y = top + (current_h - inner_h) / 2. - 2.;

            // Slotted inset cavity for the dynamic bars
            rounded(&c, inner_x, inner_y, inner_w, inner_h, 6.);
            let slot_g = LinearGradient::new(0., inner_y, 0., inner_y + inner_h);
            slot_g.add_color_stop_rgba(0., 0.02, 0.03, 0.04, 0.9);
            slot_g.add_color_stop_rgba(1., 0.06, 0.07, 0.09, 0.9);
            c.set_source(&slot_g)?;
            c.fill_preserve()?;
            c.set_source_rgba(0., 0., 0., 0.6);
            c.set_line_width(0.8);
            c.stroke()?;

            // Render responsive EQ visualizer bars
            let num_bars = 16;
            let bar_gap = 2.0;
            let total_gaps = (num_bars - 1) as f64 * bar_gap;
            let bar_w = ((inner_w - 12. - total_gaps) / num_bars as f64).max(1.5);
            let start_x = inner_x + 6.;

            for i in 0..num_bars {
                let bx = start_x + i as f64 * (bar_w + bar_gap);
                let center_dist = ((i as f64 - 7.5) / 7.5).abs();
                let shape_factor = 1.0 - center_dist * 0.45;

                // Height calculation based on level (speech amplitude) or harmonic pulse (processing)
                let bh = if level < 0. {
                    // Processing / Spinner wave pulse
                    let phase = tick * 0.18 + i as f64 * 0.38;
                    let wave = (phase.sin() + 1.0) / 2.0;
                    (4.0 + wave * 9.0) * eased
                } else {
                    // Voice mic dynamics: dynamic height scaled from detected RMS
                    let jitter = ((tick * 0.4 + i as f64 * 1.5).sin() * 0.15).max(-0.15);
                    let h_val = (level * shape_factor + jitter).clamp(0.05, 1.0);
                    (3.0 + h_val * 11.0) * eased
                };

                let by = inner_y + (inner_h - bh) / 2.;
                rounded(&c, bx, by, bar_w, bh, bar_w / 2.);

                // Dynamic gradient coloring per bar
                let bar_g = LinearGradient::new(0., by, 0., by + bh);
                bar_g.add_color_stop_rgba(
                    0.,
                    (rgb[0] * 1.25).min(1.),
                    (rgb[1] * 1.25).min(1.),
                    (rgb[2] * 1.25).min(1.),
                    0.95,
                );
                bar_g.add_color_stop_rgba(1., rgb[0] * 0.75, rgb[1] * 0.75, rgb[2] * 0.75, 0.85);
                c.set_source(&bar_g)?;
                c.fill()?;
            }
        } else {
            // Idle state: elegant micro indicator pill
            let pill_w = 26.;
            let pill_h = 3.5;
            let pill_x = left + (current_w - pill_w) / 2.;
            let pill_y = top + (current_h - pill_h) / 2. - 1.;
            rounded(&c, pill_x, pill_y, pill_w, pill_h, 1.75);
            c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.75);
            c.fill()?;
        }
    }
    Ok(surface.data()?.to_vec())
}

fn carapace(c: &Context, left: f64, right: f64, bottom: f64, top: f64) {
    let mid = (left + right) / 2.;
    let span = right - left;
    c.new_sub_path();
    c.move_to(left, bottom);
    c.line_to(left + span * 0.08, top + (bottom - top) * 0.42);
    c.curve_to(left + span * 0.22, top, left + span * 0.36, top, mid, top);
    c.curve_to(
        right - span * 0.36,
        top,
        right - span * 0.22,
        top,
        right - span * 0.08,
        top + (bottom - top) * 0.42,
    );
    c.line_to(right, bottom);
    c.close_path();
}

fn rounded(c: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    c.new_sub_path();
    c.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.);
    c.arc(x + w - r, y + h - r, r, 0., std::f64::consts::FRAC_PI_2);
    c.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    c.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        std::f64::consts::PI * 1.5,
    );
    c.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_keeps_transparent_corners_and_animated_shape() {
        let small = shell(176, 44, [55, 170, 130], 0., 0., 0.).unwrap();
        let expanded = shell(176, 44, [235, 65, 75], 1., 0.7, 3.).unwrap();
        assert_ne!(small, expanded);
        assert_eq!(&small[..176 * 24 * 4], &vec![0; 176 * 24 * 4]);
        assert_eq!(&expanded[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn ease_curve_starts_at_zero_ends_at_one_and_is_monotonic() {
        assert_eq!(ease(0.), 0.);
        assert!((ease(1.) - 1.).abs() < 1e-9);
        for i in 1..=20 {
            assert!(ease(i as f64 / 20.) >= ease((i - 1) as f64 / 20.));
        }
    }
}
