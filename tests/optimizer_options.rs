use lopdf::{ImageCompressionPolicy, ImageRecompression, OptimizeOptions};

#[test]
fn all_optimization_stages_are_selected_independently_from_image_policy() {
    for policy in [ImageCompressionPolicy::Conservative, ImageCompressionPolicy::Aggressive] {
        let options = OptimizeOptions::all(policy);

        assert!(options.compress_streams);
        assert!(options.prune_unused_objects);
        assert!(options.deduplicate_objects);
        assert!(options.renumber_objects);
        assert!(options.fonts.is_some());
        assert!(options.images.is_some());
    }
}

#[test]
fn single_stage_constructors_do_not_enable_other_stages() {
    let structure = OptimizeOptions::only_structure();
    assert!(structure.compress_streams);
    assert!(structure.prune_unused_objects);
    assert!(structure.deduplicate_objects);
    assert!(structure.renumber_objects);
    assert!(structure.fonts.is_none());
    assert!(structure.images.is_none());

    let fonts = OptimizeOptions::only_fonts();
    assert!(!fonts.compress_streams);
    assert!(!fonts.prune_unused_objects);
    assert!(!fonts.deduplicate_objects);
    assert!(!fonts.renumber_objects);
    assert!(fonts.fonts.is_some());
    assert!(fonts.images.is_none());

    let images = OptimizeOptions::only_images(ImageCompressionPolicy::Conservative);
    assert!(!images.compress_streams);
    assert!(!images.prune_unused_objects);
    assert!(!images.deduplicate_objects);
    assert!(!images.renumber_objects);
    assert!(images.fonts.is_none());
    assert!(images.images.is_some());
}

#[test]
fn only_aggressive_image_policy_allows_lossy_color_changes() {
    let conservative = OptimizeOptions::only_images(ImageCompressionPolicy::Conservative)
        .images
        .unwrap();
    let aggressive = OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive)
        .images
        .unwrap();

    assert_eq!(conservative.color_lossless.subsample_above_dpi, None);
    assert_eq!(conservative.color_lossless.recompression, ImageRecompression::Flate);
    assert_eq!(conservative.color_lossy.recompression, ImageRecompression::Never);
    assert!(!conservative.convert_icc_cmyk_to_srgb);
    assert!(aggressive.color_lossless.subsample_above_dpi.is_some());
    assert!(matches!(
        aggressive.color_lossless.recompression,
        ImageRecompression::Jpeg { .. }
    ));
    assert!(aggressive.convert_icc_cmyk_to_srgb);
}
