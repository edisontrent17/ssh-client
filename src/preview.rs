//! One bounded, on-demand window into a remote file. Nothing is persisted locally.
pub const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct Chunk {
    pub offset: u64,
    pub size: u64,
    pub bytes: Vec<u8>,
}

impl Chunk {
    pub fn has_next(&self) -> bool {
        self.offset.saturating_add(CHUNK_BYTES as u64) < self.size
            && self.bytes.len() >= CHUNK_BYTES
    }

    pub fn text(&self) -> Option<&str> {
        // Three look-ahead bytes finish a UTF-8 character straddling a chunk boundary.
        // Its continuation bytes are skipped at the start of the following chunk.
        let mut start = 0;
        if self.offset > 0 {
            while start < self.bytes.len().min(3) && self.bytes[start] & 0xc0 == 0x80 {
                start += 1;
            }
        }
        let mut end = self.bytes.len().min(CHUNK_BYTES);
        while end < self.bytes.len() && self.bytes[end] & 0xc0 == 0x80 {
            end += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..end]).ok()?;
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return None;
        }
        Some(if self.offset == 0 {
            text.trim_start_matches('\u{feff}')
        } else {
            text
        })
    }

    pub fn lines(&self) -> (bool, Vec<String>) {
        if let Some(text) = self.text() {
            (
                false,
                text.split('\n')
                    .map(|line| line.trim_end_matches('\r').to_owned())
                    .collect(),
            )
        } else {
            let lines = self.bytes[..self.bytes.len().min(CHUNK_BYTES)]
                .chunks(16)
                .enumerate()
                .map(|(i, bytes)| {
                    let hex = bytes
                        .iter()
                        .map(|b| format!("{b:02x} "))
                        .collect::<String>();
                    let ascii = bytes
                        .iter()
                        .map(|b| {
                            if b.is_ascii_graphic() || *b == b' ' {
                                *b as char
                            } else {
                                '.'
                            }
                        })
                        .collect::<String>();
                    format!("{:08x}  {hex:48} {ascii}", self.offset + (i * 16) as u64)
                })
                .collect();
            (true, lines)
        }
    }
}

pub struct FilePreview {
    pub id: u64,
    pub path: String,
    pub name: String,
    pub host: String,
    pub offset: u64,
    pub chunk: Option<Chunk>,
    pub lines: Vec<String>,
    pub binary: bool,
    pub error: Option<String>,
}

impl FilePreview {
    pub fn accept(&mut self, result: Result<Chunk, String>) {
        match result {
            Ok(chunk) => {
                (self.binary, self.lines) = chunk.lines();
                self.chunk = Some(chunk);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_is_not_lost_or_duplicated_across_chunk_boundaries() {
        for character in ["é", "€", "🦀"] {
            for overlap in 1..character.len() {
                let text = format!("{}{}tail", "x".repeat(CHUNK_BYTES - overlap), character);
                let bytes = text.as_bytes();
                let first = Chunk {
                    offset: 0,
                    size: bytes.len() as u64,
                    bytes: bytes[..bytes.len().min(CHUNK_BYTES + 3)].to_vec(),
                };
                let second = Chunk {
                    offset: CHUNK_BYTES as u64,
                    size: bytes.len() as u64,
                    bytes: bytes[CHUNK_BYTES..].to_vec(),
                };
                assert_eq!(
                    format!("{}{}", first.text().unwrap(), second.text().unwrap()),
                    text
                );
                assert!(first.has_next());
                assert!(!second.has_next());
            }
        }
    }

    #[test]
    fn binary_and_empty_files_have_readable_preview_states() {
        let binary = Chunk {
            offset: 65536,
            size: 65540,
            bytes: vec![0, 255, b'A', b'\n'],
        };
        let (is_binary, lines) = binary.lines();
        assert!(is_binary);
        assert!(lines[0].starts_with("00010000  00 ff 41 0a"));
        let empty = Chunk {
            offset: 0,
            size: 0,
            bytes: vec![],
        };
        assert_eq!(empty.text(), Some(""));
        assert!(!empty.has_next());
    }
}
