//! Flatpak update discovery and progress parsing.
//!
//! Discovery uses libflatpak `list_installed_refs_for_update`, the same call
//! GNOME Software uses. That includes a ref whose own commit is current when a
//! related extension (for example the NVIDIA GL driver) still needs to be
//! installed. Apply stays `flatpak update` on those refs, which pulls the
//! extensions. `flatpak remote-ls --updates` does not see them.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::OnceLock;

use crate::model::{format_bytes, AdvisoryKind, Package, PackageStatus, UpdateSource};

use super::ProgressHint;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub related_to: Vec<String>,
}

/// Result of a user or system Flatpak check.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatpakCheck {
    pub packages: Vec<Package>,
    pub warning: Option<String>,
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
    let requested: HashMap<&str, usize> = refs
        .iter()
        .enumerate()
        .map(|(i, r)| (r.formatted_ref.as_str(), i))
        .collect();

    let mut installs_for: HashMap<usize, Vec<String>> = HashMap::new();
    let mut bytes_for: HashMap<usize, u64> = HashMap::new();

    for op in ops {
        if op.kind == FlatpakOpKind::Uninstall {
            continue;
        }
        if let Some(&index) = requested.get(op.formatted_ref.as_str()) {
            *bytes_for.entry(index).or_default() += op.download_bytes;
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
            size: (bytes > 0).then(|| format_bytes(bytes)),
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: AdvisoryKind::Unknown,
            source,
            detail,
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

struct RawOp {
    formatted_ref: String,
    kind: FlatpakOpKind,
    download_bytes: u64,
    related_to: Vec<String>,
}

struct ReadyCapture {
    ops: Vec<RawOp>,
    saw_ready: bool,
    cancellable: *mut gtk::gio::ffi::GCancellable,
    api: &'static FlatpakApi,
}

/// Resolve `list_installed_refs_for_update`, then abort a transaction at
/// `ready-pre-auth` so the related installs are known without downloading.
///
/// # Safety
/// Caller must not hold a pointer into the Flatpak objects after return.
unsafe fn query_raw(
    user: bool,
) -> Result<
    (
        Vec<FlatpakListedRef>,
        Vec<FlatpakResolvedOp>,
        Option<String>,
    ),
    String,
> {
    let api = flatpak_api()?;
    let mut error: *mut glib::ffi::GError = std::ptr::null_mut();
    let installation = if user {
        (api.new_user)(std::ptr::null_mut(), &mut error)
    } else {
        (api.new_system)(std::ptr::null_mut(), &mut error)
    };
    if installation.is_null() {
        return Err(take_error(error));
    }

    let listed = (api.list_for_update)(installation, std::ptr::null_mut(), &mut error);
    if listed.is_null() {
        glib::gobject_ffi::g_object_unref(installation as *mut glib::gobject_ffi::GObject);
        return Err(take_error(error));
    }

    let refs = listed_refs_from_array(api, listed);
    glib::ffi::g_ptr_array_unref(listed);
    if refs.is_empty() {
        glib::gobject_ffi::g_object_unref(installation as *mut glib::gobject_ffi::GObject);
        return Ok((Vec::new(), Vec::new(), None));
    }

    let transaction = (api.new_transaction)(installation, std::ptr::null_mut(), &mut error);
    if transaction.is_null() {
        glib::gobject_ffi::g_object_unref(installation as *mut glib::gobject_ffi::GObject);
        return Err(take_error(error));
    }
    (api.set_no_interaction)(transaction, glib::ffi::GTRUE);

    let mut added = 0usize;
    let mut add_error: Option<String> = None;
    for r in &refs {
        let Ok(cref) = CString::new(r.formatted_ref.as_str()) else {
            continue;
        };
        // add_update(transaction, ref, subpaths, commit, error). NULL commit
        // means the latest commit on the installed remote.
        let ok = (api.add_update)(
            transaction,
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

    let cancellable = gtk::gio::ffi::g_cancellable_new();
    let mut capture = ReadyCapture {
        ops: Vec::new(),
        saw_ready: false,
        cancellable,
        api,
    };
    if added > 0 {
        glib::gobject_ffi::g_signal_connect_data(
            transaction as *mut glib::gobject_ffi::GObject,
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
        let ran = (api.run)(transaction, cancellable, &mut error);
        if ran == glib::ffi::GTRUE {
            // The ready handler failed to abort. Stop anything still in flight.
            gtk::gio::ffi::g_cancellable_cancel(cancellable);
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

    let ops = capture
        .ops
        .into_iter()
        .map(|op| FlatpakResolvedOp {
            formatted_ref: op.formatted_ref,
            kind: op.kind,
            download_bytes: op.download_bytes,
            related_to: op.related_to,
        })
        .collect();

    glib::gobject_ffi::g_object_unref(cancellable as *mut glib::gobject_ffi::GObject);
    glib::gobject_ffi::g_object_unref(transaction as *mut glib::gobject_ffi::GObject);
    glib::gobject_ffi::g_object_unref(installation as *mut glib::gobject_ffi::GObject);
    Ok((refs, ops, add_error))
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

unsafe fn collect_ops(api: &FlatpakApi, transaction: *mut c_void) -> Vec<RawOp> {
    let list = (api.get_operations)(transaction as *mut FlatpakTransaction);
    let mut out = Vec::new();
    let mut link = list;
    while !link.is_null() {
        let op = (*link).data as *mut FlatpakTransactionOperation;
        if !op.is_null() {
            let kind = match (api.get_operation_type)(op) {
                0 | 2 => FlatpakOpKind::Install,
                1 => FlatpakOpKind::Update,
                _ => FlatpakOpKind::Uninstall,
            };
            let mut related_to = Vec::new();
            let related = (api.get_related_to_ops)(op);
            if !related.is_null() && !(*related).pdata.is_null() {
                for i in 0..(*related).len {
                    let parent =
                        *(*related).pdata.add(i as usize) as *mut FlatpakTransactionOperation;
                    if parent.is_null() {
                        continue;
                    }
                    related_to.push(c_to_string((api.get_ref)(parent)));
                }
            }
            out.push(RawOp {
                formatted_ref: c_to_string((api.get_ref)(op)),
                kind,
                download_bytes: (api.get_download_size)(op),
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
        refs.push(FlatpakListedRef {
            arch: c_to_string((api.get_arch)(iref as *mut FlatpakRef)),
            branch: c_to_string((api.get_branch)(iref as *mut FlatpakRef)),
            version: c_to_string((api.appdata_version)(iref)),
            origin: c_to_string((api.get_origin)(iref)),
            name,
            formatted_ref: formatted,
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

fn c_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
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

type ErrSlot = *mut *mut glib::ffi::GError;
type Cancel = *mut gtk::gio::ffi::GCancellable;

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
    appdata_version: unsafe extern "C" fn(*mut FlatpakInstalledRef) -> *const c_char,
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
    get_related_to_ops:
        unsafe extern "C" fn(*mut FlatpakTransactionOperation) -> *mut glib::ffi::GPtrArray,
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
        appdata_version: load_fn(handle, c"flatpak_installed_ref_get_appdata_version")?,
        new_transaction: load_fn(handle, c"flatpak_transaction_new_for_installation")?,
        add_update: load_fn(handle, c"flatpak_transaction_add_update")?,
        set_no_interaction: load_fn(handle, c"flatpak_transaction_set_no_interaction")?,
        run: load_fn(handle, c"flatpak_transaction_run")?,
        get_operations: load_fn(handle, c"flatpak_transaction_get_operations")?,
        get_operation_type: load_fn(handle, c"flatpak_transaction_operation_get_operation_type")?,
        get_ref: load_fn(handle, c"flatpak_transaction_operation_get_ref")?,
        get_download_size: load_fn(handle, c"flatpak_transaction_operation_get_download_size")?,
        get_related_to_ops: load_fn(handle, c"flatpak_transaction_operation_get_related_to_ops")?,
    })
}

unsafe fn load_fn<T>(handle: *mut c_void, name: &CStr) -> Result<T, String> {
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
    s.contains('.') && !s.contains(' ') && s.len() >= 3
}

pub fn parse_progress_line(line: &str) -> ProgressHint {
    let lower = line.to_ascii_lowercase();
    let mut hint = ProgressHint::default();

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
        let id = token.split('/').next().unwrap_or(token);
        if looks_like_app_id(id) {
            hint.package_name = Some(
                id.trim_start_matches("app/")
                    .trim_start_matches("runtime/")
                    .to_string(),
            );
            break;
        }
    }

    if let Some(pct) = extract_percent(line) {
        hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
    }

    hint
}

fn extract_percent(line: &str) -> Option<f64> {
    for token in line.split_whitespace() {
        if let Some(num) = token.strip_suffix('%') {
            if let Ok(v) = num.parse::<f64>() {
                if (0.0..=100.0).contains(&v) {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn update_ref_roundtrip() {
        let p = Package {
            id: "flatpak:system:app/org.videolan.vlc/x86_64/stable".into(),
            name: "VLC".into(),
            arch: "x86_64".into(),
            version: "3.0.23".into(),
            repo: "fedora".into(),
            size: None,
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: AdvisoryKind::Unknown,
            source: UpdateSource::FlatpakSystem,
            detail: String::new(),
        };
        assert_eq!(
            update_ref_from_package(&p).as_deref(),
            Some("app/org.videolan.vlc/x86_64/stable")
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
            formatted_ref: "runtime/org.freedesktop.Platform.GL32.nvidia-620-70-06/x86_64/1.4"
                .into(),
            kind: FlatpakOpKind::Install,
            download_bytes: 251_000_000,
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
                .contains("installs org.freedesktop.Platform.GL32.nvidia-620-70-06"),
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
        assert!(pkgs[0].size.is_some());
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
            formatted_ref: "runtime/org.freedesktop.Platform.GL.nvidia-620-70-06/x86_64/1.4".into(),
            kind: FlatpakOpKind::Install,
            download_bytes: 373 * 1024 * 1024,
            related_to: vec![
                "runtime/org.gnome.Platform/x86_64/51".into(),
                "runtime/org.freedesktop.Platform/x86_64/26.08".into(),
            ],
        }];
        let pkgs = packages_from_flatpak_updates(UpdateSource::FlatpakSystem, &refs, &ops);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().all(|p| {
            p.detail
                .contains("installs org.freedesktop.Platform.GL.nvidia-620-70-06")
        }));
        assert_eq!(pkgs[0].version, "51");
        assert_eq!(pkgs.iter().filter(|p| p.size.is_some()).count(), 1);
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
            related_to: vec![],
        }];
        let pkgs = packages_from_flatpak_updates(UpdateSource::FlatpakUser, &refs, &ops);
        assert_eq!(pkgs.len(), 1);
        assert!(!pkgs[0].detail.contains("installs"), "{}", pkgs[0].detail);
        assert!(pkgs[0].detail.contains("8.40.0"));
        assert_eq!(pkgs[0].size.as_deref(), Some("20 MB"));
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
            related_to: vec!["runtime/org.freedesktop.Platform/x86_64/26.08".into()],
        }];
        let pkgs = packages_from_flatpak_updates(UpdateSource::FlatpakSystem, &refs, &ops);
        assert_eq!(pkgs.len(), 1);
        assert!(!pkgs[0].detail.contains("installs"), "{}", pkgs[0].detail);
        assert!(pkgs[0].size.is_none());
    }

    /// While the host NVIDIA driver has no matching Flatpak GL extension, the
    /// system check must name that extension on Steam and the platforms.
    /// Skips once the extension is installed, or when Flatpak is absent.
    #[test]
    fn system_query_names_missing_nvidia_extension() {
        if Command::new("flatpak").arg("--version").output().is_err() {
            return;
        }
        let installed = Command::new("flatpak")
            .args([
                "info",
                "--system",
                "org.freedesktop.Platform.GL.nvidia-620-70-06",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if installed {
            return;
        }
        let steam_installed = Command::new("flatpak")
            .args(["info", "--system", "com.valvesoftware.Steam"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !steam_installed {
            return;
        }
        let check = query_updates(false).expect("system flatpak query");
        let steam = check
            .packages
            .iter()
            .find(|p| p.id.contains("com.valvesoftware.Steam"))
            .expect("Steam is offered while its 32-bit NVIDIA driver is missing");
        assert!(
            steam
                .detail
                .contains("installs org.freedesktop.Platform.GL32.nvidia-620-70-06"),
            "steam detail was {}",
            steam.detail
        );
        assert!(
            check.packages.iter().any(|p| {
                p.detail
                    .contains("installs org.freedesktop.Platform.GL.nvidia-620-70-06")
            }),
            "no platform row named the 64-bit NVIDIA extension: {:?}",
            check
                .packages
                .iter()
                .map(|p| format!("{} ({})", p.name, p.detail))
                .collect::<Vec<_>>()
        );
    }
}
