//! Optional native check with the game's own `Live2DCubismCore.dll`:
//! revive the moc, initialise a model, read counts and IDs, and confirm
//! that `ParamMouthOpenY` moves vertices. Only the Core library is loaded.

use anyhow::{Context, Result, bail, ensure};
use libloading::{Library, Symbol};
use serde::Serialize;
use std::{
    ffi::{CStr, c_char, c_void},
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const CORE_RELATIVE: &str = "GF2_Exilium_Data/Plugins/x86_64/Live2DCubismCore.dll";
pub const MOUTH_PARAMETER: &str = "ParamMouthOpenY";
const MOC_ALIGNMENT: usize = 64;
const MODEL_ALIGNMENT: usize = 16;

fn join_relative(base: &Path) -> PathBuf {
    CORE_RELATIVE
        .split('/')
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

/// Finds the Core DLL below the chosen game directory.
pub fn find_core(game: &Path) -> Option<PathBuf> {
    [game.to_path_buf(), game.join("GF2 Game")]
        .into_iter()
        .chain(game.ancestors().skip(1).take(4).map(Path::to_path_buf))
        .map(|base| join_relative(&base))
        .find(|path| path.is_file())
}

/// Loads the Core once per path and keeps it loaded for the rest of the
/// process; it is small and its C runtime is not meant to come and go.
fn library(path: &Path) -> Result<&'static Library> {
    static LOADED: Mutex<Vec<(PathBuf, &'static Library)>> = Mutex::new(Vec::new());
    let mut loaded = LOADED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, library)) = loaded.iter().find(|(p, _)| p == path) {
        return Ok(library);
    }
    // SAFETY: loading the Cubism Core only runs its DLL initialisation; the
    // library exports plain C functions.
    let library = unsafe { Library::new(path) }
        .with_context(|| tr!("无法加载 {}", "Cannot load {}", path.display()))?;
    let library: &'static Library = Box::leak(Box::new(library));
    loaded.push((path.to_path_buf(), library));
    Ok(library)
}

/// Duplicates of the standard handles, put back when dropped. In a
/// GUI-subsystem process the shipped Core's C runtime closes the inherited
/// standard handles during `csmReviveMocInPlace`; without this, redirected
/// command-line output would silently go nowhere.
struct StdHandles(Vec<(u32, std::os::windows::io::OwnedHandle)>);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
}

impl StdHandles {
    fn save() -> Self {
        use std::os::windows::io::{AsRawHandle, BorrowedHandle};
        let handles = [
            (-10i32 as u32, std::io::stdin().as_raw_handle()),
            (-11i32 as u32, std::io::stdout().as_raw_handle()),
            (-12i32 as u32, std::io::stderr().as_raw_handle()),
        ];
        Self(
            handles
                .into_iter()
                .filter(|(_, raw)| !raw.is_null() && *raw as isize != -1)
                .filter_map(|(which, raw)| {
                    // SAFETY: the handle is the process's current standard
                    // handle and stays open while it is duplicated.
                    let borrowed = unsafe { BorrowedHandle::borrow_raw(raw) };
                    borrowed
                        .try_clone_to_owned()
                        .ok()
                        .map(|owned| (which, owned))
                })
                .collect(),
        )
    }
}

impl Drop for StdHandles {
    fn drop(&mut self) {
        use std::os::windows::io::IntoRawHandle;
        for (which, owned) in self.0.drain(..) {
            // SAFETY: hands a valid duplicated handle to the process; it
            // stays open as the new standard handle.
            unsafe { SetStdHandle(which, owned.into_raw_handle()) };
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CoreReport {
    pub library: PathBuf,
    pub core_version: String,
    pub latest_moc_version: u32,
    pub moc_version: u32,
    /// `None` when the shipped Core has no `csmHasMocConsistency`.
    pub consistency: Option<bool>,
    pub model_bytes: u32,
    pub parameter_count: usize,
    pub part_count: usize,
    pub drawable_count: usize,
    pub texture_indices: Vec<i32>,
    pub parameter_ids: Vec<String>,
    pub part_ids: Vec<String>,
    /// Vertex coordinate components changed by setting the mouth
    /// parameter to 1; `None` when the model has no such parameter.
    pub mouth_changed_vertices: Option<usize>,
}

/// A buffer whose usable part starts at the requested alignment.
struct Aligned {
    storage: Vec<u8>,
    offset: usize,
}

impl Aligned {
    fn new(size: usize, alignment: usize) -> Self {
        let storage = vec![0u8; size + alignment];
        let address = storage.as_ptr() as usize;
        let offset = address.next_multiple_of(alignment) - address;
        Self { storage, offset }
    }
    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.storage[self.offset..].as_mut_ptr().cast()
    }
}

type Counter = unsafe extern "C" fn(*const c_void) -> i32;
type Ids = unsafe extern "C" fn(*const c_void) -> *const *const c_char;

/// Copies `count` C strings.
///
/// # Safety
/// `ids` must point to `count` valid NUL-terminated strings.
unsafe fn read_ids(ids: *const *const c_char, count: usize) -> Vec<String> {
    if ids.is_null() {
        return Vec::new();
    }
    (0..count)
        .map(|index| {
            // SAFETY: the Core returns `count` valid C strings.
            unsafe { CStr::from_ptr(*ids.add(index)) }
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// Runs the check on a moc3 file held in memory.
pub fn verify(library_path: &Path, moc: &[u8]) -> Result<CoreReport> {
    ensure!(
        moc.starts_with(b"MOC3"),
        tr!("moc3 签名无效", "Invalid moc3 signature")
    );
    let size = u32::try_from(moc.len()).context(tr!("moc3 过大", "The moc3 is too large"))?;
    let library = library(library_path)?;
    let _std_handles = StdHandles::save();
    // SAFETY: every call below follows the Cubism Core C API (r12): buffers
    // keep the documented alignment and outlive the moc / model handles,
    // array lengths come from the matching count functions.
    unsafe {
        let symbol = |name: &[u8]| -> Result<Symbol<*const c_void>> {
            library.get::<*const c_void>(name).with_context(|| {
                tr!(
                    "Core 缺少 {}",
                    "The Core is missing {}",
                    String::from_utf8_lossy(name)
                )
            })
        };
        macro_rules! function {
            ($name:literal, $type:ty) => {{
                let raw = symbol(concat!($name, "\0").as_bytes())?;
                std::mem::transmute::<*const c_void, $type>(*raw)
            }};
        }
        let get_version = function!("csmGetVersion", unsafe extern "C" fn() -> u32);
        let latest = function!("csmGetLatestMocVersion", unsafe extern "C" fn() -> u32);
        let moc_version = function!(
            "csmGetMocVersion",
            unsafe extern "C" fn(*const c_void, u32) -> u32
        );
        let revive = function!(
            "csmReviveMocInPlace",
            unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void
        );
        let sizeof_model = function!(
            "csmGetSizeofModel",
            unsafe extern "C" fn(*const c_void) -> u32
        );
        let initialize = function!(
            "csmInitializeModelInPlace",
            unsafe extern "C" fn(*const c_void, *mut c_void, u32) -> *mut c_void
        );
        let update = function!("csmUpdateModel", unsafe extern "C" fn(*mut c_void));
        let parameter_count = function!("csmGetParameterCount", Counter);
        let parameter_ids = function!("csmGetParameterIds", Ids);
        let parameter_values = function!(
            "csmGetParameterValues",
            unsafe extern "C" fn(*mut c_void) -> *mut f32
        );
        let part_count = function!("csmGetPartCount", Counter);
        let part_ids = function!("csmGetPartIds", Ids);
        let drawable_count = function!("csmGetDrawableCount", Counter);
        let texture_indices = function!(
            "csmGetDrawableTextureIndices",
            unsafe extern "C" fn(*const c_void) -> *const i32
        );
        let vertex_counts = function!(
            "csmGetDrawableVertexCounts",
            unsafe extern "C" fn(*const c_void) -> *const i32
        );
        let vertex_positions = function!(
            "csmGetDrawableVertexPositions",
            unsafe extern "C" fn(*const c_void) -> *const *const f32
        );

        let mut moc_buffer = Aligned::new(moc.len(), MOC_ALIGNMENT);
        let moc_at = moc_buffer.as_mut_ptr();
        std::ptr::copy_nonoverlapping(moc.as_ptr(), moc_at.cast::<u8>(), moc.len());
        let mut report = CoreReport {
            library: library_path.to_path_buf(),
            core_version: format!("{:#010x}", get_version()),
            latest_moc_version: latest(),
            moc_version: moc_version(moc_at, size),
            ..CoreReport::default()
        };
        if let Ok(raw) = library.get::<*const c_void>(b"csmHasMocConsistency\0") {
            let check = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut c_void, u32) -> i32,
            >(*raw);
            report.consistency = Some(check(moc_at, size) != 0);
        }
        if report.moc_version > report.latest_moc_version {
            bail!(tr!(
                "moc3 版本 {} 高于 Core 支持的 {}",
                "moc3 version {} is newer than the {} the Core supports",
                report.moc_version,
                report.latest_moc_version
            ));
        }
        let handle = revive(moc_at, size);
        ensure!(
            !handle.is_null(),
            tr!("csmReviveMocInPlace 失败", "csmReviveMocInPlace failed")
        );
        report.model_bytes = sizeof_model(handle);
        let mut model_buffer = Aligned::new(report.model_bytes as usize, MODEL_ALIGNMENT);
        let model = initialize(handle, model_buffer.as_mut_ptr(), report.model_bytes);
        ensure!(
            !model.is_null(),
            tr!(
                "csmInitializeModelInPlace 失败",
                "csmInitializeModelInPlace failed"
            )
        );
        update(model);

        let count = |value: i32| usize::try_from(value).unwrap_or(0);
        report.parameter_count = count(parameter_count(model));
        report.parameter_ids = read_ids(parameter_ids(model), report.parameter_count);
        report.part_count = count(part_count(model));
        report.part_ids = read_ids(part_ids(model), report.part_count);
        report.drawable_count = count(drawable_count(model));
        let drawables = report.drawable_count;
        let indices = std::slice::from_raw_parts(texture_indices(model), drawables);
        let mut unique: Vec<i32> = indices.to_vec();
        unique.sort_unstable();
        unique.dedup();
        report.texture_indices = unique;

        let snapshot = || -> Vec<f32> {
            let counts = std::slice::from_raw_parts(vertex_counts(model), drawables);
            let positions = std::slice::from_raw_parts(vertex_positions(model), drawables);
            let mut all = Vec::new();
            for (index, vertices) in counts.iter().enumerate() {
                let length = count(*vertices) * 2;
                all.extend_from_slice(std::slice::from_raw_parts(positions[index], length));
            }
            all
        };
        if let Some(mouth) = report
            .parameter_ids
            .iter()
            .position(|id| id == MOUTH_PARAMETER)
        {
            let before = snapshot();
            *parameter_values(model).add(mouth) = 1.0;
            update(model);
            let after = snapshot();
            report.mouth_changed_vertices =
                Some(before.iter().zip(&after).filter(|(a, b)| a != b).count());
        }
        drop(model_buffer);
        drop(moc_buffer);
        Ok(report)
    }
}

/// IDs used by an expression (`Parameters[].Id`) or motion
/// (`Curves[]` with target `Parameter` / `PartOpacity`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsedIds {
    pub parameters: Vec<String>,
    pub parts: Vec<String>,
}

pub fn used_ids(json: &serde_json::Value) -> UsedIds {
    let mut used = UsedIds::default();
    let id = |item: &serde_json::Value| item.get("Id").and_then(|v| v.as_str()).map(str::to_owned);
    for item in json
        .get("Parameters")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        used.parameters.extend(id(item));
    }
    for curve in json
        .get("Curves")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        match curve.get("Target").and_then(|v| v.as_str()) {
            Some("Parameter") => used.parameters.extend(id(curve)),
            Some("PartOpacity") => used.parts.extend(id(curve)),
            _ => {}
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_ids_from_expressions_and_motions() {
        let expression = serde_json::json!({
            "Type": "Live2D Expression",
            "Parameters": [{"Id": "ParamA", "Value": 1}, {"Id": "ParamB"}]
        });
        assert_eq!(used_ids(&expression).parameters, ["ParamA", "ParamB"]);
        let motion = serde_json::json!({
            "Curves": [
                {"Target": "Parameter", "Id": "ParamMouthOpenY"},
                {"Target": "PartOpacity", "Id": "Part01"},
                {"Target": "Model", "Id": "EyeBlink"}
            ]
        });
        let used = used_ids(&motion);
        assert_eq!(used.parameters, ["ParamMouthOpenY"]);
        assert_eq!(used.parts, ["Part01"]);
    }

    #[test]
    fn aligned_buffers_honour_alignment() {
        for alignment in [16, 64] {
            let mut buffer = Aligned::new(10, alignment);
            assert_eq!(buffer.as_mut_ptr() as usize % alignment, 0);
        }
    }

    #[test]
    fn rejects_non_moc_before_loading() {
        let error = verify(Path::new("missing.dll"), b"nope").unwrap_err();
        assert!(error.to_string().contains("签名"));
    }
}
