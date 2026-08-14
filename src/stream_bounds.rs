#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamBoundsError {
    NegativeLength,
    LengthOverflow,
    OutOfBounds,
}

/// Return the exclusive end of a declared stream range after validating every
/// conversion and addition against the available source bytes.
#[inline]
pub(crate) fn checked_stream_end(
    start: usize, length: i64, source_len: usize,
) -> std::result::Result<usize, StreamBoundsError> {
    let length = usize::try_from(length).map_err(|_| StreamBoundsError::NegativeLength)?;
    let end = start.checked_add(length).ok_or(StreamBoundsError::LengthOverflow)?;
    (end <= source_len).then_some(end).ok_or(StreamBoundsError::OutOfBounds)
}

/// Validate the `endstream` boundary after exactly `content_len` bytes.
///
/// A stream terminator may follow directly or after one PDF line ending. The
/// returned offset is immediately after `endstream`.
#[inline]
pub(crate) fn declared_endstream(input: &[u8], content_len: usize) -> Option<usize> {
    let mut position = content_len;
    let rest = input.get(position..)?;
    if rest.starts_with(b"\r\n") {
        position += 2;
    } else if rest.first().is_some_and(|byte| matches!(*byte, b'\r' | b'\n')) {
        position += 1;
    }
    input
        .get(position..position + b"endstream".len())
        .filter(|keyword| *keyword == b"endstream")?;
    let end = position + b"endstream".len();
    input
        .get(end)
        .is_none_or(|byte| byte.is_ascii_whitespace() || b"()<>[]{}/%".contains(byte))
        .then_some(end)
}

/// Locate the first line-delimited `endstream` marker for lenient recovery.
/// The line ending before the marker is structural and excluded from the
/// recovered content length.
pub(crate) fn recovered_stream_end(input: &[u8]) -> Option<(usize, usize)> {
    input
        .windows(b"endstream".len())
        .enumerate()
        .find_map(|(position, keyword)| {
            if keyword != b"endstream" || position == 0 {
                return None;
            }
            let content_end = if input.get(position.saturating_sub(2)..position) == Some(b"\r\n") {
                position - 2
            } else if input
                .get(position - 1)
                .is_some_and(|byte| matches!(*byte, b'\r' | b'\n'))
            {
                position - 1
            } else {
                return None;
            };
            let end = position + b"endstream".len();
            input
                .get(end)
                .is_none_or(|byte| byte.is_ascii_whitespace() || b"()<>[]{}/%".contains(byte))
                .then_some((content_end, end))
        })
}

#[cfg(test)]
mod tests {
    use super::{StreamBoundsError, checked_stream_end, declared_endstream, recovered_stream_end};

    #[test]
    fn checks_declared_stream_ranges_without_overflow() {
        assert_eq!(checked_stream_end(4, 3, 7), Ok(7));
        assert_eq!(checked_stream_end(4, -1, 7), Err(StreamBoundsError::NegativeLength));
        assert_eq!(
            checked_stream_end(usize::MAX, 1, usize::MAX),
            Err(StreamBoundsError::LengthOverflow)
        );
        assert_eq!(checked_stream_end(4, 4, 7), Err(StreamBoundsError::OutOfBounds));
    }

    #[test]
    fn recognizes_exact_and_recovered_endstream_boundaries() {
        assert_eq!(declared_endstream(b"abc\nendstream", 3), Some(13));
        assert_eq!(declared_endstream(b"abcendstream", 3), Some(12));
        assert_eq!(declared_endstream(b"abc\nnotstream", 3), None);
        assert_eq!(recovered_stream_end(b"abc\r\nendstream\n"), Some((3, 14)));
        assert_eq!(recovered_stream_end(b"abc endstream"), None);
    }
}
