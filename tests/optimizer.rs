#![cfg(feature = "embed_image")]

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, RgbImage};
use jpeg_decoder::{Decoder as JpegDecoder, PixelFormat};
use jpeg_encoder::ColorType as JpegColorType;
use lopdf::{
    Dictionary, Document, ImageCompressionPolicy, Object, OptimizeOptions, Stream, content::Content, dictionary,
};
use std::io::Cursor;

#[test]
fn structural_optimization_deduplicates_dictionary_order_independently() {
    let mut document = Document::with_version("1.7");
    let first = document.add_object(dictionary! {
        "Alpha" => 1,
        "Beta" => 2,
    });
    let second = document.add_object(dictionary! {
        "Beta" => 2,
        "Alpha" => 1,
    });
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "First" => first,
        "Second" => second,
    });
    document.trailer.set("Root", catalog);

    let options = OptimizeOptions {
        renumber_objects: false,
        ..OptimizeOptions::default()
    };
    let report = document.optimize(&options).unwrap();

    assert_eq!(report.structure.deduplicated_objects, 1);
    let catalog = document.get_dictionary(catalog).unwrap();
    assert_eq!(
        catalog.get(b"First").and_then(Object::as_reference).unwrap(),
        catalog.get(b"Second").and_then(Object::as_reference).unwrap()
    );
}

#[test]
fn structural_dedup_rewrites_unreachable_object_graphs() {
    let mut document = Document::with_version("1.7");
    let first = document.add_object(dictionary! { "Value" => 7 });
    let duplicate = document.add_object(dictionary! { "Value" => 7 });
    let holder = document.add_object(dictionary! { "Reference" => duplicate });
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Value" => first,
    });
    document.trailer.set("Root", catalog);

    let options = OptimizeOptions {
        prune_unused_objects: false,
        renumber_objects: false,
        ..OptimizeOptions::default()
    };
    document.optimize(&options).unwrap();

    assert!(document.get_object(duplicate).is_err());
    assert_eq!(
        document
            .get_dictionary(holder)
            .unwrap()
            .get(b"Reference")
            .unwrap()
            .as_reference()
            .unwrap(),
        first
    );
}

#[test]
fn content_optimization_minifies_page_streams_without_changing_operations() {
    let source = b"q\nBT\n/F1 12 Tf\n<0041> Tj\nET\nQ\n".repeat(200);
    let expected_operations = Content::decode(&source).unwrap().operations;
    let mut stream = Stream::new(Dictionary::new(), source);
    stream.compress().unwrap();
    let original_compressed_size = stream.content.len();
    let (mut document, content_id) = page_document(stream);
    let options = OptimizeOptions {
        renumber_objects: false,
        ..OptimizeOptions::default()
    };

    let report = document.optimize(&options).unwrap();

    let optimized = document.get_object(content_id).unwrap().as_stream().unwrap();
    let optimized_content = optimized.get_plain_content().unwrap();
    let actual_operations = Content::decode(&optimized_content).unwrap().operations;
    assert_same_operations(&actual_operations, &expected_operations);
    assert!(!optimized_content.contains(&b'\n'));
    assert!(optimized.content.len() < original_compressed_size);
    assert_eq!(report.structure.compressed_streams, 1);
    assert_eq!(
        report.structure.stream_bytes_saved,
        original_compressed_size - optimized.content.len()
    );
}

#[test]
fn content_optimization_preserves_inline_images() {
    let source = b"q BI /W 1 /H 1 /BPC 8 /CS /Gray ID \x80 EI Q".to_vec();
    let mut stream = Stream::new(Dictionary::new(), source);
    stream.compress().unwrap();
    let original_content = stream.content.clone();
    let (mut document, content_id) = page_document(stream);
    let options = OptimizeOptions {
        renumber_objects: false,
        ..OptimizeOptions::default()
    };

    let report = document.optimize(&options).unwrap();

    let optimized = document.get_object(content_id).unwrap().as_stream().unwrap();
    assert_eq!(optimized.content, original_content);
    assert_eq!(report.structure.compressed_streams, 0);
}
#[test]
fn content_optimization_uses_the_shortest_string_representation() {
    let source = b"<2151> Tj\n[<2151> 10 <4142>] TJ\n".repeat(200);
    let expected_operations = Content::decode(&source).unwrap().operations;
    let mut stream = Stream::new(Dictionary::new(), source);
    stream.compress().unwrap();
    let (mut document, content_id) = page_document(stream);
    let options = OptimizeOptions {
        renumber_objects: false,
        ..OptimizeOptions::default()
    };

    document.optimize(&options).unwrap();

    let optimized = document.get_object(content_id).unwrap().as_stream().unwrap();
    let optimized_content = optimized.get_plain_content().unwrap();
    assert_same_operations(
        &Content::decode(&optimized_content).unwrap().operations,
        &expected_operations,
    );
    assert!(
        optimized_content
            .windows(b"(!Q)Tj".len())
            .any(|bytes| bytes == b"(!Q)Tj")
    );
    assert!(
        optimized_content
            .windows(b"(AB)]TJ".len())
            .any(|bytes| bytes == b"(AB)]TJ")
    );
}

#[test]
fn content_optimization_minifies_type3_charprocs() {
    let source = b"0 0 500 500 d1\n0 0 m 100 100 l S\n".repeat(200);
    let expected_operations = Content::decode(&source).unwrap().operations;
    let charproc = Stream::new(Dictionary::new(), source);
    let original_size = charproc.content.len();

    let mut document = Document::with_version("1.7");
    let charproc_id = document.add_object(charproc);
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 500.into(), 500.into()],
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "CharProcs" => dictionary! {
            "A" => charproc_id,
        },
        "Encoding" => dictionary! {
            "Type" => "Encoding",
            "Differences" => vec![65.into(), Object::Name(b"A".to_vec())],
        },
        "FirstChar" => 65,
        "LastChar" => 65,
        "Widths" => vec![500.into()],
        "Resources" => Dictionary::new(),
    });
    let page_content = Stream::new(Dictionary::new(), b"BT /F1 12 Tf (A) Tj ET".to_vec());
    let (mut document, _) = page_document_with_resources(
        document,
        page_content,
        dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        },
    );
    let options = OptimizeOptions {
        renumber_objects: false,
        ..OptimizeOptions::default()
    };

    document.optimize(&options).unwrap();

    let optimized = document.get_object(charproc_id).unwrap().as_stream().unwrap();
    let optimized_content = optimized.get_plain_content().unwrap();
    assert_same_operations(
        &Content::decode(&optimized_content).unwrap().operations,
        &expected_operations,
    );
    assert!(optimized.content.len() < original_size);
}

#[test]
fn conservative_image_policy_avoids_lossy_reencoding_and_preserves_lossless_dimensions() {
    let (mut lossy_document, lossy_image_id) = image_document(&[72.0], false);
    let original_jpeg = lossy_document
        .get_object(lossy_image_id)
        .unwrap()
        .as_stream()
        .unwrap()
        .content
        .clone();

    let lossy_report = lossy_document
        .optimize(&OptimizeOptions::only_images(ImageCompressionPolicy::Conservative))
        .unwrap();

    assert_eq!(lossy_report.images.rewritten_images, 0);
    assert_eq!(
        lossy_document
            .get_object(lossy_image_id)
            .unwrap()
            .as_stream()
            .unwrap()
            .content,
        original_jpeg
    );

    let (mut conservative_document, conservative_image_id) = image_document(&[72.0], false);
    *conservative_document.get_object_mut(conservative_image_id).unwrap() = lossless_rgb_image_stream().into();

    let conservative_report = conservative_document
        .optimize(&OptimizeOptions::only_images(ImageCompressionPolicy::Conservative))
        .unwrap();
    let conservative_image = conservative_document
        .get_object(conservative_image_id)
        .unwrap()
        .as_stream()
        .unwrap();

    assert_eq!(conservative_report.images.rewritten_images, 1);
    assert_eq!(conservative_report.images.resized_images, 0);
    assert_eq!(
        conservative_image.dict.get(b"Width").and_then(Object::as_i64).unwrap(),
        600
    );
    assert_eq!(
        conservative_image
            .dict
            .get(b"Filter")
            .and_then(Object::as_name)
            .unwrap(),
        b"FlateDecode"
    );

    let (mut aggressive_document, aggressive_image_id) = image_document(&[72.0], false);
    *aggressive_document.get_object_mut(aggressive_image_id).unwrap() = lossless_rgb_image_stream().into();

    let aggressive_report = aggressive_document
        .optimize(&OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive))
        .unwrap();
    let aggressive_image = aggressive_document
        .get_object(aggressive_image_id)
        .unwrap()
        .as_stream()
        .unwrap();

    assert_eq!(aggressive_report.images.rewritten_images, 1);
    assert_eq!(aggressive_report.images.resized_images, 1);
    assert_eq!(
        aggressive_image.dict.get(b"Width").and_then(Object::as_i64).unwrap(),
        150
    );
    assert_eq!(
        aggressive_image.dict.get(b"Filter").and_then(Object::as_name).unwrap(),
        b"DCTDecode"
    );
}

#[test]
fn aggressive_image_policy_downsamples_to_its_configured_target_dpi() {
    let (mut document, image_id) = image_document(&[216.0], false);

    let report = document
        .optimize(&OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive))
        .unwrap();

    assert_eq!(report.images.rewritten_images, 1);
    assert_eq!(report.images.resized_images, 1);
    let image = document.get_object(image_id).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 450);
    assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 450);
}

#[test]
fn image_optimization_follows_nested_form_ctms() {
    let (mut document, image_id) = image_document(&[72.0], true);
    let options = OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive);

    let report = document.optimize(&options).unwrap();

    assert_eq!(report.images.total_images, 1);
    assert_eq!(report.images.rewritten_images, 1);
    assert_eq!(report.images.resized_images, 1);
    let image = document.get_object(image_id).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 150);
    assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 150);
    assert_eq!(
        image.dict.get(b"Filter").and_then(Object::as_name).unwrap(),
        b"DCTDecode"
    );
}

#[test]
fn shared_image_keeps_resolution_needed_by_its_largest_use() {
    let (mut document, image_id) = image_document(&[72.0, 720.0], false);
    let options = OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive);

    let report = document.optimize(&options).unwrap();

    assert_eq!(report.images.rewritten_images, 1);
    assert_eq!(report.images.resized_images, 0);
    let image = document.get_object(image_id).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 600);
    assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 600);
}

#[test]
fn image_in_annotation_appearance_protects_resize() {
    let (mut document, image_id) = image_document(&[1.0], false);
    let appearance_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im0" => image_id },
            },
        },
        b"/Im0 Do".to_vec(),
    ));
    let annotation_id = document.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "AP" => dictionary! { "N" => appearance_id },
    });
    let page_id = *document.get_pages().values().next().unwrap();
    document
        .get_dictionary_mut(page_id)
        .unwrap()
        .set("Annots", vec![annotation_id.into()]);

    let options = OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive);
    let report = document.optimize(&options).unwrap();

    assert_eq!(report.images.rewritten_images, 1);
    assert_eq!(report.images.resized_images, 0);
    assert_eq!(
        document
            .get_object(image_id)
            .unwrap()
            .as_stream()
            .unwrap()
            .dict
            .get(b"Width")
            .unwrap()
            .as_i64()
            .unwrap(),
        600
    );
}

#[test]
fn image_optimization_rewrites_ascii85_wrapped_jpegs() {
    let (mut document, image_id) = image_document(&[72.0], false);
    let image = document.get_object_mut(image_id).unwrap().as_stream_mut().unwrap();
    let jpeg = std::mem::take(&mut image.content);
    image.dict.set(
        "Filter",
        vec![
            Object::Name(b"ASCII85Decode".to_vec()),
            Object::Name(b"DCTDecode".to_vec()),
        ],
    );
    image.set_content(ascii85_encode(&jpeg));

    let options = OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive);
    let report = document.optimize(&options).unwrap();

    assert_eq!(report.images.rewritten_images, 1);
    let image = document.get_object(image_id).unwrap().as_stream().unwrap();
    assert_eq!(
        image.dict.get(b"Filter").and_then(Object::as_name).unwrap(),
        b"DCTDecode"
    );
    assert_eq!(&image.content[..2], &[0xff, 0xd8]);
}

#[test]
fn aggressive_image_policy_rewrites_jpx_while_conservative_preserves_it() {
    let (mut conservative, conservative_image_id) = image_document(&[72.0], false);
    *conservative.get_object_mut(conservative_image_id).unwrap() = jpx_image_stream().into();
    let original = conservative
        .get_object(conservative_image_id)
        .unwrap()
        .as_stream()
        .unwrap()
        .content
        .clone();

    let conservative_report = conservative
        .optimize(&OptimizeOptions::only_images(ImageCompressionPolicy::Conservative))
        .unwrap();

    assert_eq!(conservative_report.images.rewritten_images, 0);
    assert_eq!(
        conservative_report
            .images
            .skips
            .get(&lopdf::ImageSkipReason::PolicyDisabled),
        Some(&1)
    );
    assert_eq!(
        conservative
            .get_object(conservative_image_id)
            .unwrap()
            .as_stream()
            .unwrap()
            .content,
        original
    );

    let (mut aggressive, aggressive_image_id) = image_document(&[72.0], false);
    *aggressive.get_object_mut(aggressive_image_id).unwrap() = jpx_image_stream().into();

    let aggressive_report = aggressive
        .optimize(&OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive))
        .unwrap();

    assert_eq!(aggressive_report.images.rewritten_images, 1);
    assert_eq!(aggressive_report.images.resized_images, 1);
    let image = aggressive.get_object(aggressive_image_id).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 200);
    assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 150);
    assert_eq!(
        image.dict.get(b"Filter").and_then(Object::as_name).unwrap(),
        b"DCTDecode"
    );
    assert!(image.content.len() < original.len());
}
#[test]
fn aggressive_image_policy_converts_valid_iccbased_cmyk_jpegs_to_srgb() {
    let (mut document, image_id) = image_document(&[72.0], false);
    let profile_id = document.add_object(Stream::new(
        dictionary! {
            "N" => 4,
            "Alternate" => "DeviceCMYK",
            "Filter" => "FlateDecode",
        },
        include_bytes!("fixtures/generic-cmyk.icc.zlib").to_vec(),
    ));
    let color_space_id = document.add_object(Object::Array(vec![
        Object::Name(b"ICCBased".to_vec()),
        profile_id.into(),
    ]));
    let mut image = cmyk_image_stream();
    image.dict.set(
        "Decode",
        vec![
            1.into(),
            0.into(),
            1.into(),
            0.into(),
            1.into(),
            0.into(),
            1.into(),
            0.into(),
        ],
    );
    image.dict.set("ColorSpace", color_space_id);
    *document.get_object_mut(image_id).unwrap() = image.into();

    let report = document
        .optimize(&OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive))
        .unwrap();

    assert_eq!(report.images.rewritten_images, 1);
    assert_eq!(report.images.resized_images, 1);
    let image = document.get_object(image_id).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 150);
    assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 150);
    assert!(!image.dict.has(b"Decode"));
    assert_eq!(
        image.dict.get(b"ColorSpace").and_then(Object::as_name).unwrap(),
        b"DeviceRGB"
    );

    let mut decoder = JpegDecoder::new(Cursor::new(&image.content));
    let decoded = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    assert_eq!(info.pixel_format, PixelFormat::RGB24);
    assert_eq!(decoded.len(), 150 * 150 * 3);
    assert!(document.get_object(color_space_id).is_err());
    assert!(document.get_object(profile_id).is_err());
}

#[test]
fn aggressive_image_policy_preserves_cmyk_when_icc_profile_is_invalid() {
    let (mut document, image_id) = image_document(&[72.0], false);
    let profile_id = document.add_object(Stream::new(
        dictionary! {
            "N" => 4,
            "Alternate" => "DeviceCMYK",
        },
        Vec::new(),
    ));
    let mut image = cmyk_image_stream();
    image.dict.set(
        "Decode",
        vec![
            1.into(),
            0.into(),
            1.into(),
            0.into(),
            1.into(),
            0.into(),
            1.into(),
            0.into(),
        ],
    );
    image.dict.set(
        "ColorSpace",
        vec![Object::Name(b"ICCBased".to_vec()), profile_id.into()],
    );
    *document.get_object_mut(image_id).unwrap() = image.into();

    let options = OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive);
    let report = document.optimize(&options).unwrap();

    assert_eq!(report.images.rewritten_images, 1);
    assert_eq!(report.images.resized_images, 1);
    let image = document.get_object(image_id).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 150);
    assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 150);
    assert!(!image.dict.has(b"Decode"));
    assert_eq!(
        image
            .dict
            .get(b"ColorSpace")
            .unwrap()
            .as_array()
            .unwrap()
            .first()
            .and_then(|value| value.as_name().ok())
            .unwrap(),
        b"ICCBased"
    );

    let mut decoder = JpegDecoder::new(Cursor::new(&image.content));
    let decoded = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    assert_eq!(info.pixel_format, PixelFormat::CMYK32);
    assert_eq!(decoded.len(), 150 * 150 * 4);
}

#[test]
fn image_optimization_preserves_base_images_and_their_soft_masks() {
    let (mut document, image_id) = image_document(&[72.0], false);
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 600,
            "Height" => 600,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0; 600 * 600],
    ));
    document
        .get_object_mut(image_id)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .dict
        .set("SMask", mask_id);
    let options = OptimizeOptions::only_images(ImageCompressionPolicy::Aggressive);

    let report = document.optimize(&options).unwrap();

    assert_eq!(report.images.total_images, 2);
    assert_eq!(report.images.rewritten_images, 0);
    assert_eq!(report.images.skips.get(&lopdf::ImageSkipReason::Masked), Some(&2));
}

fn page_document(content: Stream) -> (Document, lopdf::ObjectId) {
    let mut document = Document::with_version("1.7");
    let pages_id = document.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Vec::<Object>::new(),
        "Count" => 0,
    });
    let content_id = document.add_object(content);
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 72.into(), 72.into()],
        "Contents" => content_id,
        "Resources" => Dictionary::new(),
    });
    let pages = document.get_dictionary_mut(pages_id).unwrap();
    pages.set("Kids", vec![page_id.into()]);
    pages.set("Count", 1);
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    (document, content_id)
}
fn page_document_with_resources(
    mut document: Document, content: Stream, resources: Dictionary,
) -> (Document, lopdf::ObjectId) {
    let pages_id = document.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Vec::<Object>::new(),
        "Count" => 0,
    });
    let content_id = document.add_object(content);
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 72.into(), 72.into()],
        "Contents" => content_id,
        "Resources" => resources,
    });
    let pages = document.get_dictionary_mut(pages_id).unwrap();
    pages.set("Kids", vec![page_id.into()]);
    pages.set("Count", 1);
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    (document, content_id)
}

fn assert_same_operations(actual: &[lopdf::content::Operation], expected: &[lopdf::content::Operation]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.operator, expected.operator);
        assert_eq!(actual.operands.len(), expected.operands.len());
        assert!(
            actual
                .operands
                .iter()
                .zip(&expected.operands)
                .all(|(actual, expected)| objects_semantically_equal(actual, expected))
        );
    }
}

fn objects_semantically_equal(actual: &Object, expected: &Object) -> bool {
    match (actual, expected) {
        (Object::String(actual, _), Object::String(expected, _)) => actual == expected,
        (Object::Array(actual), Object::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| objects_semantically_equal(actual, expected))
        }
        (Object::Dictionary(actual), Object::Dictionary(expected)) => {
            actual.len() == expected.len()
                && actual.iter().all(|(key, value)| {
                    expected
                        .get(key)
                        .is_ok_and(|expected| objects_semantically_equal(value, expected))
                })
        }
        _ => actual == expected,
    }
}

fn image_document(draw_sizes: &[f32], nested_form: bool) -> (Document, lopdf::ObjectId) {
    let mut document = Document::with_version("1.7");
    let pages_id = document.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Vec::<Object>::new(),
        "Count" => 0,
    });
    let image_id = document.add_object(jpeg_image_stream());

    let (resource_name, resource_id) = if nested_form {
        let form_resources = dictionary! {
            "XObject" => dictionary! {
                "Im0" => image_id,
            },
        };
        let form = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
                "Resources" => form_resources,
            },
            b"/Im0 Do".to_vec(),
        );
        ("Fm0", document.add_object(form))
    } else {
        ("Im0", image_id)
    };

    let mut operations = String::new();
    for size in draw_sizes {
        operations.push_str(&format!("q\n{size} 0 0 {size} 0 0 cm\n/{resource_name} Do\nQ\n"));
    }
    let content_id = document.add_object(Stream::new(Dictionary::new(), operations.into_bytes()));
    let maximum_size = draw_sizes.iter().copied().fold(72.0_f32, f32::max);
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), maximum_size.into(), maximum_size.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "XObject" => dictionary! {
                resource_name => resource_id,
            },
        },
    });
    let pages = document.get_dictionary_mut(pages_id).unwrap();
    pages.set("Kids", vec![page_id.into()]);
    pages.set("Count", 1);
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    (document, image_id)
}

fn jpeg_image_stream() -> Stream {
    let image = RgbImage::from_fn(600, 600, |x, y| {
        image::Rgb([
            (x % 256) as u8,
            (y % 256) as u8,
            ((x.wrapping_mul(17) + y.wrapping_mul(31)) % 256) as u8,
        ])
    });
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 95)
        .encode(image.as_raw(), 600, 600, ExtendedColorType::Rgb8)
        .unwrap();
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 600,
            "Height" => 600,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    )
}

fn jpx_image_stream() -> Stream {
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 400,
            "Height" => 300,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "JPXDecode",
        },
        include_bytes!("fixtures/jpx-rgb.jp2").to_vec(),
    )
}

fn lossless_rgb_image_stream() -> Stream {
    let image = RgbImage::from_fn(600, 600, |x, y| {
        image::Rgb([
            (x % 256) as u8,
            (y % 256) as u8,
            ((x.wrapping_mul(17) + y.wrapping_mul(31)) % 256) as u8,
        ])
    });
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 600,
            "Height" => 600,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
        },
        image.into_raw(),
    )
}

fn cmyk_image_stream() -> Stream {
    let mut pixels = Vec::with_capacity(600 * 600 * 4);
    for y in 0..600_u32 {
        for x in 0..600_u32 {
            pixels.extend_from_slice(&[
                (x % 256) as u8,
                (y % 256) as u8,
                ((x.wrapping_mul(17) + y.wrapping_mul(31)) % 256) as u8,
                32,
            ]);
        }
    }
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 95)
        .encode(&pixels, 600, 600, JpegColorType::Cmyk)
        .unwrap();
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 600,
            "Height" => 600,
            "ColorSpace" => "DeviceCMYK",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    )
}

fn ascii85_encode(input: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(input.len() * 5 / 4 + 2);
    for chunk in input.chunks(4) {
        let mut value = 0_u32;
        for byte in chunk {
            value = (value << 8) | u32::from(*byte);
        }
        value <<= (4 - chunk.len()) * 8;
        if chunk.len() == 4 && value == 0 {
            encoded.push(b'z');
            continue;
        }
        let mut digits = [b'!'; 5];
        for digit in digits.iter_mut().rev() {
            *digit = (value % 85) as u8 + b'!';
            value /= 85;
        }
        encoded.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    encoded.extend_from_slice(b"~>");
    encoded
}
