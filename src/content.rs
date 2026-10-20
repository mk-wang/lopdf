use super::{Object, Stream};
use crate::Result;
pub use crate::parser::ContentOperations;
use crate::writer::Writer;
use std::io::Write;

const PDF_DELIMITERS: &[u8] = b"()<>[]{}/%";

#[derive(Debug, Clone)]
pub struct Operation {
    pub operator: String,
    pub operands: Vec<Object>,
}

impl Operation {
    pub fn new(operator: &str, operands: Vec<Object>) -> Operation {
        Operation {
            operator: operator.to_string(),
            operands,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Content<Operations: AsRef<[Operation]> = Vec<Operation>> {
    pub operations: Operations,
}

impl<Operations: AsRef<[Operation]>> Content<Operations> {
    /// Encode content operations.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        let mut first_operation = true;
        for operation in self.operations.as_ref() {
            // Add new line after each operation except the last one.
            if first_operation {
                first_operation = false;
            } else {
                buffer.write_all(b"\n")?;
            }
            // A decoded inline image is a BI operation holding the image as a stream.
            if let ("BI", [Object::Stream(image)]) = (operation.operator.as_str(), operation.operands.as_slice()) {
                encode_inline_image(&mut buffer, image)?;
                continue;
            }
            for operand in &operation.operands {
                Writer::write_object(&mut buffer, operand)?;
                buffer.write_all(b" ")?;
            }
            buffer.write_all(operation.operator.as_bytes())?;
        }
        Ok(buffer)
    }

    pub(crate) fn encode_minified(&self) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        for operation in self.operations.as_ref() {
            for operand in &operation.operands {
                write_separator(&mut buffer, object_starts_with_delimiter(operand));
                Writer::write_object_minified(&mut buffer, operand)?;
            }
            write_separator(
                &mut buffer,
                operation
                    .operator
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| PDF_DELIMITERS.contains(byte)),
            );
            buffer.write_all(operation.operator.as_bytes())?;
        }
        Ok(buffer)
    }
}

fn object_starts_with_delimiter(object: &Object) -> bool {
    matches!(
        object,
        Object::Name(_) | Object::String(_, _) | Object::Array(_) | Object::Dictionary(_) | Object::Stream(_)
    )
}

fn write_separator(buffer: &mut Vec<u8>, next_starts_with_delimiter: bool) {
    if !next_starts_with_delimiter
        && buffer
            .last()
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !PDF_DELIMITERS.contains(byte))
    {
        buffer.push(b' ');
    }
}

/// Write an inline image as `BI`, its dictionary entries, `ID`, its data and `EI`.
fn encode_inline_image(buffer: &mut Vec<u8>, image: &Stream) -> Result<()> {
    buffer.write_all(b"BI")?;
    for (key, value) in &image.dict {
        // Readers find the end of the data from the image size, and `Stream::new` adds a
        // Length the source dictionary did not have.
        if key == b"Length" {
            continue;
        }
        buffer.write_all(b" ")?;
        Writer::write_name(buffer, key)?;
        buffer.write_all(b" ")?;
        Writer::write_object(buffer, value)?;
    }
    buffer.write_all(b" ID ")?;
    buffer.write_all(&image.content)?;
    buffer.write_all(b"\nEI")?;
    Ok(())
}
