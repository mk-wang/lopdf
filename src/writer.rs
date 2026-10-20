use std::fs::File;
use std::io::{BufWriter, Result, Write};
use std::path::Path;

use super::Object::*;
use super::{Dictionary, Document, Object, Stream, StringFormat};
use crate::encryption;
use crate::{IncrementalDocument, xref::*};

impl Document {
    /// Save PDF document to specified file path.
    #[inline]
    pub fn save<P: AsRef<Path>>(&mut self, path: P) -> Result<File> {
        self.validate_for_save()?;
        let mut file = BufWriter::new(File::create(path)?);
        self.save_internal(&mut file)?;
        Ok(file.into_inner()?)
    }

    /// Save PDF to arbitrary target
    #[inline]
    pub fn save_to<W: Write>(&mut self, target: &mut W) -> Result<()> {
        self.validate_for_save()?;
        self.save_internal(target)
    }

    /// Save PDF with custom options
    ///
    /// Object streams are skipped for an encrypted document, which is written with every
    /// object serialized individually instead. See [`Document::save_modern`].
    pub fn save_with_options<W: Write>(&mut self, target: &mut W, options: crate::SaveOptions) -> Result<()> {
        let use_xref_streams = options.use_xref_streams
            || matches!(
                self.reference_table.cross_reference_type,
                XrefType::CrossReferenceStream
            );
        let use_object_streams = options.use_object_streams && !self.is_encrypted();
        self.validate_for_save_with_reservation(use_object_streams, use_xref_streams, &options.object_stream_config)?;
        // Cross-reference streams are independent of object streams: a document can use one
        // without the other. Select the requested type here so the choice applies whichever
        // path writes the body below. Both features arrived in PDF 1.5, so a document that is
        // about to use one is moved up to that version.
        if options.use_xref_streams {
            self.reference_table.cross_reference_type = XrefType::CrossReferenceStream;

            if self.version.as_str() < "1.5" {
                self.version = "1.5".to_string();
            }
        }

        if use_object_streams {
            self.save_with_object_streams(target, options)
        } else {
            self.save_internal(target)
        }
    }

    fn validate_for_save(&self) -> Result<()> {
        let use_xref_streams = matches!(
            self.reference_table.cross_reference_type,
            XrefType::CrossReferenceStream
        );
        self.validate_for_save_with_reservation(false, use_xref_streams, &crate::ObjectStreamConfig::default())
    }

    fn validate_for_save_with_reservation(
        &self, use_object_streams: bool, use_xref_streams: bool, object_stream_config: &crate::ObjectStreamConfig,
    ) -> Result<()> {
        Writer::validate_binary_mark(&self.binary_mark)?;
        for object in self.objects.values() {
            Writer::validate_object(object)?;
        }

        let object_stream_count = if use_object_streams {
            self.count_object_streams(object_stream_config)?
        } else {
            0
        };
        let reserved = object_stream_count
            .checked_add(u32::from(use_xref_streams))
            .ok_or_else(|| std::io::Error::other(crate::Error::ObjectIdOverflow))?;
        self.max_id
            .checked_add(reserved)
            .and_then(|id| id.checked_add(u32::from(use_xref_streams)))
            .ok_or_else(|| std::io::Error::other(crate::Error::ObjectIdOverflow))?;
        Ok(())
    }

    fn count_object_streams(&self, config: &crate::ObjectStreamConfig) -> Result<u32> {
        use crate::ObjectStream;

        let mut stream_count = 0_u32;
        let mut objects_in_stream = 0_usize;
        for (&(id, generation), object) in &self.objects {
            if let Object::Stream(stream) = object
                && stream.dict.get(b"Type").and_then(Object::as_name).ok() == Some(b"ObjStm")
            {
                continue;
            }
            if generation != 0 || !ObjectStream::can_be_compressed((id, generation), object, self) {
                continue;
            }
            if objects_in_stream == 0 || objects_in_stream >= config.max_objects_per_stream {
                stream_count = stream_count
                    .checked_add(1)
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "too many object streams"))?;
                objects_in_stream = 0;
            }
            objects_in_stream = objects_in_stream.saturating_add(1);
        }
        Ok(stream_count)
    }

    /// Save PDF with modern features (object streams and cross-reference streams)
    ///
    /// An encrypted document is written without object streams. The objects are already
    /// encrypted by the time they reach the writer and the file encryption key is gone,
    /// so an object stream built here could only be written in the clear, contradicting
    /// the document's `/Encrypt` dictionary. The requested cross-reference type is still
    /// used; cross-reference streams are never encrypted.
    pub fn save_modern<W: Write>(&mut self, target: &mut W) -> Result<()> {
        let options = crate::SaveOptions {
            use_object_streams: true,
            use_xref_streams: true,
            ..Default::default()
        };
        self.save_with_options(target, options)
    }

    /// Objects whose own bytes are never rewritten in a plain save: an object
    /// stream or cross-reference stream is regenerated from `self.objects`, and
    /// the linearization dictionary is dropped, so copying them forward would
    /// contradict the new file layout.
    fn is_regenerated_on_save(object: &Object) -> bool {
        matches!(object.type_name(), Ok(name) if name == b"ObjStm" || name == b"XRef" || name == b"Linearized")
    }

    fn save_internal<W: Write>(&mut self, target: &mut W) -> Result<()> {
        let mut target = CountingWrite {
            inner: target,
            bytes_written: 0,
        };

        let mut xref = Xref::new(self.max_id.saturating_add(1), self.reference_table.cross_reference_type);
        writeln!(target, "%PDF-{}", self.version)?;

        Writer::write_binary_mark(&mut target, &self.binary_mark)?;

        for (&(id, generation), object) in &self.objects {
            if !Self::is_regenerated_on_save(object) {
                Writer::write_indirect_object(&mut target, id, generation, object, &mut xref)?;
            }
        }

        let xref_start = target.bytes_written;
        self.write_xref_and_trailer(&mut target, &mut xref, xref_start)
    }

    /// Write the cross-reference data in the requested form, then the
    /// `startxref` pointer that closes the file.
    fn write_xref_and_trailer<W: Write>(
        &mut self, target: &mut CountingWrite<&mut W>, xref: &mut Xref, xref_start: usize,
    ) -> Result<()> {
        // Pick right cross reference stream.
        match xref.cross_reference_type {
            XrefType::CrossReferenceTable => {
                Writer::write_xref(target, xref)?;
                self.write_trailer(target)?;
            }
            XrefType::CrossReferenceStream => {
                // Cross Reference Stream instead of XRef and Trailer
                self.write_cross_reference_stream(target, xref, Writer::xref_offset(xref_start)?)?;
            }
        }
        // Write `startxref` part of trailer
        write!(target, "\nstartxref\n{xref_start}\n%%EOF")
    }

    /// Save PDF with object streams enabled
    fn save_with_object_streams<W: Write>(&mut self, target: &mut W, options: crate::SaveOptions) -> Result<()> {
        use crate::ObjectStream;
        use std::collections::HashMap;

        // Ensure PDF version is at least 1.5 (required for object streams)
        if self.version.as_str() < "1.5" {
            self.version = "1.5".to_string();
        }

        // An object stream would go out unencrypted: `Document::encrypt` already dropped the key,
        // and the strings packed inside are themselves already encrypted (an object stream is the
        // unit of encryption). Write the objects out individually instead, as `save` does.
        if self.is_encrypted() {
            return self.save_internal(target);
        }

        let mut target = CountingWrite {
            inner: target,
            bytes_written: 0,
        };

        let mut xref = Xref::new(self.max_id.saturating_add(1), self.reference_table.cross_reference_type);
        writeln!(target, "%PDF-{}", self.version)?;
        Writer::write_binary_mark(&mut target, &self.binary_mark)?;

        // Organize objects into streams
        let mut object_streams: Vec<crate::ObjectStream> = Vec::new();
        let mut objects_to_write_directly = Vec::new();
        let mut object_to_stream_map = HashMap::new();

        // Categorize objects
        for (&(id, generation), object) in &self.objects {
            // Skip existing object streams - we'll create new ones
            if let Object::Stream(stream) = object
                && let Ok(type_obj) = stream.dict.get(b"Type")
                && let Ok(type_name) = type_obj.as_name()
                && type_name == b"ObjStm"
            {
                continue; // Skip existing object streams
            }

            if generation == 0 && ObjectStream::can_be_compressed((id, generation), object, self) {
                // Object can be compressed
                // Find or create an object stream for it
                let stream_index = object_streams.len().saturating_sub(1);

                if object_streams.is_empty()
                    || object_streams[stream_index].object_count()
                        >= options.object_stream_config.max_objects_per_stream
                {
                    // Create new object stream
                    let new_stream = ObjectStream::builder()
                        .max_objects(options.object_stream_config.max_objects_per_stream)
                        .compression_level(options.object_stream_config.compression_level)
                        .build();
                    object_streams.push(new_stream);
                }

                let stream_index = object_streams.len() - 1;
                object_streams[stream_index]
                    .add_object((id, generation), object.clone())
                    .ok();
                object_to_stream_map.insert((id, generation), stream_index);
            } else {
                // Object must be written directly
                objects_to_write_directly.push(((id, generation), object));
            }
        }

        // Write direct objects first
        for ((id, generation), object) in objects_to_write_directly {
            Writer::write_indirect_object(&mut target, id, generation, object, &mut xref)?;
        }

        // Write object streams

        for obj_stream in object_streams.into_iter() {
            let (stream_id, _) = self.try_new_object_id().map_err(std::io::Error::other)?;
            let stream_obj = obj_stream.to_stream_object().map_err(std::io::Error::other)?;

            // Record compressed objects in xref. The index must match the order
            // the stream actually stores its members in.
            for (index_in_stream, obj_id) in obj_stream.sorted_object_ids().iter().enumerate() {
                let index = u16::try_from(index_in_stream).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "object stream index is too large")
                })?;
                xref.insert(
                    obj_id.0,
                    XrefEntry::Compressed {
                        container: stream_id,
                        index,
                    },
                );
            }

            // Write the object stream
            Writer::write_indirect_object(&mut target, stream_id, 0, &Object::Stream(stream_obj), &mut xref)?;
        }

        let xref_start = target.bytes_written;
        self.write_xref_and_trailer(&mut target, &mut xref, xref_start)
    }

    /// Write the Cross Reference Stream.
    ///
    /// Insert an `Object` to the end of the PDF (not visible when inspecting `Document`).
    /// Note: This is different from the "Cross Reference Table".
    fn write_cross_reference_stream<W: Write>(
        &mut self, file: &mut CountingWrite<&mut W>, xref: &mut Xref, xref_start: u32,
    ) -> Result<()> {
        // Reserve the cross-reference stream object and the resulting /Size
        // value without allowing a crafted max object number to wrap.
        let new_obj_id_for_crs = self
            .max_id
            .checked_add(1)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "maximum object ID is too large"))?;
        let size = new_obj_id_for_crs
            .checked_add(1)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "maximum object ID is too large"))?;
        self.max_id = new_obj_id_for_crs;
        xref.insert(
            new_obj_id_for_crs,
            XrefEntry::Normal {
                offset: xref_start,
                generation: 0,
            },
        );
        self.trailer.set("Type", Name(b"XRef".to_vec()));
        self.trailer.set("Size", i64::from(size));

        // The builder reads `xref.entries` directly, so entries recorded after
        // the initial `/Size` was computed (object streams and this stream's own
        // object) are emitted too, exactly as `write_xref` does for the table.
        let mut builder = XrefStreamBuilder::new(xref);
        let widths = builder.calculate_optimal_widths();
        let stream = builder.build_stream_content().map_err(std::io::Error::other)?;
        self.trailer.set(
            "W",
            Array(widths.into_iter().map(|width| Integer(width as i64)).collect()),
        );
        self.trailer.set("Index", builder.build_index_array());
        self.trailer.remove(b"DecodeParms");
        self.trailer.remove(b"Filter");
        self.trailer.remove(b"Length");

        let mut cross_reference_stream = Stream::new(self.trailer.clone(), stream);
        cross_reference_stream.compress().map_err(std::io::Error::other)?;
        Writer::write_indirect_object(
            file,
            new_obj_id_for_crs,
            0,
            &Object::Stream(cross_reference_stream),
            xref,
        )?;

        Ok(())
    }

    fn write_trailer(&mut self, file: &mut dyn Write) -> Result<()> {
        self.trailer.set("Size", i64::from(self.max_id) + 1);
        file.write_all(b"trailer\n")?;
        Writer::write_dictionary(file, &self.trailer)?;
        Ok(())
    }
}

impl IncrementalDocument {
    /// Save PDF document to specified file path.
    ///
    /// The `check_incremental_save_supported` guard is invoked before
    /// `File::create` so an unsupported input (e.g. a still-encrypted
    /// previous revision) does not truncate a pre-existing file at `path`.
    #[inline]
    pub fn save<P: AsRef<Path>>(&mut self, path: P) -> Result<File> {
        self.validate_incremental_save()?;
        let mut file = BufWriter::new(File::create(path)?);
        self.save_internal(&mut file)?;
        Ok(file.into_inner()?)
    }

    /// Save PDF to arbitrary target
    #[inline]
    pub fn save_to<W: Write>(&mut self, target: &mut W) -> Result<()> {
        self.validate_incremental_save()?;
        self.save_internal(target)
    }

    /// Reject the two cases we still cannot handle: a document that arrived
    /// still-encrypted (no password was supplied), and the inconsistent case
    /// of `encryption_state` set but `encrypt_object_id` missing (which should
    /// not occur in practice — `decrypt_raw` records the id — but we guard
    /// against it defensively).
    fn check_incremental_save_supported(&self) -> Result<()> {
        let prev = self.get_prev_documents();
        if prev.is_encrypted() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "incremental update of a still-encrypted PDF is not supported: \
                 call `Document::decrypt` on the previous revision first \
                 (see https://github.com/J-F-Liu/lopdf/issues/520)",
            ));
        }
        if let Some(state) = prev.encryption_state.as_ref()
            && state.encrypt_object_id().is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "cannot incrementally save this decrypted document: \
                 the /Encrypt object id was not recorded during decryption",
            ));
        }
        Ok(())
    }

    /// Reserve the IDs needed by the appended revision before touching the
    /// output. A traditional xref table needs no synthetic object, while an
    /// xref stream allocates one additional object and therefore needs both
    /// `max_id + 1` and the resulting `/Size` (`max_id + 2`) to fit.
    fn validate_incremental_save(&self) -> Result<()> {
        self.check_incremental_save_supported()?;
        let xref_stream_extra = u32::from(matches!(
            self.get_prev_documents().reference_table.cross_reference_type,
            XrefType::CrossReferenceStream
        ));
        self.new_document
            .max_id
            .checked_add(1)
            .and_then(|id| id.checked_add(xref_stream_extra))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "maximum object ID is too large"))?;
        Ok(())
    }

    fn save_internal<W: Write>(&mut self, target: &mut W) -> Result<()> {
        self.validate_incremental_save()?;

        // Re-encrypt appended objects with the previous encryption state and restore `/Encrypt`.
        // Cloning the (small) state avoids borrow conflicts with `&mut self.new_document`.
        let encryption_state = self.get_prev_documents().encryption_state.as_ref().cloned();

        let mut target = CountingWrite {
            inner: target,
            bytes_written: 0,
        };

        // Write previous document versions.
        let prev_document_bytes = self.get_prev_documents_bytes();
        target.inner.write_all(prev_document_bytes)?;
        target.bytes_written += prev_document_bytes.len();

        // Write/Append new document version.
        let xref_size =
            self.new_document.max_id.checked_add(1).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "maximum object ID is too large")
            })?;
        let mut xref = Xref::new(
            xref_size,
            self.get_prev_documents().reference_table.cross_reference_type,
        );

        if let Some(last_byte) = prev_document_bytes.last()
            && *last_byte != b'\n'
        {
            // Add a newline if it was not already present
            writeln!(target)?;
        }

        // No file header or binary marker: an incremental update (ISO 32000-1, 7.5.6) is the
        // original file plus changed objects, a xref section and a trailer. A second "%PDF-x.y"
        // would make the appended region look like the start of another document.

        // Encrypt a clone of each object, leaving the in-memory ones as plaintext so repeated
        // saves do not double-encrypt.
        for (&(id, generation), object) in &self.new_document.objects {
            // `Self` is an `IncrementalDocument` here; the helper belongs to `Document`.
            if Document::is_regenerated_on_save(object) {
                continue;
            }
            if let Some(state) = encryption_state.as_ref() {
                let mut encrypted = object.clone();
                encryption::encrypt_object(state, (id, generation), &mut encrypted).map_err(std::io::Error::other)?;
                Writer::write_indirect_object(&mut target, id, generation, &encrypted, &mut xref)?;
            } else {
                Writer::write_indirect_object(&mut target, id, generation, object, &mut xref)?;
            }
        }

        // Swap in a trailer copy carrying the /Encrypt reference, so the writers that mutate
        // Size/W/Length see it; swap back so `new_document.trailer` stays clean for later saves.
        let saved_trailer = if let Some(state) = encryption_state.as_ref() {
            let encrypt_id = state
                .encrypt_object_id()
                .expect("encrypt_object_id presence checked by check_incremental_save_supported");
            let mut modified = self.new_document.trailer.clone();
            modified.set(b"Encrypt", Object::Reference(encrypt_id));
            Some(std::mem::replace(&mut self.new_document.trailer, modified))
        } else {
            None
        };

        let xref_start = target.bytes_written;

        // Pick right cross reference stream.
        let write_result = self
            .new_document
            .write_xref_and_trailer(&mut target, &mut xref, xref_start);

        // Restore the original in-memory trailer even if writing failed.
        if let Some(saved) = saved_trailer {
            self.new_document.trailer = saved;
        }

        write_result
    }
}

pub struct Writer;

impl Writer {
    fn need_separator(object: &Object) -> bool {
        matches!(*object, Null | Boolean(_) | Integer(_) | Real(_) | Reference(_))
    }

    fn need_end_separator(object: &Object) -> bool {
        matches!(
            *object,
            Null | Boolean(_) | Integer(_) | Real(_) | Name(_) | Reference(_) | Object::Stream(_)
        )
    }
    fn xref_offset(offset: usize) -> Result<u32> {
        u32::try_from(offset)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "cross-reference offset is too large"))
    }

    fn validate_binary_mark(binary_mark: &[u8]) -> Result<()> {
        if binary_mark.iter().all(|&byte| byte >= 128) {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid binary mark",
            ))
        }
    }

    fn validate_object(object: &Object) -> Result<()> {
        match object {
            Array(array) => {
                for object in array {
                    Self::validate_object(object)?;
                }
            }
            Object::Dictionary(dictionary) => {
                for (_, object) in dictionary.iter() {
                    Self::validate_object(object)?;
                }
            }
            Object::Stream(stream) => {
                i64::try_from(stream.content.len())
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "stream content is too large"))?;
                for (_, object) in stream.dict.iter() {
                    Self::validate_object(object)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Group the table's entries into the contiguous runs a cross-reference
    /// table or stream is built from.
    ///
    /// Walk the entries, not every number up to the highest, so that a high
    /// object number does not make saving slow. A section starts at a
    /// *present* id; starting it at a missing id would shift every subsequent
    /// entry by one. Object 0 is left out; only a table has to invent an entry
    /// for it, and [`Self::write_xref`] is where that happens.
    fn xref_sections(xref: &Xref) -> Vec<XrefSection> {
        let mut sections: Vec<XrefSection> = Vec::new();

        for (&obj_id, entry) in xref.entries.range(1..) {
            match sections.last_mut() {
                // No number is missing between the section and this entry. The sum cannot
                // overflow: it is at most `obj_id`.
                Some(section) if section.starting_id + section.entries.len() as u32 == obj_id => {
                    section.add_entry(entry.clone());
                }
                _ => {
                    let mut section = XrefSection::new(obj_id);
                    section.add_entry(entry.clone());
                    sections.push(section);
                }
            }
        }

        sections
    }

    /// Write Cross Reference Table.
    ///
    /// Note: This is different from a "Cross Reference Stream".
    fn write_xref(file: &mut dyn Write, xref: &Xref) -> Result<()> {
        writeln!(file, "xref")?;

        let mut sections = Self::xref_sections(xref);

        // A table has to list object 0 as free. It can only share a subsection
        // with the ids that directly follow it, so it joins the first section
        // when that section starts at object 1, and otherwise gets a section of
        // its own — appending it to a section that starts later would attribute
        // that section's first entry to object 1. A document with no objects at
        // all still needs the lone entry, which the `insert` covers.
        if sections.first().is_some_and(|section| section.starting_id == 1) {
            sections[0].starting_id = 0;
        } else {
            sections.insert(0, XrefSection::new(0));
        }
        sections[0].entries.insert(0, XrefEntry::UnusableFree);

        // `XrefEntry::write_xref_entry` already renders a compressed object in
        // the free-slot form the spec gives it, so the entries go out as they
        // are.
        for section in &sections {
            section.write_xref_section(file)?;
        }

        Ok(())
    }

    fn write_indirect_object<W: Write>(
        file: &mut CountingWrite<&mut W>, id: u32, generation: u16, object: &Object, xref: &mut Xref,
    ) -> Result<()> {
        let offset = Writer::xref_offset(file.bytes_written)?;
        xref.insert(id, XrefEntry::Normal { offset, generation });
        write!(
            file,
            "{} {} obj\n{}",
            id,
            generation,
            if Writer::need_separator(object) { " " } else { "" }
        )?;
        Writer::write_object(file, object)?;
        writeln!(
            file,
            "{}\nendobj",
            if Writer::need_end_separator(object) { " " } else { "" }
        )?;
        Ok(())
    }

    pub fn write_object(file: &mut dyn Write, object: &Object) -> Result<()> {
        match object {
            Null => file.write_all(b"null"),
            Boolean(value) => {
                if *value {
                    file.write_all(b"true")
                } else {
                    file.write_all(b"false")
                }
            }
            Integer(value) => {
                let mut buf = itoa::Buffer::new();
                file.write_all(buf.format(*value).as_bytes())
            }
            Real(value) => write!(file, "{value}"),
            Name(name) => Writer::write_name(file, name),
            String(text, format) => Writer::write_string(file, text, format),
            Array(array) => Writer::write_array(file, array),
            Object::Dictionary(dict) => Writer::write_dictionary(file, dict),
            Object::Stream(stream) => Writer::write_stream(file, stream),
            Reference(id) => write!(file, "{} {} R", id.0, id.1),
        }
    }
    pub(crate) fn write_object_minified(file: &mut dyn Write, object: &Object) -> Result<()> {
        match object {
            String(text, _) => {
                let mut unmatched_closes = 0usize;
                let mut open_parentheses = 0usize;
                let mut escaped_bytes = 0usize;
                for byte in text {
                    match byte {
                        b'(' => open_parentheses += 1,
                        b')' if open_parentheses > 0 => open_parentheses -= 1,
                        b')' => unmatched_closes += 1,
                        b'\\' | b'\r' => escaped_bytes += 1,
                        _ => {}
                    }
                }
                let literal_len = text.len() + open_parentheses + unmatched_closes + escaped_bytes + 2;
                let hexadecimal_len = text.len().saturating_mul(2).saturating_add(2);
                let format = if literal_len <= hexadecimal_len {
                    StringFormat::Literal
                } else {
                    StringFormat::Hexadecimal
                };
                Writer::write_string(file, text, &format)
            }
            Array(array) => Writer::write_array_minified(file, array),
            Object::Dictionary(dictionary) => {
                file.write_all(b"<<")?;
                for (key, value) in dictionary {
                    Writer::write_name(file, key)?;
                    if Writer::need_separator(value) {
                        file.write_all(b" ")?;
                    }
                    Writer::write_object_minified(file, value)?;
                }
                file.write_all(b">>")
            }
            _ => Writer::write_object(file, object),
        }
    }

    fn write_array_minified(file: &mut dyn Write, array: &[Object]) -> Result<()> {
        file.write_all(b"[")?;
        let mut first = true;
        for object in array {
            if first {
                first = false;
            } else if Writer::need_separator(object) {
                file.write_all(b" ")?;
            }
            Writer::write_object_minified(file, object)?;
        }
        file.write_all(b"]")
    }

    pub(crate) fn write_name(file: &mut dyn Write, name: &[u8]) -> Result<()> {
        file.write_all(b"/")?;
        for &byte in name {
            // white-space and delimiter chars are encoded to # sequences
            // also encode bytes outside of the range 33 (!) to 126 (~)
            if b" \t\n\r\x0C()<>[]{}/%#".contains(&byte) || !(33..=126).contains(&byte) {
                write!(file, "#{byte:02X}")?;
            } else {
                file.write_all(&[byte])?;
            }
        }
        Ok(())
    }

    fn write_string(file: &mut dyn Write, text: &[u8], format: &StringFormat) -> Result<()> {
        match *format {
            // In a literal string escape each backslash and unbalanced parenthesis, byte by
            // byte, and escape CR/LF since an unescaped end-of-line marker reads as \n.
            StringFormat::Literal => {
                let mut escape_indice = Vec::new();
                let mut parentheses = Vec::new();
                for (index, &byte) in text.iter().enumerate() {
                    match byte {
                        b'(' => parentheses.push(index),
                        b')' => {
                            if !parentheses.is_empty() {
                                parentheses.pop();
                            } else {
                                escape_indice.push(index);
                            }
                        }
                        b'\\' | b'\r' => escape_indice.push(index),
                        _ => continue,
                    }
                }
                escape_indice.append(&mut parentheses);

                file.write_all(b"(")?;
                if !escape_indice.is_empty() {
                    for (index, &byte) in text.iter().enumerate() {
                        if escape_indice.contains(&index) {
                            file.write_all(b"\\")?;
                            file.write_all(&[if byte == b'\r' { b'r' } else { byte }])?;
                        } else {
                            file.write_all(&[byte])?;
                        }
                    }
                } else {
                    file.write_all(text)?;
                }
                file.write_all(b")")?;
            }
            StringFormat::Hexadecimal => {
                file.write_all(b"<")?;
                for &byte in text {
                    write!(file, "{byte:02X}")?;
                }
                file.write_all(b">")?;
            }
        }
        Ok(())
    }

    fn write_array(file: &mut dyn Write, array: &[Object]) -> Result<()> {
        file.write_all(b"[")?;
        let mut first = true;
        for object in array {
            if first {
                first = false;
            } else if Writer::need_separator(object) {
                file.write_all(b" ")?;
            }
            Writer::write_object(file, object)?;
        }
        file.write_all(b"]")?;
        Ok(())
    }

    fn write_dictionary(file: &mut dyn Write, dictionary: &Dictionary) -> Result<()> {
        file.write_all(b"<<")?;
        for (key, value) in dictionary {
            Writer::write_name(file, key)?;
            if Writer::need_separator(value) {
                file.write_all(b" ")?;
            }
            Writer::write_object(file, value)?;
        }
        file.write_all(b">>")?;
        Ok(())
    }

    fn write_stream(file: &mut dyn Write, stream: &Stream) -> Result<()> {
        let length = i64::try_from(stream.content.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "stream content is too large"))?;
        let mut dictionary = stream.dict.clone();
        dictionary.set("Length", length);
        Writer::write_dictionary(file, &dictionary)?;
        file.write_all(b"stream\n")?;
        file.write_all(&stream.content)?;
        file.write_all(b"\nendstream")?;
        Ok(())
    }

    /// Write Binary mark as follows: %{binary_mark[4]}\n -> %Çì¢ or Hex(%25 c3 87 c3 ac)
    ///
    fn write_binary_mark(file: &mut dyn Write, binary_mark: &[u8]) -> Result<()> {
        Writer::validate_binary_mark(binary_mark)?;
        file.write_all(b"%")?;
        file.write_all(binary_mark)?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

pub struct CountingWrite<W: Write> {
    inner: W,
    bytes_written: usize,
}

impl<W: Write> Write for CountingWrite<W> {
    #[inline]
    fn write(&mut self, buffer: &[u8]) -> Result<usize> {
        let result = self.inner.write(buffer);
        if let Ok(bytes) = result {
            self.bytes_written += bytes;
        }
        result
    }

    #[inline]
    fn write_all(&mut self, buffer: &[u8]) -> Result<()> {
        self.bytes_written += buffer.len();
        // If this returns `Err` we can’t know how many bytes were actually written (if any)
        // but that doesn’t matter since we’re gonna abort the entire PDF generation anyway.
        self.inner.write_all(buffer)
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}

#[test]
fn save_document() {
    let mut doc = Document::with_version("1.5");
    doc.objects.insert((1, 0), Null);
    doc.objects.insert((2, 0), Boolean(true));
    doc.objects.insert((3, 0), Integer(3));
    doc.objects.insert((4, 0), Real(0.5));
    doc.objects
        .insert((5, 0), String("text((\r)".as_bytes().to_vec(), StringFormat::Literal));
    doc.objects.insert(
        (6, 0),
        String("text((\r)".as_bytes().to_vec(), StringFormat::Hexadecimal),
    );
    doc.objects.insert((7, 0), Name(b"name \t".to_vec()));
    doc.objects.insert((8, 0), Reference((1, 0)));
    doc.objects
        .insert((9, 2), Array(vec![Integer(1), Integer(2), Integer(3)]));
    doc.objects
        .insert((11, 0), Stream(Stream::new(Dictionary::new(), vec![0x41, 0x42, 0x43])));
    let mut dict = Dictionary::new();
    dict.set("A", Null);
    dict.set("B", false);
    dict.set("C", Name(b"name".to_vec()));
    doc.objects.insert((12, 0), Object::Dictionary(dict));
    doc.max_id = 12;

    // Create temporary folder to store file.
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("test_0_save.pdf");
    doc.save(&file_path).unwrap();
    // Check if file was created.
    assert!(file_path.exists());
    // Check if path is file
    assert!(file_path.is_file());
    // Check if the file is above 400 bytes (should be about 610 bytes)
    assert!(file_path.metadata().unwrap().len() > 400);
}
