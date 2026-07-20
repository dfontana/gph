//! The LSP transport used by `gph lsp` and `gph lsp-connect`.
//!
//! The daemon owns a Unix-domain socket per workspace. Each connection gets a
//! typed `tower-lsp-server` backend which advertises only full text
//! synchronization; diagnostics, completion, and other language-server
//! features remain the responsibility of merman-lsp.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use fs2::FileExt;
use tokio::net::{UnixListener as TokioUnixListener, UnixStream as TokioUnixStream};
use tower_lsp_server::jsonrpc::Result as LspResult;
use tower_lsp_server::ls_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions,
};
use tower_lsp_server::{LanguageServer, LspService, Server};

use crate::preview::{PreviewEvent, run_lsp_preview};

static CLIENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

type ClientStreams = Arc<Mutex<BTreeMap<u64, UnixStream>>>;

/// Run the workspace daemon and its Kitty preview UI.
pub fn run_daemon() -> Result<(), String> {
    if !crate::kitty::is_available() {
        return Err(
            "`gph lsp` requires Kitty (set KITTY_WINDOW_ID or TERM=xterm-kitty)".to_string(),
        );
    }

    let workspace = workspace_root()?;
    let socket = socket_path(&workspace)?;
    let _workspace_lock = WorkspaceLock::acquire(&socket)?;
    let mut artifacts = DaemonArtifacts::default();
    let marker = prepare_workspace_marker(&socket, &workspace)?;
    artifacts.marker = marker.created;
    let bound = bind_listener(&socket)?;
    artifacts.socket = Some(bound.artifact);
    let listener = bound.listener;
    listener.set_nonblocking(true).map_err(|error| {
        format!(
            "cannot configure LSP socket '{}': {error}",
            socket.display()
        )
    })?;
    let (updates, receiver) = std::sync::mpsc::channel();
    let running = Arc::new(AtomicBool::new(true));
    let client_streams = Arc::new(Mutex::new(BTreeMap::new()));
    let accept_running = Arc::clone(&running);
    let accept_streams = Arc::clone(&client_streams);
    let accept_updates = updates.clone();
    let accept_thread = thread::spawn(move || {
        let result = accept_loop(listener, updates, accept_running, accept_streams);
        report_accept_result(&accept_updates, &result);
        result
    });

    eprintln!(
        "gph lsp: previewing workspace '{}' on '{}'",
        workspace.display(),
        socket.display()
    );
    let result = run_lsp_preview(receiver);
    running.store(false, Ordering::Relaxed);
    close_client_streams(&client_streams);
    let accept_result = accept_thread
        .join()
        .map_err(|_| "gph LSP accept loop panicked".to_string())?;
    match (result, accept_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; shutdown failed: {cleanup}")),
    }
}

/// Bridge stdin/stdout to the daemon for a normal LSP client such as Helix.
pub fn run_connect() -> Result<(), String> {
    let workspace = workspace_root()?;
    let socket = socket_path(&workspace)?;
    validate_workspace_marker(&socket, &workspace)?;
    let stream = UnixStream::connect(&socket).map_err(|error| {
        format!(
            "cannot connect to gph LSP preview at '{}': {error}; start `gph lsp` in this workspace",
            socket.display()
        )
    })?;
    bridge_connection(stream, io::stdin(), io::stdout())
}

fn bridge_connection<R, W>(
    mut stream: UnixStream,
    mut input: R,
    mut output: W,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    let mut replies = stream
        .try_clone()
        .map_err(|error| format!("cannot read gph LSP replies: {error}"))?;
    thread::spawn(move || {
        let result = io::copy(&mut input, &mut replies).and_then(|_| replies.flush());
        let _ = replies.shutdown(std::net::Shutdown::Write);
        if let Err(error) = result {
            eprintln!("gph lsp-connect: cannot forward LSP input: {error}");
        }
    });

    io::copy(&mut stream, &mut output)
        .and_then(|_| output.flush())
        .map_err(|error| format!("cannot forward LSP replies: {error}"))
}

fn report_accept_result(updates: &Sender<PreviewEvent>, result: &Result<(), String>) {
    if let Err(error) = result {
        let _ = updates.send(PreviewEvent::Fatal(error.clone()));
    }
}

fn accept_loop(
    listener: UnixListener,
    updates: Sender<PreviewEvent>,
    running: Arc<AtomicBool>,
    streams: ClientStreams,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| format!("cannot start LSP runtime: {error}"))?;
    runtime.block_on(async move {
        let listener = TokioUnixListener::from_std(listener)
            .map_err(|error| format!("cannot configure LSP socket listener: {error}"))?;
        let mut clients = Vec::new();

        while running.load(Ordering::Relaxed) {
            match tokio::time::timeout(Duration::from_millis(10), listener.accept()).await {
                Ok(Ok((stream, _))) => {
                    let client = CLIENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                    let stream = match stream.into_std() {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!("gph lsp: cannot retain client connection: {error}");
                            continue;
                        }
                    };
                    let control = match stream.try_clone() {
                        Ok(control) => control,
                        Err(error) => {
                            eprintln!("gph lsp: cannot retain client connection: {error}");
                            continue;
                        }
                    };
                    let retained = streams.lock().map_or_else(
                        |_| {
                            eprintln!("gph lsp: client stream registry is unavailable");
                            false
                        },
                        |mut open_streams| {
                            if running.load(Ordering::Relaxed) {
                                open_streams.insert(client, control);
                                true
                            } else {
                                false
                            }
                        },
                    );
                    if !retained {
                        continue;
                    }
                    let stream = match TokioUnixStream::from_std(stream) {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!("gph lsp: cannot configure client connection: {error}");
                            remove_client_stream(&streams, client);
                            continue;
                        }
                    };
                    let updates = updates.clone();
                    let streams = Arc::clone(&streams);
                    clients.push(tokio::spawn(async move {
                        serve_client(stream, client, updates, streams).await;
                    }));
                }
                Ok(Err(error)) => eprintln!("gph lsp: accepting a client failed: {error}"),
                Err(_) => {}
            }
        }

        for client in clients {
            if let Err(error) = client.await {
                eprintln!("gph lsp: client task failed: {error}");
            }
        }
        Ok(())
    })
}

fn close_client_streams(streams: &ClientStreams) {
    if let Ok(mut open_streams) = streams.lock() {
        for (_, stream) in std::mem::take(&mut *open_streams) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn remove_client_stream(streams: &ClientStreams, client: u64) {
    if let Ok(mut open_streams) = streams.lock() {
        open_streams.remove(&client);
    }
}

async fn serve_client(
    stream: TokioUnixStream,
    client: u64,
    updates: Sender<PreviewEvent>,
    streams: ClientStreams,
) {
    let (read, write) = stream.into_split();
    let backend_updates = updates.clone();
    let (service, socket) = LspService::new(move |_| Backend::new(client, backend_updates));
    Server::new(read, write, socket)
        .concurrency_level(1)
        .serve(service)
        .await;
    remove_client_stream(&streams, client);
    let _ = updates.send(PreviewEvent::Disconnect { client });
}

#[derive(Debug)]
struct Backend {
    client: u64,
    updates: Sender<PreviewEvent>,
    documents: Mutex<BTreeMap<String, i32>>,
}

impl Backend {
    fn new(client: u64, updates: Sender<PreviewEvent>) -> Self {
        Self {
            client,
            updates,
            documents: Mutex::new(BTreeMap::new()),
        }
    }

    fn set_document(&self, uri: String, text: String, version: i32, require_open: bool) {
        let accepted = self.documents.lock().is_ok_and(|mut documents| {
            let is_newer = documents
                .get(&uri)
                .is_none_or(|previous| version > *previous);
            if is_newer && (!require_open || documents.contains_key(&uri)) {
                documents.insert(uri.clone(), version);
                true
            } else {
                false
            }
        });
        if accepted {
            let _ = self.updates.send(PreviewEvent::Set {
                client: self.client,
                uri,
                text,
                version: i64::from(version),
            });
        }
    }

    fn close_document(&self, uri: String) {
        let was_open = self
            .documents
            .lock()
            .is_ok_and(|mut documents| documents.remove(&uri).is_some());
        if was_open {
            let _ = self.updates.send(PreviewEvent::Close {
                client: self.client,
                uri,
            });
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        ..TextDocumentSyncOptions::default()
                    },
                )),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        self.set_document(
            document.uri.to_string(),
            document.text,
            document.version,
            false,
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let [change] = params.content_changes.as_slice() else {
            return;
        };
        if change.range.is_some() {
            return;
        }
        self.set_document(
            params.text_document.uri.to_string(),
            change.text.clone(),
            params.text_document.version,
            true,
        );
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.close_document(params.text_document.uri.to_string());
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    let directory =
        env::current_dir().map_err(|error| format!("cannot read current directory: {error}"))?;
    directory.canonicalize().map_err(|error| {
        format!(
            "cannot resolve workspace '{}': {error}",
            directory.display()
        )
    })
}

fn socket_path(workspace: &Path) -> Result<PathBuf, String> {
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "XDG_RUNTIME_DIR must be set for `gph lsp`".to_string())?;
    let directory = runtime.join("gph");
    match fs::create_dir(&directory) {
        Ok(()) => {
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
                format!(
                    "cannot secure LSP runtime directory '{}': {error}",
                    directory.display()
                )
            })?
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&directory).map_err(|metadata_error| {
                format!(
                    "cannot inspect LSP runtime directory '{}': {metadata_error}",
                    directory.display()
                )
            })?;
            if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(format!(
                    "refusing insecure LSP runtime directory '{}'",
                    directory.display()
                ));
            }
        }
        Err(error) => {
            return Err(format!(
                "cannot create LSP runtime directory '{}': {error}",
                directory.display()
            ));
        }
    }
    Ok(socket_path_in(&directory, workspace))
}

fn socket_path_in(directory: &Path, workspace: &Path) -> PathBuf {
    directory.join(format!("preview-{:016x}.sock", stable_path_hash(workspace)))
}

fn stable_path_hash(path: &Path) -> u64 {
    // FNV-1a is only an identifier, not a security primitive. Unlike
    // DefaultHasher it is stable across independently started processes.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

struct BoundListener {
    listener: UnixListener,
    artifact: OwnedArtifact,
}

fn bind_listener(path: &Path) -> Result<BoundListener, String> {
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => match UnixStream::connect(path) {
            Ok(_) => {
                return Err(format!(
                    "a gph LSP preview daemon is already running for this workspace ('{}')",
                    path.display()
                ));
            }
            Err(_) => {
                let metadata = fs::symlink_metadata(path).map_err(|metadata_error| {
                    format!(
                        "cannot inspect stale LSP socket '{}': {metadata_error}",
                        path.display()
                    )
                })?;
                if !metadata.file_type().is_socket() {
                    return Err(format!(
                        "refusing to remove non-socket path at '{}'; choose another workspace runtime directory",
                        path.display()
                    ));
                }
                fs::remove_file(path).map_err(|remove_error| {
                    format!(
                        "cannot remove stale LSP socket '{}': {remove_error}",
                        path.display()
                    )
                })?;
                UnixListener::bind(path).map_err(|bind_error| {
                    format!(
                        "cannot create LSP socket '{}': {bind_error}",
                        path.display()
                    )
                })?
            }
        },
        Err(error) => {
            return Err(format!(
                "cannot create LSP socket '{}': {error}",
                path.display()
            ));
        }
    };
    let artifact = OwnedArtifact::capture(path, ArtifactType::Socket)?;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        artifact.remove();
        return Err(format!(
            "cannot secure LSP socket '{}': {error}",
            path.display()
        ));
    }
    Ok(BoundListener { listener, artifact })
}

fn workspace_marker_path(socket: &Path) -> PathBuf {
    socket.with_extension("workspace")
}

struct PreparedMarker {
    created: Option<OwnedArtifact>,
}

fn prepare_workspace_marker(socket: &Path, workspace: &Path) -> Result<PreparedMarker, String> {
    let marker = workspace_marker_path(socket);
    let expected = workspace.as_os_str().as_encoded_bytes();
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(format!(
                    "refusing non-file LSP workspace marker '{}'",
                    marker.display()
                ));
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(format!(
                    "refusing insecure LSP workspace marker '{}'",
                    marker.display()
                ));
            }
            let actual = fs::read(&marker).map_err(|error| {
                format!(
                    "cannot read LSP workspace marker '{}': {error}",
                    marker.display()
                )
            })?;
            if actual != expected {
                return Err(format!(
                    "LSP socket hash collision at '{}'; use a different XDG_RUNTIME_DIR",
                    socket.display()
                ));
            }
            Ok(PreparedMarker { created: None })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map_err(|create_error| {
                    format!(
                        "cannot create LSP workspace marker '{}': {create_error}",
                        marker.display()
                    )
                })?;
            let artifact = OwnedArtifact::from_metadata(
                &marker,
                file.metadata().map_err(|error| {
                    format!(
                        "cannot inspect LSP workspace marker '{}': {error}",
                        marker.display()
                    )
                })?,
                ArtifactType::Regular,
            )?;
            let result = file
                .write_all(expected)
                .and_then(|()| file.flush())
                .and_then(|()| fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)));
            if let Err(error) = result {
                artifact.remove();
                return Err(format!(
                    "cannot create LSP workspace marker '{}': {error}",
                    marker.display()
                ));
            }
            Ok(PreparedMarker {
                created: Some(artifact),
            })
        }
        Err(error) => Err(format!(
            "cannot inspect LSP workspace marker '{}': {error}",
            marker.display()
        )),
    }
}

fn validate_workspace_marker(socket: &Path, workspace: &Path) -> Result<(), String> {
    let marker = workspace_marker_path(socket);
    let metadata = fs::symlink_metadata(&marker).map_err(|error| {
        format!(
            "cannot find gph LSP preview at '{}'; start `gph lsp` in this workspace ({error})",
            socket.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "refusing non-file LSP workspace marker '{}'",
            marker.display()
        ));
    }
    let actual = fs::read(&marker).map_err(|error| {
        format!(
            "cannot read LSP workspace marker '{}': {error}",
            marker.display()
        )
    })?;
    if actual == workspace.as_os_str().as_encoded_bytes() {
        Ok(())
    } else {
        Err(format!(
            "LSP socket hash collision at '{}'; use a different XDG_RUNTIME_DIR",
            socket.display()
        ))
    }
}

#[derive(Clone, Copy)]
enum ArtifactType {
    Regular,
    Socket,
}

struct OwnedArtifact {
    path: PathBuf,
    device: u64,
    inode: u64,
    kind: ArtifactType,
}

impl OwnedArtifact {
    fn capture(path: &Path, kind: ArtifactType) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            format!("cannot inspect LSP artifact '{}': {error}", path.display())
        })?;
        Self::from_metadata(path, metadata, kind)
    }

    fn from_metadata(
        path: &Path,
        metadata: fs::Metadata,
        kind: ArtifactType,
    ) -> Result<Self, String> {
        let matches_kind = match kind {
            ArtifactType::Regular => metadata.file_type().is_file(),
            ArtifactType::Socket => metadata.file_type().is_socket(),
        };
        if !matches_kind {
            return Err(format!(
                "unexpected LSP artifact type at '{}'",
                path.display()
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            kind,
        })
    }

    fn remove(&self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        let matches_kind = match self.kind {
            ArtifactType::Regular => metadata.file_type().is_file(),
            ArtifactType::Socket => metadata.file_type().is_socket(),
        };
        if matches_kind && metadata.dev() == self.device && metadata.ino() == self.inode {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Default)]
struct DaemonArtifacts {
    socket: Option<OwnedArtifact>,
    marker: Option<OwnedArtifact>,
}

impl Drop for DaemonArtifacts {
    fn drop(&mut self) {
        if let Some(socket) = &self.socket {
            socket.remove();
        }
        if let Some(marker) = &self.marker {
            marker.remove();
        }
    }
}

struct WorkspaceLock {
    file: fs::File,
}

impl WorkspaceLock {
    fn acquire(socket: &Path) -> Result<Self, String> {
        let path = socket.with_extension("lock");
        let file = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(format!("refusing non-file LSP lock '{}'", path.display()));
                }
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(format!("refusing insecure LSP lock '{}'", path.display()));
                }
                OpenOptions::new().read(true).write(true).open(&path)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(file) => {
                        let artifact = OwnedArtifact::from_metadata(
                            &path,
                            file.metadata().map_err(|error| {
                                format!("cannot inspect LSP lock '{}': {error}", path.display())
                            })?,
                            ArtifactType::Regular,
                        )?;
                        if let Err(error) =
                            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                        {
                            artifact.remove();
                            return Err(format!(
                                "cannot secure LSP lock '{}': {error}",
                                path.display()
                            ));
                        }
                        Ok(file)
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        OpenOptions::new().read(true).write(true).open(&path)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
        .map_err(|error| format!("cannot open LSP lock '{}': {error}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(format!(
                "a gph LSP preview daemon is already starting or running for this workspace ('{}')",
                socket.display()
            )),
            Err(error) => Err(format!(
                "cannot lock LSP workspace '{}': {error}",
                socket.display()
            )),
        }
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::str::FromStr;

    use tower_lsp_server::ls_types::{
        TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
        VersionedTextDocumentIdentifier,
    };

    fn uri(path: &str) -> tower_lsp_server::ls_types::Uri {
        tower_lsp_server::ls_types::Uri::from_str(path).unwrap()
    }

    fn backend(client: u64) -> (Backend, std::sync::mpsc::Receiver<PreviewEvent>) {
        let (updates, receiver) = std::sync::mpsc::channel();
        (Backend::new(client, updates), receiver)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap()
    }

    #[test]
    fn typed_backend_advertises_full_open_close_sync_and_filters_stale_changes() {
        let (backend, receiver) = backend(4);
        let result = runtime().block_on(backend.initialize(InitializeParams::default()));
        let Some(TextDocumentSyncCapability::Options(sync)) =
            result.unwrap().capabilities.text_document_sync
        else {
            panic!("backend did not advertise typed synchronization options");
        };
        assert_eq!(sync.open_close, Some(true));
        assert_eq!(sync.change, Some(TextDocumentSyncKind::FULL));

        runtime().block_on(backend.did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem::new(
                uri("file:///diagram.mmd"),
                "mermaid".into(),
                1,
                "one".into(),
            ),
        }));
        runtime().block_on(backend.did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri("file:///diagram.mmd"), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "two".into(),
            }],
        }));
        runtime().block_on(backend.did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri("file:///diagram.mmd"), 1),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "stale".into(),
            }],
        }));
        runtime().block_on(backend.did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri("file:///diagram.mmd"), 3),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(tower_lsp_server::ls_types::Range::default()),
                range_length: None,
                text: "partial".into(),
            }],
        }));
        runtime().block_on(backend.did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri("file:///diagram.mmd"), 3),
            content_changes: vec![
                TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "first partial".into(),
                },
                TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "second partial".into(),
                },
            ],
        }));
        runtime().block_on(backend.did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri("file:///diagram.mmd"), 3),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "three".into(),
            }],
        }));
        runtime().block_on(backend.did_close(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier::new(uri("file:///diagram.mmd")),
        }));

        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Set { client: 4, ref text, version: 1, .. } if text == "one"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Set { client: 4, ref text, version: 2, .. } if text == "two"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Set { client: 4, ref text, version: 3, .. } if text == "three"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Close { client: 4, ref uri } if uri == "file:///diagram.mmd"
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn tower_service_routes_a_client_session_and_disconnects_the_preview() {
        let (server, client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let (updates, receiver) = std::sync::mpsc::channel();
        let streams = Arc::new(Mutex::new(BTreeMap::new()));
        let server_stream = server.try_clone().unwrap();
        streams.lock().unwrap().insert(9, server_stream);
        let streams_for_server = Arc::clone(&streams);
        let server = thread::spawn(move || {
            runtime().block_on(async move {
                let server = TokioUnixStream::from_std(server).unwrap();
                serve_client(server, 9, updates, streams_for_server).await;
            });
        });

        let initialize =
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
        let open = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///diagram.mmd","languageId":"mermaid","version":1,"text":"one"}}}"#;
        let shutdown = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#;
        let exit = r#"{"jsonrpc":"2.0","method":"exit"}"#;
        let mut client = BufReader::new(client);
        send_message(client.get_mut(), initialize);
        let initialize_reply = read_message(&mut client);
        send_message(client.get_mut(), open);
        send_message(client.get_mut(), shutdown);
        let shutdown_reply = read_message(&mut client);
        send_message(client.get_mut(), exit);
        client
            .get_mut()
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        server.join().unwrap();
        assert!(
            initialize_reply.contains("\"textDocumentSync\":{\"openClose\":true,\"change\":1}"),
            "unexpected initialize reply: {initialize_reply}"
        );
        assert!(shutdown_reply.contains("\"id\":2"));
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Set { client: 9, ref text, version: 1, .. } if text == "one"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Disconnect { client: 9 }
        ));
        assert!(streams.lock().unwrap().is_empty());
    }

    #[test]
    fn accept_loop_failure_is_forwarded_to_the_preview() {
        let (updates, receiver) = std::sync::mpsc::channel();
        report_accept_result(&updates, &Err("cannot start LSP runtime".to_string()));
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Fatal(error) if error == "cannot start LSP runtime"
        ));
    }

    #[test]
    fn socket_is_private_to_the_current_user() {
        let path = temporary_path("private-socket");
        let bound = bind_listener(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(bound.listener);
        bound.artifact.remove();
    }

    #[test]
    fn stale_socket_cleanup_never_removes_a_regular_file() {
        let path = temporary_path("regular-file");
        fs::write(&path, "do not remove").unwrap();
        assert!(matches!(
            bind_listener(&path),
            Err(error) if error.contains("non-socket path")
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "do not remove");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn workspace_marker_detects_socket_collisions_and_is_private() {
        let socket = temporary_path("marker");
        let workspace = Path::new("/tmp/first-workspace");
        let prepared = prepare_workspace_marker(&socket, workspace).unwrap();
        let marker = workspace_marker_path(&socket);
        assert!(validate_workspace_marker(&socket, workspace).is_ok());
        assert!(matches!(
            prepare_workspace_marker(&socket, Path::new("/tmp/other-workspace")),
            Err(error) if error.contains("hash collision")
        ));
        assert_eq!(
            fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(DaemonArtifacts {
            socket: None,
            marker: prepared.created,
        });
        assert!(!marker.exists());
    }

    #[test]
    fn workspace_lock_excludes_another_daemon_until_released() {
        let socket = temporary_path("workspace-lock");
        let first = WorkspaceLock::acquire(&socket).unwrap();
        assert!(matches!(
            WorkspaceLock::acquire(&socket),
            Err(error) if error.contains("already starting or running")
        ));
        drop(first);
        drop(WorkspaceLock::acquire(&socket).unwrap());
        fs::remove_file(socket.with_extension("lock")).unwrap();
    }

    #[test]
    fn cleanup_only_removes_artifacts_created_by_this_daemon() {
        let marker = temporary_path("owned-marker");
        fs::write(&marker, "owned").unwrap();
        let artifact = OwnedArtifact::capture(&marker, ArtifactType::Regular).unwrap();
        fs::remove_file(&marker).unwrap();
        fs::write(&marker, "replacement").unwrap();
        drop(DaemonArtifacts {
            socket: None,
            marker: Some(artifact),
        });
        assert_eq!(fs::read_to_string(&marker).unwrap(), "replacement");
        fs::remove_file(marker).unwrap();
    }

    #[test]
    fn daemon_eof_ends_the_connect_bridge_without_waiting_for_stdin() {
        struct BlockingReader(std::sync::mpsc::Receiver<()>);

        impl Read for BlockingReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.0
                    .recv()
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "release missing"))?;
                Ok(0)
            }
        }

        let (server, client) = UnixStream::pair().unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let bridge = thread::spawn(move || {
            done.send(bridge_connection(
                client,
                BlockingReader(blocked),
                Vec::new(),
            ))
            .unwrap();
        });
        drop(server);
        assert!(
            finished
                .recv_timeout(Duration::from_secs(1))
                .expect("bridge did not return after daemon EOF")
                .is_ok()
        );
        release.send(()).unwrap();
        bridge.join().unwrap();
    }

    #[test]
    fn workspace_socket_name_is_stable_and_distinct() {
        let runtime = Path::new("/run/user/test/gph");
        assert_eq!(
            socket_path_in(runtime, Path::new("/tmp/workspace")),
            socket_path_in(runtime, Path::new("/tmp/workspace"))
        );
        assert_ne!(
            socket_path_in(runtime, Path::new("/tmp/one")),
            socket_path_in(runtime, Path::new("/tmp/two"))
        );
    }

    fn send_message(output: &mut UnixStream, message: &str) {
        write!(output, "Content-Length: {}\r\n\r\n{message}", message.len()).unwrap();
        output.flush().unwrap();
    }

    fn read_message(input: &mut BufReader<UnixStream>) -> String {
        let mut header = String::new();
        input.read_line(&mut header).unwrap();
        let length = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        header.clear();
        input.read_line(&mut header).unwrap();
        assert_eq!(header, "\r\n");
        let mut body = vec![0; length];
        input.read_exact(&mut body).unwrap();
        String::from_utf8(body).unwrap()
    }

    fn temporary_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gph-lsp-{name}-{}-{nanos}", std::process::id()))
    }
}
