//! Custom Cairo graphics primitives for Observatory (`ProgressRing` and
//! `ThroughputSparkline`).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gtk::prelude::*;
use libadwaita::prelude::*;

pub const RING_SIZE: i32 = 48;
pub const RING_STROKE_WIDTH: f64 = 3.5;
pub const SPARKLINE_WIDTH: i32 = 200;
pub const SPARKLINE_HEIGHT: i32 = 44;
pub const SPARKLINE_MAX_SAMPLES: usize = 60;

/// Clamp a progress fraction to `0.0..=1.0`, mapping NaN/infinities to `0.0`.
pub fn clamp_fraction(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingGeometry {
    pub cx: f64,
    pub cy: f64,
    pub radius: f64,
    pub start_angle: f64,
    pub end_angle: f64,
    pub fraction: f64,
}

/// Compute centered Cairo arc geometry starting from 12 o'clock (`-PI / 2`).
pub fn ring_geometry(width: i32, height: i32, stroke_width: f64, fraction: f64) -> RingGeometry {
    let w = f64::from(width.max(1));
    let h = f64::from(height.max(1));
    let stroke = if stroke_width.is_finite() && stroke_width > 0.0 {
        stroke_width
    } else {
        0.0
    };
    let cx = w / 2.0;
    let cy = h / 2.0;
    let radius = (w.min(h) / 2.0 - stroke / 2.0 - 0.5).max(1.0);
    let fraction = clamp_fraction(fraction);
    let start_angle = -std::f64::consts::FRAC_PI_2;
    let end_angle = start_angle + std::f64::consts::TAU * fraction;
    RingGeometry {
        cx,
        cy,
        radius,
        start_angle,
        end_angle,
        fraction,
    }
}

pub fn ring_percent_label(fraction: Option<f64>) -> String {
    match fraction {
        Some(value) => format!("{:.0}%", clamp_fraction(value) * 100.0),
        None => String::new(),
    }
}

/// Bounded ring buffer of 1-second download throughput samples (`bytes/s`).
#[derive(Debug, Clone, PartialEq)]
pub struct SparklineBuffer {
    samples: VecDeque<f64>,
    max_samples: usize,
}

impl SparklineBuffer {
    pub fn new(max_samples: usize) -> Self {
        let cap = max_samples.max(1);
        Self {
            samples: VecDeque::with_capacity(cap),
            max_samples: cap,
        }
    }

    pub fn push(&mut self, bytes_per_sec: f64) {
        let clean = if bytes_per_sec.is_finite() && bytes_per_sec >= 0.0 {
            bytes_per_sec
        } else {
            0.0
        };
        while self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back(clean);
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    #[allow(dead_code)] // Inspected in Phase 2 (#35) telemetry and unit tests.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[allow(dead_code)] // Inspected in Phase 2 (#35) telemetry and unit tests.
    pub fn latest(&self) -> f64 {
        self.samples.back().copied().unwrap_or(0.0)
    }

    pub fn peak(&self) -> f64 {
        self.samples.iter().copied().fold(0.0_f64, f64::max)
    }

    /// Map buffered throughput samples into drawable `(x, y)` coordinates within
    /// `[0, width] × [padding, height - padding]`.
    pub fn points(&self, width: f64, height: f64, padding: f64) -> Vec<(f64, f64)> {
        if self.samples.is_empty() || !width.is_finite() || !height.is_finite() {
            return Vec::new();
        }
        if width <= 0.0 || height <= 0.0 {
            return Vec::new();
        }
        let pad = padding
            .max(0.0)
            .min(((height - 1.0).max(0.0)) / 2.0);
        let usable_h = (height - 2.0 * pad).max(1.0);
        let baseline_y = pad + usable_h;
        let peak = self.peak();

        let y_for = |sample: f64| -> f64 {
            if peak <= 0.0 {
                baseline_y
            } else {
                baseline_y - (sample / peak).clamp(0.0, 1.0) * usable_h
            }
        };

        if self.samples.len() == 1 {
            let y = y_for(self.samples[0]);
            return vec![(0.0, y), (width, y)];
        }

        let step = width / (self.samples.len() - 1) as f64;
        self.samples
            .iter()
            .enumerate()
            .map(|(i, sample)| (i as f64 * step, y_for(*sample)))
            .collect()
    }
}

pub fn format_throughput(bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return "0 B/s".into();
    }
    const K: f64 = 1024.0;
    if bytes_per_sec < K {
        return format!("{bytes_per_sec:.0} B/s");
    }
    if bytes_per_sec < K * K {
        return format_rate_unit(bytes_per_sec / K, "KB/s");
    }
    if bytes_per_sec < K * K * K {
        return format_rate_unit(bytes_per_sec / (K * K), "MB/s");
    }
    format_rate_unit(bytes_per_sec / (K * K * K), "GB/s")
}

fn format_rate_unit(value: f64, unit: &str) -> String {
    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

/// Circular 48×48 progress ring driven by `Adw.SpringAnimation`.
pub struct ProgressRing {
    overlay: gtk::Overlay,
    area: gtk::DrawingArea,
    label: gtk::Label,
    displayed: Rc<Cell<f64>>,
    target: Rc<Cell<f64>>,
    completed: Rc<Cell<bool>>,
    animation: libadwaita::SpringAnimation,
}

impl ProgressRing {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .content_width(RING_SIZE)
            .content_height(RING_SIZE)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        let label = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(["numeric", "progress-ring-label"])
            .build();

        let overlay = gtk::Overlay::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(RING_SIZE)
            .height_request(RING_SIZE)
            .build();
        overlay.set_child(Some(&area));
        overlay.add_overlay(&label);

        let displayed = Rc::new(Cell::new(0.0));
        let target = Rc::new(Cell::new(0.0));
        let completed = Rc::new(Cell::new(false));

        {
            let displayed = displayed.clone();
            let completed = completed.clone();
            area.set_draw_func(move |drawing, cr, width, height| {
                draw_ring(drawing, cr, width, height, displayed.get(), completed.get());
            });
        }

        let area_for_anim = area.clone();
        let displayed_for_anim = displayed.clone();
        let anim_target = libadwaita::CallbackAnimationTarget::new(move |value| {
            displayed_for_anim.set(clamp_fraction(value));
            area_for_anim.queue_draw();
        });
        let spring_params = libadwaita::SpringParams::new(1.0, 1.0, 120.0);
        let animation =
            libadwaita::SpringAnimation::new(&area, 0.0, 0.0, spring_params, anim_target);
        animation.set_clamp(true);

        Self {
            overlay,
            area,
            label,
            displayed,
            target,
            completed,
            animation,
        }
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.overlay
    }

    pub fn set_progress(&self, fraction: Option<f64>, completed: bool) {
        self.completed.set(completed);
        let text = ring_percent_label(fraction);
        self.label.set_text(&text);

        let Some(raw) = fraction else {
            self.animation.reset();
            self.target.set(0.0);
            self.displayed.set(0.0);
            self.area.queue_draw();
            return;
        };

        let next = clamp_fraction(raw);
        let current = self.displayed.get();
        self.target.set(next);

        if !self.area.is_mapped() || (next - current).abs() < 1e-4 {
            self.animation.reset();
            self.displayed.set(next);
            self.area.queue_draw();
            return;
        }

        let initial_velocity = if self.animation.state() == libadwaita::AnimationState::Playing {
            self.animation.velocity()
        } else {
            0.0
        };
        self.animation.set_value_from(current);
        self.animation.set_value_to(next);
        self.animation.set_initial_velocity(initial_velocity);
        self.animation.play();
    }

    #[cfg(test)]
    pub fn displayed_fraction(&self) -> f64 {
        self.displayed.get()
    }

    #[cfg(test)]
    pub fn target_fraction(&self) -> f64 {
        self.target.get()
    }

    #[cfg(test)]
    pub fn label_text(&self) -> String {
        self.label.text().to_string()
    }
}

fn draw_ring(
    drawing: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    fraction: f64,
    completed: bool,
) {
    let geom = ring_geometry(width, height, RING_STROKE_WIDTH, fraction);
    let fg = drawing.color();

    // Subtle track arc (`alpha(@window_fg_color, 0.12)`).
    cr.set_source_rgba(
        f64::from(fg.red()),
        f64::from(fg.green()),
        f64::from(fg.blue()),
        f64::from(fg.alpha()) * 0.12,
    );
    cr.set_line_width(RING_STROKE_WIDTH);
    cr.arc(geom.cx, geom.cy, geom.radius, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();

    if geom.fraction > 0.001 {
        let style_mgr = libadwaita::StyleManager::default();
        let accent = if completed {
            libadwaita::AccentColor::Green.to_standalone_rgba(style_mgr.is_dark())
        } else {
            style_mgr.accent_color_rgba()
        };
        cr.set_source_rgba(
            f64::from(accent.red()),
            f64::from(accent.green()),
            f64::from(accent.blue()),
            f64::from(accent.alpha()),
        );
        cr.set_line_width(RING_STROKE_WIDTH);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.arc(geom.cx, geom.cy, geom.radius, geom.start_angle, geom.end_angle);
        let _ = cr.stroke();
    }
}

/// Phase-2 live download throughput sparkline (`200 × 44` Cairo polyline +
/// vertical linear gradient). Hidden in Phase 1 until Phase 2 supplies live
/// MB/s samples.
pub struct ThroughputSparkline {
    root: gtk::Box,
    area: gtk::DrawingArea,
    speed_label: gtk::Label,
    peak_label: gtk::Label,
    transferred_label: gtk::Label,
    buffer: Rc<RefCell<SparklineBuffer>>,
}

impl ThroughputSparkline {
    pub fn new() -> Self {
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::builder()
            .label("Throughput")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["caption", "dimmed"])
            .build();
        let speed_label = gtk::Label::builder()
            .label("0 B/s")
            .halign(gtk::Align::End)
            .single_line_mode(true)
            .css_classes(["caption", "numeric", "accent"])
            .build();
        header.append(&title);
        header.append(&speed_label);

        let area = gtk::DrawingArea::builder()
            .content_width(SPARKLINE_WIDTH)
            .content_height(SPARKLINE_HEIGHT)
            .hexpand(true)
            .build();

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let peak_label = gtk::Label::builder()
            .label("Peak 0 B/s")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .single_line_mode(true)
            .css_classes(["caption", "dimmed", "numeric"])
            .build();
        let transferred_label = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::End)
            .single_line_mode(true)
            .css_classes(["caption", "dimmed", "numeric"])
            .build();
        footer.append(&peak_label);
        footer.append(&transferred_label);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_visible(false);
        root.append(&header);
        root.append(&area);
        root.append(&footer);

        let buffer = Rc::new(RefCell::new(SparklineBuffer::new(SPARKLINE_MAX_SAMPLES)));
        {
            let buffer = buffer.clone();
            area.set_draw_func(move |_drawing, cr, width, height| {
                draw_sparkline(cr, width, height, &buffer.borrow());
            });
        }

        Self {
            root,
            area,
            speed_label,
            peak_label,
            transferred_label,
            buffer,
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    #[allow(dead_code)] // Phase 2 (#35) feeds live MB/s samples here.
    pub fn push_sample(&self, bytes_per_sec: f64, transferred: Option<(u64, u64)>) {
        let mut buf = self.buffer.borrow_mut();
        buf.push(bytes_per_sec);
        let latest = buf.latest();
        let peak = buf.peak();
        drop(buf);

        self.speed_label.set_text(&format_throughput(latest));
        self.peak_label
            .set_text(&format!("Peak {}", format_throughput(peak)));
        match transferred {
            Some((done, total)) if total > 0 => {
                self.transferred_label.set_text(&format!(
                    "{} / {}",
                    fedora_updater::format_bytes(done),
                    fedora_updater::format_bytes(total),
                ));
                self.transferred_label.set_visible(true);
            }
            _ => {
                self.transferred_label.set_text("");
                self.transferred_label.set_visible(false);
            }
        }
        self.area.queue_draw();
    }

    #[allow(dead_code)] // Phase 2 (#35) resets sparkline between runs.
    pub fn clear(&self) {
        self.buffer.borrow_mut().clear();
        self.speed_label.set_text("0 B/s");
        self.peak_label.set_text("Peak 0 B/s");
        self.transferred_label.set_text("");
        self.area.queue_draw();
    }

    #[allow(dead_code)] // Phase 2 (#35) toggles sparkline visibility cleanly.
    pub fn set_visible(&self, visible: bool) {
        self.root.set_visible(visible);
    }
}

fn draw_sparkline(
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    buffer: &SparklineBuffer,
) {
    if buffer.is_empty() || width <= 0 || height <= 0 {
        return;
    }
    let w = f64::from(width);
    let h = f64::from(height);
    let pts = buffer.points(w, h, 3.0);
    if pts.len() < 2 {
        return;
    }

    let accent = libadwaita::StyleManager::default().accent_color_rgba();
    let r = f64::from(accent.red());
    let g = f64::from(accent.green());
    let b = f64::from(accent.blue());
    let a = f64::from(accent.alpha());

    // Area fill gradient underneath polyline.
    let gradient = gtk::cairo::LinearGradient::new(0.0, 0.0, 0.0, h);
    gradient.add_color_stop_rgba(0.0, r, g, b, a * 0.28);
    gradient.add_color_stop_rgba(1.0, r, g, b, 0.0);

    cr.move_to(pts[0].0, h);
    for (x, y) in &pts {
        cr.line_to(*x, *y);
    }
    cr.line_to(pts[pts.len() - 1].0, h);
    cr.close_path();
    let _ = cr.set_source(&gradient);
    let _ = cr.fill();

    // Top 2px polyline.
    cr.move_to(pts[0].0, pts[0].1);
    for (x, y) in &pts[1..] {
        cr.line_to(*x, *y);
    }
    cr.set_source_rgba(r, g, b, a);
    cr.set_line_width(2.0);
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    let _ = cr.stroke();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_fraction_handles_bounds_and_non_finite_values() {
        assert_eq!(clamp_fraction(-0.5), 0.0);
        assert_eq!(clamp_fraction(0.0), 0.0);
        assert_eq!(clamp_fraction(0.42), 0.42);
        assert_eq!(clamp_fraction(1.0), 1.0);
        assert_eq!(clamp_fraction(2.7), 1.0);
        assert_eq!(clamp_fraction(f64::NAN), 0.0);
        assert_eq!(clamp_fraction(f64::INFINITY), 0.0);
        assert_eq!(clamp_fraction(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn ring_geometry_centers_and_starts_at_top_dead_center() {
        let g = ring_geometry(48, 48, 3.5, 0.5);
        assert!((g.cx - 24.0).abs() < 1e-9);
        assert!((g.cy - 24.0).abs() < 1e-9);
        assert!((g.radius - 21.75).abs() < 1e-9);
        assert!((g.start_angle + std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        assert!((g.end_angle - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        assert!((g.fraction - 0.5).abs() < 1e-9);

        // Degenerate container dimensions never produce non-positive radius.
        let tiny = ring_geometry(0, 0, 10.0, 1.5);
        assert!(tiny.radius >= 1.0);
        assert_eq!(tiny.fraction, 1.0);

        assert_eq!(ring_percent_label(None), "");
        assert_eq!(ring_percent_label(Some(0.724)), "72%");
        assert_eq!(ring_percent_label(Some(1.8)), "100%");
    }

    #[test]
    fn sparkline_buffer_evicts_oldest_sanitizes_and_tracks_peak() {
        let mut buf = SparklineBuffer::new(3);
        assert!(buf.is_empty());
        assert_eq!(buf.peak(), 0.0);

        buf.push(-50.0);
        buf.push(f64::NAN);
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.latest(), 0.0);
        assert_eq!(buf.peak(), 0.0);

        buf.push(10.0 * 1024.0 * 1024.0);
        buf.push(25.0 * 1024.0 * 1024.0);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.peak(), 25.0 * 1024.0 * 1024.0);

        // With capacity 3 holding [0.0, 10MB, 25MB], pushing 3 more samples evicts 25MB.
        buf.push(5.0);
        buf.push(6.0);
        buf.push(4.0);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.latest(), 4.0);
        assert_eq!(buf.peak(), 6.0);

        buf.clear();
        assert!(buf.is_empty());
        assert!(buf.points(200.0, 44.0, 3.0).is_empty());
    }

    #[test]
    fn sparkline_points_scale_cleanly_for_one_flat_and_multi_samples() {
        let mut one = SparklineBuffer::new(SPARKLINE_MAX_SAMPLES);
        one.push(100.0);
        let pts = one.points(200.0, 44.0, 3.0);
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].0, 0.0);
        assert_eq!(pts[1].0, 200.0);
        assert!((pts[0].1 - 3.0).abs() < 1e-9);
        assert!((pts[1].1 - 3.0).abs() < 1e-9);

        let mut flat_zero = SparklineBuffer::new(SPARKLINE_MAX_SAMPLES);
        flat_zero.push(0.0);
        flat_zero.push(0.0);
        let zero_pts = flat_zero.points(200.0, 44.0, 3.0);
        assert_eq!(zero_pts.len(), 2);
        assert!((zero_pts[0].1 - 41.0).abs() < 1e-9);
        assert!((zero_pts[1].1 - 41.0).abs() < 1e-9);

        let mut wave = SparklineBuffer::new(SPARKLINE_MAX_SAMPLES);
        wave.push(10.0);
        wave.push(20.0);
        wave.push(0.0);
        let wave_pts = wave.points(200.0, 44.0, 3.0);
        assert_eq!(wave_pts.len(), 3);
        assert!((wave_pts[0].0 - 0.0).abs() < 1e-9);
        assert!((wave_pts[1].0 - 100.0).abs() < 1e-9);
        assert!((wave_pts[2].0 - 200.0).abs() < 1e-9);
        assert!((wave_pts[1].1 - 3.0).abs() < 1e-9, "peak sits at top pad");
        assert!((wave_pts[2].1 - 41.0).abs() < 1e-9, "zero sits at baseline");

        assert!(wave.points(0.0, 44.0, 3.0).is_empty());
        assert!(wave.points(200.0, 0.0, 3.0).is_empty());
    }

    #[test]
    fn format_throughput_formats_expected_units() {
        assert_eq!(format_throughput(-1.0), "0 B/s");
        assert_eq!(format_throughput(f64::NAN), "0 B/s");
        assert_eq!(format_throughput(0.0), "0 B/s");
        assert_eq!(format_throughput(512.0), "512 B/s");
        assert_eq!(format_throughput(512.0 * 1024.0), "512 KB/s");
        assert_eq!(format_throughput(24.8 * 1024.0 * 1024.0), "24.8 MB/s");
        assert_eq!(format_throughput(1.5 * 1024.0 * 1024.0 * 1024.0), "1.5 GB/s");
    }
}
