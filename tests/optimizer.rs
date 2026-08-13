#![cfg(feature = "embed_image")]

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, RgbImage};
use lopdf::{Dictionary, Document, Object, OptimizeOptions, Stream, content::Content, dictionary};

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
fn image_optimization_follows_nested_form_ctms() {
    let (mut document, image_id) = image_document(&[72.0], true);
    let mut options = OptimizeOptions::maximum_compression();
    options.renumber_objects = false;

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
    let mut options = OptimizeOptions::maximum_compression();
    options.renumber_objects = false;

    let report = document.optimize(&options).unwrap();

    assert_eq!(report.images.rewritten_images, 1);
    assert_eq!(report.images.resized_images, 0);
    let image = document.get_object(image_id).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 600);
    assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 600);
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
    let mut options = OptimizeOptions::maximum_compression();
    options.renumber_objects = false;

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
