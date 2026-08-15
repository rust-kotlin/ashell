use std::borrow::Cow;

use encoding_rs::{
    BIG5, CoderResult, EUC_KR, Encoding, GB18030, GBK, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE,
    WINDOWS_1252,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TextEncoding {
    #[default]
    Utf8,
    Gb18030,
    Gbk,
    Big5,
    ShiftJis,
    EucKr,
    Windows1252,
    Utf16Le,
    Utf16Be,
}

pub(crate) const TERMINAL_ENCODINGS: &[TextEncoding] = &[
    TextEncoding::Utf8,
    TextEncoding::Gb18030,
    TextEncoding::Gbk,
    TextEncoding::Big5,
    TextEncoding::ShiftJis,
    TextEncoding::EucKr,
    TextEncoding::Windows1252,
];

pub(crate) const FILE_ENCODINGS: &[TextEncoding] = &[
    TextEncoding::Utf8,
    TextEncoding::Utf16Le,
    TextEncoding::Utf16Be,
    TextEncoding::Gb18030,
    TextEncoding::Gbk,
    TextEncoding::Big5,
    TextEncoding::ShiftJis,
    TextEncoding::EucKr,
    TextEncoding::Windows1252,
];

impl TextEncoding {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Gb18030 => "GB18030",
            Self::Gbk => "GBK",
            Self::Big5 => "Big5",
            Self::ShiftJis => "Shift_JIS",
            Self::EucKr => "EUC-KR",
            Self::Windows1252 => "Windows-1252",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
        }
    }

    pub(crate) fn encoding(self) -> &'static Encoding {
        match self {
            Self::Utf8 => UTF_8,
            Self::Gb18030 => GB18030,
            Self::Gbk => GBK,
            Self::Big5 => BIG5,
            Self::ShiftJis => SHIFT_JIS,
            Self::EucKr => EUC_KR,
            Self::Windows1252 => WINDOWS_1252,
            Self::Utf16Le => UTF_16LE,
            Self::Utf16Be => UTF_16BE,
        }
    }

    pub(crate) fn detect_bom(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            Some(Self::Utf8)
        } else if bytes.starts_with(&[0xFF, 0xFE]) {
            Some(Self::Utf16Le)
        } else if bytes.starts_with(&[0xFE, 0xFF]) {
            Some(Self::Utf16Be)
        } else {
            None
        }
    }

    pub(crate) fn decode_file(self, bytes: &[u8]) -> (String, bool, bool) {
        let bom_len = self.matching_bom_len(bytes);
        let (text, had_errors) = self
            .encoding()
            .decode_without_bom_handling(&bytes[bom_len..]);
        (text.into_owned(), had_errors, bom_len > 0)
    }

    pub(crate) fn encode_file(self, text: &str, with_bom: bool) -> (Vec<u8>, bool) {
        if matches!(self, Self::Utf16Le | Self::Utf16Be) {
            let mut bytes = Vec::with_capacity(text.len().saturating_mul(2).saturating_add(2));
            if with_bom {
                bytes.extend_from_slice(if self == Self::Utf16Le {
                    &[0xFF, 0xFE]
                } else {
                    &[0xFE, 0xFF]
                });
            }
            for code_unit in text.encode_utf16() {
                let encoded = if self == Self::Utf16Le {
                    code_unit.to_le_bytes()
                } else {
                    code_unit.to_be_bytes()
                };
                bytes.extend_from_slice(&encoded);
            }
            return (bytes, false);
        }

        let (encoded, _, had_errors) = self.encoding().encode(text);
        let mut bytes = Vec::with_capacity(encoded.len() + 3);
        if with_bom {
            match self {
                Self::Utf8 => bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]),
                Self::Utf16Le => bytes.extend_from_slice(&[0xFF, 0xFE]),
                Self::Utf16Be => bytes.extend_from_slice(&[0xFE, 0xFF]),
                _ => {}
            }
        }
        bytes.extend_from_slice(&encoded);
        (bytes, had_errors)
    }

    pub(crate) fn default_bom(self) -> bool {
        matches!(self, Self::Utf16Le | Self::Utf16Be)
    }

    pub(crate) fn encode_terminal_input<'a>(self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if self == Self::Utf8 {
            return Cow::Borrowed(bytes);
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Cow::Borrowed(bytes);
        };
        let (encoded, _, _) = self.encoding().encode(text);
        encoded
    }

    fn matching_bom_len(self, bytes: &[u8]) -> usize {
        match self {
            Self::Utf8 if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) => 3,
            Self::Utf16Le if bytes.starts_with(&[0xFF, 0xFE]) => 2,
            Self::Utf16Be if bytes.starts_with(&[0xFE, 0xFF]) => 2,
            _ => 0,
        }
    }
}

pub(crate) struct StreamingDecoder {
    decoder: encoding_rs::Decoder,
}

impl StreamingDecoder {
    pub(crate) fn new(encoding: TextEncoding) -> Self {
        Self {
            decoder: encoding.encoding().new_decoder_without_bom_handling(),
        }
    }

    pub(crate) fn decode(&mut self, bytes: &[u8]) -> Vec<u8> {
        let initial_capacity = self
            .decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or_else(|| bytes.len().saturating_mul(4).saturating_add(16));
        let mut output = String::with_capacity(initial_capacity);
        let mut remaining = bytes;

        loop {
            let (result, read, _) = self.decoder.decode_to_string(remaining, &mut output, false);
            remaining = &remaining[read..];
            match result {
                CoderResult::InputEmpty => break,
                CoderResult::OutputFull => {
                    let additional = self
                        .decoder
                        .max_utf8_buffer_length(remaining.len())
                        .unwrap_or_else(|| remaining.len().saturating_mul(4).saturating_add(16))
                        .max(16);
                    output.reserve(additional);
                }
            }
        }

        output.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamingDecoder, TextEncoding};

    #[test]
    fn streaming_decoder_preserves_split_multibyte_characters() {
        let (encoded, _) = TextEncoding::Gbk.encode_file("中文", false);
        let mut decoder = StreamingDecoder::new(TextEncoding::Gbk);
        let mut decoded = decoder.decode(&encoded[..1]);
        decoded.extend(decoder.decode(&encoded[1..]));

        assert_eq!(String::from_utf8(decoded).unwrap(), "中文");
    }

    #[test]
    fn file_encoding_preserves_matching_bom() {
        let original = [0xFF, 0xFE, b'A', 0x00];
        let (text, had_errors, has_bom) = TextEncoding::Utf16Le.decode_file(&original);
        let (encoded, encode_errors) = TextEncoding::Utf16Le.encode_file(&text, has_bom);

        assert!(!had_errors);
        assert!(!encode_errors);
        assert_eq!(encoded, original);
    }
}
