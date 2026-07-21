//! The LSP transport used by `gph lsp` and `gph lsp-connect`.
//!
//! A single daemon per user owns one Unix-domain socket, shared across every
//! workspace. Each connection gets a typed `tower-lsp-server` backend which
//! advertises only full text synchronization; diagnostics, completion, and
//! other language-server features remain the responsibility of merman-lsp.

use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use fs2::FileExt;
use tokio::net::{UnixListener as TokioUnixListener, UnixStream as TokioUnixStream};
use tokio::task::JoinSet;
use tower_lsp_server::jsonrpc::Result as LspResult;
use tower_lsp_server::ls_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions,
};
use tower_lsp_server::{LanguageServer, LspService, Server};

use crate::preview::{PreviewEvent, run_lsp_preview};
use crate::preview_ui::combine_with_context;

static CLIENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Run the per-user preview daemon and its Kitty preview UI.
pub fn run_daemon() -> Result<(), String> {
    if !crate::kitty::is_available() {
        return Err(
            "`gph lsp` requires Kitty (set KITTY_WINDOW_ID or TERM=xterm-kitty)".to_string(),
        );
    }

    let socket = socket_path()?;
    let _daemon_lock = DaemonLock::acquire(&socket)?;
    let mut artifacts = DaemonArtifacts::default();
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
    let accept_running = Arc::clone(&running);
    let accept_updates = updates.clone();
    let accept_thread = thread::spawn(move || {
        let result = accept_loop(listener, updates, accept_running);
        report_accept_result(&accept_updates, &result);
        result
    });

    eprintln!(
        "gph lsp: preview daemon listening on '{}'",
        socket.display()
    );
    let result = run_lsp_preview(receiver);
    running.store(false, Ordering::Relaxed);
    let accept_result = accept_thread
        .join()
        .map_err(|_| "gph LSP accept loop panicked".to_string())?;
    combine_with_context(result, accept_result, "shutdown")
}

/// Bridge stdin/stdout to the daemon for a normal LSP client such as Helix.
pub fn run_connect() -> Result<(), String> {
    let socket = socket_path()?;
    let stream = UnixStream::connect(&socket).map_err(|error| {
        format!(
            "cannot connect to gph LSP preview at '{}': {error}; start `gph lsp` first",
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

    copy_flushing(&mut stream, &mut output)
        .map_err(|error| format!("cannot forward LSP replies: {error}"))
}

/// Like [`io::copy`], but flushes `output` after every chunk instead of only at EOF.
///
/// `output` is typically [`io::stdout()`], which Rust always line-buffers regardless of
/// whether the destination is a terminal, pipe, or file. LSP frame bodies are single-line
/// JSON with no trailing newline, so without an explicit flush per chunk they sit buffered
/// until the connection closes — starving any client that keeps the connection open.
fn copy_flushing<R: Read + ?Sized, W: Write + ?Sized>(
    input: &mut R,
    output: &mut W,
) -> io::Result<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        output.write_all(&buffer[..read])?;
        output.flush()?;
    }
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
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| format!("cannot start LSP runtime: {error}"))?;
    runtime.block_on(async move {
        let listener = TokioUnixListener::from_std(listener)
            .map_err(|error| format!("cannot configure LSP socket listener: {error}"))?;
        let mut clients = JoinSet::new();

        while running.load(Ordering::Relaxed) {
            reap_finished_clients(&mut clients);
            match tokio::time::timeout(Duration::from_millis(10), listener.accept()).await {
                Ok(Ok((stream, _))) if running.load(Ordering::Relaxed) => {
                    let client = CLIENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                    let updates = updates.clone();
                    clients.spawn(async move {
                        serve_client(stream, client, updates).await;
                    });
                }
                Ok(Ok(_)) => break,
                Ok(Err(error)) => eprintln!("gph lsp: accepting a client failed: {error}"),
                Err(_) => {}
            }
        }

        clients.abort_all();
        while let Some(result) = clients.join_next().await {
            report_client_task_result(result);
        }
        Ok(())
    })
}

fn reap_finished_clients(clients: &mut JoinSet<()>) {
    while let Some(result) = clients.try_join_next() {
        report_client_task_result(result);
    }
}

fn report_client_task_result(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result
        && !error.is_cancelled()
    {
        eprintln!("gph lsp: client task failed: {error}");
    }
}

async fn serve_client(stream: TokioUnixStream, client: u64, updates: Sender<PreviewEvent>) {
    let (read, write) = stream.into_split();
    let backend_updates = updates.clone();
    let (service, socket) = LspService::new(move |_| Backend::new(client, backend_updates));
    Server::new(read, write, socket)
        .concurrency_level(1)
        .serve(service)
        .await;
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

/// Resolve the per-user directory that holds the daemon's socket and lock.
///
/// Linux exposes a tmpfs runtime directory (`XDG_RUNTIME_DIR`, cleared on
/// logout), which is the ideal home for a session socket. macOS has no such
/// concept, so we fall back to the per-user cache directory
/// (`~/Library/Caches`). Either way we own a private `gph` subdirectory.
fn runtime_directory() -> Result<PathBuf, String> {
    let base = dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .ok_or_else(|| {
            "cannot determine a runtime directory for `gph lsp`; set HOME or XDG_RUNTIME_DIR"
                .to_string()
        })?;
    let directory = base.join("gph");
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
    Ok(directory)
}

fn socket_path() -> Result<PathBuf, String> {
    Ok(runtime_directory()?.join("preview.sock"))
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
                    "a gph LSP preview daemon is already running ('{}')",
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
                        "refusing to remove non-socket path at '{}'; choose another runtime directory",
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

fn validate_private_regular_file(
    path: &Path,
    description: &str,
    metadata: &fs::Metadata,
) -> Result<(), String> {
    if !metadata.file_type().is_file() {
        return Err(format!(
            "refusing non-file LSP {description} '{}'",
            path.display()
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "refusing insecure LSP {description} '{}'",
            path.display()
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ArtifactType {
    Regular,
    Socket,
}

impl ArtifactType {
    fn matches(self, metadata: &fs::Metadata) -> bool {
        match self {
            Self::Regular => metadata.file_type().is_file(),
            Self::Socket => metadata.file_type().is_socket(),
        }
    }
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
        if !kind.matches(&metadata) {
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
        if self.kind.matches(&metadata)
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Default)]
struct DaemonArtifacts {
    socket: Option<OwnedArtifact>,
}

impl Drop for DaemonArtifacts {
    fn drop(&mut self) {
        if let Some(socket) = &self.socket {
            socket.remove();
        }
    }
}

struct DaemonLock {
    file: fs::File,
}

impl DaemonLock {
    fn acquire(socket: &Path) -> Result<Self, String> {
        let path = socket.with_extension("lock");
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
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
                if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
                    artifact.remove();
                    return Err(format!(
                        "cannot secure LSP lock '{}': {error}",
                        path.display()
                    ));
                }
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    format!("cannot inspect LSP lock '{}': {error}", path.display())
                })?;
                validate_private_regular_file(&path, "lock", &metadata)?;
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(|error| {
                        format!("cannot open LSP lock '{}': {error}", path.display())
                    })?
            }
            Err(error) => {
                return Err(format!(
                    "cannot open LSP lock '{}': {error}",
                    path.display()
                ));
            }
        };
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(format!(
                "a gph LSP preview daemon is already starting or running ('{}')",
                socket.display()
            )),
            Err(error) => Err(format!(
                "cannot lock gph LSP daemon '{}': {error}",
                socket.display()
            )),
        }
    }
}

impl Drop for DaemonLock {
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
    fn tower_service_routes_a_client_session_and_reports_natural_disconnects() {
        let (server, client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let (updates, receiver) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            runtime().block_on(async move {
                let server = TokioUnixStream::from_std(server).unwrap();
                serve_client(server, 9, updates).await;
            });
        });

        let initialize =
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
        let open = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///diagram.mmd","languageId":"mermaid","version":1,"text":"one"}}}"#;
        let mut client = BufReader::new(client);
        send_message(client.get_mut(), initialize);
        let initialize_reply = read_message(&mut client);
        send_message(client.get_mut(), open);
        client
            .get_mut()
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        server.join().unwrap();
        assert!(
            initialize_reply.contains("\"textDocumentSync\":{\"openClose\":true,\"change\":1}"),
            "unexpected initialize reply: {initialize_reply}"
        );
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Set { client: 9, ref text, version: 1, .. } if text == "one"
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            PreviewEvent::Disconnect { client: 9 }
        ));
    }

    #[test]
    fn daemon_shutdown_cancels_an_idle_connected_client_promptly() {
        let path = temporary_path("idle-client");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let accept_running = Arc::clone(&running);
        let (updates, _receiver) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let accept_thread = thread::spawn(move || {
            done.send(accept_loop(listener, updates, accept_running))
                .unwrap();
        });

        let client = UnixStream::connect(&path).unwrap();
        let mut client = BufReader::new(client);
        let initialize =
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
        send_message(client.get_mut(), initialize);
        let _ = read_message(&mut client);

        running.store(false, Ordering::Relaxed);
        assert!(
            finished
                .recv_timeout(Duration::from_secs(1))
                .expect("accept loop did not stop promptly")
                .is_ok()
        );
        let mut eof = [0];
        assert_eq!(client.read(&mut eof).unwrap(), 0);
        accept_thread.join().unwrap();
        fs::remove_file(path).unwrap();
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
    fn daemon_lock_excludes_another_daemon_until_released() {
        let socket = temporary_path("daemon-lock");
        let first = DaemonLock::acquire(&socket).unwrap();
        assert!(matches!(
            DaemonLock::acquire(&socket),
            Err(error) if error.contains("already starting or running")
        ));
        drop(first);
        drop(DaemonLock::acquire(&socket).unwrap());
        let path = socket.with_extension("lock");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            DaemonLock::acquire(&socket),
            Err(error) if error.contains("insecure LSP lock")
        ));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn cleanup_only_removes_artifacts_created_by_this_daemon() {
        let marker = temporary_path("owned-artifact");
        fs::write(&marker, "owned").unwrap();
        let artifact = OwnedArtifact::capture(&marker, ArtifactType::Regular).unwrap();
        fs::remove_file(&marker).unwrap();
        fs::write(&marker, "replacement").unwrap();
        // A replacement at the same path has a different inode, so the daemon's
        // cleanup must leave it untouched.
        artifact.remove();
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
