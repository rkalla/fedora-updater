//! Flatpak update discovery, native `FlatpakTransaction` apply, and progress parsing.
//!
//! Discovery uses libflatpak `list_installed_refs_for_update`, the same call
//! GNOME Software uses. That includes a ref whose own commit is current when a
//! related extension (for example the NVIDIA GL driver) still needs to be
//! installed. Apply prefers native `FlatpakTransaction` via `libflatpak.so.0`
//! (`FlatpakTransactionProgress` + `GCancellable`) and falls back transparently
//! to `flatpak update` CLI when transaction symbols are unavailable.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

use crate::model::{format_bytes, Package, PackageStatus, UpdateSource};
use crate::orchestrator::WorkCancel;

use super::{NativeOutcome, ProgressHint};

/// One ref libflatpak says should be passed to `flatpak update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakListedRef {
    /// `app/…` or `runtime/…` ref, as `flatpak update` accepts it.
    pub formatted_ref: String,
    pub name: String,
    pub arch: String,
    pub branch: String,
    pub version: String,
    pub origin: String,
    pub summary: Option<String>,
    pub installed_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum FlatpakOpKind {
    Install,
    Update,
    Uninstall,
}

/// One resolved transaction operation. `related_to` holds the parent refs that
/// caused a related extension or runtime to be pulled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakResolvedOp {
    pub formatted_ref: String,
    pub kind: FlatpakOpKind,
    pub download_bytes: u64,
    pub installed_size_bytes: u64,
    pub related_to: Vec<String>,
}

/// Result of a user or system Flatpak check.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatpakCheck {
    pub packages: Vec<Package>,
    pub warning: Option<String>,
}

fn clean_opt_text(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

/// Build the review rows for one installation.
///
/// Every listed ref becomes a row, because those are the refs `flatpak update`
/// must be given. A shared extension is named on each parent and its download
/// size is attached to the earliest listed parent only, so the review total
/// counts the bytes once.
pub fn packages_from_flatpak_updates(
    source: UpdateSource,
    refs: &[FlatpakListedRef],
    ops: &[FlatpakResolvedOp],
) -> Vec<Package> {
    let scope = match source {
        UpdateSource::FlatpakUser => "user",
        UpdateSource::FlatpakSystem => "system",
        _ => "flatpak",
    };
    let mut requested: HashMap<&str, usize> = HashMap::with_capacity(refs.len());
    for (i, r) in refs.iter().enumerate() {
        if !r.formatted_ref.is_empty() {
            requested.entry(r.formatted_ref.as_str()).or_insert(i);
        }
    }

    let mut installs_for: HashMap<usize, Vec<String>> = HashMap::new();
    let mut bytes_for: HashMap<usize, u64> = HashMap::new();
    let mut new_installed_bytes_for: HashMap<usize, u64> = HashMap::new();

    for op in ops {
        if op.kind == FlatpakOpKind::Uninstall {
            continue;
        }
        if let Some(&index) = requested.get(op.formatted_ref.as_str()) {
            *bytes_for.entry(index).or_default() += op.download_bytes;
            if op.installed_size_bytes > 0 {
                *new_installed_bytes_for.entry(index).or_default() += op.installed_size_bytes;
            }
            continue;
        }
        let Some(ext_id) = ref_id(&op.formatted_ref) else {
            continue;
        };
        let mut parents: Vec<usize> = op
            .related_to
            .iter()
            .filter_map(|r| requested.get(r.as_str()).copied())
            .collect();
        parents.sort_unstable();
        parents.dedup();
        if parents.is_empty() {
            continue;
        }
        for (n, index) in parents.iter().copied().enumerate() {
            installs_for
                .entry(index)
                .or_default()
                .push(ext_id.to_string());
            if n == 0 {
                *bytes_for.entry(index).or_default() += op.download_bytes;
            }
        }
    }

    let mut packages = Vec::with_capacity(refs.len());
    let mut seen = std::collections::HashSet::new();
    for (index, r) in refs.iter().enumerate() {
        if r.formatted_ref.is_empty() || !seen.insert(r.formatted_ref.clone()) {
            continue;
        }
        let version = if r.version.is_empty() {
            r.branch.clone()
        } else {
            r.version.clone()
        };
        let name = if r.name.is_empty() {
            ref_id(&r.formatted_ref)
                .unwrap_or(r.formatted_ref.as_str())
                .to_string()
        } else {
            r.name.clone()
        };
        let mut installs = installs_for.remove(&index).unwrap_or_default();
        installs.sort();
        installs.dedup();
        let bytes = bytes_for.get(&index).copied().unwrap_or(0);
        let install_size_delta = match (
            r.installed_size_bytes,
            new_installed_bytes_for.get(&index).copied().filter(|&v| v > 0),
        ) {
            (Some(old_sz), Some(new_sz)) => Some(
                (new_sz as i128 - old_sz as i128)
                    .clamp(i64::MIN as i128, i64::MAX as i128) as i64,
            ),
            _ => None,
        };
        let detail = detail_line(&version, &r.origin, scope, &installs);
        packages.push(Package {
            id: format!("flatpak:{scope}:{}", r.formatted_ref),
            name,
            arch: r.arch.clone(),
            version,
            repo: if r.origin.is_empty() {
                "flatpak".into()
            } else {
                r.origin.clone()
            },
            size_bytes: (bytes > 0).then_some(bytes),
            old_version: None,
            status: PackageStatus::Pending,
            source,
            detail,
            summary: clean_opt_text(r.summary.as_deref()),
            install_size_delta,
            advisories: Vec::new(),
            preflight_warning: None,
        });
    }
    packages
}

fn ref_id(formatted_ref: &str) -> Option<&str> {
    let mut parts = formatted_ref.split('/');
    let kind = parts.next()?;
    let id = parts.next()?;
    if kind.is_empty() || id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn detail_line(version: &str, origin: &str, scope: &str, installs: &[String]) -> String {
    let mut bits = Vec::new();
    if !version.is_empty() {
        bits.push(version.to_string());
    }
    if !origin.is_empty() {
        bits.push(origin.to_string());
    }
    bits.push(scope.to_string());
    if !installs.is_empty() {
        bits.push(format!("installs {}", installs.join(", ")));
    }
    bits.join(" · ")
}

/// Ask libflatpak which installed refs need an update, including related
/// extensions, and turn that into review rows.
pub fn query_updates(user: bool) -> Result<FlatpakCheck, String> {
    let source = if user {
        UpdateSource::FlatpakUser
    } else {
        UpdateSource::FlatpakSystem
    };
    let (refs, ops, warning) = unsafe { query_raw(user)? };
    Ok(FlatpakCheck {
        packages: packages_from_flatpak_updates(source, &refs, &ops),
        warning,
    })
}

type RawQueryResult = (
    Vec<FlatpakListedRef>,
    Vec<FlatpakResolvedOp>,
    Option<String>,
);

struct OwnedGObj(*mut glib::gobject_ffi::GObject);

impl OwnedGObj {
    unsafe fn from_raw<T>(ptr: *mut T) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr as *mut glib::gobject_ffi::GObject))
        }
    }

    fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }
}

impl Drop for OwnedGObj {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { glib::gobject_ffi::g_object_unref(self.0) };
        }
    }
}

struct OwnedGPtrArray(*mut glib::ffi::GPtrArray);

impl OwnedGPtrArray {
    fn from_raw(ptr: *mut glib::ffi::GPtrArray) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr))
        }
    }

    fn as_ptr(&self) -> *mut glib::ffi::GPtrArray {
        self.0
    }
}

impl Drop for OwnedGPtrArray {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { glib::ffi::g_ptr_array_unref(self.0) };
        }
    }
}

struct ReadyCapture {
    ops: Vec<FlatpakResolvedOp>,
    saw_ready: bool,
    cancellable: *mut gtk::gio::ffi::GCancellable,
    api: &'static FlatpakApi,
}

/// Resolve `list_installed_refs_for_update`, then abort a transaction at
/// `ready-pre-auth` so the related installs are known without downloading.
///
/// # Safety
/// Caller must not hold a pointer into the Flatpak objects after return.
unsafe fn query_raw(user: bool) -> Result<RawQueryResult, String> {
    let api = flatpak_api()?;
    let mut error: *mut glib::ffi::GError = std::ptr::null_mut();
    let inst_ptr = if user {
        (api.new_user)(std::ptr::null_mut(), &mut error)
    } else {
        (api.new_system)(std::ptr::null_mut(), &mut error)
    };
    let installation = OwnedGObj::from_raw(inst_ptr).ok_or_else(|| take_error(error))?;

    let refs = {
        let listed_ptr =
            (api.list_for_update)(installation.as_ptr(), std::ptr::null_mut(), &mut error);
        let listed = OwnedGPtrArray::from_raw(listed_ptr).ok_or_else(|| take_error(error))?;
        listed_refs_from_array(api, listed.as_ptr())
    };
    if refs.is_empty() {
        return Ok((Vec::new(), Vec::new(), None));
    }

    let tx_ptr = (api.new_transaction)(installation.as_ptr(), std::ptr::null_mut(), &mut error);
    let transaction = OwnedGObj::from_raw(tx_ptr).ok_or_else(|| take_error(error))?;
    (api.set_no_interaction)(transaction.as_ptr(), glib::ffi::GTRUE);

    let mut added = 0usize;
    let mut add_error: Option<String> = None;
    for r in &refs {
        let Ok(cref) = CString::new(r.formatted_ref.as_str()) else {
            continue;
        };
        // add_update(transaction, ref, subpaths, commit, error). NULL commit
        // means the latest commit on the installed remote.
        let ok = (api.add_update)(
            transaction.as_ptr(),
            cref.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut error,
        );
        if ok == glib::ffi::GFALSE {
            add_error = Some(take_error(error));
            error = std::ptr::null_mut();
        } else {
            added += 1;
        }
    }

    let cancellable_ptr = gtk::gio::ffi::g_cancellable_new();
    let cancellable = OwnedGObj::from_raw(cancellable_ptr)
        .ok_or_else(|| "g_cancellable_new returned null".to_string())?;
    let mut capture = ReadyCapture {
        ops: Vec::new(),
        saw_ready: false,
        cancellable: cancellable.as_ptr(),
        api,
    };
    if added > 0 {
        let handler_id = glib::gobject_ffi::g_signal_connect_data(
            transaction.as_ptr(),
            c"ready-pre-auth".as_ptr(),
            Some(std::mem::transmute::<
                unsafe extern "C" fn(*mut c_void, glib::ffi::gpointer) -> glib::ffi::gboolean,
                unsafe extern "C" fn(),
            >(
                on_ready_pre_auth
                    as unsafe extern "C" fn(
                        *mut c_void,
                        glib::ffi::gpointer,
                    ) -> glib::ffi::gboolean,
            )),
            (&mut capture) as *mut ReadyCapture as glib::ffi::gpointer,
            None,
            0,
        );
        let ran = (api.run)(transaction.as_ptr(), cancellable.as_ptr(), &mut error);
        if handler_id != 0 {
            glib::gobject_ffi::g_signal_handler_disconnect(transaction.as_ptr(), handler_id);
        }
        if ran == glib::ffi::GTRUE {
            // The ready handler failed to abort. Stop anything still in flight.
            gtk::gio::ffi::g_cancellable_cancel(cancellable.as_ptr());
            add_error =
                Some("flatpak transaction continued past ready-pre-auth during check".into());
            if !error.is_null() {
                glib::ffi::g_error_free(error);
            }
        } else if !capture.saw_ready {
            let message = take_error(error);
            if add_error.is_none() {
                add_error = Some(message);
            }
        } else if !error.is_null() {
            // Expected abort after the operation list was captured.
            glib::ffi::g_error_free(error);
        }
    }

    Ok((refs, capture.ops, add_error))
}

unsafe extern "C" fn on_ready_pre_auth(
    transaction: *mut c_void,
    data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    if data.is_null() || transaction.is_null() {
        return glib::ffi::GFALSE;
    }
    let capture = &mut *(data as *mut ReadyCapture);
    capture.saw_ready = true;
    capture.ops = collect_ops(capture.api, transaction);
    if !capture.cancellable.is_null() {
        gtk::gio::ffi::g_cancellable_cancel(capture.cancellable);
    }
    glib::ffi::GFALSE
}

unsafe fn collect_ops(api: &FlatpakApi, transaction: *mut c_void) -> Vec<FlatpakResolvedOp> {
    let list = (api.get_operations)(transaction as *mut FlatpakTransaction);
    let mut out = Vec::new();
    let mut link = list;
    while !link.is_null() {
        let op = (*link).data as *mut FlatpakTransactionOperation;
        if !op.is_null() {
            let kind = op_kind_from_raw((api.get_operation_type)(op));
            let related_to = read_related_to(api, op);
            out.push(FlatpakResolvedOp {
                formatted_ref: c_to_string((api.get_ref)(op)),
                kind,
                download_bytes: (api.get_download_size)(op),
                installed_size_bytes: api.op_installed_size.map_or(0, |f| f(op)),
                related_to,
            });
            glib::gobject_ffi::g_object_unref(op as *mut glib::gobject_ffi::GObject);
        }
        link = (*link).next;
    }
    if !list.is_null() {
        glib::ffi::g_list_free(list);
    }
    out
}

fn op_kind_from_raw(raw: c_int) -> FlatpakOpKind {
    match raw {
        0 | 2 => FlatpakOpKind::Install,
        1 => FlatpakOpKind::Update,
        _ => FlatpakOpKind::Uninstall,
    }
}

unsafe fn read_related_to(api: &FlatpakApi, op: *mut FlatpakTransactionOperation) -> Vec<String> {
    let mut related_to = Vec::new();
    let related = (api.get_related_to_ops)(op);
    if !related.is_null() && !(*related).pdata.is_null() {
        for i in 0..(*related).len {
            let parent = *(*related).pdata.add(i as usize) as *mut FlatpakTransactionOperation;
            if parent.is_null() {
                continue;
            }
            let r = c_to_string((api.get_ref)(parent));
            if !r.is_empty() {
                related_to.push(r);
            }
        }
    }
    related_to
}

unsafe fn listed_refs_from_array(
    api: &FlatpakApi,
    listed: *mut glib::ffi::GPtrArray,
) -> Vec<FlatpakListedRef> {
    let mut refs = Vec::new();
    if listed.is_null() || (*listed).pdata.is_null() {
        return refs;
    }
    for i in 0..(*listed).len {
        let iref = *(*listed).pdata.add(i as usize) as *mut FlatpakInstalledRef;
        if iref.is_null() {
            continue;
        }
        let formatted = owned_full((api.format_ref)(iref as *mut FlatpakRef));
        if formatted.is_empty() {
            continue;
        }
        let mut name = c_to_string((api.appdata_name)(iref));
        if name.is_empty() {
            name = ref_id(&formatted).unwrap_or(&formatted).to_string();
        }
        let summary = api
            .appdata_summary
            .and_then(|f| clean_opt_text(Some(c_to_string(f(iref)).as_str())));
        let installed_size_bytes = api
            .ref_installed_size
            .map(|f| f(iref))
            .filter(|&sz| sz > 0);
        refs.push(FlatpakListedRef {
            arch: c_to_string((api.get_arch)(iref as *mut FlatpakRef)),
            branch: c_to_string((api.get_branch)(iref as *mut FlatpakRef)),
            version: c_to_string((api.appdata_version)(iref)),
            origin: c_to_string((api.get_origin)(iref)),
            name,
            formatted_ref: formatted,
            summary,
            installed_size_bytes,
        });
    }
    refs
}

unsafe fn take_error(error: *mut glib::ffi::GError) -> String {
    if error.is_null() {
        return "unknown libflatpak error".into();
    }
    let message = c_to_string((*error).message);
    glib::ffi::g_error_free(error);
    if message.is_empty() {
        "unknown libflatpak error".into()
    } else {
        message
    }
}

/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated C string for the duration of the call.
unsafe fn c_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

unsafe fn owned_full(ptr: *mut c_char) -> String {
    let text = c_to_string(ptr);
    if !ptr.is_null() {
        glib::ffi::g_free(ptr as *mut c_void);
    }
    text
}

#[repr(C)]
struct FlatpakInstallation {
    _private: [u8; 0],
}
#[repr(C)]
struct FlatpakRef {
    _private: [u8; 0],
}
#[repr(C)]
struct FlatpakInstalledRef {
    _private: [u8; 0],
}
#[repr(C)]
struct FlatpakTransaction {
    _private: [u8; 0],
}
#[repr(C)]
struct FlatpakTransactionOperation {
    _private: [u8; 0],
}
#[repr(C)]
struct FlatpakTransactionProgress {
    _private: [u8; 0],
}

type ErrSlot = *mut *mut glib::ffi::GError;
type Cancel = *mut gtk::gio::ffi::GCancellable;

struct FlatpakApplyApi {
    is_empty: unsafe extern "C" fn(*mut FlatpakTransaction) -> glib::ffi::gboolean,
    op_is_skipped: unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> glib::ffi::gboolean,
    op_get_remote: unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> *const c_char,
    progress_set_update_frequency: unsafe extern "C" fn(*mut FlatpakTransactionProgress, u32),
    progress_get_status: unsafe extern "C" fn(*mut FlatpakTransactionProgress) -> *mut c_char,
    progress_get_is_estimating:
        unsafe extern "C" fn(*mut FlatpakTransactionProgress) -> glib::ffi::gboolean,
    progress_get_progress: unsafe extern "C" fn(*mut FlatpakTransactionProgress) -> c_int,
    progress_get_bytes_transferred: unsafe extern "C" fn(*mut FlatpakTransactionProgress) -> u64,
    progress_get_start_time: unsafe extern "C" fn(*mut FlatpakTransactionProgress) -> u64,
}

struct FlatpakApi {
    _keep_open: usize,
    new_system: unsafe extern "C" fn(Cancel, ErrSlot) -> *mut FlatpakInstallation,
    new_user: unsafe extern "C" fn(Cancel, ErrSlot) -> *mut FlatpakInstallation,
    list_for_update: unsafe extern "C" fn(
        *mut FlatpakInstallation,
        Cancel,
        ErrSlot,
    ) -> *mut glib::ffi::GPtrArray,
    format_ref: unsafe extern "C" fn(*mut FlatpakRef) -> *mut c_char,
    get_arch: unsafe extern "C" fn(*mut FlatpakRef) -> *const c_char,
    get_branch: unsafe extern "C" fn(*mut FlatpakRef) -> *const c_char,
    get_origin: unsafe extern "C" fn(*mut FlatpakInstalledRef) -> *const c_char,
    appdata_name: unsafe extern "C" fn(*mut FlatpakInstalledRef) -> *const c_char,
    appdata_summary: Option<unsafe extern "C" fn(*mut FlatpakInstalledRef) -> *const c_char>,
    appdata_version: unsafe extern "C" fn(*mut FlatpakInstalledRef) -> *const c_char,
    ref_installed_size: Option<unsafe extern "C" fn(*mut FlatpakInstalledRef) -> u64>,
    new_transaction:
        unsafe extern "C" fn(*mut FlatpakInstallation, Cancel, ErrSlot) -> *mut FlatpakTransaction,
    add_update: unsafe extern "C" fn(
        *mut FlatpakTransaction,
        *const c_char,
        *mut *const c_char,
        *const c_char,
        ErrSlot,
    ) -> glib::ffi::gboolean,
    set_no_interaction: unsafe extern "C" fn(*mut FlatpakTransaction, glib::ffi::gboolean),
    run: unsafe extern "C" fn(*mut FlatpakTransaction, Cancel, ErrSlot) -> glib::ffi::gboolean,
    get_operations: unsafe extern "C" fn(*mut FlatpakTransaction) -> *mut glib::ffi::GList,
    get_operation_type: unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> c_int,
    get_ref: unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> *const c_char,
    get_download_size: unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> u64,
    op_installed_size: Option<unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> u64>,
    get_related_to_ops:
        unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> *mut glib::ffi::GPtrArray,
    apply: Result<FlatpakApplyApi, String>,
}

fn flatpak_api() -> Result<&'static FlatpakApi, String> {
    static API: OnceLock<Result<FlatpakApi, String>> = OnceLock::new();
    match API.get_or_init(|| unsafe { load_flatpak_api() }) {
        Ok(api) => Ok(api),
        Err(message) => Err(message.clone()),
    }
}

unsafe fn load_flatpak_api() -> Result<FlatpakApi, String> {
    let handle = libc::dlopen(c"libflatpak.so.0".as_ptr(), libc::RTLD_NOW);
    if handle.is_null() {
        return Err(format!("libflatpak not available ({})", dl_error()));
    }
    let apply = load_apply_api(handle);
    Ok(FlatpakApi {
        _keep_open: handle as usize,
        new_system: load_fn(handle, c"flatpak_installation_new_system")?,
        new_user: load_fn(handle, c"flatpak_installation_new_user")?,
        list_for_update: load_fn(
            handle,
            c"flatpak_installation_list_installed_refs_for_update",
        )?,
        format_ref: load_fn(handle, c"flatpak_ref_format_ref")?,
        get_arch: load_fn(handle, c"flatpak_ref_get_arch")?,
        get_branch: load_fn(handle, c"flatpak_ref_get_branch")?,
        get_origin: load_fn(handle, c"flatpak_installed_ref_get_origin")?,
        appdata_name: load_fn(handle, c"flatpak_installed_ref_get_appdata_name")?,
        appdata_summary: load_opt_fn(handle, c"flatpak_installed_ref_get_appdata_summary"),
        appdata_version: load_fn(handle, c"flatpak_installed_ref_get_appdata_version")?,
        ref_installed_size: load_opt_fn(handle, c"flatpak_installed_ref_get_installed_size"),
        new_transaction: load_fn(handle, c"flatpak_transaction_new_for_installation")?,
        add_update: load_fn(handle, c"flatpak_transaction_add_update")?,
        set_no_interaction: load_fn(handle, c"flatpak_transaction_set_no_interaction")?,
        run: load_fn(handle, c"flatpak_transaction_run")?,
        get_operations: load_fn(handle, c"flatpak_transaction_get_operations")?,
        get_operation_type: load_fn(handle, c"flatpak_transaction_operation_get_operation_type")?,
        get_ref: load_fn(handle, c"flatpak_transaction_operation_get_ref")?,
        get_download_size: load_fn(handle, c"flatpak_transaction_operation_get_download_size")?,
        op_installed_size: load_opt_fn(handle, c"flatpak_transaction_operation_get_installed_size"),
        get_related_to_ops: load_fn(handle, c"flatpak_transaction_operation_get_related_to_ops")?,
        apply,
    })
}

unsafe fn load_apply_api(handle: *mut c_void) -> Result<FlatpakApplyApi, String> {
    Ok(FlatpakApplyApi {
        is_empty: load_fn(handle, c"flatpak_transaction_is_empty")?,
        op_is_skipped: load_fn(handle, c"flatpak_transaction_operation_get_is_skipped")?,
        op_get_remote: load_fn(handle, c"flatpak_transaction_operation_get_remote")?,
        progress_set_update_frequency: load_fn(
            handle,
            c"flatpak_transaction_progress_set_update_frequency",
        )?,
        progress_get_status: load_fn(handle, c"flatpak_transaction_progress_get_status")?,
        progress_get_is_estimating: load_fn(
            handle,
            c"flatpak_transaction_progress_get_is_estimating",
        )?,
        progress_get_progress: load_fn(handle, c"flatpak_transaction_progress_get_progress")?,
        progress_get_bytes_transferred: load_fn(
            handle,
            c"flatpak_transaction_progress_get_bytes_transferred",
        )?,
        progress_get_start_time: load_fn(handle, c"flatpak_transaction_progress_get_start_time")?,
    })
}

unsafe fn load_opt_fn<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
    load_fn(handle, name).ok()
}

unsafe fn load_fn<T: Copy>(handle: *mut c_void, name: &CStr) -> Result<T, String> {
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    libc::dlerror();
    let sym = libc::dlsym(handle, name.as_ptr());
    if sym.is_null() {
        return Err(format!(
            "libflatpak is missing {} ({})",
            name.to_string_lossy(),
            dl_error()
        ));
    }
    Ok(std::mem::transmute_copy(&sym))
}

fn dl_error() -> String {
    let message = unsafe { c_to_string(libc::dlerror()) };
    if message.is_empty() {
        "unknown dlopen error".into()
    } else {
        message
    }
}

/// Bitflag on `FlatpakTransaction::operation-error` `details` (`FLATPAK_TRANSACTION_ERROR_DETAILS_NON_FATAL`).
pub const FLATPAK_TRANSACTION_ERROR_DETAILS_NON_FATAL: u32 = 1;
/// Bitflag on `FlatpakTransaction::operation-done` `result` (`FLATPAK_TRANSACTION_RESULT_NO_CHANGE`).
pub const FLATPAK_TRANSACTION_RESULT_NO_CHANGE: u32 = 1;

/// Thread-safe handle wrapping a `GIO` `GCancellable` for stopping in-flight `flatpak_transaction_run`.
#[derive(Debug)]
pub struct FlatpakCancellable(*mut gtk::gio::ffi::GCancellable);

// `GCancellable` methods (`g_cancellable_cancel`, `g_cancellable_is_cancelled`) and GObject ref/unref are thread-safe.
unsafe impl Send for FlatpakCancellable {}
unsafe impl Sync for FlatpakCancellable {}

impl FlatpakCancellable {
    pub fn new() -> Result<Self, String> {
        let ptr = unsafe { gtk::gio::ffi::g_cancellable_new() };
        if ptr.is_null() {
            Err("g_cancellable_new returned null".into())
        } else {
            Ok(Self(ptr))
        }
    }

    pub fn cancel(&self) {
        if !self.0.is_null() {
            unsafe { gtk::gio::ffi::g_cancellable_cancel(self.0) };
        }
    }

    pub fn is_cancelled(&self) -> bool {
        !self.0.is_null()
            && unsafe { gtk::gio::ffi::g_cancellable_is_cancelled(self.0) != glib::ffi::GFALSE }
    }

    fn as_ptr(&self) -> *mut gtk::gio::ffi::GCancellable {
        self.0
    }
}

impl Clone for FlatpakCancellable {
    fn clone(&self) -> Self {
        if !self.0.is_null() {
            unsafe { glib::gobject_ffi::g_object_ref(self.0 as *mut glib::gobject_ffi::GObject) };
        }
        Self(self.0)
    }
}

impl Drop for FlatpakCancellable {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { glib::gobject_ffi::g_object_unref(self.0 as *mut glib::gobject_ffi::GObject) };
        }
    }
}

static ACTIVE_CANCELLABLES: Mutex<Vec<(u64, FlatpakCancellable)>> = Mutex::new(Vec::new());
static NEXT_CANCELLABLE_ID: AtomicU64 = AtomicU64::new(1);

struct ActiveCancellableGuard(u64);

impl ActiveCancellableGuard {
    fn register(cancellable: &FlatpakCancellable) -> Self {
        let id = NEXT_CANCELLABLE_ID.fetch_add(1, Ordering::SeqCst);
        let mut guard = ACTIVE_CANCELLABLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.push((id, cancellable.clone()));
        Self(id)
    }
}

impl Drop for ActiveCancellableGuard {
    fn drop(&mut self) {
        let mut guard = ACTIVE_CANCELLABLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.retain(|(id, _)| *id != self.0);
    }
}

/// Cancel all active native `FlatpakTransaction` instances.
/// Safe to invoke from the GTK main thread when the user clicks Stop or closes the window.
pub fn cancel_active_transaction() {
    let snapshot: Vec<FlatpakCancellable> = {
        let guard = ACTIVE_CANCELLABLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.iter().map(|(_, c)| c.clone()).collect()
    };
    for c in snapshot {
        c.cancel();
    }
}

/// Download throughput measurement computed from `FlatpakTransactionProgress`.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatpakThroughputSample {
    pub bytes_transferred: u64,
    pub elapsed_secs: f64,
    pub bytes_per_sec: f64,
}

/// Snapshot of a `FlatpakTransactionOperation` used both natively and in offline tests.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FlatpakOperationMeta {
    pub formatted_ref: String,
    pub kind: FlatpakOpKind,
    #[serde(default)]
    pub remote: String,
    #[serde(default)]
    pub download_bytes: u64,
    #[serde(default)]
    pub installed_size_bytes: u64,
    #[serde(default)]
    pub related_to: Vec<String>,
    #[serde(default)]
    pub is_skipped: bool,
}

/// Snapshot of a `FlatpakTransactionProgress` emission used both natively and in offline tests.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FlatpakProgressSnapshot {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub progress_pct: i32,
    #[serde(default)]
    pub bytes_transferred: u64,
    #[serde(default)]
    pub is_estimating: bool,
    #[serde(default)]
    pub start_time_us: u64,
}

/// Structured telemetry/log delta emitted by [`FlatpakTransactionTracker`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlatpakApplyEvent {
    pub console_line: Option<String>,
    pub console_draw: Option<String>,
    pub hint: Option<ProgressHint>,
    pub throughput: Option<FlatpakThroughputSample>,
}

/// Result summary from executing a native `FlatpakTransaction`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlatpakApplyOutcome {
    pub completed_ops: u32,
    pub any_changed: bool,
    pub throughput_samples: Vec<FlatpakThroughputSample>,
}

/// Compute average download throughput when elapsed time is at least `0.1s`.
pub fn compute_throughput(
    bytes_transferred: u64,
    start_time_us: u64,
    now_us: u64,
) -> Option<FlatpakThroughputSample> {
    if bytes_transferred == 0 || now_us <= start_time_us {
        return None;
    }
    let delta_us = now_us - start_time_us;
    if delta_us < 100_000 {
        return None;
    }
    let elapsed_secs = delta_us as f64 / 1_000_000.0;
    let bytes_per_sec = bytes_transferred as f64 / elapsed_secs;
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return None;
    }
    Some(FlatpakThroughputSample {
        bytes_transferred,
        elapsed_secs,
        bytes_per_sec,
    })
}

/// Pure, offline-testable state machine mapping `FlatpakTransaction` signals into
/// structured [`ProgressHint`] and console events.
#[derive(Debug, Clone)]
pub struct FlatpakTransactionTracker {
    requested_refs: Vec<String>,
    op_parent: HashMap<String, String>,
    pending_ops_per_parent: HashMap<String, u32>,
    total_ops: u32,
    current_op_index: u32,
    completed_ops: u32,
    any_changed: bool,
    active_op: Option<FlatpakOperationMeta>,
    last_emitted_pct: Option<i32>,
    last_emitted_status: String,
    last_emitted_bytes: u64,
    errors: Vec<String>,
    fatal_error: bool,
    throughput_samples: Vec<FlatpakThroughputSample>,
}

impl FlatpakTransactionTracker {
    pub fn new(requested_refs: Vec<String>) -> Self {
        Self {
            requested_refs,
            op_parent: HashMap::new(),
            pending_ops_per_parent: HashMap::new(),
            total_ops: 0,
            current_op_index: 0,
            completed_ops: 0,
            any_changed: false,
            active_op: None,
            last_emitted_pct: None,
            last_emitted_status: String::new(),
            last_emitted_bytes: 0,
            errors: Vec::new(),
            fatal_error: false,
            throughput_samples: Vec::new(),
        }
    }

    pub fn total_ops(&self) -> u32 {
        self.total_ops
    }

    pub fn completed_ops(&self) -> u32 {
        self.completed_ops
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    fn resolve_parent_ref<'a>(&'a self, op: &'a FlatpakOperationMeta) -> Option<&'a str> {
        if let Some(mapped) = self.op_parent.get(&op.formatted_ref) {
            return Some(mapped.as_str());
        }
        if let Some(req) = self
            .requested_refs
            .iter()
            .find(|r| r.as_str() == op.formatted_ref.as_str())
        {
            return Some(req.as_str());
        }
        for req in &self.requested_refs {
            if op.related_to.iter().any(|rel| rel == req) {
                return Some(req.as_str());
            }
        }
        (!op.formatted_ref.is_empty()).then_some(op.formatted_ref.as_str())
    }

    fn package_hint_for_op(&self, op: &FlatpakOperationMeta) -> Option<String> {
        let target_ref = self.resolve_parent_ref(op)?;
        Some(ref_id(target_ref).unwrap_or(target_ref).to_string())
    }

    pub fn on_ready(&mut self, ops: Vec<FlatpakOperationMeta>) -> Vec<FlatpakApplyEvent> {
        self.op_parent.clear();
        self.pending_ops_per_parent.clear();
        let mut active_count = 0u32;

        for op in &ops {
            if op.is_skipped || op.kind == FlatpakOpKind::Uninstall {
                continue;
            }
            active_count += 1;
            let parent = self
                .requested_refs
                .iter()
                .find(|r| r.as_str() == op.formatted_ref.as_str())
                .or_else(|| {
                    self.requested_refs
                        .iter()
                        .find(|req| op.related_to.iter().any(|rel| rel == *req))
                })
                .cloned();
            if let Some(parent_ref) = parent {
                self.op_parent
                    .insert(op.formatted_ref.clone(), parent_ref.clone());
                *self.pending_ops_per_parent.entry(parent_ref).or_default() += 1;
            }
        }
        self.total_ops = active_count;

        if active_count == 0 {
            return Vec::new();
        }
        let total_download: u64 = ops
            .iter()
            .filter(|op| !op.is_skipped && op.kind != FlatpakOpKind::Uninstall)
            .map(|op| op.download_bytes)
            .sum();
        let msg = if total_download > 0 {
            format!(
                "Prepared {active_count} Flatpak operation(s) ({} download)",
                format_bytes(total_download)
            )
        } else {
            format!("Prepared {active_count} Flatpak operation(s)")
        };
        vec![FlatpakApplyEvent {
            console_line: Some(msg),
            ..Default::default()
        }]
    }

    pub fn on_new_operation(&mut self, op: FlatpakOperationMeta) -> FlatpakApplyEvent {
        self.last_emitted_pct = None;
        self.last_emitted_status.clear();
        self.last_emitted_bytes = 0;

        if op.is_skipped {
            let op_id = ref_id(&op.formatted_ref).unwrap_or(op.formatted_ref.as_str());
            return FlatpakApplyEvent {
                console_line: Some(format!("Skipping {op_id}")),
                ..Default::default()
            };
        }

        self.current_op_index += 1;
        let stage_total = self.total_ops.max(self.current_op_index);
        let op_id = ref_id(&op.formatted_ref)
            .unwrap_or(op.formatted_ref.as_str())
            .to_string();
        let parent_hint = self.package_hint_for_op(&op);
        let verb = match op.kind {
            FlatpakOpKind::Install => "Installing",
            FlatpakOpKind::Update => "Updating",
            FlatpakOpKind::Uninstall => "Removing",
        };
        let parent_suffix = match parent_hint.as_deref() {
            Some(parent_id) if parent_id != op_id => format!(" (for {parent_id})"),
            _ => String::new(),
        };
        let size_suffix = if op.download_bytes > 0 {
            format!(" ({})", format_bytes(op.download_bytes))
        } else {
            String::new()
        };
        let line = format!(
            "[{}/{}] {verb} {op_id}{parent_suffix}{size_suffix}",
            self.current_op_index, stage_total
        );
        let (status, phase_label) = if op.download_bytes > 0 && op.kind != FlatpakOpKind::Uninstall
        {
            (PackageStatus::Downloading, "Downloading Flatpak")
        } else {
            (PackageStatus::Installing, "Updating Flatpak")
        };

        self.active_op = Some(op);
        FlatpakApplyEvent {
            console_line: Some(line),
            console_draw: None,
            hint: Some(ProgressHint {
                package_name: parent_hint,
                status: Some(status),
                progress: Some(0.0),
                phase_label: Some(phase_label.into()),
                stage_current: Some(self.current_op_index),
                stage_total: Some(stage_total),
            }),
            throughput: None,
        }
    }

    pub fn on_operation_progress(
        &mut self,
        snap: &FlatpakProgressSnapshot,
        now_us: u64,
    ) -> Option<FlatpakApplyEvent> {
        const BYTE_STEP_THRESHOLD: u64 = 256 * 1024;
        let pct = snap.progress_pct.clamp(0, 100);
        let trimmed_status = snap.status.trim();
        let bytes_stepped = snap.bytes_transferred.saturating_sub(self.last_emitted_bytes)
            >= BYTE_STEP_THRESHOLD;

        if self.last_emitted_pct == Some(pct)
            && self.last_emitted_status == trimmed_status
            && !bytes_stepped
            && pct != 100
        {
            return None;
        }

        self.last_emitted_pct = Some(pct);
        self.last_emitted_status.clear();
        self.last_emitted_status.push_str(trimmed_status);
        self.last_emitted_bytes = snap.bytes_transferred;

        let active = self.active_op.as_ref();
        let package_name = active.and_then(|op| self.package_hint_for_op(op));
        let op_label = active
            .map(|op| {
                ref_id(&op.formatted_ref)
                    .unwrap_or(op.formatted_ref.as_str())
                    .to_string()
            })
            .unwrap_or_else(|| "Flatpak".into());

        let (status, phase_label) = classify_progress_stage(
            trimmed_status,
            pct,
            snap.bytes_transferred,
            active.map_or(0, |op| op.download_bytes),
        );

        let throughput = if status == PackageStatus::Downloading {
            compute_throughput(snap.bytes_transferred, snap.start_time_us, now_us)
        } else {
            None
        };
        if let Some(ref sample) = throughput {
            self.throughput_samples.push(sample.clone());
        }

        let draw = format_progress_draw(
            &op_label,
            trimmed_status,
            pct,
            snap.bytes_transferred,
            snap.is_estimating,
            throughput.as_ref(),
        );
        let stage_total = (self.total_ops > 0).then(|| self.total_ops.max(self.current_op_index));
        let stage_current = stage_total.map(|_| self.current_op_index.max(1));

        Some(FlatpakApplyEvent {
            console_line: None,
            console_draw: Some(draw),
            hint: Some(ProgressHint {
                package_name,
                status: Some(status),
                progress: Some((f64::from(pct) / 100.0).clamp(0.0, 1.0)),
                phase_label: Some(phase_label.into()),
                stage_current,
                stage_total,
            }),
            throughput,
        })
    }

    pub fn on_operation_done(
        &mut self,
        op: &FlatpakOperationMeta,
        result_flags: u32,
    ) -> FlatpakApplyEvent {
        self.active_op = None;
        if op.is_skipped {
            return FlatpakApplyEvent::default();
        }
        self.completed_ops += 1;
        let changed = (result_flags & FLATPAK_TRANSACTION_RESULT_NO_CHANGE) == 0;
        if changed {
            self.any_changed = true;
        }

        let op_id = ref_id(&op.formatted_ref).unwrap_or(op.formatted_ref.as_str());
        let parent_ref = self.resolve_parent_ref(op).map(ToString::to_string);
        let parent_finished = parent_ref.as_deref().is_some_and(|p| {
            if let Some(remaining) = self.pending_ops_per_parent.get_mut(p) {
                *remaining = remaining.saturating_sub(1);
                *remaining == 0
            } else {
                p == op.formatted_ref
            }
        });
        let parent_hint = parent_ref
            .as_deref()
            .map(|p| ref_id(p).unwrap_or(p).to_string());

        let line = if changed {
            match op.kind {
                FlatpakOpKind::Install => format!("Installed {op_id}"),
                FlatpakOpKind::Update => format!("Updated {op_id}"),
                FlatpakOpKind::Uninstall => format!("Removed {op_id}"),
            }
        } else {
            format!("Up to date: {op_id}")
        };

        let hint = if parent_finished && op.kind != FlatpakOpKind::Uninstall {
            Some(ProgressHint {
                package_name: parent_hint,
                status: Some(PackageStatus::Completed),
                progress: Some(1.0),
                phase_label: Some("Updating Flatpak".into()),
                stage_current: (self.total_ops > 0).then(|| self.completed_ops.min(self.total_ops)),
                stage_total: (self.total_ops > 0).then_some(self.total_ops),
            })
        } else {
            None
        };

        FlatpakApplyEvent {
            console_line: Some(line),
            console_draw: None,
            hint,
            throughput: None,
        }
    }

    pub fn on_operation_error(
        &mut self,
        op: &FlatpakOperationMeta,
        message: &str,
        details_flags: u32,
        cancelled: bool,
    ) -> (FlatpakApplyEvent, bool) {
        self.active_op = None;
        let non_fatal = (details_flags & FLATPAK_TRANSACTION_ERROR_DETAILS_NON_FATAL) != 0;
        if !non_fatal {
            self.fatal_error = true;
        }
        let op_id = ref_id(&op.formatted_ref).unwrap_or(op.formatted_ref.as_str());
        let clean_msg = message.trim();
        let detail = if clean_msg.is_empty() {
            format!("Failed to update {op_id}")
        } else {
            format!("Failed to update {op_id}: {clean_msg}")
        };
        if !cancelled {
            self.errors.push(detail.clone());
        }

        let parent_ref = self.resolve_parent_ref(op).map(ToString::to_string);
        if let Some(p) = parent_ref.as_deref() {
            if let Some(remaining) = self.pending_ops_per_parent.get_mut(p) {
                *remaining = remaining.saturating_sub(1);
            }
        }
        let parent_hint = parent_ref
            .as_deref()
            .map(|p| ref_id(p).unwrap_or(p).to_string());

        let ev = FlatpakApplyEvent {
            console_line: Some(format!("error: {detail}")),
            console_draw: None,
            hint: (!non_fatal).then(|| ProgressHint {
                package_name: parent_hint,
                status: Some(PackageStatus::Failed),
                progress: None,
                phase_label: Some("Updating Flatpak".into()),
                stage_current: None,
                stage_total: None,
            }),
            throughput: None,
        };
        let continue_tx = non_fatal && !cancelled;
        (ev, continue_tx)
    }

    pub fn finish_result(
        &self,
        run_ok: bool,
        run_err: Option<String>,
        cancelled: bool,
    ) -> Result<FlatpakApplyOutcome, String> {
        if cancelled {
            return Err("cancelled".into());
        }
        if !run_ok || self.fatal_error || !self.errors.is_empty() {
            if !self.errors.is_empty() {
                return Err(self.errors.join("; "));
            }
            return Err(run_err.unwrap_or_else(|| "Flatpak transaction failed".into()));
        }
        Ok(FlatpakApplyOutcome {
            completed_ops: self.completed_ops,
            any_changed: self.any_changed,
            throughput_samples: self.throughput_samples.clone(),
        })
    }
}

fn classify_progress_stage(
    status: &str,
    pct: i32,
    bytes_transferred: u64,
    expected_download_bytes: u64,
) -> (PackageStatus, &'static str) {
    let lower = status.to_ascii_lowercase();
    if lower.contains("deploy")
        || lower.contains("install")
        || lower.contains("updat")
        || lower.contains("export")
        || lower.contains("commit")
        || lower.contains("extract")
        || lower.contains("finish")
    {
        return (PackageStatus::Installing, "Updating Flatpak");
    }
    if lower.contains("download")
        || lower.contains("pull")
        || lower.contains("fetch")
        || (pct < 100 && (bytes_transferred > 0 || expected_download_bytes > 0))
    {
        return (PackageStatus::Downloading, "Downloading Flatpak");
    }
    if pct >= 100 {
        (PackageStatus::Installing, "Updating Flatpak")
    } else {
        (PackageStatus::Downloading, "Downloading Flatpak")
    }
}

fn format_progress_draw(
    op_label: &str,
    status: &str,
    pct: i32,
    bytes_transferred: u64,
    is_estimating: bool,
    throughput: Option<&FlatpakThroughputSample>,
) -> String {
    let pct_str = if is_estimating {
        format!("{pct}%…")
    } else {
        format!("{pct}%")
    };
    let head = if status.is_empty() {
        format!("{op_label} {pct_str}")
    } else {
        format!("{status} {op_label} {pct_str}")
    };
    match (bytes_transferred > 0, throughput) {
        (true, Some(tp)) => format!(
            "{head} ({} at {}/s)",
            format_bytes(bytes_transferred),
            format_bytes(tp.bytes_per_sec.round() as u64)
        ),
        (true, None) => format!("{head} ({})", format_bytes(bytes_transferred)),
        (false, _) => head,
    }
}

struct TxRunContext<'a> {
    api: &'static FlatpakApi,
    apply_api: &'static FlatpakApplyApi,
    transaction: *mut FlatpakTransaction,
    cancellable: FlatpakCancellable,
    tracker: FlatpakTransactionTracker,
    on_line: &'a mut dyn FnMut(String),
    on_draw: &'a mut dyn FnMut(String),
    on_progress: &'a mut dyn FnMut(ProgressHint),
    active_progress: Option<(*mut FlatpakTransactionProgress, libc::c_ulong)>,
}

impl TxRunContext<'_> {
    fn emit_event(&mut self, ev: FlatpakApplyEvent) {
        if let Some(line) = ev.console_line {
            (self.on_line)(line);
        }
        if let Some(draw) = ev.console_draw {
            (self.on_draw)(draw);
        }
        if let Some(hint) = ev.hint {
            (self.on_progress)(hint);
        }
    }

    unsafe fn detach_active_progress(&mut self) {
        if let Some((progress_ptr, handler_id)) = self.active_progress.take() {
            if !progress_ptr.is_null() {
                if handler_id != 0 {
                    glib::gobject_ffi::g_signal_handler_disconnect(
                        progress_ptr as *mut glib::gobject_ffi::GObject,
                        handler_id,
                    );
                }
                glib::gobject_ffi::g_object_unref(progress_ptr as *mut glib::gobject_ffi::GObject);
            }
        }
    }
}

/// Execute native `FlatpakTransaction` updates via `libflatpak.so.0`.
///
/// Returns `NativeOutcome::Unavailable` whenever `libflatpak.so.0` or its
/// transaction symbols are missing so the orchestrator falls back cleanly to CLI.
pub fn apply_updates(
    user: bool,
    refs: &[String],
    cancel: &WorkCancel,
    on_line: &mut dyn FnMut(String),
    on_draw: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
) -> NativeOutcome<FlatpakApplyOutcome> {
    if cancel.is_cancelled() {
        return NativeOutcome::Handled(Err("cancelled".into()));
    }
    let api = match flatpak_api() {
        Ok(api) => api,
        Err(reason) => return NativeOutcome::Unavailable(reason),
    };
    let apply_api = match &api.apply {
        Ok(apply_api) => apply_api,
        Err(reason) => return NativeOutcome::Unavailable(reason.clone()),
    };
    if refs.is_empty() {
        return NativeOutcome::Handled(Ok(FlatpakApplyOutcome::default()));
    }

    unsafe {
        let mut error: *mut glib::ffi::GError = std::ptr::null_mut();
        let inst_ptr = if user {
            (api.new_user)(std::ptr::null_mut(), &mut error)
        } else {
            (api.new_system)(std::ptr::null_mut(), &mut error)
        };
        let Some(installation) = OwnedGObj::from_raw(inst_ptr) else {
            return NativeOutcome::Unavailable(take_error(error));
        };

        let tx_ptr = (api.new_transaction)(installation.as_ptr(), std::ptr::null_mut(), &mut error);
        let Some(transaction) = OwnedGObj::from_raw(tx_ptr) else {
            return NativeOutcome::Unavailable(take_error(error));
        };
        (api.set_no_interaction)(transaction.as_ptr(), glib::ffi::GTRUE);

        let mut added = 0usize;
        let mut first_add_err: Option<String> = None;
        for r in refs {
            let Ok(cref) = CString::new(r.as_str()) else {
                continue;
            };
            let ok = (api.add_update)(
                transaction.as_ptr(),
                cref.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null(),
                &mut error,
            );
            if ok == glib::ffi::GFALSE {
                let msg = take_error(error);
                error = std::ptr::null_mut();
                if first_add_err.is_none() {
                    first_add_err = Some(msg);
                }
            } else {
                added += 1;
            }
        }
        if added == 0 {
            return NativeOutcome::Handled(Err(first_add_err
                .unwrap_or_else(|| "No valid Flatpak refs could be queued for update".into())));
        }
        if (apply_api.is_empty)(transaction.as_ptr()) != glib::ffi::GFALSE {
            on_line("Nothing to update.".into());
            return NativeOutcome::Handled(Ok(FlatpakApplyOutcome::default()));
        }

        let cancellable = match FlatpakCancellable::new() {
            Ok(c) => c,
            Err(e) => return NativeOutcome::Unavailable(e),
        };
        let _active_guard = ActiveCancellableGuard::register(&cancellable);
        let cancel_hook_target = cancellable.clone();
        cancel.on_cancel(move || {
            cancel_hook_target.cancel();
        });
        if cancel.is_cancelled() || cancellable.is_cancelled() {
            return NativeOutcome::Handled(Err("cancelled".into()));
        }

        let mut ctx = TxRunContext {
            api,
            apply_api,
            transaction: transaction.as_ptr(),
            cancellable: cancellable.clone(),
            tracker: FlatpakTransactionTracker::new(refs.to_vec()),
            on_line,
            on_draw,
            on_progress,
            active_progress: None,
        };
        let ctx_ptr = (&mut ctx as *mut TxRunContext<'_>) as glib::ffi::gpointer;

        let h_ready = connect_raw_signal(
            transaction.as_ptr(),
            c"ready",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(*mut c_void, glib::ffi::gpointer) -> glib::ffi::gboolean,
                unsafe extern "C" fn(),
            >(on_apply_ready)),
            ctx_ptr,
        );
        let h_new_op = connect_raw_signal(
            transaction.as_ptr(),
            c"new-operation",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut FlatpakTransactionOperation,
                    *mut FlatpakTransactionProgress,
                    glib::ffi::gpointer,
                ),
                unsafe extern "C" fn(),
            >(on_new_operation)),
            ctx_ptr,
        );
        let h_op_done = connect_raw_signal(
            transaction.as_ptr(),
            c"operation-done",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut FlatpakTransactionOperation,
                    *const c_char,
                    u32,
                    glib::ffi::gpointer,
                ),
                unsafe extern "C" fn(),
            >(on_operation_done)),
            ctx_ptr,
        );
        let h_op_error = connect_raw_signal(
            transaction.as_ptr(),
            c"operation-error",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut FlatpakTransactionOperation,
                    *const glib::ffi::GError,
                    u32,
                    glib::ffi::gpointer,
                ) -> glib::ffi::gboolean,
                unsafe extern "C" fn(),
            >(on_operation_error)),
            ctx_ptr,
        );

        let ran = (api.run)(transaction.as_ptr(), cancellable.as_ptr(), &mut error);
        for hid in [h_ready, h_new_op, h_op_done, h_op_error] {
            if hid != 0 {
                glib::gobject_ffi::g_signal_handler_disconnect(transaction.as_ptr(), hid);
            }
        }
        ctx.detach_active_progress();

        let run_ok = ran != glib::ffi::GFALSE;
        let run_err = if error.is_null() {
            None
        } else {
            Some(take_error(error))
        };
        let was_cancelled = cancel.is_cancelled() || cancellable.is_cancelled();
        NativeOutcome::Handled(ctx.tracker.finish_result(run_ok, run_err, was_cancelled))
    }
}

unsafe fn connect_raw_signal(
    instance: *mut glib::gobject_ffi::GObject,
    signal_name: &CStr,
    callback: Option<unsafe extern "C" fn()>,
    data: glib::ffi::gpointer,
) -> libc::c_ulong {
    glib::gobject_ffi::g_signal_connect_data(
        instance,
        signal_name.as_ptr(),
        callback,
        data,
        None,
        0,
    )
}

unsafe fn collect_ops_meta(
    api: &FlatpakApi,
    apply_api: &FlatpakApplyApi,
    transaction: *mut FlatpakTransaction,
) -> Vec<FlatpakOperationMeta> {
    let list = (api.get_operations)(transaction);
    let mut out = Vec::new();
    let mut link = list;
    while !link.is_null() {
        let op = (*link).data as *mut FlatpakTransactionOperation;
        if !op.is_null() {
            out.push(read_op_meta(api, apply_api, op));
            glib::gobject_ffi::g_object_unref(op as *mut glib::gobject_ffi::GObject);
        }
        link = (*link).next;
    }
    if !list.is_null() {
        glib::ffi::g_list_free(list);
    }
    out
}

unsafe fn read_op_meta(
    api: &FlatpakApi,
    apply_api: &FlatpakApplyApi,
    op: *mut FlatpakTransactionOperation,
) -> FlatpakOperationMeta {
    FlatpakOperationMeta {
        formatted_ref: c_to_string((api.get_ref)(op)),
        kind: op_kind_from_raw((api.get_operation_type)(op)),
        remote: c_to_string((apply_api.op_get_remote)(op)),
        download_bytes: (api.get_download_size)(op),
        installed_size_bytes: api.op_installed_size.map_or(0, |f| f(op)),
        related_to: read_related_to(api, op),
        is_skipped: (apply_api.op_is_skipped)(op) != glib::ffi::GFALSE,
    }
}

unsafe extern "C" fn on_apply_ready(
    transaction: *mut c_void,
    data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    if data.is_null() || transaction.is_null() {
        return glib::ffi::GFALSE;
    }
    let ctx = &mut *(data as *mut TxRunContext<'_>);
    if ctx.cancellable.is_cancelled() {
        return glib::ffi::GFALSE;
    }
    let ops = collect_ops_meta(ctx.api, ctx.apply_api, ctx.transaction);
    let events = ctx.tracker.on_ready(ops);
    for ev in events {
        ctx.emit_event(ev);
    }
    glib::ffi::GTRUE
}

unsafe extern "C" fn on_new_operation(
    _transaction: *mut c_void,
    operation: *mut FlatpakTransactionOperation,
    progress: *mut FlatpakTransactionProgress,
    data: glib::ffi::gpointer,
) {
    if data.is_null() || operation.is_null() {
        return;
    }
    let ctx = &mut *(data as *mut TxRunContext<'_>);
    ctx.detach_active_progress();

    let meta = read_op_meta(ctx.api, ctx.apply_api, operation);
    let skipped = meta.is_skipped;
    let ev = ctx.tracker.on_new_operation(meta);
    ctx.emit_event(ev);

    if !skipped && !progress.is_null() {
        (ctx.apply_api.progress_set_update_frequency)(progress, 250);
        glib::gobject_ffi::g_object_ref(progress as *mut glib::gobject_ffi::GObject);
        let hid = connect_raw_signal(
            progress as *mut glib::gobject_ffi::GObject,
            c"changed",
            Some(std::mem::transmute::<
                unsafe extern "C" fn(*mut FlatpakTransactionProgress, glib::ffi::gpointer),
                unsafe extern "C" fn(),
            >(on_progress_changed)),
            data,
        );
        ctx.active_progress = Some((progress, hid));
    }
}

unsafe extern "C" fn on_progress_changed(
    progress: *mut FlatpakTransactionProgress,
    data: glib::ffi::gpointer,
) {
    if data.is_null() || progress.is_null() {
        return;
    }
    let ctx = &mut *(data as *mut TxRunContext<'_>);
    if ctx.cancellable.is_cancelled() {
        return;
    }
    let snap = FlatpakProgressSnapshot {
        status: owned_full((ctx.apply_api.progress_get_status)(progress)),
        progress_pct: (ctx.apply_api.progress_get_progress)(progress),
        bytes_transferred: (ctx.apply_api.progress_get_bytes_transferred)(progress),
        is_estimating: (ctx.apply_api.progress_get_is_estimating)(progress) != glib::ffi::GFALSE,
        start_time_us: (ctx.apply_api.progress_get_start_time)(progress),
    };
    let now_us = glib::monotonic_time().max(0) as u64;
    if let Some(ev) = ctx.tracker.on_operation_progress(&snap, now_us) {
        ctx.emit_event(ev);
    }
}

unsafe extern "C" fn on_operation_done(
    _transaction: *mut c_void,
    operation: *mut FlatpakTransactionOperation,
    _commit: *const c_char,
    result_flags: u32,
    data: glib::ffi::gpointer,
) {
    if data.is_null() || operation.is_null() {
        return;
    }
    let ctx = &mut *(data as *mut TxRunContext<'_>);
    ctx.detach_active_progress();
    let meta = read_op_meta(ctx.api, ctx.apply_api, operation);
    let ev = ctx.tracker.on_operation_done(&meta, result_flags);
    ctx.emit_event(ev);
}

unsafe extern "C" fn on_operation_error(
    _transaction: *mut c_void,
    operation: *mut FlatpakTransactionOperation,
    error: *const glib::ffi::GError,
    details_flags: u32,
    data: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    if data.is_null() || operation.is_null() {
        return glib::ffi::GFALSE;
    }
    let ctx = &mut *(data as *mut TxRunContext<'_>);
    ctx.detach_active_progress();
    let meta = read_op_meta(ctx.api, ctx.apply_api, operation);
    let msg = if error.is_null() {
        String::new()
    } else {
        c_to_string((*error).message)
    };
    let cancelled = ctx.cancellable.is_cancelled();
    let (ev, continue_tx) = ctx
        .tracker
        .on_operation_error(&meta, &msg, details_flags, cancelled);
    ctx.emit_event(ev);
    if continue_tx {
        glib::ffi::GTRUE
    } else {
        glib::ffi::GFALSE
    }
}

/// Extract the Flatpak ref to pass to `flatpak update` from a package id.
pub fn update_ref_from_package(p: &Package) -> Option<String> {
    // flatpak:user:app/org.foo/x86_64/stable
    let rest = p.id.strip_prefix("flatpak:")?;
    let (_scope, rref) = rest.split_once(':')?;
    if rref.is_empty() {
        None
    } else {
        Some(rref.to_string())
    }
}

/// Build `flatpak update` argv for a set of refs (same installation).
pub fn update_args_for_refs(user: bool, refs: &[String]) -> Vec<String> {
    let mut args = vec!["update".into()];
    if user {
        args.push("--user".into());
    } else {
        args.push("--system".into());
    }
    args.push("-y".into());
    args.push("--noninteractive".into());
    for r in refs {
        args.push(r.clone());
    }
    args
}

fn looks_like_app_id(s: &str) -> bool {
    if s.contains('/') {
        return s
            .split('/')
            .next()
            .map(|p| p == "app" || p == "runtime" || looks_like_app_id(p))
            .unwrap_or(false);
    }
    !s.ends_with('%')
        && s.contains('.')
        && !s.contains(' ')
        && s.len() >= 3
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && s.split('.').all(|seg| !seg.is_empty())
}

pub fn parse_progress_line(line: &str) -> ProgressHint {
    let lower = line.to_ascii_lowercase();
    let mut hint = ProgressHint::default();

    if lower.trim_start().starts_with("error:") {
        return hint;
    }
    if lower.contains("nothing to update") || lower.contains("no updates") {
        hint.phase_label = Some("Nothing to update".into());
        return hint;
    }
    if lower.contains("updating") || lower.contains("installing") {
        hint.status = Some(PackageStatus::Installing);
        hint.phase_label = Some("Updating Flatpak".into());
    }
    if lower.contains("downloading") {
        hint.status = Some(PackageStatus::Downloading);
        hint.phase_label = Some("Downloading Flatpak".into());
    }

    for token in line.split_whitespace() {
        let trimmed = token.trim_matches(|c| matches!(c, ':' | ',' | ';' | '(' | ')' | '"'));
        let without_prefix = trimmed
            .strip_prefix("app/")
            .or_else(|| trimmed.strip_prefix("runtime/"))
            .unwrap_or(trimmed);
        let id = without_prefix.split('/').next().unwrap_or(without_prefix);
        if looks_like_app_id(id) {
            hint.package_name = Some(id.to_string());
            break;
        }
    }

    if let Some(pct) = super::extract_percent(line) {
        hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
    }

    hint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_ref_roundtrip() {
        let mut p = Package {
            id: "flatpak:system:app/org.videolan.vlc/x86_64/stable".into(),
            name: "VLC".into(),
            arch: "x86_64".into(),
            version: "3.0.23".into(),
            repo: "fedora".into(),
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::FlatpakSystem,
            detail: String::new(),
            summary: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        };
        assert_eq!(
            update_ref_from_package(&p).as_deref(),
            Some("app/org.videolan.vlc/x86_64/stable")
        );

        p.id = "flatpak:user:runtime/org.gnome.Platform/x86_64/48".into();
        p.source = UpdateSource::FlatpakUser;
        assert_eq!(
            update_ref_from_package(&p).as_deref(),
            Some("runtime/org.gnome.Platform/x86_64/48")
        );

        for bad_id in [
            "dnf:firefox.x86_64:140.0",
            "flatpak:user",
            "flatpak:user:",
            "",
        ] {
            p.id = bad_id.into();
            assert_eq!(
                update_ref_from_package(&p),
                None,
                "id `{bad_id}` must not produce a flatpak ref"
            );
        }
    }

    #[test]
    fn update_args_for_refs_sets_scope_noninteractive_and_refs() {
        let refs = vec![
            "app/org.signal.Signal/x86_64/stable".to_string(),
            "runtime/org.gnome.Platform/x86_64/48".to_string(),
        ];
        assert_eq!(
            update_args_for_refs(true, &refs),
            vec![
                "update",
                "--user",
                "-y",
                "--noninteractive",
                "app/org.signal.Signal/x86_64/stable",
                "runtime/org.gnome.Platform/x86_64/48",
            ]
        );
        assert_eq!(
            update_args_for_refs(false, &refs),
            vec![
                "update",
                "--system",
                "-y",
                "--noninteractive",
                "app/org.signal.Signal/x86_64/stable",
                "runtime/org.gnome.Platform/x86_64/48",
            ]
        );
        assert_eq!(
            update_args_for_refs(true, &[]),
            vec!["update", "--user", "-y", "--noninteractive"]
        );
    }

    fn listed(formatted_ref: &str, name: &str, version: &str) -> FlatpakListedRef {
        let mut parts = formatted_ref.split('/');
        let _kind = parts.next();
        let _id = parts.next();
        let arch = parts.next().unwrap_or("x86_64");
        let branch = parts.next().unwrap_or("");
        FlatpakListedRef {
            formatted_ref: formatted_ref.into(),
            name: name.into(),
            arch: arch.into(),
            branch: branch.into(),
            version: version.into(),
            origin: "flathub".into(),
            summary: None,
            installed_size_bytes: None,
        }
    }

    /// A current app stays on the list when a related extension must be installed,
    /// and the row names that extension.
    #[test]
    fn related_extension_is_named_on_the_parent_row() {
        let refs = vec![listed(
            "app/com.valvesoftware.Steam/x86_64/stable",
            "Steam",
            "1.0.0.87",
        )];
        let ops = vec![FlatpakResolvedOp {
            formatted_ref: "runtime/org.freedesktop.Platform.GL32.nvidia-550-00-00/x86_64/1.4"
                .into(),
            kind: FlatpakOpKind::Install,
            download_bytes: 251_000_000,
            installed_size_bytes: 0,
            related_to: vec!["app/com.valvesoftware.Steam/x86_64/stable".into()],
        }];
        let pkgs = packages_from_flatpak_updates(UpdateSource::FlatpakSystem, &refs, &ops);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "Steam");
        assert_eq!(
            pkgs[0].id,
            "flatpak:system:app/com.valvesoftware.Steam/x86_64/stable"
        );
        assert!(
            pkgs[0]
                .detail
                .contains("installs org.freedesktop.Platform.GL32.nvidia-550-00-00"),
            "detail was {}",
            pkgs[0].detail
        );
        assert!(
            !pkgs[0].detail.contains("runtime/"),
            "detail should name the extension id, was {}",
            pkgs[0].detail
        );
        assert!(pkgs[0].detail.contains("1.0.0.87"));
        assert_eq!(
            update_ref_from_package(&pkgs[0]).as_deref(),
            Some("app/com.valvesoftware.Steam/x86_64/stable")
        );
        assert!(pkgs[0].size_bytes.is_some());
    }

    #[test]
    fn shared_extension_download_is_counted_once() {
        let refs = vec![
            listed(
                "runtime/org.gnome.Platform/x86_64/51",
                "GNOME Application Platform version 51",
                "",
            ),
            listed(
                "runtime/org.freedesktop.Platform/x86_64/26.08",
                "Freedesktop Platform",
                "freedesktop-sdk-26.08.4",
            ),
        ];
        let ops = vec![FlatpakResolvedOp {
            formatted_ref: "runtime/org.freedesktop.Platform.GL.nvidia-550-00-00/x86_64/1.4".into(),
            kind: FlatpakOpKind::Install,
            download_bytes: 373 * 1024 * 1024,
            installed_size_bytes: 0,
            related_to: vec![
                "runtime/org.gnome.Platform/x86_64/51".into(),
                "runtime/org.freedesktop.Platform/x86_64/26.08".into(),
            ],
        }];
        let pkgs = packages_from_flatpak_updates(UpdateSource::FlatpakSystem, &refs, &ops);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().all(|p| {
            p.detail
                .contains("installs org.freedesktop.Platform.GL.nvidia-550-00-00")
        }));
        assert_eq!(pkgs[0].version, "51");
        assert_eq!(pkgs.iter().filter(|p| p.size_bytes.is_some()).count(), 1);
        assert_eq!(
            crate::model::total_download_bytes(&pkgs),
            Some(373 * 1024 * 1024)
        );
    }

    #[test]
    fn commit_update_does_not_claim_an_extension_install() {
        let refs = vec![listed(
            "app/org.signal.Signal/x86_64/stable",
            "Signal",
            "8.40.0",
        )];
        let ops = vec![FlatpakResolvedOp {
            formatted_ref: "app/org.signal.Signal/x86_64/stable".into(),
            kind: FlatpakOpKind::Update,
            download_bytes: 20 * 1024 * 1024,
            installed_size_bytes: 0,
            related_to: vec![],
        }];
        let pkgs = packages_from_flatpak_updates(UpdateSource::FlatpakUser, &refs, &ops);
        assert_eq!(pkgs.len(), 1);
        assert!(!pkgs[0].detail.contains("installs"), "{}", pkgs[0].detail);
        assert!(pkgs[0].detail.contains("8.40.0"));
        assert_eq!(pkgs[0].size_bytes, Some(20 * 1024 * 1024));
        assert_eq!(pkgs[0].size_label().as_deref(), Some("20 MB"));
        assert_eq!(
            pkgs[0].id,
            "flatpak:user:app/org.signal.Signal/x86_64/stable"
        );
    }

    #[test]
    fn uninstall_is_not_described_as_an_install() {
        let refs = vec![listed(
            "runtime/org.freedesktop.Platform/x86_64/26.08",
            "Freedesktop Platform",
            "freedesktop-sdk-26.08.4",
        )];
        let ops = vec![FlatpakResolvedOp {
            formatted_ref: "runtime/org.freedesktop.Platform.GL.nvidia-615-78-08/x86_64/1.4".into(),
            kind: FlatpakOpKind::Uninstall,
            download_bytes: 0,
            installed_size_bytes: 0,
            related_to: vec!["runtime/org.freedesktop.Platform/x86_64/26.08".into()],
        }];
        let pkgs = packages_from_flatpak_updates(UpdateSource::FlatpakSystem, &refs, &ops);
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.iter().all(|p| !p.detail.contains("installs")), "{pkgs:?}");
        assert!(pkgs[0].size_bytes.is_none());
    }

    #[test]
    fn packages_from_flatpak_updates_handles_edge_cases_and_fallbacks() {
        let refs = vec![
            // Empty formatted_ref skipped
            FlatpakListedRef {
                formatted_ref: String::new(),
                name: "Skip".into(),
                arch: "x86_64".into(),
                branch: "stable".into(),
                version: "1.0".into(),
                origin: "flathub".into(),
                summary: None,
                installed_size_bytes: None,
            },
            // Empty name, version, origin fallbacks
            FlatpakListedRef {
                formatted_ref: "app/org.example.Alpha/x86_64/stable".into(),
                name: String::new(),
                arch: "x86_64".into(),
                branch: "stable".into(),
                version: String::new(),
                origin: String::new(),
                summary: Some("  Privacy-first messenger  ".into()),
                installed_size_bytes: Some(100_000_000),
            },
            // Duplicate formatted_ref ignored (keeps first)
            FlatpakListedRef {
                formatted_ref: "app/org.example.Alpha/x86_64/stable".into(),
                name: "Duplicate".into(),
                arch: "x86_64".into(),
                branch: "stable".into(),
                version: "9.9".into(),
                origin: "flathub".into(),
                summary: Some("Should be ignored".into()),
                installed_size_bytes: None,
            },
            FlatpakListedRef {
                formatted_ref: "runtime/org.gnome.Platform/x86_64/48".into(),
                name: "GNOME".into(),
                arch: "x86_64".into(),
                branch: "48".into(),
                version: String::new(),
                origin: "flathub".into(),
                summary: Some("   ".into()),
                installed_size_bytes: None,
            },
            // Name fallback when formatted_ref lacks a slash
            FlatpakListedRef {
                formatted_ref: "bare.ref".into(),
                name: String::new(),
                arch: "x86_64".into(),
                branch: "main".into(),
                version: "2.0".into(),
                origin: "fedora".into(),
                summary: None,
                installed_size_bytes: None,
            },
        ];
        let ops = vec![
            FlatpakResolvedOp {
                formatted_ref: "app/org.example.Alpha/x86_64/stable".into(),
                kind: FlatpakOpKind::Update,
                download_bytes: 0,
                installed_size_bytes: 112_500_000,
                related_to: vec![],
            },
            // Shared extension with related_to listed in reverse order -> size still goes to earliest index (Alpha).
            FlatpakResolvedOp {
                formatted_ref: "runtime/org.shared.Ext/x86_64/1".into(),
                kind: FlatpakOpKind::Install,
                download_bytes: 5_000_000,
                installed_size_bytes: 0,
                related_to: vec![
                    "runtime/org.gnome.Platform/x86_64/48".into(),
                    "app/org.example.Alpha/x86_64/stable".into(),
                ],
            },
            // Second extension sorted alphabetically with Ext on Alpha; duplicate ext id deduplicated.
            FlatpakResolvedOp {
                formatted_ref: "runtime/org.alpha.Ext/x86_64/1".into(),
                kind: FlatpakOpKind::Install,
                download_bytes: 1_000_000,
                installed_size_bytes: 0,
                related_to: vec!["app/org.example.Alpha/x86_64/stable".into()],
            },
            FlatpakResolvedOp {
                formatted_ref: "runtime/org.alpha.Ext/x86_64/1".into(),
                kind: FlatpakOpKind::Install,
                download_bytes: 500_000,
                installed_size_bytes: 0,
                related_to: vec!["app/org.example.Alpha/x86_64/stable".into()],
            },
            // Malformed op ref (no slash) and unrelated parent refs are ignored.
            FlatpakResolvedOp {
                formatted_ref: "malformed".into(),
                kind: FlatpakOpKind::Install,
                download_bytes: 99_000_000,
                installed_size_bytes: 0,
                related_to: vec!["app/org.example.Alpha/x86_64/stable".into()],
            },
            FlatpakResolvedOp {
                formatted_ref: "runtime/org.orphan.Ext/x86_64/1".into(),
                kind: FlatpakOpKind::Install,
                download_bytes: 88_000_000,
                installed_size_bytes: 0,
                related_to: vec!["app/org.not.Listed/x86_64/stable".into()],
            },
        ];

        // Non-flatpak UpdateSource exercises default "flatpak" scope label.
        let pkgs = packages_from_flatpak_updates(UpdateSource::Dnf, &refs, &ops);
        assert_eq!(pkgs.len(), 3);

        assert_eq!(pkgs[0].id, "flatpak:flatpak:app/org.example.Alpha/x86_64/stable");
        assert_eq!(pkgs[0].name, "org.example.Alpha");
        assert_eq!(pkgs[0].version, "stable");
        assert_eq!(pkgs[0].repo, "flatpak");
        assert_eq!(pkgs[0].summary.as_deref(), Some("Privacy-first messenger"));
        assert_eq!(pkgs[0].install_size_delta, Some(12_500_000));
        assert_eq!(
            pkgs[0].detail,
            "stable · flatpak · installs org.alpha.Ext, org.shared.Ext"
        );
        assert_eq!(pkgs[0].size_bytes, Some(6_500_000));

        assert_eq!(pkgs[1].name, "GNOME");
        assert!(pkgs[1].summary.is_none());
        assert_eq!(pkgs[1].install_size_delta, None);
        assert_eq!(pkgs[1].size_bytes, None);
        assert_eq!(
            pkgs[1].detail,
            "48 · flathub · flatpak · installs org.shared.Ext"
        );

        assert_eq!(pkgs[2].name, "bare.ref");
        assert_eq!(pkgs[2].detail, "2.0 · fedora · flatpak");
        assert_eq!(pkgs[2].size_bytes, None);
    }

    #[test]
    fn tracker_emits_telemetry_extension_completion_and_error_policies() {
        let mut tracker = FlatpakTransactionTracker::new(vec![
            "app/com.valvesoftware.Steam/x86_64/stable".into(),
            "app/org.signal.Signal/x86_64/stable".into(),
        ]);

        let ops = vec![
            FlatpakOperationMeta {
                formatted_ref: "app/com.valvesoftware.Steam/x86_64/stable".into(),
                kind: FlatpakOpKind::Update,
                remote: "flathub".into(),
                download_bytes: 12_000_000,
                installed_size_bytes: 55_000_000,
                related_to: vec![],
                is_skipped: false,
            },
            FlatpakOperationMeta {
                formatted_ref:
                    "runtime/org.freedesktop.Platform.GL32.nvidia-570-133-07/x86_64/1.4".into(),
                kind: FlatpakOpKind::Install,
                remote: "flathub".into(),
                download_bytes: 251_000_000,
                installed_size_bytes: 310_000_000,
                related_to: vec!["app/com.valvesoftware.Steam/x86_64/stable".into()],
                is_skipped: false,
            },
            FlatpakOperationMeta {
                formatted_ref: "app/org.signal.Signal/x86_64/stable".into(),
                kind: FlatpakOpKind::Update,
                remote: "flathub".into(),
                download_bytes: 44_040_192,
                installed_size_bytes: 380_000_000,
                related_to: vec![],
                is_skipped: false,
            },
            FlatpakOperationMeta {
                formatted_ref: "runtime/org.unused.Old/x86_64/1".into(),
                kind: FlatpakOpKind::Uninstall,
                remote: "flathub".into(),
                download_bytes: 0,
                installed_size_bytes: 0,
                related_to: vec![],
                is_skipped: false,
            },
        ];

        let ready_events = tracker.on_ready(ops.clone());
        assert_eq!(tracker.total_ops(), 3);
        assert!(ready_events[0]
            .console_line
            .as_deref()
            .unwrap()
            .contains("Prepared 3 Flatpak operation(s)"));

        // Op 1: Steam direct update
        let start1 = tracker.on_new_operation(ops[0].clone());
        assert!(start1
            .console_line
            .as_deref()
            .unwrap()
            .contains("[1/3] Updating com.valvesoftware.Steam"));
        let h1 = start1.hint.unwrap();
        assert_eq!(h1.package_name.as_deref(), Some("com.valvesoftware.Steam"));
        assert_eq!(h1.status, Some(PackageStatus::Downloading));
        assert_eq!(h1.stage_current, Some(1));
        assert_eq!(h1.stage_total, Some(3));

        // Progress download tick computes throughput
        let snap1 = FlatpakProgressSnapshot {
            status: "Downloading files".into(),
            progress_pct: 50,
            bytes_transferred: 6_000_000,
            is_estimating: false,
            start_time_us: 1_000_000,
        };
        let ev1 = tracker
            .on_operation_progress(&snap1, 2_500_000)
            .expect("progress event");
        assert!(ev1.throughput.is_some());
        let tp = ev1.throughput.unwrap();
        assert!((tp.elapsed_secs - 1.5).abs() < 1e-9);
        assert!(ev1.console_draw.as_deref().unwrap().contains("50%"));
        assert_eq!(
            ev1.hint.as_ref().unwrap().status,
            Some(PackageStatus::Downloading)
        );
        assert_eq!(ev1.hint.as_ref().unwrap().progress, Some(0.5));

        // Identical duplicate tick without 256KiB delta is suppressed
        assert!(tracker.on_operation_progress(&snap1, 2_600_000).is_none());

        // Deploy status switches to Installing ("Updating Flatpak")
        let snap_deploy = FlatpakProgressSnapshot {
            status: "Deploying update".into(),
            progress_pct: 85,
            bytes_transferred: 12_000_000,
            is_estimating: false,
            start_time_us: 1_000_000,
        };
        let ev_deploy = tracker
            .on_operation_progress(&snap_deploy, 3_000_000)
            .expect("deploy event");
        assert_eq!(
            ev_deploy.hint.as_ref().unwrap().status,
            Some(PackageStatus::Installing)
        );
        assert_eq!(
            ev_deploy.hint.as_ref().unwrap().phase_label.as_deref(),
            Some("Updating Flatpak")
        );

        // Op 1 done -> Steam STILL has pending NVIDIA extension op, so Completed is deferred!
        let done1 = tracker.on_operation_done(&ops[0], 0);
        assert!(done1.hint.is_none());

        // Op 2: NVIDIA GL32 related extension -> attributed to Steam parent
        let start2 = tracker.on_new_operation(ops[1].clone());
        assert!(start2
            .console_line
            .as_deref()
            .unwrap()
            .contains("for com.valvesoftware.Steam"));
        assert_eq!(
            start2.hint.as_ref().unwrap().package_name.as_deref(),
            Some("com.valvesoftware.Steam")
        );
        let done2 = tracker.on_operation_done(&ops[1], 0);
        let done2_hint = done2.hint.expect("parent completed when last extension finishes");
        assert_eq!(
            done2_hint.package_name.as_deref(),
            Some("com.valvesoftware.Steam")
        );
        assert_eq!(done2_hint.status, Some(PackageStatus::Completed));

        // Op 3: Signal encounters fatal vs non-fatal errors
        let _ = tracker.on_new_operation(ops[2].clone());
        let (nonfatal_ev, cont_nonfatal) = tracker.on_operation_error(
            &ops[2],
            "transient mirror timeout",
            FLATPAK_TRANSACTION_ERROR_DETAILS_NON_FATAL,
            false,
        );
        assert!(cont_nonfatal);
        assert!(nonfatal_ev.hint.is_none());

        let (fatal_ev, cont_fatal) =
            tracker.on_operation_error(&ops[2], "Socket I/O timed out", 0, false);
        assert!(!cont_fatal);
        assert_eq!(fatal_ev.hint.unwrap().status, Some(PackageStatus::Failed));
        assert!(fatal_ev
            .console_line
            .as_deref()
            .unwrap()
            .starts_with("error: Failed to update org.signal.Signal"));

        let err = tracker
            .finish_result(false, Some("Socket I/O timed out".into()), false)
            .unwrap_err();
        assert!(err.contains("Socket I/O timed out"));

        // Clean tracker finishes with outcome
        let mut clean = FlatpakTransactionTracker::new(vec!["app/org.signal.Signal/x86_64/stable".into()]);
        let clean_op = ops[2].clone();
        clean.on_ready(vec![clean_op.clone()]);
        clean.on_new_operation(clean_op.clone());
        let done = clean.on_operation_done(&clean_op, 0);
        assert_eq!(done.hint.unwrap().status, Some(PackageStatus::Completed));
        let outcome = clean.finish_result(true, None, false).unwrap();
        assert_eq!(outcome.completed_ops, 1);
        assert!(outcome.any_changed);
        assert_eq!(
            clean.finish_result(false, Some("Cancelled".into()), true),
            Err("cancelled".into())
        );
    }

    #[test]
    fn work_cancel_and_active_registry_cancel_real_g_cancellable() {
        let c1 = FlatpakCancellable::new().expect("create GCancellable");
        let c2 = FlatpakCancellable::new().expect("create GCancellable");
        assert!(!c1.is_cancelled());
        assert!(!c2.is_cancelled());

        let cancel = WorkCancel::new();
        let c1_hook = c1.clone();
        cancel.on_cancel(move || c1_hook.cancel());

        let guard = ActiveCancellableGuard::register(&c2);
        cancel.cancel();
        assert!(c1.is_cancelled(), "WorkCancel hook must cancel GCancellable");

        cancel_active_transaction();
        assert!(
            c2.is_cancelled(),
            "cancel_active_transaction must cancel registered GCancellable"
        );
        drop(guard);

        // Late-registered on_cancel hook after cancel fires immediately.
        let c3 = FlatpakCancellable::new().expect("create GCancellable");
        let c3_hook = c3.clone();
        cancel.on_cancel(move || c3_hook.cancel());
        assert!(c3.is_cancelled());
    }

    #[test]
    #[ignore = "Requires live Fedora host with libflatpak.so.0 installed"]
    fn live_libflatpak_symbols_and_installed_refs_probe() {
        let api = flatpak_api().expect("load libflatpak.so.0");
        assert!(
            api.apply.is_ok(),
            "FlatpakApplyApi failed to resolve all symbols: {:?}",
            api.apply.as_ref().err()
        );
        assert!(
            api.appdata_summary.is_some(),
            "expected flatpak_installed_ref_get_appdata_summary"
        );
        assert!(
            api.ref_installed_size.is_some(),
            "expected flatpak_installed_ref_get_installed_size"
        );
        assert!(
            api.op_installed_size.is_some(),
            "expected flatpak_transaction_operation_get_installed_size"
        );

        let sys_res = query_updates(false);
        assert!(sys_res.is_ok(), "system query failed: {:?}", sys_res.err());
    }

    #[test]
    fn parse_progress_line_covers_prefixed_refs_phases_and_errors() {
        let dl_bare = parse_progress_line("Downloading org.signal.Signal/x86_64/stable 45%");
        assert_eq!(dl_bare.package_name.as_deref(), Some("org.signal.Signal"));
        assert_eq!(dl_bare.status, Some(PackageStatus::Downloading));
        assert_eq!(dl_bare.phase_label.as_deref(), Some("Downloading Flatpak"));
        assert_eq!(dl_bare.progress, Some(0.45));

        let dl_prefixed =
            parse_progress_line("Downloading app/org.mozilla.Thunderbird/x86_64/stable 60%");
        assert_eq!(
            dl_prefixed.package_name.as_deref(),
            Some("org.mozilla.Thunderbird")
        );
        assert_eq!(dl_prefixed.status, Some(PackageStatus::Downloading));
        assert_eq!(dl_prefixed.progress, Some(0.60));

        let inst_runtime =
            parse_progress_line("Installing runtime/org.gnome.Platform/x86_64/48 100%");
        assert_eq!(
            inst_runtime.package_name.as_deref(),
            Some("org.gnome.Platform")
        );
        assert_eq!(inst_runtime.status, Some(PackageStatus::Installing));
        assert_eq!(inst_runtime.phase_label.as_deref(), Some("Updating Flatpak"));
        assert_eq!(inst_runtime.progress, Some(1.0));

        let updating = parse_progress_line("Updating org.signal.Signal/x86_64/stable 100%");
        assert_eq!(updating.package_name.as_deref(), Some("org.signal.Signal"));
        assert_eq!(updating.status, Some(PackageStatus::Installing));

        let noop = parse_progress_line("Nothing to update.");
        assert_eq!(noop.phase_label.as_deref(), Some("Nothing to update"));
        assert_eq!(noop.status, None);
        assert_eq!(noop.package_name, None);

        let no_updates = parse_progress_line("No updates found.");
        assert_eq!(no_updates.phase_label.as_deref(), Some("Nothing to update"));

        let decimal_pct = parse_progress_line("Downloading 45.0%");
        assert_eq!(decimal_pct.package_name, None);
        assert_eq!(decimal_pct.status, Some(PackageStatus::Downloading));
        assert_eq!(decimal_pct.progress, Some(0.45));

        for noise in ["Looking for updates…", "Updates complete."] {
            let hint = parse_progress_line(noise);
            assert_eq!(hint, ProgressHint::default(), "noise `{noise}` should be ignored");
        }

        let err = parse_progress_line(
            "error: Failed to update org.signal.Signal: While pulling app/org.signal.Signal/x86_64/stable from remote flathub: Socket I/O timed out",
        );
        assert_eq!(err, ProgressHint::default());
    }
}
