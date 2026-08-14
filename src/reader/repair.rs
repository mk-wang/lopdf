use std::collections::HashSet;

use log::warn;

use crate::error::XrefError;
use crate::parser;
use crate::xref::{Xref, XrefEntry, XrefType};
use crate::{Dictionary, Error, Object, ObjectId, Result};

use super::Reader;

impl Reader<'_> {
    /// Rebuild a usable xref from validated indirect-object headers.
    ///
    /// This path deliberately remains separate from normal xref parsing. It is
    /// entered only for lenient loading after the normal startxref/xref path
    /// failed or after a normal xref points at the wrong object.
    pub(super) fn recover_xref_and_trailer(&mut self) -> Result<(Xref, Dictionary)> {
        let mut provisional = Xref::new(1, XrefType::CrossReferenceTable);
        provisional.insert(0, XrefEntry::Free);

        let mut candidates = Vec::new();
        for (obj_position, keyword) in self.buffer.windows(3).enumerate() {
            if keyword != b"obj" {
                continue;
            }
            let Some(((number, generation), offset)) = Self::object_header_before(self.buffer, obj_position) else {
                continue;
            };
            let Ok(offset_u32) = u32::try_from(offset) else {
                continue;
            };
            candidates.push((offset, (number, generation), offset_u32));
            provisional.insert(
                number,
                XrefEntry::Normal {
                    offset: offset_u32,
                    generation,
                },
            );
        }
        if provisional.entries.len() == 1 {
            return Err(Error::Xref(XrefError::Start));
        }

        // Parsing candidates in byte order and advancing by the parser's
        // consumed length skips the complete body of a valid stream.  A raw
        // `windows("obj")` scan alone would mistake a complete fake object
        // embedded in an uncompressed stream for a real indirect object.
        self.document.reference_table = provisional.clone();
        let mut recovered = Xref::new(1, XrefType::CrossReferenceTable);
        recovered.insert(0, XrefEntry::Free);
        let mut catalogs = Vec::new();
        candidates.sort_unstable_by_key(|(offset, _, _)| *offset);
        let mut scan_end = 0;
        for (offset, id, offset_u32) in candidates {
            if offset < scan_end {
                continue;
            }
            let Ok((parsed_id, object, object_end)) =
                parser::indirect_object_with_remainder(self.buffer, offset, Some(id), self, &mut HashSet::new())
            else {
                continue;
            };
            if parsed_id != id || object_end <= offset || !Self::ends_with_endobj(self.buffer, offset, object_end) {
                continue;
            }
            recovered.insert(
                id.0,
                XrefEntry::Normal {
                    offset: offset_u32,
                    generation: id.1,
                },
            );
            scan_end = object_end;
            if object
                .as_dict()
                .ok()
                .is_some_and(|dictionary| dictionary.get_type().ok() == Some(b"Catalog"))
            {
                catalogs.push((offset, id));
            }
        }

        let Some((_, latest_catalog)) = catalogs.iter().max_by_key(|(offset, _)| *offset).copied() else {
            return Err(Error::Xref(XrefError::Start));
        };

        self.document.reference_table = recovered.clone();
        let mut trailer = self.recover_trailer_dictionary(scan_end).unwrap_or_default();
        let root_is_catalog = trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .ok()
            .is_some_and(|root| catalogs.iter().any(|(_, catalog)| *catalog == root));
        if !root_is_catalog {
            trailer.set("Root", latest_catalog);
        }
        trailer.remove(b"Prev");
        trailer.remove(b"XRefStm");
        trailer.set("Size", recovered.size as i64);
        warn!(
            "Cross-reference table is missing or inconsistent; recovered {} indirect objects.",
            recovered.entries.len().saturating_sub(1)
        );
        Ok((recovered, trailer))
    }

    fn ends_with_endobj(buffer: &[u8], start: usize, end: usize) -> bool {
        let Some(bytes) = buffer.get(start..end) else {
            return false;
        };
        let bytes = bytes.trim_ascii_end();
        bytes.ends_with(b"endobj")
    }

    fn recover_trailer_dictionary(&self, minimum_position: usize) -> Option<Dictionary> {
        let mut end = self.buffer.len();
        while let Some(position) = Self::search_substring(&self.buffer[..end], b"trailer", 0) {
            if position >= minimum_position
                && (position == 0 || Self::is_pdf_delimiter(self.buffer[position - 1]))
                && self
                    .buffer
                    .get(position + b"trailer".len())
                    .is_none_or(|byte| Self::is_pdf_delimiter(*byte))
                && let Some(dictionary) = parser::direct_object(&self.buffer[position + b"trailer".len()..])
                    .and_then(|object| object.as_dict().ok().cloned())
            {
                return Some(dictionary);
            }
            end = position;
        }
        None
    }

    fn object_header_before(buffer: &[u8], obj_position: usize) -> Option<(ObjectId, usize)> {
        if buffer.get(obj_position..obj_position + 3)? != b"obj" {
            return None;
        }
        if buffer
            .get(obj_position + 3)
            .is_some_and(|byte| !Self::is_pdf_delimiter(*byte))
        {
            return None;
        }
        let (generation, generation_start) = Self::number_before(buffer, obj_position)?;
        let (number, number_start) = Self::number_before(buffer, generation_start)?;
        if number_start > 0 && !Self::is_pdf_delimiter(buffer[number_start - 1]) {
            return None;
        }
        let number = u32::try_from(number).ok()?;
        let generation = u16::try_from(generation).ok()?;
        (number != 0).then_some(((number, generation), number_start))
    }

    fn number_before(buffer: &[u8], end: usize) -> Option<(u64, usize)> {
        let mut cursor = end;
        while cursor > 0 && buffer[cursor - 1].is_ascii_whitespace() {
            cursor -= 1;
        }
        if cursor == end {
            return None;
        }
        let digits_end = cursor;
        while cursor > 0 && buffer[cursor - 1].is_ascii_digit() {
            cursor -= 1;
        }
        if cursor == digits_end {
            return None;
        }
        let number = std::str::from_utf8(&buffer[cursor..digits_end]).ok()?.parse().ok()?;
        Some((number, cursor))
    }
}
