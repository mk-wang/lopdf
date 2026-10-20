//! Per-load aggregate decompression budget
//! (`LoadOptions::max_total_decompressed_size`).
//!
//! `max_decompressed_size` bounds each stream separately; these tests cover the
//! shared budget that covers every xref and object stream decoded during one
//! load, including lenient loading and encrypted documents.

#![cfg(not(feature = "async"))]

use std::io::Write;

use flate2::{Compression, write::ZlibEncoder};
use lopdf::{
    DecompressError, Dictionary, Document, Error, LoadOptions, Object, Permissions, Stream,
    encryption::{EncryptionState, EncryptionVersion},
};

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// A valid object-stream payload of exactly `total_len` bytes whose index block
/// points at one embedded object with object number `object_number`.
fn objstm_payload(object_number: u32, total_len: usize) -> (Vec<u8>, usize) {
    let index = format!("{object_number} 0 ");
    let prefix = "<< /Pad /";
    let suffix = " >>";
    let fill = total_len - index.len() - prefix.len() - suffix.len();
    let mut content = index.clone().into_bytes();
    content.extend_from_slice(prefix.as_bytes());
    content.extend(std::iter::repeat_n(b'A', fill));
    content.extend_from_slice(suffix.as_bytes());
    assert_eq!(content.len(), total_len);
    (content, index.len())
}

/// A classic-xref PDF whose only compressed payloads are `streams.len()`
/// object streams, each a separate normal xref entry.
fn pdf_with_objstms(streams: &[(Vec<u8>, usize)]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.5\n".to_vec();
    let mut offsets = Vec::new();

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    for (index, (content, first)) in streams.iter().enumerate() {
        offsets.push(pdf.len());
        let compressed = zlib(content);
        pdf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Filter /FlateDecode /Length {} >>\nstream\n",
                index + 2,
                compressed.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

fn push_xref_entry(body: &mut Vec<u8>, kind: u8, field: u32, generation: u8) {
    body.push(kind);
    body.extend_from_slice(&field.to_be_bytes()[1..]);
    body.push(generation);
}

/// A PDF whose xref is a Flate-compressed stream (`W [1 3 1]`, 5 entries, so 25
/// decoded bytes) and which also contains one Flate-compressed object stream of
/// `objstm_len` decoded bytes.
fn pdf_with_xref_stream_and_objstm(objstm_len: usize) -> (Vec<u8>, usize, usize) {
    const XREF_DECODED_LEN: usize = 25;

    let (content, first) = objstm_payload(100, objstm_len);
    let compressed_objstm = zlib(&content);

    let mut pdf = b"%PDF-1.5\n".to_vec();

    let catalog_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let objstm_offset = pdf.len();
    pdf.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed_objstm.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&compressed_objstm);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = pdf.len();
    let mut body = Vec::with_capacity(XREF_DECODED_LEN);
    push_xref_entry(&mut body, 0, 0, 255);
    push_xref_entry(&mut body, 1, catalog_offset as u32, 0);
    push_xref_entry(&mut body, 1, objstm_offset as u32, 0);
    push_xref_entry(&mut body, 1, xref_offset as u32, 0);
    push_xref_entry(&mut body, 0, 0, 0);
    assert_eq!(body.len(), XREF_DECODED_LEN);
    let compressed_xref = zlib(&body);

    pdf.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 5 /W [1 3 1] /Index [0 5] /Root 1 0 R \
             /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed_xref.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&compressed_xref);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    pdf.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

    (pdf, XREF_DECODED_LEN, objstm_len)
}

fn assert_budget_error(result: lopdf::Result<Document>, expected_limit: usize) {
    match result {
        Err(Error::Decompress(DecompressError::MemoryLimitExceeded { limit })) => {
            assert_eq!(limit, expected_limit, "error reported the wrong aggregate limit");
        }
        Err(other) => panic!("expected aggregate MemoryLimitExceeded, got {other:?}"),
        Ok(_) => panic!("expected aggregate MemoryLimitExceeded, but the document loaded"),
    }
}

#[test]
fn aggregate_budget_rejects_individually_small_object_streams() {
    let (a, first_a) = objstm_payload(100, 400);
    let (b, first_b) = objstm_payload(101, 400);
    let pdf = pdf_with_objstms(&[(a, first_a), (b, first_b)]);

    // 400 + 400 = 800 decoded bytes. Each stream is fine on its own and no
    // per-stream cap is set, but the shared total is not.
    // Lenient (default) mode must still surface the refusal.
    assert_budget_error(
        Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(700)),
        700,
    );

    // Exactly 800 is the boundary and succeeds.
    Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(800))
        .expect("decoding exactly 800 bytes against an 800-byte budget should succeed");
}

#[test]
fn budget_is_per_load_not_process_wide() {
    let (content, first) = objstm_payload(200, 300);
    let pdf = pdf_with_objstms(&[(content, first)]);

    for attempt in 0..2 {
        Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(300))
            .unwrap_or_else(|err| panic!("load {attempt} should get a fresh budget, got {err:?}"));
    }
}

#[test]
fn xref_and_object_streams_share_one_budget() {
    let (pdf, xref_len, objstm_len) = pdf_with_xref_stream_and_objstm(300);
    let total = xref_len + objstm_len;

    // One byte short of both decodes fails...
    assert_budget_error(
        Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(total - 1)),
        total - 1,
    );
    // ...while the exact sum succeeds, proving both decodes are charged.
    Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(total))
        .expect("xref stream plus object stream should fit their exact combined size");
}

#[test]
fn xref_reconstruction_preserves_the_budget_error() {
    let (mut pdf, xref_len, _) = pdf_with_xref_stream_and_objstm(300);
    let marker = b"startxref";
    let start = pdf.windows(marker.len()).rposition(|bytes| bytes == marker).unwrap();
    pdf.truncate(start);
    pdf.extend_from_slice(b"%%EOF\n");
    let repaired = Document::load_mem_with_options(&pdf, LoadOptions::default()).unwrap();
    assert!(repaired.objects.contains_key(&(1, 0)));
    assert_budget_error(
        Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(xref_len - 1)),
        xref_len - 1,
    );
}

#[test]
fn corrupt_object_stream_is_not_reported_as_budget_exhaustion() {
    let (content, _) = objstm_payload(100, 64);
    // Declare a /First offset past the decoded payload: a structural error,
    // not a budget refusal, even with a generous budget.
    let pdf = pdf_with_objstms(&[(content, 999)]);

    match Document::load_mem_with_options(
        &pdf,
        LoadOptions {
            strict: true,
            max_total_decompressed_size: Some(1024),
            ..Default::default()
        },
    ) {
        Err(Error::InvalidOffset(999)) => {}
        Err(Error::Decompress(DecompressError::MemoryLimitExceeded { limit })) => {
            panic!("corrupt object stream was misreported as budget exhaustion (limit {limit})")
        }
        Err(other) => panic!("expected InvalidOffset(999), got {other:?}"),
        Ok(_) => panic!("corrupt object stream loaded in strict mode"),
    }
}

#[cfg(feature = "rayon")]
#[test]
fn aggregate_budget_holds_when_object_streams_decode_in_parallel() {
    let streams: Vec<_> = (0..4).map(|i| objstm_payload(100 + i, 100)).collect();
    let pdf = pdf_with_objstms(&streams);

    // 400 decoded bytes across four containers. With rayon they decode
    // concurrently; the shared counter must reject the total, not let every
    // worker spend the remaining allowance on its own.
    assert_budget_error(
        Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(350)),
        350,
    );
    Document::load_mem_with_options(&pdf, LoadOptions::with_max_total_decompressed_size(400))
        .expect("four 100-byte object streams fit a 400-byte budget");
}

// --- Encrypted packed objects -------------------------------------------
//
// The writer refuses to build new object streams for encrypted documents (the
// file encryption key is gone by then), and the repository has no encrypted
// object-stream fixture, so build a minimal RC4 one by hand: encrypt the
// container with lopdf's own primitives, then serialize it with an xref stream
// that marks the packed object as compressed.

fn hex(out: &mut Vec<u8>, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize]);
        out.push(DIGITS[(byte & 0x0f) as usize]);
    }
}

fn write_pdf_object(out: &mut Vec<u8>, object: &Object) {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(value) => out.extend_from_slice(if *value { &b"true"[..] } else { &b"false"[..] }),
        Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Real(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Name(name) => {
            out.push(b'/');
            out.extend_from_slice(name);
        }
        // Hex strings avoid escaping; the reader treats both forms alike.
        Object::String(bytes, _) => {
            out.push(b'<');
            hex(out, bytes);
            out.push(b'>');
        }
        Object::Array(items) => {
            out.push(b'[');
            for item in items {
                write_pdf_object(out, item);
                out.push(b' ');
            }
            out.push(b']');
        }
        Object::Dictionary(dict) => write_pdf_dict(out, dict),
        Object::Reference((number, generation)) => out.extend_from_slice(format!("{number} {generation} R").as_bytes()),
        Object::Stream(_) => panic!("this fixture writes streams explicitly"),
    }
}

fn write_pdf_dict(out: &mut Vec<u8>, dict: &Dictionary) {
    out.extend_from_slice(b"<< ");
    for (key, value) in dict.iter() {
        out.push(b'/');
        out.extend_from_slice(key);
        out.push(b' ');
        write_pdf_object(out, value);
        out.push(b' ');
    }
    out.extend_from_slice(b">>");
}

fn encrypted_objstm_pdf(decoded_len: usize) -> (Vec<u8>, usize) {
    let (plain, first) = objstm_payload(100, decoded_len);
    let compressed = zlib(&plain);

    let mut doc = Document::with_version("1.5");
    doc.trailer.set(
        "ID",
        Object::Array(vec![
            Object::string_literal(vec![0x11u8; 16]),
            Object::string_literal(vec![0x11u8; 16]),
        ]),
    );
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &mut doc,
        owner_password: "owner",
        user_password: "",
        key_length: 128,
        permissions: Permissions::PRINTABLE,
    })
    .unwrap();

    let mut dict = Dictionary::new();
    dict.set("Type", "ObjStm");
    dict.set("N", 1);
    dict.set("First", first as i64);
    dict.set("Filter", "FlateDecode");
    let mut container = Object::Stream(Stream::new(dict, compressed.clone()));
    lopdf::encryption::encrypt_object(&state, (2, 0), &mut container).unwrap();
    let encrypted = container.as_stream().unwrap();
    assert_eq!(encrypted.content.len(), compressed.len(), "RC4 must preserve length");
    let encrypt_dict = state.encode().unwrap();
    let file_id = doc.trailer.get(b"ID").unwrap().clone();

    let mut pdf = b"%PDF-1.5\n".to_vec();
    let catalog_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let objstm_offset = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n");
    write_pdf_dict(&mut pdf, &encrypted.dict);
    pdf.extend_from_slice(b"\nstream\n");
    pdf.extend_from_slice(&encrypted.content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    let encrypt_offset = pdf.len();
    pdf.extend_from_slice(b"3 0 obj\n");
    write_pdf_dict(&mut pdf, &encrypt_dict);
    pdf.extend_from_slice(b"\nendobj\n");

    let xref_offset = pdf.len();
    // Index [0 5 100 1]: objects 0-4 and the packed object 100.
    let mut body = Vec::with_capacity(30);
    push_xref_entry(&mut body, 0, 0, 255);
    push_xref_entry(&mut body, 1, catalog_offset as u32, 0);
    push_xref_entry(&mut body, 1, objstm_offset as u32, 0);
    push_xref_entry(&mut body, 1, encrypt_offset as u32, 0);
    push_xref_entry(&mut body, 1, xref_offset as u32, 0);
    push_xref_entry(&mut body, 2, 2, 0);
    assert_eq!(body.len(), 30);

    let mut xref_dict = Dictionary::new();
    xref_dict.set("Type", "XRef");
    xref_dict.set("Size", 101);
    xref_dict.set("Length", body.len() as i64);
    xref_dict.set("W", Object::Array(vec![1.into(), 3.into(), 1.into()]));
    xref_dict.set("Index", Object::Array(vec![0.into(), 5.into(), 100.into(), 1.into()]));
    xref_dict.set("Root", Object::Reference((1, 0)));
    xref_dict.set("Encrypt", Object::Reference((3, 0)));
    xref_dict.set("ID", file_id);
    pdf.extend_from_slice(b"4 0 obj\n");
    write_pdf_dict(&mut pdf, &xref_dict);
    pdf.extend_from_slice(b"\nstream\n");
    pdf.extend_from_slice(&body);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    pdf.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

    (pdf, decoded_len)
}

#[test]
fn encrypted_object_streams_are_charged_to_the_aggregate_budget() {
    let (pdf, decoded_len) = encrypted_objstm_pdf(300);

    Document::load_mem_with_raw_password_and_options(&pdf, b"", LoadOptions::default())
        .expect("the hand-built RC4 object stream fixture should load without a budget");

    // The xref stream in this fixture is uncompressed and costs nothing; the
    // decrypted object stream must be charged exactly once.
    Document::load_mem_with_raw_password_and_options(
        &pdf,
        b"",
        LoadOptions::with_max_total_decompressed_size(decoded_len),
    )
    .expect("the exact decoded size should fit the budget");

    assert_budget_error(
        Document::load_mem_with_raw_password_and_options(
            &pdf,
            b"",
            LoadOptions::with_max_total_decompressed_size(decoded_len - 1),
        ),
        decoded_len - 1,
    );
}
