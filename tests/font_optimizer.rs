#![cfg(feature = "font_subsetting")]

use lopdf::{
    Dictionary, Document, FontOptimizationOptions, Object, OptimizeOptions, Stream, content::Content, dictionary,
};

#[test]
fn subsets_identity_cid_font_without_rewriting_character_codes() {
    assert_subsets_identity_cid_font(include_bytes!("../assets/fonts/Montserrat-Regular.ttf").to_vec());
}

#[test]
fn subsets_identity_cid_font_with_unsorted_sfnt_table_directory() {
    let mut font_data = include_bytes!("../assets/fonts/Montserrat-Regular.ttf").to_vec();
    reverse_sfnt_table_directory(&mut font_data);
    assert_subsets_identity_cid_font(font_data);
}

fn assert_subsets_identity_cid_font(font_data: Vec<u8>) {
    let original_font_size = font_data.len();
    let mut document = Document::with_version("1.7");
    let pages_id = document.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Vec::<Object>::new(),
        "Count" => 0,
    });
    let font_file_id = document.add_object(Stream::new(
        dictionary! { "Length1" => original_font_size as i64 },
        font_data,
    ));
    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "Montserrat-Regular",
        "Flags" => 32,
        "FontBBox" => vec![0.into(), (-300).into(), 1200.into(), 1100.into()],
        "ItalicAngle" => 0,
        "Ascent" => 1000,
        "Descent" => -300,
        "CapHeight" => 700,
        "StemV" => 80,
        "FontFile2" => font_file_id,
    });
    let descendant_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "Montserrat-Regular",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "CIDToGIDMap" => "Identity",
        "DW" => 1000,
    });
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "Montserrat-Regular",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let content_id = document.add_object(Stream::new(
        Dictionary::new(),
        b"BT /F0 12 Tf <000100020003> Tj ET".to_vec(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F0" => font_id },
        },
    });
    document
        .get_dictionary_mut(pages_id)
        .unwrap()
        .set("Kids", vec![page_id.into()]);
    document.get_dictionary_mut(pages_id).unwrap().set("Count", 1);
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);

    let options = OptimizeOptions {
        fonts: Some(FontOptimizationOptions::default()),
        renumber_objects: false,
        ..OptimizeOptions::default()
    };
    let report = document.optimize(&options).unwrap();

    assert_eq!(report.fonts.total_fonts, 1);
    assert_eq!(report.fonts.subset_fonts, 1);
    assert!(report.fonts.font_bytes_saved > original_font_size / 2);
    let subset = document.get_object(font_file_id).unwrap().as_stream().unwrap();
    assert!(subset.content.len() < original_font_size / 2);
    let cid_map_id = document
        .get_dictionary(descendant_id)
        .unwrap()
        .get(b"CIDToGIDMap")
        .and_then(Object::as_reference)
        .unwrap();
    let cid_map = document
        .get_object(cid_map_id)
        .unwrap()
        .as_stream()
        .unwrap()
        .get_plain_content()
        .unwrap();
    assert_eq!(&cid_map[0..8], &[0, 0, 0, 1, 0, 2, 0, 3]);
    let optimized_content = document.get_page_content(page_id);
    let operations = Content::decode_strict(&optimized_content).unwrap().operations;
    assert_eq!(operations.len(), 4);
    assert_eq!(operations[2].operator, "Tj");
    assert_eq!(operations[2].operands.len(), 1);
    assert_eq!(operations[2].operands[0].as_str().unwrap(), b"\0\x01\0\x02\0\x03");
}

fn reverse_sfnt_table_directory(font_data: &mut [u8]) {
    let table_count = u16::from_be_bytes(font_data[4..6].try_into().unwrap()) as usize;
    let directory_end = 12 + table_count * 16;
    let directory = &mut font_data[12..directory_end];
    for index in 0..table_count / 2 {
        let opposite = table_count - index - 1;
        for byte in 0..16 {
            directory.swap(index * 16 + byte, opposite * 16 + byte);
        }
    }
    assert!(!directory.chunks_exact(16).map(|record| &record[..4]).is_sorted());
}
