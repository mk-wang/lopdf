#![cfg(not(feature = "async"))]

use lopdf::{Document, IncrementalDocument, LoadOptions, Object, SaveOptions, Stream, dictionary, xref::XrefType};

fn append_object(pdf: &mut Vec<u8>, id: u32, body: &[u8]) -> usize {
    let offset = pdf.len();
    pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
    pdf.extend_from_slice(body);
    pdf.extend_from_slice(b"\nendobj\n");
    offset
}

fn append_xref_table(pdf: &mut Vec<u8>, entries: &[(u32, usize)], trailer: &str) -> usize {
    let offset = pdf.len();
    pdf.extend_from_slice(b"xref\n");
    pdf.extend_from_slice(format!("0 {}\n", entries.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for (_, object_offset) in entries {
        pdf.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(b"trailer\n");
    pdf.extend_from_slice(trailer.as_bytes());
    pdf.extend_from_slice(b"\n");
    offset
}

fn hybrid_incremental_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.5\n".to_vec();
    let old_catalog = append_object(&mut pdf, 1, b"<< /Type /Catalog >>");
    let old_info = append_object(&mut pdf, 2, b"<< /Old (yes) >>");
    let old_xref = append_xref_table(
        &mut pdf,
        &[(1, old_catalog), (2, old_info)],
        "<< /Size 3 /Root 1 0 R >>",
    );
    pdf.extend_from_slice(format!("startxref\n{old_xref}\n%%EOF\n").as_bytes());

    let latest_info = append_object(&mut pdf, 3, b"<< /Title (latest) >>");
    let companion_object = append_object(&mut pdf, 4, b"<< /Marker /Companion >>");

    let companion_offset = pdf.len();
    let companion_entry = [
        1,
        ((companion_object >> 24) & 0xff) as u8,
        ((companion_object >> 16) & 0xff) as u8,
        ((companion_object >> 8) & 0xff) as u8,
        (companion_object & 0xff) as u8,
        0,
        0,
    ];
    pdf.extend_from_slice(b"5 0 obj\n");
    pdf.extend_from_slice(
        format!(
            "<< /Type /XRef /Size 6 /W [1 4 2] /Index [4 1] /Length {} >>\nstream\n",
            companion_entry.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&companion_entry);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    let latest_xref = pdf.len();
    pdf.extend_from_slice(b"xref\n3 1\n");
    pdf.extend_from_slice(format!("{latest_info:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(b"5 1\n");
    pdf.extend_from_slice(format!("{companion_offset:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(b"trailer\n");
    pdf.extend_from_slice(
        format!("<< /Size 6 /Root 1 0 R /Info 3 0 R /XRefStm {companion_offset} /Prev {old_xref} >>\n").as_bytes(),
    );
    pdf.extend_from_slice(format!("startxref\n{latest_xref}\n%%EOF").as_bytes());
    pdf
}

#[test]
fn revision_local_hybrid_xref_merges_each_revision() {
    let pdf = hybrid_incremental_pdf();
    let mut document = Document::load_mem(&pdf).expect("hybrid incremental PDF should load");

    assert_eq!(document.max_id, 5);
    assert!(document.get_object((2, 0)).is_ok(), "older revision object was lost");
    assert!(document.get_object((4, 0)).is_ok(), "latest companion object was lost");
    assert!(document.trailer.get(b"Prev").is_err());
    assert!(document.trailer.get(b"XRefStm").is_err());
    assert_eq!(
        Document::load_metadata_mem(&pdf).unwrap().title.as_deref(),
        Some("latest")
    );

    let mut rewritten = Vec::new();
    document.save_to(&mut rewritten).unwrap();
    let rewritten = Document::load_mem(&rewritten).unwrap();
    assert!(rewritten.get_object((2, 0)).is_ok());
    assert!(rewritten.get_object((4, 0)).is_ok());
}

fn wrong_object_offset_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let catalog = append_object(&mut pdf, 1, b"<< /Type /Catalog >>");
    let marker = append_object(&mut pdf, 2, b"<< /Marker /Recovered >>");
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    pdf.extend_from_slice(format!("{catalog:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(format!("{catalog:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
    pdf.extend_from_slice(format!("startxref\n{xref}\n%%EOF").as_bytes());
    assert_ne!(catalog, marker);
    pdf
}

#[test]
fn stale_normal_xref_entry_is_repaired_once() {
    let pdf = wrong_object_offset_pdf();
    let document = Document::load_mem(&pdf).expect("lenient loading should repair the offset");
    assert_eq!(
        document
            .get_object((2, 0))
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Marker")
            .unwrap()
            .as_name()
            .unwrap(),
        b"Recovered"
    );

    let strict = Document::load_mem_with_options(
        &pdf,
        LoadOptions {
            strict: true,
            ..Default::default()
        },
    );
    assert!(strict.is_err(), "strict loading must not silently repair a stale xref");
}

fn malformed_stream_pdf(length: i64) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let object = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n");
    pdf.extend_from_slice(format!("<< /Length {length} >>\nstream\nabc\nendstream\nendobj\n").as_bytes());
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
    pdf.extend_from_slice(format!("{object:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n");
    pdf.extend_from_slice(format!("startxref\n{xref}\n%%EOF").as_bytes());
    pdf
}

#[test]
fn lenient_stream_recovery_uses_endstream_boundary() {
    for length in [1, 99] {
        let pdf = malformed_stream_pdf(length);
        let document = Document::load_mem(&pdf).expect("lenient stream recovery should find endstream");
        assert_eq!(
            document.get_object((1, 0)).unwrap().as_stream().unwrap().content,
            b"abc"
        );
        assert!(
            Document::load_mem_with_options(
                &pdf,
                LoadOptions {
                    strict: true,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }

    let negative = Document::load_mem(&malformed_stream_pdf(-1)).unwrap();
    assert!(
        negative.get_object((1, 0)).is_err(),
        "negative stream lengths must not be recovered"
    );
}

#[test]
fn writer_replaces_stale_stream_length() {
    let mut document = Document::new();
    let stream_id = document.add_object(Stream::new(dictionary! { "Length" => 1 }, b"abc".to_vec()));
    let mut output = Vec::new();
    document.save_to(&mut output).unwrap();
    assert!(output.windows(b"/Length 3".len()).any(|window| window == b"/Length 3"));
    assert_eq!(
        Document::load_mem(&output)
            .unwrap()
            .get_object(stream_id)
            .unwrap()
            .as_stream()
            .unwrap()
            .content,
        b"abc"
    );
}

#[test]
fn save_preflight_does_not_truncate_existing_path() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("existing.pdf");
    std::fs::write(&path, b"sentinel").unwrap();

    let mut document = Document::new();
    document.binary_mark = vec![1, 2, 3, 4];
    assert!(document.save(&path).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"sentinel");
}

#[test]
fn xref_stream_id_reservation_fails_without_wrapping() {
    let mut document = Document::new();
    document.max_id = u32::MAX - 1;
    document.reference_table.cross_reference_type = XrefType::CrossReferenceStream;
    let mut output = Vec::new();
    assert!(document.save_to(&mut output).is_err());
    assert!(output.is_empty());
}

#[test]
fn incremental_xref_stream_reservation_fails_before_output() {
    let mut previous = Document::new();
    let mut previous_bytes = Vec::new();
    previous
        .save_with_options(
            &mut previous_bytes,
            SaveOptions {
                use_xref_streams: true,
                ..Default::default()
            },
        )
        .unwrap();
    let previous = Document::load_mem(&previous_bytes).unwrap();
    let mut incremental = IncrementalDocument::create_from(previous_bytes, previous);
    incremental.new_document.max_id = u32::MAX - 1;

    let mut output = Vec::new();
    assert!(incremental.save_to(&mut output).is_err());
    assert!(output.is_empty());

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("existing.pdf");
    std::fs::write(&path, b"sentinel").unwrap();
    assert!(incremental.save(&path).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"sentinel");
}

#[test]
fn metadata_accepts_compact_and_comment_separated_object_headers() {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let catalog = pdf.len();
    pdf.extend_from_slice(b"1 0\0obj<</Type/Catalog/Pages 2 0 R>>endobj\n");
    let pages = pdf.len();
    pdf.extend_from_slice(b"2 0 obj% header comment\n<</Type/Pages/Kids[]/Count 0>>endobj\n");
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    pdf.extend_from_slice(format!("{catalog:010} 00000 n \n{pages:010} 00000 n \n").as_bytes());
    pdf.extend_from_slice(b"trailer\n<</Root 1 0 R/Size 3>>\n");
    pdf.extend_from_slice(format!("startxref\n{xref}\n%%EOF").as_bytes());

    let metadata = Document::load_metadata_mem(&pdf).unwrap();
    assert_eq!(metadata.page_count, 0);
    assert_eq!(Document::load_mem(&pdf).unwrap().xref_start, xref);
}

#[test]
fn traversal_and_pruning_handle_shared_cycles_once() {
    let mut document = Document::new();
    document.trailer.set("Root", Object::Reference((1, 0)));
    document.objects.insert(
        (1, 0),
        dictionary! {
            "First" => Object::Reference((2, 0)),
            "Second" => vec![Object::Reference((2, 0)), Object::Reference((3, 0))],
            "Stream" => Object::Reference((4, 0)),
        }
        .into(),
    );
    document
        .objects
        .insert((2, 0), dictionary! { "Next" => Object::Reference((3, 0)) }.into());
    document.objects.insert(
        (3, 0),
        Stream::new(dictionary! { "Back" => Object::Reference((2, 0)) }, Vec::new()).into(),
    );
    document.objects.insert((4, 0), dictionary! {}.into());
    document.objects.insert((5, 0), Object::Null);

    let refs = document.traverse_objects(|_| {});
    assert_eq!(refs, vec![(1, 0), (2, 0), (3, 0), (4, 0)]);
    assert_eq!(document.prune_objects(), vec![(5, 0)]);
}

#[test]
fn recovery_skips_fake_objects_and_trailers_inside_streams() {
    let stream_payload = b"2 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 2 0 R >>\n";
    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.extend_from_slice(b"1 0 obj\n");
    pdf.extend_from_slice(format!("<< /Length {} >>\nstream\n", stream_payload.len()).as_bytes());
    pdf.extend_from_slice(stream_payload);
    pdf.extend_from_slice(b"endstream\nendobj\n");
    append_object(&mut pdf, 3, b"<< /Type /Catalog /Pages 4 0 R >>");
    append_object(&mut pdf, 4, b"<< /Type /Pages /Kids [] /Count 0 >>");
    pdf.extend_from_slice(b"trailer\n<< /Root 3 0 R >>\n");

    let document = Document::load_mem(&pdf).expect("recovery should skip stream payload tokens");
    assert_eq!(document.trailer.get(b"Root").unwrap().as_reference().unwrap(), (3, 0));
    assert!(document.get_object((2, 0)).is_err());
    assert_eq!(document.get_pages().len(), 0);
}
