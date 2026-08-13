//! Capability-scoped loading and execution of runtime JavaScript.
//!
//! This module is compiled only with the `dynamic-js` feature. Ordinary AOT
//! applications therefore do not link SWC, W3IR lowering, or W3VM.

use anyhow::{Result, anyhow};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::TryRecvError;
use url::Url;
use w3cos_core::Value;
use w3cos_ir::BindingKind;
use w3cos_vm::{
    BindingCell, BindingCells, Limits, Vm, VmError, binding_cell, external_binding_cell,
    uninitialized_binding_cell,
};

pub use crate::html_compat::DocumentCompatibilityMode;
use crate::html_parser_host::ParserScriptHost;
pub use crate::html_parser_state::{DocumentParseProgress, StreamingDocumentParser};
#[cfg(test)]
use crate::html_parser_state::{HTML_NAMESPACE, MATHML_NAMESPACE, SVG_NAMESPACE};
use crate::html_parser_state::{parser_insertion_active, reset_parser_insertion_state};

thread_local! {
    static ACTIVE_DOCUMENT_LOADER: RefCell<Option<(ScriptLoader, String)>> =
        const { RefCell::new(None) };
    static DOCUMENT_PUMP_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static DYNAMIC_SCRIPT_NODES: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static DYNAMIC_STYLESHEET_NODES: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static DYNAMIC_IMAGE_NODES: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static SCRIPT_LOADERS: RefCell<Vec<Weak<ScriptLoaderInner>>> =
        const { RefCell::new(Vec::new()) };
}

static NEXT_STYLESHEET_OWNER: AtomicU64 = AtomicU64::new(1);
static NEXT_STYLESHEET_FONT_OWNER: AtomicU64 = AtomicU64::new(1);

const MAX_STYLESHEET_IMPORT_DEPTH: usize = 32;
const MAX_STYLESHEET_IMPORTS: usize = 256;

#[derive(Debug, Clone)]
pub struct ScriptPolicy {
    pub max_source_bytes: usize,
    /// Maximum compiled classic-script and ESM entries retained per loader.
    /// Persistent W3IR and HTTP response artifacts share this disk entry
    /// budget. Set to zero to disable both cache tiers.
    pub max_compiled_cache_entries: usize,
    /// Maximum estimated resident bytes retained per loader. Set to zero to
    /// disable the compiled-source cache. The same value bounds the combined
    /// persistent W3IR and HTTP response artifact bytes.
    pub max_compiled_cache_bytes: usize,
    /// Embedder-owned, application-private directory for persistent W3IR
    /// artifacts. `None` keeps the cache process-local and avoids assuming a
    /// platform storage path.
    pub compiled_cache_dir: Option<PathBuf>,
    /// Bounded transport/status retry policy shared by classic scripts and
    /// module graph fetches. The default performs one attempt, matching normal
    /// browser script fetching unless the embedder explicitly opts in.
    pub retry: ScriptRetryPolicy,
    pub limits: Limits,
    pub allow_network: bool,
    /// Execute classic/module/import-map scripts. Reader mode disables this
    /// while retaining the shared network stack for HTML, CSS and images.
    pub allow_scripts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptRetryPolicy {
    /// Total attempts including the initial request. Values below one are
    /// treated as one.
    pub max_attempts: u32,
    /// Initial exponential-backoff delay.
    pub base_delay_ms: u64,
    /// Upper bound for exponential backoff and `Retry-After`.
    pub max_delay_ms: u64,
    /// Honor delta-seconds and IMF-fixdate `Retry-After` response values.
    pub respect_retry_after: bool,
}

/// Credentials mode inherited by one ESM graph. The first fetch for a module
/// URL owns the module-map entry; later consumers reuse that entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ModuleCredentialsMode {
    Omit,
    SameOrigin,
    Include,
}

impl Default for ScriptRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            base_delay_ms: 100,
            max_delay_ms: 10_000,
            respect_retry_after: true,
        }
    }
}

impl Default for ScriptPolicy {
    fn default() -> Self {
        Self {
            max_source_bytes: 4 * 1024 * 1024,
            max_compiled_cache_entries: 256,
            max_compiled_cache_bytes: 64 * 1024 * 1024,
            compiled_cache_dir: None,
            retry: ScriptRetryPolicy::default(),
            limits: Limits::default(),
            allow_network: true,
            allow_scripts: true,
        }
    }
}

#[derive(Clone)]
pub struct ScriptLoader {
    inner: Rc<ScriptLoaderInner>,
}

impl ParserScriptHost for ScriptLoader {
    fn begin_document_parse(&self, document_url: &str) -> Result<()> {
        ScriptLoader::begin_document_parse(self, document_url)
    }

    fn has_pending_parser_blocking_script(&self) -> bool {
        ScriptLoader::has_pending_parser_blocking_script(self)
    }

    fn execute_pending_document_scripts(&self, document_url: &str) -> Result<()> {
        ScriptLoader::execute_pending_document_scripts(self, document_url).map(|_| ())
    }

    fn finish_document_parse(&self) {
        ScriptLoader::finish_document_parse(self);
    }
}

struct ScriptLoaderInner {
    policy: ScriptPolicy,
    stylesheet_owner: u64,
    source_cache: RefCell<HashMap<String, String>>,
    compiled_source_cache: RefCell<CompiledSourceCache>,
    http_source_cache_stats: RefCell<HttpSourceCacheCounters>,
    script_retry_stats: RefCell<ScriptRetryCounters>,
    processed_elements: RefCell<Vec<Value>>,
    pending_classic_fetches: RefCell<HashMap<String, PendingClassicFetch>>,
    processed_stylesheet_nodes: RefCell<HashSet<u32>>,
    installed_stylesheets: RefCell<HashMap<u32, InstalledStylesheet>>,
    pending_stylesheet_fetches: RefCell<HashMap<u32, PendingStylesheetFetch>>,
    pending_stylesheet_font_fetches: RefCell<HashMap<u32, PendingStylesheetFontFetch>>,
    deferred_stylesheet_fonts: RefCell<HashMap<u32, DeferredStylesheetFontBatch>>,
    stylesheet_font_owners: RefCell<HashMap<u32, u64>>,
    processed_image_nodes: RefCell<HashSet<u32>>,
    pending_image_fetches: RefCell<HashMap<u32, PendingImageFetch>>,
    processed_background_images: RefCell<HashSet<String>>,
    pending_background_image_fetches: RefCell<HashMap<String, PendingBackgroundImageFetch>>,
    image_decode_waiters: RefCell<HashMap<u32, Vec<ImageDecodeWaiter>>>,
    ready_stylesheets: RefCell<BTreeMap<u64, ReadyStylesheet>>,
    next_stylesheet_order: Cell<u64>,
    next_stylesheet_apply: Cell<u64>,
    ready_ordered_classic_scripts: RefCell<
        BTreeMap<
            u64,
            (
                String,
                ClassicScriptRequest,
                std::result::Result<String, String>,
            ),
        >,
    >,
    ready_deferred_classic_scripts: RefCell<
        BTreeMap<
            u64,
            (
                String,
                ClassicScriptRequest,
                std::result::Result<String, String>,
            ),
        >,
    >,
    cancelled_classic_orders: RefCell<HashSet<u64>>,
    cancelled_deferred_classic_orders: RefCell<HashSet<u64>>,
    next_classic_order: Cell<u64>,
    next_classic_execution: Cell<u64>,
    next_deferred_classic_order: Cell<u64>,
    next_deferred_classic_execution: Cell<u64>,
    module_element_cancellations: RefCell<HashMap<u32, Rc<Cell<bool>>>>,
    module_records: RefCell<HashMap<String, Rc<ModuleRecord>>>,
    import_map: RefCell<ImportMapState>,
    module_resolution_started: Cell<bool>,
    resolved_module_requests: RefCell<HashSet<ResolvedModuleRequest>>,
    module_request_origin: RefCell<Option<String>>,
    document_url: RefCell<Option<String>>,
    parser_finished: Cell<bool>,
    dom_content_loaded_fired: Cell<bool>,
    document_load_fired: Cell<bool>,
    document_load_queued: Cell<bool>,
    document_lifecycle_generation: Cell<u64>,
    dom_content_loaded_blockers: RefCell<HashSet<u32>>,
    document_load_blockers: RefCell<HashSet<u32>>,
    parser_blocking_elements: RefCell<HashSet<u32>>,
    deferred_parser_modules: RefCell<Vec<DeferredParserModule>>,
    module_final_urls: RefCell<HashMap<String, String>>,
    pending_source_fetches: RefCell<HashMap<String, PendingSourceFetch>>,
    module_graph_loads: RefCell<HashMap<String, Rc<RefCell<ModuleGraphLoad>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum CompileMode {
    ClassicScript,
    Module,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptExecutionRoute {
    RuntimeW3vm,
    PrecompiledAot,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CompiledSourceCacheKey {
    resolved_url: String,
    source_hash: u64,
    source_len: usize,
    w3ir_format_version: u32,
    compile_mode: CompileMode,
}

struct CompiledSourceCacheEntry {
    source: String,
    module: w3cos_ir::Module,
    resident_bytes: usize,
    last_used: u64,
}

#[derive(Default)]
struct CompiledSourceCache {
    entries: HashMap<CompiledSourceCacheKey, CompiledSourceCacheEntry>,
    resident_bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    persistent_hits: u64,
    persistent_misses: u64,
    persistent_writes: u64,
    persistent_evictions: u64,
    persistent_errors: u64,
}

/// Read-only counters for the shared classic-script and ESM W3IR cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompiledSourceCacheStats {
    pub entries: usize,
    pub resident_bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub persistent_hits: u64,
    pub persistent_misses: u64,
    pub persistent_writes: u64,
    pub persistent_evictions: u64,
    pub persistent_errors: u64,
}

const PERSISTENT_COMPILED_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistentCompiledSource {
    schema_version: u32,
    resolved_url: String,
    source_hash: u64,
    source_len: usize,
    w3ir_format_version: u32,
    compile_mode: CompileMode,
    source: String,
    module: w3cos_ir::Module,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum ScriptFetchMode {
    ClassicScript(ClassicScriptFetchMode),
    Module(ModuleCredentialsMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum ClassicScriptFetchMode {
    NoCors,
    CorsAnonymous,
    CorsUseCredentials,
}

type PersistentHttpSource = crate::browser_http_cache::CachedResponse;

#[derive(Default)]
struct HttpSourceCacheCounters {
    candidates: u64,
    misses: u64,
    not_modified: u64,
    refreshed: u64,
    writes: u64,
    evictions: u64,
    errors: u64,
}

#[derive(Default)]
struct ScriptRetryCounters {
    scheduled: u64,
    started: u64,
    succeeded: u64,
    exhausted: u64,
    cancelled: u64,
}

/// Read-only counters for classic-script and ESM fetch retries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptRetryStats {
    pub scheduled: u64,
    pub started: u64,
    pub succeeded: u64,
    pub exhausted: u64,
    pub cancelled: u64,
}

/// Read-only counters for persistent HTTP validator/body revalidation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HttpSourceCacheStats {
    pub candidates: u64,
    pub misses: u64,
    pub not_modified: u64,
    pub refreshed: u64,
    pub writes: u64,
    pub evictions: u64,
    pub errors: u64,
}

enum PersistentCacheLookup {
    Hit(w3cos_ir::Module),
    Miss,
    Error,
}

struct PendingSourceFetch {
    url: String,
    task: Option<crate::fetch::TextFetchTask>,
    cached_response: Option<PersistentHttpSource>,
    options: crate::fetch::FetchOptions,
    request_origin: Option<String>,
    referrer_source: String,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
    credentials_mode: ModuleCredentialsMode,
    attempts_started: u32,
    retry_at: Option<std::time::Instant>,
}

struct PendingClassicFetch {
    url: String,
    fetch_mode: ClassicScriptFetchMode,
    integrity: String,
    referrer_source: String,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
    task: Option<crate::fetch::TextFetchTask>,
    cached_response: Option<PersistentHttpSource>,
    options: crate::fetch::FetchOptions,
    attempts_started: u32,
    retry_at: Option<std::time::Instant>,
    requests: Vec<ClassicScriptRequest>,
}

struct PendingStylesheetFetch {
    graph: StylesheetGraphLoad,
    action: StylesheetFetchAction,
    request_url: String,
    cache_key: crate::browser_http_cache::CacheKey,
    request_headers: HashMap<String, String>,
    cached_response: Option<PersistentHttpSource>,
    task: crate::fetch::TextFetchTask,
}

struct StylesheetGraphLoad {
    element: Value,
    node: u32,
    order: u64,
    request_origin: String,
    credentials_mode: ModuleCredentialsMode,
    cors_enabled: bool,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
    actions: VecDeque<StylesheetGraphAction>,
    expanded_source: String,
    root_href: Option<String>,
    fetched_imports: usize,
    total_source_bytes: usize,
    font_faces: Vec<StylesheetFontFaceLoad>,
    fonts_discovered: bool,
    font_owner: Option<u64>,
    font_only: bool,
    font_loading_started: bool,
    font_loading_failed: bool,
    font_event_faces: Vec<Value>,
}

struct StylesheetFetchAction {
    url: String,
    media: Option<String>,
    depth: usize,
    root: bool,
    ancestry: Vec<String>,
    referrer_source: String,
    integrity: String,
}

enum StylesheetGraphAction {
    Fetch(StylesheetFetchAction),
    Append {
        source: String,
        base_url: String,
        media: Option<String>,
        font_faces: Vec<StylesheetFontFaceLoad>,
    },
    LoadFont(StylesheetFontFaceLoad),
}

#[derive(Clone)]
struct StylesheetFontFaceLoad {
    face: w3cos_compiler::esm_css::StylesheetFontFace,
    base_url: String,
    js_face: Option<Value>,
    demanded: bool,
}

struct DeferredStylesheetFontBatch {
    element: Value,
    node: u32,
    request_origin: String,
    credentials_mode: ModuleCredentialsMode,
    cors_enabled: bool,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
    faces: Vec<StylesheetFontFaceLoad>,
}

struct PendingStylesheetFontFetch {
    graph: StylesheetGraphLoad,
    face: w3cos_compiler::esm_css::StylesheetFontFace,
    base_url: String,
    js_face: Option<Value>,
    remaining_sources: VecDeque<w3cos_compiler::esm_css::StylesheetFontSource>,
    request_url: String,
    cache_key: crate::browser_http_cache::CacheKey,
    request_headers: HashMap<String, String>,
    cached_response: Option<PersistentHttpSource>,
    task: crate::fetch::BinaryFetchTask,
}

struct PendingImageFetch {
    element: Value,
    density: f64,
    request: BrowserImageRequest,
}

struct BrowserImageRequest {
    source: String,
    request_url: String,
    request_origin: String,
    credentials_mode: ModuleCredentialsMode,
    cors_enabled: bool,
    cache_key: crate::browser_http_cache::CacheKey,
    request_headers: HashMap<String, String>,
    cached_response: Option<PersistentHttpSource>,
    task: crate::fetch::BinaryFetchTask,
}

struct PendingBackgroundImageFetch {
    request: BrowserImageRequest,
}

struct ImageDecodeWaiter {
    resolve: Value,
    reject: Value,
}

struct ReadyStylesheet {
    element: Value,
    node: u32,
    source: std::result::Result<Option<(Option<String>, String)>, String>,
}

#[derive(Clone)]
struct InstalledStylesheet {
    href: Option<String>,
    source: String,
}

struct ClassicScriptRequest {
    element: Value,
    order: Option<u64>,
    deferred_until_parse_end: bool,
}

struct ModuleGraphLoad {
    root: String,
    integrity: String,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
    promise: Value,
    resolve: Value,
    reject: Value,
    scheduled: HashSet<String>,
    pending: HashSet<String>,
    credentials_mode: ModuleCredentialsMode,
    element_consumers: HashSet<u32>,
    uncancellable_consumers: u32,
    settled: bool,
}

struct DeferredParserModule {
    element: Value,
    specifier: std::result::Result<String, String>,
    prepared_graph: Option<Value>,
    element_node: u32,
    cancellation: Rc<Cell<bool>>,
    credentials_mode: ModuleCredentialsMode,
    integrity: String,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
}

impl Drop for ScriptLoaderInner {
    fn drop(&mut self) {
        teardown_runtime_module_records(self.module_records.get_mut());
        for load in self.module_graph_loads.get_mut().values() {
            reject_graph_load(load, "dynamic module loader was cancelled");
        }
        for fetch in self.pending_classic_fetches.get_mut().values() {
            if let Some(task) = &fetch.task {
                task.cancel();
            }
        }
        for fetch in self.pending_source_fetches.get_mut().values() {
            if let Some(task) = &fetch.task {
                task.cancel();
            }
        }
        for fetch in self.pending_stylesheet_fetches.get_mut().values() {
            fetch.task.cancel();
        }
        for fetch in self.pending_stylesheet_font_fetches.get_mut().values() {
            fetch.task.cancel();
            if fetch.graph.font_loading_started {
                crate::font_face::FontFaceSet::global().mark_ready();
                crate::font_loading_web::cancel_font_loading(fetch.graph.font_event_faces.clone());
            }
        }
        let cancelled = self
            .pending_classic_fetches
            .get_mut()
            .values()
            .filter(|fetch| fetch.retry_at.is_some() || fetch.attempts_started > 1)
            .count()
            + self
                .pending_source_fetches
                .get_mut()
                .values()
                .filter(|fetch| fetch.retry_at.is_some() || fetch.attempts_started > 1)
                .count();
        let stats = self.script_retry_stats.get_mut();
        stats.cancelled = stats.cancelled.saturating_add(cancelled as u64);
        self.pending_classic_fetches.get_mut().clear();
        self.pending_source_fetches.get_mut().clear();
        self.pending_stylesheet_fetches.get_mut().clear();
        self.pending_stylesheet_font_fetches.get_mut().clear();
        self.installed_stylesheets.get_mut().clear();
        for owner in self.stylesheet_font_owners.get_mut().values() {
            crate::font_face::FontRegistry::global().clear_owner(*owner);
            crate::font_loading_web::clear_stylesheet_font_owner(*owner);
        }
        self.stylesheet_font_owners.get_mut().clear();
        w3cos_dom::stylesheet::clear_owner(self.stylesheet_owner);
    }
}

#[derive(Clone, Default)]
struct ImportMapState {
    imports: ImportEntries,
    /// Canonical scope URL prefix plus its normalized specifier map, ordered
    /// longest-prefix first.
    scopes: Vec<(String, ImportEntries)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ResolvedModuleRequest {
    base: String,
    request: String,
    resolved: String,
}

type ImportEntries = HashMap<String, ImportMapTarget>;

#[derive(Clone)]
enum ImportMapTarget {
    Address(String),
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleState {
    Linked,
    Evaluating,
    Evaluated,
    Failed,
}

struct ModuleRecord {
    module: w3cos_ir::Module,
    vm: Vm,
    bindings: RefCell<BindingCells>,
    state: Cell<ModuleState>,
    evaluation_promise: RefCell<Option<Value>>,
    evaluation_error: RefCell<Option<String>>,
    cycle_root: RefCell<Option<Weak<ModuleRecord>>>,
    star_export_records: RefCell<Vec<Weak<ModuleRecord>>>,
    /// Native/AOT providers forwarded through `export * from ...`.
    star_export_external_urls: RefCell<Vec<String>>,
    namespace: RefCell<Option<Value>>,
    credentials_mode: ModuleCredentialsMode,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
}

fn teardown_runtime_module_records(records: &mut HashMap<String, Rc<ModuleRecord>>) {
    for record in records.values() {
        record.vm.cancellation_token().cancel();
    }
    for specifier in records.keys() {
        w3cos_core::module_registry::unregister(specifier);
    }
    records.clear();
}

impl StreamingDocumentParser {
    pub fn new(loader: ScriptLoader, document_url: &str) -> Result<Self> {
        Self::new_with_script_host(Rc::new(loader), document_url)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DocumentLoaderOptions {
    pub max_body_bytes: usize,
    pub parse_chunk_bytes: usize,
    pub timeout_ms: u64,
}

impl Default for DocumentLoaderOptions {
    fn default() -> Self {
        Self {
            max_body_bytes: 16 * 1024 * 1024,
            parse_chunk_bytes: 16 * 1024,
            timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentLoadProgress {
    Idle,
    Fetching,
    Parsing,
    BlockedOnScript,
    WaitingForLoad,
    Complete,
    Failed(String),
    Cancelled,
}

/// Top-level Browser navigation loader sharing Fetch, Cookie Store, the live
/// DOM, [`StreamingDocumentParser`], and [`ScriptLoader`].
pub struct DocumentLoader {
    script_loader: ScriptLoader,
    options: DocumentLoaderOptions,
    fetch: Option<crate::fetch::BytesFetchTask>,
    parser: Option<StreamingDocumentParser>,
    decoded_source: Option<String>,
    source_offset: usize,
    sniff_buffer: Vec<u8>,
    decoder: Option<DocumentByteDecoder>,
    content_type: String,
    plain_text: bool,
    network_complete: bool,
    requested_url: Option<String>,
    final_url: Option<String>,
    redirected: bool,
    terminal_error: Option<String>,
    progress: DocumentLoadProgress,
}

impl DocumentLoader {
    pub fn new(script_policy: ScriptPolicy, options: DocumentLoaderOptions) -> Self {
        Self {
            script_loader: ScriptLoader::new(script_policy),
            options,
            fetch: None,
            parser: None,
            decoded_source: None,
            source_offset: 0,
            sniff_buffer: Vec::new(),
            decoder: None,
            content_type: String::new(),
            plain_text: false,
            network_complete: false,
            requested_url: None,
            final_url: None,
            redirected: false,
            terminal_error: None,
            progress: DocumentLoadProgress::Idle,
        }
    }

    pub fn navigate(&mut self, url: &str) -> Result<()> {
        let requested =
            Url::parse(url).map_err(|error| anyhow!("invalid document URL {url}: {error}"))?;
        if !matches!(requested.scheme(), "http" | "https") {
            return Err(anyhow!(
                "document navigation supports only HTTP(S), got {}",
                requested.scheme()
            ));
        }
        self.cancel_active_work();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        self.script_loader
            .begin_document_parse(requested.as_str())?;

        let previous_url = crate::history::get_href();
        crate::history::location_replace(requested.as_str());
        let mut fetch_options = crate::fetch::FetchOptions {
            timeout_ms: Some(self.options.timeout_ms),
            ..crate::fetch::FetchOptions::default()
        };
        fetch_options.headers.insert(
            "accept".to_string(),
            "text/html,application/xhtml+xml;q=0.9,text/plain;q=0.8,*/*;q=0.1".to_string(),
        );
        self.fetch = Some(crate::fetch::fetch_document_bytes_async(
            requested.as_str(),
            fetch_options,
            previous_url.clone(),
            crate::cookie_store_web::snapshot(),
            previous_url,
            crate::fetch::ScriptReferrerPolicy::default(),
            self.options.max_body_bytes,
        ));
        self.requested_url = Some(requested.to_string());
        self.final_url = None;
        self.redirected = false;
        self.parser = None;
        self.decoded_source = None;
        self.source_offset = 0;
        self.sniff_buffer.clear();
        self.decoder = None;
        self.content_type.clear();
        self.plain_text = false;
        self.network_complete = false;
        self.terminal_error = None;
        self.progress = DocumentLoadProgress::Fetching;
        Ok(())
    }

    pub fn poll(&mut self) -> DocumentLoadProgress {
        crate::jsdom::drain_microtasks();
        if matches!(
            self.progress,
            DocumentLoadProgress::Idle
                | DocumentLoadProgress::Complete
                | DocumentLoadProgress::Failed(_)
                | DocumentLoadProgress::Cancelled
        ) {
            return self.progress.clone();
        }
        if self.progress == DocumentLoadProgress::WaitingForLoad
            && self.script_loader.document_load_complete()
        {
            self.finish_document_load();
            return self.progress.clone();
        }

        loop {
            let event = self.fetch.as_ref().map(|task| task.receiver.try_recv());
            match event {
                Some(Ok(event)) => {
                    if self.process_fetch_event(event) {
                        break;
                    }
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    self.fetch = None;
                    if !self.network_complete {
                        self.install_error_page("document fetch worker disconnected".to_string());
                    }
                    break;
                }
                Some(Err(TryRecvError::Empty)) | None => break,
            }
        }

        self.advance_parser();
        self.progress.clone()
    }

    pub fn cancel(&mut self) {
        self.cancel_active_work();
        self.progress = DocumentLoadProgress::Cancelled;
        crate::jsdom::set_document_ready_state("complete");
    }

    pub fn progress(&self) -> &DocumentLoadProgress {
        &self.progress
    }

    pub fn requested_url(&self) -> Option<&str> {
        self.requested_url.as_deref()
    }

    pub fn final_url(&self) -> Option<&str> {
        self.final_url.as_deref()
    }

    pub fn redirected(&self) -> bool {
        self.redirected
    }

    fn cancel_active_work(&mut self) {
        if let Some(fetch) = self.fetch.take() {
            fetch.cancel();
        }
        self.parser = None;
        self.decoded_source = None;
        self.source_offset = 0;
        self.sniff_buffer.clear();
        self.decoder = None;
        self.content_type.clear();
        self.plain_text = false;
        self.network_complete = false;
        self.script_loader.cancel_for_navigation();
        ACTIVE_DOCUMENT_LOADER.with(|active| {
            let should_detach = active
                .borrow()
                .as_ref()
                .is_some_and(|(loader, _)| Rc::ptr_eq(&loader.inner, &self.script_loader.inner));
            if should_detach {
                active.borrow_mut().take();
            }
        });
    }

    fn process_fetch_event(&mut self, event: crate::fetch::DocumentFetchEvent) -> bool {
        match event {
            crate::fetch::DocumentFetchEvent::Response(response) => {
                self.accept_response(response);
                self.fetch.is_none()
            }
            crate::fetch::DocumentFetchEvent::BodyChunk(bytes) => {
                if let Err(error) = self.accept_body_chunk(&bytes, false) {
                    self.install_error_page(error.to_string());
                    true
                } else {
                    false
                }
            }
            crate::fetch::DocumentFetchEvent::Complete => {
                self.fetch = None;
                self.network_complete = true;
                if let Err(error) = self.accept_body_chunk(&[], true) {
                    self.install_error_page(error.to_string());
                }
                true
            }
            crate::fetch::DocumentFetchEvent::Error(error) => {
                self.fetch = None;
                self.install_error_page(error);
                true
            }
        }
    }

    fn accept_response(&mut self, response: crate::fetch::FetchBytesResponse) {
        for (url, cookie) in &response.set_cookies {
            crate::cookie_store_web::set_cookie_assignment_for_url(url, cookie, true);
        }
        self.final_url = Some(response.url.clone());
        self.redirected = response.redirected;
        crate::history::location_replace(&response.url);
        if !response.ok {
            self.install_error_page(format!(
                "navigation failed with status {} {}",
                response.status, response.status_text
            ));
            return;
        }

        let content_type = header_value(&response.headers, "content-type").unwrap_or_default();
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let plain_text = if media_type.is_empty()
            || matches!(media_type.as_str(), "text/html" | "application/xhtml+xml")
        {
            false
        } else if media_type == "text/plain" {
            true
        } else {
            self.install_error_page(format!("unsupported document MIME type {media_type:?}"));
            return;
        };
        if let Err(error) = self.script_loader.begin_document_parse(&response.url) {
            self.install_error_page(error.to_string());
            return;
        }
        match StreamingDocumentParser::from_started_navigation(
            Rc::new(self.script_loader.clone()),
            &response.url,
        ) {
            Ok(parser) => {
                self.parser = Some(parser);
                self.decoded_source = Some(if plain_text {
                    "<html><body><pre>".to_string()
                } else {
                    String::new()
                });
                self.source_offset = 0;
                self.sniff_buffer.clear();
                self.decoder = None;
                self.content_type = content_type.to_string();
                self.plain_text = plain_text;
                self.network_complete = false;
                self.progress = DocumentLoadProgress::Parsing;
            }
            Err(error) => self.install_error_page(error.to_string()),
        }
    }

    fn accept_body_chunk(&mut self, bytes: &[u8], final_chunk: bool) -> Result<()> {
        if self.parser.is_none() {
            return Ok(());
        }
        let decoded = if let Some(decoder) = self.decoder.as_mut() {
            decoder.decode(bytes, final_chunk)?
        } else {
            self.sniff_buffer.extend_from_slice(bytes);
            let Some((mut decoder, bom_bytes)) =
                DocumentByteDecoder::detect(&self.sniff_buffer, &self.content_type, final_chunk)?
            else {
                return Ok(());
            };
            let buffered = self.sniff_buffer.split_off(bom_bytes);
            self.sniff_buffer.clear();
            let decoded = decoder.decode(&buffered, final_chunk)?;
            self.decoder = Some(decoder);
            decoded
        };
        if let Some(source) = self.decoded_source.as_mut() {
            if self.plain_text {
                source.push_str(&escape_html_text(&decoded));
                if final_chunk {
                    source.push_str("</pre></body></html>");
                }
            } else {
                source.push_str(&decoded);
            }
        }
        Ok(())
    }

    fn advance_parser(&mut self) {
        let Some(parser) = self.parser.as_mut() else {
            return;
        };
        if parser.is_blocked() {
            self.progress = DocumentLoadProgress::BlockedOnScript;
            return;
        }
        if self.progress == DocumentLoadProgress::BlockedOnScript {
            match parser.resume() {
                Ok(DocumentParseProgress::BlockedOnScript) => {
                    self.progress = DocumentLoadProgress::BlockedOnScript;
                    return;
                }
                Ok(DocumentParseProgress::Complete) => {
                    self.finish_parser();
                    return;
                }
                Ok(DocumentParseProgress::Advanced) => {}
                Err(error) => {
                    self.install_error_page(error.to_string());
                    return;
                }
            }
        }

        let source_len = self.decoded_source.as_ref().map_or(0, String::len);
        while self.source_offset < source_len {
            let source = self
                .decoded_source
                .as_ref()
                .expect("document source remains available while parsing");
            let mut end = self
                .source_offset
                .saturating_add(self.options.parse_chunk_bytes.max(1))
                .min(source.len());
            while end > self.source_offset && !source.is_char_boundary(end) {
                end -= 1;
            }
            if end == self.source_offset {
                end = source[self.source_offset..]
                    .char_indices()
                    .nth(1)
                    .map_or(source.len(), |(relative, _)| self.source_offset + relative);
            }
            let chunk = source[self.source_offset..end].to_string();
            self.source_offset = end;
            match parser.write(&chunk) {
                Ok(DocumentParseProgress::BlockedOnScript) => {
                    self.progress = DocumentLoadProgress::BlockedOnScript;
                    return;
                }
                Ok(DocumentParseProgress::Advanced) => {}
                Ok(DocumentParseProgress::Complete) => {
                    self.finish_parser();
                    return;
                }
                Err(error) => {
                    self.install_error_page(error.to_string());
                    return;
                }
            }
        }
        if !self.network_complete {
            self.progress = DocumentLoadProgress::Parsing;
            return;
        }
        match parser.finish() {
            Ok(DocumentParseProgress::BlockedOnScript) => {
                self.progress = DocumentLoadProgress::BlockedOnScript;
            }
            Ok(DocumentParseProgress::Complete) => self.finish_parser(),
            Ok(DocumentParseProgress::Advanced) => {
                self.progress = DocumentLoadProgress::Parsing;
            }
            Err(error) => self.install_error_page(error.to_string()),
        }
    }

    fn finish_parser(&mut self) {
        self.parser = None;
        self.decoded_source = None;
        self.source_offset = 0;
        self.sniff_buffer.clear();
        self.decoder = None;
        self.progress = DocumentLoadProgress::WaitingForLoad;
        if self.script_loader.document_load_complete() {
            self.finish_document_load();
        }
    }

    fn finish_document_load(&mut self) {
        if let Some(error) = self.terminal_error.take() {
            self.progress = DocumentLoadProgress::Failed(error);
        } else {
            self.progress = DocumentLoadProgress::Complete;
        }
    }

    fn install_error_page(&mut self, error: String) {
        let document_url = self
            .final_url
            .clone()
            .or_else(|| self.requested_url.clone())
            .unwrap_or_else(|| "https://invalid.invalid/".to_string());
        if let Some(fetch) = self.fetch.take() {
            fetch.cancel();
        }
        self.parser = None;
        self.decoded_source = None;
        self.source_offset = 0;
        self.sniff_buffer.clear();
        self.decoder = None;
        self.content_type.clear();
        self.plain_text = false;
        self.network_complete = true;
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        self.terminal_error = Some(error.clone());
        let source = format!(
            "<html><head><title>Navigation error</title></head><body>\
             <h1>Unable to load page</h1><pre>{}</pre></body></html>",
            escape_html_text(&error)
        );
        if self
            .script_loader
            .begin_document_parse(&document_url)
            .is_err()
        {
            self.progress = DocumentLoadProgress::Failed(error);
            return;
        }
        match StreamingDocumentParser::from_started_navigation(
            Rc::new(self.script_loader.clone()),
            &document_url,
        ) {
            Ok(parser) => {
                self.parser = Some(parser);
                self.decoded_source = Some(source);
                self.source_offset = 0;
                self.network_complete = true;
                self.progress = DocumentLoadProgress::Parsing;
            }
            Err(_) => self.progress = DocumentLoadProgress::Failed(error),
        }
    }
}

impl Drop for DocumentLoader {
    fn drop(&mut self) {
        self.cancel_active_work();
    }
}

#[derive(Debug)]
enum DocumentByteDecoder {
    Utf8(Vec<u8>),
    Utf16 {
        little_endian: bool,
        pending: Vec<u8>,
    },
    Windows1252,
}

impl DocumentByteDecoder {
    fn detect(
        bytes: &[u8],
        content_type: &str,
        final_chunk: bool,
    ) -> Result<Option<(Self, usize)>> {
        if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            return Ok(Some((Self::Utf8(Vec::new()), 3)));
        }
        if bytes.starts_with(&[0xff, 0xfe]) {
            return Ok(Some((
                Self::Utf16 {
                    little_endian: true,
                    pending: Vec::new(),
                },
                2,
            )));
        }
        if bytes.starts_with(&[0xfe, 0xff]) {
            return Ok(Some((
                Self::Utf16 {
                    little_endian: false,
                    pending: Vec::new(),
                },
                2,
            )));
        }
        let transport_charset = content_type_charset(content_type);
        if bytes.len() < 3
            && !final_chunk
            && transport_charset.is_none()
            && matches!(bytes, [0xef] | [0xef, 0xbb] | [0xff] | [0xfe])
        {
            return Ok(None);
        }
        if transport_charset.is_none() && bytes.len() < 1024 && !final_chunk {
            return Ok(None);
        }
        let charset = transport_charset
            .or_else(|| sniff_meta_charset(bytes))
            .unwrap_or_else(|| "windows-1252".to_string());
        let decoder = match charset.as_str() {
            "utf-8" | "utf8" => Self::Utf8(Vec::new()),
            "utf-16" | "utf-16le" => Self::Utf16 {
                little_endian: true,
                pending: Vec::new(),
            },
            "utf-16be" => Self::Utf16 {
                little_endian: false,
                pending: Vec::new(),
            },
            "windows-1252" | "cp1252" | "iso-8859-1" | "latin1" => Self::Windows1252,
            _ => return Err(anyhow!("unsupported document charset {charset:?}")),
        };
        Ok(Some((decoder, 0)))
    }

    fn decode(&mut self, bytes: &[u8], final_chunk: bool) -> Result<String> {
        match self {
            Self::Utf8(pending) => {
                pending.extend_from_slice(bytes);
                let mut decoded = String::new();
                loop {
                    match std::str::from_utf8(pending) {
                        Ok(text) => {
                            decoded.push_str(text);
                            pending.clear();
                            break;
                        }
                        Err(error) => {
                            let valid = error.valid_up_to();
                            if valid > 0 {
                                decoded.push_str(
                                    std::str::from_utf8(&pending[..valid])
                                        .expect("UTF-8 validator marks the prefix as valid"),
                                );
                                pending.drain(..valid);
                            }
                            if let Some(invalid) = error.error_len() {
                                decoded.push(char::REPLACEMENT_CHARACTER);
                                pending.drain(..invalid);
                            } else {
                                if final_chunk && !pending.is_empty() {
                                    decoded.push(char::REPLACEMENT_CHARACTER);
                                    pending.clear();
                                }
                                break;
                            }
                        }
                    }
                }
                Ok(decoded)
            }
            Self::Utf16 {
                little_endian,
                pending,
            } => {
                pending.extend_from_slice(bytes);
                if final_chunk && !pending.len().is_multiple_of(2) {
                    return Err(anyhow!("UTF-16 document body has an odd byte length"));
                }
                let mut process_len = pending.len() / 2 * 2;
                if !final_chunk && process_len >= 2 {
                    let last = if *little_endian {
                        u16::from_le_bytes([pending[process_len - 2], pending[process_len - 1]])
                    } else {
                        u16::from_be_bytes([pending[process_len - 2], pending[process_len - 1]])
                    };
                    if (0xd800..=0xdbff).contains(&last) {
                        process_len -= 2;
                    }
                }
                let decoded = decode_utf16_document(&pending[..process_len], *little_endian)?;
                pending.drain(..process_len);
                Ok(decoded)
            }
            Self::Windows1252 => Ok(decode_windows_1252(bytes)),
        }
    }
}

#[cfg(test)]
fn decode_document_bytes(bytes: &[u8], content_type: &str) -> Result<String> {
    if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16_document(bytes, true);
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16_document(bytes, false);
    }
    let charset = content_type_charset(content_type)
        .or_else(|| sniff_meta_charset(bytes))
        .unwrap_or_else(|| "windows-1252".to_string());
    match charset.as_str() {
        "utf-8" | "utf8" => Ok(String::from_utf8_lossy(bytes).into_owned()),
        "utf-16" | "utf-16le" => decode_utf16_document(bytes, true),
        "utf-16be" => decode_utf16_document(bytes, false),
        "windows-1252" | "cp1252" | "iso-8859-1" | "latin1" => Ok(decode_windows_1252(bytes)),
        _ => Err(anyhow!("unsupported document charset {charset:?}")),
    }
}

fn content_type_charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\'', '"']).to_ascii_lowercase())
    })
}

fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let mut rest = prefix.as_str();
    while let Some(index) = rest.find("<meta") {
        rest = &rest[index + "<meta".len()..];
        if rest
            .chars()
            .next()
            .is_some_and(|character| !character.is_whitespace() && character != '>')
        {
            continue;
        }
        let end = crate::jsdom::html_tag_end(rest).unwrap_or(rest.len());
        let attributes = crate::jsdom::parse_html_attributes(&rest[..end]);
        if let Some(charset) = attributes
            .iter()
            .find(|(name, _)| name == "charset")
            .map(|(_, value)| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
        {
            return Some(charset);
        }
        if let Some(content) = attributes
            .iter()
            .find(|(name, _)| name == "content")
            .map(|(_, value)| value)
            && let Some(charset) = content_type_charset(content)
        {
            return Some(charset);
        }
        rest = &rest[end.min(rest.len())..];
    }
    None
}

fn decode_utf16_document(bytes: &[u8], little_endian: bool) -> Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(anyhow!("UTF-16 document body has an odd byte length"));
    }
    let units = bytes.chunks_exact(2).map(|pair| {
        if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        }
    });
    Ok(char::decode_utf16(units)
        .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect())
}

fn decode_windows_1252(bytes: &[u8]) -> String {
    const C1: [char; 32] = [
        '\u{20ac}', '\u{0081}', '\u{201a}', '\u{0192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02c6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008d}',
        '\u{017d}', '\u{008f}', '\u{0090}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02dc}', '\u{2122}', '\u{0161}', '\u{203a}',
        '\u{0153}', '\u{009d}', '\u{017e}', '\u{0178}',
    ];
    bytes
        .iter()
        .map(|byte| match *byte {
            0x80..=0x9f => C1[usize::from(*byte - 0x80)],
            byte => char::from(byte),
        })
        .collect()
}

fn escape_html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl ScriptLoader {
    pub fn new(policy: ScriptPolicy) -> Self {
        let stylesheet_owner = NEXT_STYLESHEET_OWNER.fetch_add(1, Ordering::Relaxed);
        let inner = Rc::new(ScriptLoaderInner {
            policy,
            stylesheet_owner,
            source_cache: RefCell::new(HashMap::new()),
            compiled_source_cache: RefCell::new(CompiledSourceCache::default()),
            http_source_cache_stats: RefCell::new(HttpSourceCacheCounters::default()),
            script_retry_stats: RefCell::new(ScriptRetryCounters::default()),
            processed_elements: RefCell::new(Vec::new()),
            pending_classic_fetches: RefCell::new(HashMap::new()),
            processed_stylesheet_nodes: RefCell::new(HashSet::new()),
            installed_stylesheets: RefCell::new(HashMap::new()),
            pending_stylesheet_fetches: RefCell::new(HashMap::new()),
            pending_stylesheet_font_fetches: RefCell::new(HashMap::new()),
            deferred_stylesheet_fonts: RefCell::new(HashMap::new()),
            stylesheet_font_owners: RefCell::new(HashMap::new()),
            processed_image_nodes: RefCell::new(HashSet::new()),
            pending_image_fetches: RefCell::new(HashMap::new()),
            processed_background_images: RefCell::new(HashSet::new()),
            pending_background_image_fetches: RefCell::new(HashMap::new()),
            image_decode_waiters: RefCell::new(HashMap::new()),
            ready_stylesheets: RefCell::new(BTreeMap::new()),
            next_stylesheet_order: Cell::new(0),
            next_stylesheet_apply: Cell::new(0),
            ready_ordered_classic_scripts: RefCell::new(BTreeMap::new()),
            ready_deferred_classic_scripts: RefCell::new(BTreeMap::new()),
            cancelled_classic_orders: RefCell::new(HashSet::new()),
            cancelled_deferred_classic_orders: RefCell::new(HashSet::new()),
            next_classic_order: Cell::new(0),
            next_classic_execution: Cell::new(0),
            next_deferred_classic_order: Cell::new(0),
            next_deferred_classic_execution: Cell::new(0),
            module_element_cancellations: RefCell::new(HashMap::new()),
            module_records: RefCell::new(HashMap::new()),
            import_map: RefCell::new(ImportMapState::default()),
            module_resolution_started: Cell::new(false),
            resolved_module_requests: RefCell::new(HashSet::new()),
            module_request_origin: RefCell::new(None),
            document_url: RefCell::new(None),
            parser_finished: Cell::new(true),
            dom_content_loaded_fired: Cell::new(true),
            document_load_fired: Cell::new(true),
            document_load_queued: Cell::new(false),
            document_lifecycle_generation: Cell::new(0),
            dom_content_loaded_blockers: RefCell::new(HashSet::new()),
            document_load_blockers: RefCell::new(HashSet::new()),
            parser_blocking_elements: RefCell::new(HashSet::new()),
            deferred_parser_modules: RefCell::new(Vec::new()),
            module_final_urls: RefCell::new(HashMap::new()),
            pending_source_fetches: RefCell::new(HashMap::new()),
            module_graph_loads: RefCell::new(HashMap::new()),
        });
        SCRIPT_LOADERS.with(|loaders| loaders.borrow_mut().push(Rc::downgrade(&inner)));
        let loader = Self { inner };
        let weak_loader = Rc::downgrade(&loader.inner);
        w3cos_core::host_modules::register(
            w3cos_core::host_modules::DYNAMIC_IMPORT_PATH,
            Value::function(move |_, arguments| {
                let Some(inner) = weak_loader.upgrade() else {
                    return w3cos_core::promise::reject(vec![Value::string(
                        "AbortError: the page module loader is no longer available",
                    )]);
                };
                let request = arguments
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let referrer = arguments
                    .get(1)
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                ScriptLoader { inner }.dynamic_import_value(
                    &referrer,
                    &request,
                    ModuleCredentialsMode::SameOrigin,
                    crate::fetch::ScriptReferrerPolicy::default(),
                )
            }),
        );
        loader
    }

    pub fn execute_source(&self, source: &str, specifier: &str) -> Result<Value> {
        if script_execution_route(specifier) == ScriptExecutionRoute::PrecompiledAot {
            return self.execute_precompiled_aot(specifier);
        }
        if source.len() > self.inner.policy.max_source_bytes {
            return Err(anyhow!(
                "dynamic script exceeds source limit ({} > {} bytes)",
                source.len(),
                self.inner.policy.max_source_bytes
            ));
        }
        let module = self.lower_cached_source(source, specifier, CompileMode::ClassicScript)?;
        let bindings = module
            .imports
            .iter()
            .map(|import| (import.local, resolve_global(&import.imported)))
            .collect();
        let result = Vm::new(module, self.inner.policy.limits)
            .map_err(|error| anyhow!(error.to_string()))?
            .run_with_bindings(bindings)
            .map_err(|error| anyhow!(error.to_string()))?;
        crate::jsdom::drain_microtasks();
        Ok(result)
    }

    pub fn load_and_execute(&self, url: &str) -> Result<Value> {
        if script_execution_route(url) == ScriptExecutionRoute::PrecompiledAot {
            return self.execute_precompiled_aot(url);
        }
        let source = self.load_source(url)?;
        self.execute_source(&source, url)
    }

    fn execute_precompiled_aot(&self, specifier: &str) -> Result<Value> {
        let evaluation = precompiled_aot_evaluation(specifier)?;
        self.settle_module_evaluation(evaluation, specifier)
    }

    fn lower_cached_source(
        &self,
        source: &str,
        resolved_url: &str,
        compile_mode: CompileMode,
    ) -> Result<w3cos_ir::Module> {
        let key = CompiledSourceCacheKey {
            resolved_url: resolved_url.to_string(),
            source_hash: stable_source_hash(source.as_bytes()),
            source_len: source.len(),
            w3ir_format_version: w3cos_ir::FORMAT_VERSION,
            compile_mode,
        };
        let cached_module = {
            let mut cache = self.inner.compiled_source_cache.borrow_mut();
            cache.clock = cache.clock.saturating_add(1);
            let clock = cache.clock;
            let module = cache
                .entries
                .get_mut(&key)
                .filter(|entry| entry.source == source)
                .map(|entry| {
                    entry.last_used = clock;
                    entry.module.clone()
                });
            if module.is_some() {
                cache.hits = cache.hits.saturating_add(1);
            } else {
                cache.misses = cache.misses.saturating_add(1);
            }
            module
        };
        if let Some(module) = cached_module {
            return Ok(module);
        }
        if let Some(module) = self.load_persistent_compiled_source(&key, source) {
            self.cache_compiled_source(key, source, &module);
            return Ok(module);
        }

        let module = match compile_mode {
            CompileMode::ClassicScript => {
                w3cos_compiler::w3ir_lowering::lower_script(source, resolved_url)?
            }
            CompileMode::Module => {
                w3cos_compiler::w3ir_lowering::lower_module(source, resolved_url)?
            }
        };
        self.cache_compiled_source(key.clone(), source, &module);
        self.store_persistent_compiled_source(&key, source, &module);
        Ok(module)
    }

    fn load_persistent_compiled_source(
        &self,
        key: &CompiledSourceCacheKey,
        source: &str,
    ) -> Option<w3cos_ir::Module> {
        let path = self.persistent_compiled_cache_path(key)?;
        if self.inner.policy.max_compiled_cache_entries == 0
            || self.inner.policy.max_compiled_cache_bytes == 0
        {
            return None;
        }
        let result = (|| {
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return PersistentCacheLookup::Miss;
                }
                Err(_) => return PersistentCacheLookup::Error,
            };
            if metadata.len() > self.inner.policy.max_compiled_cache_bytes as u64 {
                return PersistentCacheLookup::Error;
            }
            let encoded = match std::fs::read(&path) {
                Ok(encoded) => encoded,
                Err(_) => return PersistentCacheLookup::Error,
            };
            let artifact: PersistentCompiledSource = match serde_json::from_slice(&encoded) {
                Ok(artifact) => artifact,
                Err(_) => return PersistentCacheLookup::Error,
            };
            if artifact.schema_version != PERSISTENT_COMPILED_CACHE_SCHEMA_VERSION
                || artifact.resolved_url != key.resolved_url
                || artifact.w3ir_format_version != key.w3ir_format_version
                || artifact.compile_mode != key.compile_mode
            {
                return PersistentCacheLookup::Miss;
            }
            if artifact.source_hash != key.source_hash
                || artifact.source_len != key.source_len
                || artifact.source != source
            {
                return PersistentCacheLookup::Miss;
            }
            if artifact.module.specifier != key.resolved_url || artifact.module.validate().is_err()
            {
                return PersistentCacheLookup::Error;
            }
            PersistentCacheLookup::Hit(artifact.module)
        })();
        let mut cache = self.inner.compiled_source_cache.borrow_mut();
        match result {
            PersistentCacheLookup::Hit(module) => {
                cache.persistent_hits = cache.persistent_hits.saturating_add(1);
                Some(module)
            }
            PersistentCacheLookup::Miss => {
                cache.persistent_misses = cache.persistent_misses.saturating_add(1);
                None
            }
            PersistentCacheLookup::Error => {
                cache.persistent_misses = cache.persistent_misses.saturating_add(1);
                cache.persistent_errors = cache.persistent_errors.saturating_add(1);
                None
            }
        }
    }

    fn store_persistent_compiled_source(
        &self,
        key: &CompiledSourceCacheKey,
        source: &str,
        module: &w3cos_ir::Module,
    ) {
        let Some(path) = self.persistent_compiled_cache_path(key) else {
            return;
        };
        let result = (|| -> std::io::Result<Option<(u64, u64)>> {
            if self.inner.policy.max_compiled_cache_entries == 0
                || self.inner.policy.max_compiled_cache_bytes == 0
            {
                return Ok(None);
            }
            let artifact = PersistentCompiledSource {
                schema_version: PERSISTENT_COMPILED_CACHE_SCHEMA_VERSION,
                resolved_url: key.resolved_url.clone(),
                source_hash: key.source_hash,
                source_len: key.source_len,
                w3ir_format_version: key.w3ir_format_version,
                compile_mode: key.compile_mode,
                source: source.to_string(),
                module: module.clone(),
            };
            let encoded = serde_json::to_vec(&artifact).map_err(std::io::Error::other)?;
            if encoded.len() > self.inner.policy.max_compiled_cache_bytes {
                return Ok(None);
            }
            let Some(directory) = path.parent() else {
                return Ok(None);
            };
            std::fs::create_dir_all(directory)?;
            let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
            temporary.write_all(&encoded)?;
            temporary.flush()?;
            temporary.persist(&path).map_err(|error| error.error)?;
            let evictions = self.prune_persistent_compiled_cache(directory)?;
            Ok(Some(evictions))
        })();
        let mut cache = self.inner.compiled_source_cache.borrow_mut();
        let http_evictions = match result {
            Ok(Some((compiled_evictions, http_evictions))) => {
                cache.persistent_writes = cache.persistent_writes.saturating_add(1);
                cache.persistent_evictions = cache
                    .persistent_evictions
                    .saturating_add(compiled_evictions);
                http_evictions
            }
            Ok(None) => 0,
            Err(_) => {
                cache.persistent_errors = cache.persistent_errors.saturating_add(1);
                0
            }
        };
        drop(cache);
        if http_evictions > 0 {
            let mut stats = self.inner.http_source_cache_stats.borrow_mut();
            stats.evictions = stats.evictions.saturating_add(http_evictions);
        }
    }

    fn persistent_compiled_cache_path(&self, key: &CompiledSourceCacheKey) -> Option<PathBuf> {
        let directory = self.inner.policy.compiled_cache_dir.as_ref()?;
        let identity = persistent_compiled_cache_identity(key);
        Some(directory.join(format!("{identity:016x}.w3ir.json")))
    }

    fn prune_persistent_compiled_cache(&self, directory: &Path) -> std::io::Result<(u64, u64)> {
        let mut files = std::fs::read_dir(directory)?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.ends_with(".w3ir.json") || name.ends_with(".response.json")
                    })
                    .then(|| {
                        let metadata = entry.metadata().ok()?;
                        Some((
                            path,
                            metadata.len(),
                            metadata
                                .modified()
                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                        ))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| {
            left.2
                .cmp(&right.2)
                .then_with(|| left.0.as_os_str().cmp(right.0.as_os_str()))
        });
        let mut bytes = files
            .iter()
            .fold(0_u64, |total, (_, size, _)| total.saturating_add(*size));
        let max_entries = self.inner.policy.max_compiled_cache_entries;
        let max_bytes = self.inner.policy.max_compiled_cache_bytes as u64;
        let mut entries = files.len();
        let mut compiled_evictions = 0_u64;
        let mut http_evictions = 0_u64;
        for (path, size, _) in files {
            if entries <= max_entries && bytes <= max_bytes {
                break;
            }
            let is_http_source = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".response.json"));
            std::fs::remove_file(path)?;
            entries = entries.saturating_sub(1);
            bytes = bytes.saturating_sub(size);
            if is_http_source {
                http_evictions = http_evictions.saturating_add(1);
            } else {
                compiled_evictions = compiled_evictions.saturating_add(1);
            }
        }
        Ok((compiled_evictions, http_evictions))
    }

    fn cache_compiled_source(
        &self,
        key: CompiledSourceCacheKey,
        source: &str,
        module: &w3cos_ir::Module,
    ) {
        let max_entries = self.inner.policy.max_compiled_cache_entries;
        let max_bytes = self.inner.policy.max_compiled_cache_bytes;
        if max_entries == 0 || max_bytes == 0 {
            return;
        }
        let resident_bytes = key
            .resolved_url
            .len()
            .saturating_add(source.len())
            .saturating_add(
                serde_json::to_vec(module)
                    .map(|encoded| encoded.len())
                    .unwrap_or_default(),
            );
        if resident_bytes > max_bytes {
            return;
        }

        let mut cache = self.inner.compiled_source_cache.borrow_mut();
        let last_used = cache.clock;
        let replaced = cache.entries.insert(
            key,
            CompiledSourceCacheEntry {
                source: source.to_string(),
                module: module.clone(),
                resident_bytes,
                last_used,
            },
        );
        if let Some(replaced) = replaced {
            cache.resident_bytes = cache.resident_bytes.saturating_sub(replaced.resident_bytes);
        }
        cache.resident_bytes = cache.resident_bytes.saturating_add(resident_bytes);

        while cache.entries.len() > max_entries || cache.resident_bytes > max_bytes {
            let Some(lru_key) = cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = cache.entries.remove(&lru_key) {
                cache.resident_bytes = cache.resident_bytes.saturating_sub(evicted.resident_bytes);
                cache.evictions = cache.evictions.saturating_add(1);
            }
        }
    }

    /// Returns a snapshot of the loader's compiled W3IR cache counters.
    pub fn compiled_source_cache_stats(&self) -> CompiledSourceCacheStats {
        let cache = self.inner.compiled_source_cache.borrow();
        CompiledSourceCacheStats {
            entries: cache.entries.len(),
            resident_bytes: cache.resident_bytes,
            hits: cache.hits,
            misses: cache.misses,
            evictions: cache.evictions,
            persistent_hits: cache.persistent_hits,
            persistent_misses: cache.persistent_misses,
            persistent_writes: cache.persistent_writes,
            persistent_evictions: cache.persistent_evictions,
            persistent_errors: cache.persistent_errors,
        }
    }

    /// Returns a snapshot of persistent HTTP source revalidation counters.
    pub fn http_source_cache_stats(&self) -> HttpSourceCacheStats {
        let stats = self.inner.http_source_cache_stats.borrow();
        HttpSourceCacheStats {
            candidates: stats.candidates,
            misses: stats.misses,
            not_modified: stats.not_modified,
            refreshed: stats.refreshed,
            writes: stats.writes,
            evictions: stats.evictions,
            errors: stats.errors,
        }
    }

    /// Returns a snapshot of fetch retry scheduling and outcome counters.
    pub fn script_retry_stats(&self) -> ScriptRetryStats {
        let stats = self.inner.script_retry_stats.borrow();
        ScriptRetryStats {
            scheduled: stats.scheduled,
            started: stats.started,
            succeeded: stats.succeeded,
            exhausted: stats.exhausted,
            cancelled: stats.cancelled,
        }
    }

    fn prepare_http_revalidation(
        &self,
        request_url: &str,
        fetch_mode: ScriptFetchMode,
    ) -> (crate::fetch::FetchOptions, Option<PersistentHttpSource>) {
        let mut options = crate::fetch::FetchOptions::default();
        let Some(cached) = self.load_persistent_http_source(request_url, fetch_mode) else {
            return (options, None);
        };
        let same_redirect_origin = Url::parse(request_url)
            .ok()
            .zip(Url::parse(&cached.final_url).ok())
            .is_some_and(|(request, final_url)| request.origin() == final_url.origin());
        if !same_redirect_origin {
            let mut stats = self.inner.http_source_cache_stats.borrow_mut();
            stats.misses = stats.misses.saturating_add(1);
            return (options, None);
        }
        crate::browser_http_cache::add_revalidation_headers(&mut options.headers, &cached);
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        stats.candidates = stats.candidates.saturating_add(1);
        drop(stats);
        (options, Some(cached))
    }

    fn load_persistent_http_source(
        &self,
        request_url: &str,
        fetch_mode: ScriptFetchMode,
    ) -> Option<PersistentHttpSource> {
        let policy = self.browser_http_cache_policy();
        let key = self.browser_http_cache_key(request_url, fetch_mode);
        let result = crate::browser_http_cache::load(&policy, &key, &HashMap::new());
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        match result {
            Ok(Some(cached)) => Some(cached),
            Ok(None) => {
                stats.misses = stats.misses.saturating_add(1);
                None
            }
            Err(_) => {
                stats.misses = stats.misses.saturating_add(1);
                stats.errors = stats.errors.saturating_add(1);
                None
            }
        }
    }

    fn store_persistent_http_source(
        &self,
        request_url: &str,
        fetch_mode: ScriptFetchMode,
        response: &crate::fetch::FetchTextResponse,
    ) {
        let policy = self.browser_http_cache_policy();
        let key = self.browser_http_cache_key(request_url, fetch_mode);
        let cached = crate::browser_http_cache::CachedResponse::from_network(
            &key,
            response.url.clone(),
            response.status,
            response.status_text.clone(),
            response.headers.clone(),
            response.body.as_bytes().to_vec(),
        );
        let result =
            crate::browser_http_cache::store(&policy, &key, &HashMap::new(), cached, false);
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        let compiled_evictions = match result {
            Ok(outcome) if outcome.wrote => {
                stats.writes = stats.writes.saturating_add(1);
                stats.evictions = stats.evictions.saturating_add(outcome.response_evictions);
                outcome.other_evictions
            }
            Ok(_) => 0,
            Err(_) => {
                stats.errors = stats.errors.saturating_add(1);
                0
            }
        };
        drop(stats);
        if compiled_evictions > 0 {
            let mut cache = self.inner.compiled_source_cache.borrow_mut();
            cache.persistent_evictions = cache
                .persistent_evictions
                .saturating_add(compiled_evictions);
        }
    }

    fn persistent_http_source_path(
        &self,
        request_url: &str,
        fetch_mode: ScriptFetchMode,
    ) -> Option<PathBuf> {
        crate::browser_http_cache::cache_path(
            &self.browser_http_cache_policy(),
            &self.browser_http_cache_key(request_url, fetch_mode),
        )
    }

    fn apply_http_revalidation(
        &self,
        response: crate::fetch::FetchTextResponse,
        cached: Option<PersistentHttpSource>,
    ) -> Result<crate::fetch::FetchTextResponse> {
        if response.status != 304 {
            if response.ok && cached.is_some() {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.refreshed = stats.refreshed.saturating_add(1);
            }
            return Ok(response);
        }
        let cached = cached.ok_or_else(|| {
            anyhow!("received HTTP 304 without a matching persistent subresource response")
        })?;
        let cached =
            crate::browser_http_cache::merge_not_modified(cached, &response.url, &response.headers)
                .map_err(anyhow::Error::from)?;
        let body = String::from_utf8(cached.body)
            .map_err(|error| anyhow!("cached subresource response is not UTF-8: {error}"))?;
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        stats.not_modified = stats.not_modified.saturating_add(1);
        Ok(crate::fetch::FetchTextResponse {
            status: cached.status,
            ok: true,
            status_text: cached.status_text,
            headers: cached.headers,
            url: response.url,
            redirected: response.redirected,
            set_cookies: response.set_cookies,
            body,
        })
    }

    fn apply_binary_http_revalidation(
        &self,
        response: crate::fetch::FetchBinaryResponse,
        cached: Option<PersistentHttpSource>,
    ) -> Result<crate::fetch::FetchBinaryResponse> {
        if response.status != 304 {
            if response.ok && cached.is_some() {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.refreshed = stats.refreshed.saturating_add(1);
            }
            return Ok(response);
        }
        let cached = cached.ok_or_else(|| {
            anyhow!("received HTTP 304 without a matching persistent font response")
        })?;
        let cached =
            crate::browser_http_cache::merge_not_modified(cached, &response.url, &response.headers)
                .map_err(anyhow::Error::from)?;
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        stats.not_modified = stats.not_modified.saturating_add(1);
        Ok(crate::fetch::FetchBinaryResponse {
            status: cached.status,
            ok: true,
            status_text: cached.status_text,
            headers: cached.headers,
            url: response.url,
            redirected: response.redirected,
            set_cookies: response.set_cookies,
            body: cached.body,
        })
    }

    fn browser_http_cache_policy(&self) -> crate::browser_http_cache::CachePolicy {
        crate::browser_http_cache::CachePolicy {
            directory: self.inner.policy.compiled_cache_dir.clone(),
            max_entries: self.inner.policy.max_compiled_cache_entries,
            max_bytes: self.inner.policy.max_compiled_cache_bytes,
            max_body_bytes: self.inner.policy.max_source_bytes,
        }
    }

    fn browser_http_cache_key(
        &self,
        request_url: &str,
        fetch_mode: ScriptFetchMode,
    ) -> crate::browser_http_cache::CacheKey {
        crate::browser_http_cache::CacheKey {
            request_url: request_url.to_string(),
            partition: script_http_cache_partition(fetch_mode),
        }
    }

    /// Registers an already available module body in the shared source cache.
    /// HTML inline modules, packaged resources, and tests use the same module
    /// graph linker as fetched modules.
    pub fn register_module_source(&self, url: &str, source: &str) -> Result<()> {
        let canonical = canonical_module_url(url)?;
        self.check_source_size(source)?;
        self.inner
            .source_cache
            .borrow_mut()
            .insert(canonical, source.to_string());
        Ok(())
    }

    /// Installs exact and trailing-slash import-map entries. Targets are
    /// canonicalized against the document URL before module instantiation.
    pub fn set_import_map(
        &self,
        document_url: &str,
        imports: HashMap<String, String>,
    ) -> Result<()> {
        self.set_scoped_import_map(document_url, imports, HashMap::new())
    }

    /// Installs global and scoped import-map entries. Scope prefixes,
    /// URL-like keys, and targets are canonicalized against the document URL.
    pub fn set_scoped_import_map(
        &self,
        document_url: &str,
        imports: HashMap<String, String>,
        scopes: HashMap<String, HashMap<String, String>>,
    ) -> Result<()> {
        let imports = import_addresses(imports);
        let scopes = scopes
            .into_iter()
            .map(|(scope, entries)| (scope, import_addresses(entries)))
            .collect();
        let import_map = self.normalize_import_map(document_url, imports, scopes)?;
        if self.inner.module_resolution_started.get() {
            self.merge_import_map(import_map);
        } else {
            *self.inner.import_map.borrow_mut() = import_map;
        }
        Ok(())
    }

    fn normalize_import_map(
        &self,
        document_url: &str,
        imports: ImportEntries,
        scopes: HashMap<String, ImportEntries>,
    ) -> Result<ImportMapState> {
        let base = Url::parse(document_url)
            .map_err(|error| anyhow!("invalid import-map base URL {document_url}: {error}"))?;
        let imports = normalize_import_entries(&base, imports)?;
        let mut resolved_scopes = Vec::with_capacity(scopes.len());
        for (scope, entries) in scopes {
            let scope_url = base
                .join(&scope)
                .map_err(|error| anyhow!("invalid import-map scope {scope:?}: {error}"))?
                .to_string();
            resolved_scopes.push((scope_url, normalize_import_entries(&base, entries)?));
        }
        resolved_scopes.sort_by(|left, right| {
            right
                .0
                .len()
                .cmp(&left.0.len())
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(ImportMapState {
            imports,
            scopes: resolved_scopes,
        })
    }

    fn merge_import_map(&self, incoming: ImportMapState) {
        let mut installed = self.inner.import_map.borrow_mut();
        for (specifier, target) in incoming.imports {
            if installed.imports.contains_key(&specifier) {
                continue;
            }
            let mut candidate = installed.clone();
            candidate.imports.insert(specifier.clone(), target.clone());
            if self.import_map_preserves_resolved_requests(&candidate) {
                installed.imports.insert(specifier, target);
            }
        }
        for (scope, entries) in incoming.scopes {
            for (specifier, target) in entries {
                let already_installed = installed
                    .scopes
                    .iter()
                    .find(|(installed_scope, _)| installed_scope == &scope)
                    .is_some_and(|(_, entries)| entries.contains_key(&specifier));
                if already_installed {
                    continue;
                }
                let mut candidate = installed.clone();
                if let Some((_, candidate_entries)) = candidate
                    .scopes
                    .iter_mut()
                    .find(|(installed_scope, _)| installed_scope == &scope)
                {
                    candidate_entries.insert(specifier.clone(), target.clone());
                } else {
                    candidate.scopes.push((
                        scope.clone(),
                        HashMap::from([(specifier.clone(), target.clone())]),
                    ));
                }
                sort_import_map_scopes(&mut candidate.scopes);
                if !self.import_map_preserves_resolved_requests(&candidate) {
                    continue;
                }
                if let Some((_, installed_entries)) = installed
                    .scopes
                    .iter_mut()
                    .find(|(installed_scope, _)| installed_scope == &scope)
                {
                    installed_entries.insert(specifier, target);
                } else {
                    installed
                        .scopes
                        .push((scope.clone(), HashMap::from([(specifier, target)])));
                }
            }
        }
        sort_import_map_scopes(&mut installed.scopes);
    }

    fn import_map_preserves_resolved_requests(&self, candidate: &ImportMapState) -> bool {
        self.inner
            .resolved_module_requests
            .borrow()
            .iter()
            .all(|resolved| {
                resolve_module_url_with_map(&resolved.base, &resolved.request, candidate)
                    .is_ok_and(|candidate_url| candidate_url == resolved.resolved)
            })
    }

    /// Compiles, links, and evaluates an inline/embedded ESM graph through
    /// SWC → W3IR → W3VM. The returned namespace has live export getters.
    pub fn execute_module_source(&self, source: &str, specifier: &str) -> Result<Value> {
        let evaluation = self.execute_module_source_async(source, specifier);
        self.settle_module_evaluation(evaluation, specifier)
    }

    /// Promise-returning module entry used by browser script lifecycle.
    /// Parsing/linking failures become rejections and top-level await remains
    /// pending until its adopted Promise settles.
    pub fn execute_module_source_async(&self, source: &str, specifier: &str) -> Value {
        if script_execution_route(specifier) == ScriptExecutionRoute::PrecompiledAot {
            return precompiled_aot_evaluation(specifier).unwrap_or_else(|error| {
                w3cos_core::promise::reject(vec![Value::string(&error.to_string())])
            });
        }
        if let Err(error) = self.register_module_source(specifier, source) {
            return w3cos_core::promise::reject(vec![Value::string(&error.to_string())]);
        }
        self.load_and_execute_module_async_guarded(
            specifier,
            ModuleCredentialsMode::SameOrigin,
            None,
            String::new(),
            crate::fetch::ScriptReferrerPolicy::default(),
            None,
        )
    }

    /// Fetches (when not cached), links, and evaluates an ESM graph once.
    pub fn load_and_execute_module(&self, url: &str) -> Result<Value> {
        let evaluation = self.load_and_execute_module_async(url);
        self.settle_module_evaluation(evaluation, url)
    }

    /// Synchronous embedding adapter for an explicitly credentialed module
    /// graph. Browser task-pump fetching and W3IR/W3VM evaluation remain shared.
    pub fn load_and_execute_module_with_credentials(
        &self,
        url: &str,
        credentials_mode: ModuleCredentialsMode,
    ) -> Result<Value> {
        let evaluation = self.load_and_execute_module_async_with_credentials(url, credentials_mode);
        self.settle_module_evaluation(evaluation, url)
    }

    /// Fetches the complete dependency graph without blocking the browser
    /// task pump, then links and evaluates it through the same W3IR/W3VM path
    /// used by embedded and packaged modules.
    pub fn load_and_execute_module_async(&self, url: &str) -> Value {
        self.load_and_execute_module_async_with_credentials(url, ModuleCredentialsMode::SameOrigin)
    }

    /// Fetches and evaluates a module graph using an explicit Fetch credentials
    /// mode while retaining the same URL-keyed module map and W3IR/W3VM path.
    pub fn load_and_execute_module_async_with_credentials(
        &self,
        url: &str,
        credentials_mode: ModuleCredentialsMode,
    ) -> Value {
        self.load_and_execute_module_async_guarded(
            url,
            credentials_mode,
            None,
            String::new(),
            crate::fetch::ScriptReferrerPolicy::default(),
            None,
        )
    }

    fn load_and_execute_module_async_guarded(
        &self,
        url: &str,
        credentials_mode: ModuleCredentialsMode,
        element_guard: Option<(u32, Rc<Cell<bool>>)>,
        integrity: String,
        referrer_policy: crate::fetch::ScriptReferrerPolicy,
        referrer_source: Option<String>,
    ) -> Value {
        let canonical = match canonical_module_url(url) {
            Ok(canonical) => canonical,
            Err(error) => {
                return w3cos_core::promise::reject(vec![Value::string(&error.to_string())]);
            }
        };
        if script_execution_route(&canonical) == ScriptExecutionRoute::PrecompiledAot {
            return precompiled_aot_evaluation(&canonical).unwrap_or_else(|error| {
                w3cos_core::promise::reject(vec![Value::string(&error.to_string())])
            });
        }
        let element_consumer = element_guard.as_ref().map(|(node, _)| *node);
        let cancellation = element_guard.map(|(_, cancellation)| cancellation);
        let (graph, credentials_mode) = self.ensure_module_graph_async(
            &canonical,
            credentials_mode,
            element_consumer,
            integrity,
            referrer_policy,
            referrer_source,
        );
        self.evaluate_prepared_module_graph_async(
            canonical,
            graph,
            credentials_mode,
            referrer_policy,
            cancellation,
        )
    }

    fn evaluate_prepared_module_graph_async(
        &self,
        module_url: String,
        graph: Value,
        credentials_mode: ModuleCredentialsMode,
        referrer_policy: crate::fetch::ScriptReferrerPolicy,
        cancellation: Option<Rc<Cell<bool>>>,
    ) -> Value {
        let weak_loader = Rc::downgrade(&self.inner);
        let instantiate = Value::function(move |_, _| {
            if cancellation
                .as_ref()
                .is_some_and(|cancellation| cancellation.get())
            {
                return w3cos_core::promise::reject(vec![Value::string(
                    "script element was removed before module evaluation",
                )]);
            }
            let Some(inner) = weak_loader.upgrade() else {
                return w3cos_core::promise::reject(vec![Value::string(
                    "dynamic module loader was cancelled",
                )]);
            };
            let loader = ScriptLoader { inner };
            match loader
                .instantiate_module(&module_url, credentials_mode, referrer_policy)
                .and_then(|record| loader.evaluate_module(&record))
            {
                Ok(evaluation) => evaluation,
                Err(error) => w3cos_core::promise::reject(vec![Value::string(&error.to_string())]),
            }
        });
        graph.call_method("then", vec![instantiate])
    }

    fn ensure_module_graph_async(
        &self,
        root: &str,
        credentials_mode: ModuleCredentialsMode,
        element_consumer: Option<u32>,
        integrity: String,
        referrer_policy: crate::fetch::ScriptReferrerPolicy,
        referrer_source: Option<String>,
    ) -> (Value, ModuleCredentialsMode) {
        self.inner.module_resolution_started.set(true);
        if self.inner.module_request_origin.borrow().is_none()
            && let Ok(root_url) = Url::parse(root)
        {
            *self.inner.module_request_origin.borrow_mut() =
                Some(root_url.origin().ascii_serialization());
        }
        let referrer_source = referrer_source
            .or_else(|| self.inner.document_url.borrow().clone())
            .unwrap_or_else(|| root.to_string());
        let graph_key = module_graph_key(root, &integrity, referrer_policy, &referrer_source);
        if let Some(load) = self.inner.module_graph_loads.borrow().get(&graph_key) {
            let mut load = load.borrow_mut();
            if let Some(node) = element_consumer {
                load.element_consumers.insert(node);
            } else {
                load.uncancellable_consumers = load.uncancellable_consumers.saturating_add(1);
            }
            return (load.promise.clone(), load.credentials_mode);
        }
        let (promise, resolve, reject) = deferred_promise();
        let load = Rc::new(RefCell::new(ModuleGraphLoad {
            root: root.to_string(),
            integrity,
            referrer_policy,
            promise: promise.clone(),
            resolve,
            reject,
            scheduled: HashSet::new(),
            pending: HashSet::new(),
            credentials_mode,
            element_consumers: element_consumer.into_iter().collect(),
            uncancellable_consumers: u32::from(element_consumer.is_none()),
            settled: false,
        }));
        self.inner
            .module_graph_loads
            .borrow_mut()
            .insert(graph_key, Rc::clone(&load));
        if let Err(error) = self.schedule_graph_url(&load, root, &referrer_source) {
            reject_graph_load(&load, &error.to_string());
        } else {
            finish_graph_load(self, &load);
        }
        (promise, credentials_mode)
    }

    fn schedule_graph_url(
        &self,
        load: &Rc<RefCell<ModuleGraphLoad>>,
        url: &str,
        referrer_source: &str,
    ) -> Result<()> {
        if !load.borrow_mut().scheduled.insert(url.to_string()) {
            return Ok(());
        }
        if script_execution_route(url) == ScriptExecutionRoute::PrecompiledAot {
            resolve_precompiled_aot_specifier(url)?;
            return Ok(());
        }
        if self.inner.source_cache.borrow().contains_key(url) {
            return self.discover_graph_dependencies(load, url);
        }
        self.validate_network_url(url)?;
        let (credentials_mode, referrer_policy) = {
            let load = load.borrow();
            (load.credentials_mode, load.referrer_policy)
        };
        let fetch_key = module_fetch_key(url, referrer_source, referrer_policy);
        load.borrow_mut().pending.insert(fetch_key.clone());
        let (options, cached_response) =
            self.prepare_http_revalidation(url, ScriptFetchMode::Module(credentials_mode));
        let mut fetches = self.inner.pending_source_fetches.borrow_mut();
        if !fetches.contains_key(&fetch_key) {
            let request_origin = self.inner.module_request_origin.borrow().clone();
            let task = start_script_fetch(
                url,
                options.clone(),
                request_origin.as_deref(),
                credentials_mode,
                true,
                referrer_source,
                referrer_policy,
            );
            fetches.insert(
                fetch_key,
                PendingSourceFetch {
                    url: url.to_string(),
                    task: Some(task),
                    cached_response,
                    options,
                    request_origin,
                    referrer_source: referrer_source.to_string(),
                    referrer_policy,
                    credentials_mode,
                    attempts_started: 1,
                    retry_at: None,
                },
            );
        }
        Ok(())
    }

    fn discover_graph_dependencies(
        &self,
        load: &Rc<RefCell<ModuleGraphLoad>>,
        url: &str,
    ) -> Result<()> {
        let source = self
            .inner
            .source_cache
            .borrow()
            .get(url)
            .cloned()
            .ok_or_else(|| anyhow!("module source was not fetched: {url}"))?;
        let module_url = self
            .inner
            .module_final_urls
            .borrow()
            .get(url)
            .cloned()
            .unwrap_or_else(|| url.to_string());
        let module = self.lower_cached_source(&source, &module_url, CompileMode::Module)?;
        for request in &module.requested_modules {
            let dependency_url = self.resolve_module_url(&module_url, request)?;
            if w3cos_core::module_registry::contains_native(&dependency_url)
                && !self
                    .inner
                    .module_records
                    .borrow()
                    .contains_key(&dependency_url)
            {
                continue;
            }
            self.schedule_graph_url(load, &dependency_url, &module_url)?;
        }
        Ok(())
    }

    fn validate_network_url(&self, url: &str) -> Result<()> {
        if !self.inner.policy.allow_network {
            return Err(anyhow!("network script loading is disabled by policy"));
        }
        let parsed =
            Url::parse(url).map_err(|error| anyhow!("invalid dynamic script URL: {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(anyhow!("dynamic script URL must use http or https"));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(anyhow!("dynamic script URL must not include credentials"));
        }
        Ok(())
    }

    fn validate_module_cors_response(
        &self,
        url: &str,
        response: &crate::fetch::FetchTextResponse,
        credentials_mode: ModuleCredentialsMode,
    ) -> Result<()> {
        let Some(request_origin) = self.inner.module_request_origin.borrow().clone() else {
            return Ok(());
        };
        validate_script_cors_headers(
            &request_origin,
            &response.url,
            &response.headers,
            credentials_mode,
        )
        .map_err(|detail| {
            let prefix = if credentials_mode == ModuleCredentialsMode::Include {
                "credentialed module CORS check failed"
            } else {
                "module CORS check failed"
            };
            anyhow!(
                "{prefix} for {url} after redirect to {}: {detail}",
                response.url
            )
        })
    }

    fn validate_classic_cors_response(
        &self,
        url: &str,
        response: &crate::fetch::FetchTextResponse,
        fetch_mode: ClassicScriptFetchMode,
    ) -> Result<()> {
        if fetch_mode == ClassicScriptFetchMode::NoCors {
            return self.validate_classic_corp_response(url, response);
        }
        self.validate_module_cors_response(url, response, classic_credentials_mode(fetch_mode))
            .map_err(|error| {
                anyhow!(
                    error
                        .to_string()
                        .replace("module CORS", "classic script CORS")
                )
            })
    }

    fn validate_classic_corp_response(
        &self,
        url: &str,
        response: &crate::fetch::FetchTextResponse,
    ) -> Result<()> {
        let Some(request_origin) = self.inner.module_request_origin.borrow().clone() else {
            return Ok(());
        };
        let policy = header_value(&response.headers, "cross-origin-resource-policy")
            .map(str::trim)
            .filter(|value| matches!(*value, "same-origin" | "same-site" | "cross-origin"));
        let allowed = match policy {
            None | Some("cross-origin") => true,
            Some("same-origin") => Url::parse(&response.url)
                .map(|response_url| response_url.origin().ascii_serialization() == request_origin)
                .unwrap_or(false),
            Some("same-site") => {
                crate::cookie_store_web::urls_are_corp_same_site(&request_origin, &response.url)
            }
            Some(_) => unreachable!("CORP policy was filtered to recognized values"),
        };
        if allowed {
            return Ok(());
        }
        Err(anyhow!(
            "classic script CORP check failed for {url} after redirect to {}: policy {policy:?} blocks initiator {request_origin}",
            response.url
        ))
    }

    fn validate_classic_mime_response(
        &self,
        response: &crate::fetch::FetchTextResponse,
    ) -> Result<()> {
        let nosniff =
            header_value(&response.headers, "x-content-type-options").is_some_and(|value| {
                value
                    .split(',')
                    .next()
                    .is_some_and(|token| token.trim().eq_ignore_ascii_case("nosniff"))
            });
        if !nosniff {
            return Ok(());
        }
        let content_type = header_value(&response.headers, "content-type").unwrap_or_default();
        if is_javascript_mime_type(content_type) {
            return Ok(());
        }
        let essence = content_type.split(';').next().unwrap_or_default().trim();
        let reported = if essence.is_empty() {
            "<missing>"
        } else {
            essence
        };
        Err(anyhow!(
            "classic script MIME check failed for {}: nosniff requires a JavaScript MIME type, received {reported}",
            response.url
        ))
    }

    fn validate_module_mime_response(
        &self,
        response: &crate::fetch::FetchTextResponse,
    ) -> Result<()> {
        let content_type = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        if is_javascript_mime_type(&content_type) {
            return Ok(());
        }
        let reported = if content_type.is_empty() {
            "<missing>"
        } else {
            &content_type
        };
        Err(anyhow!(
            "module MIME check failed for {}: expected a JavaScript MIME type, received {reported}",
            response.url
        ))
    }

    fn store_script_response_cookies(
        &self,
        response: &crate::fetch::FetchTextResponse,
        credentials_mode: ModuleCredentialsMode,
    ) {
        for (url, cookie) in &response.set_cookies {
            self.store_script_cookie(url, cookie, credentials_mode);
        }
    }

    fn store_script_cookie(
        &self,
        url: &str,
        cookie: &str,
        credentials_mode: ModuleCredentialsMode,
    ) {
        if self.script_credentials_allowed(url, credentials_mode) {
            crate::cookie_store_web::set_cookie_assignment_for_url(url, cookie, true);
        }
    }

    fn script_credentials_allowed(
        &self,
        url: &str,
        credentials_mode: ModuleCredentialsMode,
    ) -> bool {
        if credentials_mode == ModuleCredentialsMode::Omit {
            return false;
        }
        if credentials_mode == ModuleCredentialsMode::Include {
            return true;
        }
        let Some(request_origin) = self.inner.module_request_origin.borrow().clone() else {
            return false;
        };
        Url::parse(url)
            .map(|url| url.origin().ascii_serialization() == request_origin)
            .unwrap_or(false)
    }

    fn subscribe_module_element(
        &self,
        element: Value,
        evaluation: Value,
        cancellation: Rc<Cell<bool>>,
    ) {
        let loaded_element = element.clone();
        let fulfilled_cancellation = Rc::clone(&cancellation);
        let fulfilled_loader = Rc::downgrade(&self.inner);
        let on_fulfilled = Value::function(move |_, arguments| {
            if fulfilled_cancellation.get() {
                if let Some(inner) = fulfilled_loader.upgrade() {
                    ScriptLoader { inner }.complete_document_script(&loaded_element);
                }
                return arguments.first().cloned().unwrap_or(Value::Undefined);
            }
            let onload = loaded_element.get_property("onload");
            if !onload.is_null() && !onload.is_undefined() {
                onload.call(loaded_element.clone(), vec![]);
            }
            if let Some(inner) = fulfilled_loader.upgrade() {
                ScriptLoader { inner }.complete_document_script(&loaded_element);
            }
            arguments.first().cloned().unwrap_or(Value::Undefined)
        });
        let failed_element = element;
        let rejected_loader = Rc::downgrade(&self.inner);
        let on_rejected = Value::function(move |_, arguments| {
            let reason = arguments.first().cloned().unwrap_or(Value::Undefined);
            if cancellation.get() {
                if let Some(inner) = rejected_loader.upgrade() {
                    ScriptLoader { inner }.complete_document_script(&failed_element);
                }
                return Value::Undefined;
            }
            let onerror = failed_element.get_property("onerror");
            if !onerror.is_null() && !onerror.is_undefined() {
                onerror.call(failed_element.clone(), vec![reason.clone()]);
            }
            if let Some(inner) = rejected_loader.upgrade() {
                ScriptLoader { inner }.complete_document_script(&failed_element);
            }
            Value::Undefined
        });
        evaluation.call_method("then", vec![on_fulfilled, on_rejected]);
    }

    fn module_element_cancellation(&self, element: &Value) -> (u32, Rc<Cell<bool>>) {
        let cancellation = Rc::new(Cell::new(false));
        let node = crate::jsdom::node_id_of(element)
            .expect("document script elements always retain a DOM node identity");
        self.inner
            .module_element_cancellations
            .borrow_mut()
            .insert(node, Rc::clone(&cancellation));
        (node, cancellation)
    }

    fn module_element_credentials_mode(element: &Value) -> ModuleCredentialsMode {
        let cross_origin = element.call_method("getAttribute", vec![Value::string("crossorigin")]);
        if !cross_origin.is_null()
            && cross_origin
                .to_js_string()
                .eq_ignore_ascii_case("use-credentials")
        {
            ModuleCredentialsMode::Include
        } else {
            ModuleCredentialsMode::SameOrigin
        }
    }

    fn settle_module_evaluation(&self, evaluation: Value, specifier: &str) -> Result<Value> {
        loop {
            crate::jsdom::drain_microtasks();
            if !matches!(
                w3cos_core::promise::status(&evaluation),
                Some(w3cos_core::promise::PromiseStatus::Pending)
            ) || self.inner.pending_source_fetches.borrow().is_empty()
            {
                break;
            }
            let now = std::time::Instant::now();
            if let Some(deadline) = self.next_fetch_deadline()
                && deadline > now
            {
                std::thread::sleep(
                    deadline
                        .duration_since(now)
                        .min(std::time::Duration::from_millis(10)),
                );
            } else {
                std::thread::yield_now();
            }
        }
        match w3cos_core::promise::status(&evaluation) {
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(namespace)) => Ok(namespace),
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => Err(anyhow!(
                "module evaluation failed for {specifier}: {}",
                reason.to_js_string()
            )),
            Some(w3cos_core::promise::PromiseStatus::Pending) => Err(anyhow!(
                "module evaluation is pending for {specifier}; use the asynchronous module API for asynchronously settled top-level await"
            )),
            None => Err(anyhow!(
                "module evaluation did not return a runtime promise: {specifier}"
            )),
        }
    }

    fn next_fetch_deadline(&self) -> Option<std::time::Instant> {
        let now = std::time::Instant::now();
        let active_poll = now + std::time::Duration::from_millis(16);
        let modules = self.inner.pending_source_fetches.borrow();
        let classics = self.inner.pending_classic_fetches.borrow();
        let stylesheets = self.inner.pending_stylesheet_fetches.borrow();
        let fonts = self.inner.pending_stylesheet_font_fetches.borrow();
        let images = self.inner.pending_image_fetches.borrow();
        let background_images = self.inner.pending_background_image_fetches.borrow();
        modules
            .values()
            .map(|fetch| fetch.retry_at.unwrap_or(active_poll))
            .chain(
                classics
                    .values()
                    .map(|fetch| fetch.retry_at.unwrap_or(active_poll)),
            )
            .chain(stylesheets.values().map(|_| active_poll))
            .chain(fonts.values().map(|_| active_poll))
            .chain(images.values().map(|_| active_poll))
            .chain(background_images.values().map(|_| active_poll))
            .min()
    }

    /// Attaches this loader to the live document and executes scripts already
    /// present in it. Later direct `<script>` insertions are scheduled through
    /// the shared browser microtask queue.
    pub fn attach_to_document(&self, document_url: &str) -> Result<usize> {
        self.begin_document_parse(document_url)?;
        let result = self.execute_pending_document_scripts(document_url);
        self.finish_document_parse();
        result
    }

    /// Begin a parser-driven navigation. A streaming HTML parser may call
    /// [`execute_pending_document_scripts`](Self::execute_pending_document_scripts)
    /// after each tree-builder checkpoint, then call
    /// [`finish_document_parse`](Self::finish_document_parse) at EOF.
    pub fn begin_document_parse(&self, document_url: &str) -> Result<()> {
        let document_url = Url::parse(document_url)
            .map_err(|error| anyhow!("invalid document URL {document_url}: {error}"))?;
        // DOM-facing image cache keys may be relative source strings. A
        // navigation creates a new URL-resolution scope, so retain encoded
        // HTTP responses in the partitioned Browser cache but release decoded
        // aliases before the new document can paint.
        crate::image_loader::clear_cache();
        for fetch in self.inner.pending_stylesheet_fetches.borrow().values() {
            fetch.task.cancel();
        }
        for fetch in self.inner.pending_stylesheet_font_fetches.borrow().values() {
            fetch.task.cancel();
            if fetch.graph.font_loading_started {
                crate::font_face::FontFaceSet::global().mark_ready();
                crate::font_loading_web::cancel_font_loading(fetch.graph.font_event_faces.clone());
            }
        }
        for fetch in self.inner.pending_image_fetches.borrow().values() {
            fetch.request.task.cancel();
        }
        for fetch in self
            .inner
            .pending_background_image_fetches
            .borrow()
            .values()
        {
            fetch.request.task.cancel();
        }
        let pending_image_nodes = self
            .inner
            .image_decode_waiters
            .borrow()
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for node in pending_image_nodes {
            self.reject_image_decode_waiters(node, "The document navigation was replaced");
        }
        self.inner.pending_stylesheet_fetches.borrow_mut().clear();
        self.inner
            .pending_stylesheet_font_fetches
            .borrow_mut()
            .clear();
        self.inner.deferred_stylesheet_fonts.borrow_mut().clear();
        self.inner.pending_image_fetches.borrow_mut().clear();
        self.inner.processed_image_nodes.borrow_mut().clear();
        self.inner
            .pending_background_image_fetches
            .borrow_mut()
            .clear();
        self.inner.processed_background_images.borrow_mut().clear();
        self.inner.ready_stylesheets.borrow_mut().clear();
        self.inner.processed_stylesheet_nodes.borrow_mut().clear();
        self.inner.installed_stylesheets.borrow_mut().clear();
        for owner in self.inner.stylesheet_font_owners.borrow().values() {
            crate::font_face::FontRegistry::global().clear_owner(*owner);
            crate::font_loading_web::clear_stylesheet_font_owner(*owner);
        }
        self.inner.stylesheet_font_owners.borrow_mut().clear();
        self.inner.next_stylesheet_order.set(0);
        self.inner.next_stylesheet_apply.set(0);
        DYNAMIC_STYLESHEET_NODES.with(|nodes| nodes.borrow_mut().clear());
        DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow_mut().clear());
        w3cos_dom::stylesheet::clear_owner(self.inner.stylesheet_owner);
        *self.inner.module_request_origin.borrow_mut() =
            Some(document_url.origin().ascii_serialization());
        *self.inner.document_url.borrow_mut() = Some(document_url.to_string());
        self.inner.parser_finished.set(false);
        self.inner.dom_content_loaded_fired.set(false);
        self.inner.document_load_fired.set(false);
        self.inner.document_load_queued.set(false);
        self.inner.document_lifecycle_generation.set(
            self.inner
                .document_lifecycle_generation
                .get()
                .wrapping_add(1),
        );
        self.inner.dom_content_loaded_blockers.borrow_mut().clear();
        self.inner.document_load_blockers.borrow_mut().clear();
        self.inner.parser_blocking_elements.borrow_mut().clear();
        crate::cookie_store_web::set_active_url(document_url.as_str());
        crate::fetch::set_page_http_cache_policy(self.browser_http_cache_policy());
        ACTIVE_DOCUMENT_LOADER.with(|active| {
            *active.borrow_mut() = Some((self.clone(), document_url.to_string()));
        });
        crate::jsdom::set_document_ready_state("loading");
        Ok(())
    }

    /// Mark parser EOF. `DOMContentLoaded` waits for parser-deferred classic
    /// scripts and non-async modules; `load` additionally waits for async
    /// scripts. Both advance from the regular browser task pump.
    pub fn finish_document_parse(&self) {
        if self.inner.parser_finished.replace(true) {
            return;
        }
        let document_url = self.inner.document_url.borrow().clone();
        if let Some(document_url) = document_url {
            self.prepare_pending_stylesheets(&document_url);
            self.prepare_pending_images(&document_url);
        }
        crate::jsdom::set_document_ready_state("interactive");
        self.drain_deferred_classic_scripts();
        self.drain_deferred_parser_modules();
        self.advance_document_lifecycle();
    }

    fn cancel_for_navigation(&self) {
        let graph_loads = self
            .inner
            .module_graph_loads
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for load in graph_loads {
            reject_graph_load(&load, "document navigation cancelled script loading");
        }
        let cancelled = self
            .inner
            .pending_classic_fetches
            .borrow()
            .values()
            .filter(|fetch| fetch.retry_at.is_some() || fetch.attempts_started > 1)
            .count()
            + self
                .inner
                .pending_source_fetches
                .borrow()
                .values()
                .filter(|fetch| fetch.retry_at.is_some() || fetch.attempts_started > 1)
                .count();
        if cancelled > 0 {
            let mut stats = self.inner.script_retry_stats.borrow_mut();
            stats.cancelled = stats.cancelled.saturating_add(cancelled as u64);
        }
        for fetch in self.inner.pending_classic_fetches.borrow().values() {
            if let Some(task) = &fetch.task {
                task.cancel();
            }
        }
        for fetch in self.inner.pending_source_fetches.borrow().values() {
            if let Some(task) = &fetch.task {
                task.cancel();
            }
        }
        for fetch in self.inner.pending_stylesheet_fetches.borrow().values() {
            fetch.task.cancel();
        }
        for fetch in self.inner.pending_stylesheet_font_fetches.borrow().values() {
            fetch.task.cancel();
            if fetch.graph.font_loading_started {
                crate::font_face::FontFaceSet::global().mark_ready();
                crate::font_loading_web::cancel_font_loading(fetch.graph.font_event_faces.clone());
            }
        }
        for fetch in self.inner.pending_image_fetches.borrow().values() {
            fetch.request.task.cancel();
        }
        for fetch in self
            .inner
            .pending_background_image_fetches
            .borrow()
            .values()
        {
            fetch.request.task.cancel();
        }
        let image_waiters = self
            .inner
            .image_decode_waiters
            .borrow()
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for node in image_waiters {
            self.reject_image_decode_waiters(node, "The document navigation was cancelled");
        }
        self.inner.pending_classic_fetches.borrow_mut().clear();
        self.inner
            .ready_ordered_classic_scripts
            .borrow_mut()
            .clear();
        self.inner
            .ready_deferred_classic_scripts
            .borrow_mut()
            .clear();
        self.inner.cancelled_classic_orders.borrow_mut().clear();
        self.inner
            .cancelled_deferred_classic_orders
            .borrow_mut()
            .clear();
        for cancellation in self.inner.module_element_cancellations.borrow().values() {
            cancellation.set(true);
        }
        self.inner.module_element_cancellations.borrow_mut().clear();
        self.inner.pending_source_fetches.borrow_mut().clear();
        self.inner.pending_stylesheet_fetches.borrow_mut().clear();
        self.inner
            .pending_stylesheet_font_fetches
            .borrow_mut()
            .clear();
        self.inner.deferred_stylesheet_fonts.borrow_mut().clear();
        self.inner.pending_image_fetches.borrow_mut().clear();
        self.inner.processed_image_nodes.borrow_mut().clear();
        self.inner
            .pending_background_image_fetches
            .borrow_mut()
            .clear();
        self.inner.processed_background_images.borrow_mut().clear();
        self.inner.ready_stylesheets.borrow_mut().clear();
        self.inner.processed_stylesheet_nodes.borrow_mut().clear();
        self.inner.installed_stylesheets.borrow_mut().clear();
        for owner in self.inner.stylesheet_font_owners.borrow().values() {
            crate::font_face::FontRegistry::global().clear_owner(*owner);
            crate::font_loading_web::clear_stylesheet_font_owner(*owner);
        }
        self.inner.stylesheet_font_owners.borrow_mut().clear();
        self.inner.next_stylesheet_order.set(0);
        self.inner.next_stylesheet_apply.set(0);
        w3cos_dom::stylesheet::clear_owner(self.inner.stylesheet_owner);
        self.inner.module_graph_loads.borrow_mut().clear();
        self.inner.processed_elements.borrow_mut().clear();
        teardown_runtime_module_records(&mut self.inner.module_records.borrow_mut());
        self.inner.module_final_urls.borrow_mut().clear();
        self.inner.source_cache.borrow_mut().clear();
        *self.inner.import_map.borrow_mut() = ImportMapState::default();
        self.inner.module_resolution_started.set(false);
        self.inner.resolved_module_requests.borrow_mut().clear();
        *self.inner.module_request_origin.borrow_mut() = None;
        *self.inner.document_url.borrow_mut() = None;
        self.inner.parser_finished.set(true);
        self.inner.dom_content_loaded_fired.set(true);
        self.inner.document_load_fired.set(true);
        self.inner.document_load_queued.set(false);
        self.inner.document_lifecycle_generation.set(
            self.inner
                .document_lifecycle_generation
                .get()
                .wrapping_add(1),
        );
        self.inner.dom_content_loaded_blockers.borrow_mut().clear();
        self.inner.document_load_blockers.borrow_mut().clear();
        self.inner.parser_blocking_elements.borrow_mut().clear();
        self.inner.deferred_parser_modules.borrow_mut().clear();
        self.inner.next_classic_order.set(0);
        self.inner.next_classic_execution.set(0);
        self.inner.next_deferred_classic_order.set(0);
        self.inner.next_deferred_classic_execution.set(0);
    }

    fn cancel_removed_script_nodes(&self, nodes: &HashSet<u32>) {
        let mut cancelled_retry_chains = 0_u64;
        let mut cancelled_orders = Vec::new();
        {
            let mut fetches = self.inner.pending_classic_fetches.borrow_mut();
            fetches.retain(|_, fetch| {
                fetch.requests.retain(|request| {
                    let removed = crate::jsdom::node_id_of(&request.element)
                        .is_some_and(|node| nodes.contains(&node));
                    if removed && let Some(order) = request.order {
                        cancelled_orders.push((order, request.deferred_until_parse_end));
                    }
                    !removed
                });
                let keep = !fetch.requests.is_empty();
                if !keep && let Some(task) = &fetch.task {
                    task.cancel();
                }
                if !keep && (fetch.retry_at.is_some() || fetch.attempts_started > 1) {
                    cancelled_retry_chains = cancelled_retry_chains.saturating_add(1);
                }
                keep
            });
        }

        self.inner
            .ready_ordered_classic_scripts
            .borrow_mut()
            .retain(|order, (_, request, _)| {
                let removed = crate::jsdom::node_id_of(&request.element)
                    .is_some_and(|node| nodes.contains(&node));
                if removed {
                    cancelled_orders.push((*order, false));
                }
                !removed
            });
        self.inner
            .ready_deferred_classic_scripts
            .borrow_mut()
            .retain(|order, (_, request, _)| {
                let removed = crate::jsdom::node_id_of(&request.element)
                    .is_some_and(|node| nodes.contains(&node));
                if removed {
                    cancelled_orders.push((*order, true));
                }
                !removed
            });
        for (order, deferred) in cancelled_orders {
            if deferred {
                self.inner
                    .cancelled_deferred_classic_orders
                    .borrow_mut()
                    .insert(order);
            } else {
                self.inner
                    .cancelled_classic_orders
                    .borrow_mut()
                    .insert(order);
            }
        }

        let mut cancellations = self.inner.module_element_cancellations.borrow_mut();
        for node in nodes {
            if let Some(cancellation) = cancellations.remove(node) {
                cancellation.set(true);
            }
        }
        drop(cancellations);
        self.inner
            .deferred_parser_modules
            .borrow_mut()
            .retain(|module| {
                crate::jsdom::node_id_of(&module.element).is_none_or(|node| !nodes.contains(&node))
            });
        cancelled_retry_chains =
            cancelled_retry_chains.saturating_add(self.cancel_removed_module_consumers(nodes));

        if cancelled_retry_chains > 0 {
            let mut stats = self.inner.script_retry_stats.borrow_mut();
            stats.cancelled = stats.cancelled.saturating_add(cancelled_retry_chains);
        }
        for node in nodes {
            self.inner
                .parser_blocking_elements
                .borrow_mut()
                .remove(node);
            self.complete_document_script_node(*node);
        }
        self.drain_ordered_classic_scripts();
        self.drain_deferred_classic_scripts();
        self.drain_deferred_parser_modules();
    }

    fn cancel_removed_module_consumers(&self, nodes: &HashSet<u32>) -> u64 {
        let loads = self
            .inner
            .module_graph_loads
            .borrow()
            .iter()
            .map(|(root, load)| (root.clone(), Rc::clone(load)))
            .collect::<Vec<_>>();
        let mut orphaned = Vec::new();
        for (root, load) in loads {
            let mut load = load.borrow_mut();
            load.element_consumers.retain(|node| !nodes.contains(node));
            if !load.settled
                && load.element_consumers.is_empty()
                && load.uncancellable_consumers == 0
            {
                orphaned.push(root);
            }
        }
        for root in &orphaned {
            let load = self.inner.module_graph_loads.borrow().get(root).cloned();
            if let Some(load) = load {
                reject_graph_load(&load, "script element was removed during module fetch");
            }
            self.inner.module_graph_loads.borrow_mut().remove(root);
        }

        let referenced = self
            .inner
            .module_graph_loads
            .borrow()
            .values()
            .filter_map(|load| {
                let load = load.borrow();
                (!load.settled).then(|| load.pending.iter().cloned().collect::<Vec<_>>())
            })
            .flatten()
            .collect::<HashSet<_>>();
        let mut cancelled_retry_chains = 0_u64;
        self.inner
            .pending_source_fetches
            .borrow_mut()
            .retain(|url, fetch| {
                if referenced.contains(url) {
                    return true;
                }
                if let Some(task) = &fetch.task {
                    task.cancel();
                }
                if fetch.retry_at.is_some() || fetch.attempts_started > 1 {
                    cancelled_retry_chains = cancelled_retry_chains.saturating_add(1);
                }
                false
            });
        cancelled_retry_chains
    }

    fn cancel_removed_stylesheet_nodes(&self, nodes: &HashSet<u32>) {
        self.invalidate_stylesheet_nodes(nodes);
    }

    fn invalidate_image_nodes(&self, nodes: &HashSet<u32>, removed: bool) {
        for node in nodes {
            if let Some(fetch) = self.inner.pending_image_fetches.borrow_mut().remove(node) {
                fetch.request.task.cancel();
                crate::image_loader::reserve_browser_source(&fetch.request.source);
            }
            self.reject_image_decode_waiters(
                *node,
                if removed {
                    "The image element was removed"
                } else {
                    "The image source was changed"
                },
            );
            self.inner.processed_image_nodes.borrow_mut().remove(node);
            if removed {
                self.complete_document_script_node(*node);
            } else {
                let element = crate::jsdom::element_value(*node);
                let has_source = crate::dom::get_attribute(*node, "src")
                    .is_some_and(|source| !source.trim().is_empty());
                set_image_element_state(&element, !has_source, "", 0, 0);
            }
        }
    }

    fn invalidate_stylesheet_nodes(&self, nodes: &HashSet<u32>) {
        let removed = nodes
            .iter()
            .filter_map(|node| {
                self.inner
                    .pending_stylesheet_fetches
                    .borrow_mut()
                    .remove(node)
                    .map(|fetch| (*node, fetch))
            })
            .collect::<Vec<_>>();
        for (node, fetch) in removed {
            fetch.task.cancel();
            self.inner.ready_stylesheets.borrow_mut().insert(
                fetch.graph.order,
                ReadyStylesheet {
                    element: fetch.graph.element,
                    node,
                    source: Ok(None),
                },
            );
        }
        let removed_fonts = nodes
            .iter()
            .filter_map(|node| {
                self.inner
                    .pending_stylesheet_font_fetches
                    .borrow_mut()
                    .remove(node)
                    .map(|fetch| (*node, fetch))
            })
            .collect::<Vec<_>>();
        for (node, fetch) in removed_fonts {
            fetch.task.cancel();
            if fetch.graph.font_loading_started {
                crate::font_face::FontFaceSet::global().mark_ready();
                crate::font_loading_web::cancel_font_loading(fetch.graph.font_event_faces.clone());
            }
            if !fetch.graph.font_only {
                self.inner.ready_stylesheets.borrow_mut().insert(
                    fetch.graph.order,
                    ReadyStylesheet {
                        element: fetch.graph.element,
                        node,
                        source: Ok(None),
                    },
                );
            }
        }
        self.inner
            .deferred_stylesheet_fonts
            .borrow_mut()
            .retain(|node, _| !nodes.contains(node));
        for ready in self.inner.ready_stylesheets.borrow_mut().values_mut() {
            if nodes.contains(&ready.node) {
                ready.source = Ok(None);
            }
        }
        self.inner
            .processed_stylesheet_nodes
            .borrow_mut()
            .retain(|node| !nodes.contains(node));
        self.inner
            .installed_stylesheets
            .borrow_mut()
            .retain(|node, _| !nodes.contains(node));
        for node in nodes {
            crate::jsdom::remove_author_stylesheet(*node);
            if let Some(owner) = self.inner.stylesheet_font_owners.borrow_mut().remove(node) {
                crate::font_face::FontRegistry::global().clear_owner(owner);
                crate::font_loading_web::clear_stylesheet_font_owner(owner);
            }
        }
        self.rebuild_stylesheet_rules();
        self.drain_ready_stylesheets();
    }

    fn rebuild_stylesheet_rules(&self) {
        w3cos_dom::stylesheet::clear_owner(self.inner.stylesheet_owner);
        let installed = self.inner.installed_stylesheets.borrow().clone();
        let mut nodes = Vec::new();
        for root in crate::dom::get_elements_by_tag_name("html") {
            collect_stylesheet_nodes_in_tree_order(root, &mut nodes);
        }
        let mut sheet_nodes = Vec::new();
        for node in nodes {
            if !crate::dom::is_connected(node) || crate::dom::has_attribute(node, "disabled") {
                continue;
            }
            let Some(stylesheet) = installed.get(&node) else {
                continue;
            };
            sheet_nodes.push(node);
            let sheet_media = crate::dom::get_attribute(node, "media").unwrap_or_default();
            if !sheet_media.trim().is_empty() && !crate::jsdom::media_query_matches(&sheet_media) {
                continue;
            }
            let parsed = w3cos_compiler::esm_css::parse_css_source(
                &stylesheet.source,
                stylesheet.href.as_deref().unwrap_or("inline <style>"),
            );
            for rule in parsed.rules {
                if rule
                    .media
                    .as_deref()
                    .is_some_and(|media| !crate::jsdom::media_query_matches(media))
                {
                    continue;
                }
                let declarations = rule
                    .declarations
                    .iter()
                    .map(|(property, value)| (property.as_str(), value.as_str()))
                    .collect::<Vec<_>>();
                w3cos_dom::stylesheet::register_rule_for_owner(
                    self.inner.stylesheet_owner,
                    &rule.selector,
                    &declarations,
                );
            }
        }
        crate::jsdom::order_author_stylesheets(&sheet_nodes);
        let document_url = self.inner.document_url.borrow().clone();
        if let Some(document_url) = document_url {
            self.prepare_background_images(&document_url);
        }
    }

    fn prepare_pending_stylesheets(&self, document_url: &str) {
        let Ok(base) = Url::parse(document_url) else {
            return;
        };
        let mut nodes = Vec::new();
        for root in crate::dom::get_elements_by_tag_name("html") {
            collect_stylesheet_nodes_in_tree_order(root, &mut nodes);
        }
        for node in nodes {
            if !crate::dom::is_connected(node)
                || self
                    .inner
                    .processed_stylesheet_nodes
                    .borrow()
                    .contains(&node)
            {
                continue;
            }
            let tag = crate::dom::tag_name(node);
            let element = crate::jsdom::element_value(node);
            let dynamically_inserted =
                DYNAMIC_STYLESHEET_NODES.with(|nodes| nodes.borrow().contains(&node));
            if !self.inner.parser_finished.get() && !dynamically_inserted {
                // The streaming tree builder may expose parser-authored
                // elements before their token/raw text is complete. Claim
                // them together at EOF so CSS source order is exact.
                continue;
            }
            if crate::dom::has_attribute(node, "disabled")
                && matches!(tag.as_str(), "style" | "link")
            {
                continue;
            }
            if tag.eq_ignore_ascii_case("style") {
                let content_type = crate::dom::get_attribute(node, "type").unwrap_or_default();
                if !content_type.is_empty() && !content_type.eq_ignore_ascii_case("text/css") {
                    continue;
                }
                let source = crate::dom::inner_text(node);
                if source.is_empty() {
                    continue;
                }
                self.inner
                    .processed_stylesheet_nodes
                    .borrow_mut()
                    .insert(node);
                DYNAMIC_STYLESHEET_NODES.with(|nodes| {
                    nodes.borrow_mut().remove(&node);
                });
                self.start_inline_stylesheet(element, node, source, document_url);
                continue;
            }
            if !tag.eq_ignore_ascii_case("link") {
                continue;
            }
            let rel = crate::dom::get_attribute(node, "rel").unwrap_or_default();
            if !rel
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
            {
                continue;
            }
            let content_type = crate::dom::get_attribute(node, "type").unwrap_or_default();
            if !content_type.is_empty() && !content_type.eq_ignore_ascii_case("text/css") {
                continue;
            }
            let Some(href) =
                crate::dom::get_attribute(node, "href").filter(|href| !href.trim().is_empty())
            else {
                continue;
            };
            self.inner
                .processed_stylesheet_nodes
                .borrow_mut()
                .insert(node);
            DYNAMIC_STYLESHEET_NODES.with(|nodes| {
                nodes.borrow_mut().remove(&node);
            });
            match base.join(&href) {
                Ok(url) if matches!(url.scheme(), "http" | "https") => {
                    self.start_stylesheet_fetch(element, node, url.as_str());
                }
                Ok(url) => self.queue_stylesheet_result(
                    element,
                    node,
                    Err(format!(
                        "stylesheet URL scheme {:?} is not supported",
                        url.scheme()
                    )),
                ),
                Err(error) => self.queue_stylesheet_result(
                    element,
                    node,
                    Err(format!("invalid stylesheet URL {href:?}: {error}")),
                ),
            }
        }
        self.drain_ready_stylesheets();
    }

    fn prepare_pending_images(&self, document_url: &str) {
        let Ok(base) = Url::parse(document_url) else {
            return;
        };
        for node in crate::dom::get_elements_by_tag_name("img") {
            if crate::dom::is_connected(node) {
                self.prepare_image_node(node, &base);
            }
        }
    }

    fn prepare_background_images(&self, document_url: &str) {
        let Ok(base) = Url::parse(document_url) else {
            return;
        };
        let mut nodes = Vec::new();
        for root in crate::dom::get_elements_by_tag_name("html") {
            collect_dom_nodes_in_tree_order(root, &mut nodes);
        }
        let mut active_sources = HashSet::new();
        for node in nodes {
            let value = crate::dom::computed_style_property(node, "background-image");
            for source in crate::image_loader::css_image_urls(&value) {
                active_sources.insert(source);
            }
        }
        self.inner
            .processed_background_images
            .borrow_mut()
            .retain(|source| active_sources.contains(source));
        self.inner
            .pending_background_image_fetches
            .borrow_mut()
            .retain(|source, fetch| {
                let active = active_sources.contains(source);
                if !active {
                    fetch.request.task.cancel();
                }
                active
            });

        for source in active_sources {
            if !self
                .inner
                .processed_background_images
                .borrow_mut()
                .insert(source.clone())
            {
                continue;
            }
            if crate::image_loader::dimensions(&source).is_some() {
                continue;
            }
            match base.join(&source) {
                Ok(url) if matches!(url.scheme(), "http" | "https") => {
                    self.start_background_image_fetch(source, url.as_str());
                }
                Ok(url) => eprintln!(
                    "[w3cos] warning: background image URL scheme {:?} is not supported",
                    url.scheme()
                ),
                Err(error) => {
                    eprintln!("[w3cos] warning: invalid background image URL {source:?}: {error}")
                }
            }
        }
    }

    fn start_background_image_fetch(&self, source: String, request_url: &str) {
        crate::image_loader::reserve_browser_source(&source);
        if !self.inner.policy.allow_network {
            return;
        }
        let referrer_source = self
            .inner
            .document_url
            .borrow()
            .clone()
            .unwrap_or_else(|| request_url.to_string());
        let request = self.start_browser_image_request(
            source.clone(),
            request_url,
            ModuleCredentialsMode::Include,
            false,
            &referrer_source,
            crate::fetch::ScriptReferrerPolicy::default(),
        );
        self.inner
            .pending_background_image_fetches
            .borrow_mut()
            .insert(source, PendingBackgroundImageFetch { request });
    }

    fn prepare_mutated_images(&self, document_url: &str) {
        let Ok(base) = Url::parse(document_url) else {
            return;
        };
        let nodes = DYNAMIC_IMAGE_NODES.with(|nodes| nodes.take());
        for node in nodes {
            let singleton = HashSet::from([node]);
            self.invalidate_image_nodes(&singleton, false);
            self.prepare_image_node(node, &base);
        }
    }

    fn prepare_image_node(&self, node: u32, base: &Url) {
        DYNAMIC_IMAGE_NODES.with(|nodes| {
            nodes.borrow_mut().remove(&node);
        });
        if self.inner.processed_image_nodes.borrow().contains(&node) {
            return;
        }
        if should_defer_lazy_image(node) {
            return;
        }
        self.inner.processed_image_nodes.borrow_mut().insert(node);
        let element = crate::jsdom::element_value(node);
        let Some(selection) = select_image_source(node) else {
            crate::dom::set_image_render_source(node, None);
            set_image_element_state(&element, true, "", 0, 0);
            self.reject_image_decode_waiters(node, "The image has no source");
            return;
        };
        crate::dom::set_image_render_source(node, Some(&selection.source));
        if let Some((bytes, _media_type)) = w3cos_core::web::object_url_resource(&selection.source)
        {
            match crate::image_loader::decode_and_install(&selection.source, &bytes) {
                Ok(decoded) => {
                    let current_src = selection.source.clone();
                    let width = decoded.intrinsic_width;
                    let height = decoded.intrinsic_height;
                    element.set_property("__w3cos_image_request_src", Value::string(&current_src));
                    set_image_element_state(&element, true, &current_src, width, height);
                    crate::dom::mark_dom_dirty();
                    self.resolve_image_decode_waiters(node);
                    crate::jsdom::dispatch_element_lifecycle_event(node, "load");
                    let onload = element.get_property("onload");
                    if !onload.is_null() && !onload.is_undefined() {
                        onload.call(element, vec![]);
                    }
                }
                Err(error) => {
                    set_image_element_state(&element, true, &selection.source, 0, 0);
                    self.reject_image_decode_waiters(node, &error);
                    self.dispatch_image_error(&element, &error);
                }
            }
            return;
        }
        match base.join(&selection.source) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                element.set_property("__w3cos_image_request_src", Value::string(url.as_str()));
                self.start_image_fetch(
                    element,
                    node,
                    selection.source,
                    selection.density,
                    url.as_str(),
                );
            }
            Ok(url) => {
                element.set_property("__w3cos_image_request_src", Value::string(url.as_str()));
                set_image_element_state(&element, true, url.as_str(), 0, 0);
                self.reject_image_decode_waiters(
                    node,
                    &format!("image URL scheme {:?} is not supported", url.scheme()),
                );
                self.dispatch_image_error(
                    &element,
                    &format!("image URL scheme {:?} is not supported", url.scheme()),
                );
            }
            Err(error) => {
                element.set_property("__w3cos_image_request_src", Value::string(""));
                set_image_element_state(&element, true, "", 0, 0);
                self.reject_image_decode_waiters(
                    node,
                    &format!("invalid image URL {:?}: {error}", selection.source),
                );
                self.dispatch_image_error(
                    &element,
                    &format!("invalid image URL {:?}: {error}", selection.source),
                );
            }
        }
    }

    fn start_image_fetch(
        &self,
        element: Value,
        node: u32,
        source: String,
        density: f64,
        request_url: &str,
    ) {
        crate::image_loader::reserve_browser_source(&source);
        set_image_element_state(&element, false, request_url, 0, 0);
        let delays_document_load = crate::dom::get_attribute(node, "loading")
            .is_none_or(|value| !value.eq_ignore_ascii_case("lazy"));
        if delays_document_load && crate::dom::is_connected(node) {
            self.register_document_script(&element, false);
        }
        if !self.inner.policy.allow_network {
            set_image_element_state(&element, true, request_url, 0, 0);
            self.reject_image_decode_waiters(node, "network loading is disabled by script policy");
            self.dispatch_image_error(&element, "network loading is disabled by script policy");
            self.complete_document_script_node(node);
            return;
        }

        let cross_origin = crate::dom::get_attribute(node, "crossorigin");
        let (credentials_mode, cors_enabled) = match cross_origin.as_deref() {
            Some(value) if value.eq_ignore_ascii_case("use-credentials") => {
                (ModuleCredentialsMode::Include, true)
            }
            Some(_) => (ModuleCredentialsMode::Omit, true),
            None => (ModuleCredentialsMode::Include, false),
        };
        let referrer_source = self
            .inner
            .document_url
            .borrow()
            .clone()
            .unwrap_or_else(|| request_url.to_string());
        let referrer_policy = Self::script_referrer_policy(&element);
        let request = self.start_browser_image_request(
            source,
            request_url,
            credentials_mode,
            cors_enabled,
            &referrer_source,
            referrer_policy,
        );
        self.inner.pending_image_fetches.borrow_mut().insert(
            node,
            PendingImageFetch {
                element,
                density,
                request,
            },
        );
    }

    fn start_browser_image_request(
        &self,
        source: String,
        request_url: &str,
        credentials_mode: ModuleCredentialsMode,
        cors_enabled: bool,
        referrer_source: &str,
        referrer_policy: crate::fetch::ScriptReferrerPolicy,
    ) -> BrowserImageRequest {
        let request_origin = self
            .inner
            .module_request_origin
            .borrow()
            .clone()
            .unwrap_or_else(|| {
                Url::parse(request_url)
                    .map(|url| url.origin().ascii_serialization())
                    .unwrap_or_default()
            });
        let cookies = crate::cookie_store_web::snapshot();
        let mut options = crate::fetch::FetchOptions::default();
        options.headers.insert(
            "accept".to_string(),
            "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8".to_string(),
        );
        let request_headers = crate::fetch::browser_subresource_request_headers(
            request_url,
            &options,
            &request_origin,
            &cookies,
            credentials_mode,
            cors_enabled,
            referrer_source,
            referrer_policy,
        )
        .unwrap_or_else(|_| options.headers.clone());
        let cache_key =
            image_http_cache_key(request_url, &request_origin, credentials_mode, cors_enabled);
        let cached_response = match crate::browser_http_cache::load(
            &self.browser_http_cache_policy(),
            &cache_key,
            &request_headers,
        ) {
            Ok(Some(cached)) if cached.final_url == request_url => {
                crate::browser_http_cache::add_revalidation_headers(&mut options.headers, &cached);
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.candidates = stats.candidates.saturating_add(1);
                Some(cached)
            }
            Ok(Some(_)) | Ok(None) => {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.misses = stats.misses.saturating_add(1);
                None
            }
            Err(_) => {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.misses = stats.misses.saturating_add(1);
                stats.errors = stats.errors.saturating_add(1);
                None
            }
        };
        let task = crate::fetch::fetch_script_bytes_async(
            request_url,
            options,
            request_origin.clone(),
            cookies,
            credentials_mode,
            cors_enabled,
            referrer_source.to_string(),
            referrer_policy,
        );
        BrowserImageRequest {
            source,
            request_url: request_url.to_string(),
            request_origin,
            credentials_mode,
            cors_enabled,
            cache_key,
            request_headers,
            cached_response,
            task,
        }
    }

    fn dispatch_image_error(&self, element: &Value, message: &str) {
        if let Some(node) = crate::jsdom::node_id_of(element) {
            crate::jsdom::dispatch_element_lifecycle_event(node, "error");
        }
        let onerror = element.get_property("onerror");
        if !onerror.is_null() && !onerror.is_undefined() {
            onerror.call(element.clone(), vec![Value::string(message)]);
        }
    }

    fn resolve_image_decode_waiters(&self, node: u32) {
        for waiter in self
            .inner
            .image_decode_waiters
            .borrow_mut()
            .remove(&node)
            .unwrap_or_default()
        {
            waiter
                .resolve
                .call(Value::Undefined, vec![Value::Undefined]);
        }
    }

    fn reject_image_decode_waiters(&self, node: u32, message: &str) {
        let error = image_encoding_error(message);
        for waiter in self
            .inner
            .image_decode_waiters
            .borrow_mut()
            .remove(&node)
            .unwrap_or_default()
        {
            waiter.reject.call(Value::Undefined, vec![error.clone()]);
        }
    }

    fn start_inline_stylesheet(
        &self,
        element: Value,
        node: u32,
        source: String,
        document_url: &str,
    ) {
        let parsed = w3cos_compiler::esm_css::parse_css_source(&source, "inline <style>");
        if parsed.imports.is_empty() && parsed.font_faces.is_empty() {
            self.queue_stylesheet_result(element, node, Ok(Some((None, source))));
            return;
        }
        let source_bytes = source.len();
        if source_bytes > self.inner.policy.max_source_bytes {
            self.queue_stylesheet_result(
                element,
                node,
                Err(format!(
                    "inline stylesheet graph exceeds source limit ({} > {} bytes)",
                    source_bytes, self.inner.policy.max_source_bytes
                )),
            );
            return;
        }
        let order = self.inner.next_stylesheet_order.get();
        self.inner.next_stylesheet_order.set(order + 1);
        self.register_document_script(&element, false);
        let request_origin = self
            .inner
            .module_request_origin
            .borrow()
            .clone()
            .or_else(|| {
                Url::parse(document_url)
                    .ok()
                    .map(|url| url.origin().ascii_serialization())
            })
            .unwrap_or_default();
        let base = Url::parse(document_url).ok();
        let font_faces = parsed
            .font_faces
            .into_iter()
            .map(|mut face| {
                face.media = combine_stylesheet_media(None, face.media.as_deref());
                StylesheetFontFaceLoad {
                    face,
                    base_url: document_url.to_string(),
                    js_face: None,
                    demanded: false,
                }
            })
            .collect();
        let mut actions = VecDeque::new();
        actions.push_back(StylesheetGraphAction::Append {
            source,
            base_url: document_url.to_string(),
            media: None,
            font_faces,
        });
        for import in parsed.imports.into_iter().rev() {
            let Some(url) = base
                .as_ref()
                .and_then(|base| base.join(&import.href).ok())
                .filter(|url| matches!(url.scheme(), "http" | "https"))
            else {
                eprintln!(
                    "[w3cos] warning: unsupported inline stylesheet @import URL {:?}",
                    import.href
                );
                continue;
            };
            actions.push_front(StylesheetGraphAction::Fetch(StylesheetFetchAction {
                url: url.to_string(),
                media: import.media,
                depth: 1,
                root: false,
                ancestry: Vec::new(),
                referrer_source: document_url.to_string(),
                integrity: String::new(),
            }));
        }
        self.advance_stylesheet_graph(StylesheetGraphLoad {
            element,
            node,
            order,
            request_origin,
            credentials_mode: ModuleCredentialsMode::SameOrigin,
            cors_enabled: false,
            referrer_policy: Self::script_referrer_policy(&crate::jsdom::element_value(node)),
            actions,
            expanded_source: String::new(),
            root_href: None,
            fetched_imports: 0,
            total_source_bytes: source_bytes,
            font_faces: Vec::new(),
            fonts_discovered: false,
            font_owner: None,
            font_only: false,
            font_loading_started: false,
            font_loading_failed: false,
            font_event_faces: Vec::new(),
        });
    }

    fn start_stylesheet_fetch(&self, element: Value, node: u32, url: &str) {
        let order = self.inner.next_stylesheet_order.get();
        self.inner.next_stylesheet_order.set(order + 1);
        self.register_document_script(&element, false);
        if !self.inner.policy.allow_network {
            self.inner.ready_stylesheets.borrow_mut().insert(
                order,
                ReadyStylesheet {
                    element,
                    node,
                    source: Err("network loading is disabled by script policy".to_string()),
                },
            );
            return;
        }
        let request_origin = self
            .inner
            .module_request_origin
            .borrow()
            .clone()
            .unwrap_or_else(|| {
                Url::parse(url)
                    .map(|url| url.origin().ascii_serialization())
                    .unwrap_or_default()
            });
        let cross_origin = element.call_method("getAttribute", vec![Value::string("crossorigin")]);
        let (credentials_mode, cors_enabled) = if cross_origin.is_null() {
            (ModuleCredentialsMode::SameOrigin, false)
        } else if cross_origin
            .to_js_string()
            .eq_ignore_ascii_case("use-credentials")
        {
            (ModuleCredentialsMode::Include, true)
        } else {
            (ModuleCredentialsMode::Omit, true)
        };
        let referrer_source = self
            .inner
            .document_url
            .borrow()
            .clone()
            .unwrap_or_else(|| url.to_string());
        let referrer_policy = Self::script_referrer_policy(&element);
        let integrity = element
            .call_method("getAttribute", vec![Value::string("integrity")])
            .to_js_string();
        let mut actions = VecDeque::new();
        actions.push_back(StylesheetGraphAction::Fetch(StylesheetFetchAction {
            url: url.to_string(),
            media: None,
            depth: 0,
            root: true,
            ancestry: Vec::new(),
            referrer_source,
            integrity: (integrity != "null")
                .then_some(integrity)
                .unwrap_or_default(),
        }));
        self.advance_stylesheet_graph(StylesheetGraphLoad {
            element,
            node,
            order,
            request_origin,
            credentials_mode,
            cors_enabled,
            referrer_policy,
            actions,
            expanded_source: String::new(),
            root_href: None,
            fetched_imports: 0,
            total_source_bytes: 0,
            font_faces: Vec::new(),
            fonts_discovered: false,
            font_owner: None,
            font_only: false,
            font_loading_started: false,
            font_loading_failed: false,
            font_event_faces: Vec::new(),
        });
    }

    fn advance_stylesheet_graph(&self, mut graph: StylesheetGraphLoad) {
        loop {
            let Some(action) = graph.actions.pop_front() else {
                if !graph.fonts_discovered {
                    graph.fonts_discovered = true;
                    if !graph.font_faces.is_empty() {
                        let owner = NEXT_STYLESHEET_FONT_OWNER.fetch_add(1, Ordering::Relaxed);
                        graph.font_owner = Some(owner);
                        self.inner
                            .stylesheet_font_owners
                            .borrow_mut()
                            .insert(graph.node, owner);
                        let discovered = std::mem::take(&mut graph.font_faces)
                            .into_iter()
                            .map(|mut load| {
                                load.js_face =
                                    Some(crate::font_loading_web::register_stylesheet_font_face(
                                        owner,
                                        &load.face.family,
                                        load.face.style.as_deref(),
                                        load.face.weight.as_deref(),
                                        load.face.display.as_deref(),
                                        load.face.unicode_range.as_deref(),
                                    ));
                                load
                            })
                            .collect::<Vec<_>>();
                        if !discovered.is_empty() {
                            self.inner.deferred_stylesheet_fonts.borrow_mut().insert(
                                graph.node,
                                DeferredStylesheetFontBatch {
                                    element: graph.element.clone(),
                                    node: graph.node,
                                    request_origin: graph.request_origin.clone(),
                                    credentials_mode: graph.credentials_mode,
                                    cors_enabled: graph.cors_enabled,
                                    referrer_policy: graph.referrer_policy,
                                    faces: discovered,
                                },
                            );
                        }
                    }
                }
                if graph.font_loading_started {
                    crate::font_face::FontFaceSet::global().mark_ready();
                    let (loaded, failed): (Vec<_>, Vec<_>) =
                        graph.font_event_faces.iter().cloned().partition(|face| {
                            face.get_property("status").to_js_string() == "loaded"
                        });
                    crate::font_loading_web::finish_font_loading(
                        loaded,
                        if graph.font_loading_failed {
                            failed
                        } else {
                            Vec::new()
                        },
                    );
                } else {
                    crate::font_face::FontFaceSet::global().mark_ready_if_idle();
                }
                if graph.font_only {
                    self.activate_deferred_stylesheet_fonts();
                    return;
                }
                self.inner.ready_stylesheets.borrow_mut().insert(
                    graph.order,
                    ReadyStylesheet {
                        element: graph.element,
                        node: graph.node,
                        source: Ok(Some((graph.root_href, graph.expanded_source))),
                    },
                );
                self.drain_ready_stylesheets();
                self.activate_deferred_stylesheet_fonts();
                return;
            };
            match action {
                StylesheetGraphAction::Append {
                    source,
                    base_url,
                    media,
                    font_faces,
                } => {
                    let source = Url::parse(&base_url)
                        .map(|base| crate::image_loader::absolutize_css_urls(&source, &base))
                        .unwrap_or(source);
                    append_stylesheet_source(&mut graph.expanded_source, &source, media.as_deref());
                    graph.font_faces.extend(font_faces);
                }
                StylesheetGraphAction::Fetch(action) => {
                    if action.depth > MAX_STYLESHEET_IMPORT_DEPTH {
                        eprintln!(
                            "[w3cos] warning: stylesheet @import depth exceeded for {}",
                            action.url
                        );
                        continue;
                    }
                    if action
                        .ancestry
                        .iter()
                        .any(|ancestor| ancestor == &action.url)
                    {
                        eprintln!(
                            "[w3cos] warning: cyclic stylesheet @import skipped for {}",
                            action.url
                        );
                        continue;
                    }
                    if !action.root && graph.fetched_imports >= MAX_STYLESHEET_IMPORTS {
                        eprintln!(
                            "[w3cos] warning: stylesheet @import count exceeded for document"
                        );
                        continue;
                    }
                    if !self.inner.policy.allow_network {
                        eprintln!(
                            "[w3cos] warning: stylesheet @import {} skipped because network loading is disabled",
                            action.url
                        );
                        continue;
                    }
                    if !action.root {
                        graph.fetched_imports += 1;
                    }
                    self.start_stylesheet_graph_request(graph, action);
                    return;
                }
                StylesheetGraphAction::LoadFont(load) => {
                    if !graph.font_loading_started {
                        graph.font_loading_started = true;
                        crate::font_face::FontFaceSet::global().mark_loading();
                        for face in &graph.font_event_faces {
                            face.set_property("status", Value::string("loading"));
                        }
                        crate::font_loading_web::begin_font_loading(graph.font_event_faces.clone());
                    }
                    let sources = load.face.sources.iter().cloned().collect();
                    self.advance_stylesheet_font_sources(graph, load, sources);
                    return;
                }
            }
        }
    }

    fn demand_stylesheet_fonts_for_text(
        &self,
        style: &w3cos_std::style::Style,
        text: &str,
    ) -> usize {
        if text.is_empty() {
            return 0;
        }
        let Some(stack) = style.font_family.as_deref() else {
            return 0;
        };
        let families = stylesheet_font_families(stack);
        if families.is_empty() {
            return 0;
        }
        let weight = crate::font_face::FontWeight(style.font_weight);
        let face_style = match style.font_style {
            w3cos_std::style::FontStyle::Normal => crate::font_face::FontFaceStyle::Normal,
            w3cos_std::style::FontStyle::Italic => crate::font_face::FontFaceStyle::Italic,
            w3cos_std::style::FontStyle::Oblique => crate::font_face::FontFaceStyle::Oblique,
        };
        let mut matched = 0;
        let mut found = false;
        {
            let mut deferred = self.inner.deferred_stylesheet_fonts.borrow_mut();
            for character in text.chars() {
                for family in &families {
                    let loaded_score = crate::font_face::FontRegistry::global()
                        .resolve_for_character(family, weight, face_style, character)
                        .map(|font| {
                            (
                                u8::from(font.style != face_style),
                                font.weight.0.abs_diff(weight.0),
                                font.weight.0,
                            )
                        });
                    let deferred_score = deferred
                        .values()
                        .flat_map(|batch| batch.faces.iter())
                        .filter_map(|load| {
                            stylesheet_font_descriptor_score(
                                &load.face, family, weight, face_style, character,
                            )
                        })
                        .min();
                    let loading_score =
                        self.stylesheet_font_loading_score(family, weight, face_style, character);
                    let Some(selected_score) = deferred_score
                        .into_iter()
                        .chain(loading_score)
                        .chain(loaded_score)
                        .min()
                    else {
                        continue;
                    };
                    if loaded_score.is_some_and(|score| score <= selected_score)
                        || loading_score.is_some_and(|score| score <= selected_score)
                    {
                        break;
                    }
                    for batch in deferred.values_mut() {
                        for load in &mut batch.faces {
                            if stylesheet_font_descriptor_score(
                                &load.face, family, weight, face_style, character,
                            ) == Some(selected_score)
                            {
                                found = true;
                                if !load.demanded {
                                    load.demanded = true;
                                    matched += 1;
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }
        if found {
            self.activate_deferred_stylesheet_fonts();
        }
        matched
    }

    fn stylesheet_font_loading_score(
        &self,
        family: &str,
        weight: crate::font_face::FontWeight,
        style: crate::font_face::FontFaceStyle,
        character: char,
    ) -> Option<(u8, u16, u16)> {
        self.inner
            .pending_stylesheet_font_fetches
            .borrow()
            .values()
            .flat_map(|fetch| {
                stylesheet_font_descriptor_score(&fetch.face, family, weight, style, character)
                    .into_iter()
                    .chain(
                        fetch
                            .graph
                            .actions
                            .iter()
                            .filter_map(|action| match action {
                                StylesheetGraphAction::LoadFont(load) => {
                                    stylesheet_font_descriptor_score(
                                        &load.face, family, weight, style, character,
                                    )
                                }
                                _ => None,
                            }),
                    )
            })
            .min()
    }

    fn demand_stylesheet_font_faces(&self, requested: &[Value], text: &str) -> usize {
        let mut matched = 0;
        {
            let mut deferred = self.inner.deferred_stylesheet_fonts.borrow_mut();
            for batch in deferred.values_mut() {
                for load in &mut batch.faces {
                    let Some(face) = load.js_face.as_ref() else {
                        continue;
                    };
                    if requested.iter().any(|requested| requested == face)
                        && crate::font_face::unicode_range_matches_text(
                            load.face.unicode_range.as_deref(),
                            text,
                        )
                    {
                        load.demanded = true;
                        matched += 1;
                    }
                }
            }
        }
        if matched > 0 {
            self.activate_deferred_stylesheet_fonts();
        }
        matched
    }

    fn activate_deferred_stylesheet_fonts(&self) {
        let pending = self.inner.pending_stylesheet_font_fetches.borrow();
        let mut activations = Vec::new();
        {
            let mut deferred = self.inner.deferred_stylesheet_fonts.borrow_mut();
            deferred.retain(|node, batch| {
                if !crate::dom::is_connected(*node) || crate::dom::has_attribute(*node, "disabled")
                {
                    return false;
                }
                if pending.contains_key(node) {
                    return true;
                }
                let mut active = Vec::new();
                batch.faces.retain(|face| {
                    if face.demanded && stylesheet_font_face_media_matches(*node, face) {
                        active.push(face.clone());
                        false
                    } else {
                        true
                    }
                });
                if !active.is_empty() {
                    activations.push((
                        batch.element.clone(),
                        batch.node,
                        batch.request_origin.clone(),
                        batch.credentials_mode,
                        batch.cors_enabled,
                        batch.referrer_policy,
                        active,
                    ));
                }
                !batch.faces.is_empty()
            });
        }
        drop(pending);

        let activated = !activations.is_empty();
        for (
            element,
            node,
            request_origin,
            credentials_mode,
            cors_enabled,
            referrer_policy,
            faces,
        ) in activations
        {
            let Some(owner) = self
                .inner
                .stylesheet_font_owners
                .borrow()
                .get(&node)
                .copied()
            else {
                continue;
            };
            let font_event_faces = faces
                .iter()
                .filter_map(|load| load.js_face.clone())
                .collect();
            let actions = faces
                .into_iter()
                .map(StylesheetGraphAction::LoadFont)
                .collect();
            self.advance_stylesheet_graph(StylesheetGraphLoad {
                element,
                node,
                order: 0,
                request_origin,
                credentials_mode,
                cors_enabled,
                referrer_policy,
                actions,
                expanded_source: String::new(),
                root_href: None,
                fetched_imports: 0,
                total_source_bytes: 0,
                font_faces: Vec::new(),
                fonts_discovered: true,
                font_owner: Some(owner),
                font_only: true,
                font_loading_started: false,
                font_loading_failed: false,
                font_event_faces,
            });
        }
        if activated {
            schedule_document_script_pump();
        }
    }

    fn advance_stylesheet_font_sources(
        &self,
        mut graph: StylesheetGraphLoad,
        load: StylesheetFontFaceLoad,
        mut sources: VecDeque<w3cos_compiler::esm_css::StylesheetFontSource>,
    ) {
        let owner = graph
            .font_owner
            .expect("stylesheet font owner exists while loading faces");
        while let Some(source) = sources.pop_front() {
            match source {
                w3cos_compiler::esm_css::StylesheetFontSource::Local(name) => {
                    let face = native_stylesheet_font_face(
                        &load.face,
                        crate::font_face::FontSource::Local(name.clone()),
                    );
                    if crate::font_face::FontRegistry::global()
                        .register_local_for_owner(owner, face, &name)
                        .is_ok()
                    {
                        if let Some(face) = &load.js_face {
                            face.set_property("status", Value::string("loaded"));
                        }
                        self.advance_stylesheet_graph(graph);
                        return;
                    }
                }
                w3cos_compiler::esm_css::StylesheetFontSource::Url { href, format } => {
                    if !supported_stylesheet_font_format(format.as_deref()) {
                        continue;
                    }
                    let Some(url) = Url::parse(&load.base_url)
                        .ok()
                        .and_then(|base| base.join(&href).ok())
                        .filter(|url| matches!(url.scheme(), "http" | "https"))
                    else {
                        continue;
                    };
                    self.start_stylesheet_font_request(graph, load, sources, url.as_str());
                    return;
                }
            }
        }
        eprintln!(
            "[w3cos] warning: no usable @font-face source loaded for {:?}",
            load.face.family
        );
        if let Some(face) = &load.js_face {
            face.set_property("status", Value::string("error"));
        }
        graph.font_loading_failed = true;
        self.advance_stylesheet_graph(graph);
    }

    fn start_stylesheet_font_request(
        &self,
        graph: StylesheetGraphLoad,
        load: StylesheetFontFaceLoad,
        remaining_sources: VecDeque<w3cos_compiler::esm_css::StylesheetFontSource>,
        url: &str,
    ) {
        let cookies = crate::cookie_store_web::snapshot();
        let mut options = crate::fetch::FetchOptions::default();
        options.headers.insert(
            "accept".to_string(),
            "font/woff2,font/woff,font/ttf,font/otf,application/font-woff,*/*;q=0.1".to_string(),
        );
        let request_headers = crate::fetch::browser_subresource_request_headers(
            url,
            &options,
            &graph.request_origin,
            &cookies,
            ModuleCredentialsMode::Omit,
            true,
            &load.base_url,
            graph.referrer_policy,
        )
        .unwrap_or_else(|_| options.headers.clone());
        let cache_key = stylesheet_font_http_cache_key(url, &graph.request_origin);
        let cached_response = match crate::browser_http_cache::load(
            &self.browser_http_cache_policy(),
            &cache_key,
            &request_headers,
        ) {
            Ok(Some(cached)) if cached.final_url == url => {
                crate::browser_http_cache::add_revalidation_headers(&mut options.headers, &cached);
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.candidates = stats.candidates.saturating_add(1);
                Some(cached)
            }
            Ok(Some(_)) | Ok(None) => {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.misses = stats.misses.saturating_add(1);
                None
            }
            Err(_) => {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.misses = stats.misses.saturating_add(1);
                stats.errors = stats.errors.saturating_add(1);
                None
            }
        };
        let task = crate::fetch::fetch_script_bytes_async(
            url,
            options,
            graph.request_origin.clone(),
            cookies,
            ModuleCredentialsMode::Omit,
            true,
            load.base_url.clone(),
            graph.referrer_policy,
        );
        self.inner
            .pending_stylesheet_font_fetches
            .borrow_mut()
            .insert(
                graph.node,
                PendingStylesheetFontFetch {
                    graph,
                    face: load.face,
                    base_url: load.base_url,
                    js_face: load.js_face,
                    remaining_sources,
                    request_url: url.to_string(),
                    cache_key,
                    request_headers,
                    cached_response,
                    task,
                },
            );
    }

    fn start_stylesheet_graph_request(
        &self,
        graph: StylesheetGraphLoad,
        action: StylesheetFetchAction,
    ) {
        let cookies = crate::cookie_store_web::snapshot();
        let mut options = crate::fetch::FetchOptions::default();
        options
            .headers
            .insert("accept".to_string(), "text/css,*/*;q=0.1".to_string());
        let request_headers = crate::fetch::browser_subresource_request_headers(
            &action.url,
            &options,
            &graph.request_origin,
            &cookies,
            graph.credentials_mode,
            graph.cors_enabled,
            &action.referrer_source,
            graph.referrer_policy,
        )
        .unwrap_or_else(|_| options.headers.clone());
        let cache_key = stylesheet_http_cache_key(
            &action.url,
            &graph.request_origin,
            graph.credentials_mode,
            graph.cors_enabled,
        );
        let cached_response = match crate::browser_http_cache::load(
            &self.browser_http_cache_policy(),
            &cache_key,
            &request_headers,
        ) {
            Ok(Some(cached)) if cached.final_url == action.url => {
                crate::browser_http_cache::add_revalidation_headers(&mut options.headers, &cached);
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.candidates = stats.candidates.saturating_add(1);
                Some(cached)
            }
            Ok(Some(_)) | Ok(None) => {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.misses = stats.misses.saturating_add(1);
                None
            }
            Err(_) => {
                let mut stats = self.inner.http_source_cache_stats.borrow_mut();
                stats.misses = stats.misses.saturating_add(1);
                stats.errors = stats.errors.saturating_add(1);
                None
            }
        };
        let task = crate::fetch::fetch_script_text_async(
            &action.url,
            options,
            graph.request_origin.clone(),
            cookies,
            graph.credentials_mode,
            graph.cors_enabled,
            action.referrer_source.clone(),
            graph.referrer_policy,
        );
        let node = graph.node;
        self.inner.pending_stylesheet_fetches.borrow_mut().insert(
            node,
            PendingStylesheetFetch {
                graph,
                request_url: action.url.clone(),
                action,
                cache_key,
                request_headers,
                cached_response,
                task,
            },
        );
    }

    fn queue_stylesheet_result(
        &self,
        element: Value,
        node: u32,
        source: std::result::Result<Option<(Option<String>, String)>, String>,
    ) {
        let order = self.inner.next_stylesheet_order.get();
        self.inner.next_stylesheet_order.set(order + 1);
        self.inner.ready_stylesheets.borrow_mut().insert(
            order,
            ReadyStylesheet {
                element,
                node,
                source,
            },
        );
    }

    fn poll_stylesheet_fetches(&self) -> usize {
        let completed = {
            let pending = self.inner.pending_stylesheet_fetches.borrow();
            pending
                .iter()
                .filter_map(|(node, fetch)| match fetch.task.receiver.try_recv() {
                    Ok(result) => Some((*node, result)),
                    Err(TryRecvError::Disconnected) => Some((
                        *node,
                        Err("stylesheet fetch worker disconnected".to_string()),
                    )),
                    Err(TryRecvError::Empty) => None,
                })
                .collect::<Vec<_>>()
        };
        let completed_count = completed.len();
        for (node, result) in completed {
            let Some(fetch) = self
                .inner
                .pending_stylesheet_fetches
                .borrow_mut()
                .remove(&node)
            else {
                continue;
            };
            let response = match result {
                Err(error) => Err(error),
                Ok(response) => self
                    .apply_http_revalidation(response, fetch.cached_response.clone())
                    .map_err(|error| error.to_string())
                    .and_then(|response| (|| {
                    for (url, cookie) in &response.set_cookies {
                        crate::cookie_store_web::set_cookie_assignment_for_url(url, cookie, true);
                    }
                    if !response.ok {
                        return Err(format!(
                            "stylesheet fetch failed with status {} {}",
                            response.status, response.status_text
                        ));
                    }
                    if fetch.graph.cors_enabled {
                        validate_script_cors_headers(
                            &fetch.graph.request_origin,
                            &response.url,
                            &response.headers,
                            fetch.graph.credentials_mode,
                        )
                        .map_err(|detail| {
                            format!(
                                "stylesheet CORS check failed for {} after redirect to {}: {detail}",
                                fetch.request_url, response.url
                            )
                        })?;
                    }
                    if response.body.len() > self.inner.policy.max_source_bytes {
                        return Err(format!(
                            "stylesheet exceeds source limit ({} > {} bytes)",
                            response.body.len(),
                            self.inner.policy.max_source_bytes
                        ));
                    }
                    let content_type =
                        header_value(&response.headers, "content-type").unwrap_or_default();
                    let essence = content_type.split(';').next().unwrap_or_default().trim();
                    if !essence.eq_ignore_ascii_case("text/css") {
                        return Err(format!(
                            "stylesheet MIME check failed for {}: expected text/css, received {}",
                            response.url,
                            if essence.is_empty() {
                                "<missing>"
                            } else {
                                essence
                            }
                        ));
                    }
                    let response_origin = Url::parse(&response.url)
                        .map(|url| url.origin().ascii_serialization())
                        .unwrap_or_default();
                    check_integrity_metadata(
                        response.body.as_bytes(),
                        &fetch.action.integrity,
                        response_origin == fetch.graph.request_origin || fetch.graph.cors_enabled,
                    )
                    .map_err(|error| error.to_string())?;
                    if !response.redirected && response.url == fetch.request_url {
                        self.store_stylesheet_http_source(&fetch, &response);
                    }
                    Ok(response)
                })()),
            };
            let PendingStylesheetFetch {
                mut graph, action, ..
            } = fetch;
            let response = match response {
                Ok(response) => response,
                Err(error) if action.root => {
                    self.inner.ready_stylesheets.borrow_mut().insert(
                        graph.order,
                        ReadyStylesheet {
                            element: graph.element,
                            node,
                            source: Err(error),
                        },
                    );
                    continue;
                }
                Err(error) => {
                    eprintln!(
                        "[w3cos] warning: stylesheet @import {} failed: {error}",
                        action.url
                    );
                    self.advance_stylesheet_graph(graph);
                    continue;
                }
            };
            if !action.root
                && action
                    .ancestry
                    .iter()
                    .any(|ancestor| ancestor == &response.url)
            {
                eprintln!(
                    "[w3cos] warning: cyclic stylesheet @import redirect skipped for {}",
                    response.url
                );
                self.advance_stylesheet_graph(graph);
                continue;
            }
            let next_total = graph.total_source_bytes.saturating_add(response.body.len());
            if next_total > self.inner.policy.max_source_bytes {
                let error = format!(
                    "stylesheet graph exceeds source limit ({} > {} bytes)",
                    next_total, self.inner.policy.max_source_bytes
                );
                if action.root {
                    self.inner.ready_stylesheets.borrow_mut().insert(
                        graph.order,
                        ReadyStylesheet {
                            element: graph.element,
                            node,
                            source: Err(error),
                        },
                    );
                } else {
                    eprintln!("[w3cos] warning: {error}; import skipped");
                    self.advance_stylesheet_graph(graph);
                }
                continue;
            }
            graph.total_source_bytes = next_total;
            if action.root {
                graph.root_href = Some(response.url.clone());
            }
            let parsed = w3cos_compiler::esm_css::parse_css_source(&response.body, &response.url);
            let font_faces = parsed
                .font_faces
                .into_iter()
                .map(|mut face| {
                    face.media =
                        combine_stylesheet_media(action.media.as_deref(), face.media.as_deref());
                    StylesheetFontFaceLoad {
                        face,
                        base_url: response.url.clone(),
                        js_face: None,
                        demanded: false,
                    }
                })
                .collect();
            let mut ancestry = action.ancestry;
            ancestry.push(response.url.clone());
            graph.actions.push_front(StylesheetGraphAction::Append {
                source: response.body,
                base_url: response.url.clone(),
                media: action.media.clone(),
                font_faces,
            });
            let base = Url::parse(&response.url).ok();
            for import in parsed.imports.into_iter().rev() {
                let Some(url) = base
                    .as_ref()
                    .and_then(|base| base.join(&import.href).ok())
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                else {
                    eprintln!(
                        "[w3cos] warning: unsupported stylesheet @import URL {:?} from {}",
                        import.href, response.url
                    );
                    continue;
                };
                graph
                    .actions
                    .push_front(StylesheetGraphAction::Fetch(StylesheetFetchAction {
                        url: url.to_string(),
                        media: combine_stylesheet_media(
                            action.media.as_deref(),
                            import.media.as_deref(),
                        ),
                        depth: action.depth + 1,
                        root: false,
                        ancestry: ancestry.clone(),
                        referrer_source: response.url.clone(),
                        integrity: String::new(),
                    }));
            }
            self.advance_stylesheet_graph(graph);
        }
        self.drain_ready_stylesheets();
        completed_count
    }

    fn poll_stylesheet_font_fetches(&self) -> usize {
        let completed = {
            let pending = self.inner.pending_stylesheet_font_fetches.borrow();
            pending
                .iter()
                .filter_map(|(node, fetch)| match fetch.task.receiver.try_recv() {
                    Ok(result) => Some((*node, result)),
                    Err(TryRecvError::Disconnected) => Some((
                        *node,
                        Err("stylesheet font fetch worker disconnected".to_string()),
                    )),
                    Err(TryRecvError::Empty) => None,
                })
                .collect::<Vec<_>>()
        };
        let completed_count = completed.len();
        for (node, result) in completed {
            let Some(fetch) = self
                .inner
                .pending_stylesheet_font_fetches
                .borrow_mut()
                .remove(&node)
            else {
                continue;
            };
            let response = result
                .and_then(|response| {
                    self.apply_binary_http_revalidation(response, fetch.cached_response.clone())
                        .map_err(|error| error.to_string())
                })
                .and_then(|mut response| {
                    for (url, cookie) in &response.set_cookies {
                        crate::cookie_store_web::set_cookie_assignment_for_url(url, cookie, true);
                    }
                    if !response.ok {
                        return Err(format!(
                            "font fetch failed with status {} {}",
                            response.status, response.status_text
                        ));
                    }
                    validate_script_cors_headers(
                        &fetch.graph.request_origin,
                        &response.url,
                        &response.headers,
                        ModuleCredentialsMode::Omit,
                    )
                    .map_err(|detail| {
                        format!(
                            "font CORS check failed for {} after redirect to {}: {detail}",
                            fetch.request_url, response.url
                        )
                    })?;
                    if response.body.len() > self.inner.policy.max_source_bytes {
                        return Err(format!(
                            "font exceeds source limit ({} > {} bytes)",
                            response.body.len(),
                            self.inner.policy.max_source_bytes
                        ));
                    }
                    let content_type =
                        header_value(&response.headers, "content-type").unwrap_or_default();
                    if !is_supported_font_mime_type(content_type) {
                        return Err(format!(
                            "font MIME check failed for {}: received {}",
                            response.url,
                            if content_type.is_empty() {
                                "<missing>"
                            } else {
                                content_type
                            }
                        ));
                    }
                    let decoded_font = crate::font_face::normalize_font_bytes_with_limit(
                        &response.body,
                        self.inner.policy.max_source_bytes,
                    )
                    .and_then(|bytes| {
                        fontdue::Font::from_bytes(
                            bytes.as_slice(),
                            fontdue::FontSettings::default(),
                        )
                        .map_err(|error| error.to_string())?;
                        Ok(bytes)
                    })
                    .map_err(|error| format!("font decode failed for {}: {error}", response.url))?;
                    if !response.redirected && response.url == fetch.request_url {
                        self.store_stylesheet_font_source(&fetch, &response);
                    }
                    // HTTP cache keeps the original compressed representation;
                    // the shared registry receives canonical sfnt bytes.
                    response.body = decoded_font;
                    Ok(response)
                });
            let PendingStylesheetFontFetch {
                mut graph,
                face,
                base_url,
                js_face,
                remaining_sources,
                request_url,
                ..
            } = fetch;
            match response {
                Ok(response) => {
                    let owner = graph
                        .font_owner
                        .expect("stylesheet font owner exists for fetched font");
                    let native = native_stylesheet_font_face(
                        &face,
                        crate::font_face::FontSource::Bytes(response.body),
                    );
                    if let Err(error) =
                        crate::font_face::FontRegistry::global().register_for_owner(owner, native)
                    {
                        graph.font_loading_failed = true;
                        if let Some(face) = &js_face {
                            face.set_property("status", Value::string("error"));
                        }
                        eprintln!(
                            "[w3cos] warning: @font-face registration failed for {:?}: {error}",
                            face.family
                        );
                    } else if let Some(face) = &js_face {
                        face.set_property("status", Value::string("loaded"));
                    }
                    self.advance_stylesheet_graph(graph);
                }
                Err(error) => {
                    eprintln!(
                        "[w3cos] warning: @font-face source {} failed: {error}",
                        request_url
                    );
                    self.advance_stylesheet_font_sources(
                        graph,
                        StylesheetFontFaceLoad {
                            face,
                            base_url,
                            js_face,
                            demanded: true,
                        },
                        remaining_sources,
                    );
                }
            }
        }
        completed_count
    }

    fn store_stylesheet_font_source(
        &self,
        fetch: &PendingStylesheetFontFetch,
        response: &crate::fetch::FetchBinaryResponse,
    ) {
        let cached = crate::browser_http_cache::CachedResponse::from_network(
            &fetch.cache_key,
            response.url.clone(),
            response.status,
            response.status_text.clone(),
            response.headers.clone(),
            response.body.clone(),
        );
        let result = crate::browser_http_cache::store(
            &self.browser_http_cache_policy(),
            &fetch.cache_key,
            &fetch.request_headers,
            cached,
            true,
        );
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        match result {
            Ok(outcome) if outcome.wrote => {
                stats.writes = stats.writes.saturating_add(1);
                stats.evictions = stats.evictions.saturating_add(outcome.response_evictions);
                drop(stats);
                if outcome.other_evictions > 0 {
                    let mut cache = self.inner.compiled_source_cache.borrow_mut();
                    cache.persistent_evictions = cache
                        .persistent_evictions
                        .saturating_add(outcome.other_evictions);
                }
            }
            Ok(_) => {}
            Err(_) => stats.errors = stats.errors.saturating_add(1),
        }
    }

    fn store_stylesheet_http_source(
        &self,
        fetch: &PendingStylesheetFetch,
        response: &crate::fetch::FetchTextResponse,
    ) {
        let cached = crate::browser_http_cache::CachedResponse::from_network(
            &fetch.cache_key,
            response.url.clone(),
            response.status,
            response.status_text.clone(),
            response.headers.clone(),
            response.body.as_bytes().to_vec(),
        );
        let result = crate::browser_http_cache::store(
            &self.browser_http_cache_policy(),
            &fetch.cache_key,
            &fetch.request_headers,
            cached,
            true,
        );
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        match result {
            Ok(outcome) if outcome.wrote => {
                stats.writes = stats.writes.saturating_add(1);
                stats.evictions = stats.evictions.saturating_add(outcome.response_evictions);
                drop(stats);
                if outcome.other_evictions > 0 {
                    let mut cache = self.inner.compiled_source_cache.borrow_mut();
                    cache.persistent_evictions = cache
                        .persistent_evictions
                        .saturating_add(outcome.other_evictions);
                }
            }
            Ok(_) => {}
            Err(_) => {
                stats.errors = stats.errors.saturating_add(1);
            }
        }
    }

    fn drain_ready_stylesheets(&self) {
        loop {
            let order = self.inner.next_stylesheet_apply.get();
            let Some(ready) = self.inner.ready_stylesheets.borrow_mut().remove(&order) else {
                break;
            };
            self.inner.next_stylesheet_apply.set(order + 1);
            if !crate::dom::is_connected(ready.node) {
                self.complete_document_script_node(ready.node);
                continue;
            }
            match ready.source {
                Ok(Some((href, source))) => {
                    crate::jsdom::install_author_stylesheet(ready.node, href.as_deref(), &source);
                    self.inner
                        .installed_stylesheets
                        .borrow_mut()
                        .insert(ready.node, InstalledStylesheet { href, source });
                    self.rebuild_stylesheet_rules();
                    let onload = ready.element.get_property("onload");
                    if !onload.is_null() && !onload.is_undefined() {
                        onload.call(ready.element.clone(), vec![]);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let onerror = ready.element.get_property("onerror");
                    if !onerror.is_null() && !onerror.is_undefined() {
                        onerror.call(ready.element.clone(), vec![Value::string(&error)]);
                    }
                }
            }
            self.complete_document_script_node(ready.node);
        }
    }

    /// Executes script elements that have appeared in the live document since
    /// the previous pump. This is the initial DocumentLoader integration point:
    /// HTML parsing and DOM insertion share this loader instead of owning a
    /// second JavaScript execution path.
    pub fn execute_pending_document_scripts(&self, document_url: &str) -> Result<usize> {
        let base = Url::parse(document_url)
            .map_err(|error| anyhow!("invalid document URL {document_url}: {error}"))?;
        *self.inner.module_request_origin.borrow_mut() = Some(base.origin().ascii_serialization());
        *self.inner.document_url.borrow_mut() = Some(base.to_string());
        crate::cookie_store_web::set_active_url(base.as_str());
        self.prepare_mutated_images(document_url);
        self.prepare_pending_stylesheets(document_url);
        self.prepare_pending_images(document_url);
        self.prepare_background_images(document_url);
        let scripts = crate::jsdom::document_value()
            .call_method("querySelectorAll", vec![Value::string("script")]);
        let length = scripts.get_property("length").to_u32();
        let mut executed = 0;

        for index in 0..length {
            let element = scripts.call_method("item", vec![Value::Number(f64::from(index))]);
            if crate::jsdom::node_id_of(&element)
                .is_some_and(|node| !crate::dom::is_connected(node))
            {
                continue;
            }
            if self
                .inner
                .processed_elements
                .borrow()
                .iter()
                .any(|processed| processed == &element)
            {
                continue;
            }
            if !self.inner.policy.allow_scripts {
                self.inner
                    .processed_elements
                    .borrow_mut()
                    .push(element.clone());
                if let Some(node) = crate::jsdom::node_id_of(&element) {
                    self.complete_document_script_node(node);
                }
                continue;
            }
            let type_attribute = element.call_method("getAttribute", vec![Value::string("type")]);
            let script_type = if type_attribute.is_null() {
                String::new()
            } else {
                type_attribute.to_js_string()
            };
            let script_type = script_type.trim();
            if script_type.eq_ignore_ascii_case("importmap") {
                self.inner
                    .processed_elements
                    .borrow_mut()
                    .push(element.clone());
                if let Err(error) = self.install_import_map_element(&element, document_url) {
                    let onerror = element.get_property("onerror");
                    if !onerror.is_null() && !onerror.is_undefined() {
                        onerror.call(element, vec![Value::string(&error.to_string())]);
                    }
                    return Err(error);
                }
                continue;
            }
            let is_module = script_type.eq_ignore_ascii_case("module");
            if !script_type.is_empty()
                && !is_javascript_mime_type_essence_match(script_type)
                && !is_module
            {
                continue;
            }

            let src = element.call_method("getAttribute", vec![Value::string("src")]);
            let has_inline_source = !element
                .get_property("textContent")
                .to_js_string()
                .is_empty();
            if src.is_null() && !has_inline_source {
                // Preparing an empty connected script aborts before setting the
                // HTML "already started" flag. A later src/text mutation must
                // therefore be able to schedule this same element again.
                continue;
            }

            // Claim the element before execution. Script evaluation drains the
            // microtask queue and may insert another script, whose pump must not
            // re-enter this element.
            self.inner
                .processed_elements
                .borrow_mut()
                .push(element.clone());
            let dynamically_inserted = crate::jsdom::node_id_of(&element)
                .map(|node| DYNAMIC_SCRIPT_NODES.with(|nodes| nodes.borrow_mut().remove(&node)))
                .unwrap_or(false);
            if !is_module
                && element
                    .call_method("hasAttribute", vec![Value::string("nomodule")])
                    .to_bool()
            {
                // This runtime supports module scripts, so the HTML
                // preparation algorithm treats classic `nomodule` scripts as
                // already started without fetching or evaluating them.
                continue;
            }
            if is_module {
                let (element_node, cancellation) = self.module_element_cancellation(&element);
                let credentials_mode = Self::module_element_credentials_mode(&element);
                let referrer_policy = Self::script_referrer_policy(&element);
                let integrity = element
                    .call_method("getAttribute", vec![Value::string("integrity")])
                    .to_js_string();
                let integrity = (integrity != "null")
                    .then_some(integrity)
                    .unwrap_or_default();
                let specifier = if src.is_null() || src.to_js_string().is_empty() {
                    let specifier = format!("{document_url}#inline-script-{index}");
                    let source = element.get_property("textContent").to_js_string();
                    match self.register_module_source(&specifier, &source) {
                        Ok(()) => Ok(specifier),
                        Err(error) => Err(error.to_string()),
                    }
                } else {
                    match base.join(&src.to_js_string()) {
                        Ok(resolved) => Ok(resolved.to_string()),
                        Err(error) => Err(format!(
                            "invalid script URL {}: {error}",
                            src.to_js_string()
                        )),
                    }
                };
                let has_async = element
                    .call_method("hasAttribute", vec![Value::string("async")])
                    .to_bool();
                self.register_document_script(&element, !dynamically_inserted && !has_async);
                let module = DeferredParserModule {
                    element,
                    specifier,
                    prepared_graph: None,
                    element_node,
                    cancellation,
                    credentials_mode,
                    integrity,
                    referrer_policy,
                };
                if !dynamically_inserted && !has_async && !self.inner.parser_finished.get() {
                    self.prepare_deferred_parser_module(module);
                } else {
                    self.start_module_element(module);
                }
                executed += 1;
                continue;
            }

            let result = if src.is_null() || src.to_js_string().is_empty() {
                let specifier = format!("{document_url}#inline-script-{index}");
                let source = element.get_property("textContent").to_js_string();
                self.execute_source(&source, &specifier).map(Some)
            } else {
                match base.join(&src.to_js_string()) {
                    Ok(resolved) => {
                        let has_async = element
                            .call_method("hasAttribute", vec![Value::string("async")])
                            .to_bool();
                        let has_defer = element
                            .call_method("hasAttribute", vec![Value::string("defer")])
                            .to_bool();
                        self.register_document_script(
                            &element,
                            !dynamically_inserted && !has_async,
                        );
                        if !dynamically_inserted
                            && !has_async
                            && !has_defer
                            && !self.inner.parser_finished.get()
                            && let Some(node) = crate::jsdom::node_id_of(&element)
                        {
                            self.inner
                                .parser_blocking_elements
                                .borrow_mut()
                                .insert(node);
                        }
                        let (order, deferred_until_parse_end) =
                            self.classic_script_order(&element, dynamically_inserted);
                        self.schedule_classic_script(
                            resolved.as_str(),
                            element.clone(),
                            order,
                            deferred_until_parse_end,
                            Self::classic_script_fetch_mode(&element),
                        )
                    }
                    Err(error) => Err(anyhow!(
                        "invalid script URL {}: {error}",
                        src.to_js_string()
                    )),
                }
            };

            match result {
                Ok(Some(_)) => {
                    let onload = element.get_property("onload");
                    if !onload.is_null() && !onload.is_undefined() {
                        onload.call(element.clone(), vec![]);
                    }
                    executed += 1;
                }
                Ok(None) => {
                    executed += 1;
                }
                Err(error) => {
                    let onerror = element.get_property("onerror");
                    if !onerror.is_null() && !onerror.is_undefined() {
                        onerror.call(element, vec![Value::string(&error.to_string())]);
                    }
                    return Err(error);
                }
            }
        }
        Ok(executed)
    }

    fn classic_script_order(
        &self,
        element: &Value,
        dynamically_inserted: bool,
    ) -> (Option<u64>, bool) {
        let has_async_attribute = element
            .call_method("hasAttribute", vec![Value::string("async")])
            .to_bool();
        let async_property = element.get_property("async");
        let explicitly_ordered =
            matches!(async_property, Value::Bool(false)) && !has_async_attribute;
        let is_async = if dynamically_inserted {
            !explicitly_ordered
        } else {
            has_async_attribute
        };
        if is_async {
            (None, false)
        } else if !dynamically_inserted
            && element
                .call_method("hasAttribute", vec![Value::string("defer")])
                .to_bool()
        {
            let order = self.inner.next_deferred_classic_order.get();
            self.inner.next_deferred_classic_order.set(order + 1);
            (Some(order), true)
        } else {
            let order = self.inner.next_classic_order.get();
            self.inner.next_classic_order.set(order + 1);
            (Some(order), false)
        }
    }

    fn register_document_script(&self, element: &Value, blocks_dom_content_loaded: bool) {
        if self.inner.document_load_fired.get() {
            return;
        }
        let Some(node) = crate::jsdom::node_id_of(element) else {
            return;
        };
        self.inner.document_load_blockers.borrow_mut().insert(node);
        if blocks_dom_content_loaded && !self.inner.dom_content_loaded_fired.get() {
            self.inner
                .dom_content_loaded_blockers
                .borrow_mut()
                .insert(node);
        }
    }

    fn start_module_element(&self, module: DeferredParserModule) {
        if module.cancellation.get() {
            self.complete_document_script(&module.element);
            return;
        }
        let evaluation = match (module.specifier, module.prepared_graph) {
            (Ok(specifier), Some(graph)) => self.evaluate_prepared_module_graph_async(
                specifier,
                graph,
                module.credentials_mode,
                module.referrer_policy,
                Some(Rc::clone(&module.cancellation)),
            ),
            (Ok(specifier), None) => self.load_and_execute_module_async_guarded(
                &specifier,
                module.credentials_mode,
                Some((module.element_node, Rc::clone(&module.cancellation))),
                module.integrity,
                module.referrer_policy,
                None,
            ),
            (Err(error), _) => w3cos_core::promise::reject(vec![Value::string(&error)]),
        };
        self.subscribe_module_element(module.element, evaluation, module.cancellation);
    }

    fn prepare_deferred_parser_module(&self, mut module: DeferredParserModule) {
        if let Ok(specifier) = &module.specifier {
            match canonical_module_url(specifier) {
                Ok(canonical) => {
                    let (graph, credentials_mode) = self.ensure_module_graph_async(
                        &canonical,
                        module.credentials_mode,
                        Some(module.element_node),
                        module.integrity.clone(),
                        module.referrer_policy,
                        None,
                    );
                    module.specifier = Ok(canonical);
                    module.prepared_graph = Some(graph.clone());
                    module.credentials_mode = credentials_mode;

                    let weak_loader = Rc::downgrade(&self.inner);
                    let wake = Value::function(move |_, _| {
                        if let Some(inner) = weak_loader.upgrade() {
                            ScriptLoader { inner }.drain_deferred_parser_modules();
                        }
                        Value::Undefined
                    });
                    graph.call_method("then", vec![wake.clone(), wake]);
                }
                Err(error) => module.specifier = Err(error.to_string()),
            }
        }
        self.inner.deferred_parser_modules.borrow_mut().push(module);
    }

    fn drain_deferred_parser_modules(&self) {
        if !self.inner.parser_finished.get() {
            return;
        }
        loop {
            let ready = self
                .inner
                .deferred_parser_modules
                .borrow()
                .first()
                .is_some_and(|module| {
                    module.prepared_graph.as_ref().is_none_or(|graph| {
                        !matches!(
                            w3cos_core::promise::status(graph),
                            Some(w3cos_core::promise::PromiseStatus::Pending) | None
                        )
                    })
                });
            if !ready {
                break;
            }
            let module = self.inner.deferred_parser_modules.borrow_mut().remove(0);
            self.start_module_element(module);
        }
    }

    fn complete_document_script(&self, element: &Value) {
        if let Some(node) = crate::jsdom::node_id_of(element) {
            self.complete_document_script_node(node);
        }
    }

    fn complete_document_script_node(&self, node: u32) {
        self.inner
            .parser_blocking_elements
            .borrow_mut()
            .remove(&node);
        self.inner
            .dom_content_loaded_blockers
            .borrow_mut()
            .remove(&node);
        self.inner.document_load_blockers.borrow_mut().remove(&node);
        self.advance_document_lifecycle();
    }

    fn advance_document_lifecycle(&self) {
        if !self.inner.parser_finished.get() {
            return;
        }
        if !self.inner.dom_content_loaded_fired.get()
            && self.inner.dom_content_loaded_blockers.borrow().is_empty()
        {
            self.inner.dom_content_loaded_fired.set(true);
            crate::jsdom::dispatch_document_lifecycle_event("DOMContentLoaded");
        }
        if self.inner.dom_content_loaded_fired.get()
            && !self.inner.document_load_fired.get()
            && self.inner.document_load_blockers.borrow().is_empty()
            && !self.inner.document_load_queued.replace(true)
        {
            let weak_loader = Rc::downgrade(&self.inner);
            let generation = self.inner.document_lifecycle_generation.get();
            crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
                let Some(inner) = weak_loader.upgrade() else {
                    return Value::Undefined;
                };
                if inner.document_lifecycle_generation.get() != generation {
                    return Value::Undefined;
                }
                inner.document_load_queued.set(false);
                if !inner.document_load_fired.get()
                    && inner.dom_content_loaded_fired.get()
                    && inner.document_load_blockers.borrow().is_empty()
                {
                    inner.document_load_fired.set(true);
                    crate::jsdom::set_document_ready_state("complete");
                    crate::jsdom::dispatch_window_lifecycle_event("load");
                }
                Value::Undefined
            }));
        }
    }

    /// Whether a streaming tree builder must remain paused at the current
    /// parser-inserted classic script.
    pub fn has_pending_parser_blocking_script(&self) -> bool {
        !self.inner.parser_blocking_elements.borrow().is_empty()
    }

    fn document_load_complete(&self) -> bool {
        self.inner.document_load_fired.get()
    }

    fn classic_script_fetch_mode(element: &Value) -> ClassicScriptFetchMode {
        let cross_origin = element.call_method("getAttribute", vec![Value::string("crossorigin")]);
        if cross_origin.is_null() {
            ClassicScriptFetchMode::NoCors
        } else if cross_origin
            .to_js_string()
            .eq_ignore_ascii_case("use-credentials")
        {
            ClassicScriptFetchMode::CorsUseCredentials
        } else {
            ClassicScriptFetchMode::CorsAnonymous
        }
    }

    fn script_referrer_policy(element: &Value) -> crate::fetch::ScriptReferrerPolicy {
        let policy = element
            .call_method("getAttribute", vec![Value::string("referrerpolicy")])
            .to_js_string();
        crate::fetch::ScriptReferrerPolicy::parse((policy != "null").then_some(policy.as_str()))
    }

    fn schedule_classic_script(
        &self,
        url: &str,
        element: Value,
        order: Option<u64>,
        deferred_until_parse_end: bool,
        fetch_mode: ClassicScriptFetchMode,
    ) -> Result<Option<Value>> {
        if script_execution_route(url) == ScriptExecutionRoute::PrecompiledAot {
            let resolution = resolve_precompiled_aot_specifier(url)
                .map(|_| String::new())
                .map_err(|error| error.to_string());
            self.complete_classic_request(
                url.to_string(),
                ClassicScriptRequest {
                    element,
                    order,
                    deferred_until_parse_end,
                },
                resolution,
            );
            return Ok(None);
        }
        let integrity = element
            .call_method("getAttribute", vec![Value::string("integrity")])
            .to_js_string();
        let integrity = (integrity != "null")
            .then_some(integrity)
            .unwrap_or_default();
        let referrer_policy = Self::script_referrer_policy(&element);
        let referrer_source = self
            .inner
            .document_url
            .borrow()
            .clone()
            .unwrap_or_else(|| url.to_string());
        let fetch_key = classic_fetch_key(
            url,
            fetch_mode,
            &integrity,
            referrer_policy,
            &referrer_source,
        );
        if let Some(source) = self.inner.source_cache.borrow().get(&fetch_key).cloned() {
            self.complete_classic_request(
                url.to_string(),
                ClassicScriptRequest {
                    element,
                    order,
                    deferred_until_parse_end,
                },
                Ok(source),
            );
            return Ok(None);
        }
        if let Err(error) = self.validate_network_url(url) {
            self.complete_classic_request(
                url.to_string(),
                ClassicScriptRequest {
                    element,
                    order,
                    deferred_until_parse_end,
                },
                Err(error.to_string()),
            );
            return Ok(None);
        }
        let mut fetches = self.inner.pending_classic_fetches.borrow_mut();
        if let Some(fetch) = fetches.get_mut(&fetch_key) {
            fetch.requests.push(ClassicScriptRequest {
                element,
                order,
                deferred_until_parse_end,
            });
        } else {
            let script_fetch_mode = ScriptFetchMode::ClassicScript(fetch_mode);
            let (options, cached_response) = self.prepare_http_revalidation(url, script_fetch_mode);
            fetches.insert(
                fetch_key,
                PendingClassicFetch {
                    url: url.to_string(),
                    fetch_mode,
                    integrity,
                    referrer_source: referrer_source.clone(),
                    referrer_policy,
                    task: Some(start_script_fetch(
                        url,
                        options.clone(),
                        self.inner.module_request_origin.borrow().as_deref(),
                        classic_credentials_mode(fetch_mode),
                        fetch_mode != ClassicScriptFetchMode::NoCors,
                        &referrer_source,
                        referrer_policy,
                    )),
                    cached_response,
                    options,
                    attempts_started: 1,
                    retry_at: None,
                    requests: vec![ClassicScriptRequest {
                        element,
                        order,
                        deferred_until_parse_end,
                    }],
                },
            );
        }
        Ok(None)
    }

    fn load_source(&self, url: &str) -> Result<String> {
        if let Some(source) = self.inner.source_cache.borrow().get(url) {
            return Ok(source.clone());
        }
        self.validate_network_url(url)?;
        let fetch_mode = ScriptFetchMode::ClassicScript(ClassicScriptFetchMode::NoCors);
        let (options, cached_response) = self.prepare_http_revalidation(url, fetch_mode);
        let response = crate::fetch::fetch(url, options);
        let body = response.text().map_err(|error| anyhow!(error))?;
        let response = crate::fetch::FetchTextResponse {
            status: response.status,
            ok: response.ok,
            status_text: response.status_text,
            headers: response.headers,
            url: response.url,
            redirected: response.redirected,
            set_cookies: Vec::new(),
            body,
        };
        let response = self.apply_http_revalidation(response, cached_response)?;
        if !response.ok {
            return Err(anyhow!(
                "script fetch failed with status {} {}",
                response.status,
                response.status_text
            ));
        }
        self.validate_classic_mime_response(&response)?;
        self.check_source_size(&response.body)?;
        self.store_persistent_http_source(url, fetch_mode, &response);
        let source = response.body;
        self.inner
            .source_cache
            .borrow_mut()
            .insert(url.to_string(), source.clone());
        Ok(source)
    }

    fn check_source_size(&self, source: &str) -> Result<()> {
        if source.len() > self.inner.policy.max_source_bytes {
            return Err(anyhow!(
                "dynamic script exceeds source limit ({} > {} bytes)",
                source.len(),
                self.inner.policy.max_source_bytes
            ));
        }
        Ok(())
    }

    fn install_import_map_element(&self, element: &Value, document_url: &str) -> Result<()> {
        let source = element.get_property("textContent").to_js_string();
        self.check_source_size(&source)?;
        let value: serde_json::Value = serde_json::from_str(&source)
            .map_err(|error| anyhow!("invalid import map JSON: {error}"))?;
        let imports = parse_import_entries(value.get("imports"), "imports")?;
        let scopes = match value.get("scopes") {
            None => HashMap::new(),
            Some(scopes) => scopes
                .as_object()
                .ok_or_else(|| anyhow!("import-map 'scopes' field must be an object"))?
                .iter()
                .map(|(scope, entries)| {
                    parse_import_entries(Some(entries), &format!("scope {scope:?}"))
                        .map(|entries| (scope.clone(), entries))
                })
                .collect::<Result<HashMap<_, _>>>()?,
        };
        let import_map = self.normalize_import_map(document_url, imports, scopes)?;
        self.merge_import_map(import_map);
        Ok(())
    }

    fn instantiate_module(
        &self,
        url: &str,
        credentials_mode: ModuleCredentialsMode,
        referrer_policy: crate::fetch::ScriptReferrerPolicy,
    ) -> Result<Rc<ModuleRecord>> {
        if let Some(record) = self.inner.module_records.borrow().get(url) {
            return Ok(Rc::clone(record));
        }

        let source = self.load_source(url)?;
        let module_url = self
            .inner
            .module_final_urls
            .borrow()
            .get(url)
            .cloned()
            .unwrap_or_else(|| url.to_string());
        let module = self.lower_cached_source(&source, &module_url, CompileMode::Module)?;
        let entry = module
            .functions
            .iter()
            .find(|function| function.id == module.entry)
            .ok_or_else(|| anyhow!("module {url} has no entry function"))?;
        let bindings = entry
            .bindings
            .iter()
            .map(|binding| {
                let cell = if matches!(
                    binding.kind,
                    BindingKind::Let
                        | BindingKind::Const
                        | BindingKind::Class
                        | BindingKind::Import
                        | BindingKind::Catch
                ) {
                    uninitialized_binding_cell()
                } else {
                    binding_cell(Value::Undefined)
                };
                (binding.id, cell)
            })
            .collect();
        let vm = Vm::new(module.clone(), self.inner.policy.limits)
            .map_err(|error| anyhow!(error.to_string()))?;
        let weak_loader = Rc::downgrade(&self.inner);
        let referrer = url.to_string();
        vm.set_dynamic_import_handler(move |request| {
            let Some(inner) = weak_loader.upgrade() else {
                return w3cos_core::promise::reject(vec![Value::string(
                    "dynamic module loader is no longer available",
                )]);
            };
            ScriptLoader { inner }.dynamic_import_value(
                &referrer,
                &request,
                credentials_mode,
                referrer_policy,
            )
        });
        let record = Rc::new(ModuleRecord {
            module,
            vm,
            bindings: RefCell::new(bindings),
            state: Cell::new(ModuleState::Linked),
            evaluation_promise: RefCell::new(None),
            evaluation_error: RefCell::new(None),
            cycle_root: RefCell::new(None),
            star_export_records: RefCell::new(Vec::new()),
            star_export_external_urls: RefCell::new(Vec::new()),
            namespace: RefCell::new(None),
            credentials_mode,
            referrer_policy,
        });
        self.inner
            .module_records
            .borrow_mut()
            .insert(url.to_string(), Rc::clone(&record));
        if module_url != url {
            self.inner
                .module_records
                .borrow_mut()
                .insert(module_url.clone(), Rc::clone(&record));
        }

        let link_result = (|| {
            for request in &record.module.requested_modules {
                let dependency_url = self.resolve_module_url(url, request)?;
                if w3cos_core::module_registry::contains_native(&dependency_url)
                    && !self
                        .inner
                        .module_records
                        .borrow()
                        .contains_key(&dependency_url)
                {
                    continue;
                }
                self.instantiate_module(&dependency_url, credentials_mode, referrer_policy)?;
            }
            for request in &record.module.star_exports {
                let dependency_url = self.resolve_module_url(url, request)?;
                if w3cos_core::module_registry::contains_native(&dependency_url)
                    && !self
                        .inner
                        .module_records
                        .borrow()
                        .contains_key(&dependency_url)
                {
                    record
                        .star_export_external_urls
                        .borrow_mut()
                        .push(dependency_url);
                    continue;
                }
                let dependency =
                    self.instantiate_module(&dependency_url, credentials_mode, referrer_policy)?;
                record
                    .star_export_records
                    .borrow_mut()
                    .push(Rc::downgrade(&dependency));
            }
            for import in &record.module.imports {
                let cell = if import.specifier == "w3cos:global" {
                    binding_cell(resolve_global(&import.imported))
                } else {
                    let dependency_url = self.resolve_module_url(url, &import.specifier)?;
                    let shared_external =
                        w3cos_core::module_registry::contains_native(&dependency_url)
                            && !self
                                .inner
                                .module_records
                                .borrow()
                                .contains_key(&dependency_url);
                    if shared_external
                        && import.imported == "*"
                        && let Some(namespace) =
                            w3cos_core::module_registry::namespace(&dependency_url)
                    {
                        record
                            .bindings
                            .borrow_mut()
                            .insert(import.local, binding_cell(namespace));
                        continue;
                    }
                    if shared_external
                        && let Some(export) =
                            w3cos_core::module_registry::export(&dependency_url, &import.imported)
                    {
                        record.bindings.borrow_mut().insert(
                            import.local,
                            external_binding_cell(export.getter(), export.setter()),
                        );
                        continue;
                    }
                    let dependency = self.instantiate_module(
                        &dependency_url,
                        credentials_mode,
                        referrer_policy,
                    )?;
                    if import.imported == "*" {
                        binding_cell(module_namespace(&dependency))
                    } else {
                        match exported_cell(&dependency, &import.imported) {
                            ExportResolution::Found(cell) => cell,
                            ExportResolution::Ambiguous => {
                                return Err(anyhow!(
                                    "module {} has an ambiguous export {:?}, imported by {url}",
                                    dependency.module.specifier,
                                    import.imported
                                ));
                            }
                            ExportResolution::Missing => {
                                return Err(anyhow!(
                                    "module {} does not export {:?}, imported by {url}",
                                    dependency.module.specifier,
                                    import.imported
                                ));
                            }
                        }
                    }
                };
                record.bindings.borrow_mut().insert(import.local, cell);
            }
            Ok(())
        })();
        if let Err(error) = link_result {
            let mut records = self.inner.module_records.borrow_mut();
            records.remove(url);
            records.remove(&module_url);
            return Err(error);
        }
        self.register_shared_module_record(url, &module_url, &record);
        Ok(record)
    }

    fn register_shared_module_record(
        &self,
        requested_url: &str,
        module_url: &str,
        record: &Rc<ModuleRecord>,
    ) {
        let mut names = HashSet::new();
        collect_export_names(record, true, &mut HashSet::new(), &mut names);
        let exports = names
            .into_iter()
            .filter_map(|name| {
                let ExportResolution::Found(cell) = exported_cell(record, &name) else {
                    return None;
                };
                let getter_cell = cell.clone();
                let exported_name = name.clone();
                Some((
                    name,
                    w3cos_core::module_registry::ExportBinding::new(
                        Value::function(move |_, _| {
                            if !getter_cell.is_initialized() {
                                w3cos_core::throw_value(Value::string(&format!(
                                    "ReferenceError: export {exported_name:?} is not initialized"
                                )));
                            }
                            getter_cell.read_value()
                        }),
                        Value::Undefined,
                    ),
                ))
            })
            .collect::<HashMap<_, _>>();
        let weak_loader = Rc::downgrade(&self.inner);
        let evaluation_record = Rc::clone(record);
        let evaluator = Value::function(move |_, _| {
            let Some(inner) = weak_loader.upgrade() else {
                return w3cos_core::promise::reject(vec![Value::string(
                    "module loader is no longer available",
                )]);
            };
            ScriptLoader { inner }
                .evaluate_module_direct(&evaluation_record)
                .unwrap_or_else(|error| {
                    w3cos_core::promise::reject(vec![Value::string(&error.to_string())])
                })
        });
        w3cos_core::module_registry::register_runtime(module_url, exports, Some(evaluator));
        if module_url != requested_url {
            w3cos_core::module_registry::register_alias(requested_url, module_url);
        }
    }

    fn evaluate_module(&self, record: &Rc<ModuleRecord>) -> Result<Value> {
        if let Some(evaluation) = w3cos_core::module_registry::evaluate(&record.module.specifier) {
            return Ok(evaluation);
        }
        self.evaluate_module_direct(record)
    }

    fn evaluate_module_direct(&self, record: &Rc<ModuleRecord>) -> Result<Value> {
        let evaluation = self.evaluate_module_inner(record, &mut Vec::new())?;
        let requested_record = Rc::clone(record);
        let namespace = Value::function(move |_, _| module_namespace(&requested_record));
        Ok(evaluation.call_method("then", vec![namespace]))
    }

    fn evaluate_module_inner(
        &self,
        record: &Rc<ModuleRecord>,
        stack: &mut Vec<Rc<ModuleRecord>>,
    ) -> Result<Value> {
        // During the initial DFS, an evaluating cycle member keeps its own
        // async-evaluation promise: a later sibling may depend on that member
        // even though both will ultimately share one cycle root. Only external
        // or post-DFS Evaluate calls are projected onto the root settlement.
        let record = if record.state.get() == ModuleState::Evaluating && !stack.is_empty() {
            Rc::clone(record)
        } else {
            module_cycle_root(record)
        };
        match record.state.get() {
            ModuleState::Evaluated => {
                return Ok(w3cos_core::promise::resolve(vec![Value::Undefined]));
            }
            ModuleState::Evaluating => {
                if let Some(position) = stack
                    .iter()
                    .position(|candidate| Rc::ptr_eq(candidate, &record))
                {
                    // The instantiation phase already created live binding
                    // cells. Do not make a strongly connected module graph
                    // await its own evaluation promise. Every record on the
                    // back-edge path belongs to this SCC and future Evaluate
                    // calls must resolve through its cycle root.
                    let root = Rc::downgrade(&stack[position]);
                    for member in &stack[position..] {
                        *member.cycle_root.borrow_mut() = Some(root.clone());
                    }
                    return Ok(w3cos_core::promise::resolve(vec![Value::Undefined]));
                }
                return record.evaluation_promise.borrow().clone().ok_or_else(|| {
                    anyhow!(
                        "module {} is evaluating without an evaluation promise",
                        record.module.specifier
                    )
                });
            }
            ModuleState::Failed => {
                if let Some(evaluation) = record.evaluation_promise.borrow().clone() {
                    return Ok(evaluation);
                }
                let detail = record
                    .evaluation_error
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| record.module.specifier.clone());
                return Err(anyhow!("module evaluation previously failed: {detail}"));
            }
            ModuleState::Linked => {}
        }
        record.state.set(ModuleState::Evaluating);

        stack.push(Rc::clone(&record));
        let dependencies: Result<Vec<Value>> = (|| {
            let mut promises = Vec::new();
            for request in &record.module.requested_modules {
                let dependency_url = self.resolve_module_url(&record.module.specifier, request)?;
                if w3cos_core::module_registry::contains_native(&dependency_url)
                    && !self
                        .inner
                        .module_records
                        .borrow()
                        .contains_key(&dependency_url)
                {
                    let evaluation = w3cos_core::module_registry::evaluate(&dependency_url)
                        .unwrap_or(Value::Undefined);
                    promises.push(w3cos_core::promise::resolve(vec![evaluation]));
                    continue;
                }
                let dependency = self.instantiate_module(
                    &dependency_url,
                    record.credentials_mode,
                    record.referrer_policy,
                )?;
                let evaluation = self.evaluate_module_inner(&dependency, stack)?;
                let rejected_synchronously = matches!(
                    w3cos_core::promise::status(&evaluation),
                    Some(w3cos_core::promise::PromiseStatus::Rejected(_))
                );
                promises.push(evaluation);
                if rejected_synchronously {
                    // A rejection already observable before this DFS frame
                    // unwinds is an abrupt synchronous evaluation result (or
                    // a cached prior failure). Do not visit later siblings.
                    // Top-level-await failures remain Pending here and still
                    // allow the initial graph traversal to complete.
                    break;
                }
            }
            Ok(promises)
        })();
        stack.pop();
        let dependencies = match dependencies {
            Ok(dependencies) => dependencies,
            Err(error) => {
                record.state.set(ModuleState::Failed);
                *record.evaluation_error.borrow_mut() = Some(error.to_string());
                return Err(error);
            }
        };

        let run_record = Rc::clone(&record);
        let run_module = Value::function(move |_, _| {
            match run_record
                .vm
                .run_with_cells(run_record.bindings.borrow().clone())
            {
                Ok(result) => result,
                Err(VmError::Thrown(value)) => w3cos_core::throw_value(value),
                Err(error) => w3cos_core::throw_value(Value::string(&error.to_string())),
            }
        });
        let mut pending_dependency = false;
        let mut rejected_dependency = None;
        for dependency in &dependencies {
            match w3cos_core::promise::status(dependency) {
                Some(w3cos_core::promise::PromiseStatus::Pending) | None => {
                    pending_dependency = true;
                }
                Some(w3cos_core::promise::PromiseStatus::Fulfilled(_)) => {}
                Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => {
                    rejected_dependency = Some(reason);
                    break;
                }
            }
        }
        let execution = if let Some(reason) = rejected_dependency {
            w3cos_core::promise::reject(vec![reason])
        } else if pending_dependency {
            let dependency_ready = w3cos_core::promise::all(vec![Value::array(dependencies)]);
            dependency_ready.call_method("then", vec![run_module])
        } else {
            // InnerModuleEvaluation executes a module immediately when every
            // dependency completed synchronously. Avoiding an extra
            // Promise.all reaction here preserves DFS async-evaluation order;
            // the Promise constructor still contains thrown VM values and
            // assimilates a top-level-await Promise returned by W3VM.
            w3cos_core::promise::new(vec![Value::function(move |_, arguments| {
                let result = run_module.call(Value::Undefined, Vec::new());
                arguments
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .call(Value::Undefined, vec![result]);
                Value::Undefined
            })])
        };
        match w3cos_core::promise::status(&execution) {
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_)) => {
                record.state.set(ModuleState::Evaluated);
                let evaluation = w3cos_core::promise::resolve(vec![Value::Undefined]);
                *record.evaluation_promise.borrow_mut() = Some(evaluation.clone());
                return Ok(evaluation);
            }
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => {
                record.state.set(ModuleState::Failed);
                *record.evaluation_error.borrow_mut() = Some(reason.to_js_string());
                let evaluation = w3cos_core::promise::reject(vec![reason]);
                *record.evaluation_promise.borrow_mut() = Some(evaluation.clone());
                return Ok(evaluation);
            }
            Some(w3cos_core::promise::PromiseStatus::Pending) | None => {}
        }

        let fulfilled_record = Rc::clone(&record);
        let on_fulfilled = Value::function(move |_, _| {
            fulfilled_record.state.set(ModuleState::Evaluated);
            Value::Undefined
        });
        let rejected_record = Rc::clone(&record);
        let on_rejected = Value::function(move |_, arguments| {
            rejected_record.state.set(ModuleState::Failed);
            let reason = arguments.first().cloned().unwrap_or(Value::Undefined);
            *rejected_record.evaluation_error.borrow_mut() = Some(reason.to_js_string());
            w3cos_core::throw_value(reason)
        });
        let evaluation = execution.call_method("then", vec![on_fulfilled, on_rejected]);
        *record.evaluation_promise.borrow_mut() = Some(evaluation.clone());
        Ok(evaluation)
    }

    fn dynamic_import_value(
        &self,
        referrer: &str,
        request: &str,
        credentials_mode: ModuleCredentialsMode,
        referrer_policy: crate::fetch::ScriptReferrerPolicy,
    ) -> Value {
        match self.resolve_module_url(referrer, request) {
            Ok(url) => self.load_and_execute_module_async_guarded(
                &url,
                credentials_mode,
                None,
                String::new(),
                referrer_policy,
                Some(referrer.to_string()),
            ),
            Err(error) => w3cos_core::promise::reject(vec![Value::string(&error.to_string())]),
        }
    }

    fn resolve_module_url(&self, base: &str, request: &str) -> Result<String> {
        let resolved = {
            let import_map = self.inner.import_map.borrow();
            resolve_module_url_with_map(base, request, &import_map)?
        };
        let request_record = ResolvedModuleRequest {
            base: base.to_string(),
            request: request.to_string(),
            resolved: resolved.clone(),
        };
        self.inner
            .resolved_module_requests
            .borrow_mut()
            .insert(request_record);
        Ok(resolved)
    }
}

fn resolve_module_url_with_map(
    base: &str,
    request: &str,
    import_map: &ImportMapState,
) -> Result<String> {
    let base_url =
        Url::parse(base).map_err(|error| anyhow!("invalid module URL {base}: {error}"))?;
    let url_like = Url::parse(request)
        .map(|url| url.to_string())
        .or_else(|_| base_url.join(request).map(|url| url.to_string()))
        .ok()
        .filter(|_| {
            request.starts_with("./")
                || request.starts_with("../")
                || request.starts_with('/')
                || Url::parse(request).is_ok()
        });
    let normalized = url_like.as_deref().unwrap_or(request);
    for (_, entries) in import_map
        .scopes
        .iter()
        .filter(|(scope, _)| base.starts_with(scope))
    {
        if let Some(resolved) = resolve_import_entries(entries, normalized)? {
            return Ok(resolved);
        }
    }
    if let Some(resolved) = resolve_import_entries(&import_map.imports, normalized)? {
        return Ok(resolved);
    }
    if let Some(url) = url_like {
        return Ok(url);
    }
    Err(anyhow!(
        "bare module specifier {request:?} requires an import map"
    ))
}

fn sort_import_map_scopes(scopes: &mut [(String, ImportEntries)]) {
    scopes.sort_by(|left, right| {
        right
            .0
            .len()
            .cmp(&left.0.len())
            .then_with(|| left.0.cmp(&right.0))
    });
}

fn deferred_promise() -> (Value, Value, Value) {
    let callbacks = Rc::new(RefCell::new(None));
    let executor_callbacks = Rc::clone(&callbacks);
    let executor = Value::function(move |_, arguments| {
        *executor_callbacks.borrow_mut() = Some((
            arguments.first().cloned().unwrap_or(Value::Undefined),
            arguments.get(1).cloned().unwrap_or(Value::Undefined),
        ));
        Value::Undefined
    });
    let promise = w3cos_core::promise::new(vec![executor]);
    let (resolve, reject) = callbacks
        .borrow_mut()
        .take()
        .expect("Promise executor must run synchronously");
    (promise, resolve, reject)
}

fn finish_graph_load(loader: &ScriptLoader, load: &Rc<RefCell<ModuleGraphLoad>>) {
    let (root, integrity) = {
        let load = load.borrow();
        if load.settled || !load.pending.is_empty() {
            return;
        }
        (load.root.clone(), load.integrity.clone())
    };
    let integrity_result = loader
        .inner
        .source_cache
        .borrow()
        .get(&root)
        .ok_or_else(|| anyhow!("module source was not fetched: {root}"))
        .and_then(|source| check_integrity_metadata(source.as_bytes(), &integrity, true));
    if let Err(error) = integrity_result {
        reject_graph_load(
            load,
            &format!("module integrity check failed for {root}: {error}"),
        );
        return;
    }
    let resolve = {
        let mut load = load.borrow_mut();
        if load.settled {
            return;
        }
        load.settled = true;
        load.resolve.clone()
    };
    resolve.call(Value::Undefined, vec![Value::Undefined]);
}

fn reject_graph_load(load: &Rc<RefCell<ModuleGraphLoad>>, message: &str) {
    let reject = {
        let mut load = load.borrow_mut();
        if load.settled {
            return;
        }
        load.settled = true;
        load.pending.clear();
        load.reject.clone()
    };
    reject.call(Value::Undefined, vec![Value::string(message)]);
}

fn start_script_fetch(
    url: &str,
    options: crate::fetch::FetchOptions,
    request_origin: Option<&str>,
    credentials_mode: ModuleCredentialsMode,
    cors_mode: bool,
    referrer_source: &str,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
) -> crate::fetch::TextFetchTask {
    if let Some(request_origin) = request_origin {
        crate::fetch::fetch_script_text_async(
            url,
            options,
            request_origin.to_string(),
            crate::cookie_store_web::snapshot(),
            credentials_mode,
            cors_mode,
            referrer_source.to_string(),
            referrer_policy,
        )
    } else {
        crate::fetch::fetch_text_async_cancellable(url, options)
    }
}

fn classic_credentials_mode(fetch_mode: ClassicScriptFetchMode) -> ModuleCredentialsMode {
    match fetch_mode {
        ClassicScriptFetchMode::CorsAnonymous => ModuleCredentialsMode::SameOrigin,
        ClassicScriptFetchMode::NoCors | ClassicScriptFetchMode::CorsUseCredentials => {
            ModuleCredentialsMode::Include
        }
    }
}

fn classic_fetch_key(
    url: &str,
    fetch_mode: ClassicScriptFetchMode,
    integrity: &str,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
    referrer_source: &str,
) -> String {
    format!(
        "classic:{}:{integrity}:{}:{referrer_source}:{url}",
        match fetch_mode {
            ClassicScriptFetchMode::NoCors => "no-cors",
            ClassicScriptFetchMode::CorsAnonymous => "cors-anonymous",
            ClassicScriptFetchMode::CorsUseCredentials => "cors-use-credentials",
        },
        referrer_policy.as_str(),
    )
}

fn module_graph_key(
    root: &str,
    integrity: &str,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
    referrer_source: &str,
) -> String {
    format!(
        "{root}\u{0}{integrity}\u{0}{}\u{0}{referrer_source}",
        referrer_policy.as_str()
    )
}

fn module_fetch_key(
    url: &str,
    referrer_source: &str,
    referrer_policy: crate::fetch::ScriptReferrerPolicy,
) -> String {
    format!(
        "{url}\u{0}{referrer_source}\u{0}{}",
        referrer_policy.as_str()
    )
}

fn is_retryable_script_fetch(
    result: &std::result::Result<crate::fetch::FetchTextResponse, String>,
) -> bool {
    match result {
        Err(_) => true,
        Ok(response) => matches!(response.status, 408 | 425 | 429 | 500 | 502 | 503 | 504),
    }
}

fn script_retry_delay(
    policy: ScriptRetryPolicy,
    attempts_started: u32,
    result: &std::result::Result<crate::fetch::FetchTextResponse, String>,
) -> Option<std::time::Duration> {
    if !is_retryable_script_fetch(result) || attempts_started >= policy.max_attempts.max(1) {
        return None;
    }
    let exponent = attempts_started.saturating_sub(1).min(63);
    let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    let backoff_ms = policy
        .base_delay_ms
        .saturating_mul(multiplier)
        .min(policy.max_delay_ms);
    let retry_after = if policy.respect_retry_after {
        result
            .as_ref()
            .ok()
            .and_then(|response| header_value(&response.headers, "retry-after"))
            .and_then(parse_retry_after)
            .unwrap_or_default()
    } else {
        std::time::Duration::ZERO
    };
    Some(
        std::time::Duration::from_millis(backoff_ms)
            .max(retry_after)
            .min(std::time::Duration::from_millis(policy.max_delay_ms)),
    )
}

fn parse_retry_after(value: &str) -> Option<std::time::Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(std::time::Duration::from_secs(seconds));
    }
    let mut fields = value.split_ascii_whitespace();
    let weekday = fields.next()?;
    let day = fields.next()?.parse::<u32>().ok()?;
    let month = match fields.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = fields.next()?.parse::<i64>().ok()?;
    let mut time = fields.next()?.split(':');
    let hour = time.next()?.parse::<u32>().ok()?;
    let minute = time.next()?.parse::<u32>().ok()?;
    let second = time.next()?.parse::<u32>().ok()?;
    if fields.next()? != "GMT"
        || fields.next().is_some()
        || !matches!(
            weekday,
            "Mon," | "Tue," | "Wed," | "Thu," | "Fri," | "Sat," | "Sun,"
        )
        || hour > 23
        || minute > 59
        || second > 59
        || !valid_civil_date(year, month, day)
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let timestamp = i128::from(days)
        .checked_mul(86_400)?
        .checked_add(i128::from(hour) * 3_600)?
        .checked_add(i128::from(minute) * 60)?
        .checked_add(i128::from(second))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?;
    if timestamp <= i128::from(now.as_secs()) {
        return Some(std::time::Duration::ZERO);
    }
    let seconds = u64::try_from(timestamp - i128::from(now.as_secs())).ok()?;
    Some(std::time::Duration::from_secs(seconds))
}

fn valid_civil_date(year: i64, month: u32, day: u32) -> bool {
    let leap = year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

fn days_from_civil(mut year: i64, month: u32, day: u32) -> i64 {
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

impl ScriptLoader {
    fn poll_background_image_fetches(&self) -> usize {
        let completed = {
            let pending = self.inner.pending_background_image_fetches.borrow();
            pending
                .iter()
                .filter_map(
                    |(source, fetch)| match fetch.request.task.receiver.try_recv() {
                        Ok(result) => Some((source.clone(), result)),
                        Err(TryRecvError::Disconnected) => Some((
                            source.clone(),
                            Err("background image fetch worker disconnected".to_string()),
                        )),
                        Err(TryRecvError::Empty) => None,
                    },
                )
                .collect::<Vec<_>>()
        };
        let completed_count = completed.len();
        for (source, result) in completed {
            let Some(fetch) = self
                .inner
                .pending_background_image_fetches
                .borrow_mut()
                .remove(&source)
            else {
                continue;
            };
            let result = result
                .and_then(|response| self.complete_browser_image_request(&fetch.request, response));
            if let Err(error) = result {
                crate::image_loader::reserve_browser_source(&fetch.request.source);
                eprintln!("[w3cos] warning: {error}");
            } else {
                crate::dom::mark_dom_dirty();
            }
        }
        completed_count
    }

    fn poll_image_fetches(&self) -> usize {
        let completed = {
            let pending = self.inner.pending_image_fetches.borrow();
            pending
                .iter()
                .filter_map(
                    |(node, fetch)| match fetch.request.task.receiver.try_recv() {
                        Ok(result) => Some((*node, result)),
                        Err(TryRecvError::Disconnected) => {
                            Some((*node, Err("image fetch worker disconnected".to_string())))
                        }
                        Err(TryRecvError::Empty) => None,
                    },
                )
                .collect::<Vec<_>>()
        };
        let completed_count = completed.len();
        for (node, result) in completed {
            let Some(fetch) = self.inner.pending_image_fetches.borrow_mut().remove(&node) else {
                continue;
            };
            let result = result.and_then(|response| {
                self.complete_browser_image_request(&fetch.request, response)
                    .map(|(response, decoded)| {
                        crate::image_loader::set_density(&fetch.request.source, fetch.density);
                        let (intrinsic_width, intrinsic_height) =
                            crate::image_loader::dimensions(&fetch.request.source)
                                .unwrap_or((decoded.width, decoded.height));
                        (response.url, intrinsic_width, intrinsic_height)
                    })
            });

            match result {
                Ok((current_src, width, height)) => {
                    set_image_element_state(&fetch.element, true, &current_src, width, height);
                    crate::dom::mark_dom_dirty();
                    self.resolve_image_decode_waiters(node);
                    crate::jsdom::dispatch_element_lifecycle_event(node, "load");
                    let onload = fetch.element.get_property("onload");
                    if !onload.is_null() && !onload.is_undefined() {
                        onload.call(fetch.element.clone(), vec![]);
                    }
                }
                Err(error) => {
                    crate::image_loader::reserve_browser_source(&fetch.request.source);
                    set_image_element_state(&fetch.element, true, &fetch.request.request_url, 0, 0);
                    self.reject_image_decode_waiters(node, &error);
                    self.dispatch_image_error(&fetch.element, &error);
                }
            }
            self.complete_document_script_node(node);
        }
        completed_count
    }

    fn complete_browser_image_request(
        &self,
        request: &BrowserImageRequest,
        response: crate::fetch::FetchBinaryResponse,
    ) -> std::result::Result<
        (
            crate::fetch::FetchBinaryResponse,
            crate::image_loader::DecodedImage,
        ),
        String,
    > {
        let response = self
            .apply_binary_http_revalidation(response, request.cached_response.clone())
            .map_err(|error| error.to_string())?;
        for (url, cookie) in &response.set_cookies {
            crate::cookie_store_web::set_cookie_assignment_for_url(url, cookie, true);
        }
        if !response.ok {
            return Err(format!(
                "image fetch failed with status {} {}",
                response.status, response.status_text
            ));
        }
        if request.cors_enabled {
            validate_script_cors_headers(
                &request.request_origin,
                &response.url,
                &response.headers,
                request.credentials_mode,
            )
            .map_err(|detail| {
                format!(
                    "image CORS check failed for {} after redirect to {}: {detail}",
                    request.request_url, response.url
                )
            })?;
        }
        if response.body.len() > self.inner.policy.max_source_bytes {
            return Err(format!(
                "image exceeds source limit ({} > {} bytes)",
                response.body.len(),
                self.inner.policy.max_source_bytes
            ));
        }
        let content_type = header_value(&response.headers, "content-type").unwrap_or_default();
        if !is_supported_image_mime_type(content_type) {
            return Err(format!(
                "image MIME check failed for {}: received {}",
                response.url,
                if content_type.is_empty() {
                    "<missing>"
                } else {
                    content_type
                }
            ));
        }
        let decoded = crate::image_loader::decode_and_install(&request.source, &response.body)
            .map_err(|error| format!("image decode failed for {}: {error}", response.url))?;
        if !response.redirected && response.url == request.request_url {
            self.store_image_source(&request.cache_key, &request.request_headers, &response);
        }
        Ok((response, decoded))
    }

    fn store_image_source(
        &self,
        cache_key: &crate::browser_http_cache::CacheKey,
        request_headers: &HashMap<String, String>,
        response: &crate::fetch::FetchBinaryResponse,
    ) {
        let cached = crate::browser_http_cache::CachedResponse::from_network(
            cache_key,
            response.url.clone(),
            response.status,
            response.status_text.clone(),
            response.headers.clone(),
            response.body.clone(),
        );
        let result = crate::browser_http_cache::store(
            &self.browser_http_cache_policy(),
            cache_key,
            request_headers,
            cached,
            true,
        );
        let mut stats = self.inner.http_source_cache_stats.borrow_mut();
        match result {
            Ok(outcome) if outcome.wrote => {
                stats.writes = stats.writes.saturating_add(1);
                stats.evictions = stats.evictions.saturating_add(outcome.response_evictions);
                drop(stats);
                if outcome.other_evictions > 0 {
                    let mut cache = self.inner.compiled_source_cache.borrow_mut();
                    cache.persistent_evictions = cache
                        .persistent_evictions
                        .saturating_add(outcome.other_evictions);
                }
            }
            Ok(_) => {}
            Err(_) => stats.errors = stats.errors.saturating_add(1),
        }
    }

    fn poll_source_fetches(&self) -> usize {
        let image_completed = self.poll_image_fetches();
        let background_image_completed = self.poll_background_image_fetches();
        let stylesheet_completed = self.poll_stylesheet_fetches();
        let stylesheet_font_completed = self.poll_stylesheet_font_fetches();
        let classic_completed = self.poll_classic_fetches();
        let (retry_started, completions) = {
            let mut fetches = self.inner.pending_source_fetches.borrow_mut();
            let now = std::time::Instant::now();
            let mut retry_started = 0;
            for fetch in fetches.values_mut() {
                if fetch.task.is_none() && fetch.retry_at.is_some_and(|deadline| deadline <= now) {
                    fetch.task = Some(start_script_fetch(
                        &fetch.url,
                        fetch.options.clone(),
                        fetch.request_origin.as_deref(),
                        fetch.credentials_mode,
                        true,
                        &fetch.referrer_source,
                        fetch.referrer_policy,
                    ));
                    fetch.retry_at = None;
                    fetch.attempts_started = fetch.attempts_started.saturating_add(1);
                    retry_started += 1;
                }
            }
            let fetch_keys = fetches.keys().cloned().collect::<Vec<_>>();
            let mut completions = Vec::new();
            for fetch_key in fetch_keys {
                let result = match fetches.get(&fetch_key) {
                    Some(fetch) => {
                        fetch
                            .task
                            .as_ref()
                            .and_then(|task| match task.receiver.try_recv() {
                                Ok(result) => Some(result),
                                Err(TryRecvError::Disconnected) => {
                                    Some(Err("module fetch worker disconnected".to_string()))
                                }
                                Err(TryRecvError::Empty) => None,
                            })
                    }
                    None => None,
                };
                if let Some(result) = result {
                    let fetch = fetches
                        .remove(&fetch_key)
                        .expect("completed module fetch remains registered");
                    let completion_order = fetch
                        .task
                        .as_ref()
                        .map(crate::fetch::TextFetchTask::completion_order)
                        .unwrap_or(0);
                    completions.push((completion_order, fetch_key, fetch, result));
                }
            }
            completions.sort_by_key(|(completion_order, _, _, _)| *completion_order);
            (retry_started, completions)
        };
        if retry_started > 0 {
            let mut stats = self.inner.script_retry_stats.borrow_mut();
            stats.started = stats.started.saturating_add(retry_started);
        }

        let completed = completions.len();
        for (_, fetch_key, mut fetch, result) in completions {
            let url = fetch.url.clone();
            if let Some(delay) =
                script_retry_delay(self.inner.policy.retry, fetch.attempts_started, &result)
            {
                if let Ok(response) = &result {
                    self.store_script_response_cookies(response, fetch.credentials_mode);
                }
                fetch.task = None;
                fetch.retry_at = Some(std::time::Instant::now() + delay);
                self.inner
                    .pending_source_fetches
                    .borrow_mut()
                    .insert(fetch_key, fetch);
                let mut stats = self.inner.script_retry_stats.borrow_mut();
                stats.scheduled = stats.scheduled.saturating_add(1);
                continue;
            }
            if is_retryable_script_fetch(&result) {
                let mut stats = self.inner.script_retry_stats.borrow_mut();
                stats.exhausted = stats.exhausted.saturating_add(1);
            }
            let retried = fetch.attempts_started > 1;
            let result = result
                .map_err(anyhow::Error::msg)
                .and_then(|response| self.apply_http_revalidation(response, fetch.cached_response));
            let (source, final_url) = match result {
                Ok(response) if response.ok => {
                    self.store_script_response_cookies(&response, fetch.credentials_mode);
                    let final_url = response.url.clone();
                    let validation = self
                        .validate_module_cors_response(&url, &response, fetch.credentials_mode)
                        .and_then(|_| self.validate_module_mime_response(&response))
                        .and_then(|_| self.check_source_size(&response.body));
                    if validation.is_ok() {
                        self.store_persistent_http_source(
                            &url,
                            ScriptFetchMode::Module(fetch.credentials_mode),
                            &response,
                        );
                    }
                    (validation.map(|_| response.body), final_url)
                }
                Ok(response) => {
                    self.store_script_response_cookies(&response, fetch.credentials_mode);
                    (
                        Err(anyhow!(
                            "script fetch failed with status {} {}",
                            response.status,
                            response.status_text
                        )),
                        response.url,
                    )
                }
                Err(error) => (Err(error), url.clone()),
            };
            if retried && source.is_ok() {
                let mut stats = self.inner.script_retry_stats.borrow_mut();
                stats.succeeded = stats.succeeded.saturating_add(1);
            }
            if let Ok(source) = &source {
                let mut sources = self.inner.source_cache.borrow_mut();
                sources.insert(url.clone(), source.clone());
                sources.insert(final_url.clone(), source.clone());
                drop(sources);
                let mut aliases = self.inner.module_final_urls.borrow_mut();
                aliases.insert(url.clone(), final_url.clone());
                aliases
                    .entry(final_url.clone())
                    .or_insert_with(|| final_url.clone());
            }

            let graph_loads = self
                .inner
                .module_graph_loads
                .borrow()
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for load in graph_loads {
                let waiting = {
                    let mut load = load.borrow_mut();
                    !load.settled && load.pending.remove(&fetch_key)
                };
                if !waiting {
                    continue;
                }
                match &source {
                    Ok(_) => {
                        if let Err(error) = self.discover_graph_dependencies(&load, &url) {
                            reject_graph_load(&load, &error.to_string());
                        } else {
                            finish_graph_load(self, &load);
                        }
                    }
                    Err(error) => reject_graph_load(&load, &error.to_string()),
                }
            }
        }
        completed
            + image_completed
            + background_image_completed
            + classic_completed
            + stylesheet_completed
            + stylesheet_font_completed
            + retry_started as usize
    }

    fn poll_classic_fetches(&self) -> usize {
        let (retry_started, completions) = {
            let mut fetches = self.inner.pending_classic_fetches.borrow_mut();
            let now = std::time::Instant::now();
            let mut retry_started = 0;
            for fetch in fetches.values_mut() {
                if fetch.task.is_none() && fetch.retry_at.is_some_and(|deadline| deadline <= now) {
                    fetch.task = Some(start_script_fetch(
                        &fetch.url,
                        fetch.options.clone(),
                        self.inner.module_request_origin.borrow().as_deref(),
                        classic_credentials_mode(fetch.fetch_mode),
                        fetch.fetch_mode != ClassicScriptFetchMode::NoCors,
                        &fetch.referrer_source,
                        fetch.referrer_policy,
                    ));
                    fetch.retry_at = None;
                    fetch.attempts_started = fetch.attempts_started.saturating_add(1);
                    retry_started += 1;
                }
            }
            let keys = fetches.keys().cloned().collect::<Vec<_>>();
            let mut completions = Vec::new();
            for key in keys {
                let result = match fetches.get(&key) {
                    Some(fetch) => {
                        fetch
                            .task
                            .as_ref()
                            .and_then(|task| match task.receiver.try_recv() {
                                Ok(result) => Some(result),
                                Err(TryRecvError::Disconnected) => Some(Err(
                                    "classic script fetch worker disconnected".to_string(),
                                )),
                                Err(TryRecvError::Empty) => None,
                            })
                    }
                    None => None,
                };
                if let Some(result) = result {
                    let fetch = fetches
                        .remove(&key)
                        .expect("completed classic fetch remains registered");
                    let completion_order = fetch
                        .task
                        .as_ref()
                        .map(crate::fetch::TextFetchTask::completion_order)
                        .unwrap_or(0);
                    completions.push((completion_order, key, fetch, result));
                }
            }
            completions.sort_by_key(|(completion_order, _, _, _)| *completion_order);
            (retry_started, completions)
        };
        if retry_started > 0 {
            let mut stats = self.inner.script_retry_stats.borrow_mut();
            stats.started = stats.started.saturating_add(retry_started);
        }

        let completed = completions.len();
        for (_, key, mut fetch, result) in completions {
            if let Some(delay) =
                script_retry_delay(self.inner.policy.retry, fetch.attempts_started, &result)
            {
                if let Ok(response) = &result {
                    self.store_script_response_cookies(
                        response,
                        classic_credentials_mode(fetch.fetch_mode),
                    );
                }
                fetch.task = None;
                fetch.retry_at = Some(std::time::Instant::now() + delay);
                self.inner
                    .pending_classic_fetches
                    .borrow_mut()
                    .insert(key, fetch);
                let mut stats = self.inner.script_retry_stats.borrow_mut();
                stats.scheduled = stats.scheduled.saturating_add(1);
                continue;
            }
            if is_retryable_script_fetch(&result) {
                let mut stats = self.inner.script_retry_stats.borrow_mut();
                stats.exhausted = stats.exhausted.saturating_add(1);
            }
            let retried = fetch.attempts_started > 1;
            let url = fetch.url.clone();
            let result = result
                .map_err(anyhow::Error::msg)
                .and_then(|response| self.apply_http_revalidation(response, fetch.cached_response));
            if let Ok(response) = &result {
                self.store_script_response_cookies(
                    response,
                    classic_credentials_mode(fetch.fetch_mode),
                );
            }
            let source = match result {
                Ok(response) if response.ok => {
                    if let Err(error) =
                        self.validate_classic_cors_response(&url, &response, fetch.fetch_mode)
                    {
                        Err(error.to_string())
                    } else if let Err(error) = self.validate_classic_mime_response(&response) {
                        Err(error.to_string())
                    } else {
                        let integrity_eligible = fetch.fetch_mode != ClassicScriptFetchMode::NoCors
                            || self
                                .inner
                                .module_request_origin
                                .borrow()
                                .as_deref()
                                .is_none_or(|origin| {
                                    Url::parse(&response.url).is_ok_and(|response_url| {
                                        response_url.origin().ascii_serialization() == origin
                                    })
                                });
                        let validation = self.check_source_size(&response.body).and_then(|_| {
                            check_integrity_metadata(
                                response.body.as_bytes(),
                                &fetch.integrity,
                                integrity_eligible,
                            )
                        });
                        if validation.is_ok() {
                            self.store_persistent_http_source(
                                &url,
                                ScriptFetchMode::ClassicScript(fetch.fetch_mode),
                                &response,
                            );
                        }
                        validation
                            .map(|_| response.body)
                            .map_err(|error| error.to_string())
                    }
                }
                Ok(response) => Err(format!(
                    "script fetch failed with status {} {}",
                    response.status, response.status_text
                )),
                Err(error) => Err(error.to_string()),
            };
            if retried && source.is_ok() {
                let mut stats = self.inner.script_retry_stats.borrow_mut();
                stats.succeeded = stats.succeeded.saturating_add(1);
            }
            if let Ok(source) = &source {
                self.inner.source_cache.borrow_mut().insert(
                    classic_fetch_key(
                        &url,
                        fetch.fetch_mode,
                        &fetch.integrity,
                        fetch.referrer_policy,
                        &fetch.referrer_source,
                    ),
                    source.clone(),
                );
            }
            for request in fetch.requests {
                self.complete_classic_request(url.clone(), request, source.clone());
            }
        }
        completed + retry_started as usize
    }

    fn complete_classic_request(
        &self,
        url: String,
        request: ClassicScriptRequest,
        source: std::result::Result<String, String>,
    ) {
        if request.deferred_until_parse_end {
            let order = request
                .order
                .expect("deferred parser scripts retain document order");
            self.inner
                .ready_deferred_classic_scripts
                .borrow_mut()
                .insert(order, (url, request, source));
            self.drain_deferred_classic_scripts();
        } else if let Some(order) = request.order {
            self.inner
                .ready_ordered_classic_scripts
                .borrow_mut()
                .insert(order, (url, request, source));
            self.drain_ordered_classic_scripts();
        } else {
            self.execute_classic_request(&url, request.element, source);
        }
    }

    fn drain_ordered_classic_scripts(&self) {
        loop {
            let order = self.inner.next_classic_execution.get();
            if self
                .inner
                .cancelled_classic_orders
                .borrow_mut()
                .remove(&order)
            {
                self.inner.next_classic_execution.set(order + 1);
                continue;
            }
            let Some((url, request, source)) = self
                .inner
                .ready_ordered_classic_scripts
                .borrow_mut()
                .remove(&order)
            else {
                break;
            };
            self.inner.next_classic_execution.set(order + 1);
            self.execute_classic_request(&url, request.element, source);
        }
    }

    fn drain_deferred_classic_scripts(&self) {
        if !self.inner.parser_finished.get() {
            return;
        }
        loop {
            let order = self.inner.next_deferred_classic_execution.get();
            if self
                .inner
                .cancelled_deferred_classic_orders
                .borrow_mut()
                .remove(&order)
            {
                self.inner.next_deferred_classic_execution.set(order + 1);
                continue;
            }
            let Some((url, request, source)) = self
                .inner
                .ready_deferred_classic_scripts
                .borrow_mut()
                .remove(&order)
            else {
                break;
            };
            self.inner.next_deferred_classic_execution.set(order + 1);
            self.execute_classic_request(&url, request.element, source);
        }
    }

    fn execute_classic_request(
        &self,
        url: &str,
        element: Value,
        source: std::result::Result<String, String>,
    ) {
        let execution = source
            .map_err(|error| anyhow!(error))
            .and_then(|source| self.execute_source(&source, url));
        match execution {
            Ok(_) => {
                let onload = element.get_property("onload");
                if !onload.is_null() && !onload.is_undefined() {
                    onload.call(element.clone(), vec![]);
                }
            }
            Err(error) => {
                let onerror = element.get_property("onerror");
                if !onerror.is_null() && !onerror.is_undefined() {
                    onerror.call(element.clone(), vec![Value::string(&error.to_string())]);
                }
            }
        }
        self.complete_document_script(&element);
    }
}

/// Poll completed background classic/module fetches and advance their browser
/// lifecycle callbacks or graph promises.
/// Called by the browser task pump; all W3IR/W3VM work remains on this thread.
pub fn poll_script_fetches() -> usize {
    let loaders = SCRIPT_LOADERS.with(|registry| {
        let mut registry = registry.borrow_mut();
        let loaders = registry
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        registry.retain(|loader| loader.strong_count() > 0);
        loaders
    });
    loaders
        .into_iter()
        .map(|inner| ScriptLoader { inner }.poll_source_fetches())
        .sum()
}

pub fn has_pending_script_fetches() -> bool {
    SCRIPT_LOADERS.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .any(|inner| {
                !inner.pending_source_fetches.borrow().is_empty()
                    || !inner.pending_classic_fetches.borrow().is_empty()
                    || !inner.pending_stylesheet_fetches.borrow().is_empty()
                    || !inner.pending_stylesheet_font_fetches.borrow().is_empty()
                    || !inner.pending_image_fetches.borrow().is_empty()
            })
    })
}

pub(crate) fn active_document_font_fetch_limits() -> Option<(bool, usize)> {
    ACTIVE_DOCUMENT_LOADER.with(|active| {
        active.borrow().as_ref().map(|(loader, _)| {
            (
                loader.inner.policy.allow_network,
                loader.inner.policy.max_source_bytes,
            )
        })
    })
}

pub(crate) fn request_stylesheet_fonts_for_text(
    style: &w3cos_std::style::Style,
    text: &str,
) -> usize {
    ACTIVE_DOCUMENT_LOADER.with(|active| {
        let loader = active.borrow().as_ref().map(|(loader, _)| loader.clone());
        loader.map_or(0, |loader| {
            loader.demand_stylesheet_fonts_for_text(style, text)
        })
    })
}

pub(crate) fn request_stylesheet_font_faces(faces: &[Value], text: &str) -> usize {
    ACTIVE_DOCUMENT_LOADER.with(|active| {
        let loader = active.borrow().as_ref().map(|(loader, _)| loader.clone());
        loader.map_or(0, |loader| loader.demand_stylesheet_font_faces(faces, text))
    })
}

pub(crate) fn decode_image_element(node: u32) -> Value {
    let Some((loader, document_url)) = ACTIVE_DOCUMENT_LOADER.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|(loader, document_url)| (loader.clone(), document_url.clone()))
    }) else {
        return w3cos_core::promise::reject(vec![image_encoding_error(
            "The image has no active Browser document loader",
        )]);
    };
    let mutated = DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow_mut().remove(&node));
    if mutated && let Ok(base) = Url::parse(&document_url) {
        loader.invalidate_image_nodes(&HashSet::from([node]), false);
        loader.prepare_image_node(node, &base);
    }
    let element = crate::jsdom::element_value(node);
    if element.get_property("complete").to_bool() {
        if element.get_property("naturalWidth").to_number() > 0.0 {
            return w3cos_core::promise::resolve(vec![Value::Undefined]);
        }
        return w3cos_core::promise::reject(vec![image_encoding_error(
            "The image could not be decoded",
        )]);
    }
    let (promise, resolve, reject) = deferred_promise();
    loader
        .inner
        .image_decode_waiters
        .borrow_mut()
        .entry(node)
        .or_default()
        .push(ImageDecodeWaiter { resolve, reject });
    schedule_document_script_pump();
    promise
}

fn image_encoding_error(message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, "EncodingError")
}

fn set_image_element_state(
    element: &Value,
    complete: bool,
    current_src: &str,
    natural_width: u32,
    natural_height: u32,
) {
    element.set_property("__w3cos_image_complete", Value::Bool(complete));
    element.set_property("__w3cos_image_current_src", Value::string(current_src));
    element.set_property(
        "__w3cos_image_natural_width",
        Value::Number(f64::from(natural_width)),
    );
    element.set_property(
        "__w3cos_image_natural_height",
        Value::Number(f64::from(natural_height)),
    );
}

/// Earliest polling or retry deadline across live script loaders.
pub fn next_script_fetch_deadline() -> Option<std::time::Instant> {
    SCRIPT_LOADERS.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .filter_map(|inner| ScriptLoader { inner }.next_fetch_deadline())
            .min()
    })
}

/// Backward-compatible module-loader entry point.
pub fn poll_module_fetches() -> usize {
    poll_script_fetches()
}

/// Backward-compatible module-loader pending check.
pub fn has_pending_module_fetches() -> bool {
    has_pending_script_fetches()
}

fn canonical_module_url(url: &str) -> Result<String> {
    Url::parse(url)
        .map(|url| url.to_string())
        .map_err(|error| anyhow!("invalid module URL {url}: {error}"))
}

/// File URLs identify build-time-known code and therefore resolve only through
/// the native AOT module registry. Network and inline/runtime-created sources
/// retain the SWC → W3IR → W3VM path.
fn script_execution_route(specifier: &str) -> ScriptExecutionRoute {
    match Url::parse(specifier) {
        Ok(url) if url.scheme() == "file" => ScriptExecutionRoute::PrecompiledAot,
        _ => ScriptExecutionRoute::RuntimeW3vm,
    }
}

fn resolve_precompiled_aot_specifier(specifier: &str) -> Result<String> {
    let canonical = canonical_module_url(specifier)?;
    if w3cos_core::module_registry::contains_native(&canonical) {
        return Ok(canonical);
    }

    let url = Url::parse(&canonical)
        .map_err(|error| anyhow!("invalid precompiled script URL {specifier}: {error}"))?;
    let path = url.to_file_path().map_err(|()| {
        anyhow!("file script URL cannot be converted to a local build path: {canonical}")
    })?;
    let path = path.to_str().ok_or_else(|| {
        anyhow!("file script URL is not valid UTF-8 and cannot identify an AOT module: {canonical}")
    })?;
    if w3cos_core::module_registry::contains_native(path) {
        w3cos_core::module_registry::register_alias(&canonical, path);
        return Ok(canonical);
    }

    Err(anyhow!(
        "file script requires a precompiled native AOT module registered for {canonical}; file sources never fall back to W3VM interpretation"
    ))
}

fn precompiled_aot_evaluation(specifier: &str) -> Result<Value> {
    let canonical = resolve_precompiled_aot_specifier(specifier)?;
    let evaluation = w3cos_core::module_registry::evaluate(&canonical).ok_or_else(|| {
        anyhow!("precompiled AOT module disappeared before evaluation: {canonical}")
    })?;
    let namespace_specifier = canonical.clone();
    let namespace = Value::function(move |_, _| {
        w3cos_core::module_registry::namespace(&namespace_specifier).unwrap_or(Value::Undefined)
    });
    Ok(evaluation.call_method("then", vec![namespace]))
}

/// Stable FNV-1a digest used only as the compact lookup component. Cache hits
/// also compare the complete source, so a digest collision cannot return the
/// wrong W3IR module.
fn stable_source_hash(source: &[u8]) -> u64 {
    extend_stable_hash(0xcbf29ce484222325_u64, source)
}

fn extend_stable_hash(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn persistent_compiled_cache_identity(key: &CompiledSourceCacheKey) -> u64 {
    let mut hash = extend_stable_hash(0xcbf29ce484222325_u64, key.resolved_url.as_bytes());
    hash = extend_stable_hash(
        hash,
        &PERSISTENT_COMPILED_CACHE_SCHEMA_VERSION.to_le_bytes(),
    );
    hash = extend_stable_hash(hash, &key.w3ir_format_version.to_le_bytes());
    extend_stable_hash(
        hash,
        &[match key.compile_mode {
            CompileMode::ClassicScript => 0,
            CompileMode::Module => 1,
        }],
    )
}

fn persistent_http_source_cache_identity(request_url: &str, fetch_mode: ScriptFetchMode) -> u64 {
    crate::browser_http_cache::cache_identity(&crate::browser_http_cache::CacheKey {
        request_url: request_url.to_string(),
        partition: script_http_cache_partition(fetch_mode),
    })
}

fn script_http_cache_partition(fetch_mode: ScriptFetchMode) -> String {
    match fetch_mode {
        ScriptFetchMode::ClassicScript(ClassicScriptFetchMode::NoCors) => "script:classic:no-cors",
        ScriptFetchMode::ClassicScript(ClassicScriptFetchMode::CorsAnonymous) => {
            "script:classic:cors-anonymous"
        }
        ScriptFetchMode::ClassicScript(ClassicScriptFetchMode::CorsUseCredentials) => {
            "script:classic:cors-include"
        }
        ScriptFetchMode::Module(ModuleCredentialsMode::Omit) => "script:module:omit",
        ScriptFetchMode::Module(ModuleCredentialsMode::SameOrigin) => "script:module:same-origin",
        ScriptFetchMode::Module(ModuleCredentialsMode::Include) => "script:module:include",
    }
    .to_string()
}

fn stylesheet_http_cache_key(
    request_url: &str,
    request_origin: &str,
    credentials_mode: ModuleCredentialsMode,
    cors_enabled: bool,
) -> crate::browser_http_cache::CacheKey {
    let credentials = match credentials_mode {
        ModuleCredentialsMode::Omit => "omit",
        ModuleCredentialsMode::SameOrigin => "same-origin",
        ModuleCredentialsMode::Include => "include",
    };
    let mode = if cors_enabled { "cors" } else { "no-cors" };
    crate::browser_http_cache::CacheKey {
        request_url: request_url.to_string(),
        partition: format!(
            "stylesheet:request-origin={request_origin}:credentials={credentials}:mode={mode}"
        ),
    }
}

fn stylesheet_font_http_cache_key(
    request_url: &str,
    request_origin: &str,
) -> crate::browser_http_cache::CacheKey {
    crate::browser_http_cache::CacheKey {
        request_url: request_url.to_string(),
        partition: format!("font:request-origin={request_origin}:credentials=omit:mode=cors"),
    }
}

fn image_http_cache_key(
    request_url: &str,
    request_origin: &str,
    credentials_mode: ModuleCredentialsMode,
    cors_enabled: bool,
) -> crate::browser_http_cache::CacheKey {
    let credentials = match credentials_mode {
        ModuleCredentialsMode::Omit => "omit",
        ModuleCredentialsMode::SameOrigin => "same-origin",
        ModuleCredentialsMode::Include => "include",
    };
    let mode = if cors_enabled { "cors" } else { "no-cors" };
    crate::browser_http_cache::CacheKey {
        request_url: request_url.to_string(),
        partition: format!(
            "image:request-origin={request_origin}:credentials={credentials}:mode={mode}"
        ),
    }
}

fn is_supported_image_mime_type(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("image/") || essence == "application/octet-stream"
}

fn supported_stylesheet_font_format(format: Option<&str>) -> bool {
    format.is_none_or(|format| {
        matches!(
            format.trim().to_ascii_lowercase().as_str(),
            "truetype" | "opentype" | "ttf" | "otf" | "woff" | "woff2"
        )
    })
}

pub(crate) fn is_supported_font_mime_type(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "font/ttf"
            | "font/otf"
            | "font/woff"
            | "font/woff2"
            | "application/font-woff"
            | "application/font-woff2"
            | "application/x-font-woff"
            | "application/x-font-ttf"
            | "application/x-font-opentype"
            | "application/font-sfnt"
            | "application/octet-stream"
    )
}

fn native_stylesheet_font_face(
    face: &w3cos_compiler::esm_css::StylesheetFontFace,
    source: crate::font_face::FontSource,
) -> crate::font_face::FontFace {
    let display = match face
        .display
        .as_deref()
        .unwrap_or("auto")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "block" => crate::font_face::FontDisplay::Block,
        "swap" => crate::font_face::FontDisplay::Swap,
        "fallback" => crate::font_face::FontDisplay::Fallback,
        "optional" => crate::font_face::FontDisplay::Optional,
        _ => crate::font_face::FontDisplay::Auto,
    };
    crate::font_face::FontFace {
        family: face.family.clone(),
        src: source,
        weight: crate::font_face::FontWeight::from_str(face.weight.as_deref().unwrap_or("normal")),
        style: crate::font_face::FontFaceStyle::from_str(face.style.as_deref().unwrap_or("normal")),
        display,
        unicode_range: face.unicode_range.clone(),
    }
}

fn combine_stylesheet_media(parent: Option<&str>, child: Option<&str>) -> Option<String> {
    match (
        parent.map(str::trim).filter(|value| !value.is_empty()),
        child.map(str::trim).filter(|value| !value.is_empty()),
    ) {
        (Some(parent), Some(child)) => Some(format!("({parent}) and ({child})")),
        (Some(parent), None) => Some(parent.to_string()),
        (None, Some(child)) => Some(child.to_string()),
        (None, None) => None,
    }
}

fn append_stylesheet_source(target: &mut String, source: &str, media: Option<&str>) {
    if !target.is_empty() {
        target.push('\n');
    }
    if let Some(media) = media.map(str::trim).filter(|media| !media.is_empty()) {
        target.push_str("@media ");
        target.push_str(media);
        target.push_str(" {\n");
        target.push_str(source);
        target.push_str("\n}");
    } else {
        target.push_str(source);
    }
}

fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn is_javascript_mime_type(value: &str) -> bool {
    let essence = value.split(';').next().unwrap_or_default().trim();
    is_javascript_mime_type_essence_match(essence)
}

fn is_javascript_mime_type_essence_match(value: &str) -> bool {
    let essence = value.trim();
    [
        "application/ecmascript",
        "application/javascript",
        "application/x-ecmascript",
        "application/x-javascript",
        "text/ecmascript",
        "text/javascript",
        "text/javascript1.0",
        "text/javascript1.1",
        "text/javascript1.2",
        "text/javascript1.3",
        "text/javascript1.4",
        "text/javascript1.5",
        "text/jscript",
        "text/livescript",
        "text/x-ecmascript",
        "text/x-javascript",
    ]
    .iter()
    .any(|candidate| essence.eq_ignore_ascii_case(candidate))
}

fn parse_import_entries(value: Option<&serde_json::Value>, field: &str) -> Result<ImportEntries> {
    match value {
        None => Ok(HashMap::new()),
        Some(value) => value
            .as_object()
            .ok_or_else(|| anyhow!("import-map {field} must be an object"))?
            .iter()
            .map(|(specifier, target)| {
                let target = if target.is_null() {
                    ImportMapTarget::Blocked
                } else {
                    ImportMapTarget::Address(
                        target
                            .as_str()
                            .ok_or_else(|| {
                                anyhow!(
                                    "import-map target for {specifier:?} in {field} must be a string URL or null"
                                )
                            })?
                            .to_string(),
                    )
                };
                Ok((specifier.clone(), target))
            })
            .collect(),
    }
}

fn import_addresses(entries: HashMap<String, String>) -> ImportEntries {
    entries
        .into_iter()
        .map(|(specifier, target)| (specifier, ImportMapTarget::Address(target)))
        .collect()
}

fn normalize_import_entries(base: &Url, entries: ImportEntries) -> Result<ImportEntries> {
    entries
        .into_iter()
        .map(|(specifier, target)| {
            if specifier.is_empty() {
                return Err(anyhow!("import-map specifier keys must not be empty"));
            }
            let normalized_specifier = if Url::parse(&specifier).is_ok()
                || specifier.starts_with("./")
                || specifier.starts_with("../")
                || specifier.starts_with('/')
            {
                base.join(&specifier)
                    .map_err(|error| {
                        anyhow!("invalid import-map specifier {specifier:?}: {error}")
                    })?
                    .to_string()
            } else {
                specifier.clone()
            };
            let target = match target {
                ImportMapTarget::Blocked => ImportMapTarget::Blocked,
                ImportMapTarget::Address(target) => {
                    let target = base
                        .join(&target)
                        .map_err(|error| anyhow!("invalid import-map target {target:?}: {error}"))?
                        .to_string();
                    if normalized_specifier.ends_with('/') && !target.ends_with('/') {
                        return Err(anyhow!(
                            "import-map prefix target for {specifier:?} must end with '/'"
                        ));
                    }
                    ImportMapTarget::Address(target)
                }
            };
            Ok((normalized_specifier, target))
        })
        .collect()
}

pub(crate) fn validate_script_cors_headers(
    request_origin: &str,
    response_url: &str,
    headers: &HashMap<String, String>,
    credentials_mode: ModuleCredentialsMode,
) -> std::result::Result<(), String> {
    let credentials_mode = match credentials_mode {
        ModuleCredentialsMode::Omit => crate::fetch::BrowserCredentialsMode::Omit,
        ModuleCredentialsMode::SameOrigin => crate::fetch::BrowserCredentialsMode::SameOrigin,
        ModuleCredentialsMode::Include => crate::fetch::BrowserCredentialsMode::Include,
    };
    crate::fetch::validate_browser_cors_headers(
        request_origin,
        response_url,
        headers,
        credentials_mode,
    )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum IntegrityAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl IntegrityAlgorithm {
    fn ring(self) -> &'static ring::digest::Algorithm {
        match self {
            Self::Sha256 => &ring::digest::SHA256,
            Self::Sha384 => &ring::digest::SHA384,
            Self::Sha512 => &ring::digest::SHA512,
        }
    }
}

fn check_integrity_metadata(source: &[u8], metadata: &str, eligible: bool) -> Result<()> {
    use base64::Engine as _;

    let mut parsed = metadata
        .split_ascii_whitespace()
        .filter_map(|token| {
            let expression = token
                .split_once('?')
                .map_or(token, |(expression, _)| expression);
            let (algorithm, expected) = expression.split_once('-')?;
            let algorithm = match algorithm {
                "sha256" => IntegrityAlgorithm::Sha256,
                "sha384" => IntegrityAlgorithm::Sha384,
                "sha512" => IntegrityAlgorithm::Sha512,
                _ => return None,
            };
            let decoded = [
                &base64::engine::general_purpose::STANDARD,
                &base64::engine::general_purpose::STANDARD_NO_PAD,
                &base64::engine::general_purpose::URL_SAFE,
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            ]
            .into_iter()
            .find_map(|engine| engine.decode(expected).ok())?;
            Some((algorithm, decoded))
        })
        .collect::<Vec<_>>();
    let Some(strongest) = parsed.iter().map(|(algorithm, _)| *algorithm).max() else {
        // Unsupported or malformed metadata is ignored for forward
        // compatibility, matching the SRI parsing algorithm.
        return Ok(());
    };
    if !eligible {
        return Err(anyhow!(
            "subresource integrity requires a same-origin or CORS-enabled response"
        ));
    }
    parsed.retain(|(algorithm, _)| *algorithm == strongest);
    let actual = ring::digest::digest(strongest.ring(), source);
    if parsed
        .iter()
        .any(|(_, expected)| expected.as_slice() == actual.as_ref())
    {
        return Ok(());
    }
    Err(anyhow!(
        "subresource integrity mismatch for the strongest supplied hash"
    ))
}

fn resolve_import_entries(entries: &ImportEntries, request: &str) -> Result<Option<String>> {
    if let Some(target) = entries.get(request) {
        return resolve_import_target(request, request, target);
    }
    let prefix = entries
        .keys()
        .filter(|key| key.ends_with('/') && request.starts_with(key.as_str()))
        .max_by(|left, right| left.len().cmp(&right.len()).then_with(|| right.cmp(left)));
    let Some(prefix) = prefix else {
        return Ok(None);
    };
    let target = entries.get(prefix).expect("selected import-map key");
    resolve_import_target(request, prefix, target)
}

fn resolve_import_target(
    request: &str,
    matched: &str,
    target: &ImportMapTarget,
) -> Result<Option<String>> {
    let ImportMapTarget::Address(target) = target else {
        return Err(anyhow!(
            "import map blocked module specifier {request:?} through mapping {matched:?}"
        ));
    };
    if matched == request {
        return Ok(Some(target.clone()));
    }
    Url::parse(target)
        .and_then(|base| base.join(&request[matched.len()..]))
        .map(|url| Some(url.to_string()))
        .map_err(|error| anyhow!("invalid import-map prefix target {target:?}: {error}"))
}

enum ExportResolution {
    Missing,
    Found(BindingCell),
    Ambiguous,
}

fn exported_cell(record: &ModuleRecord, name: &str) -> ExportResolution {
    resolve_exported_cell(record, name, &mut HashSet::new())
}

fn resolve_exported_cell(
    record: &ModuleRecord,
    name: &str,
    visited: &mut HashSet<String>,
) -> ExportResolution {
    if let Some(export) = record
        .module
        .exports
        .iter()
        .find(|export| export.exported == name)
    {
        return record
            .bindings
            .borrow()
            .get(&export.local)
            .cloned()
            .map(ExportResolution::Found)
            .unwrap_or(ExportResolution::Missing);
    }
    if name == "default" || !visited.insert(record.module.specifier.clone()) {
        return ExportResolution::Missing;
    }

    let mut resolved: Option<BindingCell> = None;
    for dependency in record
        .star_export_records
        .borrow()
        .iter()
        .filter_map(Weak::upgrade)
    {
        match resolve_exported_cell(&dependency, name, visited) {
            ExportResolution::Missing => {}
            ExportResolution::Ambiguous => return ExportResolution::Ambiguous,
            ExportResolution::Found(cell) => {
                if let Some(previous) = &resolved
                    && !Rc::ptr_eq(previous, &cell)
                {
                    return ExportResolution::Ambiguous;
                }
                resolved = Some(cell);
            }
        }
    }
    for dependency_url in record.star_export_external_urls.borrow().iter() {
        let Some(binding) = w3cos_core::module_registry::export(dependency_url, name) else {
            continue;
        };
        if resolved.is_some() {
            return ExportResolution::Ambiguous;
        }
        resolved = Some(external_binding_cell(binding.getter(), binding.setter()));
    }
    resolved
        .map(ExportResolution::Found)
        .unwrap_or(ExportResolution::Missing)
}

fn collect_export_names(
    record: &ModuleRecord,
    include_default: bool,
    visited: &mut HashSet<String>,
    names: &mut HashSet<String>,
) {
    if !visited.insert(record.module.specifier.clone()) {
        return;
    }
    names.extend(record.module.exports.iter().filter_map(|export| {
        (include_default || export.exported != "default").then(|| export.exported.clone())
    }));
    for dependency in record
        .star_export_records
        .borrow()
        .iter()
        .filter_map(Weak::upgrade)
    {
        collect_export_names(&dependency, false, visited, names);
    }
    for dependency_url in record.star_export_external_urls.borrow().iter() {
        names.extend(w3cos_core::module_registry::export_names(
            dependency_url,
            false,
        ));
    }
}

fn module_cycle_root(record: &Rc<ModuleRecord>) -> Rc<ModuleRecord> {
    let mut root = Rc::clone(record);
    loop {
        let next = root.cycle_root.borrow().as_ref().and_then(Weak::upgrade);
        let Some(next) = next else {
            break;
        };
        if Rc::ptr_eq(&next, &root) {
            break;
        }
        root = next;
    }
    if !Rc::ptr_eq(record, &root) {
        *record.cycle_root.borrow_mut() = Some(Rc::downgrade(&root));
    }
    root
}

fn module_namespace(record: &Rc<ModuleRecord>) -> Value {
    if let Some(namespace) = record.namespace.borrow().clone() {
        return namespace;
    }
    let namespace = Value::object(HashMap::new());
    let mut names = HashSet::new();
    collect_export_names(record, true, &mut HashSet::new(), &mut names);
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    for exported in names {
        let ExportResolution::Found(cell) = exported_cell(record, &exported) else {
            // Ambiguous star exports are not properties of the namespace.
            continue;
        };
        namespace.set_property(
            &format!("__w3cos_getter_{exported}"),
            Value::function(move |_, _| {
                if !cell.is_initialized() {
                    w3cos_core::throw_value(Value::string(&format!(
                        "ReferenceError: export {exported:?} is not initialized"
                    )));
                }
                cell.read_value()
            }),
        );
    }
    *record.namespace.borrow_mut() = Some(namespace.clone());
    namespace
}

pub(crate) fn notify_node_inserted(node: u32) {
    if parser_insertion_active() {
        return;
    }
    let mut scripts = HashSet::new();
    collect_script_nodes(node, &mut scripts);
    let mut stylesheets = HashSet::new();
    collect_stylesheet_nodes(node, &mut stylesheets);
    let mut images = HashSet::new();
    collect_image_nodes(node, &mut images);
    if scripts.is_empty() && stylesheets.is_empty() && images.is_empty() {
        return;
    }
    let has_active_loader = ACTIVE_DOCUMENT_LOADER.with(|active| active.borrow().is_some());
    if !has_active_loader {
        return;
    }
    DYNAMIC_SCRIPT_NODES.with(|nodes| {
        nodes.borrow_mut().extend(scripts);
    });
    let has_stylesheets = !stylesheets.is_empty();
    DYNAMIC_STYLESHEET_NODES.with(|nodes| {
        nodes.borrow_mut().extend(stylesheets.iter().copied());
    });
    if has_stylesheets
        && let Some((loader, _)) = ACTIVE_DOCUMENT_LOADER.with(|slot| slot.borrow().clone())
    {
        loader.rebuild_stylesheet_rules();
    }
    schedule_document_script_pump();
}

/// Re-run the standard script preparation path when a connected, not-yet
/// started script gains a source or inline text after insertion.
pub(crate) fn notify_script_mutated(node: u32) {
    let tag = crate::dom::tag_name(node);
    if parser_insertion_active() {
        return;
    }
    if tag.eq_ignore_ascii_case("img") {
        DYNAMIC_IMAGE_NODES.with(|nodes| {
            nodes.borrow_mut().insert(node);
        });
        schedule_document_script_pump();
        return;
    }
    if tag.eq_ignore_ascii_case("picture") || tag.eq_ignore_ascii_case("source") {
        let picture = if tag.eq_ignore_ascii_case("picture") {
            Some(node)
        } else {
            crate::dom::parent_node(node)
                .filter(|parent| crate::dom::tag_name(*parent).eq_ignore_ascii_case("picture"))
        };
        if let Some(picture) = picture {
            let mut images = HashSet::new();
            collect_image_nodes(picture, &mut images);
            DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow_mut().extend(images));
            schedule_document_script_pump();
        }
        return;
    }
    if !crate::dom::is_connected(node) {
        return;
    }
    if tag.eq_ignore_ascii_case("script") {
        DYNAMIC_SCRIPT_NODES.with(|nodes| {
            nodes.borrow_mut().insert(node);
        });
    } else if matches!(tag.as_str(), "style" | "link") {
        let nodes = HashSet::from([node]);
        if let Some((loader, _)) = ACTIVE_DOCUMENT_LOADER.with(|slot| slot.borrow().clone()) {
            loader.invalidate_stylesheet_nodes(&nodes);
        }
        DYNAMIC_STYLESHEET_NODES.with(|nodes| {
            nodes.borrow_mut().insert(node);
        });
    } else {
        return;
    }
    schedule_document_script_pump();
}

pub(crate) fn refresh_responsive_images() {
    let Some((_, document_url)) = ACTIVE_DOCUMENT_LOADER.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let Ok(base) = Url::parse(&document_url) else {
        return;
    };
    let mut changed = Vec::new();
    for node in crate::dom::get_elements_by_tag_name("img") {
        if !crate::dom::is_connected(node) || !image_uses_responsive_candidates(node) {
            continue;
        }
        let selected = select_image_source(node)
            .and_then(|selection| base.join(&selection.source).ok())
            .map(|url| url.to_string())
            .unwrap_or_default();
        let element = crate::jsdom::element_value(node);
        if element
            .get_property("__w3cos_image_request_src")
            .to_js_string()
            != selected
        {
            changed.push(node);
        }
    }
    if changed.is_empty() {
        return;
    }
    DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow_mut().extend(changed));
    schedule_document_script_pump();
}

fn image_uses_responsive_candidates(node: u32) -> bool {
    if crate::dom::get_attribute(node, "srcset").is_some_and(|value| !value.trim().is_empty()) {
        return true;
    }
    crate::dom::parent_node(node).is_some_and(|parent| {
        crate::dom::tag_name(parent).eq_ignore_ascii_case("picture")
            && crate::dom::children(parent).into_iter().any(|child| {
                crate::dom::tag_name(child).eq_ignore_ascii_case("source")
                    && crate::dom::get_attribute(child, "srcset")
                        .is_some_and(|value| !value.trim().is_empty())
            })
    })
}

fn schedule_document_script_pump() {
    let has_active_loader = ACTIVE_DOCUMENT_LOADER.with(|active| active.borrow().is_some());
    if !has_active_loader {
        return;
    }
    if DOCUMENT_PUMP_SCHEDULED.with(|scheduled| scheduled.replace(true)) {
        return;
    }

    crate::jsdom::queue_microtask_value(Value::function(|_, _| {
        DOCUMENT_PUMP_SCHEDULED.with(|scheduled| scheduled.set(false));
        let active = ACTIVE_DOCUMENT_LOADER.with(|slot| slot.borrow().clone());
        if let Some((loader, document_url)) = active {
            // `execute_pending_document_scripts` dispatches the element's
            // `onerror` callback. A queued host callback has no synchronous
            // caller to receive the Rust error.
            let _ = loader.execute_pending_document_scripts(&document_url);
        }
        Value::Undefined
    }));
}

pub(crate) fn notify_node_removed(node: u32) {
    if crate::dom::is_connected(node) {
        return;
    }
    let mut scripts = HashSet::new();
    collect_script_nodes(node, &mut scripts);
    let mut stylesheets = HashSet::new();
    collect_stylesheet_nodes(node, &mut stylesheets);
    let mut images = HashSet::new();
    collect_image_nodes(node, &mut images);
    if scripts.is_empty() && stylesheets.is_empty() && images.is_empty() {
        return;
    }
    DYNAMIC_SCRIPT_NODES.with(|nodes| {
        nodes.borrow_mut().retain(|node| !scripts.contains(node));
    });
    DYNAMIC_STYLESHEET_NODES.with(|nodes| {
        nodes
            .borrow_mut()
            .retain(|node| !stylesheets.contains(node));
    });
    DYNAMIC_IMAGE_NODES.with(|nodes| {
        nodes.borrow_mut().retain(|node| !images.contains(node));
    });
    let active = ACTIVE_DOCUMENT_LOADER.with(|slot| slot.borrow().clone());
    if let Some((loader, _)) = active {
        loader.cancel_removed_script_nodes(&scripts);
        loader.cancel_removed_stylesheet_nodes(&stylesheets);
        loader.invalidate_image_nodes(&images, true);
    }
}

fn collect_script_nodes(node: u32, scripts: &mut HashSet<u32>) {
    if crate::dom::tag_name(node).eq_ignore_ascii_case("script") {
        scripts.insert(node);
    }
    for child in crate::dom::children(node) {
        collect_script_nodes(child, scripts);
    }
}

fn collect_stylesheet_nodes(node: u32, stylesheets: &mut HashSet<u32>) {
    if matches!(crate::dom::tag_name(node).as_str(), "style" | "link") {
        stylesheets.insert(node);
    }
    for child in crate::dom::children(node) {
        collect_stylesheet_nodes(child, stylesheets);
    }
}

fn collect_image_nodes(node: u32, images: &mut HashSet<u32>) {
    if crate::dom::tag_name(node).eq_ignore_ascii_case("img") {
        images.insert(node);
    }
    for child in crate::dom::children(node) {
        collect_image_nodes(child, images);
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SelectedImageSource {
    source: String,
    density: f64,
}

#[derive(Debug, Clone)]
struct ImageCandidate {
    source: String,
    descriptor: ImageCandidateDescriptor,
}

#[derive(Debug, Clone, Copy)]
enum ImageCandidateDescriptor {
    Density(f64),
    Width(f64),
}

fn select_image_source(node: u32) -> Option<SelectedImageSource> {
    let (viewport_width, viewport_height, device_pixel_ratio) = crate::jsdom::viewport();
    let dpr = device_pixel_ratio.max(0.01);
    if let Some(parent) = crate::dom::parent_node(node)
        && crate::dom::tag_name(parent).eq_ignore_ascii_case("picture")
    {
        for child in crate::dom::children(parent) {
            if child == node {
                break;
            }
            if !crate::dom::tag_name(child).eq_ignore_ascii_case("source") {
                continue;
            }
            let media = crate::dom::get_attribute(child, "media").unwrap_or_default();
            if !media.trim().is_empty() && !crate::jsdom::media_query_matches(&media) {
                continue;
            }
            let content_type = crate::dom::get_attribute(child, "type").unwrap_or_default();
            if !content_type.trim().is_empty() && !is_decodable_picture_type(&content_type) {
                continue;
            }
            let Some(srcset) = crate::dom::get_attribute(child, "srcset") else {
                continue;
            };
            let sizes = crate::dom::get_attribute(child, "sizes")
                .or_else(|| crate::dom::get_attribute(node, "sizes"));
            if let Some(selected) = select_srcset_candidate(
                &srcset,
                sizes.as_deref(),
                viewport_width,
                viewport_height,
                dpr,
            ) {
                return Some(selected);
            }
        }
    }

    if let Some(srcset) =
        crate::dom::get_attribute(node, "srcset").filter(|value| !value.trim().is_empty())
    {
        let sizes = crate::dom::get_attribute(node, "sizes");
        if let Some(selected) = select_srcset_candidate(
            &srcset,
            sizes.as_deref(),
            viewport_width,
            viewport_height,
            dpr,
        ) {
            return Some(selected);
        }
    }

    crate::dom::get_attribute(node, "src")
        .filter(|source| !source.trim().is_empty())
        .map(|source| SelectedImageSource {
            source,
            density: 1.0,
        })
}

fn select_srcset_candidate(
    srcset: &str,
    sizes: Option<&str>,
    viewport_width: f64,
    viewport_height: f64,
    dpr: f64,
) -> Option<SelectedImageSource> {
    let candidates = parse_srcset(srcset);
    if candidates.is_empty() {
        return None;
    }
    let uses_width = candidates
        .iter()
        .any(|candidate| matches!(candidate.descriptor, ImageCandidateDescriptor::Width(_)));
    if uses_width
        && candidates
            .iter()
            .any(|candidate| matches!(candidate.descriptor, ImageCandidateDescriptor::Density(_)))
    {
        return None;
    }
    let source_size = if uses_width {
        parse_source_size(sizes.unwrap_or("100vw"), viewport_width, viewport_height).max(1.0)
    } else {
        1.0
    };
    let mut candidates = candidates
        .into_iter()
        .map(|candidate| {
            let density = match candidate.descriptor {
                ImageCandidateDescriptor::Density(value) => value,
                ImageCandidateDescriptor::Width(width) => width / source_size,
            };
            (candidate.source, density)
        })
        .filter(|(_, density)| density.is_finite() && *density > 0.0)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.1.total_cmp(&right.1));
    let (source, density) = candidates
        .iter()
        .find(|(_, density)| *density >= dpr)
        .or_else(|| candidates.last())?
        .clone();
    Some(SelectedImageSource { source, density })
}

fn parse_srcset(srcset: &str) -> Vec<ImageCandidate> {
    srcset
        .split(',')
        .filter_map(|candidate| {
            let mut parts = candidate.split_ascii_whitespace();
            let source = parts.next()?.trim();
            if source.is_empty() {
                return None;
            }
            let descriptor = match parts.next() {
                None => ImageCandidateDescriptor::Density(1.0),
                Some(value) if value.ends_with('x') => value[..value.len() - 1]
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .map(ImageCandidateDescriptor::Density)?,
                Some(value) if value.ends_with('w') => value[..value.len() - 1]
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .map(ImageCandidateDescriptor::Width)?,
                Some(_) => return None,
            };
            if parts.next().is_some() {
                return None;
            }
            Some(ImageCandidate {
                source: source.to_string(),
                descriptor,
            })
        })
        .collect()
}

fn parse_source_size(sizes: &str, viewport_width: f64, viewport_height: f64) -> f64 {
    for item in sizes
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let (media, length) = match item.rsplit_once(char::is_whitespace) {
            Some((media, length)) if !media.trim().is_empty() => (Some(media.trim()), length),
            _ => (None, item),
        };
        if media.is_some_and(|media| !crate::jsdom::media_query_matches(media)) {
            continue;
        }
        if let Some(length) = parse_source_size_length(length, viewport_width, viewport_height) {
            return length;
        }
    }
    viewport_width
}

fn parse_source_size_length(
    length: &str,
    viewport_width: f64,
    viewport_height: f64,
) -> Option<f64> {
    let length = length.trim().to_ascii_lowercase();
    for (unit, factor) in [
        ("vmax", viewport_width.max(viewport_height) / 100.0),
        ("vmin", viewport_width.min(viewport_height) / 100.0),
        ("rem", 16.0),
        ("vw", viewport_width / 100.0),
        ("px", 1.0),
        ("em", 16.0),
    ] {
        if let Some(number) = length.strip_suffix(unit) {
            return number
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|value| value * factor);
        }
    }
    None
}

fn is_decodable_picture_type(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/apng"
            | "image/bmp"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
            | "image/avif"
    )
}

/// Native lazy-loading policy. Images with established layout boxes are
/// requested when they enter a generous prefetch band; zero-sized boxes are
/// loaded eagerly so intrinsic sizing cannot deadlock layout.
fn should_defer_lazy_image(node: u32) -> bool {
    if !crate::dom::get_attribute(node, "loading")
        .is_some_and(|value| value.eq_ignore_ascii_case("lazy"))
    {
        return false;
    }
    let rect = crate::dom::bounding_rect(node);
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return false;
    }
    let viewport_height = crate::jsdom::window_value()
        .get_property("innerHeight")
        .to_number() as f32;
    let margin = (viewport_height * 2.0).max(1250.0);
    rect.bottom() < -margin || rect.top() > viewport_height + margin
}

fn collect_stylesheet_nodes_in_tree_order(node: u32, stylesheets: &mut Vec<u32>) {
    if matches!(crate::dom::tag_name(node).as_str(), "style" | "link") {
        stylesheets.push(node);
    }
    for child in crate::dom::children(node) {
        collect_stylesheet_nodes_in_tree_order(child, stylesheets);
    }
}

fn collect_dom_nodes_in_tree_order(node: u32, nodes: &mut Vec<u32>) {
    nodes.push(node);
    for child in crate::dom::children(node) {
        collect_dom_nodes_in_tree_order(child, nodes);
    }
}

pub(crate) fn reset_document_loader() {
    let active = ACTIVE_DOCUMENT_LOADER.with(|active| active.borrow_mut().take());
    if let Some((loader, _)) = active {
        loader.cancel_for_navigation();
    }
    DOCUMENT_PUMP_SCHEDULED.with(|scheduled| scheduled.set(false));
    DYNAMIC_SCRIPT_NODES.with(|nodes| nodes.borrow_mut().clear());
    DYNAMIC_STYLESHEET_NODES.with(|nodes| nodes.borrow_mut().clear());
    DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow_mut().clear());
    reset_parser_insertion_state();
}

pub(crate) fn refresh_stylesheet_media_queries() {
    if let Some((loader, _)) = ACTIVE_DOCUMENT_LOADER.with(|slot| slot.borrow().clone()) {
        loader.rebuild_stylesheet_rules();
        loader.activate_deferred_stylesheet_fonts();
    }
}

fn stylesheet_font_families(stack: &str) -> Vec<String> {
    stack
        .split(',')
        .map(|family| {
            family
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        })
        .filter(|family| !family.is_empty())
        .collect()
}

fn stylesheet_font_descriptor_score(
    face: &w3cos_compiler::esm_css::StylesheetFontFace,
    family: &str,
    weight: crate::font_face::FontWeight,
    style: crate::font_face::FontFaceStyle,
    character: char,
) -> Option<(u8, u16, u16)> {
    let mut encoded = [0; 4];
    let character = character.encode_utf8(&mut encoded);
    if !family.eq_ignore_ascii_case(&face.family)
        || !crate::font_face::unicode_range_matches_text(face.unicode_range.as_deref(), character)
    {
        return None;
    }
    let candidate_weight =
        crate::font_face::FontWeight::from_str(face.weight.as_deref().unwrap_or("normal"));
    let candidate_style =
        crate::font_face::FontFaceStyle::from_str(face.style.as_deref().unwrap_or("normal"));
    Some((
        u8::from(candidate_style != style),
        candidate_weight.0.abs_diff(weight.0),
        candidate_weight.0,
    ))
}

fn stylesheet_font_face_media_matches(node: u32, face: &StylesheetFontFaceLoad) -> bool {
    let stylesheet_matches = crate::dom::get_attribute(node, "media")
        .as_deref()
        .map(str::trim)
        .filter(|media| !media.is_empty())
        .is_none_or(crate::jsdom::media_query_matches);
    stylesheet_matches
        && face
            .face
            .media
            .as_deref()
            .map(str::trim)
            .filter(|media| !media.is_empty())
            .is_none_or(crate::jsdom::media_query_matches)
}

fn resolve_global(name: &str) -> Value {
    match name {
        "window" | "self" | "globalThis" => crate::jsdom::window_value(),
        "document" => crate::jsdom::document_value(),
        _ => {
            let value = crate::jsdom::window_value().get_property(name);
            if !value.is_undefined() {
                return value;
            }
            match name {
                "Object" => w3cos_core::object_value(),
                "Array" => w3cos_core::array_value(),
                "Math" => w3cos_core::math_value(),
                "JSON" => w3cos_core::json_value(),
                _ => Value::Undefined,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::rc::Rc;
    use std::thread;

    const SAME_SITE_CROSS_ORIGIN_PAGE: &str = "http://127.0.0.1:1";

    fn persistent_cache_files(directory: &Path) -> Vec<PathBuf> {
        let mut files = std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".w3ir.json"))
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    fn persistent_http_source_cache_files(directory: &Path) -> Vec<PathBuf> {
        let mut files = std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".response.json"))
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set fixture read timeout");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 2048];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).expect("read fixture request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8(request).expect("fixture request is HTTP text")
    }

    fn narrow_inter_woff2_bytes() -> Vec<u8> {
        use base64::Engine as _;

        base64::engine::general_purpose::STANDARD
            .decode(concat!(
                "d09GMgABAAAAAAFwAAoAAAAAAlQAAAEoAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "BmAAKApUbgE2AiQDCAsGAAQgBSAHIBu3AWAuCmNj5eRPhLMIVYH5vdURznTLEXy/",
                "3+ue+24QFaD7wiYpO0JUndiya1ztV0VywK51BDq1qQecZTDyNIAlcfNE0ufvxMXy",
                "fLABBxooDsArb7D6ADx4yJ4eTSNQ5L9xQQX5Q35DXMnBNGY8vlRXprqKosg5p4wA",
                "gGnz58821TLLKudOzlWuGosvCMhrKptj+kCgQL160AUILMMyBEggI9CJEiADorNJJ",
                "+tOLKlbdkodHyqX1i4+9WaVeebDs/rvs/9+9+ddrPcO++54wrXO8k6sGHqkgUcAg",
                "cStTweerPPwv5r+asDVj6XPwM+znZlqOF1XQo0EgmqTlBvu36wROloKLsd3cwVSd",
                "BpwUpUAAFkgtCpErpE0YqEttjlitw0OKoD/ZeMAAAA="
            ))
            .expect("embedded narrow WOFF2 fixture")
    }

    fn module_fixture(
        body: &'static str,
        allow_origin: Option<&'static str>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind module fixture");
        let address = listener.local_addr().expect("module fixture address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept module request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let cors = allow_origin
                .map(|origin| format!("Access-Control-Allow-Origin: {origin}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\n{cors}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write module fixture");
        });
        (format!("http://{address}/dependency.js"), handle)
    }

    #[test]
    fn source_executes_against_the_real_jsdom_document() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .execute_source(
                r#"document.body.setAttribute("data-runtime", "w3vm");"#,
                "inline:test",
            )
            .unwrap();
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-runtime")])
                .to_js_string(),
            "w3vm"
        );
    }

    #[test]
    fn script_protocol_selects_w3vm_or_precompiled_aot() {
        assert_eq!(
            script_execution_route("http://example.test/runtime.js"),
            ScriptExecutionRoute::RuntimeW3vm
        );
        assert_eq!(
            script_execution_route("https://example.test/runtime.js"),
            ScriptExecutionRoute::RuntimeW3vm
        );
        assert_eq!(
            script_execution_route("inline:parser-created"),
            ScriptExecutionRoute::RuntimeW3vm
        );
        assert_eq!(
            script_execution_route("data:text/javascript,globalThis.x=1"),
            ScriptExecutionRoute::RuntimeW3vm
        );
        assert_eq!(
            script_execution_route("file:///tmp/precompiled-entry.js"),
            ScriptExecutionRoute::PrecompiledAot
        );
    }

    #[test]
    fn srcset_selection_supports_density_and_width_descriptors() {
        assert_eq!(
            select_srcset_candidate(
                "small.png 1x, retina.png 2x, huge.png 3x",
                None,
                1024.0,
                768.0,
                1.5,
            ),
            Some(SelectedImageSource {
                source: "retina.png".to_string(),
                density: 2.0,
            })
        );
        assert_eq!(
            select_srcset_candidate(
                "small.png 320w, medium.png 640w, large.png 1280w",
                Some("(max-width: 600px) 100vw, 50vw"),
                1000.0,
                800.0,
                1.0,
            )
            .map(|selected| selected.source),
            Some("medium.png".to_string())
        );
    }

    #[test]
    fn picture_selects_the_first_matching_decodable_source() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(800.0, 600.0);
        crate::jsdom::set_device_pixel_ratio(2.0);

        let picture = crate::dom::create_element("picture");
        let unsupported = crate::dom::create_element("source");
        crate::dom::set_attribute(unsupported, "type", "image/svg+xml");
        crate::dom::set_attribute(unsupported, "srcset", "vector.svg 1x");
        let wide = crate::dom::create_element("source");
        crate::dom::set_attribute(wide, "type", "image/png");
        crate::dom::set_attribute(wide, "media", "(min-width: 700px)");
        crate::dom::set_attribute(wide, "srcset", "wide.png 1x, wide-2x.png 2x");
        let image = crate::dom::create_element("img");
        crate::dom::set_attribute(image, "src", "fallback.png");
        crate::dom::set_attribute(image, "srcset", "fallback.png 1x, fallback-2x.png 2x");
        crate::dom::append_child(picture, unsupported);
        crate::dom::append_child(picture, wide);
        crate::dom::append_child(picture, image);
        crate::dom::append_child(crate::dom::body_id(), picture);

        assert_eq!(
            select_image_source(image),
            Some(SelectedImageSource {
                source: "wide-2x.png".to_string(),
                density: 2.0,
            })
        );
    }

    #[test]
    fn viewport_change_queues_only_a_new_responsive_candidate() {
        reset_document_loader();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(500.0, 600.0);

        let picture = crate::dom::create_element("picture");
        let wide = crate::dom::create_element("source");
        crate::dom::set_attribute(wide, "media", "(min-width: 700px)");
        crate::dom::set_attribute(wide, "srcset", "wide.png");
        let image = crate::dom::create_element("img");
        crate::dom::set_attribute(image, "src", "small.png");
        crate::dom::append_child(picture, wide);
        crate::dom::append_child(picture, image);
        crate::dom::append_child(crate::dom::body_id(), picture);

        let loader = ScriptLoader::new(ScriptPolicy {
            allow_network: false,
            ..ScriptPolicy::default()
        });
        loader
            .attach_to_document("https://example.test/index.html")
            .expect("attach responsive image test document");
        let element = crate::jsdom::element_value(image);
        element.set_property(
            "__w3cos_image_request_src",
            Value::string("https://example.test/small.png"),
        );
        DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow_mut().clear());

        crate::jsdom::set_viewport(500.0, 600.0);
        assert!(
            !DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow().contains(&image)),
            "an unchanged selected URL must not restart loading"
        );
        crate::jsdom::set_viewport(800.0, 600.0);
        assert!(
            DYNAMIC_IMAGE_NODES.with(|nodes| nodes.borrow().contains(&image)),
            "a media-query change must queue the image for shared-loader reselection"
        );
        reset_document_loader();
    }

    #[test]
    fn file_script_executes_registered_aot_without_parsing_source() {
        let path = "/tmp/w3cos-protocol-aot-entry.js";
        let url = "file:///tmp/w3cos-protocol-aot-entry.js";
        let evaluations = Rc::new(Cell::new(0_u32));
        let evaluator_count = Rc::clone(&evaluations);
        w3cos_core::module_registry::register(
            path,
            HashMap::from([(
                "kind".to_string(),
                w3cos_core::module_registry::ExportBinding::new(
                    Value::function(|_, _| Value::string("aot")),
                    Value::Undefined,
                ),
            )]),
            Some(Value::function(move |_, _| {
                evaluator_count.set(evaluator_count.get() + 1);
                Value::Undefined
            })),
        );

        let loader = ScriptLoader::new(ScriptPolicy::default());
        let namespace = loader
            .execute_source("this is intentionally not valid JavaScript", url)
            .unwrap();
        assert_eq!(
            namespace.get_property("kind").to_js_string(),
            "aot",
            "the file URL must resolve the build-path AOT registration"
        );
        loader.load_and_execute_module(url).unwrap();
        assert_eq!(
            evaluations.get(),
            1,
            "Core must preserve one cached AOT evaluation state"
        );
        w3cos_core::module_registry::unregister(url);
    }

    #[test]
    fn unregistered_file_script_never_falls_back_to_w3vm() {
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let error = loader
            .execute_source("globalThis.mustNotRun = true;", "file:///tmp/not-built.js")
            .unwrap_err();
        assert!(error.to_string().contains("precompiled native AOT module"));
        assert!(error.to_string().contains("never fall back to W3VM"));
    }

    #[test]
    fn dynamic_typescript_erases_type_only_declarations_before_w3vm() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .execute_source(
                r#"
                    interface MapOptions { zoom: number }
                    type MapMode = "vector" | "raster";
                    declare function ambientMap(options: MapOptions): void;
                    declare class AmbientLayer {}
                    declare const ambientVersion: string;
                    declare enum AmbientKind { Vector }
                    declare namespace AmbientMaps {
                        const ready: boolean;
                    }
                    const options: MapOptions = { zoom: 12 };
                    const mode: MapMode = "vector";
                    document.body.setAttribute(
                        "data-typescript-runtime",
                        mode + ":" + options.zoom
                    );
                "#,
                "https://example.test/maps/runtime.ts",
            )
            .unwrap();
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method(
                    "getAttribute",
                    vec![Value::string("data-typescript-runtime")]
                )
                .to_js_string(),
            "vector:12"
        );
    }

    #[test]
    fn object_and_sparse_array_spread_execute_through_w3ir_w3vm() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .execute_source(
                r#"
                    const defaults = { mode: "map", zoom: 12 };
                    const options = { ...null, ...defaults, zoom: 14 };
                    const levels = [10, ...[12, 14], 16];
                    const formatter = {
                        prefix: "map",
                        format(...parts) {
                            return this.prefix + ":" + parts.join(",");
                        }
                    };
                    document.body.setAttribute(
                        "data-options",
                        formatter.format(options.zoom, ...levels)
                    );
                    const sparse = [10, , ...[, 16]];
                    const visited = [];
                    sparse.forEach((value, index) => visited.push(index));
                    document.body.setAttribute(
                        "data-sparse",
                        [
                            sparse.length,
                            1 in sparse,
                            2 in sparse,
                            visited.join(","),
                            Object.keys(sparse).join(",")
                        ].join("|")
                    );
                "#,
                "inline:object-spread",
            )
            .unwrap();
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-options")])
                .to_js_string(),
            "map:14,10,12,14,16"
        );
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-sparse")])
                .to_js_string(),
            "4|false|true|0,2,3|0,2,3"
        );
    }

    #[test]
    fn w3vm_fetch_uses_the_page_runtime_global() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fetch fixture");
        let address = listener.local_addr().expect("fetch fixture address");
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fetch request");
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /data.json HTTP/1.1"));
            let body = "shared-fetch";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write fetch response");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/page/index.html"))
            .unwrap();
        loader
            .execute_source(
                r#"document.body.setAttribute("data-fetch", fetch("/data.json").text());"#,
                "inline:fetch-global",
            )
            .unwrap();
        fixture.join().expect("fetch fixture completed");
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-fetch")])
                .to_js_string(),
            "shared-fetch"
        );
    }

    #[test]
    fn control_flow_executes_against_the_real_document() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    let index = 0;
                    while (index < 3) {
                        index = index + 1;
                    }
                    if (index === 3) {
                        document.body.setAttribute("data-cfg", "ready");
                    }
                "#,
                "inline:control-flow",
            )
            .unwrap();
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-cfg")])
                .to_js_string(),
            "ready"
        );
    }

    #[test]
    fn vm_callback_reenters_from_the_host_timer_queue() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    let marker = "scheduled";
                    setTimeout(() => {
                        document.body.setAttribute("data-timer", marker);
                    }, 0);
                    marker = "fired";
                "#,
                "inline:timer-callback",
            )
            .unwrap();

        assert_eq!(crate::jsdom::tick_timers(), 1);
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-timer")])
                .to_js_string(),
            "fired"
        );
    }

    #[test]
    fn vm_callback_reenters_from_the_shared_microtask_queue() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    let marker = "queued";
                    queueMicrotask(() => {
                        document.body.setAttribute("data-microtask", marker);
                    });
                    marker = "drained";
                "#,
                "inline:microtask-callback",
            )
            .unwrap();

        assert_eq!(crate::jsdom::drain_microtasks(), 0);
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-microtask")])
                .to_js_string(),
            "drained"
        );
    }

    #[test]
    fn shared_promise_reactions_reenter_vm_closures() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    Promise.resolve("resolved").then((value) => {
                        document.body.setAttribute("data-promise-resolve", value);
                    });
                    new Promise((resolve) => {
                        resolve("constructed");
                    }).then((value) => {
                        document.body.setAttribute("data-promise-new", value);
                    });
                "#,
                "inline:promise-reactions",
            )
            .unwrap();

        assert_eq!(crate::jsdom::drain_microtasks(), 0);
        let body = crate::jsdom::document_value().get_property("body");
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-promise-resolve")])
                .to_js_string(),
            "resolved"
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-promise-new")])
                .to_js_string(),
            "constructed"
        );
    }

    #[test]
    fn async_vm_function_resumes_across_multiple_awaits() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    const run = async () => {
                        let marker = "before:";
                        const first = await Promise.resolve("one:");
                        marker = "after:";
                        const second = await Promise.resolve("two");
                        return marker + first + second;
                    };
                    run().then((value) => {
                        document.body.setAttribute("data-await", value);
                    });
                "#,
                "inline:async-await",
            )
            .unwrap();

        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-await")])
                .to_js_string(),
            "after:one:two"
        );
    }

    #[test]
    fn rejected_await_rejects_the_async_function_promise() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    const run = async () => {
                        await Promise.reject("await-failed");
                        document.body.setAttribute("data-unreachable", "no");
                    };
                    run().catch((reason) => {
                        document.body.setAttribute("data-await-error", reason);
                    });
                "#,
                "inline:async-await-rejection",
            )
            .unwrap();

        let body = crate::jsdom::document_value().get_property("body");
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-await-error")])
                .to_js_string(),
            "await-failed"
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-unreachable")]),
            Value::Null
        );
    }

    #[test]
    fn dynamic_script_destructures_catch_bindings_through_w3ir() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    const key = "message";
                    try {
                        throw {
                            message: "map-error",
                            details: [void 0, 12, 13],
                            retryable: true
                        };
                    } catch ({
                        [key]: message,
                        details: [mode = "vector", zoom, ...levels],
                        ...metadata
                    }) {
                        document.body.setAttribute(
                            "data-catch-pattern",
                            message + ":" + mode + ":" + zoom + ":" +
                                levels[0] + ":" + metadata.retryable
                        );
                    }
                "#,
                "https://example.test/maps/catch-pattern.js",
            )
            .unwrap();

        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-catch-pattern")])
                .to_js_string(),
            "map-error:vector:12:13:true"
        );
    }

    #[test]
    fn async_try_catch_finally_survives_browser_script_suspension() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    async function run(shouldThrow) {
                        let trace = "";
                        try {
                            trace += await Promise.resolve("A");
                            if (shouldThrow) throw "boom";
                            return trace + "R";
                        } catch (error) {
                            trace += "C" + error;
                            return trace;
                        } finally {
                            trace += await Promise.resolve("F");
                            if (shouldThrow) {
                                document.body.setAttribute("data-finally-rejected", trace);
                            } else {
                                document.body.setAttribute("data-finally-resolved", trace);
                            }
                        }
                    }
                    Promise.all([run(false), run(true)]).then((values) => {
                        document.body.setAttribute(
                            "data-finally-completions",
                            values[0] + ":" + values[1]
                        );
                    });
                "#,
                "inline:async-try-finally",
            )
            .unwrap();

        let body = crate::jsdom::document_value().get_property("body");
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-finally-resolved")])
                .to_js_string(),
            "AF"
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-finally-rejected")])
                .to_js_string(),
            "ACboomF"
        );
        assert_eq!(
            body.call_method(
                "getAttribute",
                vec![Value::string("data-finally-completions")]
            )
            .to_js_string(),
            "AR:ACboom"
        );
    }

    #[test]
    fn generator_script_uses_resumable_vm_frames_and_iterator_close() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    function* coordinates() {
                        try {
                            const sent = yield "A";
                            yield sent + "B";
                        } finally {
                            document.body.setAttribute("data-generator-closed", "yes");
                        }
                    }
                    function* delegatedCoordinates() {
                        return yield* coordinates();
                    }

                    const direct = delegatedCoordinates();
                    const first = direct.next();
                    const second = direct.next("sent:");
                    const done = direct.next();
                    document.body.setAttribute(
                        "data-generator-direct",
                        first.value + ":" + second.value + ":" + done.done
                    );

                    let iterated = "";
                    for (const value of coordinates()) {
                        iterated += value;
                        break;
                    }
                    document.body.setAttribute("data-generator-loop", iterated);
                "#,
                "inline:generator",
            )
            .unwrap();

        let body = crate::jsdom::document_value().get_property("body");
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-generator-direct")])
                .to_js_string(),
            "A:sent:B:true"
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-generator-loop")])
                .to_js_string(),
            "A"
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-generator-closed")])
                .to_js_string(),
            "yes"
        );
    }

    #[test]
    fn async_generator_script_queues_requests_and_closes_for_await_iterators() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_source(
                r#"
                    let closes = 0;
                    async function* coordinates() {
                        try {
                            const sent = yield await Promise.resolve("A");
                            yield Promise.resolve(sent + "B");
                            return "done";
                        } finally {
                            closes += 1;
                        }
                    }
                    async function* delegatedCoordinates() {
                        return yield* coordinates();
                    }

                    const direct = delegatedCoordinates();
                    Promise.all([
                        direct.next(),
                        direct.next("sent:"),
                        direct.next()
                    ]).then((steps) => {
                        document.body.setAttribute(
                            "data-async-generator-direct",
                            steps[0].value + ":" +
                            steps[1].value + ":" +
                            steps[2].value + ":" +
                            steps[2].done
                        );
                    });

                    (async () => {
                        let iterated = "";
                        for await (const value of coordinates()) {
                            iterated += value;
                            break;
                        }
                        document.body.setAttribute(
                            "data-async-generator-loop",
                            iterated + ":" + closes
                        );
                    })();
                "#,
                "inline:async-generator",
            )
            .unwrap();

        let body = crate::jsdom::document_value().get_property("body");
        assert_eq!(
            body.call_method(
                "getAttribute",
                vec![Value::string("data-async-generator-direct")]
            )
            .to_js_string(),
            "A:sent:B:done:true"
        );
        assert_eq!(
            body.call_method(
                "getAttribute",
                vec![Value::string("data-async-generator-loop")]
            )
            .to_js_string(),
            "A:2"
        );
    }

    #[test]
    fn policy_blocks_oversized_and_network_sources() {
        let loader = ScriptLoader::new(ScriptPolicy {
            max_source_bytes: 3,
            allow_network: false,
            ..ScriptPolicy::default()
        });
        assert!(loader.execute_source("1234", "inline:test").is_err());
        assert!(
            loader
                .load_and_execute("https://example.test/app.js")
                .is_err()
        );
    }

    #[test]
    fn fetched_external_script_mutates_the_real_document() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let source = r#"document.body.setAttribute("data-fetched", "yes");"#;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .unwrap();
        });

        ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute(&format!("http://{address}/external.js"))
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-fetched")])
                .to_js_string(),
            "yes"
        );
    }

    #[test]
    fn document_loader_executes_new_inline_scripts_once() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.set_property(
            "textContent",
            Value::string(r#"document.body.setAttribute("data-inline", "yes");"#),
        );
        let load_count = Rc::new(Cell::new(0_u32));
        let callback_count = Rc::clone(&load_count);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                callback_count.set(callback_count.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);

        let loader = ScriptLoader::new(ScriptPolicy::default());
        assert_eq!(
            loader
                .execute_pending_document_scripts("https://example.test/index.html")
                .unwrap(),
            1
        );
        assert_eq!(
            loader
                .execute_pending_document_scripts("https://example.test/index.html")
                .unwrap(),
            0
        );
        assert_eq!(load_count.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-inline")])
                .to_js_string(),
            "yes"
        );
    }

    #[test]
    fn script_preparation_skips_classic_nomodule_and_normalizes_javascript_types() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let document = crate::jsdom::document_value();
        let head = document.get_property("head");

        let classic_nomodule = document.call_method("createElement", vec![Value::string("script")]);
        classic_nomodule.set_property("noModule", Value::Bool(true));
        classic_nomodule.set_property(
            "textContent",
            Value::string(r#"document.body.setAttribute("data-classic-nomodule", "unexpected");"#),
        );
        head.call_method("appendChild", vec![classic_nomodule.clone()]);

        let module_nomodule = document.call_method("createElement", vec![Value::string("script")]);
        module_nomodule.set_property("type", Value::string(" module "));
        module_nomodule.set_property("noModule", Value::Bool(true));
        module_nomodule.set_property(
            "textContent",
            Value::string(r#"document.body.setAttribute("data-module-nomodule", "executed");"#),
        );
        head.call_method("appendChild", vec![module_nomodule]);

        let legacy_typed = document.call_method("createElement", vec![Value::string("script")]);
        legacy_typed.set_property("type", Value::string(" Text/JavaScript "));
        legacy_typed.set_property(
            "textContent",
            Value::string(r#"document.body.setAttribute("data-typed-classic", "executed");"#),
        );
        head.call_method("appendChild", vec![legacy_typed]);

        let parameterized_data =
            document.call_method("createElement", vec![Value::string("script")]);
        parameterized_data.set_property("type", Value::string("text/javascript; charset=utf-8"));
        parameterized_data.set_property(
            "textContent",
            Value::string(
                r#"document.body.setAttribute("data-parameterized-type", "unexpected");"#,
            ),
        );
        head.call_method("appendChild", vec![parameterized_data]);

        let external_nomodule =
            document.call_method("createElement", vec![Value::string("script")]);
        external_nomodule.set_property("noModule", Value::Bool(true));
        external_nomodule.set_property("src", Value::string("/must-not-fetch.js"));
        head.call_method("appendChild", vec![external_nomodule]);

        let loader = ScriptLoader::new(ScriptPolicy {
            allow_network: false,
            ..ScriptPolicy::default()
        });
        assert_eq!(
            loader
                .execute_pending_document_scripts("https://example.test/index.html")
                .unwrap(),
            2
        );
        crate::jsdom::drain_microtasks();

        let body = document.get_property("body");
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-classic-nomodule")]),
            Value::Null
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-module-nomodule")])
                .to_js_string(),
            "executed"
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-typed-classic")])
                .to_js_string(),
            "executed"
        );
        assert_eq!(
            body.call_method(
                "getAttribute",
                vec![Value::string("data-parameterized-type")]
            ),
            Value::Null
        );

        // Once preparation skipped the classic script, removing `nomodule`
        // must not make the already-started element execute later.
        classic_nomodule.call_method("removeAttribute", vec![Value::string("nomodule")]);
        classic_nomodule.set_property(
            "textContent",
            Value::string(r#"document.body.setAttribute("data-classic-nomodule", "late");"#),
        );
        assert_eq!(
            loader
                .execute_pending_document_scripts("https://example.test/index.html")
                .unwrap(),
            0
        );
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-classic-nomodule")]),
            Value::Null
        );
    }

    #[test]
    fn attached_document_loader_automatically_executes_inserted_script() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        assert_eq!(
            loader
                .attach_to_document("https://example.test/index.html")
                .unwrap(),
            0
        );

        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.set_property(
            "textContent",
            Value::string(r#"document.body.setAttribute("data-automatic", "yes");"#),
        );
        let load_count = Rc::new(Cell::new(0_u32));
        let callback_count = Rc::clone(&load_count);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                callback_count.set(callback_count.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);

        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-automatic")]),
            Value::Null
        );
        assert!(crate::jsdom::drain_microtasks() >= 1);
        assert_eq!(load_count.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-automatic")])
                .to_js_string(),
            "yes"
        );
    }

    #[test]
    fn connected_empty_script_executes_after_late_src_property_assignment() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let source = r#"document.body.setAttribute("data-late-script-source", "executed");"#;
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.contains("GET /late.js "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .unwrap();
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        let load_count = Rc::new(Cell::new(0_u32));
        let callback_count = Rc::clone(&load_count);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                callback_count.set(callback_count.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script.clone()]);

        // An empty preparation attempt must leave the element eligible for a
        // later source mutation.
        crate::jsdom::drain_microtasks();
        assert_eq!(load_count.get(), 0);
        script.set_property("src", Value::string("/late.js"));
        assert_eq!(
            script
                .call_method("getAttribute", vec![Value::string("src")])
                .to_js_string(),
            "/late.js"
        );
        crate::jsdom::drain_microtasks();
        server.join().unwrap();
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }

        assert!(!has_pending_script_fetches());
        assert_eq!(load_count.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method(
                    "getAttribute",
                    vec![Value::string("data-late-script-source")]
                )
                .to_js_string(),
            "executed"
        );
    }

    #[test]
    fn connected_empty_script_executes_after_late_text_property_assignment() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();
        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        document
            .get_property("head")
            .call_method("appendChild", vec![script.clone()]);
        crate::jsdom::drain_microtasks();

        script.set_property(
            "text",
            Value::string(r#"document.body.setAttribute("data-late-script-text", "executed");"#),
        );
        assert_eq!(
            script.get_property("text").to_js_string(),
            r#"document.body.setAttribute("data-late-script-text", "executed");"#
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-late-script-text")])
                .to_js_string(),
            "executed"
        );
    }

    #[test]
    fn attaching_a_fragment_reschedules_scripts_discovered_while_detached() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let document = crate::jsdom::document_value();
        let fragment = document.call_method("createDocumentFragment", vec![]);
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.set_property(
            "textContent",
            Value::string(r#"document.body.setAttribute("data-fragment-script", "yes");"#),
        );
        fragment.call_method("appendChild", vec![script]);

        // The first insertion notification runs while the fragment is detached,
        // so the document scan cannot claim its script yet.
        assert!(crate::jsdom::drain_microtasks() >= 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-fragment-script")]),
            Value::Null
        );

        document
            .get_property("head")
            .call_method("appendChild", vec![fragment]);
        assert!(crate::jsdom::drain_microtasks() >= 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-fragment-script")])
                .to_js_string(),
            "yes"
        );
    }

    #[test]
    fn nested_script_insertion_does_not_reexecute_the_current_script() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let insertion_count = Rc::new(Cell::new(0_u32));
        let callback_count = Rc::clone(&insertion_count);
        crate::jsdom::window_value().set_property(
            "insertNext",
            Value::function(move |_, _| {
                callback_count.set(callback_count.get() + 1);
                let document = crate::jsdom::document_value();
                let script = document.call_method("createElement", vec![Value::string("script")]);
                script.set_property(
                    "textContent",
                    Value::string(r#"document.body.setAttribute("data-nested-script", "yes");"#),
                );
                document
                    .get_property("head")
                    .call_method("appendChild", vec![script]);
                Value::Undefined
            }),
        );

        let document = crate::jsdom::document_value();
        let first = document.call_method("createElement", vec![Value::string("script")]);
        first.set_property(
            "textContent",
            Value::string(
                r#"insertNext(); document.body.setAttribute("data-first-script", "yes");"#,
            ),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![first]);

        crate::jsdom::drain_microtasks();
        assert_eq!(insertion_count.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-nested-script")])
                .to_js_string(),
            "yes"
        );
    }

    #[test]
    fn inserted_module_script_uses_the_shared_module_graph_loader() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/dependency.js",
                r#"export const status = "module-ready";"#,
            )
            .unwrap();
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.set_property(
            "textContent",
            Value::string(
                r#"
                    import { status } from "./dependency.js";
                    document.body.setAttribute("data-module", status);
                "#,
            ),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);

        crate::jsdom::drain_microtasks();
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-module")])
                .to_js_string(),
            "module-ready"
        );
    }

    #[test]
    fn module_script_load_waits_for_pending_top_level_await() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
        let executor_slot = Rc::clone(&resolve_slot);
        let deferred = w3cos_core::promise::new(vec![Value::function(move |_, arguments| {
            *executor_slot.borrow_mut() = arguments.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        })]);
        crate::jsdom::window_value().set_property("deferredModule", deferred);

        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.set_property(
            "textContent",
            Value::string(
                r#"
                    await deferredModule;
                    document.body.setAttribute("data-module-await", "settled");
                "#,
            ),
        );
        let load_count = Rc::new(Cell::new(0_u32));
        let loaded = Rc::clone(&load_count);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                loaded.set(loaded.get() + 1);
                Value::Undefined
            }),
        );
        let error_count = Rc::new(Cell::new(0_u32));
        let failed = Rc::clone(&error_count);
        script.set_property(
            "onerror",
            Value::function(move |_, _| {
                failed.set(failed.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);

        crate::jsdom::drain_microtasks();
        assert_eq!(load_count.get(), 0);
        assert_eq!(error_count.get(), 0);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-module-await")]),
            Value::Null
        );

        resolve_slot
            .borrow()
            .call(Value::Undefined, vec![Value::string("ready")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(load_count.get(), 1);
        assert_eq!(error_count.get(), 0);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-module-await")])
                .to_js_string(),
            "settled"
        );
    }

    #[test]
    fn rejected_module_evaluation_dispatches_error_instead_of_load() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.set_property(
            "textContent",
            Value::string(r#"await Promise.reject("module-failed");"#),
        );
        let load_count = Rc::new(Cell::new(0_u32));
        let loaded = Rc::clone(&load_count);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                loaded.set(loaded.get() + 1);
                Value::Undefined
            }),
        );
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let observed_errors = Rc::clone(&errors);
        script.set_property(
            "onerror",
            Value::function(move |_, arguments| {
                observed_errors.borrow_mut().push(
                    arguments
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string(),
                );
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);

        crate::jsdom::drain_microtasks();
        assert_eq!(load_count.get(), 0);
        assert_eq!(errors.borrow().as_slice(), &["module-failed"]);
    }

    #[test]
    fn document_loader_resolves_relative_urls_and_reuses_source_cache() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let source = r#"document.body.setAttribute("data-chunk", "loaded");"#;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).contains("GET /assets/chunk.js "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .unwrap();
        });

        let document = crate::jsdom::document_value();
        let load_count = Rc::new(Cell::new(0));
        for _ in 0..2 {
            let script = document.call_method("createElement", vec![Value::string("script")]);
            script.call_method(
                "setAttribute",
                vec![Value::string("src"), Value::string("../assets/chunk.js")],
            );
            let observed_loads = Rc::clone(&load_count);
            script.set_property(
                "onload",
                Value::function(move |_, _| {
                    observed_loads.set(observed_loads.get() + 1);
                    Value::Undefined
                }),
            );
            document
                .get_property("head")
                .call_method("appendChild", vec![script]);
        }

        let loader = ScriptLoader::new(ScriptPolicy::default());
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/app/index.html"))
                .unwrap(),
            2
        );
        server.join().unwrap();
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_module_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert!(!has_pending_module_fetches());
        assert_eq!(load_count.get(), 2);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-chunk")])
                .to_js_string(),
            "loaded"
        );
    }

    #[test]
    fn classic_script_failure_dispatches_error_and_new_element_retries() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let source = r#"document.body.setAttribute("data-retried", "yes");"#;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request);
                if attempt == 0 {
                    write!(
                        stream,
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        source.len(),
                        source
                    )
                    .unwrap();
                }
            }
        });

        let document = crate::jsdom::document_value();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let errors = Rc::new(Cell::new(0));
        let first = document.call_method("createElement", vec![Value::string("script")]);
        first.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string("/chunk.js")],
        );
        let observed_errors = Rc::clone(&errors);
        first.set_property(
            "onerror",
            Value::function(move |_, _| {
                observed_errors.set(observed_errors.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![first]);
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/index.html"))
                .unwrap(),
            1
        );
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_module_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(errors.get(), 1);

        let loads = Rc::new(Cell::new(0));
        let retry = document.call_method("createElement", vec![Value::string("script")]);
        retry.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string("/chunk.js")],
        );
        let observed_loads = Rc::clone(&loads);
        retry.set_property(
            "onload",
            Value::function(move |_, _| {
                observed_loads.set(observed_loads.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![retry]);
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/index.html"))
                .unwrap(),
            1
        );
        server.join().unwrap();
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_module_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert!(!has_pending_module_fetches());
        assert_eq!(loads.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-retried")])
                .to_js_string(),
            "yes"
        );
    }

    #[test]
    fn classic_script_nosniff_rejects_wrong_mime_without_caching_the_failure() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let blocked_source = r#"document.body.setAttribute("data-nosniff-blocked", "unexpected");"#;
        let accepted_source = r#"document.body.setAttribute("data-nosniff-retry", "executed");"#;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind nosniff fixture");
        let address = listener.local_addr().expect("nosniff fixture address");
        let server = thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept nosniff request");
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request);
                let (content_type, source) = if attempt == 0 {
                    ("text/plain", blocked_source)
                } else {
                    (" Text/JavaScript ; charset=UTF-8", accepted_source)
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type:{content_type}\r\nX-Content-Type-Options: NoSnIfF\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    source.len(),
                    source
                )
                .expect("write nosniff response");
            }
        });

        let document = crate::jsdom::document_value();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let errors = Rc::new(Cell::new(0_u32));
        let first = document.call_method("createElement", vec![Value::string("script")]);
        first.set_property("src", Value::string("/nosniff.js"));
        let observed_errors = Rc::clone(&errors);
        first.set_property(
            "onerror",
            Value::function(move |_, _| {
                observed_errors.set(observed_errors.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![first]);
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/index.html"))
                .unwrap(),
            1
        );
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert!(!has_pending_script_fetches());
        assert_eq!(errors.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-nosniff-blocked")]),
            Value::Null
        );

        let loads = Rc::new(Cell::new(0_u32));
        let retry = document.call_method("createElement", vec![Value::string("script")]);
        retry.set_property("src", Value::string("/nosniff.js"));
        let observed_loads = Rc::clone(&loads);
        retry.set_property(
            "onload",
            Value::function(move |_, _| {
                observed_loads.set(observed_loads.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![retry]);
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/index.html"))
                .unwrap(),
            1
        );
        server.join().expect("nosniff fixture completed");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert!(!has_pending_script_fetches());
        assert_eq!(loads.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-nosniff-retry")])
                .to_js_string(),
            "executed"
        );
    }

    #[test]
    fn configured_classic_retry_chain_is_deduplicated_and_dispatches_load_once_per_element() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let source = r#"document.body.setAttribute("data-auto-retry", "loaded");"#;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind classic retry fixture");
        let address = listener
            .local_addr()
            .expect("classic retry fixture address");
        let server = thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept classic retry request");
                let request = read_http_request(&mut stream).to_ascii_lowercase();
                if attempt == 0 {
                    stream
                        .write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nSet-Cookie: retry-a=one; Path=/\r\nSet-Cookie: retry-b=two; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write classic retry failure");
                } else {
                    assert!(request.contains("retry-a=one"));
                    assert!(request.contains("retry-b=two"));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        source.len(),
                        source
                    )
                    .expect("write classic retry success");
                }
            }
        });
        let loader = ScriptLoader::new(ScriptPolicy {
            retry: ScriptRetryPolicy {
                max_attempts: 2,
                base_delay_ms: 0,
                max_delay_ms: 0,
                respect_retry_after: true,
            },
            ..ScriptPolicy::default()
        });
        let document = crate::jsdom::document_value();
        let loads = Rc::new(Cell::new(0_u32));
        let errors = Rc::new(Cell::new(0_u32));
        for _ in 0..2 {
            let script = document.call_method("createElement", vec![Value::string("script")]);
            script.call_method(
                "setAttribute",
                vec![Value::string("src"), Value::string("/shared-chunk.js")],
            );
            let observed_loads = Rc::clone(&loads);
            script.set_property(
                "onload",
                Value::function(move |_, _| {
                    observed_loads.set(observed_loads.get() + 1);
                    Value::Undefined
                }),
            );
            let observed_errors = Rc::clone(&errors);
            script.set_property(
                "onerror",
                Value::function(move |_, _| {
                    observed_errors.set(observed_errors.get() + 1);
                    Value::Undefined
                }),
            );
            document
                .get_property("head")
                .call_method("appendChild", vec![script]);
        }
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/index.html"))
                .unwrap(),
            2
        );
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        server.join().expect("classic retry fixture completed");

        assert_eq!(loads.get(), 2);
        assert_eq!(errors.get(), 0);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-auto-retry")])
                .to_js_string(),
            "loaded"
        );
        assert_eq!(
            loader.script_retry_stats(),
            ScriptRetryStats {
                scheduled: 1,
                started: 1,
                succeeded: 1,
                ..ScriptRetryStats::default()
            }
        );
    }

    #[test]
    fn configured_module_retry_recovers_without_a_second_execution_path() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind module retry fixture");
        let address = listener.local_addr().expect("module retry fixture address");
        let server = thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept module retry request");
                let _ = read_http_request(&mut stream);
                if attempt == 0 {
                    stream
                        .write_all(
                            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write module retry failure");
                } else {
                    let body = r#"export const answer = 42;"#;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("write module retry success");
                }
            }
        });
        let loader = ScriptLoader::new(ScriptPolicy {
            retry: ScriptRetryPolicy {
                max_attempts: 2,
                base_delay_ms: 0,
                max_delay_ms: 0,
                respect_retry_after: true,
            },
            ..ScriptPolicy::default()
        });

        let namespace = loader
            .load_and_execute_module(&format!("http://{address}/retry.js"))
            .unwrap();
        server.join().expect("module retry fixture completed");
        assert_eq!(namespace.get_property("answer"), Value::Number(42.0));
        assert_eq!(
            loader.script_retry_stats(),
            ScriptRetryStats {
                scheduled: 1,
                started: 1,
                succeeded: 1,
                ..ScriptRetryStats::default()
            }
        );
    }

    #[test]
    fn configured_module_retry_reports_exhaustion_after_the_attempt_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind retry exhaustion fixture");
        let address = listener
            .local_addr()
            .expect("retry exhaustion fixture address");
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept retry exhaustion request");
                let _ = read_http_request(&mut stream);
                stream
                    .write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .expect("write retry exhaustion response");
            }
        });
        let loader = ScriptLoader::new(ScriptPolicy {
            retry: ScriptRetryPolicy {
                max_attempts: 2,
                base_delay_ms: 0,
                max_delay_ms: 0,
                respect_retry_after: true,
            },
            ..ScriptPolicy::default()
        });

        let error = loader
            .load_and_execute_module(&format!("http://{address}/exhausted.js"))
            .unwrap_err();
        server.join().expect("retry exhaustion fixture completed");
        let message = error.to_string();
        assert!(message.contains("503"), "{message}");
        assert_eq!(
            loader.script_retry_stats(),
            ScriptRetryStats {
                scheduled: 1,
                started: 1,
                exhausted: 1,
                ..ScriptRetryStats::default()
            }
        );
    }

    #[test]
    fn navigation_cancels_a_scheduled_script_retry_before_it_starts() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind retry cancellation fixture");
        let address = listener
            .local_addr()
            .expect("retry cancellation fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("accept retry cancellation request");
            let _ = read_http_request(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 10\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write retry cancellation response");
        });
        let loader = ScriptLoader::new(ScriptPolicy {
            retry: ScriptRetryPolicy {
                max_attempts: 3,
                base_delay_ms: 100,
                max_delay_ms: 100,
                respect_retry_after: true,
            },
            ..ScriptPolicy::default()
        });
        let evaluation =
            loader.load_and_execute_module_async(&format!("http://{address}/cancel.js"));
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if loader.script_retry_stats().scheduled == 1 {
                break;
            }
            thread::yield_now();
        }
        server.join().expect("retry cancellation fixture completed");
        assert_eq!(loader.script_retry_stats().scheduled, 1);
        assert!(next_script_fetch_deadline().is_some());

        loader.cancel_for_navigation();
        thread::sleep(std::time::Duration::from_millis(120));
        crate::jsdom::drain_microtasks();

        assert!(!has_pending_script_fetches());
        assert_eq!(
            loader.script_retry_stats(),
            ScriptRetryStats {
                scheduled: 1,
                cancelled: 1,
                ..ScriptRetryStats::default()
            }
        );
        assert!(matches!(
            w3cos_core::promise::status(&evaluation),
            Some(w3cos_core::promise::PromiseStatus::Rejected(_))
        ));
    }

    #[test]
    fn retry_policy_classifies_statuses_and_caps_retry_after_dates() {
        let response = |status, retry_after: Option<&str>| crate::fetch::FetchTextResponse {
            status,
            ok: (200..300).contains(&status),
            status_text: String::new(),
            headers: retry_after
                .map(|value| HashMap::from([("Retry-After".to_string(), value.to_string())]))
                .unwrap_or_default(),
            url: "https://example.test/retry.js".to_string(),
            redirected: false,
            set_cookies: Vec::new(),
            body: String::new(),
        };
        assert!(is_retryable_script_fetch(&Ok(response(408, None))));
        assert!(is_retryable_script_fetch(&Ok(response(503, None))));
        assert!(!is_retryable_script_fetch(&Ok(response(404, None))));
        assert!(is_retryable_script_fetch(&Err("transport".to_string())));
        assert_eq!(
            parse_retry_after("2"),
            Some(std::time::Duration::from_secs(2))
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(std::time::Duration::ZERO)
        );
        assert!(parse_retry_after("invalid").is_none());

        let policy = ScriptRetryPolicy {
            max_attempts: 2,
            base_delay_ms: 10,
            max_delay_ms: 250,
            respect_retry_after: true,
        };
        assert_eq!(
            script_retry_delay(
                policy,
                1,
                &Ok(response(503, Some("Sun, 06 Nov 2094 08:49:37 GMT")))
            ),
            Some(std::time::Duration::from_millis(250))
        );
        assert_eq!(
            script_retry_delay(policy, 2, &Ok(response(503, None))),
            None
        );
    }

    #[test]
    fn parser_classic_scripts_fetch_concurrently_but_execute_in_document_order() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                requests.push((stream, request.contains("GET /slow.js ")));
            }
            requests.sort_by_key(|(_, is_slow)| *is_slow);
            for (mut stream, is_slow) in requests {
                if is_slow {
                    thread::sleep(std::time::Duration::from_millis(25));
                }
                let source = if is_slow {
                    r#"record("slow");"#
                } else {
                    r#"record("fast");"#
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    source.len(),
                    source
                )
                .unwrap();
            }
        });

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "record",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        let document = crate::jsdom::document_value();
        for src in ["/slow.js", "/fast.js"] {
            let script = document.call_method("createElement", vec![Value::string("script")]);
            script.call_method(
                "setAttribute",
                vec![Value::string("src"), Value::string(src)],
            );
            document
                .get_property("head")
                .call_method("appendChild", vec![script]);
        }
        let loader = ScriptLoader::new(ScriptPolicy::default());
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/index.html"))
                .unwrap(),
            2
        );
        server.join().unwrap();
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_module_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert!(!has_pending_module_fetches());
        assert_eq!(observed.borrow().as_slice(), &["slow", "fast"]);
    }

    #[test]
    fn streaming_document_parser_builds_across_token_boundaries_and_runs_inline_script() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/article.html").unwrap();
        assert_eq!(
            parser.write("<!doctype html><html lang=en><he").unwrap(),
            DocumentParseProgress::Advanced
        );
        assert_eq!(
            parser.write("ad><title>A &am",).unwrap(),
            DocumentParseProgress::Advanced
        );
        assert_eq!(
            parser
                .write("p; B</title><script>document.body.setAttribute(\"data-stream-inline\", ")
                .unwrap(),
            DocumentParseProgress::Advanced
        );
        assert_eq!(
            parser
                .write(
                    "\"ran\");</script></head><body><p id=first>Hello &amp; <em>world</em><p id=second>Next"
                )
                .unwrap(),
            DocumentParseProgress::Advanced
        );
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
        crate::jsdom::drain_microtasks();

        let document = crate::jsdom::document_value();
        assert_eq!(
            document
                .get_property("documentElement")
                .call_method("getAttribute", vec![Value::string("lang")])
                .to_js_string(),
            "en"
        );
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("title")])
                .get_property("textContent")
                .to_js_string(),
            "A & B"
        );
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-stream-inline")])
                .to_js_string(),
            "ran"
        );
        assert_eq!(
            document
                .call_method("querySelectorAll", vec![Value::string("p")])
                .get_property("length")
                .to_u32(),
            2
        );
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("#first")])
                .get_property("textContent")
                .to_js_string(),
            "Hello & world"
        );
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
    }

    #[test]
    fn streaming_document_parser_applies_basic_table_insertion_modes() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/table.html").unwrap();
        parser
            .write(
                "<body><table id=grid>outside<div id=foster>moved</div>\
                 <tr><td id=first>A<td id=second>B</table>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        let table = document.call_method("querySelector", vec![Value::string("#grid")]);
        let tbody = document.call_method("querySelector", vec![Value::string("tbody")]);
        let first = document.call_method("querySelector", vec![Value::string("#first")]);
        let second = document.call_method("querySelector", vec![Value::string("#second")]);
        let foster = document.call_method("querySelector", vec![Value::string("#foster")]);
        let table_id = crate::jsdom::node_id_of(&table).unwrap();
        let tbody_id = crate::jsdom::node_id_of(&tbody).unwrap();
        let first_id = crate::jsdom::node_id_of(&first).unwrap();
        let second_id = crate::jsdom::node_id_of(&second).unwrap();
        let foster_id = crate::jsdom::node_id_of(&foster).unwrap();
        assert_eq!(crate::dom::parent_node(tbody_id), Some(table_id));
        assert_eq!(
            crate::dom::tag_name(crate::dom::parent_node(first_id).unwrap()),
            "tr"
        );
        assert_eq!(
            crate::dom::parent_node(first_id),
            crate::dom::parent_node(second_id)
        );
        assert_eq!(
            crate::dom::parent_node(foster_id),
            Some(crate::dom::body_id())
        );
        assert_eq!(foster.get_property("textContent").to_js_string(), "moved");
        assert!(
            table
                .get_property("textContent")
                .to_js_string()
                .contains("AB")
        );
        assert!(
            !table
                .get_property("textContent")
                .to_js_string()
                .contains("outside")
        );
    }

    #[test]
    fn streaming_document_parser_tracks_foreign_content_and_integration_points() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/foreign.html").unwrap();
        parser
            .write(
                "<body>\
                 <svg id=svg><g id=group>\
                   <foreignObject><div id=svg-html>HTML</div></foreignObject>\
                   <title id=svg-title>A<tspan id=title-child>B</tspan></title>\
                 </g></svg>\
                 <math id=math><mtext><span id=math-html>M</span></mtext>\
                   <mi><mglyph id=glyph></mglyph></mi></math>\
                 <svg id=break-root><g><div id=breakout>outside</div></g></svg>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        let select =
            |selector: &str| document.call_method("querySelector", vec![Value::string(selector)]);
        for selector in ["#svg", "#group", "#svg-title"] {
            assert_eq!(
                select(selector).get_property("namespaceURI").to_js_string(),
                SVG_NAMESPACE
            );
        }
        assert_eq!(
            select("#math").get_property("namespaceURI").to_js_string(),
            MATHML_NAMESPACE
        );
        assert_eq!(
            select("#glyph").get_property("namespaceURI").to_js_string(),
            MATHML_NAMESPACE
        );
        for selector in ["#svg-html", "#title-child", "#math-html", "#breakout"] {
            assert_eq!(
                select(selector).get_property("namespaceURI").to_js_string(),
                HTML_NAMESPACE
            );
        }
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&select("#breakout")).unwrap()),
            Some(crate::dom::body_id())
        );
        assert!(!select("#title-child").is_null());
    }

    #[test]
    fn streaming_document_parser_adjusts_foreign_names_and_accepts_foreign_cdata() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/foreign-names.html")
                .unwrap();
        parser
            .write(
                "<body>\
                 <svg><lineargradient id=gradient viewbox='0 0 10 10'>\
                   <textpath id=path textlength=5><![CDATA[A&B]]></textpath>\
                 </lineargradient></svg>\
                 <math><annotation-xml id=annotation definitionurl=urn:test /></math>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        let select =
            |selector: &str| document.call_method("querySelector", vec![Value::string(selector)]);
        let gradient = select("#gradient");
        assert_eq!(
            gradient.get_property("localName").to_js_string(),
            "linearGradient"
        );
        assert_eq!(
            gradient
                .call_method("getAttribute", vec![Value::string("viewBox")])
                .to_js_string(),
            "0 0 10 10"
        );
        let path = select("#path");
        assert_eq!(path.get_property("localName").to_js_string(), "textPath");
        assert_eq!(path.get_property("textContent").to_js_string(), "A&B");
        assert_eq!(
            path.call_method("getAttribute", vec![Value::string("textLength")])
                .to_js_string(),
            "5"
        );
        let annotation = select("#annotation");
        assert_eq!(
            annotation
                .call_method("getAttribute", vec![Value::string("definitionURL")])
                .to_js_string(),
            "urn:test"
        );
    }

    #[test]
    fn streaming_document_parser_builds_inert_template_content_fragments() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/template.html").unwrap();
        parser
            .write(
                "<body><template id=card><section id=inside>Template</section>\
                 <script>document.body.setAttribute('data-template-script', 'ran')</script>\
                 </template><p id=outside>Page</p>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
        crate::jsdom::drain_microtasks();

        let document = crate::jsdom::document_value();
        let template = document.call_method("querySelector", vec![Value::string("#card")]);
        let content = template.get_property("content");
        assert_eq!(content.get_property("nodeType").to_u32(), 11);
        assert_eq!(
            template
                .get_property("childNodes")
                .get_property("length")
                .to_u32(),
            0
        );
        assert!(
            document
                .call_method("querySelector", vec![Value::string("#inside")])
                .is_null()
        );
        assert_eq!(
            content
                .call_method("querySelector", vec![Value::string("#inside")])
                .get_property("textContent")
                .to_js_string(),
            "Template"
        );
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-template-script")]),
            Value::Null
        );
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("#outside")])
                .get_property("textContent")
                .to_js_string(),
            "Page"
        );
    }

    #[test]
    fn streaming_document_parser_scopes_table_modes_to_template_content() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/template-table.html")
                .unwrap();
        parser
            .write(
                "<body><table id=outer><template id=rows>\
                   <tr id=template-row>foster<td>T</td></tr>\
                   </table><p id=template-after-close>P</p>\
                 </template><tr id=live-row><td>L</td></tr></table>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        let template = document.call_method("querySelector", vec![Value::string("#rows")]);
        let content = template.get_property("content");
        assert!(
            !content
                .call_method("querySelector", vec![Value::string("#template-row")])
                .is_null()
        );
        assert!(
            !content
                .call_method(
                    "querySelector",
                    vec![Value::string("#template-after-close")]
                )
                .is_null()
        );
        assert!(
            content
                .get_property("textContent")
                .to_js_string()
                .contains("foster")
        );
        assert!(
            document
                .call_method(
                    "querySelector",
                    vec![Value::string("#template-after-close")]
                )
                .is_null()
        );
        let outer = document.call_method("querySelector", vec![Value::string("#outer")]);
        let live_row = document.call_method("querySelector", vec![Value::string("#live-row")]);
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&live_row).unwrap())
                .and_then(|parent| crate::dom::parent_node(parent)),
            crate::jsdom::node_id_of(&outer)
        );
    }

    #[test]
    fn streaming_document_parser_repairs_misnested_active_formatting() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/formatting.html").unwrap();
        parser
            .write(
                "<body><p><b><i>one</b>two</i></p>\
                 <p id=split><strong>left</p>right</strong>",
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        let italics = document.call_method("querySelectorAll", vec![Value::string("i")]);
        assert_eq!(italics.get_property("length").to_u32(), 2);
        assert_eq!(
            italics
                .call_method("item", vec![Value::Number(0.0)])
                .get_property("textContent")
                .to_js_string(),
            "one"
        );
        assert_eq!(
            italics
                .call_method("item", vec![Value::Number(1.0)])
                .get_property("textContent")
                .to_js_string(),
            "two"
        );
        let strong = document.call_method("querySelectorAll", vec![Value::string("strong")]);
        assert_eq!(strong.get_property("length").to_u32(), 2);
        assert_eq!(
            strong
                .call_method("item", vec![Value::Number(0.0)])
                .get_property("textContent")
                .to_js_string(),
            "left"
        );
        assert_eq!(
            strong
                .call_method("item", vec![Value::Number(1.0)])
                .get_property("textContent")
                .to_js_string(),
            "right"
        );
        assert_eq!(
            crate::dom::parent_node(
                crate::jsdom::node_id_of(&strong.call_method("item", vec![Value::Number(1.0)]))
                    .unwrap()
            ),
            Some(crate::dom::body_id())
        );
    }

    #[test]
    fn streaming_document_parser_runs_furthest_block_adoption_agency_path() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "https://example.test/adoption-block.html")
                .unwrap();
        parser
            .write("<body><b>one<div id=block>two</b>three</div>tail")
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        let bold = document.call_method("querySelectorAll", vec![Value::string("b")]);
        assert_eq!(bold.get_property("length").to_u32(), 2);
        let first = bold.call_method("item", vec![Value::Number(0.0)]);
        let second = bold.call_method("item", vec![Value::Number(1.0)]);
        let block = document.call_method("querySelector", vec![Value::string("#block")]);
        assert_eq!(first.get_property("textContent").to_js_string(), "one");
        assert_eq!(second.get_property("textContent").to_js_string(), "two");
        assert_eq!(block.get_property("textContent").to_js_string(), "twothree");
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&first).unwrap()),
            Some(crate::dom::body_id())
        );
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&block).unwrap()),
            Some(crate::dom::body_id())
        );
        assert_eq!(
            crate::dom::parent_node(crate::jsdom::node_id_of(&second).unwrap()),
            crate::jsdom::node_id_of(&block)
        );
        assert_eq!(
            document
                .get_property("body")
                .get_property("textContent")
                .to_js_string(),
            "onetwothreetail"
        );
    }

    #[test]
    fn streaming_document_parser_sets_doctype_and_compatibility_mode() {
        fn parse(source: &str) -> (Value, DocumentCompatibilityMode, usize) {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            let loader = ScriptLoader::new(ScriptPolicy::default());
            let mut parser =
                StreamingDocumentParser::new(loader, "https://example.test/mode.html").unwrap();
            parser.write(source).unwrap();
            assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);
            (
                crate::jsdom::document_value(),
                parser.compatibility_mode(),
                parser.parse_error_count(),
            )
        }

        let (standards, standards_mode, standards_errors) =
            parse("<!DOCTYPE html><title>standards</title>");
        assert_eq!(standards_mode, DocumentCompatibilityMode::NoQuirks);
        assert_eq!(standards_errors, 0);
        assert_eq!(
            standards.get_property("compatMode").to_js_string(),
            "CSS1Compat"
        );
        assert_eq!(
            standards
                .get_property("doctype")
                .get_property("name")
                .to_js_string(),
            "html"
        );

        let (quirks, quirks_mode, quirks_errors) =
            parse("<html><body>missing doctype</body></html>");
        assert_eq!(quirks_mode, DocumentCompatibilityMode::Quirks);
        assert!(quirks_errors >= 1);
        assert_eq!(
            quirks.get_property("compatMode").to_js_string(),
            "BackCompat"
        );

        let (legacy, legacy_mode, _) = parse(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\">\
             <html><body>legacy</body></html>",
        );
        assert_eq!(legacy_mode, DocumentCompatibilityMode::Quirks);
        assert_eq!(
            legacy.get_property("compatMode").to_js_string(),
            "BackCompat"
        );
        assert_eq!(
            legacy
                .get_property("doctype")
                .get_property("publicId")
                .to_js_string(),
            "-//W3C//DTD HTML 4.01 Transitional//EN"
        );

        let (_, limited_xhtml, _) = parse(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \
             \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\"><p>x",
        );
        assert_eq!(limited_xhtml, DocumentCompatibilityMode::LimitedQuirks);

        let (limited_html, limited_html_mode, _) = parse(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \
             \"http://www.w3.org/TR/html4/loose.dtd\"><p>x",
        );
        assert_eq!(
            limited_html.get_property("compatMode").to_js_string(),
            "CSS1Compat"
        );
        assert_eq!(limited_html_mode, DocumentCompatibilityMode::LimitedQuirks);

        let (_, ibm_mode, _) = parse(
            "<!DOCTYPE html SYSTEM \
             \"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd\"><p>x",
        );
        assert_eq!(ibm_mode, DocumentCompatibilityMode::Quirks);

        let (_, malformed_mode, malformed_errors) =
            parse("<!DOCTYPE html PUBLIC nope><p>x</missing>");
        assert_eq!(malformed_mode, DocumentCompatibilityMode::Quirks);
        assert!(malformed_errors >= 2);

        let (_, duplicate_mode, duplicate_errors) =
            parse("<!DOCTYPE html><!DOCTYPE html><div/></not-open>");
        assert_eq!(duplicate_mode, DocumentCompatibilityMode::NoQuirks);
        assert!(duplicate_errors >= 3);
    }

    #[test]
    fn streaming_document_parser_pauses_and_resumes_after_external_classic_script() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind parser-blocking fixture");
        let address = listener
            .local_addr()
            .expect("parser-blocking fixture address");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept parser-blocking request");
            let _ = read_http_request(&mut stream);
            accepted_tx
                .send(())
                .expect("signal parser-blocking request");
            release_rx.recv().expect("release parser-blocking response");
            let source = r#"document.body.setAttribute("data-parser-blocking", "external-ran");"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .expect("write parser-blocking response");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, &format!("http://{address}/document.html"))
                .unwrap();
        let html = format!(
            "<html><head><script src=\"http://{address}/blocking.js\"></script>\
             <script>document.body.setAttribute(\"data-after-block\", \"inline-ran\");</script>\
             </head><body><p id=after>After</p></body></html>"
        );
        assert_eq!(
            parser.write(&html).unwrap(),
            DocumentParseProgress::BlockedOnScript
        );
        assert!(parser.is_blocked());
        assert_eq!(
            parser.finish().unwrap(),
            DocumentParseProgress::BlockedOnScript
        );
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("parser-blocking request accepted");
        let document = crate::jsdom::document_value();
        assert!(
            document
                .call_method("querySelector", vec![Value::string("#after")])
                .is_null()
        );
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-after-block")]),
            Value::Null
        );

        release_tx
            .send(())
            .expect("release parser-blocking fixture");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !parser.is_blocked() {
                break;
            }
            thread::yield_now();
        }
        server.join().expect("parser-blocking fixture completed");
        assert_eq!(parser.resume().unwrap(), DocumentParseProgress::Complete);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-parser-blocking")])
                .to_js_string(),
            "external-ran"
        );
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-after-block")])
                .to_js_string(),
            "inline-ran"
        );
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("#after")])
                .get_property("textContent")
                .to_js_string(),
            "After"
        );
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
    }

    #[test]
    fn document_loader_applies_inline_and_external_stylesheets_in_source_order() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stylesheet fixture");
        let address = listener.local_addr().expect("stylesheet fixture address");
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept stylesheet document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /index.html "));
            let body = r#"<html><head>
                <style>.target { color: red; width: 10px; }</style>
                <link rel="stylesheet" href="/theme.css">
                <style>.other { color: green; }</style>
                </head><body><div class="target"></div></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write stylesheet document");

            let (mut stylesheet, _) = listener.accept().expect("accept stylesheet request");
            let request = read_http_request(&mut stylesheet);
            assert!(request.starts_with("GET /theme.css "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("accept: text/css,*/*;q=0.1")
            );
            let body = ".target { color: blue; height: 20px; }";
            write!(
                stylesheet,
                "HTTP/1.1 200 OK\r\nContent-Type: text/css; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write stylesheet response");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("stylesheet navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        server.join().expect("stylesheet fixture completed");

        let document = crate::jsdom::document_value();
        let target = document.call_method("querySelector", vec![Value::string(".target")]);
        let computed = crate::jsdom::window_value().call_method("getComputedStyle", vec![target]);
        assert_eq!(computed.get_property("color").to_js_string(), "#0000ff");
        assert_eq!(computed.get_property("width").to_js_string(), "10px");
        assert_eq!(computed.get_property("height").to_js_string(), "20px");

        let sheets = document.get_property("styleSheets");
        assert_eq!(sheets.get_property("length").to_u32(), 3);
        let styles = document.call_method("querySelectorAll", vec![Value::string("style")]);
        let links = document.call_method("querySelectorAll", vec![Value::string("link")]);
        assert_eq!(styles.get_property("length").to_u32(), 2);
        assert_eq!(links.get_property("length").to_u32(), 1);
        for element in [
            styles.call_method("item", vec![Value::Number(0.0)]),
            links.call_method("item", vec![Value::Number(0.0)]),
            styles.call_method("item", vec![Value::Number(1.0)]),
        ] {
            assert!(
                !element.get_property("sheet").is_null(),
                "each authored stylesheet must expose its CSSStyleSheet"
            );
        }
    }

    #[test]
    fn external_stylesheet_background_image_uses_fragment_base_and_shared_cache() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::image_loader::clear_cache();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind background image fixture");
        let address = listener
            .local_addr()
            .expect("background image fixture address");
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([21, 43, 65, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("encode background PNG");
        let png = png.into_inner();
        let server = thread::spawn(move || {
            let responses = [
                (
                    "/index.html",
                    "text/html",
                    b"<html><head><link rel=\"stylesheet\" href=\"/css/theme.css\"></head><body><div class=\"hero\"></div></body></html>".as_slice(),
                ),
                (
                    "/css/theme.css",
                    "text/css",
                    b".hero { width: 20px; height: 10px; background-image: url('../images/bg.png'); }".as_slice(),
                ),
                ("/images/bg.png", "image/png", png.as_slice()),
            ];
            for (path, content_type, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept background resource");
                let request = read_http_request(&mut stream);
                assert!(request.starts_with(&format!("GET {path} ")), "{request}");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .expect("write background response headers");
                stream
                    .write_all(body)
                    .expect("write background response body");
            }
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        let source = format!("http://{address}/images/bg.png");
        for _ in 0..10_000 {
            let _ = loader.poll();
            poll_script_fetches();
            if crate::image_loader::dimensions(&source) == Some((2, 1)) {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        server.join().expect("background image fixture completed");
        assert_eq!(crate::image_loader::dimensions(&source), Some((2, 1)));
        let hero = crate::jsdom::document_value()
            .call_method("querySelector", vec![Value::string(".hero")]);
        let computed = crate::jsdom::window_value().call_method("getComputedStyle", vec![hero]);
        assert_eq!(
            computed.get_property("backgroundImage").to_js_string(),
            format!("url('{source}')")
        );
    }

    #[test]
    fn stylesheet_import_graph_preserves_order_media_failures_and_cycles() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind import fixture");
        let address = listener.local_addr().expect("import fixture address");
        let server = thread::spawn(move || {
            let responses = [
                (
                    "/index.html",
                    "200 OK",
                    "text/html; charset=utf-8",
                    r#"<html><head><link rel="stylesheet" href="/root.css"></head>
                       <body><div class="target"></div></body></html>"#,
                ),
                (
                    "/root.css",
                    "200 OK",
                    "text/css",
                    r#"@import "/a.css";
                       @import "/missing.css";
                       @import url("/wide.css") screen and (min-width: 800px);
                       .target { color: black; }"#,
                ),
                (
                    "/a.css",
                    "200 OK",
                    "text/css",
                    r#"@import "/nested.css";
                       @import "/root.css";
                       .target { color: red; height: 20px; }"#,
                ),
                (
                    "/nested.css",
                    "200 OK",
                    "text/css",
                    ".target { color: blue; height: 30px; }",
                ),
                ("/missing.css", "404 Not Found", "text/css", "not found"),
                (
                    "/wide.css",
                    "200 OK",
                    "text/css",
                    ".target { width: 20px; }",
                ),
            ];
            for (path, status, content_type, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept import request");
                let request = read_http_request(&mut stream);
                assert!(
                    request.starts_with(&format!("GET {path} ")),
                    "unexpected stylesheet graph request: {request}"
                );
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                )
                .expect("write import response");
            }
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("stylesheet import navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        server.join().expect("stylesheet import fixture completed");

        let document = crate::jsdom::document_value();
        let target = document.call_method("querySelector", vec![Value::string(".target")]);
        let computed =
            || crate::jsdom::window_value().call_method("getComputedStyle", vec![target.clone()]);
        assert_eq!(computed().get_property("color").to_js_string(), "#000000");
        assert_eq!(computed().get_property("height").to_js_string(), "20px");
        assert_eq!(computed().get_property("width").to_js_string(), "20px");
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("length")
                .to_u32(),
            1
        );

        crate::jsdom::set_viewport(500.0, 900.0);
        assert_ne!(computed().get_property("width").to_js_string(), "20px");
        crate::jsdom::set_viewport(1024.0, 768.0);
        assert_eq!(computed().get_property("width").to_js_string(), "20px");
    }

    #[test]
    fn inline_stylesheet_import_uses_document_url_and_blocks_load_completion() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind inline import fixture");
        let address = listener
            .local_addr()
            .expect("inline import fixture address");
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept inline import document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /pages/index.html "));
            let body = r#"<html><head><style>
                @import "../shared/base.css";
                .target { color: black; }
                </style></head><body><div class="target"></div></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write inline import document");

            let (mut stylesheet, _) = listener.accept().expect("accept inline import");
            let request = read_http_request(&mut stylesheet);
            assert!(request.starts_with("GET /shared/base.css "));
            let body = ".target { color: red; height: 12px; }";
            write!(
                stylesheet,
                "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write inline import response");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/pages/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("inline stylesheet import navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        server.join().expect("inline import fixture completed");

        let document = crate::jsdom::document_value();
        let target = document.call_method("querySelector", vec![Value::string(".target")]);
        let computed = crate::jsdom::window_value().call_method("getComputedStyle", vec![target]);
        assert_eq!(computed.get_property("color").to_js_string(), "#000000");
        assert_eq!(computed.get_property("height").to_js_string(), "12px");
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
    }

    #[test]
    fn browser_picture_uses_selected_candidate_for_fetch_state_and_rendering() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::image_loader::clear_cache();
        crate::jsdom::set_viewport(800.0, 600.0);
        crate::jsdom::set_device_pixel_ratio(2.0);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind picture fixture");
        let address = listener.local_addr().expect("picture fixture address");
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept picture document");
            let _ = read_http_request(&mut document);
            let body = r#"<html><body><picture>
                <source type="image/svg+xml" srcset="/ignored.svg 2x">
                <source type="image/png" media="(min-width: 700px)"
                        srcset="/wide.png 1x, /wide-2x.png 2x">
                <img id="hero" src="/fallback.png"
                     srcset="/fallback.png 1x, /fallback-2x.png 2x">
            </picture></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write picture document");

            let (mut image, _) = listener.accept().expect("accept selected picture image");
            let request = read_http_request(&mut image);
            assert!(
                request.starts_with("GET /wide-2x.png "),
                "picture must fetch the selected source: {request}"
            );
            let pixels = image::RgbaImage::from_pixel(6, 4, image::Rgba([30, 60, 90, 255]));
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(pixels)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .expect("encode selected picture image");
            let bytes = bytes.into_inner();
            write!(
                image,
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len(),
            )
            .expect("write selected picture headers");
            image
                .write_all(&bytes)
                .expect("write selected picture body");
        });

        let mut loader = DocumentLoader::new(
            ScriptPolicy {
                allow_network: true,
                ..ScriptPolicy::default()
            },
            DocumentLoaderOptions::default(),
        );
        loader
            .navigate(&format!("http://{address}/index.html"))
            .expect("navigate picture fixture");
        // Navigation resets page globals; the host reapplies its current
        // viewport before the response is parsed and subresources are chosen.
        crate::jsdom::set_viewport(800.0, 600.0);
        crate::jsdom::set_device_pixel_ratio(2.0);
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("picture navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        server.join().expect("picture fixture completed");
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);

        let image = crate::jsdom::document_value()
            .call_method("querySelector", vec![Value::string("#hero")]);
        assert_eq!(
            image.get_property("src").to_js_string(),
            "/fallback.png",
            "the reflected fallback source must not be rewritten"
        );
        assert_eq!(
            image.get_property("srcset").to_js_string(),
            "/fallback.png 1x, /fallback-2x.png 2x"
        );
        assert_eq!(
            image.get_property("currentSrc").to_js_string(),
            format!("http://{address}/wide-2x.png")
        );
        assert_eq!(image.get_property("naturalWidth").to_u32(), 3);
        assert_eq!(image.get_property("naturalHeight").to_u32(), 2);
        assert_eq!(
            crate::image_loader::dimensions("/wide-2x.png"),
            Some((3, 2))
        );
        let tree = crate::dom::with_document(w3cos_dom::Document::to_component_tree);
        let picture = tree.children.first().expect("picture component");
        let rendered_image = picture
            .children
            .iter()
            .find(|component| {
                matches!(
                    component.kind,
                    w3cos_std::component::ComponentKind::Image { .. }
                )
            })
            .expect("selected image component");
        assert!(matches!(
            rendered_image.kind,
            w3cos_std::component::ComponentKind::Image { ref src } if src == "/wide-2x.png"
        ));
    }

    #[test]
    fn browser_image_loads_into_shared_cache_exposes_intrinsics_and_blocks_document_load() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::image_loader::clear_cache();
        let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([20, 40, 60, 255]));
        let mut image_bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut image_bytes, image::ImageFormat::Png)
            .unwrap();
        let image_bytes = image_bytes.into_inner();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind image fixture");
        let address = listener.local_addr().expect("image fixture address");
        let (requested_tx, requested_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept image document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /pages/index.html "));
            let body = r#"<html><body><img id="hero" src="../assets/hero.png"></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write image document");

            let (mut image, _) = listener.accept().expect("accept image subresource");
            let request = read_http_request(&mut image).to_ascii_lowercase();
            assert!(request.starts_with("get /assets/hero.png "));
            assert!(request.contains("accept: image/avif,image/webp"));
            requested_tx.send(()).expect("signal image request");
            release_rx.recv().expect("release image body");
            write!(
                image,
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nETag: \"hero-v1\"\r\nCache-Control: max-age=60\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                image_bytes.len(),
            )
            .expect("write image response headers");
            image
                .write_all(&image_bytes)
                .expect("write image response body");
        });

        let directory = tempfile::tempdir().expect("image cache directory");
        let mut loader = DocumentLoader::new(
            ScriptPolicy {
                compiled_cache_dir: Some(directory.path().to_path_buf()),
                ..ScriptPolicy::default()
            },
            DocumentLoaderOptions::default(),
        );
        loader
            .navigate(&format!("http://{address}/pages/index.html"))
            .unwrap();
        let mut image_request_started = false;
        for _ in 0..5_000 {
            let _ = loader.poll();
            if requested_rx.try_recv().is_ok() {
                image_request_started = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(image_request_started, "image request started");
        let image = crate::jsdom::document_value()
            .call_method("querySelector", vec![Value::string("#hero")]);
        assert!(!image.get_property("complete").to_bool());
        assert_eq!(image.get_property("naturalWidth").to_u32(), 0);
        assert_ne!(
            crate::jsdom::document_value()
                .get_property("readyState")
                .to_js_string(),
            "complete",
            "pending images must hold the document load lifecycle"
        );
        let decoded = image.call_method("decode", vec![]);
        assert!(matches!(
            w3cos_core::promise::status(&decoded),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));
        let load_count = Rc::new(Cell::new(0_u32));
        let load_count_for_handler = Rc::clone(&load_count);
        image.set_property(
            "onload",
            Value::function(move |_, _| {
                load_count_for_handler.set(load_count_for_handler.get() + 1);
                Value::Undefined
            }),
        );
        let image_error = Rc::new(RefCell::new(String::new()));
        let image_error_for_handler = Rc::clone(&image_error);
        image.set_property(
            "onerror",
            Value::function(move |_, args| {
                *image_error_for_handler.borrow_mut() =
                    args.first().map(Value::to_js_string).unwrap_or_default();
                Value::Undefined
            }),
        );
        let listener_count = Rc::new(Cell::new(0_u32));
        let listener_count_for_handler = Rc::clone(&listener_count);
        image.call_method(
            "addEventListener",
            vec![
                Value::string("load"),
                Value::function(move |_, _| {
                    listener_count_for_handler.set(listener_count_for_handler.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        release_tx.send(()).expect("release image fixture");
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("image navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        server.join().expect("image fixture completed");
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        assert!(
            image_error.borrow().is_empty(),
            "image unexpectedly failed: {}",
            image_error.borrow()
        );
        assert!(image.get_property("complete").to_bool());
        assert_eq!(image.get_property("naturalWidth").to_u32(), 4);
        assert_eq!(image.get_property("naturalHeight").to_u32(), 2);
        assert_eq!(
            image.get_property("currentSrc").to_js_string(),
            format!("http://{address}/assets/hero.png")
        );
        assert!(matches!(
            w3cos_core::promise::status(&decoded),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(load_count.get(), 1);
        assert_eq!(listener_count.get(), 1);
        assert_eq!(
            crate::image_loader::dimensions("../assets/hero.png"),
            Some((4, 2)),
            "renderers must consume the Browser-decoded cache entry"
        );
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("readyState")
                .to_js_string(),
            "complete"
        );
        assert!(
            loader.script_loader.http_source_cache_stats().writes >= 1,
            "image response must enter the shared persistent HTTP cache"
        );
    }

    #[test]
    fn blob_object_url_image_decodes_without_a_network_scheme() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::image_loader::clear_cache();

        let pixels = image::RgbaImage::from_pixel(3, 2, image::Rgba([20, 80, 160, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encode blob image");
        let bytes = w3cos_core::binary::typed_array_value(
            png.into_inner()
                .into_iter()
                .map(|byte| Value::Number(f64::from(byte)))
                .collect(),
        );
        let blob = w3cos_core::class::construct(
            &w3cos_core::web::blob_class(),
            vec![
                Value::array(vec![bytes]),
                Value::object(HashMap::from([("type".into(), Value::from("image/png"))])),
            ],
        );
        let source = w3cos_core::web::url_class()
            .call_method("createObjectURL", vec![blob])
            .to_js_string();
        let image = crate::dom::with_document_mut(|document| {
            let image = document.create_element("img");
            image.set_attribute(document, "src", &source);
            document.body().append_child(document, image);
            image.id.as_u32()
        });
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader.prepare_image_node(
            image,
            &Url::parse("http://127.0.0.1/document.html").unwrap(),
        );

        let element = crate::jsdom::element_value(image);
        assert!(element.get_property("complete").to_bool());
        assert_eq!(element.get_property("currentSrc").to_js_string(), source);
        assert_eq!(element.get_property("naturalWidth").to_u32(), 3);
        assert_eq!(element.get_property("naturalHeight").to_u32(), 2);
        assert_eq!(crate::image_loader::dimensions(&source), Some((3, 2)));
    }

    #[test]
    fn browser_image_decode_failure_rejects_decode_dispatches_error_and_releases_load() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind broken image fixture");
        let address = listener.local_addr().expect("broken image fixture address");
        let (requested_tx, requested_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept broken image document");
            let _ = read_http_request(&mut document);
            let body = r#"<html><body><img id="broken" src="/broken.png"></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write broken image document");

            let (mut image, _) = listener.accept().expect("accept broken image");
            let request = read_http_request(&mut image);
            assert!(request.starts_with("GET /broken.png "));
            requested_tx.send(()).expect("signal broken image request");
            release_rx.recv().expect("release broken image");
            let body = b"not an image";
            write!(
                image,
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            )
            .expect("write broken image response");
            image.write_all(body).expect("write broken image bytes");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        let mut image_request_started = false;
        for _ in 0..5_000 {
            let _ = loader.poll();
            if requested_rx.try_recv().is_ok() {
                image_request_started = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(image_request_started, "broken image request started");
        let image = crate::jsdom::document_value()
            .call_method("querySelector", vec![Value::string("#broken")]);
        let decoded = image.call_method("decode", vec![]);
        let error_count = Rc::new(Cell::new(0_u32));
        let error_count_for_handler = Rc::clone(&error_count);
        image.set_property(
            "onerror",
            Value::function(move |_, _| {
                error_count_for_handler.set(error_count_for_handler.get() + 1);
                Value::Undefined
            }),
        );
        let listener_count = Rc::new(Cell::new(0_u32));
        let listener_count_for_handler = Rc::clone(&listener_count);
        image.call_method(
            "addEventListener",
            vec![
                Value::string("error"),
                Value::function(move |_, _| {
                    listener_count_for_handler.set(listener_count_for_handler.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        release_tx.send(()).expect("release broken image fixture");
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("broken image must not fail navigation: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        server.join().expect("broken image fixture completed");
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        assert!(image.get_property("complete").to_bool());
        assert_eq!(image.get_property("naturalWidth").to_u32(), 0);
        assert_eq!(image.get_property("naturalHeight").to_u32(), 0);
        let Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) =
            w3cos_core::promise::status(&decoded)
        else {
            panic!("HTMLImageElement.decode() must reject after decode failure");
        };
        assert_eq!(reason.get_property("name").to_js_string(), "EncodingError");
        assert_eq!(error_count.get(), 1);
        assert_eq!(listener_count.get(), 1);
    }

    #[test]
    fn detached_image_factory_uses_the_same_async_browser_loader() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::image_loader::clear_cache();
        let pixels = image::RgbaImage::from_pixel(1, 3, image::Rgba([7, 8, 9, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let bytes = bytes.into_inner();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind detached image fixture");
        let address = listener.local_addr().expect("detached image address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept detached image");
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /preload.png "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len(),
            )
            .expect("write detached image headers");
            stream.write_all(&bytes).expect("write detached image");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let image = w3cos_core::class::construct(
            &crate::jsdom::window_value().get_property("Image"),
            vec![],
        );
        assert!(!image.get_property("isConnected").to_bool());
        image.set_property("src", Value::string("/preload.png"));
        let decoded = image.call_method("decode", vec![]);
        for _ in 0..5_000 {
            poll_script_fetches();
            crate::jsdom::drain_microtasks();
            if image.get_property("complete").to_bool() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        server.join().expect("detached image fixture completed");
        assert!(image.get_property("complete").to_bool());
        assert_eq!(image.get_property("naturalWidth").to_u32(), 1);
        assert_eq!(image.get_property("naturalHeight").to_u32(), 3);
        assert!(matches!(
            w3cos_core::promise::status(&decoded),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(
            crate::image_loader::dimensions("/preload.png"),
            Some((1, 3))
        );
    }

    #[test]
    fn stylesheet_font_face_loads_woff2_subsets_and_cleans_up_with_owner() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind font fixture");
        let address = listener.local_addr().expect("font fixture address");
        let font_bytes = narrow_inter_woff2_bytes();
        let expected_font_len = crate::font_face::normalize_font_bytes(&font_bytes)
            .expect("decode WOFF2 fixture")
            .len();
        let digit_font_bytes = include_bytes!("../assets/Inter-Regular.ttf").to_vec();
        let expected_digit_font_len = digit_font_bytes.len();
        fn target_text_style(component: &w3cos_std::Component) -> Option<&w3cos_std::style::Style> {
            if matches!(
                &component.kind,
                w3cos_std::ComponentKind::Text { content } if content == "W3W"
            ) {
                return Some(&component.style);
            }
            component.children.iter().find_map(target_text_style)
        }
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept font document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /pages/index.html "));
            let body = r#"<html><head>
                <link id="font-sheet" rel="stylesheet" href="/assets/fonts.css">
                </head><body><div class="target">W3W</div></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write font document");

            let (mut stylesheet, _) = listener.accept().expect("accept font stylesheet");
            let request = read_http_request(&mut stylesheet);
            assert!(request.starts_with("GET /assets/fonts.css "));
            let body = r#"@font-face {
                    font-family: "W3COS Remote Test";
                    src: url("./missing.woff2") format("woff2"),
                         local("Definitely Missing W3COS Font"),
                         url("./inter.woff2") format("woff2");
                    font-weight: 400;
                    font-style: normal;
                    font-display: swap;
                    unicode-range: U+0057;
                }
                @font-face {
                    font-family: "W3COS Remote Test";
                    src: url("./inter.ttf") format("truetype");
                    font-weight: 400;
                    font-style: normal;
                    unicode-range: U+0030-0039;
                }
                @font-face {
                    font-family: "W3COS Remote Test";
                    src: url("./unused.ttf") format("truetype");
                    font-weight: 400;
                    font-style: normal;
                    unicode-range: U+1F600-1F64F;
                }
                @font-face {
                    font-family: "W3COS Fallback Test";
                    src: url("./fallback.ttf") format("truetype");
                    font-weight: 400;
                    font-style: normal;
                    unicode-range: U+0057;
                }
                .target { font-family: "W3COS Remote Test"; }"#;
            write!(
                stylesheet,
                "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write font stylesheet");

            let (mut missing, _) = listener.accept().expect("accept missing WOFF2");
            let request = read_http_request(&mut missing);
            assert!(request.starts_with("GET /assets/missing.woff2 "));
            write!(
                missing,
                "HTTP/1.1 404 Not Found\r\nContent-Type: font/woff2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write missing WOFF2 response");

            let (mut font, _) = listener.accept().expect("accept WOFF2 fallback");
            let request = read_http_request(&mut font);
            assert!(
                request.starts_with("GET /assets/inter.woff2 "),
                "failed WOFF2 must advance to the next source: {request}"
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("accept: font/woff2,font/woff,font/ttf,font/otf")
            );
            write!(
                font,
                "HTTP/1.1 200 OK\r\nContent-Type: font/woff2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                font_bytes.len(),
            )
            .expect("write font response headers");
            font.write_all(&font_bytes)
                .expect("write font response body");

            let (mut digit_font, _) = listener.accept().expect("accept digit TTF subset");
            let request = read_http_request(&mut digit_font);
            assert!(request.starts_with("GET /assets/inter.ttf "));
            write!(
                digit_font,
                "HTTP/1.1 200 OK\r\nContent-Type: font/ttf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                digit_font_bytes.len(),
            )
            .expect("write digit font response headers");
            digit_font
                .write_all(&digit_font_bytes)
                .expect("write digit font response body");

            listener
                .set_nonblocking(true)
                .expect("make unused subset check nonblocking");
            for _ in 0..100 {
                match listener.accept() {
                    Ok((mut unexpected, _)) => {
                        let request = read_http_request(&mut unexpected);
                        panic!("unused unicode-range subset was fetched: {request}");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("check unused subset request: {error}"),
                }
            }
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/pages/index.html"))
            .unwrap();
        let mut demand_started = false;
        for _ in 0..5_000 {
            let progress = loader.poll();
            if !demand_started {
                let fonts = crate::jsdom::document_value().get_property("fonts");
                if fonts.get_property("size").to_u32() == 4 {
                    let demand_style = w3cos_std::style::Style {
                        font_family: Some(
                            "\"W3COS Remote Test\", \"W3COS Fallback Test\"".to_string(),
                        ),
                        font_weight: 500,
                        ..Default::default()
                    };
                    crate::font_face::FontRegistry::global()
                        .resolve_style_runs(&demand_style, "W3W");
                    demand_started = true;
                    continue;
                }
            }
            let requested_fonts_loaded = demand_started
                && crate::font_face::FontRegistry::global()
                    .resolve_for_character(
                        "W3COS Remote Test",
                        crate::font_face::FontWeight::NORMAL,
                        crate::font_face::FontFaceStyle::Normal,
                        'W',
                    )
                    .is_some()
                && crate::font_face::FontRegistry::global()
                    .resolve_for_character(
                        "W3COS Remote Test",
                        crate::font_face::FontWeight::NORMAL,
                        crate::font_face::FontFaceStyle::Normal,
                        '3',
                    )
                    .is_some();
            match progress {
                DocumentLoadProgress::Complete if requested_fonts_loaded => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("font navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert!(
            demand_started,
            "stylesheet faces must be discovered for demand"
        );
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        server.join().expect("font fixture completed");

        let registry = crate::font_face::FontRegistry::global();
        let w_subset = registry
            .resolve_for_character(
                "W3COS Remote Test",
                crate::font_face::FontWeight::NORMAL,
                crate::font_face::FontFaceStyle::Normal,
                'W',
            )
            .expect("network WOFF2 subset registered");
        let digit_subset = registry
            .resolve_for_character(
                "W3COS Remote Test",
                crate::font_face::FontWeight::NORMAL,
                crate::font_face::FontFaceStyle::Normal,
                '3',
            )
            .expect("network TTF digit subset registered");
        assert_eq!(w_subset.data.len(), expected_font_len);
        assert_eq!(digit_subset.data.len(), expected_digit_font_len);
        assert_ne!(w_subset.cache_key(), digit_subset.cache_key());
        let document = crate::jsdom::document_value();
        let tree = crate::dom::to_component_tree();
        let style = target_text_style(&tree)
            .expect("styled target text")
            .clone();
        assert_eq!(style.font_family.as_deref(), Some("W3COS Remote Test"));
        let runs = registry.resolve_style_runs(&style, "W3W");
        assert_eq!(
            runs.iter()
                .map(|run| run
                    .font
                    .as_ref()
                    .map(crate::font_face::LoadedFont::cache_key))
                .collect::<Vec<_>>(),
            [
                Some(w_subset.cache_key()),
                Some(digit_subset.cache_key()),
                Some(w_subset.cache_key())
            ]
        );
        let custom_width = crate::layout::text_intrinsic_size("WWWWWWWW", &style).0;
        let fallback_width = crate::layout::text_intrinsic_size(
            "WWWWWWWW",
            &w3cos_std::style::Style {
                font_family: None,
                ..style.clone()
            },
        )
        .0;
        assert!(
            custom_width < fallback_width * 0.75,
            "loaded CSS font must drive layout metrics ({custom_width} vs {fallback_width})"
        );
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
        let fonts = document.get_property("fonts");
        assert_eq!(fonts.get_property("size").to_u32(), 4);
        let stylesheet_faces = fonts.call_method("values", vec![]);
        for index in 0..2 {
            let face = stylesheet_faces.get_property(&index.to_string());
            assert!(w3cos_core::class::instance_of(
                &face,
                &crate::font_loading_web::font_face_class()
            ));
            assert_eq!(
                face.get_property("family").to_js_string(),
                "W3COS Remote Test"
            );
            assert_eq!(face.get_property("status").to_js_string(), "loaded");
        }
        let unused_face = stylesheet_faces.get_property("2");
        assert_eq!(
            unused_face.get_property("status").to_js_string(),
            "unloaded",
            "a unicode-range with no matching text must remain lazy"
        );
        let fallback_face = stylesheet_faces.get_property("3");
        assert_eq!(
            fallback_face.get_property("status").to_js_string(),
            "unloaded",
            "a later family must remain lazy when the first family covers the glyph"
        );

        let link = document.call_method("querySelector", vec![Value::string("#font-sheet")]);
        document
            .get_property("head")
            .call_method("removeChild", vec![link]);
        crate::jsdom::drain_microtasks();
        assert_eq!(fonts.get_property("size").to_u32(), 0);
        assert!(
            crate::font_face::FontRegistry::global()
                .resolve(
                    "W3COS Remote Test",
                    crate::font_face::FontWeight::NORMAL,
                    crate::font_face::FontFaceStyle::Normal,
                )
                .is_none(),
            "removing the stylesheet must release owner-scoped font data"
        );
    }

    #[test]
    fn stylesheet_font_face_defers_inactive_media_until_viewport_matches() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind deferred font fixture");
        let address = listener
            .local_addr()
            .expect("deferred font fixture address");
        let font_bytes = include_bytes!("../assets/Inter-Regular.ttf").to_vec();
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept deferred font document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /index.html "));
            let body = r#"<html><head>
                <style id="conditional-font" media="(min-width: 1000px)">
                    @font-face {
                        font-family: "W3COS Deferred Media Test";
                        src: url("/inter.ttf") format("truetype");
                    }
                </style>
                </head><body></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write deferred font document");

            let (mut font, _) = listener
                .accept()
                .expect("accept font only after media activation");
            let request = read_http_request(&mut font);
            assert!(request.starts_with("GET /inter.ttf "));
            write!(
                font,
                "HTTP/1.1 200 OK\r\nContent-Type: font/ttf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                font_bytes.len(),
            )
            .expect("write deferred font response headers");
            font.write_all(&font_bytes)
                .expect("write deferred font response body");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        crate::jsdom::set_viewport(500.0, 900.0);
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("deferred font navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(
            loader.progress(),
            &DocumentLoadProgress::Complete,
            "an inactive @font-face must not block document completion"
        );
        let registry = crate::font_face::FontRegistry::global();
        assert!(
            registry
                .resolve(
                    "W3COS Deferred Media Test",
                    crate::font_face::FontWeight::NORMAL,
                    crate::font_face::FontFaceStyle::Normal,
                )
                .is_none(),
            "an inactive media query must not register or fetch its font"
        );

        let fonts = crate::jsdom::document_value().get_property("fonts");
        assert_eq!(fonts.get_property("size").to_u32(), 1);
        let stylesheet_face = fonts.call_method("values", vec![]).get_property("0");
        assert!(w3cos_core::class::instance_of(
            &stylesheet_face,
            &crate::font_loading_web::font_face_class()
        ));
        assert_eq!(
            stylesheet_face.get_property("family").to_js_string(),
            "W3COS Deferred Media Test"
        );
        assert_eq!(
            stylesheet_face.get_property("status").to_js_string(),
            "unloaded"
        );
        assert!(
            !fonts
                .call_method("delete", vec![stylesheet_face.clone()])
                .to_bool()
        );
        fonts.call_method("clear", vec![]);
        assert_eq!(fonts.get_property("size").to_u32(), 1);

        let loading_events = Rc::new(Cell::new(0));
        let loading_events_for_callback = Rc::clone(&loading_events);
        let loading_face_matches = Rc::new(Cell::new(false));
        let loading_face_matches_for_callback = Rc::clone(&loading_face_matches);
        let stylesheet_face_for_loading = stylesheet_face.clone();
        let ready = Rc::new(Cell::new(false));
        let ready_for_loading = Rc::clone(&ready);
        fonts.call_method(
            "addEventListener",
            vec![
                Value::string("loading"),
                Value::function(move |this, args| {
                    loading_events_for_callback.set(loading_events_for_callback.get() + 1);
                    let event_face = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .get_property("fontfaces")
                        .get_property("0");
                    loading_face_matches_for_callback.set(
                        event_face == stylesheet_face_for_loading
                            && event_face.get_property("status").to_js_string() == "loading",
                    );
                    let ready_for_loading = Rc::clone(&ready_for_loading);
                    this.get_property("ready").call_method(
                        "then",
                        vec![Value::function(move |_, _| {
                            ready_for_loading.set(true);
                            Value::Undefined
                        })],
                    );
                    Value::Undefined
                }),
            ],
        );
        let loading_done_events = Rc::new(Cell::new(0));
        let loading_done_events_for_callback = Rc::clone(&loading_done_events);
        let done_face_matches = Rc::new(Cell::new(false));
        let done_face_matches_for_callback = Rc::clone(&done_face_matches);
        let stylesheet_face_for_done = stylesheet_face.clone();
        fonts.call_method(
            "addEventListener",
            vec![
                Value::string("loadingdone"),
                Value::function(move |_, args| {
                    loading_done_events_for_callback
                        .set(loading_done_events_for_callback.get() + 1);
                    let event_face = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .get_property("fontfaces")
                        .get_property("0");
                    done_face_matches_for_callback.set(
                        event_face == stylesheet_face_for_done
                            && event_face.get_property("status").to_js_string() == "loaded",
                    );
                    Value::Undefined
                }),
            ],
        );
        assert_eq!(
            request_stylesheet_font_faces(std::slice::from_ref(&stylesheet_face), ""),
            1,
            "explicit demand must remember the face while its media is inactive"
        );
        assert_eq!(
            stylesheet_face.get_property("status").to_js_string(),
            "unloaded"
        );
        crate::jsdom::set_viewport(1200.0, 900.0);
        assert_eq!(fonts.get_property("status").to_js_string(), "loading");
        assert_eq!(loading_events.get(), 1);
        assert!(loading_face_matches.get());
        assert!(!ready.get());
        for _ in 0..5_000 {
            crate::jsdom::drain_microtasks();
            poll_script_fetches();
            if registry
                .resolve(
                    "W3COS Deferred Media Test",
                    crate::font_face::FontWeight::NORMAL,
                    crate::font_face::FontFaceStyle::Normal,
                )
                .is_some()
            {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            registry
                .resolve(
                    "W3COS Deferred Media Test",
                    crate::font_face::FontWeight::NORMAL,
                    crate::font_face::FontFaceStyle::Normal,
                )
                .is_some(),
            "a newly matching media query must activate the shared font loader"
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(fonts.get_property("status").to_js_string(), "loaded");
        assert_eq!(loading_done_events.get(), 1);
        assert!(done_face_matches.get());
        assert_eq!(
            stylesheet_face.get_property("status").to_js_string(),
            "loaded"
        );
        assert!(ready.get());
        server.join().expect("deferred font fixture completed");

        let document = crate::jsdom::document_value();
        let style = document.call_method("querySelector", vec![Value::string("#conditional-font")]);
        document
            .get_property("head")
            .call_method("removeChild", vec![style]);
        crate::jsdom::drain_microtasks();
        assert_eq!(fonts.get_property("size").to_u32(), 0);
        assert!(
            registry
                .resolve(
                    "W3COS Deferred Media Test",
                    crate::font_face::FontWeight::NORMAL,
                    crate::font_face::FontFaceStyle::Normal,
                )
                .is_none(),
            "removing the stylesheet must clean up an activated deferred font"
        );
    }

    #[test]
    fn document_fonts_load_activates_only_text_matching_stylesheet_face() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind font load fixture");
        let address = listener.local_addr().expect("font load fixture address");
        let font_bytes = include_bytes!("../assets/Inter-Regular.ttf").to_vec();
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept font load document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /index.html "));
            let body = r#"<html><head><style>
                @font-face {
                    font-family: "W3COS Font Load Test";
                    src: url("/latin.ttf") format("truetype");
                    unicode-range: U+0041;
                }
                @font-face {
                    font-family: "W3COS Font Load Test";
                    src: url("/emoji.ttf") format("truetype");
                    unicode-range: U+1F600-1F64F;
                }
                </style></head><body></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write font load document");

            let (mut latin, _) = listener.accept().expect("accept demanded Latin subset");
            let request = read_http_request(&mut latin);
            assert!(request.starts_with("GET /latin.ttf "));
            write!(
                latin,
                "HTTP/1.1 200 OK\r\nContent-Type: font/ttf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                font_bytes.len(),
            )
            .expect("write Latin font headers");
            latin.write_all(&font_bytes).expect("write Latin font body");

            listener
                .set_nonblocking(true)
                .expect("make emoji subset check nonblocking");
            for _ in 0..100 {
                match listener.accept() {
                    Ok((mut unexpected, _)) => {
                        let request = read_http_request(&mut unexpected);
                        panic!("non-matching FontFaceSet.load subset was fetched: {request}");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("check non-matching font request: {error}"),
                }
            }
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("font load navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        let fonts = crate::jsdom::document_value().get_property("fonts");
        assert_eq!(fonts.get_property("size").to_u32(), 2);
        let faces = fonts.call_method("values", vec![]);
        assert_eq!(
            faces
                .get_property("0")
                .get_property("status")
                .to_js_string(),
            "unloaded"
        );
        assert_eq!(
            faces
                .get_property("1")
                .get_property("status")
                .to_js_string(),
            "unloaded"
        );

        let load = fonts.call_method(
            "load",
            vec![
                Value::string("500 16px \"W3COS Font Load Test\""),
                Value::string("A"),
            ],
        );
        assert!(matches!(
            w3cos_core::promise::status(&load),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));
        for _ in 0..5_000 {
            poll_script_fetches();
            crate::jsdom::drain_microtasks();
            if !matches!(
                w3cos_core::promise::status(&load),
                Some(w3cos_core::promise::PromiseStatus::Pending)
            ) {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        let Some(w3cos_core::promise::PromiseStatus::Fulfilled(loaded)) =
            w3cos_core::promise::status(&load)
        else {
            panic!("FontFaceSet.load must fulfill after the demanded subset loads");
        };
        assert_eq!(loaded.get_property("length").to_u32(), 1);
        assert_eq!(
            faces
                .get_property("0")
                .get_property("status")
                .to_js_string(),
            "loaded"
        );
        assert_eq!(
            faces
                .get_property("1")
                .get_property("status")
                .to_js_string(),
            "unloaded"
        );
        server.join().expect("font load fixture completed");
    }

    #[test]
    fn programmatic_network_font_face_uses_browser_fetch_cors_cookies_and_cache() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind programmatic font fixture");
        let address = listener
            .local_addr()
            .expect("programmatic font fixture address");
        let font_bytes = include_bytes!("../assets/Inter-Regular.ttf").to_vec();
        let server = thread::spawn(move || {
            let (mut document, _) = listener
                .accept()
                .expect("accept programmatic font document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /index.html "));
            let body = "<html><head></head><body></body></html>";
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: font_session=secret; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write programmatic font document");
            drop(document);

            let (mut first_font, _) = listener.accept().expect("accept first FontFace fetch");
            let request = read_http_request(&mut first_font);
            assert!(request.starts_with("GET /programmatic.ttf "));
            let lower = request.to_ascii_lowercase();
            assert!(lower.contains("accept: font/woff2,font/woff,font/ttf,font/otf"));
            assert!(
                !lower.contains("cookie:"),
                "FontFace fetches use CORS with credentials omitted"
            );
            write!(
                first_font,
                "HTTP/1.1 200 OK\r\nContent-Type: font/ttf\r\nETag: \"programmatic-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                font_bytes.len(),
            )
            .expect("write first FontFace response headers");
            first_font
                .write_all(&font_bytes)
                .expect("write first FontFace response body");
            drop(first_font);

            let (mut revalidation, _) = listener.accept().expect("accept FontFace revalidation");
            let request = read_http_request(&mut revalidation);
            assert!(request.starts_with("GET /programmatic.ttf "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("if-none-match: \"programmatic-v1\""),
                "the second programmatic FontFace must revalidate through shared Browser cache"
            );
            write!(
                revalidation,
                "HTTP/1.1 304 Not Modified\r\nETag: \"programmatic-v1\"\r\nConnection: close\r\n\r\n"
            )
            .expect("write FontFace 304 response");
        });

        let cache = tempfile::tempdir().expect("programmatic font cache directory");
        let mut policy = ScriptPolicy::default();
        policy.compiled_cache_dir = Some(cache.path().to_path_buf());
        let mut loader = DocumentLoader::new(policy, DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("programmatic font navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);

        let window = crate::jsdom::window_value();
        let fonts = crate::jsdom::document_value().get_property("fonts");
        let make_face = |family: &str| {
            w3cos_core::class::construct(
                &window.get_property("FontFace"),
                vec![
                    Value::string(family),
                    Value::string("url(\"/programmatic.ttf\") format(\"truetype\")"),
                ],
            )
        };
        let first = make_face("W3COS Programmatic Network One");
        let second = make_face("W3COS Programmatic Network Two");
        fonts.call_method("add", vec![first.clone()]);
        fonts.call_method("add", vec![second.clone()]);

        let first_resolved = Rc::new(Cell::new(false));
        let first_resolved_for_callback = Rc::clone(&first_resolved);
        first.call_method("load", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                first_resolved_for_callback
                    .set(args[0].get_property("status").to_js_string() == "loaded");
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert!(first_resolved.get());
        assert_eq!(first.get_property("status").to_js_string(), "loaded");

        let second_resolved = Rc::new(Cell::new(false));
        let second_resolved_for_callback = Rc::clone(&second_resolved);
        second.call_method("load", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                second_resolved_for_callback
                    .set(args[0].get_property("status").to_js_string() == "loaded");
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert!(second_resolved.get());
        assert_eq!(second.get_property("status").to_js_string(), "loaded");
        assert!(
            fonts
                .call_method(
                    "check",
                    vec![Value::string("12px W3COS Programmatic Network Two")],
                )
                .to_bool()
        );
        server.join().expect("programmatic font fixture completed");
    }

    #[test]
    fn programmatic_network_font_face_rejects_cross_origin_without_cors() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let document_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind FontFace CORS document fixture");
        let document_address = document_listener
            .local_addr()
            .expect("FontFace CORS document address");
        let font_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind cross-origin FontFace fixture");
        let font_address = font_listener
            .local_addr()
            .expect("cross-origin FontFace address");
        let document_server = thread::spawn(move || {
            let (mut document, _) = document_listener
                .accept()
                .expect("accept FontFace CORS document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /index.html "));
            let body = "<html><head></head><body></body></html>";
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .expect("write FontFace CORS document");
        });
        let font_server = thread::spawn(move || {
            let (mut font, _) = font_listener
                .accept()
                .expect("accept cross-origin FontFace request");
            let request = read_http_request(&mut font);
            assert!(request.starts_with("GET /blocked.ttf "));
            assert!(request.to_ascii_lowercase().contains("origin: http://"));
            write!(
                font,
                "HTTP/1.1 200 OK\r\nContent-Type: font/ttf\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write FontFace response without CORS headers");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{document_address}/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("FontFace CORS document navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        document_server
            .join()
            .expect("FontFace CORS document fixture completed");

        let window = crate::jsdom::window_value();
        let fonts = crate::jsdom::document_value().get_property("fonts");
        let face = w3cos_core::class::construct(
            &window.get_property("FontFace"),
            vec![
                Value::string("W3COS Programmatic CORS Failure"),
                Value::string(&format!("url(\"http://{font_address}/blocked.ttf\")")),
            ],
        );
        fonts.call_method("add", vec![face.clone()]);
        let error_events = Rc::new(Cell::new(0));
        let error_events_for_callback = Rc::clone(&error_events);
        let face_for_event = face.clone();
        fonts.call_method(
            "addEventListener",
            vec![
                Value::string("loadingerror"),
                Value::function(move |_, args| {
                    let event_face = args[0].get_property("fontfaces").get_property("0");
                    if event_face == face_for_event
                        && event_face.get_property("status").to_js_string() == "error"
                    {
                        error_events_for_callback.set(error_events_for_callback.get() + 1);
                    }
                    Value::Undefined
                }),
            ],
        );
        let rejected = Rc::new(Cell::new(false));
        let rejected_for_callback = Rc::clone(&rejected);
        face.call_method("load", vec![]).call_method(
            "catch",
            vec![Value::function(move |_, _| {
                rejected_for_callback.set(true);
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert!(rejected.get());
        assert_eq!(error_events.get(), 1);
        assert_eq!(face.get_property("status").to_js_string(), "error");
        assert_eq!(fonts.get_property("status").to_js_string(), "loaded");
        font_server
            .join()
            .expect("cross-origin FontFace fixture completed");
    }

    #[test]
    fn stylesheet_media_conditions_react_to_viewport_and_dpr_changes() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let mut parser =
            StreamingDocumentParser::new(loader, "http://example.test/index.html").unwrap();
        parser
            .write(
                r#"<html><head>
                <style>
                    .target { color: #111111; width: 10px; height: 10px; }
                    @media (max-width: 600px) {
                        .target { color: #0000ff; width: 20px; }
                    }
                    @media screen and (min-width: 400px) {
                        @media (max-width: 900px) {
                            .target { width: 25px; }
                        }
                    }
                    @media (min-resolution: 2dppx) {
                        .target { height: 30px; }
                    }
                </style>
                <style media="screen and (min-width: 800px)">
                    .target { color: #008000; }
                </style>
                </head><body><div class="target"></div></body></html>"#,
            )
            .unwrap();
        assert_eq!(parser.finish().unwrap(), DocumentParseProgress::Complete);

        let document = crate::jsdom::document_value();
        let target = document.call_method("querySelector", vec![Value::string(".target")]);
        let computed =
            || crate::jsdom::window_value().call_method("getComputedStyle", vec![target.clone()]);
        assert_eq!(computed().get_property("color").to_js_string(), "#008000");
        assert_eq!(computed().get_property("width").to_js_string(), "10px");
        assert_eq!(computed().get_property("height").to_js_string(), "10px");
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("length")
                .to_u32(),
            2
        );
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("1")
                .get_property("media")
                .get_property("mediaText")
                .to_js_string(),
            "screen and (min-width: 800px)"
        );

        crate::jsdom::set_viewport(500.0, 900.0);
        assert_eq!(computed().get_property("color").to_js_string(), "#0000ff");
        assert_eq!(computed().get_property("width").to_js_string(), "25px");
        assert_eq!(computed().get_property("height").to_js_string(), "10px");

        crate::jsdom::set_device_pixel_ratio(2.0);
        assert_eq!(computed().get_property("height").to_js_string(), "30px");

        crate::jsdom::set_viewport(300.0, 900.0);
        assert_eq!(computed().get_property("color").to_js_string(), "#0000ff");
        assert_eq!(computed().get_property("width").to_js_string(), "20px");
    }

    #[test]
    fn loaded_stylesheets_replace_disable_reenable_and_remove_without_stale_rules() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mutable stylesheet fixture");
        let address = listener
            .local_addr()
            .expect("mutable stylesheet fixture address");
        let server = thread::spawn(move || {
            let (mut document, _) = listener.accept().expect("accept mutable CSS document");
            let _ = read_http_request(&mut document);
            let body = r#"<html><head>
                <style id="inline">.target { color: red; }</style>
                <link id="theme" rel="stylesheet" href="/theme.css">
                </head><body><div class="target"></div></body></html>"#;
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write mutable CSS document");
            for request_index in 0..3 {
                let (mut stylesheet, _) = listener.accept().expect("accept mutable CSS request");
                let request = read_http_request(&mut stylesheet);
                let alternate = request.starts_with("GET /alternate.css ");
                assert!(alternate || request.starts_with("GET /theme.css "));
                let body = if alternate {
                    ".target { color: #800080; }"
                } else {
                    ".target { color: blue; }"
                };
                write!(
                    stylesheet,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nX-Request: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    request_index + 1,
                    body.len(),
                    body
                )
                .expect("write mutable CSS response");
            }
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("mutable stylesheet navigation failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        let document = crate::jsdom::document_value();
        let target = document.call_method("querySelector", vec![Value::string(".target")]);
        let computed_color = || {
            crate::jsdom::window_value()
                .call_method("getComputedStyle", vec![target.clone()])
                .get_property("color")
                .to_js_string()
        };
        assert_eq!(computed_color(), "#0000ff");
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("length")
                .to_u32(),
            2
        );

        let inline = document.call_method("querySelector", vec![Value::string("#inline")]);
        inline.set_property("textContent", Value::string(".target { color: #008000; }"));
        crate::jsdom::drain_microtasks();
        assert_eq!(
            computed_color(),
            "#0000ff",
            "replacing the earlier inline sheet must preserve DOM cascade order"
        );

        let theme = document.call_method("querySelector", vec![Value::string("#theme")]);
        theme.set_property("disabled", Value::Bool(true));
        crate::jsdom::drain_microtasks();
        assert_eq!(computed_color(), "#008000");
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("length")
                .to_u32(),
            1
        );
        assert!(theme.get_property("sheet").is_nullish());

        theme.set_property("disabled", Value::Bool(false));
        for _ in 0..5_000 {
            crate::jsdom::drain_microtasks();
            poll_script_fetches();
            if !has_pending_script_fetches() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(computed_color(), "#0000ff");
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("length")
                .to_u32(),
            2
        );

        theme.set_property("href", Value::string("/alternate.css"));
        for _ in 0..5_000 {
            crate::jsdom::drain_microtasks();
            poll_script_fetches();
            if !has_pending_script_fetches() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(computed_color(), "#800080");
        assert_eq!(
            theme
                .get_property("sheet")
                .get_property("href")
                .to_js_string(),
            format!("http://{address}/alternate.css")
        );

        document
            .get_property("head")
            .call_method("removeChild", vec![theme.clone()]);
        crate::jsdom::drain_microtasks();
        assert_eq!(computed_color(), "#008000");
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("length")
                .to_u32(),
            1
        );
        assert!(theme.get_property("sheet").is_nullish());
        server.join().expect("mutable stylesheet fixture completed");
    }

    #[test]
    fn stylesheet_revalidates_shared_http_cache_with_vary_request_headers() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let directory = tempfile::tempdir().expect("create stylesheet cache directory");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stylesheet cache fixture");
        let address = listener
            .local_addr()
            .expect("stylesheet cache fixture address");
        let server = thread::spawn(move || {
            for request_index in 0..2 {
                let (mut document, _) = listener.accept().expect("accept cached CSS document");
                let request = read_http_request(&mut document);
                assert!(request.starts_with("GET /index.html "));
                let body = r#"<html><head><link rel="stylesheet" href="/theme.css"></head>
                    <body><div class="cached-target"></div></body></html>"#;
                write!(
                    document,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write cached CSS document");

                let (mut stylesheet, _) = listener.accept().expect("accept cached CSS request");
                let request = read_http_request(&mut stylesheet);
                let request_lower = request.to_ascii_lowercase();
                assert!(request.starts_with("GET /theme.css "));
                assert!(request_lower.contains("accept: text/css,*/*;q=0.1"));
                if request_index == 0 {
                    assert!(!request_lower.contains("if-none-match:"));
                    let body = ".cached-target { color: #800080; }";
                    write!(
                        stylesheet,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nETag: \"css-v1\"\r\nVary: Accept\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("write initial cached CSS");
                } else {
                    assert!(request_lower.contains("if-none-match: \"css-v1\""));
                    write!(
                        stylesheet,
                        "HTTP/1.1 304 Not Modified\r\nETag: \"css-v1\"\r\nVary: Accept\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .expect("write cached CSS revalidation");
                }
            }
        });

        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };
        let mut loader = DocumentLoader::new(policy, DocumentLoaderOptions::default());
        let page_url = format!("http://{address}/index.html");
        for navigation in 0..2 {
            loader.navigate(&page_url).unwrap();
            for _ in 0..5_000 {
                match loader.poll() {
                    DocumentLoadProgress::Complete => break,
                    DocumentLoadProgress::Failed(error) => {
                        panic!("cached stylesheet navigation {navigation} failed: {error}")
                    }
                    _ => thread::sleep(std::time::Duration::from_millis(1)),
                }
            }
            assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
            let document = crate::jsdom::document_value();
            let target =
                document.call_method("querySelector", vec![Value::string(".cached-target")]);
            let computed =
                crate::jsdom::window_value().call_method("getComputedStyle", vec![target]);
            assert_eq!(computed.get_property("color").to_js_string(), "#800080");
        }
        server.join().expect("stylesheet cache fixture completed");
        assert_eq!(
            persistent_http_source_cache_files(directory.path()).len(),
            1
        );
        let stats = loader.script_loader.http_source_cache_stats();
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.not_modified, 1);
        assert_eq!(stats.errors, 0);
        assert!(stats.writes >= 1);
    }

    #[test]
    fn crossorigin_stylesheet_requires_final_cors_permission() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_dom::stylesheet::clear_rules();
        let document_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind CORS stylesheet document");
        let document_address = document_listener
            .local_addr()
            .expect("CORS stylesheet document address");
        let stylesheet_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind cross-origin stylesheet");
        let stylesheet_address = stylesheet_listener
            .local_addr()
            .expect("cross-origin stylesheet address");
        let document_server = thread::spawn(move || {
            let (mut document, _) = document_listener
                .accept()
                .expect("accept CORS CSS document");
            let _ = read_http_request(&mut document);
            let body = format!(
                "<html><head><link rel=\"stylesheet\" crossorigin=\"anonymous\" \
                 href=\"http://{stylesheet_address}/theme.css\"></head>\
                 <body><div class=\"cors-target\"></div></body></html>"
            );
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write CORS CSS document");
        });
        let stylesheet_server = thread::spawn(move || {
            let (mut stylesheet, _) = stylesheet_listener
                .accept()
                .expect("accept cross-origin CSS request");
            let request = read_http_request(&mut stylesheet).to_ascii_lowercase();
            assert!(request.contains(&format!("origin: http://{document_address}")));
            let body = ".cors-target { color: red; }";
            write!(
                stylesheet,
                "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write CSS without CORS permission");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{document_address}/index.html"))
            .unwrap();
        for _ in 0..5_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("CORS stylesheet document failed: {error}")
                }
                _ => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        document_server.join().expect("CORS CSS document completed");
        stylesheet_server
            .join()
            .expect("cross-origin CSS fixture completed");
        let document = crate::jsdom::document_value();
        let target = document.call_method("querySelector", vec![Value::string(".cors-target")]);
        let computed = crate::jsdom::window_value().call_method("getComputedStyle", vec![target]);
        assert_eq!(computed.get_property("color").to_js_string(), "#000000");
        assert_eq!(
            document
                .get_property("styleSheets")
                .get_property("length")
                .to_u32(),
            0
        );
        assert!(
            document
                .call_method("querySelector", vec![Value::string("link")])
                .get_property("sheet")
                .is_nullish()
        );
    }

    #[test]
    fn document_loader_follows_redirect_decodes_charset_and_resolves_relative_script() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind document loader fixture");
        let address = listener
            .local_addr()
            .expect("document loader fixture address");
        let server = thread::spawn(move || {
            let (mut redirect, _) = listener.accept().expect("accept navigation redirect");
            let request = read_http_request(&mut redirect);
            assert!(request.starts_with("GET /start "));
            write!(
                redirect,
                "HTTP/1.1 302 Found\r\nLocation: /final/index.html\r\nSet-Cookie: nav_redirect=yes; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write navigation redirect");

            let (mut document, _) = listener.accept().expect("accept final document");
            let request = read_http_request(&mut document);
            assert!(request.starts_with("GET /final/index.html "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("cookie: nav_redirect=yes")
            );
            let mut body =
                br#"<html><head><script src="app.js"></script></head><body><p id="price">Price "#
                    .to_vec();
            body.push(0x80);
            body.extend_from_slice(b"</p></body></html>");
            write!(
                document,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=windows-1252\r\nSet-Cookie: nav_page=ready; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write final document headers");
            document
                .write_all(&body)
                .expect("write final document body");

            let (mut script, _) = listener.accept().expect("accept relative document script");
            let request = read_http_request(&mut script);
            assert!(request.starts_with("GET /final/app.js "));
            let request = request.to_ascii_lowercase();
            assert!(request.contains("nav_redirect=yes"));
            assert!(request.contains("nav_page=ready"));
            let source = r#"document.body.setAttribute("data-document-loader", "script-ran");"#;
            write!(
                script,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .expect("write relative document script");
        });

        let mut loader = DocumentLoader::new(
            ScriptPolicy::default(),
            DocumentLoaderOptions {
                parse_chunk_bytes: 7,
                ..DocumentLoaderOptions::default()
            },
        );
        loader.navigate(&format!("http://{address}/start")).unwrap();
        for _ in 0..20_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("document navigation failed: {error}")
                }
                _ => thread::yield_now(),
            }
        }
        server.join().expect("document loader fixture completed");
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        assert!(loader.redirected());
        assert_eq!(
            loader.final_url(),
            Some(format!("http://{address}/final/index.html").as_str())
        );
        let document = crate::jsdom::document_value();
        assert_eq!(
            document
                .call_method("querySelector", vec![Value::string("#price")])
                .get_property("textContent")
                .to_js_string(),
            "Price €"
        );
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-document-loader")])
                .to_js_string(),
            "script-ran"
        );
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
        assert_eq!(
            crate::history::get_href(),
            format!("http://{address}/final/index.html")
        );
    }

    #[test]
    fn document_loader_parses_body_before_network_eof() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind streaming document fixture");
        let address = listener.local_addr().expect("streaming fixture address");
        let (early_tx, early_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = "<html><body><p id=early>available</p>";
        let last = "<p id=late>finished</p></body></html>";
        let content_length = first.len() + last.len();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept streaming navigation");
            let _ = read_http_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n{first}"
            )
            .expect("write first document chunk");
            stream.flush().expect("flush first document chunk");
            early_tx.send(()).expect("signal first document chunk");
            release_rx.recv().expect("release final document chunk");
            stream
                .write_all(last.as_bytes())
                .expect("write final document chunk");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/stream.html"))
            .unwrap();
        early_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("first chunk reached transport");
        let mut observed_before_eof = false;
        for _ in 0..10_000 {
            let progress = loader.poll();
            let early = crate::jsdom::document_value()
                .call_method("querySelector", vec![Value::string("#early")]);
            if !early.is_null() {
                observed_before_eof = true;
                assert_eq!(
                    early.get_property("textContent").to_js_string(),
                    "available"
                );
                assert_ne!(progress, DocumentLoadProgress::Complete);
                break;
            }
            thread::yield_now();
        }
        release_tx
            .send(())
            .expect("release final streaming document chunk");
        assert!(
            observed_before_eof,
            "the first response chunk should enter the DOM before network EOF"
        );
        server.join().expect("streaming document fixture completed");
        for _ in 0..10_000 {
            if loader.poll() == DocumentLoadProgress::Complete {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        assert_eq!(
            crate::jsdom::document_value()
                .call_method("querySelector", vec![Value::string("#late")])
                .get_property("textContent")
                .to_js_string(),
            "finished"
        );
    }

    #[test]
    fn document_loader_renders_error_page_for_unsupported_mime() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind MIME fixture");
        let address = listener.local_addr().expect("MIME fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept MIME request");
            let _ = read_http_request(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbin",
                )
                .expect("write MIME response");
        });
        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/download"))
            .unwrap();
        let mut failure = None;
        for _ in 0..20_000 {
            match loader.poll() {
                DocumentLoadProgress::Failed(error) => {
                    failure = Some(error);
                    break;
                }
                _ => thread::yield_now(),
            }
        }
        let failure = failure.expect("unsupported MIME navigation must fail");
        server.join().expect("MIME fixture completed");
        assert!(failure.contains("unsupported document MIME type"));
        assert!(
            crate::jsdom::document_value()
                .get_property("body")
                .get_property("textContent")
                .to_js_string()
                .contains("unsupported document MIME type")
        );
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("readyState")
                .to_js_string(),
            "complete"
        );
    }

    #[test]
    fn document_loader_renders_plain_text_without_executing_markup() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind plain-text fixture");
        let address = listener.local_addr().expect("plain-text fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept plain-text request");
            let _ = read_http_request(&mut stream);
            let body =
                r#"<script>document.body.setAttribute("data-plain-text", "executed");</script>"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write plain-text response");
        });
        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{address}/source.txt"))
            .unwrap();
        for _ in 0..20_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("plain-text navigation failed: {error}")
                }
                _ => thread::yield_now(),
            }
        }
        server.join().expect("plain-text fixture completed");
        let body = crate::jsdom::document_value().get_property("body");
        assert_eq!(
            body.call_method("getAttribute", vec![Value::string("data-plain-text")]),
            Value::Null
        );
        assert!(
            body.get_property("textContent")
                .to_js_string()
                .contains("<script>document.body.setAttribute")
        );
    }

    #[test]
    fn document_loader_navigation_discards_a_cancelled_stale_response() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let stale_listener = TcpListener::bind("127.0.0.1:0").expect("bind stale document");
        let stale_address = stale_listener.local_addr().expect("stale document address");
        let (stale_accepted_tx, stale_accepted_rx) = std::sync::mpsc::channel();
        let (release_stale_tx, release_stale_rx) = std::sync::mpsc::channel();
        let stale_server = thread::spawn(move || {
            let (mut stream, _) = stale_listener.accept().expect("accept stale document");
            let _ = read_http_request(&mut stream);
            stale_accepted_tx.send(()).expect("signal stale request");
            release_stale_rx.recv().expect("release stale response");
            let body = r#"<html><body><script>document.body.setAttribute("data-navigation", "stale");</script></body></html>"#;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        });

        let current_listener = TcpListener::bind("127.0.0.1:0").expect("bind current document");
        let current_address = current_listener
            .local_addr()
            .expect("current document address");
        let current_server = thread::spawn(move || {
            let (mut stream, _) = current_listener.accept().expect("accept current document");
            let _ = read_http_request(&mut stream);
            let body = r#"<html><body><script>document.body.setAttribute("data-navigation", "current");</script></body></html>"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write current document");
        });

        let mut loader =
            DocumentLoader::new(ScriptPolicy::default(), DocumentLoaderOptions::default());
        loader
            .navigate(&format!("http://{stale_address}/stale"))
            .unwrap();
        stale_accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("stale request accepted");
        loader
            .navigate(&format!("http://{current_address}/current"))
            .unwrap();
        release_stale_tx.send(()).expect("release stale response");
        for _ in 0..20_000 {
            match loader.poll() {
                DocumentLoadProgress::Complete => break,
                DocumentLoadProgress::Failed(error) => {
                    panic!("replacement navigation failed: {error}")
                }
                _ => thread::yield_now(),
            }
        }
        stale_server.join().expect("stale fixture completed");
        current_server.join().expect("current fixture completed");
        assert_eq!(loader.progress(), &DocumentLoadProgress::Complete);
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-navigation")])
                .to_js_string(),
            "current"
        );
        assert_eq!(
            loader.final_url(),
            Some(format!("http://{current_address}/current").as_str())
        );
    }

    #[test]
    fn document_charset_decoder_honors_bom_meta_and_windows_1252() {
        assert_eq!(
            decode_document_bytes(b"\xef\xbb\xbfhello", "").unwrap(),
            "hello"
        );
        assert_eq!(
            decode_document_bytes(
                r#"<meta charset="utf-8"><p>你好</p>"#.as_bytes(),
                "text/html"
            )
            .unwrap(),
            r#"<meta charset="utf-8"><p>你好</p>"#
        );
        assert_eq!(
            decode_document_bytes(b"Price \x80", "text/html").unwrap(),
            "Price €"
        );
        assert!(
            decode_document_bytes(b"body", "text/html; charset=made-up")
                .unwrap_err()
                .to_string()
                .contains("unsupported document charset")
        );

        let (mut utf8, bom) = DocumentByteDecoder::detect(&[0xef, 0xbb, 0xbf], "text/html", false)
            .unwrap()
            .unwrap();
        assert_eq!(bom, 3);
        assert_eq!(utf8.decode(&[0xe4, 0xbd], false).unwrap(), "");
        assert_eq!(utf8.decode(&[0xa0], true).unwrap(), "你");

        let (mut utf16, bom) = DocumentByteDecoder::detect(&[0xff, 0xfe], "text/html", false)
            .unwrap()
            .unwrap();
        assert_eq!(bom, 2);
        assert_eq!(utf16.decode(&[0x3d, 0xd8], false).unwrap(), "");
        assert_eq!(utf16.decode(&[0x00, 0xde], true).unwrap(), "\u{1f600}");
    }

    #[test]
    fn parser_deferred_and_async_scripts_gate_document_readiness_separately() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        fn controlled_script(
            source: &'static str,
        ) -> (String, std::sync::mpsc::Sender<()>, thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind lifecycle fixture");
            let address = listener.local_addr().expect("lifecycle fixture address");
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept lifecycle request");
                let _ = read_http_request(&mut stream);
                release_rx.recv().expect("release lifecycle response");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    source.len(),
                    source
                )
                .expect("write lifecycle response");
            });
            (format!("http://{address}/script.js"), release_tx, handle)
        }

        let (defer_url, release_defer, defer_server) =
            controlled_script(r#"document.body.setAttribute("data-defer-lifecycle", "done");"#);
        let (async_url, release_async, async_server) =
            controlled_script(r#"document.body.setAttribute("data-async-lifecycle", "done");"#);
        let (dynamic_url, release_dynamic, dynamic_server) =
            controlled_script(r#"document.body.setAttribute("data-dynamic-lifecycle", "done");"#);
        let document = crate::jsdom::document_value();
        let window = crate::jsdom::window_value();
        let ready_states = Rc::new(RefCell::new(Vec::<String>::new()));
        let states = Rc::clone(&ready_states);
        let observed_document = document.clone();
        document.call_method(
            "addEventListener",
            vec![
                Value::string("readystatechange"),
                Value::function(move |_, _| {
                    states
                        .borrow_mut()
                        .push(observed_document.get_property("readyState").to_js_string());
                    Value::Undefined
                }),
            ],
        );
        let dom_content_loaded = Rc::new(Cell::new(0_u32));
        let dcl_count = Rc::clone(&dom_content_loaded);
        let dcl_document = document.clone();
        document.call_method(
            "addEventListener",
            vec![
                Value::string("DOMContentLoaded"),
                Value::function(move |_, _| {
                    dcl_count.set(dcl_count.get() + 1);
                    let script =
                        dcl_document.call_method("createElement", vec![Value::string("script")]);
                    script.call_method(
                        "setAttribute",
                        vec![Value::string("src"), Value::string(&dynamic_url)],
                    );
                    dcl_document
                        .get_property("head")
                        .call_method("appendChild", vec![script]);
                    Value::Undefined
                }),
            ],
        );
        let loads = Rc::new(Cell::new(0_u32));
        let load_count = Rc::clone(&loads);
        window.call_method(
            "addEventListener",
            vec![
                Value::string("load"),
                Value::function(move |_, _| {
                    load_count.set(load_count.get() + 1);
                    Value::Undefined
                }),
            ],
        );

        let deferred = document.call_method("createElement", vec![Value::string("script")]);
        deferred.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&defer_url)],
        );
        deferred.call_method(
            "setAttribute",
            vec![Value::string("defer"), Value::string("")],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![deferred]);
        let asynchronous = document.call_method("createElement", vec![Value::string("script")]);
        asynchronous.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&async_url)],
        );
        asynchronous.call_method(
            "setAttribute",
            vec![Value::string("async"), Value::string("")],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![asynchronous]);

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .begin_document_parse("http://127.0.0.1/document.html")
            .unwrap();
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "loading"
        );
        assert_eq!(
            loader
                .execute_pending_document_scripts("http://127.0.0.1/document.html")
                .unwrap(),
            2
        );
        release_defer.send(()).expect("release deferred response");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !loader
                .inner
                .ready_deferred_classic_scripts
                .borrow()
                .is_empty()
            {
                break;
            }
            thread::yield_now();
        }
        defer_server.join().expect("deferred fixture completed");
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "loading"
        );
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-defer-lifecycle")]),
            Value::Null
        );
        assert_eq!(dom_content_loaded.get(), 0);

        loader.finish_document_parse();
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "interactive"
        );
        assert_eq!(dom_content_loaded.get(), 1);
        assert_eq!(loads.get(), 0);

        release_async.send(()).expect("release async response");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-async-lifecycle")])
                .to_js_string()
                == "done"
            {
                break;
            }
            thread::yield_now();
        }
        async_server.join().expect("async fixture completed");
        assert_eq!(loads.get(), 0);
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "interactive"
        );

        release_dynamic
            .send(())
            .expect("release dynamically inserted response");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if loads.get() == 1 {
                break;
            }
            thread::yield_now();
        }
        dynamic_server.join().expect("dynamic fixture completed");
        assert_eq!(loads.get(), 1);
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
        assert_eq!(
            ready_states.borrow().as_slice(),
            &["loading", "interactive", "complete"]
        );
    }

    #[test]
    fn parser_module_top_level_await_delays_dom_content_loaded_and_load() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let document = crate::jsdom::document_value();
        let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
        let executor_slot = Rc::clone(&resolve_slot);
        let gate = w3cos_core::promise::new(vec![Value::function(move |_, arguments| {
            *executor_slot.borrow_mut() = arguments.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        })]);
        crate::jsdom::window_value().set_property("parserModuleGate", gate);

        let dom_content_loaded = Rc::new(Cell::new(0_u32));
        let dcl_count = Rc::clone(&dom_content_loaded);
        document.call_method(
            "addEventListener",
            vec![
                Value::string("DOMContentLoaded"),
                Value::function(move |_, _| {
                    dcl_count.set(dcl_count.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        let loads = Rc::new(Cell::new(0_u32));
        let load_count = Rc::clone(&loads);
        crate::jsdom::window_value().call_method(
            "addEventListener",
            vec![
                Value::string("load"),
                Value::function(move |_, _| {
                    load_count.set(load_count.get() + 1);
                    Value::Undefined
                }),
            ],
        );

        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.set_property(
            "textContent",
            Value::string(
                r#"
                    document.body.setAttribute("data-parser-module-started", "yes");
                    await parserModuleGate;
                    document.body.setAttribute("data-parser-module", "settled");
                "#,
            ),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .begin_document_parse("https://example.test/document.html")
            .unwrap();
        assert_eq!(
            loader
                .execute_pending_document_scripts("https://example.test/document.html")
                .unwrap(),
            1
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "loading"
        );
        assert_eq!(
            document.get_property("body").call_method(
                "getAttribute",
                vec![Value::string("data-parser-module-started")]
            ),
            Value::Null
        );
        loader.finish_document_parse();
        crate::jsdom::drain_microtasks();
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "interactive"
        );
        assert_eq!(dom_content_loaded.get(), 0);
        assert_eq!(loads.get(), 0);
        assert_eq!(
            document
                .get_property("body")
                .call_method(
                    "getAttribute",
                    vec![Value::string("data-parser-module-started")]
                )
                .to_js_string(),
            "yes"
        );

        resolve_slot
            .borrow()
            .call(Value::Undefined, vec![Value::string("ready")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(dom_content_loaded.get(), 1);
        assert_eq!(loads.get(), 1);
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-parser-module")])
                .to_js_string(),
            "settled"
        );
    }

    #[test]
    fn parser_modules_prefetch_during_parsing_and_execute_in_document_order() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind parser module fixture");
        let address = listener.local_addr().expect("parser module address");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept parser module");
            let _ = read_http_request(&mut stream);
            accepted_tx.send(()).expect("signal module prefetch");
            release_rx.recv().expect("release parser module response");
            let source = r#"recordParserModule("external");"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .expect("write parser module response");
        });

        let document = crate::jsdom::document_value();
        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "recordParserModule",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        let dom_content_loaded = Rc::new(Cell::new(0_u32));
        let dcl_count = Rc::clone(&dom_content_loaded);
        document.call_method(
            "addEventListener",
            vec![
                Value::string("DOMContentLoaded"),
                Value::function(move |_, _| {
                    dcl_count.set(dcl_count.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        let loads = Rc::new(Cell::new(0_u32));
        let load_count = Rc::clone(&loads);
        crate::jsdom::window_value().call_method(
            "addEventListener",
            vec![
                Value::string("load"),
                Value::function(move |_, _| {
                    load_count.set(load_count.get() + 1);
                    Value::Undefined
                }),
            ],
        );

        let external = document.call_method("createElement", vec![Value::string("script")]);
        external.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        let external_url = format!("http://{address}/first.js");
        external.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&external_url)],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![external]);
        let inline = document.call_method("createElement", vec![Value::string("script")]);
        inline.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        inline.set_property(
            "textContent",
            Value::string(r#"recordParserModule("inline");"#),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![inline]);

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .begin_document_parse(&format!("http://{address}/index.html"))
            .unwrap();
        assert_eq!(
            loader
                .execute_pending_document_scripts(&format!("http://{address}/index.html"))
                .unwrap(),
            2
        );
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("parser module graph must start fetching before EOF");
        assert!(observed.borrow().is_empty());

        loader.finish_document_parse();
        crate::jsdom::drain_microtasks();
        assert!(observed.borrow().is_empty());
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "interactive"
        );
        assert_eq!(dom_content_loaded.get(), 0);
        assert_eq!(loads.get(), 0);

        release_tx.send(()).expect("release parser module");
        server.join().expect("parser module fixture completed");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if loads.get() == 1 {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(observed.borrow().as_slice(), &["external", "inline"]);
        assert_eq!(dom_content_loaded.get(), 1);
        assert_eq!(loads.get(), 1);
        assert_eq!(
            document.get_property("readyState").to_js_string(),
            "complete"
        );
    }

    #[test]
    fn removing_one_deduplicated_ordered_classic_script_keeps_the_other_subscriber_live() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind removal fixture");
        let address = listener.local_addr().expect("removal fixture address");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept shared script request");
            let _ = read_http_request(&mut stream);
            accepted_tx.send(()).expect("signal accepted request");
            release_rx.recv().expect("release shared script response");
            let source = r#"record("shared");"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .expect("write shared script response");
        });

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "record",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        let first_loads = Rc::new(Cell::new(0_u32));
        let second_loads = Rc::new(Cell::new(0_u32));
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let head = document.get_property("head");
        let container = document.call_method("createElement", vec![Value::string("div")]);
        let mut scripts = Vec::new();
        for loads in [Rc::clone(&first_loads), Rc::clone(&second_loads)] {
            let script = document.call_method("createElement", vec![Value::string("script")]);
            script.call_method(
                "setAttribute",
                vec![Value::string("src"), Value::string("/shared.js")],
            );
            script.set_property("async", Value::Bool(false));
            script.set_property(
                "onload",
                Value::function(move |_, _| {
                    loads.set(loads.get() + 1);
                    Value::Undefined
                }),
            );
            container.call_method("appendChild", vec![script.clone()]);
            scripts.push(script);
        }
        head.call_method("appendChild", vec![container.clone()]);
        crate::jsdom::drain_microtasks();
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("shared request started");

        container.call_method("removeChild", vec![scripts[0].clone()]);
        release_tx.send(()).expect("release response");
        server.join().expect("removal fixture completed");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }

        assert!(!has_pending_script_fetches());
        assert_eq!(observed.borrow().as_slice(), &["shared"]);
        assert_eq!(first_loads.get(), 0);
        assert_eq!(second_loads.get(), 1);

        container.call_method("appendChild", vec![scripts.remove(0)]);
        crate::jsdom::drain_microtasks();
        assert_eq!(observed.borrow().as_slice(), &["shared"]);
        assert_eq!(first_loads.get(), 0);
    }

    #[test]
    fn removing_inline_module_before_settlement_prevents_evaluation_and_callbacks() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let observed = Rc::new(Cell::new(0_u32));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "recordRemovedModule",
            Value::function(move |_, _| {
                callback_observed.set(callback_observed.get() + 1);
                Value::Undefined
            }),
        );
        let loads = Rc::new(Cell::new(0_u32));
        let errors = Rc::new(Cell::new(0_u32));
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();
        let document = crate::jsdom::document_value();
        let head = document.get_property("head");
        let container = document.call_method("createElement", vec![Value::string("div")]);
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.set_property(
            "textContent",
            Value::string("recordRemovedModule(); export const value = 1;"),
        );
        let load_counter = Rc::clone(&loads);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                load_counter.set(load_counter.get() + 1);
                Value::Undefined
            }),
        );
        let error_counter = Rc::clone(&errors);
        script.set_property(
            "onerror",
            Value::function(move |_, _| {
                error_counter.set(error_counter.get() + 1);
                Value::Undefined
            }),
        );
        container.call_method("appendChild", vec![script.clone()]);
        head.call_method("appendChild", vec![container.clone()]);
        assert_eq!(
            loader
                .execute_pending_document_scripts("https://example.test/index.html")
                .unwrap(),
            1
        );

        head.call_method("removeChild", vec![container.clone()]);
        crate::jsdom::drain_microtasks();
        assert_eq!(observed.get(), 0);
        assert_eq!(loads.get(), 0);
        assert_eq!(errors.get(), 0);

        head.call_method("appendChild", vec![container]);
        crate::jsdom::drain_microtasks();
        assert_eq!(observed.get(), 0);
        assert_eq!(loads.get(), 0);
        assert_eq!(errors.get(), 0);
    }

    #[test]
    fn dynamically_inserted_classic_scripts_execute_in_fetch_completion_order() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                requests.push((stream, request.contains("GET /slow.js ")));
            }
            requests.sort_by_key(|(_, is_slow)| *is_slow);
            for (mut stream, is_slow) in requests {
                if is_slow {
                    thread::sleep(std::time::Duration::from_millis(25));
                }
                let source = if is_slow {
                    r#"record("slow");"#
                } else {
                    r#"record("fast");"#
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    source.len(),
                    source
                )
                .unwrap();
            }
        });

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "record",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        for src in ["/slow.js", "/fast.js"] {
            let script = document.call_method("createElement", vec![Value::string("script")]);
            script.call_method(
                "setAttribute",
                vec![Value::string("src"), Value::string(src)],
            );
            document
                .get_property("head")
                .call_method("appendChild", vec![script]);
        }
        crate::jsdom::drain_microtasks();
        server.join().unwrap();
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_module_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert!(!has_pending_module_fetches());
        assert_eq!(observed.borrow().as_slice(), &["fast", "slow"]);
    }

    #[test]
    fn document_navigation_discards_pending_classic_script_results() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requested_tx, requested_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            requested_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let source = r#"document.body.setAttribute("data-stale-script", "ran");"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .unwrap();
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let events = Rc::new(Cell::new(0));
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string("/stale.js")],
        );
        for property in ["onload", "onerror"] {
            let observed_events = Rc::clone(&events);
            script.set_property(
                property,
                Value::function(move |_, _| {
                    observed_events.set(observed_events.get() + 1);
                    Value::Undefined
                }),
            );
        }
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);
        crate::jsdom::drain_microtasks();
        requested_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();

        reset_document_loader();
        assert!(!has_pending_script_fetches());
        release_tx.send(()).unwrap();
        server.join().unwrap();
        crate::jsdom::drain_microtasks();
        assert_eq!(events.get(), 0);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-stale-script")])
                .to_js_string(),
            "null"
        );
    }

    #[test]
    fn document_navigation_rejects_a_pending_module_graph() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requested_tx, requested_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            requested_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let source = r#"export const stale = true;"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .unwrap();
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let evaluation =
            loader.load_and_execute_module_async(&format!("http://{address}/stale-module.js"));
        requested_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        reset_document_loader();
        crate::jsdom::drain_microtasks();
        let Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) =
            w3cos_core::promise::status(&evaluation)
        else {
            panic!("navigation must reject the pending module graph");
        };
        assert!(
            reason
                .to_js_string()
                .contains("document navigation cancelled")
        );
        assert!(!has_pending_script_fetches());
        release_tx.send(()).unwrap();
        server.join().unwrap();
        crate::jsdom::drain_microtasks();
    }

    #[test]
    fn external_jsonp_script_calls_a_registered_window_callback() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let source = r#"mapCallback({ status: "ready", tiles: [1, 2] });"#;
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .unwrap();
        });

        let payload = Rc::new(RefCell::new(Value::Undefined));
        let callback_payload = Rc::clone(&payload);
        crate::jsdom::window_value().set_property(
            "mapCallback",
            Value::function(move |_, arguments| {
                *callback_payload.borrow_mut() = arguments[0].clone();
                Value::Undefined
            }),
        );

        ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute(&format!("http://{address}/jsonp?callback=mapCallback"))
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            payload.borrow().get_property("status"),
            Value::string("ready")
        );
        assert_eq!(
            payload.borrow().get_property("tiles").get_property("1"),
            Value::Number(2.0)
        );
    }

    #[test]
    fn esm_graph_uses_live_bindings_and_returns_a_live_namespace() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/modules/dependency.js",
                r#"
                    export let marker = "before";
                    export const update = () => {
                        marker = "after";
                    };
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        let namespace = loader
            .execute_module_source(
                r#"
                    import { marker, update } from "./dependency.js";
                    callback(marker);
                    update();
                    callback(marker);
                    export { marker as observed };
                "#,
                "https://example.test/modules/main.js",
            )
            .unwrap();

        assert_eq!(observed.borrow().as_slice(), &["before", "after"]);
        assert_eq!(namespace.get_property("observed").to_js_string(), "after");
    }

    #[test]
    fn mixed_aot_and_bytecode_modules_share_the_core_live_binding_registry() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_core::module_registry::clear();

        let aot_value = Rc::new(RefCell::new(Value::Number(1.0)));
        let getter_value = Rc::clone(&aot_value);
        let aot_evaluations = Rc::new(Cell::new(0));
        let evaluator_count = Rc::clone(&aot_evaluations);
        w3cos_core::module_registry::register(
            "build:///generated/aot.js",
            HashMap::from([
                (
                    "value".into(),
                    w3cos_core::module_registry::ExportBinding::new(
                        Value::function(move |_, _| getter_value.borrow().clone()),
                        Value::Undefined,
                    ),
                ),
                (
                    "default".into(),
                    w3cos_core::module_registry::ExportBinding::new(
                        Value::function(|_, _| Value::string("not-star-forwarded")),
                        Value::Undefined,
                    ),
                ),
            ]),
            Some(Value::function(move |_, _| {
                evaluator_count.set(evaluator_count.get() + 1);
                let back_edge = w3cos_core::module_registry::evaluate(
                    "https://example.test/modules/bytecode.js",
                )
                .expect("bytecode half of the mixed SCC must already be registered");
                assert!(matches!(
                    w3cos_core::promise::status(&back_edge),
                    Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
                ));
                Value::Undefined
            })),
        );
        w3cos_core::module_registry::register_alias(
            "https://example.test/modules/aot.js",
            "build:///generated/aot.js",
        );

        let loader = ScriptLoader::new(ScriptPolicy::default());
        let namespace = loader
            .execute_module_source(
                r#"
                    import { value } from "./aot.js";
                    export * from "./aot.js";
                    export function read() {
                        return value;
                    }
                "#,
                "https://example.test/modules/bytecode.js",
            )
            .unwrap();
        let read = namespace.get_property("read");
        assert_eq!(namespace.get_property("value"), Value::Number(1.0));
        assert!(namespace.get_property("default").is_undefined());
        assert_eq!(read.call(Value::Undefined, Vec::new()), Value::Number(1.0));
        *aot_value.borrow_mut() = Value::Number(2.0);
        assert_eq!(namespace.get_property("value"), Value::Number(2.0));
        assert_eq!(read.call(Value::Undefined, Vec::new()), Value::Number(2.0));

        // This is the ABI used by generated AOT code: resolving the bytecode
        // module through Core returns the same callable live export.
        let bytecode_namespace =
            w3cos_core::module_registry::namespace("https://example.test/modules/bytecode.js")
                .unwrap();
        assert_eq!(
            bytecode_namespace
                .get_property("read")
                .call(Value::Undefined, Vec::new()),
            Value::Number(2.0)
        );
        let cached_aot =
            w3cos_core::module_registry::evaluate("https://example.test/modules/aot.js").unwrap();
        assert!(matches!(
            w3cos_core::promise::status(&cached_aot),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(aot_evaluations.get(), 1);
        drop(loader);
        w3cos_core::module_registry::clear();
    }

    #[test]
    fn esm_module_cache_evaluates_each_url_once() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/shared.js",
                r#"
                    callback("shared");
                    export const value = 2;
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        loader
            .register_module_source(
                "https://example.test/main.js",
                r#"
                    import { value } from "./shared.js";
                    import { value as sameValue } from "./shared.js";
                    callback(value + sameValue);
                    export const result = value + sameValue;
                "#,
            )
            .unwrap();

        let first = loader
            .load_and_execute_module("https://example.test/main.js")
            .unwrap();
        let second = loader
            .load_and_execute_module("https://example.test/main.js")
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &["shared", "4"]);
        assert_eq!(first.get_property("result"), Value::Number(4.0));
        assert_eq!(second.get_property("result"), Value::Number(4.0));
    }

    #[test]
    fn compiled_source_cache_reuses_module_lowering_between_graph_phases() {
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/dependency.js",
                "export const value = 41;",
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/main.js",
                r#"import { value } from "./dependency.js"; export const answer = value + 1;"#,
            )
            .unwrap();

        let namespace = loader
            .load_and_execute_module("https://example.test/main.js")
            .unwrap();

        assert_eq!(namespace.get_property("answer"), Value::Number(42.0));
        let stats = loader.compiled_source_cache_stats();
        assert_eq!(stats.entries, 2);
        assert_eq!((stats.hits, stats.misses, stats.evictions), (2, 2, 0));
        assert!(stats.resident_bytes > 0);
    }

    #[test]
    fn dynamic_script_policy_enforces_the_vm_wall_clock_limit() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy {
            limits: Limits {
                max_wall_time: Some(std::time::Duration::ZERO),
                ..Limits::default()
            },
            ..ScriptPolicy::default()
        });

        let error = loader
            .execute_source("document.body.dataset.ready = 'yes';", "inline:deadline")
            .expect_err("a zero wall-clock budget must stop page script execution");
        assert!(error.to_string().contains("WallClockLimitExceeded"));
    }

    #[test]
    fn compiled_source_cache_survives_navigation_but_not_source_changes() {
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let url = "https://example.test/versioned.js";
        loader
            .register_module_source(url, "export const version = 1;")
            .unwrap();
        let first = loader.load_and_execute_module(url).unwrap();
        assert_eq!(first.get_property("version"), Value::Number(1.0));
        let stats = loader.compiled_source_cache_stats();
        assert_eq!((stats.entries, stats.hits, stats.misses), (1, 1, 1));

        loader.cancel_for_navigation();
        loader
            .register_module_source(url, "export const version = 1;")
            .unwrap();
        let unchanged = loader.load_and_execute_module(url).unwrap();
        assert_eq!(unchanged.get_property("version"), Value::Number(1.0));
        let stats = loader.compiled_source_cache_stats();
        assert_eq!((stats.entries, stats.hits, stats.misses), (1, 3, 1));

        loader.cancel_for_navigation();
        loader
            .register_module_source(url, "export const version = 2;")
            .unwrap();
        let changed = loader.load_and_execute_module(url).unwrap();
        assert_eq!(changed.get_property("version"), Value::Number(2.0));
        let stats = loader.compiled_source_cache_stats();
        assert_eq!((stats.entries, stats.hits, stats.misses), (2, 4, 2));
        assert_eq!(stats.evictions, 0);
    }

    #[test]
    fn compiled_source_cache_separates_classic_and_module_compile_modes() {
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let source = "const value = 42;";
        let url = "https://example.test/shared-source.js";

        loader.execute_source(source, url).unwrap();
        loader.execute_source(source, url).unwrap();
        loader.execute_module_source(source, url).unwrap();

        let stats = loader.compiled_source_cache_stats();
        assert_eq!((stats.entries, stats.hits, stats.misses), (2, 2, 2));
    }

    #[test]
    fn compiled_source_cache_evicts_the_least_recently_used_entry() {
        let loader = ScriptLoader::new(ScriptPolicy {
            max_compiled_cache_entries: 2,
            max_compiled_cache_bytes: usize::MAX,
            ..ScriptPolicy::default()
        });

        loader.execute_source("const a = 1;", "inline:a").unwrap();
        loader.execute_source("const b = 2;", "inline:b").unwrap();
        loader.execute_source("const a = 1;", "inline:a").unwrap();
        loader.execute_source("const c = 3;", "inline:c").unwrap();
        let stats = loader.compiled_source_cache_stats();
        assert_eq!(
            (stats.entries, stats.hits, stats.misses, stats.evictions),
            (2, 1, 3, 1)
        );

        loader.execute_source("const a = 1;", "inline:a").unwrap();
        loader.execute_source("const b = 2;", "inline:b").unwrap();
        let stats = loader.compiled_source_cache_stats();
        assert_eq!(
            (stats.entries, stats.hits, stats.misses, stats.evictions),
            (2, 2, 4, 2)
        );
    }

    #[test]
    fn compiled_source_cache_honors_zero_and_byte_budgets() {
        let disabled = ScriptLoader::new(ScriptPolicy {
            max_compiled_cache_entries: 0,
            ..ScriptPolicy::default()
        });
        disabled
            .execute_source("const answer = 42;", "inline:disabled")
            .unwrap();
        disabled
            .execute_source("const answer = 42;", "inline:disabled")
            .unwrap();
        assert_eq!(
            disabled.compiled_source_cache_stats(),
            CompiledSourceCacheStats {
                misses: 2,
                ..CompiledSourceCacheStats::default()
            }
        );

        let too_small = ScriptLoader::new(ScriptPolicy {
            max_compiled_cache_bytes: 1,
            ..ScriptPolicy::default()
        });
        too_small
            .execute_source("const answer = 42;", "inline:too-small")
            .unwrap();
        let stats = too_small.compiled_source_cache_stats();
        assert_eq!(
            (stats.entries, stats.resident_bytes, stats.misses),
            (0, 0, 1)
        );

        let calibration = ScriptLoader::new(ScriptPolicy::default());
        calibration
            .execute_source("const a = 1;", "inline:a")
            .unwrap();
        let one_entry_bytes = calibration.compiled_source_cache_stats().resident_bytes;
        let byte_bounded = ScriptLoader::new(ScriptPolicy {
            max_compiled_cache_bytes: one_entry_bytes,
            ..ScriptPolicy::default()
        });
        byte_bounded
            .execute_source("const a = 1;", "inline:a")
            .unwrap();
        byte_bounded
            .execute_source("const b = 2;", "inline:b")
            .unwrap();
        let stats = byte_bounded.compiled_source_cache_stats();
        assert_eq!((stats.entries, stats.evictions), (1, 1));
        assert!(stats.resident_bytes <= one_entry_bytes);
        byte_bounded
            .execute_source("const b = 2;", "inline:b")
            .unwrap();
        assert_eq!(byte_bounded.compiled_source_cache_stats().hits, 1);
    }

    #[test]
    fn persistent_compiled_cache_reuses_validated_w3ir_across_loaders() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let directory = tempfile::tempdir().unwrap();
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };
        let source = r#"document.body.setAttribute("data-persistent-w3ir", "ready");"#;
        let url = "https://example.test/persistent-classic.js";

        let first = ScriptLoader::new(policy.clone());
        first.execute_source(source, url).unwrap();
        let first_stats = first.compiled_source_cache_stats();
        assert_eq!(
            (
                first_stats.persistent_hits,
                first_stats.persistent_misses,
                first_stats.persistent_writes,
                first_stats.persistent_errors,
            ),
            (0, 1, 1, 0)
        );
        assert_eq!(persistent_cache_files(directory.path()).len(), 1);
        drop(first);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let second = ScriptLoader::new(policy);
        second.execute_source(source, url).unwrap();
        let second_stats = second.compiled_source_cache_stats();
        assert_eq!(
            (
                second_stats.hits,
                second_stats.misses,
                second_stats.persistent_hits,
                second_stats.persistent_writes,
            ),
            (0, 1, 1, 0)
        );
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-persistent-w3ir")])
                .to_js_string(),
            "ready"
        );
    }

    #[test]
    fn persistent_compiled_cache_rejects_corruption_and_replaces_it() {
        let directory = tempfile::tempdir().unwrap();
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };
        let source = "const answer = 42;";
        let url = "https://example.test/corrupt.js";
        ScriptLoader::new(policy.clone())
            .execute_source(source, url)
            .unwrap();
        let artifact = persistent_cache_files(directory.path())
            .into_iter()
            .next()
            .expect("persistent artifact");
        std::fs::write(&artifact, b"{not-valid-json").unwrap();

        let repaired = ScriptLoader::new(policy.clone());
        repaired.execute_source(source, url).unwrap();
        let stats = repaired.compiled_source_cache_stats();
        assert_eq!(
            (
                stats.persistent_hits,
                stats.persistent_misses,
                stats.persistent_writes,
                stats.persistent_errors,
            ),
            (0, 1, 1, 1)
        );

        let verified = ScriptLoader::new(policy);
        verified.execute_source(source, url).unwrap();
        let stats = verified.compiled_source_cache_stats();
        assert_eq!((stats.persistent_hits, stats.persistent_errors), (1, 0));
    }

    #[test]
    fn persistent_compiled_cache_rejects_structurally_invalid_w3ir() {
        let directory = tempfile::tempdir().unwrap();
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };
        let source = "const answer = 42;";
        let url = "https://example.test/invalid-w3ir.js";
        ScriptLoader::new(policy.clone())
            .execute_source(source, url)
            .unwrap();
        let artifact_path = persistent_cache_files(directory.path())
            .into_iter()
            .next()
            .expect("persistent artifact");
        let mut artifact: PersistentCompiledSource =
            serde_json::from_slice(&std::fs::read(&artifact_path).unwrap()).unwrap();
        artifact.module.entry = w3cos_ir::FunctionId(u32::MAX);
        std::fs::write(&artifact_path, serde_json::to_vec(&artifact).unwrap()).unwrap();

        let repaired = ScriptLoader::new(policy.clone());
        repaired.execute_source(source, url).unwrap();
        let stats = repaired.compiled_source_cache_stats();
        assert_eq!(
            (
                stats.persistent_hits,
                stats.persistent_misses,
                stats.persistent_writes,
                stats.persistent_errors,
            ),
            (0, 1, 1, 1)
        );

        let verified = ScriptLoader::new(policy);
        verified.execute_source(source, url).unwrap();
        assert_eq!(verified.compiled_source_cache_stats().persistent_hits, 1);
    }

    #[test]
    fn persistent_compiled_cache_isolates_content_and_compile_mode() {
        let directory = tempfile::tempdir().unwrap();
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };
        let url = "https://example.test/versioned-persistent.js";
        let first_source = "export const version = 1;";
        let second_source = "export const version = 2;";
        ScriptLoader::new(policy.clone())
            .execute_module_source(first_source, url)
            .unwrap();

        let changed = ScriptLoader::new(policy.clone());
        let namespace = changed.execute_module_source(second_source, url).unwrap();
        assert_eq!(namespace.get_property("version"), Value::Number(2.0));
        let stats = changed.compiled_source_cache_stats();
        assert_eq!(
            (
                stats.persistent_hits,
                stats.persistent_misses,
                stats.persistent_writes,
                stats.persistent_errors,
            ),
            (0, 1, 1, 0)
        );

        let module_hit = ScriptLoader::new(policy.clone());
        let namespace = module_hit
            .execute_module_source(second_source, url)
            .unwrap();
        assert_eq!(namespace.get_property("version"), Value::Number(2.0));
        assert_eq!(module_hit.compiled_source_cache_stats().persistent_hits, 1);

        ScriptLoader::new(policy)
            .execute_source("const version = 2;", url)
            .unwrap();
        assert_eq!(persistent_cache_files(directory.path()).len(), 2);
    }

    #[test]
    fn persistent_compiled_cache_prunes_to_the_configured_entry_budget() {
        let directory = tempfile::tempdir().unwrap();
        let policy = ScriptPolicy {
            max_compiled_cache_entries: 2,
            max_compiled_cache_bytes: usize::MAX,
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };
        let loader = ScriptLoader::new(policy);
        loader.execute_source("const a = 1;", "inline:a").unwrap();
        loader.execute_source("const b = 2;", "inline:b").unwrap();
        loader.execute_source("const c = 3;", "inline:c").unwrap();

        assert_eq!(persistent_cache_files(directory.path()).len(), 2);
        let stats = loader.compiled_source_cache_stats();
        assert_eq!(
            (stats.persistent_writes, stats.persistent_evictions),
            (3, 1)
        );
    }

    #[test]
    fn persistent_http_cache_honors_no_store_and_refuses_unkeyed_vary() {
        let directory = tempfile::tempdir().unwrap();
        let loader = ScriptLoader::new(ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        });
        let fetch_mode = ScriptFetchMode::ClassicScript(ClassicScriptFetchMode::NoCors);
        let response = |url: &str, policy_header: (&str, &str)| crate::fetch::FetchTextResponse {
            status: 200,
            ok: true,
            status_text: "OK".to_string(),
            headers: HashMap::from([
                ("etag".to_string(), "\"cache-v1\"".to_string()),
                (policy_header.0.to_string(), policy_header.1.to_string()),
            ]),
            url: url.to_string(),
            redirected: false,
            set_cookies: Vec::new(),
            body: "globalThis.cached = true;".to_string(),
        };

        let url = "https://example.test/cache-policy.js";
        loader.store_persistent_http_source(
            url,
            fetch_mode,
            &response(url, ("cache-control", "public, max-age=60")),
        );
        assert_eq!(
            persistent_http_source_cache_files(directory.path()).len(),
            1
        );

        loader.store_persistent_http_source(
            url,
            fetch_mode,
            &response(url, ("cache-control", "private, NO-STORE")),
        );
        assert!(persistent_http_source_cache_files(directory.path()).is_empty());

        let varied_url = "https://example.test/varied.js";
        loader.store_persistent_http_source(
            varied_url,
            fetch_mode,
            &response(varied_url, ("vary", "Origin, Cookie")),
        );
        assert!(persistent_http_source_cache_files(directory.path()).is_empty());
    }

    #[test]
    fn persistent_http_cache_revalidates_classic_script_with_etag() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let directory = tempfile::tempdir().unwrap();
        let source = r#"document.body.setAttribute("data-http-cache", "etag");"#;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ETag fixture");
        let address = listener.local_addr().expect("ETag fixture address");
        let server = thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept ETag request");
                let request = read_http_request(&mut stream);
                let request_lower = request.to_ascii_lowercase();
                if request_index == 0 {
                    assert!(!request_lower.contains("if-none-match:"));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nX-Content-Type-Options: nosniff\r\nETag: \"classic-v1\"\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\nSet-Cookie: private-token=must-not-persist\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        source.len(),
                        source
                    )
                    .expect("write initial ETag response");
                } else {
                    assert!(request_lower.contains("if-none-match: \"classic-v1\""));
                    assert!(
                        request_lower.contains("if-modified-since: wed, 21 oct 2015 07:28:00 gmt")
                    );
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"classic-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write ETag 304 response");
                }
            }
        });
        let url = format!("http://{address}/classic.js");
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };

        ScriptLoader::new(policy.clone())
            .load_and_execute(&url)
            .unwrap();
        let persisted_source =
            std::fs::read_to_string(&persistent_http_source_cache_files(directory.path())[0])
                .unwrap();
        assert!(!persisted_source.contains("must-not-persist"));
        assert!(
            persisted_source
                .to_ascii_lowercase()
                .contains("x-content-type-options")
        );
        crate::jsdom::document_value()
            .get_property("body")
            .call_method(
                "setAttribute",
                vec![Value::string("data-http-cache"), Value::string("cleared")],
            );
        let second = ScriptLoader::new(policy);
        second.load_and_execute(&url).unwrap();
        server.join().expect("ETag fixture completed");

        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-http-cache")])
                .to_js_string(),
            "etag"
        );
        assert_eq!(
            second.http_source_cache_stats(),
            HttpSourceCacheStats {
                candidates: 1,
                not_modified: 1,
                writes: 1,
                ..HttpSourceCacheStats::default()
            }
        );
        assert_eq!(second.compiled_source_cache_stats().persistent_hits, 1);
    }

    #[test]
    fn corrupt_persistent_http_cache_falls_back_to_an_unconditional_fetch() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind corrupt cache fixture");
        let address = listener
            .local_addr()
            .expect("corrupt cache fixture address");
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept corrupt cache request");
                let request = read_http_request(&mut stream).to_ascii_lowercase();
                assert!(!request.contains("if-none-match:"));
                let body = r#"export const repaired = true;"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nETag: \"repair-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write corrupt cache fallback response");
            }
        });
        let url = format!("http://{address}/repair.js");
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };
        ScriptLoader::new(policy.clone())
            .load_and_execute_module(&url)
            .unwrap();
        let artifact = persistent_http_source_cache_files(directory.path())
            .into_iter()
            .next()
            .expect("persistent HTTP source artifact");
        std::fs::write(artifact, b"{broken-json").unwrap();

        let repaired_loader = ScriptLoader::new(policy);
        let namespace = repaired_loader.load_and_execute_module(&url).unwrap();
        server.join().expect("corrupt cache fixture completed");

        assert_eq!(namespace.get_property("repaired"), Value::Bool(true));
        assert_eq!(
            repaired_loader.http_source_cache_stats(),
            HttpSourceCacheStats {
                misses: 1,
                writes: 1,
                errors: 1,
                ..HttpSourceCacheStats::default()
            }
        );
    }

    #[test]
    fn persistent_http_cache_does_not_forward_validators_across_redirect_origins() {
        let directory = tempfile::tempdir().unwrap();
        let loader = ScriptLoader::new(ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        });
        let response = crate::fetch::FetchTextResponse {
            status: 200,
            ok: true,
            status_text: "OK".to_string(),
            headers: HashMap::from([
                ("content-type".to_string(), "text/javascript".to_string()),
                ("etag".to_string(), "\"private-cdn-tag\"".to_string()),
            ]),
            url: "https://cdn.example.test/module.js".to_string(),
            redirected: true,
            set_cookies: Vec::new(),
            body: "export const cached = true;".to_string(),
        };
        loader.store_persistent_http_source(
            "https://app.example.test/module.js",
            ScriptFetchMode::Module(ModuleCredentialsMode::SameOrigin),
            &response,
        );

        let (options, cached) = loader.prepare_http_revalidation(
            "https://app.example.test/module.js",
            ScriptFetchMode::Module(ModuleCredentialsMode::SameOrigin),
        );
        assert!(cached.is_none());
        assert!(!options.headers.contains_key("If-None-Match"));
        assert_eq!(loader.http_source_cache_stats().misses, 1);
    }

    #[test]
    fn persistent_module_cache_is_partitioned_by_credentials_mode() {
        let url = "https://cdn.example.test/module.js";
        let omit = persistent_http_source_cache_identity(
            url,
            ScriptFetchMode::Module(ModuleCredentialsMode::Omit),
        );
        let same_origin = persistent_http_source_cache_identity(
            url,
            ScriptFetchMode::Module(ModuleCredentialsMode::SameOrigin),
        );
        let include = persistent_http_source_cache_identity(
            url,
            ScriptFetchMode::Module(ModuleCredentialsMode::Include),
        );
        assert_ne!(omit, same_origin);
        assert_ne!(same_origin, include);
        assert_ne!(omit, include);
    }

    #[test]
    fn persistent_http_cache_refreshes_changed_module_then_revalidates_it() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Last-Modified fixture");
        let address = listener
            .local_addr()
            .expect("Last-Modified fixture address");
        let server = thread::spawn(move || {
            let versions = [
                (
                    "HTTP/1.1 200 OK",
                    "Wed, 21 Oct 2015 07:28:00 GMT",
                    r#"export const version = "v1";"#,
                ),
                (
                    "HTTP/1.1 200 OK",
                    "Thu, 22 Oct 2015 07:28:00 GMT",
                    r#"export const version = "v2";"#,
                ),
                (
                    "HTTP/1.1 304 Not Modified",
                    "Thu, 22 Oct 2015 07:28:00 GMT",
                    "",
                ),
            ];
            for (request_index, (status, modified, body)) in versions.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().expect("accept Last-Modified request");
                let request = read_http_request(&mut stream).to_ascii_lowercase();
                match request_index {
                    0 => assert!(!request.contains("if-modified-since:")),
                    1 => assert!(
                        request.contains("if-modified-since: wed, 21 oct 2015 07:28:00 gmt")
                    ),
                    2 => assert!(
                        request.contains("if-modified-since: thu, 22 oct 2015 07:28:00 gmt")
                    ),
                    _ => unreachable!(),
                }
                write!(
                    stream,
                    "{status}\r\nContent-Type: text/javascript\r\nLast-Modified: {modified}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("write Last-Modified response");
            }
        });
        let url = format!("http://{address}/versioned.js");
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };

        let first = ScriptLoader::new(policy.clone())
            .load_and_execute_module(&url)
            .unwrap();
        assert_eq!(first.get_property("version").to_js_string(), "v1");

        let second_loader = ScriptLoader::new(policy.clone());
        let second = second_loader.load_and_execute_module(&url).unwrap();
        assert_eq!(second.get_property("version").to_js_string(), "v2");
        assert_eq!(second_loader.http_source_cache_stats().refreshed, 1);
        assert_eq!(
            second_loader.compiled_source_cache_stats().persistent_hits,
            0
        );

        let third_loader = ScriptLoader::new(policy);
        let third = third_loader.load_and_execute_module(&url).unwrap();
        server.join().expect("Last-Modified fixture completed");
        assert_eq!(third.get_property("version").to_js_string(), "v2");
        assert_eq!(third_loader.http_source_cache_stats().not_modified, 1);
        assert_eq!(
            third_loader.compiled_source_cache_stats().persistent_hits,
            1
        );
    }

    #[test]
    fn persistent_http_cache_revalidates_cross_origin_module_with_cached_cors_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cached CORS fixture");
        let address = listener.local_addr().expect("cached CORS fixture address");
        let server = thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept cached CORS request");
                let request = read_http_request(&mut stream).to_ascii_lowercase();
                if request_index == 0 {
                    assert!(!request.contains("if-none-match:"));
                    let body = r#"export const value = "cors-cached";"#;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: https://app.example\r\nETag: \"cors-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("write initial cached CORS response");
                } else {
                    assert!(request.contains("if-none-match: \"cors-v1\""));
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"cors-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write cached CORS 304");
                }
            }
        });
        let dependency_url = format!("http://{address}/dependency.js");
        let main_url = "https://app.example/main.js";
        let main_source =
            format!(r#"import {{ value }} from "{dependency_url}"; export const result = value;"#);
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };

        let first_loader = ScriptLoader::new(policy.clone());
        first_loader
            .register_module_source(main_url, &main_source)
            .unwrap();
        let first = first_loader.load_and_execute_module(main_url).unwrap();
        assert_eq!(first.get_property("result").to_js_string(), "cors-cached");

        let second_loader = ScriptLoader::new(policy);
        second_loader
            .register_module_source(main_url, &main_source)
            .unwrap();
        let second = second_loader.load_and_execute_module(main_url).unwrap();
        server.join().expect("cached CORS fixture completed");

        assert_eq!(second.get_property("result").to_js_string(), "cors-cached");
        assert_eq!(second_loader.http_source_cache_stats().not_modified, 1);
    }

    #[test]
    fn persistent_http_cache_revalidates_a_redirected_module_at_the_same_final_url() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect cache fixture");
        let address = listener
            .local_addr()
            .expect("redirect cache fixture address");
        let server = thread::spawn(move || {
            for request_index in 0..4 {
                let (mut stream, _) = listener.accept().expect("accept redirect cache request");
                let request = read_http_request(&mut stream);
                let request_lower = request.to_ascii_lowercase();
                let is_entry = request.starts_with("GET /entry.js ");
                if request_index < 2 {
                    assert!(!request_lower.contains("if-none-match:"));
                } else {
                    assert!(request_lower.contains("if-none-match: \"redirect-v1\""));
                }
                if is_entry {
                    stream
                        .write_all(
                            b"HTTP/1.1 302 Found\r\nLocation: /asset.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write cached redirect");
                } else if request_index == 1 {
                    let body = r#"export const redirected = "cached";"#;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nETag: \"redirect-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("write redirect target");
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"redirect-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write redirected 304");
                }
            }
        });
        let url = format!("http://{address}/entry.js");
        let policy = ScriptPolicy {
            compiled_cache_dir: Some(directory.path().to_path_buf()),
            ..ScriptPolicy::default()
        };

        let first = ScriptLoader::new(policy.clone())
            .load_and_execute_module(&url)
            .unwrap();
        assert_eq!(first.get_property("redirected").to_js_string(), "cached");
        let second_loader = ScriptLoader::new(policy);
        let second = second_loader.load_and_execute_module(&url).unwrap();
        server.join().expect("redirect cache fixture completed");

        assert_eq!(second.get_property("redirected").to_js_string(), "cached");
        assert_eq!(second_loader.http_source_cache_stats().not_modified, 1);
        assert_eq!(
            second_loader.compiled_source_cache_stats().persistent_hits,
            1
        );
    }

    #[test]
    fn esm_linker_instantiates_cycles_without_recursive_evaluation() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/a.js",
                r#"
                    import { b } from "./b.js";
                    export const a = "a";
                    callback(a);
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/b.js",
                r#"
                    import { a } from "./a.js";
                    export const b = "b";
                    callback(b);
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        let namespace = loader
            .load_and_execute_module("https://example.test/a.js")
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &["b", "a"]);
        assert_eq!(namespace.get_property("a").to_js_string(), "a");
    }

    #[test]
    fn esm_cycle_rejects_reads_before_export_initialization() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/a.js",
                r#"
                    import { b } from "./b.js";
                    export const a = "a";
                    callback(b);
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/b.js",
                r#"
                    import { a } from "./a.js";
                    export const b = a;
                "#,
            )
            .unwrap();

        let error = loader
            .load_and_execute_module("https://example.test/a.js")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Cannot access 'a' before initialization"),
            "unexpected cycle error: {error}"
        );
    }

    #[test]
    fn failed_cyclic_module_evaluation_is_cached_with_its_original_reason() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/a.js",
                r#"
                    import "./b.js";
                    callback("a");
                    export const a = "a";
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/b.js",
                r#"
                    import "./a.js";
                    callback("b");
                    await Promise.reject("cycle-denied");
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );

        for _ in 0..2 {
            let error = loader
                .load_and_execute_module("https://example.test/a.js")
                .unwrap_err();
            assert!(
                error.to_string().contains("cycle-denied"),
                "cached evaluation lost its original rejection: {error}"
            );
        }
        loader
            .execute_module_source(
                r#"
                    import("./a.js").catch((reason) => {
                        callback("dynamic:" + reason);
                    });
                "#,
                "https://example.test/dynamic-consumer.js",
            )
            .unwrap();
        let observed = observed.borrow();
        assert_eq!(observed[0], "b");
        assert_eq!(observed.len(), 2);
        assert!(
            observed[1].contains("cycle-denied"),
            "dynamic import lost the cached rejection: {}",
            observed[1]
        );
    }

    #[test]
    fn async_cyclic_dependency_does_not_block_later_sibling_evaluation() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/a.js",
                r#"
                    import "./b.js";
                    import "./c.js";
                    callback("a");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/b.js",
                r#"
                    import "./a.js";
                    callback("b:start");
                    await Promise.resolve();
                    callback("b:end");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/c.js",
                r#"
                    import "./a.js";
                    callback("c");
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );

        loader
            .load_and_execute_module("https://example.test/a.js")
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &["b:start", "c", "b:end", "a"]
        );
    }

    #[test]
    fn async_parent_readiness_follows_dfs_order_across_shared_dependencies() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/a.js",
                r#"
                    import "./b.js";
                    import "./c.js";
                    callback("a");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/b.js",
                r#"
                    import "./d.js";
                    callback("b:start");
                    await bGate;
                    callback("b:end");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/c.js",
                r#"
                    import "./d.js";
                    import "./e.js";
                    callback("c");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/d.js",
                r#"
                    import "./a.js";
                    callback("d:start");
                    await dGate;
                    callback("d:end");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/e.js",
                r#"
                    callback("e:start");
                    await eGate;
                    callback("e:end");
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );

        let make_gate = |name: &str| {
            let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
            let executor_slot = Rc::clone(&resolve_slot);
            let gate = w3cos_core::promise::new(vec![Value::function(move |_, arguments| {
                *executor_slot.borrow_mut() =
                    arguments.first().cloned().unwrap_or(Value::Undefined);
                Value::Undefined
            })]);
            crate::jsdom::window_value().set_property(name, gate);
            resolve_slot
        };
        let d_resolve = make_gate("dGate");
        let e_resolve = make_gate("eGate");
        let b_resolve = make_gate("bGate");

        let evaluation = loader.load_and_execute_module_async("https://example.test/a.js");
        crate::jsdom::drain_microtasks();
        assert_eq!(observed.borrow().as_slice(), &["d:start", "e:start"]);
        assert!(matches!(
            w3cos_core::promise::status(&evaluation),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));

        e_resolve
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            observed.borrow().as_slice(),
            &["d:start", "e:start", "e:end"]
        );

        d_resolve
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            observed.borrow().as_slice(),
            &["d:start", "e:start", "e:end", "d:end", "b:start", "c"]
        );
        assert!(matches!(
            w3cos_core::promise::status(&evaluation),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));

        b_resolve
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                "d:start", "e:start", "e:end", "d:end", "b:start", "c", "b:end", "a"
            ]
        );
        assert!(matches!(
            w3cos_core::promise::status(&evaluation),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(_))
        ));
    }

    #[test]
    fn async_cycle_rejection_bypasses_an_unrelated_pending_sibling() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        for (url, source) in [
            (
                "https://example.test/a.js",
                r#"
                    import "./b.js";
                    import "./c.js";
                    callback("a");
                "#,
            ),
            (
                "https://example.test/b.js",
                r#"
                    import "./d.js";
                    callback("b:start");
                    await bGate;
                    callback("b:end");
                "#,
            ),
            (
                "https://example.test/c.js",
                r#"
                    import "./d.js";
                    import "./e.js";
                    callback("c:start");
                    await Promise.reject("c-denied");
                    callback("c:end");
                "#,
            ),
            (
                "https://example.test/d.js",
                r#"
                    import "./a.js";
                    callback("d:start");
                    await dGate;
                    callback("d:end");
                "#,
            ),
            (
                "https://example.test/e.js",
                r#"
                    callback("e:start");
                    await eGate;
                    callback("e:end");
                "#,
            ),
        ] {
            loader.register_module_source(url, source).unwrap();
        }

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );

        let make_gate = |name: &str| {
            let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
            let executor_slot = Rc::clone(&resolve_slot);
            let gate = w3cos_core::promise::new(vec![Value::function(move |_, arguments| {
                *executor_slot.borrow_mut() =
                    arguments.first().cloned().unwrap_or(Value::Undefined);
                Value::Undefined
            })]);
            crate::jsdom::window_value().set_property(name, gate);
            resolve_slot
        };
        let d_resolve = make_gate("dGate");
        let e_resolve = make_gate("eGate");
        let b_resolve = make_gate("bGate");

        let evaluation = loader.load_and_execute_module_async("https://example.test/a.js");
        crate::jsdom::drain_microtasks();
        assert_eq!(observed.borrow().as_slice(), &["d:start", "e:start"]);

        e_resolve
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            observed.borrow().as_slice(),
            &["d:start", "e:start", "e:end"]
        );

        d_resolve
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            observed.borrow().as_slice(),
            &["d:start", "e:start", "e:end", "d:end", "b:start", "c:start"]
        );
        match w3cos_core::promise::status(&evaluation) {
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => {
                assert_eq!(reason.to_js_string(), "c-denied");
            }
            _ => panic!("cycle root did not reject immediately"),
        }

        let member_evaluation = loader.load_and_execute_module_async("https://example.test/b.js");
        crate::jsdom::drain_microtasks();
        match w3cos_core::promise::status(&member_evaluation) {
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => {
                assert_eq!(reason.to_js_string(), "c-denied");
            }
            _ => panic!("cycle member did not reuse the root rejection"),
        }

        b_resolve
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            observed.borrow().as_slice(),
            &[
                "d:start", "e:start", "e:end", "d:end", "b:start", "c:start", "b:end"
            ]
        );
        assert!(matches!(
            w3cos_core::promise::status(&evaluation),
            Some(w3cos_core::promise::PromiseStatus::Rejected(_))
        ));
    }

    #[test]
    fn synchronous_dependency_failure_stops_later_sibling_evaluation() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        for (url, source) in [
            (
                "https://example.test/main.js",
                r#"
                    import "./bad.js";
                    import "./later.js";
                    callback("main");
                "#,
            ),
            (
                "https://example.test/bad.js",
                r#"
                    callback("bad");
                    fail();
                "#,
            ),
            ("https://example.test/later.js", r#"callback("later");"#),
        ] {
            loader.register_module_source(url, source).unwrap();
        }

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        crate::jsdom::window_value().set_property(
            "fail",
            Value::function(|_, _| w3cos_core::throw_value(Value::string("sync-denied"))),
        );

        let evaluation = loader.load_and_execute_module_async("https://example.test/main.js");
        crate::jsdom::drain_microtasks();
        match w3cos_core::promise::status(&evaluation) {
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => {
                assert_eq!(reason.to_js_string(), "sync-denied");
            }
            _ => panic!("synchronous module failure did not reject the graph"),
        }
        assert_eq!(observed.borrow().as_slice(), &["bad"]);
    }

    #[test]
    fn synchronous_module_branch_finishes_before_the_next_sibling_branch() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        for (url, source) in [
            (
                "https://example.test/a.js",
                r#"
                    import "./b.js";
                    import "./c.js";
                    callback("a");
                "#,
            ),
            (
                "https://example.test/b.js",
                r#"
                    import "./d.js";
                    callback("b");
                "#,
            ),
            (
                "https://example.test/c.js",
                r#"
                    import "./e.js";
                    callback("c");
                "#,
            ),
            ("https://example.test/d.js", r#"callback("d");"#),
            ("https://example.test/e.js", r#"callback("e");"#),
        ] {
            loader.register_module_source(url, source).unwrap();
        }

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );

        loader
            .load_and_execute_module("https://example.test/a.js")
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &["d", "b", "e", "c", "a"]);
    }

    #[test]
    fn cyclic_member_reuses_root_rejection_without_rerunning_its_body() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/root.js",
                r#"
                    import "./member.js";
                    callback("root");
                    await Promise.reject("root-denied");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/member.js",
                r#"
                    import "./root.js";
                    callback("member");
                    export const value = "member-value";
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );

        let root_error = loader
            .load_and_execute_module("https://example.test/root.js")
            .unwrap_err();
        assert!(root_error.to_string().contains("root-denied"));
        let member_error = loader
            .load_and_execute_module("https://example.test/member.js")
            .unwrap_err();
        assert!(
            member_error.to_string().contains("root-denied"),
            "cycle member did not reuse the root rejection: {member_error}"
        );
        assert_eq!(observed.borrow().as_slice(), &["member", "root"]);

        loader
            .execute_module_source(
                r#"
                    import("./member.js").catch((reason) => {
                        callback("dynamic:" + reason);
                    });
                "#,
                "https://example.test/dynamic-cycle-consumer.js",
            )
            .unwrap();
        let observed = observed.borrow();
        assert_eq!(observed.len(), 3);
        assert!(observed[2].contains("dynamic:root-denied"));
    }

    #[test]
    fn fulfilled_cycle_member_returns_its_own_namespace_via_the_root_settlement() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/root.js",
                r#"
                    import { member } from "./member.js";
                    export const root = "root-value";
                    callback(member);
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/member.js",
                r#"
                    import "./root.js";
                    export const member = "member-value";
                "#,
            )
            .unwrap();

        let calls = Rc::new(Cell::new(0_u32));
        let observed_calls = Rc::clone(&calls);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                assert_eq!(arguments[0].to_js_string(), "member-value");
                observed_calls.set(observed_calls.get() + 1);
                Value::Undefined
            }),
        );

        let root = loader
            .load_and_execute_module("https://example.test/root.js")
            .unwrap();
        assert_eq!(root.get_property("root").to_js_string(), "root-value");

        let member = loader
            .load_and_execute_module("https://example.test/member.js")
            .unwrap();
        assert_eq!(member.get_property("member").to_js_string(), "member-value");
        assert!(member.get_property("root").is_undefined());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn export_star_forwards_non_default_live_bindings() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/dependency.js",
                r#"
                    export let value = "before";
                    export const update = () => { value = "after"; };
                    export default "not-forwarded";
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/barrel.js",
                r#"export * from "./dependency.js";"#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/main.js",
                r#"
                    import { value, update } from "./barrel.js";
                    callback(value);
                    update();
                    callback(value);
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        loader
            .load_and_execute_module("https://example.test/main.js")
            .unwrap();

        assert_eq!(observed.borrow().as_slice(), &["before", "after"]);
        let barrel = loader
            .load_and_execute_module("https://example.test/barrel.js")
            .unwrap();
        assert!(barrel.get_property("default").is_undefined());
        assert_eq!(barrel.get_property("value").to_js_string(), "after");
    }

    #[test]
    fn direct_export_overrides_star_exports_and_ambiguous_stars_fail_imports() {
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source("https://example.test/a.js", r#"export const value = "a";"#)
            .unwrap();
        loader
            .register_module_source("https://example.test/b.js", r#"export const value = "b";"#)
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/override.js",
                r#"
                    export * from "./a.js";
                    export * from "./b.js";
                    export const value = "own";
                "#,
            )
            .unwrap();
        let namespace = loader
            .load_and_execute_module("https://example.test/override.js")
            .unwrap();
        assert_eq!(namespace.get_property("value").to_js_string(), "own");

        loader
            .register_module_source(
                "https://example.test/ambiguous.js",
                r#"
                    export * from "./a.js";
                    export * from "./b.js";
                "#,
            )
            .unwrap();
        let error = loader
            .execute_module_source(
                r#"import { value } from "./ambiguous.js"; callback(value);"#,
                "https://example.test/import-ambiguous.js",
            )
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous export"));
    }

    #[test]
    fn top_level_await_delays_importers_until_dependency_evaluation_finishes() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/dependency.js",
                r#"
                    callback("dependency:start");
                    export const value = await Promise.resolve("ready");
                    callback("dependency:end");
                "#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/main.js",
                r#"
                    import { value } from "./dependency.js";
                    callback("main:" + value);
                "#,
            )
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );

        loader
            .load_and_execute_module("https://example.test/main.js")
            .unwrap();
        assert_eq!(
            observed.borrow().as_slice(),
            &["dependency:start", "dependency:end", "main:ready"]
        );
    }

    #[test]
    fn rejected_top_level_await_fails_the_module_graph() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let error = ScriptLoader::new(ScriptPolicy::default())
            .execute_module_source(
                r#"
                    await Promise.reject("module-denied");
                    export const unreachable = true;
                "#,
                "https://example.test/rejected.js",
            )
            .unwrap_err();

        assert!(error.to_string().contains("module-denied"));
    }

    #[test]
    fn dynamic_import_adopts_top_level_await_before_exposing_the_namespace() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/async-chunk.js",
                r#"export const status = await Promise.resolve("async-ready");"#,
            )
            .unwrap();

        loader
            .execute_module_source(
                r#"
                    import("./async-chunk.js").then((namespace) => {
                        document.body.setAttribute("data-async-chunk", namespace.status);
                    });
                "#,
                "https://example.test/main.js",
            )
            .unwrap();

        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-async-chunk")])
                .to_js_string(),
            "async-ready"
        );
    }

    #[test]
    fn script_loader_installs_the_core_aot_dynamic_import_adapter() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_core::module_registry::clear();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let url = "https://example.test/aot-runtime-chunk.js";
        loader
            .register_module_source(
                url,
                r#"export const status = await Promise.resolve("aot-runtime-ready");"#,
            )
            .unwrap();

        let loaded = w3cos_core::host_modules::dynamic_import(
            Value::string(url),
            Value::string("https://example.test/aot-entry.js"),
        );
        crate::jsdom::drain_microtasks();
        assert!(matches!(
            w3cos_core::promise::status(&loaded),
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(namespace))
                if namespace.get_property("status").to_js_string() == "aot-runtime-ready"
        ));
        assert!(w3cos_core::module_registry::contains(url));
        w3cos_core::module_registry::clear();
    }

    #[test]
    fn navigation_unregisters_runtime_modules_without_removing_native_modules() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_core::module_registry::clear();
        let native_url = "build:///native-preserved.js";
        let runtime_url = "https://example.test/navigation-runtime.js";
        w3cos_core::module_registry::register(native_url, HashMap::new(), None);

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .execute_module_source("export const value = 42;", runtime_url)
            .unwrap();
        assert!(w3cos_core::module_registry::contains(native_url));
        assert!(w3cos_core::module_registry::contains(runtime_url));

        loader.cancel_for_navigation();

        assert!(w3cos_core::module_registry::contains(native_url));
        assert!(!w3cos_core::module_registry::contains(runtime_url));
        w3cos_core::module_registry::clear();
    }

    #[test]
    fn dropping_a_loader_unregisters_its_runtime_modules() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_core::module_registry::clear();
        let url = "https://example.test/dropped-runtime.js";
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .execute_module_source("export const value = 42;", url)
            .unwrap();
        assert!(w3cos_core::module_registry::contains(url));

        drop(loader);

        assert!(!w3cos_core::module_registry::contains(url));
        w3cos_core::module_registry::clear();
    }

    #[test]
    fn navigation_cancels_a_suspended_runtime_module_before_it_can_resume() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        w3cos_core::module_registry::clear();
        let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
        let executor_slot = Rc::clone(&resolve_slot);
        let gate = w3cos_core::promise::new(vec![Value::function(move |_, arguments| {
            *executor_slot.borrow_mut() = arguments.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        })]);
        crate::jsdom::window_value().set_property("navigationModuleGate", gate);

        let url = "https://example.test/suspended-navigation.js";
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let evaluation = loader.execute_module_source_async(
            r#"
                await navigationModuleGate;
                export const stale = true;
            "#,
            url,
        );
        crate::jsdom::drain_microtasks();
        assert!(matches!(
            w3cos_core::promise::status(&evaluation),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));
        assert!(w3cos_core::module_registry::contains(url));

        loader.cancel_for_navigation();
        resolve_slot
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        crate::jsdom::drain_microtasks();

        assert!(!w3cos_core::module_registry::contains(url));
        assert!(matches!(
            w3cos_core::promise::status(&evaluation),
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason))
                if reason.to_js_string().contains("Cancelled")
        ));
        w3cos_core::module_registry::clear();
    }

    #[test]
    fn bare_esm_specifiers_fail_until_an_import_map_resolves_them() {
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let error = loader
            .execute_module_source(
                r#"import { value } from "package"; callback(value);"#,
                "https://example.test/main.js",
            )
            .unwrap_err();
        assert!(error.to_string().contains("requires an import map"));
    }

    #[test]
    fn inserted_import_map_resolves_bare_module_prefixes() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/vendor/map-sdk/core.js",
                r#"export const status = "mapped";"#,
            )
            .unwrap();
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let document = crate::jsdom::document_value();
        let import_map = document.call_method("createElement", vec![Value::string("script")]);
        import_map.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("importmap")],
        );
        import_map.set_property(
            "textContent",
            Value::string(r#"{"imports":{"map-sdk/":"/vendor/map-sdk/"}}"#),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![import_map]);

        let module = document.call_method("createElement", vec![Value::string("script")]);
        module.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        module.set_property(
            "textContent",
            Value::string(
                r#"
                    import { status } from "map-sdk/core.js";
                    document.body.setAttribute("data-import-map", status);
                "#,
            ),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![module]);

        crate::jsdom::drain_microtasks();
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-import-map")])
                .to_js_string(),
            "mapped"
        );
    }

    #[test]
    fn scoped_import_map_uses_longest_scope_and_falls_back_to_parent_scope() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        for (url, source) in [
            (
                "https://example.test/global-sdk.js",
                r#"export const name = "global";"#,
            ),
            (
                "https://example.test/feature-sdk.js",
                r#"export const name = "feature";"#,
            ),
            (
                "https://example.test/deep-sdk.js",
                r#"export const name = "deep";"#,
            ),
            (
                "https://example.test/feature-pkg-specific/item.js",
                r#"export const tool = "scoped-tool";"#,
            ),
            (
                "https://example.test/features/deep/main.js",
                r#"
                    import { name } from "sdk";
                    import { tool } from "pkg/tool/item.js";
                    callback(name + ":" + tool);
                "#,
            ),
        ] {
            loader.register_module_source(url, source).unwrap();
        }

        let document = crate::jsdom::document_value();
        let import_map = document.call_method("createElement", vec![Value::string("script")]);
        import_map.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("importmap")],
        );
        import_map.set_property(
            "textContent",
            Value::string(
                r#"{
                    "imports": {"sdk": "/global-sdk.js"},
                    "scopes": {
                        "/features/": {
                            "sdk": "/feature-sdk.js",
                            "pkg/": "/feature-pkg-general/",
                            "pkg/tool/": "/feature-pkg-specific/"
                        },
                        "/features/deep/": {"sdk": "/deep-sdk.js"}
                    }
                }"#,
            ),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![import_map]);
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let observed = Rc::new(RefCell::new(Vec::<String>::new()));
        let callback_observed = Rc::clone(&observed);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, arguments| {
                callback_observed
                    .borrow_mut()
                    .push(arguments[0].to_js_string());
                Value::Undefined
            }),
        );
        loader
            .load_and_execute_module("https://example.test/features/deep/main.js")
            .unwrap();
        assert_eq!(observed.borrow().as_slice(), &["deep:scoped-tool"]);
    }

    #[test]
    fn import_map_can_remap_url_like_specifiers() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .set_import_map(
                "https://example.test/app/index.html",
                HashMap::from([("./dependency.js".into(), "./mapped.js".into())]),
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/app/mapped.js",
                r#"export const value = "mapped-url";"#,
            )
            .unwrap();

        let namespace = loader
            .execute_module_source(
                r#"
                    import { value } from "./dependency.js";
                    export const result = value;
                "#,
                "https://example.test/app/main.js",
            )
            .unwrap();
        assert_eq!(
            namespace.get_property("result").to_js_string(),
            "mapped-url"
        );
    }

    #[test]
    fn late_import_map_adds_unresolved_names_without_changing_prior_resolutions() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/dependency.js",
                r#"export const value = "original";"#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/replacement.js",
                r#"export const value = "replacement";"#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/late.js",
                r#"export const late = "installed";"#,
            )
            .unwrap();

        let first = loader
            .execute_module_source(
                r#"import { value } from "./dependency.js"; export const result = value;"#,
                "https://example.test/first.js",
            )
            .unwrap();
        assert_eq!(first.get_property("result").to_js_string(), "original");

        loader
            .set_import_map(
                "https://example.test/index.html",
                HashMap::from([
                    ("./dependency.js".into(), "/replacement.js".into()),
                    ("late".into(), "/late.js".into()),
                ]),
            )
            .unwrap();

        assert_eq!(
            loader
                .resolve_module_url("https://example.test/second.js", "./dependency.js")
                .unwrap(),
            "https://example.test/dependency.js"
        );
        let late = loader
            .execute_module_source(
                r#"import { late } from "late"; export const result = late;"#,
                "https://example.test/late-consumer.js",
            )
            .unwrap();
        assert_eq!(late.get_property("result").to_js_string(), "installed");
    }

    #[test]
    fn dynamically_inserted_late_import_map_serves_a_later_module_graph() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();
        loader
            .execute_module_source(
                r#"export const first = true;"#,
                "https://example.test/first.js",
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/late-dom.js",
                r#"export const value = "late-dom";"#,
            )
            .unwrap();

        let document = crate::jsdom::document_value();
        let import_map = document.call_method("createElement", vec![Value::string("script")]);
        import_map.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("importmap")],
        );
        import_map.set_property(
            "textContent",
            Value::string(r#"{"imports":{"late-dom":"/late-dom.js"}}"#),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![import_map]);
        crate::jsdom::drain_microtasks();

        let namespace = loader
            .execute_module_source(
                r#"import { value } from "late-dom"; export const result = value;"#,
                "https://example.test/consumer.js",
            )
            .unwrap();
        assert_eq!(namespace.get_property("result").to_js_string(), "late-dom");
    }

    #[test]
    fn late_scoped_import_map_preserves_prior_scope_decisions() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        for (url, source) in [
            (
                "https://example.test/original.js",
                r#"export const value = "original";"#,
            ),
            (
                "https://example.test/replacement.js",
                r#"export const value = "replacement";"#,
            ),
            (
                "https://example.test/new-scoped.js",
                r#"export const scoped = "new";"#,
            ),
        ] {
            loader.register_module_source(url, source).unwrap();
        }
        loader
            .set_import_map(
                "https://example.test/index.html",
                HashMap::from([("pkg".into(), "/original.js".into())]),
            )
            .unwrap();
        loader
            .execute_module_source(
                r#"import { value } from "pkg"; export const result = value;"#,
                "https://example.test/scope/first.js",
            )
            .unwrap();

        loader
            .set_scoped_import_map(
                "https://example.test/index.html",
                HashMap::new(),
                HashMap::from([(
                    "/scope/".into(),
                    HashMap::from([
                        ("pkg".into(), "/replacement.js".into()),
                        ("new-scoped".into(), "/new-scoped.js".into()),
                    ]),
                )]),
            )
            .unwrap();

        assert_eq!(
            loader
                .resolve_module_url("https://example.test/scope/second.js", "pkg")
                .unwrap(),
            "https://example.test/original.js"
        );
        assert_eq!(
            loader
                .resolve_module_url("https://example.test/scope/second.js", "new-scoped")
                .unwrap(),
            "https://example.test/new-scoped.js"
        );
    }

    #[test]
    fn cross_origin_module_fetch_requires_an_allow_origin_response() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let (blocked_url, blocked_fixture) = module_fixture(r#"export const value = 41;"#, None);
        let blocked_loader = ScriptLoader::new(ScriptPolicy::default());
        blocked_loader
            .register_module_source(
                "https://app.example/main.js",
                &format!(
                    r#"import {{ value }} from "{blocked_url}"; export const answer = value + 1;"#
                ),
            )
            .unwrap();
        let error = blocked_loader
            .load_and_execute_module("https://app.example/main.js")
            .unwrap_err();
        blocked_fixture.join().expect("blocked fixture completed");
        assert!(error.to_string().contains("module CORS check failed"));

        let (allowed_url, allowed_fixture) =
            module_fixture(r#"export const value = 41;"#, Some("https://app.example"));
        let allowed_loader = ScriptLoader::new(ScriptPolicy::default());
        allowed_loader
            .register_module_source(
                "https://app.example/main.js",
                &format!(
                    r#"import {{ value }} from "{allowed_url}"; export const answer = value + 1;"#
                ),
            )
            .unwrap();
        let namespace = allowed_loader
            .load_and_execute_module("https://app.example/main.js")
            .unwrap();
        allowed_fixture.join().expect("allowed fixture completed");
        assert_eq!(namespace.get_property("answer").to_u32(), 42);
    }

    #[test]
    fn cors_rejects_duplicate_singleton_response_headers() {
        for duplicate_headers in [
            "Access-Control-Allow-Origin: https://attacker.example\r\nAccess-Control-Allow-Origin: https://app.example\r\n",
            "Access-Control-Allow-Origin: https://app.example\r\nAccess-Control-Allow-Credentials: false\r\nAccess-Control-Allow-Credentials: true\r\n",
        ] {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("bind duplicate CORS header fixture");
            let address = listener
                .local_addr()
                .expect("duplicate CORS header fixture address");
            let duplicate_headers = duplicate_headers.to_string();
            let fixture = thread::spawn(move || {
                let (mut stream, _) = listener
                    .accept()
                    .expect("accept duplicate CORS header request");
                let _ = read_http_request(&mut stream);
                let body = r#"export const exposed = true;"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\n{duplicate_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write duplicate CORS header response");
            });

            let loader = ScriptLoader::new(ScriptPolicy::default());
            loader
                .register_module_source(
                    "https://app.example/main.js",
                    &format!(
                        r#"import {{ exposed }} from "http://{address}/module.js"; export {{ exposed }};"#
                    ),
                )
                .unwrap();
            let error = loader
                .load_and_execute_module_with_credentials(
                    "https://app.example/main.js",
                    ModuleCredentialsMode::Include,
                )
                .unwrap_err();
            fixture.join().expect("duplicate CORS fixture completed");
            assert!(
                error
                    .to_string()
                    .contains("credentialed module CORS check failed"),
                "unexpected duplicate CORS header result: {error}"
            );
        }
    }

    #[test]
    fn cross_origin_module_redirect_requires_cors_before_following_location() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind cross-origin redirect fixture");
        let address = listener
            .local_addr()
            .expect("cross-origin redirect fixture address");
        let fixture = thread::spawn(move || {
            let (mut redirect, _) = listener.accept().expect("accept redirect request");
            let _ = read_http_request(&mut redirect);
            redirect
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect without CORS permission");

            listener
                .set_nonblocking(true)
                .expect("set redirect fixture nonblocking");
            for _ in 0..100 {
                match listener.accept() {
                    Ok((mut target, _)) => {
                        let _ = read_http_request(&mut target);
                        let body = r#"export const followed = true;"#;
                        write!(
                            target,
                            "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: https://app.example\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .expect("write unexpectedly followed redirect target");
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept redirect target: {error}"),
                }
            }
            false
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://app.example/main.js",
                &format!(
                    r#"import {{ followed }} from "http://{address}/redirect.js"; export {{ followed }};"#
                ),
            )
            .unwrap();
        let error = loader
            .load_and_execute_module("https://app.example/main.js")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("script CORS redirect check failed"),
            "unexpected redirect CORS failure: {error}"
        );
        assert!(
            !fixture.join().expect("redirect fixture completed"),
            "a cross-origin redirect without CORS permission was followed"
        );
    }

    #[test]
    fn cors_redirect_rejects_a_location_with_url_credentials() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let redirect_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind credential redirect fixture");
        let redirect_address = redirect_listener
            .local_addr()
            .expect("credential redirect fixture address");
        let target_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind credential redirect target");
        let target_address = target_listener
            .local_addr()
            .expect("credential redirect target address");
        let redirect_fixture = thread::spawn(move || {
            let (mut stream, _) = redirect_listener
                .accept()
                .expect("accept credential redirect request");
            let _ = read_http_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://user:password@{target_address}/final.js\r\nAccess-Control-Allow-Origin: https://app.example\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write credential-bearing redirect");
        });
        let target_fixture = thread::spawn(move || {
            target_listener
                .set_nonblocking(true)
                .expect("set credential redirect target nonblocking");
            for _ in 0..100 {
                match target_listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = read_http_request(&mut stream);
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept credential redirect target: {error}"),
                }
            }
            false
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://app.example/main.js",
                &format!(
                    r#"import {{ exposed }} from "http://{redirect_address}/redirect.js"; export {{ exposed }};"#
                ),
            )
            .unwrap();
        let error = loader
            .load_and_execute_module("https://app.example/main.js")
            .unwrap_err();
        redirect_fixture
            .join()
            .expect("credential redirect fixture completed");
        assert!(
            !target_fixture
                .join()
                .expect("credential redirect target completed"),
            "a credential-bearing CORS redirect was followed"
        );
        assert!(
            error
                .to_string()
                .contains("script redirect URL must not include credentials"),
            "unexpected credential redirect failure: {error}"
        );
    }

    #[test]
    fn network_modules_require_a_javascript_mime_type() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let wrong_listener = TcpListener::bind("127.0.0.1:0").expect("bind wrong MIME fixture");
        let wrong_address = wrong_listener.local_addr().expect("wrong MIME address");
        let wrong_fixture = thread::spawn(move || {
            let (mut stream, _) = wrong_listener.accept().expect("accept wrong MIME request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = r#"export const value = 42;"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write wrong MIME response");
        });
        let error = ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute_module(&format!("http://{wrong_address}/wrong.js"))
            .unwrap_err();
        wrong_fixture.join().expect("wrong MIME fixture completed");
        assert!(error.to_string().contains("module MIME check failed"));
        assert!(error.to_string().contains("text/plain"));

        let valid_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind parameterized MIME fixture");
        let valid_address = valid_listener
            .local_addr()
            .expect("parameterized MIME address");
        let valid_fixture = thread::spawn(move || {
            let (mut stream, _) = valid_listener
                .accept()
                .expect("accept parameterized MIME request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = r#"export const value = 42;"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: Application/JavaScript; Charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write parameterized MIME response");
        });
        let namespace = ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute_module(&format!("http://{valid_address}/valid.js"))
            .unwrap();
        valid_fixture
            .join()
            .expect("parameterized MIME fixture completed");
        assert_eq!(namespace.get_property("value").to_u32(), 42);
    }

    #[test]
    fn module_cors_uses_the_final_url_after_redirects() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let final_listener = TcpListener::bind("127.0.0.1:0").expect("bind final module fixture");
        let final_address = final_listener
            .local_addr()
            .expect("final module fixture address");
        let final_fixture = thread::spawn(move || {
            let (mut stream, _) = final_listener
                .accept()
                .expect("accept final module request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = r#"export const value = 41;"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write final module response");
        });

        let origin_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind redirecting module fixture");
        let origin_address = origin_listener
            .local_addr()
            .expect("redirecting module fixture address");
        let redirect_fixture = thread::spawn(move || {
            let (mut stream, _) = origin_listener
                .accept()
                .expect("accept redirecting module request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{final_address}/final.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .expect("write module redirect");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        let root_url = format!("http://{origin_address}/main.js");
        loader
            .register_module_source(
                &root_url,
                r#"import { value } from "./redirect.js"; export const answer = value + 1;"#,
            )
            .unwrap();
        let error = loader.load_and_execute_module(&root_url).unwrap_err();
        redirect_fixture
            .join()
            .expect("redirecting fixture completed");
        final_fixture.join().expect("final fixture completed");
        assert!(
            error.to_string().contains("module CORS check failed"),
            "unexpected redirect failure: {error}"
        );
        assert!(error.to_string().contains(&final_address.to_string()));
    }

    #[test]
    fn same_origin_module_redirect_regenerates_cookies_for_the_target_url() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect cookie fixture");
        let address = listener.local_addr().expect("redirect cookie address");
        let origin = format!("http://{address}");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &format!("{origin}/final/bootstrap.js"),
            "target=matched; Path=/final",
            true,
        );
        let fixture = thread::spawn(move || {
            let (mut redirect, _) = listener.accept().expect("accept redirect request");
            let mut request = [0_u8; 2048];
            let read = redirect.read(&mut request).expect("read redirect request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(!request.to_ascii_lowercase().contains("\r\ncookie:"));
            redirect
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final/module.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect");

            let (mut target, _) = listener.accept().expect("accept redirect target");
            let mut request = [0_u8; 2048];
            let read = target.read(&mut request).expect("read target request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("\r\ncookie: target=matched\r\n"),
                "redirect target did not receive its URL-matched cookie: {request}"
            );
            let body = r#"export const redirectedCookie = true;"#;
            write!(
                target,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write redirect target");
        });

        let namespace = ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute_module(&format!("{origin}/start.js"))
            .unwrap();
        fixture.join().expect("redirect cookie fixture completed");
        assert!(namespace.get_property("redirectedCookie").to_bool());
    }

    #[test]
    fn same_origin_redirect_set_cookie_is_available_to_the_next_hop() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect Set-Cookie fixture");
        let address = listener.local_addr().expect("redirect Set-Cookie address");
        let origin = format!("http://{address}");
        let fixture = thread::spawn(move || {
            let (mut redirect, _) = listener.accept().expect("accept redirect request");
            let mut request = [0_u8; 2048];
            let _ = redirect.read(&mut request).expect("read redirect request");
            redirect
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final/module.js\r\nSet-Cookie: hop=ready; Path=/final; HttpOnly\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect Set-Cookie");

            let (mut target, _) = listener.accept().expect("accept redirect target");
            let mut request = [0_u8; 2048];
            let read = target.read(&mut request).expect("read target request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("\r\ncookie: hop=ready\r\n"),
                "next redirect hop did not receive Set-Cookie state: {request}"
            );
            let body = r#"export const redirectCookieReplay = true;"#;
            write!(
                target,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write redirect target");
        });

        let namespace = ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute_module(&format!("{origin}/start.js"))
            .unwrap();
        fixture
            .join()
            .expect("redirect Set-Cookie fixture completed");
        assert!(namespace.get_property("redirectCookieReplay").to_bool());
        assert_eq!(
            crate::cookie_store_web::cookie_header_for_url(&format!("{origin}/final/next.js")),
            "hop=ready"
        );
        crate::cookie_store_web::set_active_url(&format!("{origin}/final/index.html"));
        assert_eq!(crate::cookie_store_web::document_cookie(), "");
    }

    #[test]
    fn cross_origin_module_redirect_strips_page_cookies() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let origin_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind redirect source fixture");
        let origin_address = origin_listener
            .local_addr()
            .expect("redirect source address");
        let target_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind redirect target fixture");
        let target_address = target_listener
            .local_addr()
            .expect("redirect target address");
        let origin = format!("http://{origin_address}");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &format!("{origin}/start.js"),
            "page=secret; Path=/",
            true,
        );
        let redirect_target = format!("http://{target_address}/module.js");
        let source_request_origin = origin.clone();
        let redirect_fixture = thread::spawn(move || {
            let (mut stream, _) = origin_listener.accept().expect("accept source request");
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).expect("read source request");
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(request.contains("\r\ncookie: page=secret\r\n"));
            assert!(
                request.contains(&format!(
                    "\r\norigin: {}\r\n",
                    source_request_origin.to_ascii_lowercase()
                )),
                "module request did not carry its CORS Origin header: {request}"
            );
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {redirect_target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write cross-origin redirect");
        });
        let allowed_origin = origin.clone();
        let target_fixture = thread::spawn(move || {
            let (mut stream, _) = target_listener.accept().expect("accept target request");
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).expect("read target request");
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(
                !request.contains("\r\ncookie:"),
                "page cookie leaked across redirect origin: {request}"
            );
            assert!(
                request.contains(&format!(
                    "\r\norigin: {}\r\n",
                    allowed_origin.to_ascii_lowercase()
                )),
                "redirected module request lost its initiating Origin header: {request}"
            );
            let body = r#"export const crossOriginRedirect = true;"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: {allowed_origin}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write cross-origin target");
        });

        let namespace = ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute_module(&format!("{origin}/start.js"))
            .unwrap();
        redirect_fixture.join().expect("redirect source completed");
        target_fixture.join().expect("redirect target completed");
        assert!(namespace.get_property("crossOriginRedirect").to_bool());
    }

    #[test]
    fn redirected_module_identity_uses_the_final_url() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let origin_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind redirect origin fixture");
        let origin_address = origin_listener
            .local_addr()
            .expect("redirect origin fixture address");
        let origin = format!("http://{origin_address}");

        let final_listener =
            TcpListener::bind("127.0.0.1:0").expect("bind redirected module fixture");
        let final_address = final_listener
            .local_addr()
            .expect("redirected module fixture address");
        let final_url = format!("http://{final_address}/modules/final.js");
        let allowed_origin = origin.clone();
        let final_fixture = thread::spawn(move || {
            let (mut stream, _) = final_listener
                .accept()
                .expect("accept redirected module request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = r#"callback(); export const finalUrl = import.meta.url;"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: {allowed_origin}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write redirected module");
        });

        let redirect_target = final_url.clone();
        let redirect_fixture = thread::spawn(move || {
            let (mut stream, _) = origin_listener
                .accept()
                .expect("accept initial module request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {redirect_target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .expect("write initial module redirect");
        });

        let evaluations = Rc::new(Cell::new(0));
        let callback_evaluations = Rc::clone(&evaluations);
        crate::jsdom::window_value().set_property(
            "callback",
            Value::function(move |_, _| {
                callback_evaluations.set(callback_evaluations.get() + 1);
                Value::Undefined
            }),
        );
        let requested_url = format!("{origin}/start.js");
        let loader = ScriptLoader::new(ScriptPolicy::default());
        let namespace = loader.load_and_execute_module(&requested_url).unwrap();
        redirect_fixture.join().expect("origin fixture completed");
        final_fixture.join().expect("redirect fixture completed");
        assert_eq!(namespace.get_property("finalUrl").to_js_string(), final_url);
        assert!(w3cos_core::module_registry::contains(&requested_url));
        assert!(w3cos_core::module_registry::contains(&final_url));
        w3cos_core::module_registry::evaluate(&requested_url).unwrap();
        w3cos_core::module_registry::evaluate(&final_url).unwrap();
        assert_eq!(
            evaluations.get(),
            1,
            "redirect request and final URL must share one evaluation record"
        );
    }

    #[test]
    fn same_origin_module_graph_uses_the_url_matched_cookie_store() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cookie module fixture");
        let address = listener
            .local_addr()
            .expect("cookie module fixture address");
        let fixture = thread::spawn(move || {
            let (mut main_stream, _) = listener.accept().expect("accept main module request");
            let mut main_request = [0_u8; 2048];
            let main_read = main_stream
                .read(&mut main_request)
                .expect("read main module request");
            let main_request = String::from_utf8_lossy(&main_request[..main_read]);
            assert!(!main_request.to_ascii_lowercase().contains("\r\ncookie:"));
            let main_body =
                r#"import { value } from "./dependency.js"; export const answer = value + 1;"#;
            let main_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nSet-Cookie: session=module; Path=/; SameSite=Strict; HttpOnly\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                main_body.len(),
                main_body
            );
            main_stream
                .write_all(main_response.as_bytes())
                .expect("write main module response");

            let (mut dependency_stream, _) =
                listener.accept().expect("accept dependency module request");
            let mut dependency_request = [0_u8; 2048];
            let dependency_read = dependency_stream
                .read(&mut dependency_request)
                .expect("read dependency module request");
            let dependency_request =
                String::from_utf8_lossy(&dependency_request[..dependency_read]);
            assert!(
                dependency_request
                    .to_ascii_lowercase()
                    .contains("\r\ncookie: session=module\r\n"),
                "dependency did not receive the same-origin cookie: {dependency_request}"
            );
            let dependency_body = r#"export const value = 41;"#;
            let dependency_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                dependency_body.len(),
                dependency_body
            );
            dependency_stream
                .write_all(dependency_response.as_bytes())
                .expect("write dependency module response");
        });

        let origin = format!("http://{address}");
        let namespace = ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute_module(&format!("{origin}/main.js"))
            .unwrap();
        fixture.join().expect("cookie module fixture completed");
        assert_eq!(namespace.get_property("answer").to_u32(), 42);
        assert_eq!(
            crate::cookie_store_web::cookie_header_for_origin(&origin),
            "session=module"
        );
        crate::cookie_store_web::set_active_url(&format!("{origin}/index.html"));
        assert_eq!(crate::cookie_store_web::document_cookie(), "");
    }

    #[test]
    fn module_same_origin_credentials_do_not_send_cookies_cross_origin() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::set_cookie_assignment_for_origin(
            "https://app.example",
            "secret=token; Path=/",
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cross-origin cookie fixture");
        let address = listener
            .local_addr()
            .expect("cross-origin cookie fixture address");
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("accept cross-origin module request");
            let mut request = [0_u8; 2048];
            let read = stream
                .read(&mut request)
                .expect("read cross-origin module request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                !request.to_ascii_lowercase().contains("\r\ncookie:"),
                "same-origin credentials leaked a cookie cross-origin: {request}"
            );
            let body = r#"export const value = 42;"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: https://app.example\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write cross-origin module response");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://app.example/main.js",
                &format!(
                    r#"import {{ value }} from "http://{address}/dependency.js"; export const answer = value;"#
                ),
            )
            .unwrap();
        let namespace = loader
            .load_and_execute_module("https://app.example/main.js")
            .unwrap();
        fixture.join().expect("cross-origin fixture completed");
        assert_eq!(namespace.get_property("answer").to_u32(), 42);
    }

    #[test]
    fn credentialed_module_graph_sends_and_stores_cross_origin_cookies() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind credentialed module fixture");
        let address = listener
            .local_addr()
            .expect("credentialed module fixture address");
        let dependency_url = format!("http://{address}/dependency.js");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &dependency_url,
            "target=secret; Path=/",
            true,
        );
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept credentialed module");
            let request = read_http_request(&mut stream).to_ascii_lowercase();
            assert!(
                request.contains("\r\ncookie: target=secret\r\n"),
                "include credentials did not send the target cookie: {request}"
            );
            let body = r#"export const value = 42;"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: {SAME_SITE_CROSS_ORIGIN_PAGE}\r\nAccess-Control-Allow-Credentials: true\r\nSet-Cookie: refreshed=yes; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write credentialed module");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                &format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/main.js"),
                &format!(
                    r#"import {{ value }} from "{dependency_url}"; export const answer = value;"#
                ),
            )
            .unwrap();
        let namespace = loader
            .load_and_execute_module_with_credentials(
                &format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/main.js"),
                ModuleCredentialsMode::Include,
            )
            .unwrap();
        fixture.join().expect("credentialed fixture completed");
        assert_eq!(namespace.get_property("answer").to_u32(), 42);
        let stored = crate::cookie_store_web::cookie_header_for_url(&dependency_url);
        assert!(stored.contains("target=secret"));
        assert!(stored.contains("refreshed=yes"));
    }

    #[test]
    fn cross_site_include_suppresses_lax_and_strict_module_cookies() {
        crate::cookie_store_web::reset();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cross-site cookie fixture");
        let address = listener.local_addr().expect("cross-site cookie address");
        let module_url = format!("http://{address}/dependency.js");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &module_url,
            "lax=blocked; Path=/; SameSite=Lax",
            true,
        );
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &module_url,
            "strict=blocked; Path=/; SameSite=Strict",
            true,
        );
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept cross-site module");
            let request = read_http_request(&mut stream).to_ascii_lowercase();
            assert!(
                !request.contains("\r\ncookie:"),
                "cross-site subresource sent Lax/Strict cookies: {request}"
            );
            let body = r#"export const value = 42;"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: https://app.example\r\nAccess-Control-Allow-Credentials: true\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write cross-site module");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://app.example/main.js",
                &format!(r#"import {{ value }} from "{module_url}"; export const answer = value;"#),
            )
            .unwrap();
        let namespace = loader
            .load_and_execute_module_with_credentials(
                "https://app.example/main.js",
                ModuleCredentialsMode::Include,
            )
            .unwrap();
        fixture.join().expect("cross-site cookie fixture completed");
        assert_eq!(namespace.get_property("answer").to_u32(), 42);
    }

    #[test]
    fn omit_module_credentials_neither_sends_nor_stores_cookies() {
        crate::cookie_store_web::reset();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind omit module fixture");
        let address = listener.local_addr().expect("omit module fixture address");
        let module_url = format!("http://{address}/module.js");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &module_url,
            "existing=value; Path=/",
            true,
        );
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept omit module");
            let request = read_http_request(&mut stream).to_ascii_lowercase();
            assert!(
                !request.contains("\r\ncookie:"),
                "omit credentials sent a cookie: {request}"
            );
            let body = r#"export const value = 42;"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nSet-Cookie: ignored=yes; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write omit module");
        });

        let namespace = ScriptLoader::new(ScriptPolicy::default())
            .load_and_execute_module_with_credentials(&module_url, ModuleCredentialsMode::Omit)
            .unwrap();
        fixture.join().expect("omit fixture completed");
        assert_eq!(namespace.get_property("value").to_u32(), 42);
        assert_eq!(
            crate::cookie_store_web::cookie_header_for_url(&module_url),
            "existing=value"
        );
    }

    #[test]
    fn first_module_map_fetch_owns_credentials_for_shared_consumers() {
        crate::cookie_store_web::reset();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind shared credentials fixture");
        let address = listener
            .local_addr()
            .expect("shared credentials fixture address");
        let module_url = format!("http://{address}/shared.js");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &module_url,
            "shared=credential; Path=/",
            true,
        );
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept shared module");
            let request = read_http_request(&mut stream).to_ascii_lowercase();
            assert!(
                !request.contains("\r\ncookie:"),
                "the later include consumer replaced the first omit fetch: {request}"
            );
            let body = r#"export const value = 42;"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write shared module");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        let first = loader.load_and_execute_module_async_with_credentials(
            &module_url,
            ModuleCredentialsMode::Omit,
        );
        let second = loader.load_and_execute_module_async_with_credentials(
            &module_url,
            ModuleCredentialsMode::Include,
        );
        let first_namespace = loader
            .settle_module_evaluation(first, &module_url)
            .expect("first shared consumer");
        let second_namespace = loader
            .settle_module_evaluation(second, &module_url)
            .expect("second shared consumer");
        fixture.join().expect("shared fixture completed");
        assert_eq!(first_namespace.get_property("value").to_u32(), 42);
        assert_eq!(second_namespace.get_property("value").to_u32(), 42);
        assert_eq!(
            loader
                .inner
                .module_records
                .borrow()
                .get(&module_url)
                .expect("shared module record")
                .credentials_mode,
            ModuleCredentialsMode::Omit
        );
    }

    #[test]
    fn dynamic_import_inherits_the_parent_module_credentials_mode() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind credentialed dynamic import fixture");
        let address = listener
            .local_addr()
            .expect("credentialed dynamic import fixture address");
        let chunk_url = format!("http://{address}/chunk.js");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &chunk_url,
            "chunk=credential; Path=/",
            true,
        );
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept dynamic import");
            let request = read_http_request(&mut stream).to_ascii_lowercase();
            assert!(request.contains("\r\ncookie: chunk=credential\r\n"));
            let body = r#"export const status = "credentialed";"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: {SAME_SITE_CROSS_ORIGIN_PAGE}\r\nAccess-Control-Allow-Credentials: true\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write credentialed dynamic import");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                &format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/main.js"),
                &format!(
                    r#"
                        import("{chunk_url}").then((namespace) => {{
                            document.body.setAttribute(
                                "data-credentialed-import",
                                namespace.status
                            );
                        }});
                    "#
                ),
            )
            .unwrap();
        loader
            .load_and_execute_module_with_credentials(
                &format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/main.js"),
                ModuleCredentialsMode::Include,
            )
            .unwrap();
        fixture
            .join()
            .expect("credentialed dynamic import fixture completed");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method(
                    "getAttribute",
                    vec![Value::string("data-credentialed-import")],
                )
                .to_js_string(),
            "credentialed"
        );
    }

    #[test]
    fn credentialed_module_cors_rejects_wildcard_missing_or_invalid_allow_credentials() {
        for (allow_origin, allow_credentials) in [
            ("*", Some("true")),
            (SAME_SITE_CROSS_ORIGIN_PAGE, None),
            (SAME_SITE_CROSS_ORIGIN_PAGE, Some("TRUE")),
        ] {
            crate::cookie_store_web::reset();
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("bind credentialed CORS fixture");
            let address = listener
                .local_addr()
                .expect("credentialed CORS fixture address");
            let dependency_url = format!("http://{address}/dependency.js");
            crate::cookie_store_web::set_cookie_assignment_for_url(
                &dependency_url,
                "target=secret; Path=/",
                true,
            );
            let fixture = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept CORS module");
                let request = read_http_request(&mut stream).to_ascii_lowercase();
                assert!(request.contains("\r\ncookie: target=secret\r\n"));
                let credentials = allow_credentials.map_or_else(String::new, |value| {
                    format!("Access-Control-Allow-Credentials: {value}\r\n")
                });
                let body = r#"export const value = 42;"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: {allow_origin}\r\n{credentials}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write rejected credentialed CORS response");
            });

            let loader = ScriptLoader::new(ScriptPolicy::default());
            loader
                .register_module_source(
                    &format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/main.js"),
                    &format!(
                        r#"import {{ value }} from "{dependency_url}"; export const answer = value;"#
                    ),
                )
                .unwrap();
            let error = loader
                .load_and_execute_module_with_credentials(
                    &format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/main.js"),
                    ModuleCredentialsMode::Include,
                )
                .unwrap_err();
            fixture.join().expect("credentialed CORS fixture completed");
            assert!(error.to_string().contains("credentialed module CORS"));
        }
    }

    #[test]
    fn dom_module_crossorigin_use_credentials_selects_include_mode() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind DOM credentialed module fixture");
        let address = listener
            .local_addr()
            .expect("DOM credentialed module fixture address");
        let module_url = format!("http://{address}/module.js");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &module_url,
            "dom=credential; Path=/",
            true,
        );
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept DOM module");
            let request = read_http_request(&mut stream).to_ascii_lowercase();
            assert!(request.contains("\r\ncookie: dom=credential\r\n"));
            let body = r#"document.body.setAttribute("data-credentialed-module", "loaded");"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: {SAME_SITE_CROSS_ORIGIN_PAGE}\r\nAccess-Control-Allow-Credentials: true\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write DOM credentialed module");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.call_method(
            "setAttribute",
            vec![
                Value::string("crossorigin"),
                Value::string("use-credentials"),
            ],
        );
        script.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&module_url)],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);
        crate::jsdom::drain_microtasks();
        fixture.join().expect("DOM credentialed fixture completed");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(
            document
                .get_property("body")
                .call_method(
                    "getAttribute",
                    vec![Value::string("data-credentialed-module")],
                )
                .to_js_string(),
            "loaded"
        );
    }

    #[test]
    fn module_script_integrity_is_checked_per_initial_graph_consumer() {
        use base64::Engine as _;

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind module SRI fixture");
        let address = listener.local_addr().expect("module SRI fixture address");
        let module_url = format!("http://{address}/module.js");
        let source = r#"document.body.setAttribute("data-module-sri", "executed");"#;
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept module SRI request");
            let _ = read_http_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                source.len(),
                source
            )
            .expect("write module SRI response");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let errors = Rc::new(Cell::new(0_u32));

        let mismatched = document.call_method("createElement", vec![Value::string("script")]);
        mismatched.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        mismatched.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&module_url)],
        );
        mismatched.call_method(
            "setAttribute",
            vec![Value::string("integrity"), Value::string("sha384-AAAAAAAA")],
        );
        let error_count = Rc::clone(&errors);
        mismatched.set_property(
            "onerror",
            Value::function(move |_, _| {
                error_count.set(error_count.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![mismatched]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        fixture.join().expect("module SRI fixture completed");
        assert_eq!(errors.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-module-sri")]),
            Value::Null
        );

        let digest = ring::digest::digest(&ring::digest::SHA384, source.as_bytes());
        let integrity = format!(
            "sha384-{}",
            base64::engine::general_purpose::STANDARD.encode(digest.as_ref())
        );
        let matched = document.call_method("createElement", vec![Value::string("script")]);
        matched.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        matched.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&module_url)],
        );
        matched.call_method(
            "setAttribute",
            vec![Value::string("integrity"), Value::string(&integrity)],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![matched]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-module-sri")])
                .to_js_string(),
            "executed"
        );
    }

    #[test]
    fn dom_classic_crossorigin_modes_share_transport_but_partition_credentials() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::cookie_store_web::reset();
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind DOM classic credential fixture");
        let address = listener
            .local_addr()
            .expect("DOM classic credential fixture address");
        let script_url = format!("http://{address}/classic.js");
        crate::cookie_store_web::set_cookie_assignment_for_url(
            &script_url,
            "classic=credential; Path=/",
            true,
        );
        let fixture = thread::spawn(move || {
            for (index, expects_cookie) in [(1, true), (2, false), (3, true)] {
                let (mut stream, _) = listener.accept().expect("accept DOM classic script");
                let request = read_http_request(&mut stream).to_ascii_lowercase();
                assert_eq!(
                    request.contains("\r\ncookie: classic=credential"),
                    expects_cookie,
                    "unexpected classic-script credential mode for request {index}: {request}"
                );
                assert_eq!(
                    request.contains(&format!("\r\norigin: {}\r\n", SAME_SITE_CROSS_ORIGIN_PAGE)),
                    index != 1,
                    "unexpected classic-script CORS Origin header for request {index}: {request}"
                );
                let cors = match index {
                    1 => String::new(),
                    2 => format!("Access-Control-Allow-Origin: {SAME_SITE_CROSS_ORIGIN_PAGE}\r\n"),
                    _ => format!(
                        "Access-Control-Allow-Origin: {SAME_SITE_CROSS_ORIGIN_PAGE}\r\nAccess-Control-Allow-Credentials: true\r\n"
                    ),
                };
                let set_cookie = if index == 1 {
                    "Set-Cookie: refreshed=classic; Path=/\r\nSet-Cookie: companion=second; Path=/\r\n"
                } else {
                    ""
                };
                let body =
                    format!("document.body.setAttribute(\"data-classic-{index}\", \"loaded\");");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\n{cors}{set_cookie}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write DOM classic response");
            }
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("{SAME_SITE_CROSS_ORIGIN_PAGE}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        for (index, cross_origin) in [
            (1, None),
            (2, Some("anonymous")),
            (3, Some("use-credentials")),
        ] {
            let script = document.call_method("createElement", vec![Value::string("script")]);
            if let Some(cross_origin) = cross_origin {
                script.call_method(
                    "setAttribute",
                    vec![Value::string("crossorigin"), Value::string(cross_origin)],
                );
            }
            script.call_method(
                "setAttribute",
                vec![Value::string("src"), Value::string(&script_url)],
            );
            document
                .get_property("head")
                .call_method("appendChild", vec![script]);
            for _ in 0..10_000 {
                crate::jsdom::drain_microtasks();
                if !has_pending_script_fetches() {
                    break;
                }
                thread::yield_now();
            }
            assert_eq!(
                document.get_property("body").call_method(
                    "getAttribute",
                    vec![Value::string(&format!("data-classic-{index}"))],
                ),
                Value::string("loaded")
            );
        }
        fixture.join().expect("DOM classic fixture completed");
        let stored = crate::cookie_store_web::cookie_header_for_url(&script_url);
        assert!(stored.contains("refreshed=classic"));
        assert!(stored.contains("companion=second"));
    }

    #[test]
    fn classic_no_cors_honors_corp_while_cors_mode_uses_cors_permission() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind classic CORP fixture");
        let address = listener.local_addr().expect("classic CORP fixture address");
        let script_url = format!("http://{address}/classic.js");
        let fixture = thread::spawn(move || {
            for cors_mode in [false, true] {
                let (mut stream, _) = listener.accept().expect("accept classic CORP request");
                let request = read_http_request(&mut stream).to_ascii_lowercase();
                assert_eq!(
                    request.contains("\r\norigin: https://app.example\r\n"),
                    cors_mode
                );
                let cors = cors_mode
                    .then_some("Access-Control-Allow-Origin: https://app.example\r\n")
                    .unwrap_or_default();
                let body = r#"document.body.setAttribute("data-classic-corp", "executed");"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nCross-Origin-Resource-Policy: same-origin\r\n{cors}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write classic CORP response");
            }
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document("https://app.example/index.html")
            .unwrap();
        let document = crate::jsdom::document_value();
        let errors = Rc::new(Cell::new(0_u32));

        let blocked = document.call_method("createElement", vec![Value::string("script")]);
        blocked.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&script_url)],
        );
        let error_count = Rc::clone(&errors);
        blocked.set_property(
            "onerror",
            Value::function(move |_, _| {
                error_count.set(error_count.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![blocked]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(errors.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-classic-corp")]),
            Value::Null
        );

        let allowed = document.call_method("createElement", vec![Value::string("script")]);
        allowed.call_method(
            "setAttribute",
            vec![Value::string("crossorigin"), Value::string("anonymous")],
        );
        allowed.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&script_url)],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![allowed]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        fixture.join().expect("classic CORP fixture completed");
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-classic-corp")])
                .to_js_string(),
            "executed"
        );
    }

    #[test]
    fn classic_script_integrity_blocks_mismatch_and_accepts_match() {
        use base64::Engine as _;

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind classic SRI fixture");
        let address = listener.local_addr().expect("classic SRI fixture address");
        let script_url = format!("http://{address}/classic.js");
        let source = r#"document.body.setAttribute("data-classic-sri", "executed");"#;
        let fixture = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept classic SRI request");
                let _ = read_http_request(&mut stream);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    source.len(),
                    source
                )
                .expect("write classic SRI response");
            }
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let errors = Rc::new(Cell::new(0_u32));

        let mismatched = document.call_method("createElement", vec![Value::string("script")]);
        mismatched.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&script_url)],
        );
        mismatched.call_method(
            "setAttribute",
            vec![Value::string("integrity"), Value::string("sha384-AAAAAAAA")],
        );
        let error_count = Rc::clone(&errors);
        mismatched.set_property(
            "onerror",
            Value::function(move |_, _| {
                error_count.set(error_count.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![mismatched]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(errors.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-classic-sri")]),
            Value::Null
        );

        let digest = ring::digest::digest(&ring::digest::SHA384, source.as_bytes());
        let integrity = format!(
            "sha384-{}",
            base64::engine::general_purpose::STANDARD.encode(digest.as_ref())
        );
        let matched = document.call_method("createElement", vec![Value::string("script")]);
        matched.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(&script_url)],
        );
        matched.call_method(
            "setAttribute",
            vec![Value::string("integrity"), Value::string(&integrity)],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![matched]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        fixture.join().expect("classic SRI fixture completed");
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-classic-sri")])
                .to_js_string(),
            "executed"
        );
    }

    #[test]
    fn integrity_metadata_uses_the_strongest_supported_algorithm() {
        use base64::Engine as _;

        let source = b"const integrity = true;";
        let sha256 = ring::digest::digest(&ring::digest::SHA256, source);
        let correct_sha256 = base64::engine::general_purpose::STANDARD.encode(sha256.as_ref());
        assert!(
            check_integrity_metadata(
                source,
                &format!("unsupported-anything sha256-{correct_sha256}"),
                true
            )
            .is_ok()
        );
        assert!(
            check_integrity_metadata(
                source,
                &format!("sha256-{correct_sha256} sha512-AAAAAAAA"),
                true
            )
            .is_err()
        );
        assert!(check_integrity_metadata(source, "future-hash", false).is_ok());
        assert!(
            check_integrity_metadata(source, &format!("sha256-{correct_sha256}"), false).is_err()
        );
    }

    #[test]
    fn null_import_map_target_blocks_scope_fallback() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/global.js",
                r#"export const value = "global";"#,
            )
            .unwrap();
        loader
            .register_module_source(
                "https://example.test/blocked/main.js",
                r#"import { value } from "lib"; export const result = value;"#,
            )
            .unwrap();

        let document = crate::jsdom::document_value();
        let import_map = document.call_method("createElement", vec![Value::string("script")]);
        import_map.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("importmap")],
        );
        import_map.set_property(
            "textContent",
            Value::string(
                r#"{
                    "imports": {"lib": "/global.js"},
                    "scopes": {"/blocked/": {"lib": null}}
                }"#,
            ),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![import_map]);
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let error = loader
            .load_and_execute_module("https://example.test/blocked/main.js")
            .unwrap_err();
        assert!(error.to_string().contains("blocked module specifier"));
    }

    #[test]
    fn multiple_import_maps_merge_without_overriding_registered_entries() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        for (url, source) in [
            (
                "https://example.test/first.js",
                r#"export const value = "first";"#,
            ),
            (
                "https://example.test/second.js",
                r#"export const value = "second";"#,
            ),
            (
                "https://example.test/extra.js",
                r#"export const extra = "extra";"#,
            ),
        ] {
            loader.register_module_source(url, source).unwrap();
        }

        let document = crate::jsdom::document_value();
        for source in [
            r#"{"imports":{"lib":"/first.js"}}"#,
            r#"{"imports":{"lib":"/second.js","extra":"/extra.js"}}"#,
        ] {
            let import_map = document.call_method("createElement", vec![Value::string("script")]);
            import_map.call_method(
                "setAttribute",
                vec![Value::string("type"), Value::string("importmap")],
            );
            import_map.set_property("textContent", Value::string(source));
            document
                .get_property("head")
                .call_method("appendChild", vec![import_map]);
        }
        loader
            .attach_to_document("https://example.test/index.html")
            .unwrap();

        let namespace = loader
            .execute_module_source(
                r#"
                    import { value } from "lib";
                    import { extra } from "extra";
                    export const result = value + ":" + extra;
                "#,
                "https://example.test/main.js",
            )
            .unwrap();
        assert_eq!(
            namespace.get_property("result").to_js_string(),
            "first:extra"
        );
    }

    #[test]
    fn dynamic_import_resolves_to_the_cached_live_module_namespace() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .register_module_source(
                "https://example.test/chunk.js",
                r#"export const status = "chunk-ready";"#,
            )
            .unwrap();
        loader
            .execute_module_source(
                r#"
                    import("./chunk.js").then((namespace) => {
                        document.body.setAttribute(
                            "data-dynamic-import",
                            namespace.status
                        );
                    });
                "#,
                "https://example.test/main.js",
            )
            .unwrap();

        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-dynamic-import")])
                .to_js_string(),
            "chunk-ready"
        );
    }

    #[test]
    fn external_module_graph_fetch_is_non_blocking_and_deduplicated() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind module fixture");
        let address = listener.local_addr().expect("module fixture address");
        let fixture = thread::spawn(move || {
            let mut paths = Vec::new();
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept module request");
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).expect("read module request");
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .expect("module request path")
                    .to_string();
                paths.push(path.clone());
                if request_index == 0 {
                    thread::sleep(std::time::Duration::from_millis(100));
                }
                let body = match path.as_str() {
                    "/main.js" => {
                        r#"import { value } from "./dependency.js"; export const answer = value + 1;"#
                    }
                    "/dependency.js" => r#"export const value = 41;"#,
                    _ => panic!("unexpected module request: {path}"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write module response");
            }
            paths
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        let url = format!("http://{address}/main.js");
        let started = std::time::Instant::now();
        let first = loader.load_and_execute_module_async(&url);
        let second = loader.load_and_execute_module_async(&url);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "module API blocked on network I/O"
        );
        assert!(matches!(
            w3cos_core::promise::status(&first),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while matches!(
            w3cos_core::promise::status(&first),
            Some(w3cos_core::promise::PromiseStatus::Pending)
        ) && std::time::Instant::now() < deadline
        {
            crate::jsdom::drain_microtasks();
            thread::yield_now();
        }
        crate::jsdom::drain_microtasks();
        let first_namespace = match w3cos_core::promise::status(&first) {
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(namespace)) => namespace,
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => {
                panic!("module graph rejected: {}", reason.to_js_string())
            }
            _ => panic!("module graph did not settle"),
        };
        let second_namespace = match w3cos_core::promise::status(&second) {
            Some(w3cos_core::promise::PromiseStatus::Fulfilled(namespace)) => namespace,
            _ => panic!("deduplicated module graph did not settle"),
        };
        assert_eq!(first_namespace.get_property("answer").to_u32(), 42);
        assert_eq!(second_namespace.get_property("answer").to_u32(), 42);
        assert_eq!(
            fixture.join().expect("module fixture completed"),
            vec!["/main.js", "/dependency.js"]
        );
    }

    #[test]
    fn module_element_referrer_policy_propagates_to_descendant_fetches() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind referrer fixture");
        let address = listener.local_addr().expect("referrer fixture address");
        let expected_referrer = format!("referer: http://{address}/");
        let fixture = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().expect("accept module request");
                let request = read_http_request(&mut stream);
                let request_lower = request.to_ascii_lowercase();
                assert!(
                    request_lower.contains(&expected_referrer),
                    "module request did not inherit the element origin policy: {request}"
                );
                assert!(
                    !request_lower.contains("/private/page.html?token=1"),
                    "origin policy leaked the document path: {request}"
                );
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .expect("module request path");
                let body = match path {
                    "/main.js" => {
                        r#"import { value } from "./dependency.js"; import("./chunk.js"); document.body.setAttribute("data-referrer-module", value);"#
                    }
                    "/dependency.js" => r#"export const value = 40;"#,
                    "/chunk.js" => r#"export const extra = 2;"#,
                    _ => panic!("unexpected module request: {path}"),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write module response");
            }
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!(
                "http://{address}/private/page.html?token=1#section"
            ))
            .unwrap();
        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string("/main.js")],
        );
        script.call_method(
            "setAttribute",
            vec![Value::string("referrerpolicy"), Value::string("origin")],
        );
        let loads = Rc::new(Cell::new(0_u32));
        let load_count = Rc::clone(&loads);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                load_count.set(load_count.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        fixture.join().expect("referrer fixture completed");
        crate::jsdom::drain_microtasks();
        assert_eq!(loads.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-referrer-module")])
                .to_js_string(),
            "40"
        );
    }

    #[test]
    fn classic_script_element_honors_no_referrer_policy() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind classic referrer fixture");
        let address = listener
            .local_addr()
            .expect("classic referrer fixture address");
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept classic request");
            let request = read_http_request(&mut stream);
            assert!(
                !request.to_ascii_lowercase().contains("referer:"),
                "classic no-referrer request leaked a Referer header: {request}"
            );
            let body = r#"document.body.setAttribute("data-classic-referrer", "suppressed");"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write classic response");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!(
                "http://{address}/private/page.html?token=1#section"
            ))
            .unwrap();
        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string("/classic.js")],
        );
        script.call_method(
            "setAttribute",
            vec![
                Value::string("referrerpolicy"),
                Value::string("no-referrer"),
            ],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        fixture.join().expect("classic referrer fixture completed");
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-classic-referrer")])
                .to_js_string(),
            "suppressed"
        );
    }

    #[test]
    fn removing_one_shared_dom_module_keeps_the_other_graph_consumer_live() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind shared DOM module fixture");
        let address = listener.local_addr().expect("shared DOM module address");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept shared module request");
            let _ = read_http_request(&mut stream);
            accepted_tx.send(()).expect("signal shared module request");
            release_rx.recv().expect("release shared module response");
            let body = r#"recordSharedModule(); export const ready = true;"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write shared module response");
        });

        let evaluations = Rc::new(Cell::new(0_u32));
        let observed = Rc::clone(&evaluations);
        crate::jsdom::window_value().set_property(
            "recordSharedModule",
            Value::function(move |_, _| {
                observed.set(observed.get() + 1);
                Value::Undefined
            }),
        );
        let first_loads = Rc::new(Cell::new(0_u32));
        let second_loads = Rc::new(Cell::new(0_u32));
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let head = document.get_property("head");
        let mut scripts = Vec::new();
        for loads in [Rc::clone(&first_loads), Rc::clone(&second_loads)] {
            let script = document.call_method("createElement", vec![Value::string("script")]);
            script.call_method(
                "setAttribute",
                vec![Value::string("type"), Value::string("module")],
            );
            script.call_method(
                "setAttribute",
                vec![Value::string("src"), Value::string("/shared-module.js")],
            );
            script.set_property(
                "onload",
                Value::function(move |_, _| {
                    loads.set(loads.get() + 1);
                    Value::Undefined
                }),
            );
            head.call_method("appendChild", vec![script.clone()]);
            scripts.push(script);
        }
        crate::jsdom::drain_microtasks();
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("shared module fetch started");
        head.call_method("removeChild", vec![scripts.remove(0)]);
        release_tx.send(()).expect("release shared module");
        fixture.join().expect("shared module fixture completed");
        for _ in 0..10_000 {
            crate::jsdom::drain_microtasks();
            if !has_pending_script_fetches() {
                break;
            }
            thread::yield_now();
        }
        crate::jsdom::drain_microtasks();

        assert_eq!(evaluations.get(), 1);
        assert_eq!(first_loads.get(), 0);
        assert_eq!(second_loads.get(), 1);
    }

    #[test]
    fn removing_last_dom_module_consumer_cancels_transport_without_poisoning_retry() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind orphan module fixture");
        let address = listener.local_addr().expect("orphan module address");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let fixture = thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept orphan module request");
                let _ = read_http_request(&mut stream);
                if request_index == 0 {
                    accepted_tx.send(()).expect("signal orphan module request");
                    release_rx.recv().expect("release orphan module response");
                }
                let body = r#"export const recovered = 42;"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write orphan module response");
            }
        });

        let loads = Rc::new(Cell::new(0_u32));
        let errors = Rc::new(Cell::new(0_u32));
        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let head = document.get_property("head");
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string("/orphan.js")],
        );
        let load_count = Rc::clone(&loads);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                load_count.set(load_count.get() + 1);
                Value::Undefined
            }),
        );
        let error_count = Rc::clone(&errors);
        script.set_property(
            "onerror",
            Value::function(move |_, _| {
                error_count.set(error_count.get() + 1);
                Value::Undefined
            }),
        );
        head.call_method("appendChild", vec![script.clone()]);
        crate::jsdom::drain_microtasks();
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("orphan module fetch started");

        head.call_method("removeChild", vec![script]);
        assert!(!has_pending_script_fetches());
        release_tx.send(()).expect("release cancelled module");
        let namespace = loader
            .load_and_execute_module(&format!("http://{address}/orphan.js"))
            .expect("fresh direct consumer retries cancelled graph");
        fixture.join().expect("orphan module fixture completed");
        crate::jsdom::drain_microtasks();

        assert_eq!(namespace.get_property("recovered").to_u32(), 42);
        assert_eq!(loads.get(), 0);
        assert_eq!(errors.get(), 0);
    }

    #[test]
    fn releasing_loader_cooperatively_cancels_pending_module_graph() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cancellation fixture");
        let address = listener.local_addr().expect("cancellation fixture address");
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept cancellation request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            accepted_tx.send(()).expect("signal cancellation request");
            thread::sleep(std::time::Duration::from_millis(50));
            let body = r#"export const late = true;"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        let evaluation =
            loader.load_and_execute_module_async(&format!("http://{address}/cancel.js"));
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("module request started before loader release");
        drop(loader);
        crate::jsdom::drain_microtasks();
        match w3cos_core::promise::status(&evaluation) {
            Some(w3cos_core::promise::PromiseStatus::Rejected(reason)) => assert!(
                reason.to_js_string().contains("cancelled"),
                "unexpected cancellation reason: {}",
                reason.to_js_string()
            ),
            _ => panic!("pending graph was not rejected when its loader was released"),
        }
        fixture.join().expect("cancellation fixture completed");
    }

    #[test]
    fn external_dom_module_load_event_waits_for_async_graph() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind DOM module fixture");
        let address = listener.local_addr().expect("DOM module fixture address");
        let fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept DOM module request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            thread::sleep(std::time::Duration::from_millis(50));
            let body = r#"document.body.setAttribute("data-external-module", "ready");"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write DOM module response");
        });

        let loader = ScriptLoader::new(ScriptPolicy::default());
        loader
            .attach_to_document(&format!("http://{address}/index.html"))
            .unwrap();
        let document = crate::jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        script.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("module")],
        );
        script.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string("/module.js")],
        );
        let load_count = Rc::new(Cell::new(0_u32));
        let loaded = Rc::clone(&load_count);
        script.set_property(
            "onload",
            Value::function(move |_, _| {
                loaded.set(loaded.get() + 1);
                Value::Undefined
            }),
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);

        crate::jsdom::drain_microtasks();
        assert_eq!(load_count.get(), 0);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-external-module")]),
            Value::Null
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while load_count.get() == 0 && std::time::Instant::now() < deadline {
            crate::jsdom::drain_microtasks();
            thread::yield_now();
        }
        assert_eq!(load_count.get(), 1);
        assert_eq!(
            document
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-external-module")])
                .to_js_string(),
            "ready"
        );
        fixture.join().expect("DOM module fixture completed");
    }

    #[test]
    fn import_meta_url_uses_the_canonical_module_record_url() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        ScriptLoader::new(ScriptPolicy::default())
            .execute_module_source(
                r#"
                    document.body.setAttribute("data-import-meta", import.meta.url);
                "#,
                "https://example.test/modules/meta.js",
            )
            .unwrap();
        assert_eq!(
            crate::jsdom::document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-import-meta")])
                .to_js_string(),
            "https://example.test/modules/meta.js"
        );
    }
}
