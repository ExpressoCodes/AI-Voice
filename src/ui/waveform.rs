use std::sync::{Arc, Mutex};

use gtk4::prelude::*;

const SAMPLE_WINDOW: usize = 512;
const BAR_COUNT: usize = 64;

pub struct WaveformWidget {
    drawing_area: gtk4::DrawingArea,
    samples: Arc<Mutex<Vec<f32>>>,
}

impl WaveformWidget {
    pub fn new() -> Self {
        let drawing_area = gtk4::DrawingArea::new();
        drawing_area.set_content_height(80);
        drawing_area.set_hexpand(true);
        drawing_area.set_visible(false);

        let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(vec![0.0; SAMPLE_WINDOW]));
        let samples_draw = Arc::clone(&samples);

        drawing_area.set_draw_func(move |area, cr, width, height| {
            let samples = samples_draw.lock().unwrap();

            // Get accent colour from the style context
            let style_ctx = area.style_context();
            let color = style_ctx.color();
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                0.8,
            );

            let bar_width = width as f64 / BAR_COUNT as f64;
            let center_y = height as f64 / 2.0;
            let chunk_size = SAMPLE_WINDOW / BAR_COUNT;

            for bar in 0..BAR_COUNT {
                let start = bar * chunk_size;
                let end = (start + chunk_size).min(samples.len());
                let rms = if end > start {
                    let sum_sq: f32 = samples[start..end].iter().map(|s| s * s).sum();
                    (sum_sq / (end - start) as f32).sqrt()
                } else {
                    0.0
                };

                let bar_height = (rms * height as f32 * 4.0).min(height as f32) as f64;
                let x = bar as f64 * bar_width + 1.0;
                let y = center_y - bar_height / 2.0;

                cr.rectangle(x, y, bar_width - 2.0, bar_height);
            }

            let _ = cr.fill();
        });

        Self {
            drawing_area,
            samples,
        }
    }

    pub fn widget(&self) -> &gtk4::DrawingArea {
        &self.drawing_area
    }

    /// Push new audio samples. Keeps only the last SAMPLE_WINDOW values.
    pub fn push_samples(&self, new_samples: &[f32]) {
        let mut samples = self.samples.lock().unwrap();
        samples.extend_from_slice(new_samples);
        let len = samples.len();
        if len > SAMPLE_WINDOW {
            samples.drain(..len - SAMPLE_WINDOW);
        }
        drop(samples);
        self.drawing_area.queue_draw();
    }
}

impl Default for WaveformWidget {
    fn default() -> Self {
        Self::new()
    }
}
