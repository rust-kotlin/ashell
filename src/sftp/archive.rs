use std::{
    fs,
    io::{self, Read},
    path::Path,
};

use anyhow::{Context, Result, anyhow};

use super::transfer::TransferStateFlag;

fn check_cancelled(flag: &TransferStateFlag) -> io::Result<()> {
    if flag.is_cancelled() {
        Err(io::Error::other("transfer cancelled"))
    } else {
        Ok(())
    }
}

/// Blocking archive readers must observe cancellation even after their async
/// JoinHandle has been dropped by the network worker.
struct CancelReader<R> {
    inner: R,
    flag: TransferStateFlag,
}

impl<R: Read> Read for CancelReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        check_cancelled(&self.flag)?;
        self.inner.read(buffer)
    }
}

impl<R: io::Seek> io::Seek for CancelReader<R> {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        check_cancelled(&self.flag)?;
        self.inner.seek(position)
    }
}

pub(super) async fn extract(
    path: &Path,
    destination: &Path,
    flag: TransferStateFlag,
) -> Result<()> {
    let path = path.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::task::spawn_blocking(move || extract_blocking(&path, &destination, &flag))
        .await
        .context("extract archive task failed")?
}

fn extract_blocking(path: &Path, destination: &Path, flag: &TransferStateFlag) -> Result<()> {
    check_cancelled(flag)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid archive filename")?;
    let file = fs::File::open(path).context("open downloaded archive")?;
    check_cancelled(flag)?;
    fs::create_dir_all(destination).context("create archive destination")?;
    if name.ends_with(".zip") {
        let reader = CancelReader {
            inner: file,
            flag: flag.clone(),
        };
        let mut archive = zip::ZipArchive::new(reader).context("read ZIP archive")?;
        for index in 0..archive.len() {
            check_cancelled(flag)?;
            let mut entry = archive.by_index(index)?;
            let relative = entry
                .enclosed_name()
                .context("ZIP entry escapes destination")?
                .to_path_buf();
            let output = destination.join(relative);
            if entry.is_dir() {
                check_cancelled(flag)?;
                fs::create_dir_all(output)?;
                continue;
            }
            let parent = output.parent().context("ZIP entry has no parent")?;
            check_cancelled(flag)?;
            fs::create_dir_all(parent)?;
            check_cancelled(flag)?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            copy_cancelled(&mut entry, temporary.as_file_mut(), flag)?;
            check_cancelled(flag)?;
            temporary.persist(&output).map_err(|error| error.error)?;
        }
    } else {
        let reader: Box<dyn Read> = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Box::new(flate2::read::GzDecoder::new(file))
        } else if name.ends_with(".tar") {
            Box::new(file)
        } else {
            return Err(anyhow!("unsupported archive format"));
        };
        let reader = CancelReader {
            inner: reader,
            flag: flag.clone(),
        };
        let mut archive = tar::Archive::new(reader);
        for entry in archive.entries()? {
            check_cancelled(flag)?;
            if !entry?.unpack_in(destination)? {
                return Err(anyhow!("TAR entry escapes destination"));
            }
        }
    }
    check_cancelled(flag)?;
    Ok(())
}

fn copy_cancelled(
    reader: &mut impl Read,
    writer: &mut impl io::Write,
    flag: &TransferStateFlag,
) -> io::Result<()> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancelled(flag)?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        check_cancelled(flag)?;
        writer.write_all(&buffer[..read])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_between_read_and_write_preserves_destination() {
        struct CancelsOnRead(TransferStateFlag);
        impl Read for CancelsOnRead {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                bytes[0] = b'x';
                self.0.cancel();
                Ok(1)
            }
        }
        let flag = TransferStateFlag::new();
        let mut output = Vec::new();
        assert!(copy_cancelled(&mut CancelsOnRead(flag.clone()), &mut output, &flag).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn an_active_tar_reader_stops_after_cancellation() {
        let flag = TransferStateFlag::new();
        let mut reader = CancelReader {
            inner: io::Cursor::new(b"archive"),
            flag: flag.clone(),
        };
        let mut bytes = [0; 1];
        assert_eq!(reader.read(&mut bytes).unwrap(), 1);
        flag.cancel();
        assert!(reader.read(&mut bytes).is_err());
    }
}
