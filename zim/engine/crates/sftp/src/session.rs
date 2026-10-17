//! The connection layer: russh's server handler bridging onto the
//! SFTP subsystem, and the subsystem handler bridging onto the VFS.
//!
//! Two traits live here:
//!
//! - [`russh::server::Handler`] (`ZimSshSession`): one per TCP
//!   connection — password auth, channel bookkeeping, and the
//!   subsystem dispatch that spins up the SFTP handler.
//! - [`russh_sftp::server::Handler`] (`SftpSession`): one per SFTP
//!   channel — the protocol operations mapped onto the VFS and the
//!   files crate.
//!
//! All blocking work (Argon2id verification, persistence reads,
//! filesystem operations) rides `spawn_blocking`; the SFTP data path
//! moves the real `std::fs::File` handle in and out of the blocking
//! pool per read/write call (`OpenFile::file` is an `Option` —
//! `take`n for the hop, `put` back after — the SFTP protocol is
//! sequential per session, so the two states never interleave).

use std::collections::HashMap;
use std::sync::Arc;

use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version,
};
use tokio::sync::Mutex;

use zim_engine_domain::{AuditActor, AuditRecord, PermissionGrant};
use zim_engine_files::Filesystem;

use crate::auth::{authenticate, AuthenticatedUser, InvalidLogin};
use crate::vfs::{Vfs, VfsPath};
use crate::SftpShared;

// ── The SSH connection layer ─────────────────────────────────────

/// The per-connection SSH state: the shared engine handles plus the
/// authentication result the SFTP subsystem inherits.
pub struct ZimSshSession {
    shared: Arc<SftpShared>,
    /// Set by a successful `auth_password`; the SFTP subsystem is
    /// built from it.
    auth: Option<AuthenticatedUser>,
    /// Open channels awaiting a subsystem request (the russh example
    /// pattern: `channel_open_session` stores, `subsystem_request`
    /// takes).
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    /// The peer's address, for the audit trail.
    peer: Option<std::net::SocketAddr>,
}

/// The server factory russh calls per connection.
pub struct ZimSshServer {
    /// Engine-wide shared state.
    pub shared: Arc<SftpShared>,
}

impl russh::server::Server for ZimSshServer {
    type Handler = ZimSshSession;

    fn new_client(&mut self, peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
        ZimSshSession {
            shared: Arc::clone(&self.shared),
            auth: None,
            channels: Arc::new(Mutex::new(HashMap::new())),
            peer: peer_addr,
        }
    }
}

impl russh::server::Handler for ZimSshSession {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let shared = Arc::clone(&self.shared);
        let login = user.to_owned();
        let secret = password.to_owned();
        let decision = tokio::task::spawn_blocking(move || authenticate(&shared, &login, &secret))
            .await
            .map_err(|e| {
                eprintln!("zim_engine_sftp: auth task failed: {e}");
                russh::Error::IO(std::io::Error::other("auth task failed"))
            })?;

        match decision {
            Ok(auth) => {
                audit_login(
                    &self.shared,
                    &auth.login_name,
                    Some(auth.user.id),
                    self.peer,
                    true,
                );
                self.auth = Some(auth);
                Ok(Auth::Accept)
            }
            Err(InvalidLogin) => {
                // The throttle already counted the failure; audit the
                // refusal with the presented name (unknown names stay
                // actor-less — no account enumeration in the trail).
                audit_login(
                    &self.shared,
                    &user.trim().to_lowercase(),
                    None,
                    self.peer,
                    false,
                );
                Ok(Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                })
            }
        }
    }

    /// Publickey auth is not offered: refuse before signature
    /// verification (the config advertises password only; a client
    /// that insists anyway gets its answer here).
    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        _public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: None,
            partial_success: false,
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        {
            let mut channels = self.channels.lock().await;
            channels.insert(channel.id(), channel);
        }
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel_id)?;
            return Ok(());
        }
        let Some(auth) = self.auth.clone() else {
            // Un-authenticated channel requests cannot happen (russh
            // enforces auth before channels); refuse honestly.
            session.channel_failure(channel_id)?;
            return Ok(());
        };
        // Build the per-user VFS off the async workers.
        let shared = Arc::clone(&self.shared);
        let data_dir = shared.data_dir.clone();
        let persistence = Arc::clone(&shared.persistence);
        let user = auth.user.clone();
        let vfs = match tokio::task::spawn_blocking(move || {
            Vfs::build(&data_dir, persistence.as_ref(), &user)
        })
        .await
        {
            Ok(Ok(vfs)) => vfs,
            Ok(Err(e)) => {
                eprintln!("zim_engine_sftp: cannot build the session root: {e}");
                session.channel_failure(channel_id)?;
                return Ok(());
            }
            Err(e) => {
                eprintln!("zim_engine_sftp: vfs task failed: {e}");
                session.channel_failure(channel_id)?;
                return Ok(());
            }
        };

        let channel = {
            let mut channels = self.channels.lock().await;
            channels.remove(&channel_id)
        };
        let Some(channel) = channel else {
            session.channel_failure(channel_id)?;
            return Ok(());
        };
        session.channel_success(channel_id)?;
        let sftp = SftpSession::new(Arc::clone(&self.shared), auth, vfs);
        // russh-sftp spawns its per-channel processing loop and
        // returns; the session lives until the channel closes.
        russh_sftp::server::run(channel.into_stream(), sftp).await;
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }
}

/// Append an `auth.login` audit row for an SFTP login attempt
/// (best-effort, the HTTP route's discipline: a lost audit row never
/// fails the login itself).
fn audit_login(
    shared: &SftpShared,
    login_name: &str,
    user_id: Option<zim_engine_domain::UserId>,
    peer: Option<std::net::SocketAddr>,
    success: bool,
) {
    let record = AuditRecord {
        id: 0,
        timestamp: chrono::Utc::now(),
        actor: AuditActor::User,
        actor_id: user_id.map(|id| id.to_string()),
        action: "auth.login".to_owned(),
        instance_id: None,
        metadata: serde_json::json!({
            "transport": "sftp",
            "login_name": login_name,
            "success": success,
        }),
        ip_address: peer.map(|p| p.ip()),
        user_agent: None,
    };
    // A synchronous append on the login edge is acceptable: the
    // caller is already inside spawn_blocking (auth) and SQLite's
    // busy timeout absorbs the contention.
    if let Err(e) = shared.persistence.append_audit(&record) {
        eprintln!("zim_engine_sftp: login audit append failed: {e}");
    }
}

// ── The SFTP subsystem layer ─────────────────────────────────────

/// One open file handle. `file` is `None` exactly while the real
/// handle rides the blocking pool; the protocol's per-session
/// sequential processing guarantees the window is never observed.
struct OpenFile {
    /// The VFS entry index the file lives under.
    entry: usize,
    /// The instance-root-relative path (audit + reopen).
    path: String,
    /// The real file handle.
    file: Option<std::fs::File>,
    /// Whether the handle may be read from.
    readable: bool,
    /// Whether the handle may be written to.
    writable: bool,
    /// Total bytes written through this handle (the close-time audit
    /// row's payload).
    written: u64,
}

/// One open directory handle (the full entry list was materialized at
/// opendir; readdir streams it once, then EOF).
struct OpenDir {
    /// The materialized entries.
    files: Vec<File>,
    /// Whether the single listing pass already happened.
    sent: bool,
}

/// The per-channel SFTP session.
pub struct SftpSession {
    shared: Arc<SftpShared>,
    auth: AuthenticatedUser,
    vfs: Vfs,
    open_files: HashMap<String, OpenFile>,
    open_dirs: HashMap<String, OpenDir>,
    handle_seq: u64,
}

impl SftpSession {
    /// Assemble the subsystem session (called after successful auth;
    /// the VFS is pre-built by the connection layer).
    pub fn new(shared: Arc<SftpShared>, auth: AuthenticatedUser, vfs: Vfs) -> Self {
        Self {
            shared,
            auth,
            vfs,
            open_files: HashMap::new(),
            open_dirs: HashMap::new(),
            handle_seq: 0,
        }
    }

    /// A fresh opaque handle id.
    fn next_handle(&mut self) -> String {
        self.handle_seq += 1;
        format!("zim{handle_seq:04}", handle_seq = self.handle_seq)
    }

    /// Resolve a client path, check one grant on the target entry,
    /// and hand back the entry index + its chrooted filesystem + the
    /// root-relative path (cloned — no borrows out of `self`).
    fn resolve_with(
        &self,
        path: &str,
        grant: PermissionGrant,
    ) -> Result<(usize, Filesystem, String), StatusCode> {
        match self.vfs.resolve(path) {
            Some(VfsPath::Root) => Err(StatusCode::PermissionDenied),
            Some(VfsPath::Inside { entry, relative }) => {
                let Some(entry_ref) = self.vfs.entry(entry) else {
                    return Err(StatusCode::NoSuchFile);
                };
                if !entry_ref.has(grant) {
                    return Err(StatusCode::PermissionDenied);
                }
                Ok((entry, entry_ref.fs.clone(), relative))
            }
            None => Err(StatusCode::NoSuchFile),
        }
    }

    /// The synthetic-root listing (one directory per visible
    /// instance).
    fn root_listing(&self) -> Vec<File> {
        self.vfs
            .entries()
            .iter()
            .map(|e| dir_entry_file(&e.name, true, 0, Some(e.instance.updated_at)))
            .collect()
    }

    /// Append an `instance.file.*` audit row (best-effort, with the
    /// SFTP transport marker), bridged off the async workers.
    async fn audit_file(&self, entry_index: usize, action: &str, metadata: serde_json::Value) {
        let Some(entry) = self.vfs.entry(entry_index) else {
            return;
        };
        let record = AuditRecord {
            id: 0,
            timestamp: chrono::Utc::now(),
            actor: AuditActor::User,
            actor_id: Some(self.auth.user.id.to_string()),
            action: action.to_owned(),
            instance_id: Some(entry.instance.id),
            metadata,
            ip_address: None,
            user_agent: None,
        };
        let persistence: Arc<dyn zim_engine_application::ports::Persistence> =
            Arc::clone(&self.shared.persistence);
        if let Err(e) = tokio::task::spawn_blocking(move || persistence.append_audit(&record)).await
        {
            let _ = e;
            // Best-effort by design (the HTTP route's discipline):
            // log loudly, never fail the operation.
            eprintln!("zim_engine_sftp: file audit append failed");
        }
    }

    /// stat/lstat on the synthetic root, an instance directory, or a
    /// path inside one (the same grant the listing requires).
    fn stat_path(&self, path: &str) -> Result<FileAttributes, StatusCode> {
        match self.vfs.resolve(path) {
            Some(VfsPath::Root) => Ok(root_attrs()),
            Some(VfsPath::Inside { entry, relative }) => {
                let Some(entry_ref) = self.vfs.entry(entry) else {
                    return Err(StatusCode::NoSuchFile);
                };
                if !entry_ref.has(PermissionGrant::FileRead) {
                    return Err(StatusCode::PermissionDenied);
                }
                if relative.is_empty() {
                    let mut attrs = root_attrs();
                    attrs.mtime = Some(entry_ref.instance.updated_at.timestamp().max(0) as u32);
                    return Ok(attrs);
                }
                let stat = entry_ref.fs.stat(&relative).map_err(file_status)?;
                Ok(dir_entry_attributes(&stat))
            }
            None => Err(StatusCode::NoSuchFile),
        }
    }
}

/// The synthetic-root directory attributes.
fn root_attrs() -> FileAttributes {
    let mut attrs = FileAttributes::default();
    attrs.set_dir(true);
    attrs.permissions = Some(0o40755);
    attrs
}

/// One SFTP-level directory entry shape (portable fields only).
fn dir_entry_file(
    name: &str,
    is_dir: bool,
    size: u64,
    modified: Option<chrono::DateTime<chrono::Utc>>,
) -> File {
    let mut attrs = FileAttributes::default();
    attrs.size = Some(size);
    attrs.mtime = modified.map(|t| t.timestamp().max(0) as u32);
    attrs.atime = attrs.mtime;
    if is_dir {
        attrs.set_dir(true);
        attrs.permissions = Some(0o40755);
    } else {
        attrs.set_regular(true);
        attrs.permissions = Some(0o100644);
    }
    File::new(name, attrs)
}

/// [`zim_engine_files::DirEntry`] → [`FileAttributes`].
fn dir_entry_attributes(stat: &zim_engine_files::DirEntry) -> FileAttributes {
    dir_entry_file(&stat.name, stat.is_dir, stat.size, stat.modified).attrs
}

/// Map a [`zim_engine_files::FileError`] onto a protocol status.
fn file_status(error: zim_engine_files::FileError) -> StatusCode {
    use zim_engine_files::FileError as E;
    match error {
        E::NotFound(_) => StatusCode::NoSuchFile,
        _ => StatusCode::Failure,
    }
}

/// An `Ok` status reply.
fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_owned(),
        language_tag: "en-US".to_owned(),
    }
}

impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let wants_read = pflags.contains(OpenFlags::READ);
        let wants_write = pflags.intersects(
            OpenFlags::WRITE | OpenFlags::APPEND | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        );
        // The grant follows the traffic: byte reads need
        // `file.read_content`; any write-shaped intent needs
        // `file.write` (checked once here, enforced by the open
        // flags).
        let grant = if wants_write {
            PermissionGrant::FileWrite
        } else {
            PermissionGrant::FileReadContent
        };
        let (entry_index, fs, relative) = self.resolve_with(&filename, grant)?;
        if relative.is_empty() {
            // The instance directory itself is not openable as a file.
            return Err(StatusCode::Failure);
        }

        let hop = relative.clone();
        let opened = tokio::task::spawn_blocking(move || -> Result<std::fs::File, StatusCode> {
            // §2.6 validation first: the path must resolve inside the
            // instance chroot before any handle exists.
            let safe = fs.resolve(&hop).map_err(file_status)?;
            let target = safe.canonical();
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|_| StatusCode::Failure)?;
            }
            // The OpenFlags → OpenOptions mapping russh-sftp ships
            // (CREATE+EXCLUDE → create_new included).
            let options: std::fs::OpenOptions = pflags.into();
            options.open(target).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    StatusCode::NoSuchFile
                } else {
                    StatusCode::Failure
                }
            })
        })
        .await
        .map_err(|_| StatusCode::Failure)??;

        let handle = self.next_handle();
        self.open_files.insert(
            handle.clone(),
            OpenFile {
                entry: entry_index,
                path: relative,
                file: Some(opened),
                readable: wants_read,
                writable: wants_write,
                written: 0,
            },
        );
        Ok(Handle { id, handle })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let Some(open) = self.open_files.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if !open.readable {
            return Err(StatusCode::PermissionDenied);
        }
        let mut file = open.file.take().ok_or(StatusCode::Failure)?;
        let result =
            tokio::task::spawn_blocking(move || -> (std::fs::File, Result<Vec<u8>, StatusCode>) {
                use std::io::{Read, Seek, SeekFrom};
                let outcome = (|| -> Result<Vec<u8>, StatusCode> {
                    let mut buffer = vec![0u8; len as usize];
                    file.seek(SeekFrom::Start(offset))
                        .map_err(|_| StatusCode::Failure)?;
                    let mut total = 0;
                    while total < buffer.len() {
                        match file.read(&mut buffer[total..]) {
                            Ok(0) => break,
                            Ok(n) => total += n,
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(_) => return Err(StatusCode::Failure),
                        }
                    }
                    buffer.truncate(total);
                    if total == 0 {
                        // At/past EOF the protocol expects SSH_FX_EOF.
                        return Err(StatusCode::Eof);
                    }
                    Ok(buffer)
                })();
                (file, outcome)
            })
            .await
            .map_err(|_| StatusCode::Failure);
        // Put the handle back whichever way the read went.
        match result {
            Ok((file, outcome)) => {
                if let Some(open) = self.open_files.get_mut(&handle) {
                    open.file = Some(file);
                }
                match outcome {
                    Ok(data) => Ok(Data { id, data }),
                    Err(e) => Err(e),
                }
            }
            Err(_) => Err(StatusCode::Failure),
        }
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let Some(open) = self.open_files.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if !open.writable {
            return Err(StatusCode::PermissionDenied);
        }
        let mut file = open.file.take().ok_or(StatusCode::Failure)?;
        let bytes = data.len() as u64;
        let result =
            tokio::task::spawn_blocking(move || -> (std::fs::File, Result<(), StatusCode>) {
                use std::io::{Seek, SeekFrom, Write};
                let outcome = (|| -> Result<(), StatusCode> {
                    file.seek(SeekFrom::Start(offset))
                        .map_err(|_| StatusCode::Failure)?;
                    file.write_all(&data).map_err(|_| StatusCode::Failure)?;
                    Ok(())
                })();
                (file, outcome)
            })
            .await
            .map_err(|_| StatusCode::Failure);
        match result {
            Ok((file, outcome)) => {
                if let Some(open) = self.open_files.get_mut(&handle) {
                    open.file = Some(file);
                    if outcome.is_ok() {
                        open.written = open.written.saturating_add(bytes);
                    }
                }
                outcome.map(|()| ok_status(id))
            }
            Err(_) => Err(StatusCode::Failure),
        }
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        // Directory handles close silently; file handles audit their
        // writes at close (one row per file, not per packet).
        if self.open_dirs.remove(&handle).is_some() {
            return Ok(ok_status(id));
        }
        if let Some(open) = self.open_files.remove(&handle) {
            let bytes = open.written;
            if bytes > 0 {
                let metadata = serde_json::json!({
                    "path": open.path,
                    "bytes": bytes,
                    "transport": "sftp",
                });
                self.audit_file(open.entry, "instance.file.write", metadata)
                    .await;
            }
            drop(open.file);
        }
        Ok(ok_status(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        // The synthetic root lists the visible instances (visibility
        // was settled at VFS build; no further grant).
        if self.vfs.resolve(&path) == Some(VfsPath::Root) {
            let files = self.root_listing();
            let handle = self.next_handle();
            self.open_dirs
                .insert(handle.clone(), OpenDir { files, sent: false });
            return Ok(Handle { id, handle });
        }

        let (_entry_index, fs, relative) = self.resolve_with(&path, PermissionGrant::FileRead)?;
        let hop = if relative.is_empty() {
            ".".to_owned()
        } else {
            relative
        };
        let files = tokio::task::spawn_blocking(move || -> Result<Vec<File>, StatusCode> {
            let entries = fs.list_dir(&hop).map_err(file_status)?;
            Ok(entries
                .iter()
                .map(|e| dir_entry_file(&e.name, e.is_dir, e.size, e.modified))
                .collect())
        })
        .await
        .map_err(|_| StatusCode::Failure)??;

        let handle = self.next_handle();
        self.open_dirs
            .insert(handle.clone(), OpenDir { files, sent: false });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(open) = self.open_dirs.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if open.sent {
            // The listing was delivered; the next readdir is EOF.
            return Err(StatusCode::Eof);
        }
        open.sent = true;
        let files = open.files.clone();
        Ok(Name { id, files })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let attrs = self.stat_path(&path)?;
        Ok(Attrs { id, attrs })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        // No symlink creation through this surface; lstat == stat.
        let attrs = self.stat_path(&path)?;
        Ok(Attrs { id, attrs })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let Some(open) = self.open_files.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let Some(file) = open.file.as_ref() else {
            return Err(StatusCode::Failure);
        };
        let metadata = file.metadata().map_err(|_| StatusCode::NoSuchFile)?;
        let attrs = FileAttributes::from(&metadata);
        Ok(Attrs { id, attrs })
    }

    async fn setstat(
        &mut self,
        _id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        // No chmod/chown/utimes through SFTP (AD-33).
        Err(StatusCode::OpUnsupported)
    }

    async fn fsetstat(
        &mut self,
        _id: u32,
        _handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let (entry_index, fs, relative) = self.resolve_with(&path, PermissionGrant::FileCreate)?;
        if relative.is_empty() {
            return Err(StatusCode::Failure);
        }
        let hop = relative.clone();
        tokio::task::spawn_blocking(move || fs.mkdir(&hop).map_err(file_status))
            .await
            .map_err(|_| StatusCode::Failure)??;

        self.audit_file(
            entry_index,
            "instance.file.create",
            serde_json::json!({ "path": relative, "is_dir": true, "transport": "sftp" }),
        )
        .await;
        Ok(ok_status(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let (entry_index, fs, relative) =
            self.resolve_with(&filename, PermissionGrant::FileDelete)?;
        if relative.is_empty() {
            return Err(StatusCode::Failure);
        }
        let hop = relative.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StatusCode> {
            let safe = fs.resolve(&hop).map_err(file_status)?;
            let meta =
                std::fs::symlink_metadata(safe.canonical()).map_err(|_| StatusCode::NoSuchFile)?;
            if meta.is_dir() {
                // SSH_FXP_REMOVE is file-only (rmdir is the
                // directory operation).
                return Err(StatusCode::Failure);
            }
            std::fs::remove_file(safe.canonical()).map_err(|_| StatusCode::Failure)
        })
        .await
        .map_err(|_| StatusCode::Failure)??;

        self.audit_file(
            entry_index,
            "instance.file.delete",
            serde_json::json!({ "path": relative, "kind": "file", "transport": "sftp" }),
        )
        .await;
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let (entry_index, fs, relative) = self.resolve_with(&path, PermissionGrant::FileDelete)?;
        if relative.is_empty() {
            // The instance directory itself (the synthetic entry) is
            // not deletable through its own chroot.
            return Err(StatusCode::Failure);
        }
        let hop = relative.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StatusCode> {
            let safe = fs.resolve(&hop).map_err(file_status)?;
            // remove_dir fails on non-empty (POSIX semantics the SFTP
            // protocol itself promises clients).
            std::fs::remove_dir(safe.canonical()).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    StatusCode::NoSuchFile
                } else {
                    StatusCode::Failure
                }
            })
        })
        .await
        .map_err(|_| StatusCode::Failure)??;

        self.audit_file(
            entry_index,
            "instance.file.delete",
            serde_json::json!({ "path": relative, "kind": "dir", "transport": "sftp" }),
        )
        .await;
        Ok(ok_status(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        // "." (and "") resolve to the synthetic root; anything else
        // echoes its normalized form (the VFS's own normalization is
        // the canonicalization that matters here).
        let canonical = match self.vfs.resolve(&path) {
            Some(VfsPath::Root) => "/".to_owned(),
            Some(VfsPath::Inside { entry, relative }) => {
                let Some(entry_ref) = self.vfs.entry(entry) else {
                    return Err(StatusCode::NoSuchFile);
                };
                if relative.is_empty() {
                    format!("/{}", entry_ref.name)
                } else {
                    format!("/{}/{}", entry_ref.name, relative)
                }
            }
            None => "/".to_owned(),
        };
        Ok(Name {
            id,
            files: vec![File::dummy(canonical)],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let (old_index, fs, old_rel) = self.resolve_with(&oldpath, PermissionGrant::FileWrite)?;
        let (new_index, _fs, new_rel) = self.resolve_with(&newpath, PermissionGrant::FileWrite)?;
        if old_index != new_index {
            // A move is one operation inside one chroot.
            return Err(StatusCode::Failure);
        }
        if old_rel.is_empty() || new_rel.is_empty() {
            // Renaming the instance directory itself is refused.
            return Err(StatusCode::Failure);
        }
        let (from, to) = (old_rel.clone(), new_rel.clone());
        tokio::task::spawn_blocking(move || fs.rename(&from, &to).map_err(file_status))
            .await
            .map_err(|_| StatusCode::Failure)??;

        self.audit_file(
            old_index,
            "instance.file.rename",
            serde_json::json!({ "from": old_rel, "to": new_rel, "transport": "sftp" }),
        )
        .await;
        Ok(ok_status(id))
    }

    async fn readlink(&mut self, _id: u32, _path: String) -> Result<Name, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }

    async fn symlink(
        &mut self,
        _id: u32,
        _linkpath: String,
        _targetpath: String,
    ) -> Result<Status, Self::Error> {
        // §2.6's doctrine: the engine does not mint symlinks.
        Err(StatusCode::OpUnsupported)
    }

    async fn extended(
        &mut self,
        _id: u32,
        _request: String,
        _data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }
}
