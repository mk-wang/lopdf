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

#[test]
fn subsets_name_keyed_cff_without_rewriting_character_codes() {
    let font_data = minimal_name_keyed_cff(4);
    let mut document = Document::with_version("1.7");
    let pages_id = document.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Vec::<Object>::new(),
        "Count" => 0,
    });
    let font_file_id = document.add_object(Stream::new(
        dictionary! {
            "Length1" => font_data.len() as i64,
            "Subtype" => "CIDFontType0C",
        },
        font_data,
    ));
    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "MinimalCFF",
        "Flags" => 32,
        "FontBBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
        "ItalicAngle" => 0,
        "Ascent" => 1000,
        "Descent" => 0,
        "CapHeight" => 700,
        "StemV" => 80,
        "FontFile3" => font_file_id,
    });
    let descendant_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType0",
        "BaseFont" => "MinimalCFF",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 1000,
    });
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "MinimalCFF",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let content_id = document.add_object(Stream::new(Dictionary::new(), b"BT /F0 12 Tf <0003> Tj ET".to_vec()));
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
        fonts: Some(FontOptimizationOptions {
            subset_only_if_smaller: false,
            ..FontOptimizationOptions::default()
        }),
        renumber_objects: false,
        ..OptimizeOptions::default()
    };
    let report = document.optimize(&options).unwrap();

    assert_eq!(report.fonts.total_fonts, 1);
    assert_eq!(report.fonts.subset_fonts, 1);
    let subset = document.get_object(font_file_id).unwrap().as_stream().unwrap();
    let plain_subset = subset.get_plain_content().unwrap();
    assert_eq!(&plain_subset[..4], &[1, 0, 4, 4]);
    assert_eq!(
        subset.dict.get(b"Length1").and_then(Object::as_i64).unwrap(),
        plain_subset.len() as i64
    );
    assert!(
        document
            .get_dictionary(descendant_id)
            .unwrap()
            .get(b"CIDToGIDMap")
            .is_err()
    );
    assert!(
        document
            .get_dictionary(descriptor_id)
            .unwrap()
            .get(b"CIDSet")
            .and_then(Object::as_reference)
            .is_ok()
    );
    let optimized_content = document.get_page_content(page_id);
    let operations = Content::decode_strict(&optimized_content).unwrap().operations;
    assert_eq!(operations[2].operands[0].as_str().unwrap(), b"\0\x03");
}

fn minimal_name_keyed_cff(glyph_count: u16) -> Vec<u8> {
    assert!((1..=16).contains(&glyph_count));
    let mut charset = vec![0];
    for sid in 1..glyph_count {
        charset.extend_from_slice(&sid.to_be_bytes());
    }
    let mut char_strings = Vec::new();
    char_strings.extend_from_slice(&glyph_count.to_be_bytes());
    char_strings.push(1);
    char_strings.extend(1..=u8::try_from(glyph_count + 1).unwrap());
    char_strings.extend(std::iter::repeat_n(14, glyph_count as usize));

    let header = [1, 0, 4, 4];
    let name_index = [0, 1, 1, 1, 5, b'M', b'i', b'n', b'i'];
    let empty_index = [0, 0];
    let top_dict_len = 7;
    let top_index_len = 5 + top_dict_len;
    let charset_offset = header.len() + name_index.len() + top_index_len + empty_index.len() * 2;
    let char_strings_offset = charset_offset + charset.len();
    let private_offset = char_strings_offset + char_strings.len();
    let dict_number = |number: usize| u8::try_from(number + 139).unwrap();
    let top_dict = [
        dict_number(charset_offset),
        15,
        dict_number(char_strings_offset),
        17,
        dict_number(0),
        dict_number(private_offset),
        18,
    ];

    let mut cff = Vec::new();
    cff.extend_from_slice(&header);
    cff.extend_from_slice(&name_index);
    cff.extend_from_slice(&[0, 1, 1, 1, u8::try_from(top_dict.len() + 1).unwrap()]);
    cff.extend_from_slice(&top_dict);
    cff.extend_from_slice(&empty_index);
    cff.extend_from_slice(&empty_index);
    cff.extend_from_slice(&charset);
    cff.extend_from_slice(&char_strings);
    cff
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
