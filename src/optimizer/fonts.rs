use super::*;
use crate::content::Content;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use subsetter::{GlyphRemapper, subset};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontProgramKind {
    TrueType,
    Cff,
}

#[derive(Debug, Clone, Copy)]
struct FontCandidate {
    font_id: ObjectId,
    descendant_id: ObjectId,
    descriptor_id: ObjectId,
    font_file_id: ObjectId,
    kind: FontProgramKind,
}

#[derive(Debug, Default)]
struct FontUsageAnalysis {
    glyphs: BTreeMap<ObjectId, BTreeSet<u16>>,
    invalid_fonts: HashSet<ObjectId>,
    interactive_fonts: HashSet<ObjectId>,
    skipped_content_streams: usize,
    incomplete: bool,
}

#[derive(Debug, Clone, Copy)]
enum SelectedFont {
    Composite(ObjectId),
    Other,
}

impl Document {
    pub(super) fn optimize_fonts(&mut self, options: &FontOptimizationOptions) -> Result<FontOptimizationReport> {
        let font_ids: Vec<_> = self
            .objects
            .iter()
            .filter_map(|(&id, object)| {
                object_dictionary(object)
                    .is_some_and(|dictionary| {
                        dictionary.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type0")
                    })
                    .then_some(id)
            })
            .collect();
        let analysis = analyze_font_usage(self, options.max_decoded_content_bytes);
        let mut report = FontOptimizationReport {
            total_fonts: font_ids.len(),
            content_streams_skipped: analysis.skipped_content_streams,
            ..FontOptimizationReport::default()
        };

        if analysis.incomplete {
            for font_id in font_ids {
                let reason = if analysis.interactive_fonts.contains(&font_id) {
                    FontSkipReason::InteractiveFormResource
                } else {
                    FontSkipReason::DecodeFailed
                };
                report.skip(reason);
            }
            return Ok(report);
        }

        let protected_font_files: HashSet<_> = analysis
            .interactive_fonts
            .iter()
            .filter_map(|font_id| font_file_id_for_type0(self, *font_id))
            .collect();
        let unsafe_font_files: HashSet<_> = analysis
            .invalid_fonts
            .iter()
            .chain(
                analysis
                    .glyphs
                    .keys()
                    .filter(|font_id| font_candidate(self, **font_id).is_err()),
            )
            .filter_map(|font_id| font_file_id_for_type0(self, *font_id))
            .collect();

        let mut groups: BTreeMap<ObjectId, Vec<(FontCandidate, BTreeSet<u16>)>> = BTreeMap::new();
        for font_id in font_ids {
            if analysis.interactive_fonts.contains(&font_id) {
                report.skip(FontSkipReason::InteractiveFormResource);
                continue;
            }
            if analysis.invalid_fonts.contains(&font_id) {
                report.skip(FontSkipReason::InvalidContentCode);
                continue;
            }
            let Some(glyphs) = analysis.glyphs.get(&font_id) else {
                continue;
            };
            match font_candidate(self, font_id) {
                Ok(candidate) => groups
                    .entry(candidate.font_file_id)
                    .or_default()
                    .push((candidate, glyphs.clone())),
                Err(reason) => report.skip(reason),
            }
        }

        let all_file_descriptors = font_file_descriptors(self);
        for (font_file_id, candidates) in groups {
            if protected_font_files.contains(&font_file_id) {
                for _ in candidates {
                    report.skip(FontSkipReason::InteractiveFormResource);
                }
                continue;
            }
            if unsafe_font_files.contains(&font_file_id) {
                for _ in candidates {
                    report.skip(FontSkipReason::UnsupportedFont);
                }
                continue;
            }

            let candidate_descriptors: HashSet<_> = candidates
                .iter()
                .map(|(candidate, _)| candidate.descriptor_id)
                .collect();
            if all_file_descriptors
                .get(&font_file_id)
                .is_some_and(|descriptors| !descriptors.is_subset(&candidate_descriptors))
            {
                for _ in candidates {
                    report.skip(FontSkipReason::UnsupportedFont);
                }
                continue;
            }

            let mut glyphs = BTreeSet::from([0]);
            for (_, used) in &candidates {
                glyphs.extend(used);
            }
            let old_stream = match self
                .objects
                .get(&font_file_id)
                .and_then(|object| object.as_stream().ok())
            {
                Some(stream) => stream.clone(),
                None => {
                    for _ in candidates {
                        report.skip(FontSkipReason::MissingFontProgram);
                    }
                    continue;
                }
            };
            let old_size = old_stream.content.len();
            let font_data = match old_stream.get_plain_content_with_limit(options.max_decoded_font_bytes) {
                Ok(data) => data,
                Err(Error::Decompress(crate::DecompressError::MemoryLimitExceeded { .. })) => {
                    for _ in candidates {
                        report.skip(FontSkipReason::DecodeLimitExceeded);
                    }
                    continue;
                }
                Err(_) => {
                    for _ in candidates {
                        report.skip(FontSkipReason::DecodeFailed);
                    }
                    continue;
                }
            };
            let kind = candidates.first().map(|(candidate, _)| candidate.kind).unwrap();
            if candidates.iter().any(|(candidate, _)| candidate.kind != kind) {
                for _ in candidates {
                    report.skip(FontSkipReason::UnsupportedFont);
                }
                continue;
            }
            let subset_glyphs = match kind {
                FontProgramKind::TrueType => glyphs.clone(),
                FontProgramKind::Cff => {
                    let Some(glyphs) = identity_cff_glyphs(&font_data, &glyphs) else {
                        for _ in candidates {
                            report.skip(FontSkipReason::UnsupportedFont);
                        }
                        continue;
                    };
                    glyphs
                }
            };
            let glyph_list: Vec<_> = subset_glyphs.iter().copied().collect();
            let remapper = GlyphRemapper::new_from_glyphs_sorted(&glyph_list);
            let subset_data = match kind {
                FontProgramKind::TrueType => {
                    let normalized_font_data = normalize_sfnt_table_directory(&font_data);
                    let subset_source = normalized_font_data.as_deref().unwrap_or(&font_data);
                    subset(subset_source, 0, &remapper).ok()
                }
                FontProgramKind::Cff => subset_cff_program(&font_data, &remapper),
            };
            let Some(subset_data) = subset_data else {
                for _ in candidates {
                    report.skip(FontSkipReason::SubsetFailed);
                }
                continue;
            };
            let subset_length = subset_data.len();
            let mut dictionary = old_stream.dict.clone();
            dictionary.remove(b"DecodeParms");
            dictionary.remove(b"Filter");
            dictionary.set("Length1", subset_length as i64);
            let mut subset_stream = Stream::new(dictionary, subset_data);
            let _ = subset_stream.compress();
            let new_size = subset_stream.content.len();
            if options.subset_only_if_smaller && new_size >= old_size {
                for _ in candidates {
                    report.skip(FontSkipReason::NotSmaller);
                }
                continue;
            }

            let cid_to_gid =
                (kind == FontProgramKind::TrueType).then(|| self.add_object(cid_to_gid_stream(&glyphs, &remapper)));
            let cid_set = self.add_object(cid_set_stream(&subset_glyphs));
            let mut updated_descendants = HashSet::new();
            let mut updated_descriptors = HashSet::new();
            let subset_tag = subset_tag(font_file_id, &subset_glyphs);
            for (candidate, _) in &candidates {
                if let Ok(dictionary) = self.get_dictionary(candidate.font_id)
                    && let Ok(base_font) = dictionary.get(b"BaseFont").and_then(Object::as_name)
                {
                    let name = subset_name(base_font, &subset_tag);
                    self.get_dictionary_mut(candidate.font_id)?
                        .set("BaseFont", Object::Name(name));
                }
                if updated_descendants.insert(candidate.descendant_id) {
                    let descendant = self.get_dictionary_mut(candidate.descendant_id)?;
                    if kind == FontProgramKind::TrueType {
                        descendant.set("CIDToGIDMap", cid_to_gid.expect("TrueType CIDToGIDMap"));
                    }
                    if let Ok(base_font) = descendant.get(b"BaseFont").and_then(Object::as_name) {
                        let name = subset_name(base_font, &subset_tag);
                        descendant.set("BaseFont", Object::Name(name));
                    }
                }
                if updated_descriptors.insert(candidate.descriptor_id) {
                    let descriptor = self.get_dictionary_mut(candidate.descriptor_id)?;
                    descriptor.set("CIDSet", cid_set);
                    if let Ok(font_name) = descriptor.get(b"FontName").and_then(Object::as_name) {
                        let name = subset_name(font_name, &subset_tag);
                        descriptor.set("FontName", Object::Name(name));
                    }
                }
            }
            self.objects.insert(font_file_id, Object::Stream(subset_stream));
            report.subset_fonts += candidates
                .iter()
                .map(|(candidate, _)| candidate.font_id)
                .collect::<HashSet<_>>()
                .len();
            report.original_program_bytes += old_size;
            report.optimized_program_bytes += new_size;
            report.font_bytes_saved += old_size.saturating_sub(new_size);
        }

        Ok(report)
    }
}

fn analyze_font_usage(document: &Document, max_content_bytes: usize) -> FontUsageAnalysis {
    let mut analysis = FontUsageAnalysis::default();
    protect_interactive_form_fonts(document, &mut analysis);
    for page_id in document.get_pages().into_values() {
        let Some(resources) = inherited_resources(document, page_id) else {
            continue;
        };
        let bytes = match document.get_page_content_with_limit(page_id, max_content_bytes) {
            Ok(bytes) => bytes,
            Err(_) => {
                analysis.skipped_content_streams += 1;
                analysis.incomplete = true;
                continue;
            }
        };
        let content = match Content::decode_strict(&bytes) {
            Ok(content) => content,
            Err(_) => {
                analysis.skipped_content_streams += 1;
                analysis.incomplete = true;
                continue;
            }
        };
        analyze_content(
            document,
            &content,
            resources,
            max_content_bytes,
            0,
            None,
            &mut HashSet::new(),
            &mut analysis,
        );
        analyze_page_appearances(document, page_id, resources, max_content_bytes, &mut analysis);
    }
    analysis
}

#[allow(clippy::too_many_arguments)]
fn analyze_content(
    document: &Document, content: &Content, resources: &Dictionary, max_content_bytes: usize, depth: usize,
    inherited_font: Option<SelectedFont>, active_streams: &mut HashSet<ObjectId>, analysis: &mut FontUsageAnalysis,
) {
    if depth >= MAX_OPTIMIZER_DEPTH {
        analysis.skipped_content_streams += 1;
        analysis.incomplete = true;
        return;
    }
    let mut current_font = inherited_font;
    let mut stack = Vec::new();
    for operation in &content.operations {
        match operation.operator.as_str() {
            "q" => stack.push(current_font),
            "Q" => {
                if let Some(saved) = stack.pop() {
                    current_font = saved;
                }
            }
            "Tf" => {
                let Some(name) = operation.operands.first().and_then(|object| object.as_name().ok()) else {
                    analysis.incomplete = true;
                    continue;
                };
                match selected_font(document, resources, name) {
                    Some(font) => current_font = Some(font),
                    None => analysis.incomplete = true,
                }
            }
            "Tj" | "'" => record_text(operation.operands.first(), current_font, analysis),
            "\"" => record_text(operation.operands.last(), current_font, analysis),
            "TJ" => {
                if let Some(values) = operation.operands.first().and_then(|object| object.as_array().ok()) {
                    for value in values {
                        record_text(Some(value), current_font, analysis);
                    }
                }
            }
            "Do" => {
                let Some(name) = operation.operands.first().and_then(|object| object.as_name().ok()) else {
                    continue;
                };
                let Some((id, stream)) = resource_stream(document, resources, b"XObject", name) else {
                    continue;
                };
                if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form") {
                    analyze_resource_stream(
                        document,
                        id,
                        stream,
                        resources,
                        max_content_bytes,
                        depth,
                        current_font,
                        active_streams,
                        analysis,
                    );
                }
            }
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
                        stream,
                        resources,
                        max_content_bytes,
                        depth,
                        current_font,
                        active_streams,
                        analysis,
                    );
                }
            }
            "gs" => analyze_soft_mask(
                document,
                resources,
                operation.operands.first(),
                max_content_bytes,
                depth,
                current_font,
                active_streams,
                analysis,
            ),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_resource_stream(
    document: &Document, id: Option<ObjectId>, stream: &Stream, parent_resources: &Dictionary,
    max_content_bytes: usize, depth: usize, inherited_font: Option<SelectedFont>,
    active_streams: &mut HashSet<ObjectId>, analysis: &mut FontUsageAnalysis,
) {
    if id.is_some_and(|id| !active_streams.insert(id)) {
        analysis.skipped_content_streams += 1;
        analysis.incomplete = true;
        return;
    }
    let resources = dictionary_value(document, &stream.dict, b"Resources")
        .and_then(|value| value.as_dict().ok())
        .unwrap_or(parent_resources);
    match stream
        .decompressed_content_with_limit(max_content_bytes)
        .and_then(|bytes| Content::decode_strict(&bytes))
    {
        Ok(content) => analyze_content(
            document,
            &content,
            resources,
            max_content_bytes,
            depth + 1,
            inherited_font,
            active_streams,
            analysis,
        ),
        Err(_) => {
            analysis.skipped_content_streams += 1;
            analysis.incomplete = true;
        }
    }
    if let Some(id) = id {
        active_streams.remove(&id);
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_soft_mask(
    document: &Document, resources: &Dictionary, operand: Option<&Object>, max_content_bytes: usize, depth: usize,
    current_font: Option<SelectedFont>, active_streams: &mut HashSet<ObjectId>, analysis: &mut FontUsageAnalysis,
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
    let Some(mask) = dictionary_value(document, ext_g_state, b"SMask").and_then(|value| value.as_dict().ok()) else {
        return;
    };
    let Some((id, stream)) = object_stream(document, mask.get(b"G").ok()) else {
        return;
    };
    analyze_resource_stream(
        document,
        id,
        stream,
        resources,
        max_content_bytes,
        depth,
        current_font,
        active_streams,
        analysis,
    );
}

fn analyze_page_appearances(
    document: &Document, page_id: ObjectId, page_resources: &Dictionary, max_content_bytes: usize,
    analysis: &mut FontUsageAnalysis,
) {
    let Ok(page) = document.get_dictionary(page_id) else {
        return;
    };
    let Some(annotations) = dictionary_value(document, page, b"Annots").and_then(|value| value.as_array().ok()) else {
        return;
    };
    let mut visited = HashSet::new();
    for annotation in annotations {
        let Some(annotation) = dereferenced_dictionary(document, annotation) else {
            continue;
        };
        let Ok(appearance) = annotation.get(b"AP") else {
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
    visited: &mut HashSet<ObjectId>, analysis: &mut FontUsageAnalysis,
) {
    if depth >= MAX_OPTIMIZER_DEPTH {
        analysis.incomplete = true;
        return;
    }
    let Ok((id, value)) = document.dereference(value) else {
        analysis.incomplete = true;
        return;
    };
    match value {
        Object::Stream(stream) => {
            let mut active = HashSet::new();
            analyze_resource_stream(
                document,
                id,
                stream,
                parent_resources,
                max_content_bytes,
                depth,
                None,
                &mut active,
                analysis,
            );
        }
        Object::Dictionary(dictionary) => {
            if id.is_some_and(|id| !visited.insert(id)) {
                return;
            }
            for (_, child) in dictionary.iter() {
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

fn record_text(value: Option<&Object>, current_font: Option<SelectedFont>, analysis: &mut FontUsageAnalysis) {
    let Some(Object::String(bytes, _)) = value else {
        return;
    };
    let Some(SelectedFont::Composite(font_id)) = current_font else {
        return;
    };
    if bytes.len() % 2 != 0 {
        analysis.invalid_fonts.insert(font_id);
        return;
    }
    let glyphs = analysis.glyphs.entry(font_id).or_default();
    for code in bytes.chunks_exact(2) {
        glyphs.insert(u16::from_be_bytes([code[0], code[1]]));
    }
}

fn selected_font(document: &Document, resources: &Dictionary, name: &[u8]) -> Option<SelectedFont> {
    let fonts = dictionary_value(document, resources, b"Font")?.as_dict().ok()?;
    let value = fonts.get(name).ok()?;
    let (id, object) = document.dereference(value).ok()?;
    let dictionary = object.as_dict().ok()?;
    if dictionary.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type0") {
        id.map(SelectedFont::Composite)
    } else {
        Some(SelectedFont::Other)
    }
}

fn protect_interactive_form_fonts(document: &Document, analysis: &mut FontUsageAnalysis) {
    let Some(root) = document
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|value| dereferenced_dictionary(document, value))
    else {
        return;
    };
    let Some(form) = dictionary_value(document, root, b"AcroForm").and_then(|value| value.as_dict().ok()) else {
        return;
    };
    let Some(resources) = dictionary_value(document, form, b"DR").and_then(|value| value.as_dict().ok()) else {
        return;
    };
    let Some(fonts) = dictionary_value(document, resources, b"Font").and_then(|value| value.as_dict().ok()) else {
        return;
    };
    for (_, value) in fonts.iter() {
        if let Ok((Some(id), object)) = document.dereference(value)
            && object
                .as_dict()
                .ok()
                .is_some_and(|dictionary| dictionary.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type0"))
        {
            analysis.interactive_fonts.insert(id);
        }
    }
}

fn subset_tag(font_file_id: ObjectId, glyphs: &BTreeSet<u16>) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(font_file_id.0.to_be_bytes());
    hasher.update(font_file_id.1.to_be_bytes());
    for glyph in glyphs {
        hasher.update(glyph.to_be_bytes());
    }
    let digest = hasher.finalize();
    digest[..6].iter().map(|byte| b'A' + byte % 26).collect()
}

fn subset_name(original: &[u8], tag: &[u8]) -> Vec<u8> {
    let base = if original.len() > 7 && original[6] == b'+' && original[..6].iter().all(u8::is_ascii_uppercase) {
        &original[7..]
    } else {
        original
    };
    let mut name = Vec::with_capacity(tag.len() + 1 + base.len());
    name.extend_from_slice(tag);
    name.push(b'+');
    name.extend_from_slice(base);
    name
}

fn font_candidate(document: &Document, font_id: ObjectId) -> std::result::Result<FontCandidate, FontSkipReason> {
    let font = document
        .get_dictionary(font_id)
        .map_err(|_| FontSkipReason::UnsupportedFont)?;
    match font.get(b"Encoding").and_then(Object::as_name).ok() {
        Some(b"Identity-H" | b"Identity-V") => {}
        _ => return Err(FontSkipReason::UnsupportedEncoding),
    }
    let descendants = font
        .get(b"DescendantFonts")
        .and_then(Object::as_array)
        .map_err(|_| FontSkipReason::UnsupportedFont)?;
    if descendants.len() != 1 {
        return Err(FontSkipReason::UnsupportedFont);
    }
    let descendant_id = descendants[0]
        .as_reference()
        .map_err(|_| FontSkipReason::UnsupportedFont)?;
    let descendant = document
        .get_dictionary(descendant_id)
        .map_err(|_| FontSkipReason::UnsupportedFont)?;
    let descriptor_id = descendant
        .get(b"FontDescriptor")
        .and_then(Object::as_reference)
        .map_err(|_| FontSkipReason::MissingFontProgram)?;
    let descriptor = document
        .get_dictionary(descriptor_id)
        .map_err(|_| FontSkipReason::MissingFontProgram)?;
    let (font_file_key, kind) = match descendant.get(b"Subtype").and_then(Object::as_name).ok() {
        Some(b"CIDFontType2") => {
            if let Ok(mapping) = descendant.get(b"CIDToGIDMap")
                && mapping.as_name().ok() != Some(b"Identity")
            {
                return Err(FontSkipReason::UnsupportedCidMapping);
            }
            (b"FontFile2".as_slice(), FontProgramKind::TrueType)
        }
        Some(b"CIDFontType0") => (b"FontFile3".as_slice(), FontProgramKind::Cff),
        _ => return Err(FontSkipReason::UnsupportedFont),
    };
    let font_file_id = descriptor
        .get(font_file_key)
        .and_then(Object::as_reference)
        .map_err(|_| FontSkipReason::MissingFontProgram)?;
    if kind == FontProgramKind::Cff
        && document
            .get_object(font_file_id)
            .ok()
            .and_then(|object| object.as_stream().ok())
            .and_then(|stream| stream.dict.get(b"Subtype").ok())
            .and_then(|subtype| subtype.as_name().ok())
            != Some(b"CIDFontType0C")
    {
        return Err(FontSkipReason::UnsupportedFont);
    }
    Ok(FontCandidate {
        font_id,
        descendant_id,
        descriptor_id,
        font_file_id,
        kind,
    })
}
fn font_file_id_for_type0(document: &Document, font_id: ObjectId) -> Option<ObjectId> {
    font_candidate(document, font_id)
        .ok()
        .map(|candidate| candidate.font_file_id)
}

fn font_file_descriptors(document: &Document) -> BTreeMap<ObjectId, HashSet<ObjectId>> {
    let mut result: BTreeMap<ObjectId, HashSet<ObjectId>> = BTreeMap::new();
    for (&id, object) in &document.objects {
        let Some(dictionary) = object_dictionary(object) else {
            continue;
        };
        for key in [b"FontFile2".as_slice(), b"FontFile3".as_slice()] {
            if let Ok(font_file_id) = dictionary.get(key).and_then(Object::as_reference) {
                result.entry(font_file_id).or_default().insert(id);
            }
        }
    }
    result
}

fn normalize_sfnt_table_directory(font_data: &[u8]) -> Option<Vec<u8>> {
    let signature = font_data.get(..4)?;
    if signature != b"\0\x01\0\0" && signature != b"true" && signature != b"OTTO" {
        return None;
    }

    let table_count = u16::from_be_bytes(font_data.get(4..6)?.try_into().ok()?) as usize;
    let directory_len = table_count.checked_mul(16)?;
    let directory_end = 12usize.checked_add(directory_len)?;
    let directory = font_data.get(12..directory_end)?;
    if directory.chunks_exact(16).map(|record| &record[..4]).is_sorted() {
        return None;
    }

    let mut records: Vec<[u8; 16]> = directory
        .chunks_exact(16)
        .map(|record| {
            let mut copy = [0; 16];
            copy.copy_from_slice(record);
            copy
        })
        .collect();
    records.sort_unstable_by(|left, right| left[..4].cmp(&right[..4]));

    let mut normalized = font_data.to_vec();
    for (target, record) in normalized[12..directory_end].chunks_exact_mut(16).zip(records) {
        target.copy_from_slice(&record);
    }
    Some(normalized)
}

// CIDFontType0 has no CIDToGIDMap. Keep a contiguous GID prefix so the
// subsetter's GID-to-CID identity mapping preserves every PDF character code.
// Sparse high GIDs are left untouched instead of retaining a huge prefix.
const MAX_IDENTITY_CFF_GLYPHS: u16 = 8192;

fn identity_cff_glyphs(font_data: &[u8], used_glyphs: &BTreeSet<u16>) -> Option<BTreeSet<u16>> {
    if !cff_is_name_keyed(font_data)? {
        return None;
    }
    let max_glyph = used_glyphs.last().copied().unwrap_or(0);
    if max_glyph >= MAX_IDENTITY_CFF_GLYPHS {
        return None;
    }
    Some((0..=max_glyph).collect())
}

fn subset_cff_program(font_data: &[u8], remapper: &GlyphRemapper) -> Option<Vec<u8>> {
    let font_length = u32::try_from(font_data.len()).ok()?;
    let mut sfnt = Vec::with_capacity(28usize.checked_add(font_data.len())?.checked_add(3)?);
    sfnt.extend_from_slice(b"OTTO");
    sfnt.extend_from_slice(&1u16.to_be_bytes());
    sfnt.extend_from_slice(&16u16.to_be_bytes());
    sfnt.extend_from_slice(&0u16.to_be_bytes());
    sfnt.extend_from_slice(&0u16.to_be_bytes());
    sfnt.extend_from_slice(b"CFF ");
    sfnt.extend_from_slice(&0u32.to_be_bytes());
    sfnt.extend_from_slice(&28u32.to_be_bytes());
    sfnt.extend_from_slice(&font_length.to_be_bytes());
    sfnt.extend_from_slice(font_data);
    while sfnt.len() % 4 != 0 {
        sfnt.push(0);
    }

    let subset = subset(&sfnt, 0, remapper).ok()?;
    sfnt_table(&subset, b"CFF ").map(ToOwned::to_owned)
}

fn sfnt_table<'a>(font_data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let table_count = u16::from_be_bytes(font_data.get(4..6)?.try_into().ok()?) as usize;
    let directory_end = 12usize.checked_add(table_count.checked_mul(16)?)?;
    let directory = font_data.get(12..directory_end)?;
    for record in directory.chunks_exact(16) {
        if record.get(..4)? != tag {
            continue;
        }
        let offset = u32::from_be_bytes(record.get(8..12)?.try_into().ok()?) as usize;
        let length = u32::from_be_bytes(record.get(12..16)?.try_into().ok()?) as usize;
        return font_data.get(offset..offset.checked_add(length)?);
    }
    None
}

fn cff_is_name_keyed(font_data: &[u8]) -> Option<bool> {
    let header_size = *font_data.get(2)? as usize;
    let (_, after_names) = cff_index_first(font_data, header_size)?;
    let (top_dict, _) = cff_index_first(font_data, after_names)?;
    let top_dict = top_dict?;
    let mut offset = 0;
    while offset < top_dict.len() {
        let byte = top_dict[offset];
        match byte {
            12 => {
                if *top_dict.get(offset + 1)? == 30 {
                    return Some(false);
                }
                offset += 2;
            }
            0..=21 => offset += 1,
            28 => offset = offset.checked_add(3)?,
            29 | 255 => offset = offset.checked_add(5)?,
            30 => {
                offset += 1;
                loop {
                    let pair = *top_dict.get(offset)?;
                    offset += 1;
                    if pair >> 4 == 0x0f || pair & 0x0f == 0x0f {
                        break;
                    }
                }
            }
            32..=246 => offset += 1,
            247..=254 => offset = offset.checked_add(2)?,
            _ => return None,
        }
    }
    Some(true)
}

fn cff_index_first(font_data: &[u8], offset: usize) -> Option<(Option<&[u8]>, usize)> {
    let count = u16::from_be_bytes(font_data.get(offset..offset + 2)?.try_into().ok()?) as usize;
    if count == 0 {
        return Some((None, offset.checked_add(2)?));
    }
    let off_size = *font_data.get(offset + 2)? as usize;
    if !(1..=4).contains(&off_size) {
        return None;
    }
    let offsets_start = offset.checked_add(3)?;
    let offsets_len = count.checked_add(1)?.checked_mul(off_size)?;
    let data_start = offsets_start.checked_add(offsets_len)?;
    let first = cff_offset(font_data.get(offsets_start..offsets_start + off_size)?)?;
    let second_start = offsets_start.checked_add(off_size)?;
    let second = cff_offset(font_data.get(second_start..second_start + off_size)?)?;
    let last_start = offsets_start.checked_add(count.checked_mul(off_size)?)?;
    let last = cff_offset(font_data.get(last_start..last_start + off_size)?)?;
    if first == 0 || second < first || last < second {
        return None;
    }
    let object_start = data_start.checked_add(first - 1)?;
    let object_end = data_start.checked_add(second - 1)?;
    let index_end = data_start.checked_add(last - 1)?;
    Some((Some(font_data.get(object_start..object_end)?), index_end))
}

fn cff_offset(bytes: &[u8]) -> Option<usize> {
    bytes.iter().try_fold(0usize, |value, byte| {
        value.checked_mul(256)?.checked_add(*byte as usize)
    })
}

fn cid_to_gid_stream(glyphs: &BTreeSet<u16>, remapper: &GlyphRemapper) -> Stream {
    let max_cid = glyphs.last().copied().unwrap_or(0) as usize;
    let mut bytes = vec![0; (max_cid + 1) * 2];
    for &cid in glyphs {
        let mapped = remapper.get(cid).unwrap_or(0).to_be_bytes();
        let offset = cid as usize * 2;
        bytes[offset..offset + 2].copy_from_slice(&mapped);
    }
    let mut stream = Stream::new(Dictionary::new(), bytes);
    let _ = stream.compress();
    stream
}

fn cid_set_stream(glyphs: &BTreeSet<u16>) -> Stream {
    let max_cid = glyphs.last().copied().unwrap_or(0) as usize;
    let mut bytes = vec![0; max_cid / 8 + 1];
    for &cid in glyphs {
        let cid = cid as usize;
        bytes[cid / 8] |= 0x80 >> (cid % 8);
    }
    let mut stream = Stream::new(Dictionary::new(), bytes);
    let _ = stream.compress();
    stream
}

fn inherited_resources(document: &Document, mut id: ObjectId) -> Option<&Dictionary> {
    let mut seen = HashSet::new();
    for _ in 0..MAX_OPTIMIZER_DEPTH {
        if !seen.insert(id) {
            return None;
        }
        let dictionary = document.get_dictionary(id).ok()?;
        if let Some(resources) = dictionary_value(document, dictionary, b"Resources") {
            return resources.as_dict().ok();
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

fn resource_object<'a>(
    document: &'a Document, resources: &'a Dictionary, category: &[u8], name: &[u8],
) -> Option<(Option<ObjectId>, &'a Object)> {
    let category = dictionary_value(document, resources, category)?.as_dict().ok()?;
    document.dereference(category.get(name).ok()?).ok()
}

fn resource_stream<'a>(
    document: &'a Document, resources: &'a Dictionary, category: &[u8], name: &[u8],
) -> Option<(Option<ObjectId>, &'a Stream)> {
    let (id, object) = resource_object(document, resources, category, name)?;
    Some((id, object.as_stream().ok()?))
}

fn object_stream<'a>(document: &'a Document, value: Option<&'a Object>) -> Option<(Option<ObjectId>, &'a Stream)> {
    let (id, object) = document.dereference(value?).ok()?;
    Some((id, object.as_stream().ok()?))
}

fn dereferenced_dictionary<'a>(document: &'a Document, value: &'a Object) -> Option<&'a Dictionary> {
    document.dereference(value).ok()?.1.as_dict().ok()
}

fn object_dictionary(object: &Object) -> Option<&Dictionary> {
    match object {
        Object::Dictionary(dictionary) => Some(dictionary),
        Object::Stream(stream) => Some(&stream.dict),
        _ => None,
    }
}
