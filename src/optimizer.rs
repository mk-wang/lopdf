use crate::Stream;
use crate::content::Content;
use crate::{Dictionary, Document, Error, Object, ObjectId, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_OPTIMIZER_DEPTH: usize = 128;
const MAX_OPTIMIZED_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// Selects the optimizer stages and their detailed policies.
///
/// `Default` is structural optimization only, equivalent to
/// [`OptimizeOptions::only_structure`]. Use the named constructors when the
/// intended stage selection should be explicit.
#[derive(Debug, Clone)]
pub struct OptimizeOptions {
    pub prune_unused_objects: bool,
    pub deduplicate_objects: bool,
    pub renumber_objects: bool,
    pub compress_streams: bool,
    pub fonts: Option<FontOptimizationOptions>,
    pub images: Option<ImageOptimizationOptions>,
}

impl OptimizeOptions {
    /// Enables every optimization stage.
    ///
    /// Font subsetting is always part of full-document optimization. The image
    /// policy controls only whether image rewriting may trade visual fidelity
    /// for additional size reduction. Requires the `font_subsetting` and
    /// `embed_image` crate features.
    pub fn all(image_policy: ImageCompressionPolicy) -> Self {
        Self {
            fonts: Some(FontOptimizationOptions::default()),
            images: Some(ImageOptimizationOptions::for_policy(image_policy)),
            ..Self::default()
        }
    }

    /// Enables only structural optimization.
    pub fn only_structure() -> Self {
        Self::default()
    }

    /// Enables only font subsetting. Requires the `font_subsetting` crate
    /// feature.
    pub fn only_fonts() -> Self {
        Self {
            fonts: Some(FontOptimizationOptions::default()),
            ..Self::disabled()
        }
    }

    /// Enables only image rewriting. Requires the `embed_image` crate feature.
    pub fn only_images(image_policy: ImageCompressionPolicy) -> Self {
        Self {
            images: Some(ImageOptimizationOptions::for_policy(image_policy)),
            ..Self::disabled()
        }
    }

    fn disabled() -> Self {
        Self {
            prune_unused_objects: false,
            deduplicate_objects: false,
            renumber_objects: false,
            compress_streams: false,
            fonts: None,
            images: None,
        }
    }
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            prune_unused_objects: true,
            deduplicate_objects: true,
            renumber_objects: true,
            compress_streams: true,
            fonts: None,
            images: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FontOptimizationOptions {
    pub subset_only_if_smaller: bool,
    pub max_decoded_font_bytes: usize,
    pub max_decoded_content_bytes: usize,
}

impl Default for FontOptimizationOptions {
    fn default() -> Self {
        Self {
            subset_only_if_smaller: true,
            max_decoded_font_bytes: 128 * 1024 * 1024,
            max_decoded_content_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRecompression {
    Never,
    Preserve { jpeg_quality: u8 },
    Flate,
    Jpeg { quality: u8 },
}

/// Controls whether image optimization may sacrifice visual fidelity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImageCompressionPolicy {
    /// Keeps the original dimensions, leaves existing lossy images untouched,
    /// and uses only lossless recompression.
    #[default]
    Conservative,
    /// May downsample images, convert lossless grayscale or color images to
    /// JPEG, and convert ICCBased CMYK images to sRGB for additional size
    /// reduction.
    Aggressive,
}

#[derive(Debug, Clone, Copy)]
pub struct ImageClassOptions {
    pub subsample_above_dpi: Option<f32>,
    pub subsample_to_dpi: f32,
    pub recompression: ImageRecompression,
}

impl ImageClassOptions {
    fn aggressive_jpeg(target_dpi: f32) -> Self {
        Self {
            subsample_above_dpi: Some(target_dpi),
            subsample_to_dpi: target_dpi,
            recompression: ImageRecompression::Jpeg { quality: 70 },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageResizeFilter {
    Nearest,
    Triangle,
    CatmullRom,
    Gaussian,
    Lanczos3,
}

#[derive(Debug, Clone)]
pub struct ImageOptimizationOptions {
    pub bitonal: ImageClassOptions,
    pub grayscale_lossy: ImageClassOptions,
    pub grayscale_lossless: ImageClassOptions,
    pub color_lossy: ImageClassOptions,
    pub color_lossless: ImageClassOptions,
    pub resize_filter: ImageResizeFilter,
    pub recompress_only_if_smaller: bool,
    /// Converts valid ICCBased CMYK images to sRGB before JPEG encoding.
    ///
    /// Disabled by default to preserve print-oriented CMYK/ICC semantics.
    pub convert_icc_cmyk_to_srgb: bool,
    pub max_decoded_image_bytes: usize,
    pub max_decoded_content_bytes: usize,
}

impl ImageOptimizationOptions {
    /// Creates image options for the requested visual-fidelity policy.
    pub fn for_policy(policy: ImageCompressionPolicy) -> Self {
        match policy {
            ImageCompressionPolicy::Conservative => Self::default(),
            ImageCompressionPolicy::Aggressive => Self::aggressive(),
        }
    }

    /// Allows downsampling and lossy JPEG conversion for additional size
    /// reduction.
    pub fn aggressive() -> Self {
        Self {
            bitonal: ImageClassOptions {
                subsample_above_dpi: Some(450.0),
                subsample_to_dpi: 300.0,
                recompression: ImageRecompression::Flate,
            },
            grayscale_lossy: ImageClassOptions::aggressive_jpeg(200.0),
            grayscale_lossless: ImageClassOptions::aggressive_jpeg(200.0),
            color_lossy: ImageClassOptions::aggressive_jpeg(150.0),
            color_lossless: ImageClassOptions::aggressive_jpeg(150.0),
            resize_filter: ImageResizeFilter::Lanczos3,
            recompress_only_if_smaller: true,
            convert_icc_cmyk_to_srgb: true,
            max_decoded_image_bytes: 512 * 1024 * 1024,
            max_decoded_content_bytes: 64 * 1024 * 1024,
        }
    }
}

impl Default for ImageOptimizationOptions {
    fn default() -> Self {
        Self {
            bitonal: ImageClassOptions {
                subsample_above_dpi: None,
                subsample_to_dpi: 300.0,
                recompression: ImageRecompression::Flate,
            },
            grayscale_lossy: ImageClassOptions {
                subsample_above_dpi: None,
                subsample_to_dpi: 200.0,
                recompression: ImageRecompression::Never,
            },
            grayscale_lossless: ImageClassOptions {
                subsample_above_dpi: None,
                subsample_to_dpi: 200.0,
                recompression: ImageRecompression::Flate,
            },
            color_lossy: ImageClassOptions {
                subsample_above_dpi: None,
                subsample_to_dpi: 150.0,
                recompression: ImageRecompression::Never,
            },
            color_lossless: ImageClassOptions {
                subsample_above_dpi: None,
                subsample_to_dpi: 150.0,
                recompression: ImageRecompression::Flate,
            },
            resize_filter: ImageResizeFilter::Lanczos3,
            recompress_only_if_smaller: true,
            convert_icc_cmyk_to_srgb: false,
            max_decoded_image_bytes: 512 * 1024 * 1024,
            max_decoded_content_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructureOptimizationReport {
    pub compressed_streams: usize,
    pub stream_bytes_saved: usize,
    pub deduplicated_objects: usize,
    pub pruned_objects: usize,
    pub renumbered_objects: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FontSkipReason {
    InvalidContentCode,
    MissingFontProgram,
    UnsupportedFont,
    UnsupportedEncoding,
    UnsupportedCidMapping,
    DecodeFailed,
    DecodeLimitExceeded,
    SubsetFailed,
    NotSmaller,
    InteractiveFormResource,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontOptimizationReport {
    pub total_fonts: usize,
    pub subset_fonts: usize,
    pub original_program_bytes: usize,
    pub optimized_program_bytes: usize,
    pub font_bytes_saved: usize,
    pub content_streams_skipped: usize,
    pub skips: BTreeMap<FontSkipReason, usize>,
}

#[cfg(feature = "font_subsetting")]
impl FontOptimizationReport {
    fn skip(&mut self, reason: FontSkipReason) {
        *self.skips.entry(reason).or_default() += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImageSkipReason {
    PolicyDisabled,
    Masked,
    UnsupportedColorSpace,
    UnsupportedFilter,
    UnsupportedBitsPerComponent,
    InvalidDimensions,
    DecodeFailed,
    DecodeLimitExceeded,
    NotSmaller,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageOptimizationReport {
    pub total_images: usize,
    pub rewritten_images: usize,
    pub resized_images: usize,
    pub original_stream_bytes: usize,
    pub optimized_stream_bytes: usize,
    pub image_bytes_saved: usize,
    pub inline_images_skipped: usize,
    pub content_streams_skipped: usize,
    pub skips: BTreeMap<ImageSkipReason, usize>,
}

#[cfg(feature = "embed_image")]
impl ImageOptimizationReport {
    fn skip(&mut self, reason: ImageSkipReason) {
        *self.skips.entry(reason).or_default() += 1;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptimizeReport {
    pub structure: StructureOptimizationReport,
    pub fonts: FontOptimizationReport,
    pub images: ImageOptimizationReport,
}

#[cfg(feature = "font_subsetting")]
mod fonts;

#[cfg(feature = "embed_image")]
mod resample;

impl Document {
    pub fn optimize(&mut self, options: &OptimizeOptions) -> Result<OptimizeReport> {
        if self.is_encrypted() {
            return Err(Error::InvalidStream(
                "cannot optimize a PDF that is still encrypted; load it with a valid password first".to_string(),
            ));
        }

        // Check optional feature requirements before any optimizer stage can
        // mutate the document. This keeps a profile that mixes unavailable
        // stages from applying a partial transformation and then returning an
        // `Unimplemented` error.
        #[cfg(not(feature = "font_subsetting"))]
        if options.fonts.is_some() {
            return Err(Error::Unimplemented(
                "font optimization requires the `font_subsetting` feature",
            ));
        }
        #[cfg(not(feature = "embed_image"))]
        if options.images.is_some() {
            return Err(Error::Unimplemented(
                "image optimization requires the `embed_image` feature",
            ));
        }

        let fonts = if let Some(font_options) = options.fonts.as_ref() {
            #[cfg(feature = "font_subsetting")]
            {
                self.optimize_fonts(font_options)?
            }
            #[cfg(not(feature = "font_subsetting"))]
            {
                let _ = font_options;
                return Err(Error::Unimplemented(
                    "font optimization requires the `font_subsetting` feature",
                ));
            }
        } else {
            FontOptimizationReport::default()
        };

        let images = if let Some(image_options) = options.images.as_ref() {
            #[cfg(feature = "embed_image")]
            {
                self.optimize_images(image_options)?
            }
            #[cfg(not(feature = "embed_image"))]
            {
                let _ = image_options;
                return Err(Error::Unimplemented(
                    "image optimization requires the `embed_image` feature",
                ));
            }
        } else {
            ImageOptimizationReport::default()
        };

        let mut structure = StructureOptimizationReport::default();
        if options.compress_streams {
            self.optimize_content_streams(&mut structure);
        }
        if options.compress_streams {
            for object in self.objects.values_mut() {
                let Object::Stream(stream) = object else {
                    continue;
                };
                let old_len = stream.content.len();
                if stream.allows_compression && stream.compress().is_ok() && stream.content.len() < old_len {
                    structure.compressed_streams += 1;
                    structure.stream_bytes_saved += old_len - stream.content.len();
                }
            }
        }

        if options.prune_unused_objects {
            structure.pruned_objects += self.prune_objects().len();
        }
        if options.deduplicate_objects {
            structure.deduplicated_objects = self.deduplicate_objects();
            if options.prune_unused_objects {
                structure.pruned_objects += self.prune_objects().len();
            }
        }
        if options.renumber_objects {
            structure.renumbered_objects = self
                .objects
                .keys()
                .enumerate()
                .filter(|(index, id)| id.0 != *index as u32 + 1 || id.1 != 0)
                .count();
            self.renumber_objects();
        }

        Ok(OptimizeReport {
            structure,
            fonts,
            images,
        })
    }

    fn optimize_content_streams(&mut self, report: &mut StructureOptimizationReport) {
        let mut content_ids = HashSet::new();
        for page_id in self.get_pages().into_values() {
            content_ids.extend(self.get_page_contents(page_id));
        }
        content_ids.extend(self.objects.iter().filter_map(|(&id, object)| {
            let stream = object.as_stream().ok()?;
            let is_form = stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form");
            let is_pattern = stream.dict.get(b"PatternType").and_then(Object::as_i64).ok() == Some(1);
            (is_form || is_pattern).then_some(id)
        }));
        for object in self.objects.values() {
            collect_type3_charproc_stream_ids(self, object, 0, &mut content_ids);
        }

        for id in content_ids {
            let replacement = self.objects.get(&id).and_then(|object| {
                let stream = object.as_stream().ok()?;
                if !stream.allows_compression {
                    return None;
                }
                let bytes = stream.get_plain_content_with_limit(MAX_OPTIMIZED_CONTENT_BYTES).ok()?;
                let content = Content::decode_strict(&bytes).ok()?;
                if content.operations.iter().any(|operation| operation.operator == "BI") {
                    return None;
                }
                let encoded = content.encode_minified().ok()?;
                let mut dictionary = stream.dict.clone();
                dictionary.remove(b"DecodeParms");
                dictionary.remove(b"Filter");
                let mut optimized = Stream::new(dictionary, encoded);
                optimized.compress().ok()?;
                (optimized.content.len() < stream.content.len()).then_some((stream.content.len(), optimized))
            });
            if let Some((old_len, replacement)) = replacement {
                report.compressed_streams += 1;
                report.stream_bytes_saved += old_len - replacement.content.len();
                self.objects.insert(id, Object::Stream(replacement));
            }
        }
    }

    fn deduplicate_objects(&mut self) -> usize {
        let mut removed = 0;
        loop {
            let mut buckets: HashMap<[u8; 32], Vec<ObjectId>> = HashMap::new();
            let mut replacements = BTreeMap::new();

            for (&id, object) in &self.objects {
                if object_is_page(object) {
                    continue;
                }
                let Some(digest) = object_digest(object, 0) else {
                    continue;
                };
                let bucket = buckets.entry(digest).or_default();
                if let Some(&original_id) = bucket.iter().find(|&&candidate_id| {
                    self.objects
                        .get(&candidate_id)
                        .is_some_and(|candidate| objects_equal(candidate, object, 0))
                }) {
                    replacements.insert(id, original_id);
                } else {
                    bucket.push(id);
                }
            }

            if replacements.is_empty() {
                break;
            }

            for (_, value) in self.trailer.iter_mut() {
                rewrite_references(value, &replacements);
            }
            // Deduplication considers every object, including objects that are
            // intentionally kept unreachable when pruning is disabled. Update
            // those graphs too, otherwise removing a duplicate leaves a
            // dangling reference in the serialized document.
            for object in self.objects.values_mut() {
                rewrite_references(object, &replacements);
            }
            for id in replacements.keys() {
                self.objects.remove(id);
            }
            removed += replacements.len();
        }
        removed
    }
}

fn rewrite_references(object: &mut Object, replacements: &BTreeMap<ObjectId, ObjectId>) {
    match object {
        Object::Reference(id) => {
            if let Some(replacement) = replacements.get(id) {
                *id = *replacement;
            }
        }
        Object::Array(values) => {
            for value in values {
                rewrite_references(value, replacements);
            }
        }
        Object::Dictionary(dictionary) => {
            for (_, value) in dictionary.iter_mut() {
                rewrite_references(value, replacements);
            }
        }
        Object::Stream(stream) => {
            for (_, value) in stream.dict.iter_mut() {
                rewrite_references(value, replacements);
            }
        }
        _ => {}
    }
}

fn collect_type3_charproc_stream_ids(
    document: &Document, object: &Object, depth: usize, content_ids: &mut HashSet<ObjectId>,
) {
    if depth >= MAX_OPTIMIZER_DEPTH {
        return;
    }
    match object {
        Object::Array(values) => {
            for value in values {
                if !matches!(value, Object::Reference(_)) {
                    collect_type3_charproc_stream_ids(document, value, depth + 1, content_ids);
                }
            }
        }
        Object::Dictionary(dictionary) => {
            if dictionary.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type3")
                && let Ok(charprocs) = dictionary.get(b"CharProcs")
                && let Ok((_, Object::Dictionary(charprocs))) = document.dereference(charprocs)
            {
                for value in charprocs.iter().map(|(_, value)| value) {
                    if let Ok((Some(id), Object::Stream(_))) = document.dereference(value) {
                        content_ids.insert(id);
                    }
                }
            }
            for value in dictionary.iter().map(|(_, value)| value) {
                if !matches!(value, Object::Reference(_)) {
                    collect_type3_charproc_stream_ids(document, value, depth + 1, content_ids);
                }
            }
        }
        Object::Stream(stream) => {
            for value in stream.dict.iter().map(|(_, value)| value) {
                if !matches!(value, Object::Reference(_)) {
                    collect_type3_charproc_stream_ids(document, value, depth + 1, content_ids);
                }
            }
        }
        _ => {}
    }
}

fn object_is_page(object: &Object) -> bool {
    match object {
        Object::Dictionary(dict) => dict.has_type(b"Page"),
        Object::Stream(stream) => stream.dict.has_type(b"Page"),
        _ => false,
    }
}

fn object_digest(object: &Object, depth: usize) -> Option<[u8; 32]> {
    if depth >= MAX_OPTIMIZER_DEPTH {
        return None;
    }
    let mut hasher = Sha256::new();
    hash_object(object, depth, &mut hasher)?;
    Some(hasher.finalize().into())
}

fn hash_object(object: &Object, depth: usize, hasher: &mut Sha256) -> Option<()> {
    if depth >= MAX_OPTIMIZER_DEPTH {
        return None;
    }
    match object {
        Object::Null => hasher.update([0]),
        Object::Boolean(value) => hasher.update([1, u8::from(*value)]),
        Object::Integer(value) => {
            hasher.update([2]);
            hasher.update(value.to_be_bytes());
        }
        Object::Real(value) => {
            hasher.update([3]);
            hasher.update(value.to_bits().to_be_bytes());
        }
        Object::Name(value) => {
            hasher.update([4]);
            hash_bytes(value, hasher);
        }
        Object::String(value, format) => {
            hasher.update([5, *format as u8]);
            hash_bytes(value, hasher);
        }
        Object::Array(values) => {
            hasher.update([6]);
            hasher.update(values.len().to_be_bytes());
            for value in values {
                hash_object(value, depth + 1, hasher)?;
            }
        }
        Object::Dictionary(dict) => {
            hasher.update([7]);
            hash_dictionary(dict, depth + 1, false, hasher)?;
        }
        Object::Stream(stream) => {
            hasher.update([8]);
            hash_dictionary(&stream.dict, depth + 1, true, hasher)?;
            hash_bytes(&stream.content, hasher);
        }
        Object::Reference((number, generation)) => {
            hasher.update([9]);
            hasher.update(number.to_be_bytes());
            hasher.update(generation.to_be_bytes());
        }
    }
    Some(())
}

fn hash_dictionary(dict: &Dictionary, depth: usize, stream_dict: bool, hasher: &mut Sha256) -> Option<()> {
    let mut entries: Vec<_> = dict
        .iter()
        .filter(|(key, _)| !stream_dict || key.as_slice() != b"Length")
        .collect();
    entries.sort_unstable_by_key(|(key, _)| *key);
    hasher.update(entries.len().to_be_bytes());
    for (key, value) in entries {
        hash_bytes(key, hasher);
        hash_object(value, depth + 1, hasher)?;
    }
    Some(())
}

fn hash_bytes(bytes: &[u8], hasher: &mut Sha256) {
    hasher.update(bytes.len().to_be_bytes());
    hasher.update(bytes);
}

fn objects_equal(left: &Object, right: &Object, depth: usize) -> bool {
    if depth >= MAX_OPTIMIZER_DEPTH {
        return false;
    }
    match (left, right) {
        (Object::Null, Object::Null) => true,
        (Object::Boolean(a), Object::Boolean(b)) => a == b,
        (Object::Integer(a), Object::Integer(b)) => a == b,
        (Object::Real(a), Object::Real(b)) => a.to_bits() == b.to_bits(),
        (Object::Name(a), Object::Name(b)) => a == b,
        (Object::String(a, af), Object::String(b, bf)) => a == b && af == bf,
        (Object::Array(a), Object::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| objects_equal(a, b, depth + 1))
        }
        (Object::Dictionary(a), Object::Dictionary(b)) => dictionaries_equal(a, b, depth + 1, false),
        (Object::Stream(a), Object::Stream(b)) => {
            a.content == b.content && dictionaries_equal(&a.dict, &b.dict, depth + 1, true)
        }
        (Object::Reference(a), Object::Reference(b)) => a == b,
        _ => false,
    }
}

fn dictionaries_equal(left: &Dictionary, right: &Dictionary, depth: usize, stream_dict: bool) -> bool {
    let left_len = left
        .iter()
        .filter(|(key, _)| !stream_dict || key.as_slice() != b"Length")
        .count();
    let right_len = right
        .iter()
        .filter(|(key, _)| !stream_dict || key.as_slice() != b"Length")
        .count();
    left_len == right_len
        && left.iter().all(|(key, value)| {
            if stream_dict && key.as_slice() == b"Length" {
                return true;
            }
            right.get(key).is_ok_and(|other| objects_equal(value, other, depth + 1))
        })
}

#[cfg(feature = "embed_image")]
mod images {
    use super::*;
    use crate::content::Content;
    use hayro_jpeg2000::{ColorSpace as JpxColorSpace, DecodeSettings as JpxDecodeSettings, Image as JpxImage};
    use image::imageops::FilterType;
    use image::{GrayImage, RgbImage, RgbaImage};
    use jpeg_decoder::{Decoder as JpegDecoder, PixelFormat};
    use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};
    use moxcms::{ColorProfile, DataColorSpace, Layout, RenderingIntent, Transform8BitExecutor, TransformOptions};
    use std::borrow::Cow;
    use std::io::Cursor;
    use std::sync::Arc;

    const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;

    #[derive(Debug, Clone, Copy, Default)]
    struct ImageUsage {
        minimum_dpi: Option<f32>,
        no_resize: bool,
    }

    impl ImageUsage {
        fn record(&mut self, dpi: f32) {
            if !dpi.is_finite() || dpi <= 0.0 {
                return;
            }
            self.minimum_dpi = Some(self.minimum_dpi.map_or(dpi, |current| current.min(dpi)));
        }

        fn protect_resize(&mut self) {
            self.no_resize = true;
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct Matrix {
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        e: f32,
        f: f32,
    }

    impl Matrix {
        const IDENTITY: Self = Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        };

        fn scale(value: f32) -> Self {
            Self {
                a: value,
                d: value,
                ..Self::IDENTITY
            }
        }

        fn concat(self, local: Self) -> Self {
            Self {
                a: self.a * local.a + self.c * local.b,
                b: self.b * local.a + self.d * local.b,
                c: self.a * local.c + self.c * local.d,
                d: self.b * local.c + self.d * local.d,
                e: self.a * local.e + self.c * local.f + self.e,
                f: self.b * local.e + self.d * local.f + self.f,
            }
        }

        fn dpi(self, width: u32, height: u32) -> Option<f32> {
            let displayed_width = self.a.hypot(self.b);
            let displayed_height = self.c.hypot(self.d);
            if displayed_width <= f32::EPSILON || displayed_height <= f32::EPSILON {
                return None;
            }
            Some((width as f32 * 72.0 / displayed_width).min(height as f32 * 72.0 / displayed_height))
        }
    }

    #[derive(Debug)]
    struct UsageAnalysis {
        images: BTreeMap<ObjectId, ImageUsage>,
        inline_images: usize,
        skipped_content_streams: usize,
        conservative: bool,
    }

    impl Document {
        pub(super) fn optimize_images(
            &mut self, options: &ImageOptimizationOptions,
        ) -> Result<ImageOptimizationReport> {
            let analysis = analyze_image_usage(self, options.max_decoded_content_bytes);
            let image_ids: Vec<_> = self
                .objects
                .iter()
                .filter_map(|(&id, object)| match object {
                    Object::Stream(stream) => stream
                        .dict
                        .get(b"Subtype")
                        .and_then(Object::as_name)
                        .ok()
                        .filter(|name| *name == b"Image")
                        .map(|_| id),
                    _ => None,
                })
                .collect();
            let protected_mask_ids = directly_referenced_mask_images(self, &image_ids);

            let mut report = ImageOptimizationReport {
                total_images: image_ids.len(),
                inline_images_skipped: analysis.inline_images,
                content_streams_skipped: analysis.skipped_content_streams,
                ..ImageOptimizationReport::default()
            };
            let mut icc_transforms = IccTransformCache::default();
            let mut obsolete_color_objects = HashSet::new();

            for id in image_ids {
                let Some(Object::Stream(original)) = self.objects.get(&id) else {
                    continue;
                };
                report.original_stream_bytes += original.content.len();
                if protected_mask_ids.contains(&id) {
                    report.optimized_stream_bytes += original.content.len();
                    report.skip(ImageSkipReason::Masked);
                    continue;
                }
                let usage = analysis.images.get(&id).copied().unwrap_or_default();
                // A failed usage walk cannot prove that the image is never
                // displayed at a larger size. Recompression remains safe,
                // but resizing must fail closed for the whole document.
                let allow_resize = analysis.skipped_content_streams == 0;
                match rewrite_image(self, original, usage, options, allow_resize, &mut icc_transforms) {
                    Ok(rewrite) => {
                        let old_len = original.content.len();
                        let new_len = rewrite.stream.content.len();
                        if options.recompress_only_if_smaller && new_len >= old_len {
                            report.optimized_stream_bytes += old_len;
                            report.skip(ImageSkipReason::NotSmaller);
                            continue;
                        }
                        report.rewritten_images += 1;
                        report.resized_images += usize::from(rewrite.resized);
                        report.optimized_stream_bytes += new_len;
                        report.image_bytes_saved += old_len.saturating_sub(new_len);
                        obsolete_color_objects.extend(rewrite.obsolete_color_objects);
                        self.objects.insert(id, Object::Stream(rewrite.stream));
                    }
                    Err(reason) => {
                        report.optimized_stream_bytes += original.content.len();
                        report.skip(reason);
                    }
                }
            }
            remove_unreferenced_color_objects(self, &obsolete_color_objects);
            Ok(report)
        }
    }
    fn directly_referenced_mask_images(document: &Document, image_ids: &[ObjectId]) -> HashSet<ObjectId> {
        let mut protected = HashSet::new();
        for id in image_ids {
            let Some(Object::Stream(stream)) = document.objects.get(id) else {
                continue;
            };
            for key in [b"SMask".as_slice(), b"Mask".as_slice()] {
                let Some((Some(mask_id), mask)) = stream
                    .dict
                    .get(key)
                    .ok()
                    .and_then(|value| document.dereference(value).ok())
                else {
                    continue;
                };
                if mask
                    .as_stream()
                    .ok()
                    .and_then(|stream| stream.dict.get(b"Subtype").and_then(Object::as_name).ok())
                    == Some(b"Image")
                {
                    protected.insert(mask_id);
                }
            }
        }
        protected
    }

    fn analyze_image_usage(document: &Document, max_content_bytes: usize) -> UsageAnalysis {
        let mut analysis = UsageAnalysis {
            images: BTreeMap::new(),
            inline_images: 0,
            skipped_content_streams: 0,
            conservative: false,
        };
        for page_id in document.get_pages().into_values() {
            let resources = match inherited_resources(document, page_id) {
                Ok(Some(resources)) => resources,
                Ok(None) => Dictionary::new(),
                Err(_) => {
                    analysis.skipped_content_streams += 1;
                    Dictionary::new()
                }
            };
            let user_unit = inherited_number(document, page_id, b"UserUnit").unwrap_or(1.0);
            match document.get_page_content_with_limit(page_id, max_content_bytes) {
                Ok(bytes) => match Content::decode_strict(&bytes) {
                    Ok(content) => analyze_content(
                        document,
                        &content,
                        &resources,
                        Matrix::scale(user_unit),
                        max_content_bytes,
                        0,
                        &mut HashSet::new(),
                        &mut analysis,
                    ),
                    Err(_) => analysis.skipped_content_streams += 1,
                },
                Err(_) => analysis.skipped_content_streams += 1,
            }

            // Annotation and widget appearance streams are rendered outside
            // the page content stream. Their Rect/BBox transform is not
            // available in the content walker, so treat any image found there
            // as resize-protected rather than risking an undersized rewrite.
            analysis.conservative = true;
            analyze_page_appearances(document, page_id, &resources, max_content_bytes, &mut analysis);
            analysis.conservative = false;
        }
        // Type3 glyph procedures are another independent rendering entry
        // point. Their FontMatrix and glyph placement are not represented in
        // the page CTM walker, so protect images discovered in every CharProc
        // from DPI-based resizing as well.
        analysis.conservative = true;
        analyze_type3_charprocs(document, max_content_bytes, &mut analysis);
        analysis.conservative = false;
        analysis
    }

    fn analyze_type3_charprocs(document: &Document, max_content_bytes: usize, analysis: &mut UsageAnalysis) {
        for object in document.objects.values() {
            let Some(font) = (match object {
                Object::Dictionary(dictionary) => Some(dictionary),
                Object::Stream(stream) => Some(&stream.dict),
                _ => None,
            }) else {
                continue;
            };
            if font.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Type3") {
                continue;
            }
            let resources = dictionary_value(document, font, b"Resources")
                .and_then(|value| value.as_dict().ok())
                .cloned()
                .unwrap_or_default();
            let Some(charprocs) = dictionary_value(document, font, b"CharProcs").and_then(|value| value.as_dict().ok())
            else {
                analysis.skipped_content_streams += 1;
                continue;
            };
            for (_, value) in charprocs {
                let Some((id, stream)) = object_stream(document, Some(value)) else {
                    analysis.skipped_content_streams += 1;
                    continue;
                };
                let mut active_forms = HashSet::new();
                analyze_resource_stream(
                    document,
                    id,
                    &stream,
                    &resources,
                    Matrix::IDENTITY,
                    max_content_bytes,
                    0,
                    &mut active_forms,
                    analysis,
                );
            }
        }
    }

    fn analyze_page_appearances(
        document: &Document, page_id: ObjectId, page_resources: &Dictionary, max_content_bytes: usize,
        analysis: &mut UsageAnalysis,
    ) {
        let Ok(page) = document.get_dictionary(page_id) else {
            return;
        };
        let Some(annotations) = dictionary_value(document, page, b"Annots").and_then(|value| value.as_array().ok())
        else {
            return;
        };
        let mut visited = HashSet::new();
        for annotation in annotations {
            let Some((_, annotation)) = document.dereference(annotation).ok() else {
                analysis.skipped_content_streams += 1;
                continue;
            };
            let Ok(appearance) = annotation.as_dict().and_then(|dictionary| dictionary.get(b"AP")) else {
                continue;
            };
            analyze_appearance_object(
                document,
                appearance,
                page_resources,
                max_content_bytes,
                0,
                &mut visited,
                analysis,
            );
        }
    }

    fn analyze_appearance_object(
        document: &Document, value: &Object, parent_resources: &Dictionary, max_content_bytes: usize, depth: usize,
        visited: &mut HashSet<ObjectId>, analysis: &mut UsageAnalysis,
    ) {
        if depth >= MAX_OPTIMIZER_DEPTH {
            analysis.skipped_content_streams += 1;
            return;
        }
        let Ok((id, value)) = document.dereference(value) else {
            analysis.skipped_content_streams += 1;
            return;
        };
        match value {
            Object::Stream(stream) => {
                if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image") {
                    if let Some(id) = id {
                        analysis.images.entry(id).or_default().protect_resize();
                    }
                } else {
                    let mut active_forms = HashSet::new();
                    analyze_resource_stream(
                        document,
                        id,
                        stream,
                        parent_resources,
                        Matrix::IDENTITY,
                        max_content_bytes,
                        depth,
                        &mut active_forms,
                        analysis,
                    );
                }
            }
            Object::Dictionary(dictionary) => {
                if id.is_some_and(|id| !visited.insert(id)) {
                    return;
                }
                for (_, child) in dictionary {
                    analyze_appearance_object(
                        document,
                        child,
                        parent_resources,
                        max_content_bytes,
                        depth + 1,
                        visited,
                        analysis,
                    );
                }
            }
            Object::Array(values) => {
                for child in values {
                    analyze_appearance_object(
                        document,
                        child,
                        parent_resources,
                        max_content_bytes,
                        depth + 1,
                        visited,
                        analysis,
                    );
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_content(
        document: &Document, content: &Content, resources: &Dictionary, initial_ctm: Matrix, max_content_bytes: usize,
        depth: usize, active_forms: &mut HashSet<ObjectId>, analysis: &mut UsageAnalysis,
    ) {
        if depth >= MAX_OPTIMIZER_DEPTH {
            analysis.skipped_content_streams += 1;
            return;
        }
        let mut ctm = initial_ctm;
        let mut stack = Vec::new();
        for operation in &content.operations {
            match operation.operator.as_str() {
                "q" => stack.push(ctm),
                "Q" => {
                    if let Some(saved) = stack.pop() {
                        ctm = saved;
                    }
                }
                "cm" => {
                    if let Some(matrix) = matrix_from_operands(&operation.operands) {
                        ctm = ctm.concat(matrix);
                    }
                }
                "Do" => {
                    let Some(name) = operation.operands.first().and_then(|object| object.as_name().ok()) else {
                        continue;
                    };
                    let Some((id, stream)) = resource_stream(document, resources, b"XObject", name) else {
                        continue;
                    };
                    match stream.dict.get(b"Subtype").and_then(Object::as_name).ok() {
                        Some(b"Image") => {
                            if let Some(id) = id
                                && let (Some(width), Some(height)) = (
                                    positive_dimension(&stream.dict, b"Width"),
                                    positive_dimension(&stream.dict, b"Height"),
                                )
                            {
                                let conservative = analysis.conservative;
                                let usage = analysis.images.entry(id).or_default();
                                if conservative {
                                    usage.protect_resize();
                                } else if let Some(dpi) = ctm.dpi(width, height) {
                                    usage.record(dpi);
                                }
                            }
                        }
                        Some(b"Form") => analyze_resource_stream(
                            document,
                            id,
                            &stream,
                            resources,
                            ctm,
                            max_content_bytes,
                            depth,
                            active_forms,
                            analysis,
                        ),
                        _ => {}
                    }
                }
                "BI" => analysis.inline_images += 1,
                "scn" | "SCN" => {
                    let Some(name) = operation.operands.last().and_then(|object| object.as_name().ok()) else {
                        continue;
                    };
                    let Some((id, stream)) = resource_stream(document, resources, b"Pattern", name) else {
                        continue;
                    };
                    if stream.dict.get(b"PatternType").and_then(Object::as_i64).ok() == Some(1) {
                        analyze_resource_stream(
                            document,
                            id,
                            &stream,
                            resources,
                            ctm,
                            max_content_bytes,
                            depth,
                            active_forms,
                            analysis,
                        );
                    }
                }
                "gs" => analyze_soft_mask(
                    document,
                    resources,
                    operation.operands.first(),
                    ctm,
                    max_content_bytes,
                    depth,
                    active_forms,
                    analysis,
                ),
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_resource_stream(
        document: &Document, id: Option<ObjectId>, stream: &Stream, parent_resources: &Dictionary, parent_ctm: Matrix,
        max_content_bytes: usize, depth: usize, active_forms: &mut HashSet<ObjectId>, analysis: &mut UsageAnalysis,
    ) {
        if id.is_some_and(|id| !active_forms.insert(id)) {
            analysis.skipped_content_streams += 1;
            return;
        }
        let matrix = stream
            .dict
            .get(b"Matrix")
            .ok()
            .and_then(|value| value.as_array().ok())
            .and_then(|values| matrix_from_operands(values))
            .unwrap_or(Matrix::IDENTITY);
        let resources = dictionary_value(document, &stream.dict, b"Resources")
            .and_then(|value| value.as_dict().ok())
            .cloned()
            .unwrap_or_else(|| parent_resources.clone());
        match stream
            .decompressed_content_with_limit(max_content_bytes)
            .and_then(|bytes| Content::decode_strict(&bytes))
        {
            Ok(content) => analyze_content(
                document,
                &content,
                &resources,
                parent_ctm.concat(matrix),
                max_content_bytes,
                depth + 1,
                active_forms,
                analysis,
            ),
            Err(_) => analysis.skipped_content_streams += 1,
        }
        if let Some(id) = id {
            active_forms.remove(&id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_soft_mask(
        document: &Document, resources: &Dictionary, operand: Option<&Object>, ctm: Matrix, max_content_bytes: usize,
        depth: usize, active_forms: &mut HashSet<ObjectId>, analysis: &mut UsageAnalysis,
    ) {
        let Some(name) = operand.and_then(|object| object.as_name().ok()) else {
            return;
        };
        let Some((_, ext_g_state)) = resource_object(document, resources, b"ExtGState", name) else {
            return;
        };
        let Ok(ext_g_state) = ext_g_state.as_dict() else {
            return;
        };
        let Some(mask) = dictionary_value(document, ext_g_state, b"SMask").and_then(|value| value.as_dict().ok())
        else {
            return;
        };
        let Some((id, stream)) = object_stream(document, mask.get(b"G").ok()) else {
            return;
        };
        analyze_resource_stream(
            document,
            id,
            &stream,
            resources,
            ctm,
            max_content_bytes,
            depth,
            active_forms,
            analysis,
        );
    }

    fn matrix_from_operands(values: &[Object]) -> Option<Matrix> {
        if values.len() != 6 {
            return None;
        }
        let mut number = values.iter().map(|value| value.as_float().ok());
        Some(Matrix {
            a: number.next()??,
            b: number.next()??,
            c: number.next()??,
            d: number.next()??,
            e: number.next()??,
            f: number.next()??,
        })
    }

    fn inherited_resources(document: &Document, mut id: ObjectId) -> Result<Option<Dictionary>> {
        let mut seen = HashSet::new();
        for _ in 0..MAX_OPTIMIZER_DEPTH {
            if !seen.insert(id) {
                return Err(Error::ReferenceCycle(id));
            }
            let dictionary = document.get_dictionary(id)?;
            if let Some(resources) = dictionary_value(document, dictionary, b"Resources") {
                return Ok(resources.as_dict().ok().cloned());
            }
            let Ok(parent) = dictionary.get(b"Parent").and_then(Object::as_reference) else {
                return Ok(None);
            };
            id = parent;
        }
        Err(Error::RecursionLimit)
    }

    fn inherited_number(document: &Document, mut id: ObjectId, key: &[u8]) -> Option<f32> {
        let mut seen = HashSet::new();
        for _ in 0..MAX_OPTIMIZER_DEPTH {
            if !seen.insert(id) {
                return None;
            }
            let dictionary = document.get_dictionary(id).ok()?;
            if let Ok(value) = dictionary.get(key).and_then(Object::as_float)
                && value.is_finite()
                && value > 0.0
            {
                return Some(value);
            }
            id = dictionary.get(b"Parent").and_then(Object::as_reference).ok()?;
        }
        None
    }

    fn dictionary_value<'a>(document: &'a Document, dictionary: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
        dictionary
            .get(key)
            .ok()
            .and_then(|value| document.dereference(value).ok().map(|(_, value)| value))
    }

    fn resource_object(
        document: &Document, resources: &Dictionary, category: &[u8], name: &[u8],
    ) -> Option<(Option<ObjectId>, Object)> {
        let category = dictionary_value(document, resources, category)?.as_dict().ok()?;
        let value = category.get(name).ok()?;
        let (id, value) = document.dereference(value).ok()?;
        Some((id, value.clone()))
    }

    fn resource_stream(
        document: &Document, resources: &Dictionary, category: &[u8], name: &[u8],
    ) -> Option<(Option<ObjectId>, Stream)> {
        let (id, object) = resource_object(document, resources, category, name)?;
        Some((id, object.as_stream().ok()?.clone()))
    }

    fn object_stream(document: &Document, value: Option<&Object>) -> Option<(Option<ObjectId>, Stream)> {
        let (id, object) = document.dereference(value?).ok()?;
        Some((id, object.as_stream().ok()?.clone()))
    }

    fn positive_dimension(dictionary: &Dictionary, key: &[u8]) -> Option<u32> {
        let value = dictionary.get(key).and_then(Object::as_i64).ok()?;
        u32::try_from(value).ok().filter(|value| *value > 0)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ColorModel {
        Gray,
        Rgb,
        Cmyk,
    }
    #[derive(Debug)]
    struct ColorInfo {
        model: ColorModel,
        icc_profile: Option<IccProfile>,
    }

    #[derive(Debug)]
    struct IccProfile {
        content: Vec<u8>,
        object_ids: Vec<ObjectId>,
    }

    #[derive(Default)]
    struct IccTransformCache {
        transforms: HashMap<([u8; 32], RenderingIntent), Option<Arc<Transform8BitExecutor>>>,
    }

    impl IccTransformCache {
        fn transform(&mut self, profile: &[u8], intent: RenderingIntent) -> Option<Arc<Transform8BitExecutor>> {
            let digest: [u8; 32] = Sha256::digest(profile).into();
            self.transforms
                .entry((digest, intent))
                .or_insert_with(|| {
                    let source = ColorProfile::new_from_slice(profile).ok()?;
                    if source.color_space != DataColorSpace::Cmyk {
                        return None;
                    }
                    let destination = ColorProfile::new_srgb();
                    let options = TransformOptions {
                        rendering_intent: intent,
                        ..TransformOptions::default()
                    };
                    source
                        .create_transform_8bit(Layout::Rgba, &destination, Layout::Rgb, options)
                        .ok()
                })
                .clone()
        }
    }

    #[derive(Debug)]
    enum Pixels {
        Bitonal(GrayImage),
        Gray(GrayImage),
        Rgb(RgbImage),
        Cmyk(RgbaImage),
    }

    impl Pixels {
        fn dimensions(&self) -> (u32, u32) {
            match self {
                Self::Bitonal(image) | Self::Gray(image) => image.dimensions(),
                Self::Rgb(image) => image.dimensions(),
                Self::Cmyk(image) => image.dimensions(),
            }
        }

        fn resize(self, width: u32, height: u32, filter: FilterType) -> Self {
            match self {
                Self::Bitonal(image) => {
                    let resized = image::imageops::resize(&image, width, height, FilterType::Nearest);
                    Self::Bitonal(resized)
                }
                Self::Gray(image) if filter == FilterType::Lanczos3 => {
                    Self::Gray(resample::lanczos3(&image, width, height))
                }
                Self::Rgb(image) if filter == FilterType::Lanczos3 => {
                    Self::Rgb(resample::lanczos3(&image, width, height))
                }
                Self::Cmyk(image) if filter == FilterType::Lanczos3 => {
                    Self::Cmyk(resample::lanczos3(&image, width, height))
                }
                Self::Gray(image) => Self::Gray(image::imageops::resize(&image, width, height, filter)),
                Self::Rgb(image) => Self::Rgb(image::imageops::resize(&image, width, height, filter)),
                Self::Cmyk(image) => Self::Cmyk(image::imageops::resize(&image, width, height, filter)),
            }
        }
    }

    #[derive(Debug)]
    struct ImageRewrite {
        stream: Stream,
        resized: bool,
        obsolete_color_objects: Vec<ObjectId>,
    }

    fn rewrite_image(
        document: &Document, original: &Stream, usage: ImageUsage, options: &ImageOptimizationOptions,
        allow_resize: bool, icc_transforms: &mut IccTransformCache,
    ) -> std::result::Result<ImageRewrite, ImageSkipReason> {
        if original.dict.has(b"SMask") || original.dict.has(b"Mask") {
            return Err(ImageSkipReason::Masked);
        }
        let width = positive_dimension(&original.dict, b"Width").ok_or(ImageSkipReason::InvalidDimensions)?;
        let height = positive_dimension(&original.dict, b"Height").ok_or(ImageSkipReason::InvalidDimensions)?;
        let color = color_info(document, &original.dict, options.max_decoded_image_bytes)?;
        let model = color.model;
        let bits = original
            .dict
            .get(b"BitsPerComponent")
            .and_then(Object::as_i64)
            .ok()
            .or_else(|| {
                original
                    .dict
                    .get(b"ImageMask")
                    .and_then(Object::as_bool)
                    .ok()
                    .filter(|value| *value)
                    .map(|_| 1)
            })
            .ok_or(ImageSkipReason::UnsupportedBitsPerComponent)?;
        if !matches!(bits, 1 | 8) {
            return Err(ImageSkipReason::UnsupportedBitsPerComponent);
        }
        let lossy = has_filter(original, b"DCTDecode") || has_filter(original, b"JPXDecode");
        let class_options = match (model, bits, lossy) {
            (ColorModel::Gray, 1, _) => options.bitonal,
            (ColorModel::Gray, 8, true) => options.grayscale_lossy,
            (ColorModel::Gray, 8, false) => options.grayscale_lossless,
            (ColorModel::Rgb | ColorModel::Cmyk, 8, true) => options.color_lossy,
            (ColorModel::Rgb | ColorModel::Cmyk, 8, false) => options.color_lossless,
            _ => return Err(ImageSkipReason::UnsupportedBitsPerComponent),
        };
        if class_options.recompression == ImageRecompression::Never {
            return Err(ImageSkipReason::PolicyDisabled);
        }
        let channels = match model {
            ColorModel::Gray => 1,
            ColorModel::Rgb => 3,
            ColorModel::Cmyk => 4,
        };
        let decoded_size = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(channels))
            .ok_or(ImageSkipReason::DecodeLimitExceeded)?;
        if decoded_size > options.max_decoded_image_bytes {
            return Err(ImageSkipReason::DecodeLimitExceeded);
        }

        let mut pixels = decode_pixels(original, model, bits, options.max_decoded_image_bytes)?;
        if pixels.dimensions() != (width, height) {
            return Err(ImageSkipReason::InvalidDimensions);
        }
        let attempt_icc_conversion = options.convert_icc_cmyk_to_srgb
            && uses_jpeg_recompression(class_options.recompression, lossy)
            && color.icc_profile.is_some();
        // jpeg-decoder already normalizes Adobe CMYK/YCCK samples. The
        // conventional inverted PDF Decode array compensates decoders that
        // expose the stored JPEG components and must not be applied twice
        // before an ICC transform.
        let defer_decode =
            attempt_icc_conversion && has_filter(original, b"DCTDecode") && has_inverted_cmyk_decode(&original.dict);
        if !defer_decode {
            apply_decode(&original.dict, &mut pixels)?;
        }
        let converted = if attempt_icc_conversion {
            match (&pixels, color.icc_profile.as_ref()) {
                (Pixels::Cmyk(image), Some(profile)) => convert_icc_cmyk_to_srgb(
                    image,
                    &profile.content,
                    image_rendering_intent(&original.dict),
                    icc_transforms,
                ),
                _ => None,
            }
        } else {
            None
        };
        let mut obsolete_color_objects = Vec::new();
        let converted_to_srgb = if let Some(rgb) = converted {
            pixels = Pixels::Rgb(rgb);
            obsolete_color_objects = color.icc_profile.unwrap().object_ids;
            true
        } else {
            if defer_decode {
                apply_decode(&original.dict, &mut pixels)?;
            }
            false
        };

        let mut resized = false;
        if allow_resize
            && !usage.no_resize
            && let (Some(current_dpi), Some(threshold)) = (usage.minimum_dpi, class_options.subsample_above_dpi)
            && current_dpi > threshold
            && class_options.subsample_to_dpi > 0.0
        {
            let scale = class_options.subsample_to_dpi / current_dpi;
            let new_width = ((width as f32 * scale).round() as u32).clamp(1, width);
            let new_height = ((height as f32 * scale).round() as u32).clamp(1, height);
            if new_width < width || new_height < height {
                pixels = pixels.resize(new_width, new_height, resize_filter(options.resize_filter));
                resized = true;
            }
        }

        let stream = encode_pixels(original, pixels, class_options.recompression, lossy, converted_to_srgb)?;
        Ok(ImageRewrite {
            stream,
            resized,
            obsolete_color_objects,
        })
    }

    fn color_info(
        document: &Document, dictionary: &Dictionary, max_profile_bytes: usize,
    ) -> std::result::Result<ColorInfo, ImageSkipReason> {
        if dictionary
            .get(b"ImageMask")
            .and_then(Object::as_bool)
            .ok()
            .unwrap_or(false)
        {
            return Ok(ColorInfo {
                model: ColorModel::Gray,
                icc_profile: None,
            });
        }
        let color_space = dictionary
            .get(b"ColorSpace")
            .map_err(|_| ImageSkipReason::UnsupportedColorSpace)?;
        let (_, value) = document
            .dereference(color_space)
            .map_err(|_| ImageSkipReason::UnsupportedColorSpace)?;
        let plain = |model| {
            Ok(ColorInfo {
                model,
                icc_profile: None,
            })
        };
        match value {
            Object::Name(name) if matches!(name.as_slice(), b"DeviceGray" | b"G" | b"CalGray") => {
                plain(ColorModel::Gray)
            }
            Object::Name(name) if matches!(name.as_slice(), b"DeviceRGB" | b"RGB" | b"CalRGB") => {
                plain(ColorModel::Rgb)
            }
            Object::Name(name) if name.as_slice() == b"DeviceCMYK" => plain(ColorModel::Cmyk),
            Object::Array(values) => match values.first().and_then(|value| value.as_name().ok()) {
                Some(b"CalGray") => plain(ColorModel::Gray),
                Some(b"CalRGB") => plain(ColorModel::Rgb),
                Some(b"ICCBased") => {
                    let profile = values.get(1).ok_or(ImageSkipReason::UnsupportedColorSpace)?;
                    let (_, profile) = document
                        .dereference(profile)
                        .map_err(|_| ImageSkipReason::UnsupportedColorSpace)?;
                    let profile = profile
                        .as_stream()
                        .map_err(|_| ImageSkipReason::UnsupportedColorSpace)?;
                    let model = match profile
                        .dict
                        .get(b"N")
                        .and_then(Object::as_i64)
                        .map_err(|_| ImageSkipReason::UnsupportedColorSpace)?
                    {
                        1 => ColorModel::Gray,
                        3 => ColorModel::Rgb,
                        4 => ColorModel::Cmyk,
                        _ => return Err(ImageSkipReason::UnsupportedColorSpace),
                    };
                    let icc_profile = if model == ColorModel::Cmyk {
                        profile
                            .decompressed_content_with_limit(max_profile_bytes.min(MAX_ICC_PROFILE_BYTES))
                            .ok()
                            .map(|content| IccProfile {
                                content,
                                object_ids: color_object_ids(document, color_space),
                            })
                    } else {
                        None
                    };
                    Ok(ColorInfo { model, icc_profile })
                }
                _ => Err(ImageSkipReason::UnsupportedColorSpace),
            },
            _ => Err(ImageSkipReason::UnsupportedColorSpace),
        }
    }

    fn color_object_ids(document: &Document, color_space: &Object) -> Vec<ObjectId> {
        let mut pending = Vec::new();
        collect_direct_references(color_space, &mut pending, 0);
        let mut ids = HashSet::new();
        while let Some(id) = pending.pop() {
            if !ids.insert(id) {
                continue;
            }
            if let Some(object) = document.objects.get(&id) {
                collect_direct_references(object, &mut pending, 0);
            }
        }
        let mut ids: Vec<_> = ids.into_iter().collect();
        ids.sort_unstable();
        ids
    }
    fn collect_direct_references(object: &Object, references: &mut Vec<ObjectId>, depth: usize) {
        if depth >= MAX_OPTIMIZER_DEPTH {
            return;
        }
        match object {
            Object::Reference(id) => references.push(*id),
            Object::Array(values) => {
                for value in values {
                    collect_direct_references(value, references, depth + 1);
                }
            }
            Object::Dictionary(dictionary) => {
                for (_, value) in dictionary {
                    collect_direct_references(value, references, depth + 1);
                }
            }
            Object::Stream(stream) => {
                for (_, value) in &stream.dict {
                    collect_direct_references(value, references, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn remove_unreferenced_color_objects(document: &mut Document, candidates: &HashSet<ObjectId>) {
        if candidates.is_empty() {
            return;
        }
        let mut pending = Vec::new();
        for (id, object) in &document.objects {
            if !candidates.contains(id) {
                collect_direct_references(object, &mut pending, 0);
            }
        }
        for (_, value) in &document.trailer {
            collect_direct_references(value, &mut pending, 0);
        }

        let mut reachable = HashSet::new();
        while let Some(id) = pending.pop() {
            if !candidates.contains(&id) || !reachable.insert(id) {
                continue;
            }
            if let Some(object) = document.objects.get(&id) {
                collect_direct_references(object, &mut pending, 0);
            }
        }
        document
            .objects
            .retain(|id, _| !candidates.contains(id) || reachable.contains(id));
    }

    fn uses_jpeg_recompression(recompression: ImageRecompression, source_lossy: bool) -> bool {
        matches!(recompression, ImageRecompression::Jpeg { .. })
            || source_lossy && matches!(recompression, ImageRecompression::Preserve { .. })
    }
    fn has_inverted_cmyk_decode(dictionary: &Dictionary) -> bool {
        dictionary
            .get(b"Decode")
            .and_then(Object::as_array)
            .ok()
            .is_some_and(|values| {
                values.len() == 8
                    && values
                        .chunks_exact(2)
                        .all(|pair| pair[0].as_float().ok() == Some(1.0) && pair[1].as_float().ok() == Some(0.0))
            })
    }

    fn image_rendering_intent(dictionary: &Dictionary) -> RenderingIntent {
        match dictionary.get(b"Intent").and_then(Object::as_name).ok() {
            Some(b"AbsoluteColorimetric") => RenderingIntent::AbsoluteColorimetric,
            Some(b"Perceptual") => RenderingIntent::Perceptual,
            Some(b"Saturation") => RenderingIntent::Saturation,
            _ => RenderingIntent::RelativeColorimetric,
        }
    }

    fn convert_icc_cmyk_to_srgb(
        image: &RgbaImage, profile: &[u8], intent: RenderingIntent, transforms: &mut IccTransformCache,
    ) -> Option<RgbImage> {
        let transform = transforms.transform(profile, intent)?;
        let pixel_count = usize::try_from(image.width())
            .ok()?
            .checked_mul(usize::try_from(image.height()).ok()?)?;
        let mut output = vec![0; pixel_count.checked_mul(3)?];
        transform.transform(image.as_raw(), &mut output).ok()?;
        RgbImage::from_raw(image.width(), image.height(), output)
    }

    fn filters(stream: &Stream) -> Vec<&[u8]> {
        stream.filters().unwrap_or_default()
    }

    fn has_filter(stream: &Stream, expected: &[u8]) -> bool {
        filters(stream).contains(&expected)
    }

    fn decode_pixels(
        stream: &Stream, model: ColorModel, bits: i64, max_bytes: usize,
    ) -> std::result::Result<Pixels, ImageSkipReason> {
        let width = positive_dimension(&stream.dict, b"Width").ok_or(ImageSkipReason::InvalidDimensions)?;
        let height = positive_dimension(&stream.dict, b"Height").ok_or(ImageSkipReason::InvalidDimensions)?;
        let stream_filters = filters(stream);
        let jpeg_filters = stream_filters == [b"DCTDecode".as_slice()]
            || stream_filters == [b"ASCII85Decode".as_slice(), b"DCTDecode".as_slice()];
        if jpeg_filters {
            if bits != 8 {
                return Err(ImageSkipReason::UnsupportedBitsPerComponent);
            }
            let jpeg = jpeg_content(stream, &stream_filters, max_bytes)?;
            return decode_jpeg_pixels(&jpeg, model, width, height, max_bytes);
        }
        let jpx_filters = stream_filters == [b"JPXDecode".as_slice()]
            || stream_filters == [b"ASCII85Decode".as_slice(), b"JPXDecode".as_slice()];
        if jpx_filters {
            if bits != 8 {
                return Err(ImageSkipReason::UnsupportedBitsPerComponent);
            }
            let jpx = jpx_content(stream, &stream_filters, max_bytes)?;
            return decode_jpx_pixels(&jpx, model, width, height, max_bytes);
        }
        if stream_filters.iter().any(|filter| {
            !matches!(
                *filter,
                b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" | b"ASCIIHexDecode" | b"RunLengthDecode"
            )
        }) {
            return Err(ImageSkipReason::UnsupportedFilter);
        }
        let raw = stream
            .decompressed_content_with_limit(max_bytes)
            .map_err(image_decode_error)?;
        match (model, bits) {
            (ColorModel::Gray, 1) => {
                let bytes_per_row = usize::try_from(width).unwrap().div_ceil(8);
                if raw.len() != bytes_per_row * usize::try_from(height).unwrap() {
                    return Err(ImageSkipReason::DecodeFailed);
                }
                let mut pixels = Vec::with_capacity(usize::try_from(width).unwrap() * usize::try_from(height).unwrap());
                for row in raw.chunks_exact(bytes_per_row) {
                    for x in 0..width as usize {
                        let bit = (row[x / 8] >> (7 - x % 8)) & 1;
                        pixels.push(if bit == 0 { 0 } else { 255 });
                    }
                }
                GrayImage::from_raw(width, height, pixels)
                    .map(Pixels::Bitonal)
                    .ok_or(ImageSkipReason::DecodeFailed)
            }
            (ColorModel::Gray, 8) => GrayImage::from_raw(width, height, raw)
                .map(Pixels::Gray)
                .ok_or(ImageSkipReason::DecodeFailed),
            (ColorModel::Rgb, 8) => RgbImage::from_raw(width, height, raw)
                .map(Pixels::Rgb)
                .ok_or(ImageSkipReason::DecodeFailed),
            (ColorModel::Cmyk, 8) => RgbaImage::from_raw(width, height, raw)
                .map(Pixels::Cmyk)
                .ok_or(ImageSkipReason::DecodeFailed),
            _ => Err(ImageSkipReason::UnsupportedBitsPerComponent),
        }
    }

    fn apply_decode(dictionary: &Dictionary, pixels: &mut Pixels) -> std::result::Result<(), ImageSkipReason> {
        let Ok(value) = dictionary.get(b"Decode") else {
            return Ok(());
        };
        let values = value.as_array().map_err(|_| ImageSkipReason::DecodeFailed)?;
        let channels = match pixels {
            Pixels::Bitonal(_) | Pixels::Gray(_) => 1,
            Pixels::Rgb(_) => 3,
            Pixels::Cmyk(_) => 4,
        };
        if values.len() != channels * 2 {
            return Err(ImageSkipReason::DecodeFailed);
        }
        let ranges: Vec<_> = values
            .chunks_exact(2)
            .map(|pair| {
                let low = pair[0].as_float().map_err(|_| ImageSkipReason::DecodeFailed)?;
                let high = pair[1].as_float().map_err(|_| ImageSkipReason::DecodeFailed)?;
                if low.is_finite() && high.is_finite() {
                    Ok((low, high))
                } else {
                    Err(ImageSkipReason::DecodeFailed)
                }
            })
            .collect::<std::result::Result<_, _>>()?;
        let raw: &mut [u8] = match pixels {
            Pixels::Bitonal(image) | Pixels::Gray(image) => image.as_mut(),
            Pixels::Rgb(image) => image.as_mut(),
            Pixels::Cmyk(image) => image.as_mut(),
        };
        for (index, sample) in raw.iter_mut().enumerate() {
            let (low, high) = ranges[index % channels];
            let decoded = low + (high - low) * (f32::from(*sample) / 255.0);
            *sample = (decoded * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        Ok(())
    }

    fn decode_jpeg_pixels(
        jpeg: &[u8], model: ColorModel, width: u32, height: u32, max_bytes: usize,
    ) -> std::result::Result<Pixels, ImageSkipReason> {
        let mut decoder = JpegDecoder::new(Cursor::new(jpeg));
        decoder.set_max_decoding_buffer_size(max_bytes);
        decoder.read_info().map_err(|_| ImageSkipReason::DecodeFailed)?;
        let info = decoder.info().ok_or(ImageSkipReason::DecodeFailed)?;
        if u32::from(info.width) != width || u32::from(info.height) != height {
            return Err(ImageSkipReason::InvalidDimensions);
        }
        let decoded = decoder.decode().map_err(|_| ImageSkipReason::DecodeFailed)?;
        match (model, info.pixel_format) {
            (ColorModel::Gray, PixelFormat::L8) => GrayImage::from_raw(width, height, decoded)
                .map(Pixels::Gray)
                .ok_or(ImageSkipReason::DecodeFailed),
            (ColorModel::Rgb, PixelFormat::RGB24) => RgbImage::from_raw(width, height, decoded)
                .map(Pixels::Rgb)
                .ok_or(ImageSkipReason::DecodeFailed),
            (ColorModel::Cmyk, PixelFormat::CMYK32) => RgbaImage::from_raw(width, height, decoded)
                .map(Pixels::Cmyk)
                .ok_or(ImageSkipReason::DecodeFailed),
            _ => Err(ImageSkipReason::DecodeFailed),
        }
    }
    fn decode_jpx_pixels(
        jpx: &[u8], model: ColorModel, width: u32, height: u32, max_bytes: usize,
    ) -> std::result::Result<Pixels, ImageSkipReason> {
        let image = JpxImage::new(jpx, &JpxDecodeSettings::default()).map_err(|_| ImageSkipReason::DecodeFailed)?;
        if image.width() != width || image.height() != height {
            return Err(ImageSkipReason::InvalidDimensions);
        }
        if image.has_alpha() {
            return Err(ImageSkipReason::Masked);
        }
        let expected_channels = match model {
            ColorModel::Gray => 1,
            ColorModel::Rgb => 3,
            ColorModel::Cmyk => 4,
        };
        let color_space_matches = match (model, image.color_space()) {
            (ColorModel::Gray, JpxColorSpace::Gray)
            | (ColorModel::Rgb, JpxColorSpace::RGB)
            | (ColorModel::Cmyk, JpxColorSpace::CMYK) => true,
            (_, JpxColorSpace::Icc { num_channels, .. }) => usize::from(*num_channels) == expected_channels,
            _ => false,
        };
        if !color_space_matches {
            return Err(ImageSkipReason::UnsupportedColorSpace);
        }
        let decoded = image.decode().map_err(|_| ImageSkipReason::DecodeFailed)?;
        if decoded.len() > max_bytes {
            return Err(ImageSkipReason::DecodeLimitExceeded);
        }
        match model {
            ColorModel::Gray => GrayImage::from_raw(width, height, decoded)
                .map(Pixels::Gray)
                .ok_or(ImageSkipReason::DecodeFailed),
            ColorModel::Rgb => RgbImage::from_raw(width, height, decoded)
                .map(Pixels::Rgb)
                .ok_or(ImageSkipReason::DecodeFailed),
            ColorModel::Cmyk => RgbaImage::from_raw(width, height, decoded)
                .map(Pixels::Cmyk)
                .ok_or(ImageSkipReason::DecodeFailed),
        }
    }

    fn jpeg_content<'a>(
        stream: &'a Stream, stream_filters: &[&[u8]], max_bytes: usize,
    ) -> std::result::Result<Cow<'a, [u8]>, ImageSkipReason> {
        if stream_filters == [b"DCTDecode".as_slice()] {
            return Ok(Cow::Borrowed(stream.content.as_slice()));
        }
        if stream_filters == [b"ASCII85Decode".as_slice(), b"DCTDecode".as_slice()] {
            let mut ascii85 = stream.clone();
            ascii85.dict.set("Filter", "ASCII85Decode");
            ascii85.dict.remove(b"DecodeParms");
            return ascii85
                .decompressed_content_with_limit(max_bytes)
                .map(Cow::Owned)
                .map_err(image_decode_error);
        }
        Err(ImageSkipReason::UnsupportedFilter)
    }
    fn jpx_content<'a>(
        stream: &'a Stream, stream_filters: &[&[u8]], max_bytes: usize,
    ) -> std::result::Result<Cow<'a, [u8]>, ImageSkipReason> {
        if stream_filters == [b"JPXDecode".as_slice()] {
            return Ok(Cow::Borrowed(stream.content.as_slice()));
        }
        if stream_filters == [b"ASCII85Decode".as_slice(), b"JPXDecode".as_slice()] {
            let mut ascii85 = stream.clone();
            ascii85.dict.set("Filter", "ASCII85Decode");
            ascii85.dict.remove(b"DecodeParms");
            return ascii85
                .decompressed_content_with_limit(max_bytes)
                .map(Cow::Owned)
                .map_err(image_decode_error);
        }
        Err(ImageSkipReason::UnsupportedFilter)
    }

    fn image_decode_error(error: Error) -> ImageSkipReason {
        match error {
            Error::Decompress(crate::DecompressError::MemoryLimitExceeded { .. }) => {
                ImageSkipReason::DecodeLimitExceeded
            }
            _ => ImageSkipReason::DecodeFailed,
        }
    }

    fn encode_pixels(
        original: &Stream, pixels: Pixels, recompression: ImageRecompression, source_lossy: bool,
        converted_to_srgb: bool,
    ) -> std::result::Result<Stream, ImageSkipReason> {
        let effective = match recompression {
            ImageRecompression::Preserve { jpeg_quality } if source_lossy => {
                ImageRecompression::Jpeg { quality: jpeg_quality }
            }
            ImageRecompression::Preserve { .. } => ImageRecompression::Flate,
            other => other,
        };
        let (width, height) = pixels.dimensions();
        let mut dictionary = original.dict.clone();
        dictionary.set("Width", width);
        dictionary.set("Height", height);
        dictionary.remove(b"Decode");
        dictionary.remove(b"DecodeParms");
        dictionary.remove(b"Filter");
        if converted_to_srgb {
            dictionary.set("ColorSpace", "DeviceRGB");
        }
        match effective {
            ImageRecompression::Never | ImageRecompression::Preserve { .. } => Err(ImageSkipReason::PolicyDisabled),
            ImageRecompression::Jpeg { quality } => {
                if dictionary
                    .get(b"ImageMask")
                    .and_then(Object::as_bool)
                    .ok()
                    .unwrap_or(false)
                {
                    return encode_flate(dictionary, pixels);
                }
                let mut encoded = Vec::new();
                let width = u16::try_from(width).map_err(|_| ImageSkipReason::InvalidDimensions)?;
                let height = u16::try_from(height).map_err(|_| ImageSkipReason::InvalidDimensions)?;
                let mut encoder = JpegEncoder::new(&mut encoded, quality.clamp(1, 100));
                encoder.set_optimized_huffman_tables(true);
                match pixels {
                    Pixels::Bitonal(image) | Pixels::Gray(image) => encoder
                        .encode(image.as_raw(), width, height, JpegColorType::Luma)
                        .map_err(|_| ImageSkipReason::DecodeFailed)?,
                    Pixels::Rgb(image) => encoder
                        .encode(image.as_raw(), width, height, JpegColorType::Rgb)
                        .map_err(|_| ImageSkipReason::DecodeFailed)?,
                    Pixels::Cmyk(image) => encoder
                        .encode(image.as_raw(), width, height, JpegColorType::CmykAsYcck)
                        .map_err(|_| ImageSkipReason::DecodeFailed)?,
                }
                dictionary.set("BitsPerComponent", 8);
                dictionary.set("Filter", "DCTDecode");
                Ok(Stream::new(dictionary, encoded))
            }
            ImageRecompression::Flate => encode_flate(dictionary, pixels),
        }
    }

    fn encode_flate(mut dictionary: Dictionary, pixels: Pixels) -> std::result::Result<Stream, ImageSkipReason> {
        let (width, _) = pixels.dimensions();
        let raw = match pixels {
            Pixels::Bitonal(image) => {
                dictionary.set("BitsPerComponent", 1);
                let bytes_per_row = usize::try_from(width).unwrap().div_ceil(8);
                let mut packed = vec![0_u8; bytes_per_row * usize::try_from(image.height()).unwrap()];
                for (index, value) in image.as_raw().iter().enumerate() {
                    if *value >= 128 {
                        let x = index % width as usize;
                        let y = index / width as usize;
                        packed[y * bytes_per_row + x / 8] |= 1 << (7 - x % 8);
                    }
                }
                packed
            }
            Pixels::Gray(image) => {
                dictionary.set("BitsPerComponent", 8);
                image.into_raw()
            }
            Pixels::Rgb(image) => {
                dictionary.set("BitsPerComponent", 8);
                image.into_raw()
            }
            Pixels::Cmyk(image) => {
                dictionary.set("BitsPerComponent", 8);
                image.into_raw()
            }
        };
        let mut stream = Stream::new(dictionary, raw);
        stream.compress().map_err(|_| ImageSkipReason::DecodeFailed)?;
        Ok(stream)
    }

    fn resize_filter(filter: ImageResizeFilter) -> FilterType {
        match filter {
            ImageResizeFilter::Nearest => FilterType::Nearest,
            ImageResizeFilter::Triangle => FilterType::Triangle,
            ImageResizeFilter::CatmullRom => FilterType::CatmullRom,
            ImageResizeFilter::Gaussian => FilterType::Gaussian,
            ImageResizeFilter::Lanczos3 => FilterType::Lanczos3,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn matrix_concatenation_preserves_pdf_scale() {
            let scale = Matrix {
                a: 72.0,
                b: 0.0,
                c: 0.0,
                d: 144.0,
                e: 10.0,
                f: 20.0,
            };
            let rotation = Matrix {
                a: 0.0,
                b: 1.0,
                c: -1.0,
                d: 0.0,
                e: 0.0,
                f: 0.0,
            };
            let dpi = scale.concat(rotation).dpi(600, 300).unwrap();
            assert!((dpi - 300.0).abs() < 0.01);
        }
    }
}
