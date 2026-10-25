//! Output files: the file system, or an in-memory [`Sink`] chosen per job (the web app, which
//! offers every written file as a download; tests).

use std::io::{BufWriter, Cursor, Seek, SeekFrom, Write};

use crate::{ExportError, Result, io};

/// Receives each finished output file (path as resolved, complete contents) instead of the file
/// system. Called once per file; image sequences call it from the render threads.
pub type Sink = dyn Fn(&str, Vec<u8>) + Send + Sync;

// Download/sink outputs hold the entire file in memory. Native filesystem output streams.
const MAX_MEMORY_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;

/// One output file being written.
pub(crate) enum Out<'a> {
    File(BufWriter<std::fs::File>),
    Mem { path: String, cur: Cursor<Vec<u8>>, sink: &'a Sink },
}

/// Create `path` (parent directories included) or an in-memory file for `sink`.
pub(crate) fn create<'a>(sink: Option<&'a Sink>, path: &str) -> Result<Out<'a>> {
    match sink {
        Some(sink) => Ok(Out::Mem { path: path.to_string(), cur: Cursor::new(Vec::new()), sink }),
        None => {
            if let Some(dir) = std::path::Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(io)?;
            }
            Ok(Out::File(BufWriter::new(std::fs::File::create(path).map_err(io)?)))
        }
    }
}

/// Whether `path` already exists (never for a sink: it has no memory of earlier jobs).
pub(crate) fn exists(sink: Option<&Sink>, path: &str) -> bool {
    sink.is_none() && std::path::Path::new(path).exists()
}

impl Out<'_> {
    /// Flush and close; a sink receives the contents now. Returns the file size.
    pub(crate) fn finish(self) -> Result<u64> {
        match self {
            Out::File(mut w) => {
                w.flush().map_err(io)?;
                let f = w.into_inner().map_err(|e| ExportError::Io(e.to_string()))?;
                Ok(f.metadata().map(|m| m.len()).unwrap_or(0))
            }
            Out::Mem { path, cur, sink } => {
                let data = cur.into_inner();
                let n = data.len() as u64;
                sink(&path, data);
                Ok(n)
            }
        }
    }
}

impl Write for Out<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::File(w) => w.write(buf),
            Out::Mem { cur, .. } => {
                if buf.is_empty() {
                    return Ok(0);
                }
                let len = u64::try_from(buf.len()).map_err(std::io::Error::other)?;
                let end = cur
                    .position()
                    .checked_add(len)
                    .filter(|end| *end <= MAX_MEMORY_OUTPUT_BYTES)
                    .ok_or_else(|| std::io::Error::other("in-memory exports are limited to 512 MiB; render a shorter span or use a filesystem output"))?;
                let end = usize::try_from(end).map_err(std::io::Error::other)?;
                let additional = end.saturating_sub(cur.get_ref().len());
                cur.get_mut().try_reserve_exact(additional).map_err(|e| std::io::Error::other(format!("cannot allocate in-memory export: {e}")))?;
                cur.write(buf)
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::File(w) => w.flush(),
            Out::Mem { .. } => Ok(()),
        }
    }
}

impl Seek for Out<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match self {
            Out::File(w) => w.seek(pos),
            Out::Mem { cur, .. } => cur.seek(pos),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_memory_output_returns_an_error_and_remains_usable() {
        let sink = |_: &str, _: Vec<u8>| {};
        let mut out = create(Some(&sink), "intermediate.mov").unwrap();
        // The old Cursor writer panics with capacity overflow before attempting an allocation.
        out.seek(SeekFrom::Start(isize::MAX as u64)).unwrap();
        assert_eq!(out.write(&[]).unwrap(), 0);
        assert!(out.write_all(&[1]).is_err());
        out.seek(SeekFrom::Start(MAX_MEMORY_OUTPUT_BYTES - 1)).unwrap();
        assert!(out.write_all(&[1, 2]).is_err());
        out.seek(SeekFrom::Start(0)).unwrap();
        out.write_all(&[1, 2, 3]).unwrap();
        assert_eq!(out.finish().unwrap(), 3);
    }
}
