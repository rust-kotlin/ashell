use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

use russh_sftp::{
    client::SftpSession,
    protocol::{Attrs, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode},
};

use super::{
    TransferContext, create_remote_dir_all, deletion_path, recursive_delete,
    transfer::TransferStateFlag, upload_file_impl, upload_paths_impl,
    write_remote_file_with_commit,
};

#[derive(Default)]
struct Filesystem {
    modes: BTreeMap<String, u32>,
    files: BTreeMap<String, Vec<u8>>,
    directories: BTreeMap<String, Vec<(String, u32)>>,
    read_directories: HashSet<String>,
    calls: Vec<String>,
    fail_write: bool,
    fail_close: bool,
    fail_mkdir: bool,
    create_collision: bool,
    replace_after_lstat: Option<String>,
    replace_after_readdir: Option<String>,
}

struct Server(Arc<Mutex<Filesystem>>);

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}

impl russh_sftp::server::Handler for Server {
    type Error = StatusCode;
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("lstat:{path}"));
        let permissions = if path == "/unknown" {
            None
        } else {
            Some(*state.modes.get(&path).ok_or(StatusCode::NoSuchFile)?)
        };
        if state.replace_after_lstat.as_ref() == Some(&path) {
            state.replace_after_lstat = None;
            state.modes.insert(path, 0o120777);
        }
        Ok(Attrs {
            id,
            attrs: FileAttributes {
                permissions,
                ..FileAttributes::default()
            },
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.lstat(id, path).await
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("opendir:{path}"));
        if !state.directories.contains_key(&path) {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Handle { id, handle: path })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let mut state = self.0.lock().unwrap();
        if !state.read_directories.insert(handle.clone()) {
            return Err(StatusCode::Eof);
        }
        let files = state.directories[&handle]
            .iter()
            .map(|(name, mode)| {
                File::new(
                    name.clone(),
                    FileAttributes {
                        permissions: Some(*mode),
                        ..FileAttributes::default()
                    },
                )
            })
            .collect();
        if state.replace_after_readdir.as_ref() == Some(&handle) {
            state.replace_after_readdir = None;
            state.modes.insert(handle, 0o120777);
        }
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("remove:{filename}"));
        state.files.remove(&filename);
        state.modes.remove(&filename);
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        self.0.lock().unwrap().calls.push(format!("rmdir:{path}"));
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("mkdir:{path}"));
        if state.fail_mkdir {
            return Err(StatusCode::PermissionDenied);
        }
        if state.modes.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        state.modes.insert(path, 0o040755);
        Ok(ok(id))
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("open:{filename}"));
        assert_ne!(
            filename, "/file",
            "original file must never be opened for overwrite"
        );
        if filename == "/upload" {
            assert!(flags.contains(OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE));
        } else {
            assert!(flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE));
            assert_eq!(attrs.permissions, Some(0o600));
        }
        if state.create_collision {
            state
                .files
                .insert(filename, b"owned by somebody else".to_vec());
            return Err(StatusCode::Failure);
        }
        state.files.insert(filename.clone(), Vec::new());
        state.modes.insert(filename.clone(), 0o100600);
        Ok(Handle {
            id,
            handle: filename,
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let mut state = self.0.lock().unwrap();
        if state.fail_write {
            return Err(StatusCode::Failure);
        }
        let data = state.files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        data.resize(data.len().max(start + bytes.len()), 0);
        data[start..start + bytes.len()].copy_from_slice(&bytes);
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("close:{handle}"));
        if state.fail_close {
            Err(StatusCode::Failure)
        } else {
            Ok(ok(id))
        }
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if let Some(mode) = attrs.permissions {
            self.0.lock().unwrap().modes.insert(path, mode);
        }
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, Self::Error> {
        let mut state = self.0.lock().unwrap();
        assert!(
            state.calls.contains(&format!("close:{old}")),
            "close must be acknowledged before publication"
        );
        state.calls.push(format!("rename:{old}:{new}"));
        let bytes = state.files.remove(&old).ok_or(StatusCode::NoSuchFile)?;
        state.files.insert(new, bytes);
        Ok(ok(id))
    }
}

async fn client(state: Arc<Mutex<Filesystem>>) -> SftpSession {
    let (client, server) = tokio::io::duplex(64 * 1024);
    russh_sftp::server::run(server, Server(state)).await;
    SftpSession::new(client).await.unwrap()
}

#[tokio::test]
async fn deleting_directory_links_never_opens_or_deletes_their_targets() {
    let state = Arc::new(Mutex::new(Filesystem::default()));
    {
        let mut fs = state.lock().unwrap();
        fs.modes.insert("/link".into(), 0o120777);
        fs.directories
            .insert("/link".into(), vec![("keep".into(), 0o100644)]);
        fs.files
            .insert("/link/keep".into(), b"target contents".to_vec());
        fs.modes.insert("/dir".into(), 0o040755);
        fs.modes.insert("/dir/link".into(), 0o120777);
        // READDIR may be stale; the recursive entry must check LSTAT again.
        fs.directories
            .insert("/dir".into(), vec![("link".into(), 0o040755)]);
    }
    let sftp = client(state.clone()).await;
    recursive_delete(&sftp, "/link///".into()).await.unwrap();
    recursive_delete(&sftp, "/dir".into()).await.unwrap();
    let fs = state.lock().unwrap();
    assert_eq!(fs.files["/link/keep"], b"target contents");
    assert!(!fs.calls.contains(&"opendir:/link".into()));
    assert!(!fs.calls.contains(&"opendir:/dir/link".into()));
    assert!(fs.calls.contains(&"remove:/dir/link".into()));
    assert!(fs.calls.contains(&"rmdir:/dir".into()));
}

#[tokio::test]
async fn missing_file_types_fail_before_any_delete() {
    let state = Arc::new(Mutex::new(Filesystem::default()));
    let sftp = client(state.clone()).await;
    assert!(recursive_delete(&sftp, "/unknown".into()).await.is_err());
    assert_eq!(state.lock().unwrap().calls, vec!["lstat:/unknown"]);
}

#[tokio::test]
async fn a_directory_replaced_by_a_link_during_traversal_aborts_before_removing_children() {
    for after_listing in [false, true] {
        let state = Arc::new(Mutex::new(Filesystem::default()));
        {
            let mut fs = state.lock().unwrap();
            fs.modes.insert("/dir".into(), 0o040755);
            fs.modes.insert("/dir/keep".into(), 0o100644);
            fs.files
                .insert("/dir/keep".into(), b"outside contents".to_vec());
            fs.directories
                .insert("/dir".into(), vec![("keep".into(), 0o100644)]);
            if after_listing {
                fs.replace_after_readdir = Some("/dir".into());
            } else {
                fs.replace_after_lstat = Some("/dir".into());
            }
        }
        let sftp = client(state.clone()).await;
        assert!(recursive_delete(&sftp, "/dir".into()).await.is_err());
        let fs = state.lock().unwrap();
        assert!(
            !fs.calls
                .iter()
                .any(|call| call.starts_with("remove:") || call.starts_with("rmdir:"))
        );
        assert_eq!(fs.files["/dir/keep"], b"outside contents");
    }
}

#[test]
fn ambiguous_delete_aliases_are_rejected_without_following_links() {
    for path in [
        "/",
        "///",
        "",
        "/link/.",
        "/link/..",
        "/a/../link",
        "/a\\b",
        "/nul\0name",
    ] {
        assert!(deletion_path(path).is_err(), "{path:?}");
    }
    assert_eq!(deletion_path("/目录/a b///").unwrap(), "/目录/a b");
}

fn original_file() -> Arc<Mutex<Filesystem>> {
    let mut state = Filesystem::default();
    state.modes.insert("/file".into(), 0o100640);
    state.files.insert("/file".into(), b"original".to_vec());
    Arc::new(Mutex::new(state))
}

#[tokio::test]
async fn save_errors_never_truncate_the_original_or_publish_unclosed_data() {
    for (fail_write, fail_close) in [(true, false), (false, true), (false, false)] {
        let state = original_file();
        state.lock().unwrap().fail_write = fail_write;
        state.lock().unwrap().fail_close = fail_close;
        let sftp = client(state.clone()).await;
        let result = write_remote_file_with_commit(&sftp, "/file", b"replacement", |_, _| async {
            assert!(
                !fail_write && !fail_close,
                "failed writes/closes must not reach publication"
            );
            Err(anyhow::anyhow!("replacement unsupported"))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(state.lock().unwrap().files["/file"], b"original");
    }
}

#[tokio::test]
async fn successful_save_publishes_only_a_complete_closed_file() {
    let state = original_file();
    let sftp = client(state.clone()).await;
    let sftp_ref = &sftp;
    write_remote_file_with_commit(&sftp, "/file", b"replacement", |old, new| async move {
        sftp_ref.rename(old, new).await.map_err(Into::into)
    })
    .await
    .unwrap();
    assert_eq!(state.lock().unwrap().files["/file"], b"replacement");
}

#[tokio::test]
async fn a_temporary_name_collision_does_not_delete_somebody_elses_file() {
    let state = original_file();
    state.lock().unwrap().create_collision = true;
    let sftp = client(state.clone()).await;
    assert!(
        write_remote_file_with_commit(&sftp, "/file", b"new", |_, _| async {
            panic!("a failed temporary create must not publish")
        })
        .await
        .is_err()
    );
    let fs = state.lock().unwrap();
    assert!(!fs.calls.iter().any(|call| call.starts_with("remove:")));
    assert!(
        fs.files
            .values()
            .any(|bytes| bytes == b"owned by somebody else")
    );
    assert_eq!(fs.files["/file"], b"original");
}

#[tokio::test]
async fn upload_waits_for_close_and_propagates_the_servers_final_write_error() {
    use std::io::Write as _;
    use std::sync::{atomic::AtomicU64, mpsc};

    for fail_close in [false, true] {
        let state = Arc::new(Mutex::new(Filesystem::default()));
        state.lock().unwrap().fail_close = fail_close;
        let sftp = client(state.clone()).await;
        let mut local = tempfile::NamedTempFile::new().unwrap();
        local.write_all(b"uploaded contents").unwrap();
        let flag = TransferStateFlag::new();
        let (events, _received) = mpsc::channel();
        let result = upload_file_impl(
            &sftp,
            local.path(),
            "/upload",
            TransferContext {
                flag: &flag,
                events: &events,
                id: "upload",
            },
            Arc::new(AtomicU64::new(0)),
            Some(17),
        )
        .await;
        assert_eq!(result.is_err(), fail_close);
        let fs = state.lock().unwrap();
        assert!(fs.calls.contains(&"close:/upload".into()));
        assert_eq!(fs.files["/upload"], b"uploaded contents");
    }
}

#[tokio::test]
async fn upload_reports_a_missing_source_instead_of_completing_an_empty_batch() {
    let state = Arc::new(Mutex::new(Filesystem::default()));
    let sftp = client(state.clone()).await;
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.txt");
    let (events, _receiver) = std::sync::mpsc::channel();
    let result = upload_paths_impl(
        &sftp,
        &[missing.to_string_lossy().into_owned()],
        "/",
        TransferStateFlag::new(),
        &events,
        "missing-source",
    )
    .await;
    assert!(result.is_err());
    assert!(state.lock().unwrap().files.is_empty());
}

#[tokio::test]
async fn remote_directory_creation_only_tolerates_existing_directories() {
    for (existing_mode, fail_mkdir, expected_success) in [
        (None, false, true),
        (Some(0o040755), false, true),
        (Some(0o100644), false, false),
        (None, true, false),
    ] {
        let state = Arc::new(Mutex::new(Filesystem::default()));
        {
            let mut fs = state.lock().unwrap();
            fs.fail_mkdir = fail_mkdir;
            if let Some(mode) = existing_mode {
                fs.modes.insert("/upload-dir".into(), mode);
            }
        }
        let sftp = client(state).await;
        assert_eq!(
            create_remote_dir_all(&sftp, "/upload-dir").await.is_ok(),
            expected_success
        );
    }
}
