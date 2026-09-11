//! Row-scratch Lanczos3 for the optimizer's bounded u8 images.
//!
//! Kernel, support, normalization and rounding follow image 0.25.10
//! src/imageops/sample.rs (https://github.com/image-rs/image), under MIT:
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use image::{ImageBuffer, Pixel};

const MAX_CACHED_WEIGHTS_BYTES: usize = 1024 * 1024;

struct Span {
    start: usize,
    offset: usize,
    len: usize,
}

fn bounds(source: u32, target: u32, output: u32) -> (usize, usize, f32, f32) {
    let ratio = source as f32 / target as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let support = 3.0 * sratio;
    let input = (output as f32 + 0.5) * ratio;
    let left = ((input - support).floor() as i64).clamp(0, i64::from(source) - 1) as usize;
    let right = ((input + support).ceil() as i64).clamp(left as i64 + 1, i64::from(source)) as usize;
    (left, right, input - 0.5, sratio)
}

fn sinc(t: f32) -> f32 {
    let a = t * std::f32::consts::PI;
    if t == 0.0 { 1.0 } else { a.sin() / a }
}

fn append_weights(source: u32, target: u32, output: u32, weights: &mut Vec<f32>) -> Span {
    let (left, right, input, sratio) = bounds(source, target, output);
    let offset = weights.len();
    let mut sum = 0.0;
    for i in left..right {
        let x = (i as f32 - input) / sratio;
        let w = if x.abs() < 3.0 { sinc(x) * sinc(x / 3.0) } else { 0.0 };
        weights.push(w);
        sum += w;
    }
    for w in &mut weights[offset..] {
        *w /= sum;
    }
    Span {
        start: left,
        offset,
        len: right - left,
    }
}

fn cached_weights(source: u32, target: u32) -> Option<(Vec<Span>, Vec<f32>)> {
    let metadata_bytes = (target as usize).checked_mul(std::mem::size_of::<Span>())?;
    let available = MAX_CACHED_WEIGHTS_BYTES.checked_sub(metadata_bytes)? / std::mem::size_of::<f32>();
    let mut count = 0_usize;
    for x in 0..target {
        let (left, right, _, _) = bounds(source, target, x);
        count = count.checked_add(right - left)?;
        if count > available {
            return None;
        }
    }
    let mut spans = Vec::with_capacity(target as usize);
    let mut weights = Vec::with_capacity(count);
    for x in 0..target {
        spans.push(append_weights(source, target, x, &mut weights));
    }
    Some((spans, weights))
}

pub(super) fn lanczos3<P>(image: &ImageBuffer<P, Vec<u8>>, width: u32, height: u32) -> ImageBuffer<P, Vec<u8>>
where
    P: Pixel<Subpixel = u8> + 'static,
{
    // The optimizer only calls this for a nonempty image with at least one
    // downsampled axis. An unchanged axis still needs its Lanczos convolution.
    let Some((spans, weights)) = cached_weights(image.width(), width) else {
        // ponytail: keep the existing scaler above the cache budget. Recomputing
        // kernels per output row would make wide, tall images much slower.
        return image::imageops::resize(image, width, height, image::imageops::FilterType::Lanczos3);
    };
    let channels = usize::from(P::CHANNEL_COUNT);
    let stride = image.width() as usize * channels;
    let mut output = ImageBuffer::<P, Vec<u8>>::new(width, height);
    output
        .set_color_space(image.color_space())
        .expect("an ImageBuffer has an RGB-compatible color space");
    let mut row = vec![0.0_f32; stride];
    let mut vertical_weights = Vec::new();
    let source = image.as_raw();
    for (y, output_row) in output.as_mut().chunks_exact_mut(width as usize * channels).enumerate() {
        vertical_weights.clear();
        let vertical = append_weights(image.height(), height, y as u32, &mut vertical_weights);
        row.fill(0.0);
        // Interchanging the sample/channel loops keeps each channel's ascending
        // tap accumulation unchanged, while reading each source row contiguously.
        for (tap, &weight) in vertical_weights.iter().enumerate() {
            let start = (vertical.start + tap) * stride;
            for (value, &sample) in row.iter_mut().zip(&source[start..start + stride]) {
                *value += f32::from(sample) * weight;
            }
        }
        for (x, pixel) in output_row.chunks_exact_mut(channels).enumerate() {
            let span = &spans[x];
            for (channel, sample) in pixel.iter_mut().enumerate() {
                let mut value = 0.0_f32;
                for (tap, &weight) in weights[span.offset..span.offset + span.len].iter().enumerate() {
                    value += row[(span.start + tap) * channels + channel] * weight;
                }
                // Do not clamp the vertical intermediate: negative lobes are
                // meaningful. Match image's FloatNearest only at final output.
                *sample = value.clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb, Rgba, imageops::FilterType};

    #[test]
    fn lanczos3_matches_image_pixels_at_boundaries() {
        fn compare<P: Pixel<Subpixel = u8> + 'static>() {
            // Uneven downsampling, unchanged axes, support crossing both edges,
            // one-pixel axes, and the existing scaler above the cache budget.
            for (w, h, nw, nh) in [
                (37, 29, 13, 11),
                (17, 9, 17, 4),
                (9, 17, 4, 17),
                (7, 5, 1, 1),
                (1, 19, 1, 7),
                (19, 1, 7, 1),
                (3, 2, 2, 1),
                (40001, 2, 20000, 1),
            ] {
                let mut state = 0x91e1_0da5_u32;
                let raw = (0..w * h * u32::from(P::CHANNEL_COUNT))
                    .map(|i| {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        match i % 5 {
                            0 => 0,
                            1 => 255,
                            _ => state as u8,
                        }
                    })
                    .collect();
                let image = ImageBuffer::<P, Vec<u8>>::from_raw(w, h, raw).unwrap();
                let expected = image::imageops::resize(&image, nw, nh, FilterType::Lanczos3);
                let actual = lanczos3(&image, nw, nh);
                assert_eq!(
                    actual.as_raw(),
                    expected.as_raw(),
                    "{} channels {w}x{h}->{nw}x{nh}",
                    P::CHANNEL_COUNT
                );
                assert_eq!(actual.color_space(), expected.color_space());
            }
        }
        compare::<Luma<u8>>();
        compare::<Rgb<u8>>();
        compare::<Rgba<u8>>();
    }
}
