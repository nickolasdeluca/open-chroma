//! Drop-in replacement for Razer's `RzChromaSDK64.dll` / `RzChromaSDK.dll`.
//!
//! Exports the same 15 functions as Razer's DLL with the same C ABI, and
//! forwards everything to the OpenChroma service over a named pipe.
//!
//! Every export is wrapped in `guard` so a bug here can never unwind into, or
//! abort, the host game.

// The exports are C entry points whose contracts are Razer's SDK headers.
#![allow(clippy::missing_safety_doc)]

mod client;

use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use chroma_proto::{guids, AppInfo, Category, ColorRef, SdkEffect};

use client::Client;

pub type RzResult = i32;
pub const RZRESULT_SUCCESS: RzResult = 0;
pub const RZRESULT_ACCESS_DENIED: RzResult = 5;
pub const RZRESULT_NOT_SUPPORTED: RzResult = 50;
pub const RZRESULT_INVALID_PARAMETER: RzResult = 87;
pub const RZRESULT_NOT_FOUND: RzResult = 1168;
pub const RZRESULT_NOT_VALID_STATE: RzResult = 5023;
pub const RZRESULT_FAILED: RzResult = 0x8000_4005u32 as i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    fn as_tuple(self) -> guids::Guid {
        (self.data1, self.data2, self.data3, self.data4)
    }
}

#[repr(C)]
pub struct DeviceInfo {
    pub device_type: i32,
    pub connected: u32,
}

#[repr(C)]
pub struct AppInfoRaw {
    pub title: [u16; 256],
    pub description: [u16; 1024],
    pub author_name: [u16; 256],
    pub author_contact: [u16; 256],
    pub supported_device: u32,
    pub category: u32,
}

struct Sdk {
    client: Client,
    effects: HashMap<Guid, (Category, SdkEffect)>,
    next_id: u64,
}

static SDK: Mutex<Option<Sdk>> = Mutex::new(None);

fn guard(f: impl FnOnce() -> RzResult) -> RzResult {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(RZRESULT_FAILED)
}

fn with_sdk(f: impl FnOnce(&mut Sdk) -> RzResult) -> RzResult {
    guard(|| match SDK.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        Some(sdk) => f(sdk),
        None => RZRESULT_NOT_VALID_STATE,
    })
}

fn start(app: AppInfo) -> RzResult {
    guard(|| {
        let mut sdk = SDK.lock().unwrap_or_else(|e| e.into_inner());
        if sdk.is_none() {
            *sdk = Some(Sdk { client: Client::start(app), effects: HashMap::new(), next_id: 1 });
        }
        // Razer's DLL succeeds even before devices or the service are ready;
        // the client connects as soon as the service is reachable.
        RZRESULT_SUCCESS
    })
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

fn exe_title() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Unknown application".into())
}

#[no_mangle]
pub extern "C" fn Init() -> RzResult {
    start(AppInfo { title: exe_title(), ..Default::default() })
}

#[no_mangle]
pub unsafe extern "C" fn InitSDK(app_info: *const AppInfoRaw) -> RzResult {
    let app = match app_info.as_ref() {
        Some(a) => AppInfo {
            title: Some(wide(&a.title)).filter(|t| !t.is_empty()).unwrap_or_else(exe_title),
            description: wide(&a.description),
            author: wide(&a.author_name),
            contact: wide(&a.author_contact),
            category: a.category,
        },
        None => return RZRESULT_INVALID_PARAMETER,
    };
    start(app)
}

#[no_mangle]
pub extern "C" fn UnInit() -> RzResult {
    guard(|| {
        // Dropping the client flushes and closes the pipe, ending the session.
        let sdk = SDK.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(sdk);
        RZRESULT_SUCCESS
    })
}

/// Either show the effect now (`effect_id` null) or store it for `SetEffect`.
fn submit(sdk: &mut Sdk, category: Category, effect: SdkEffect, effect_id: *mut Guid) -> RzResult {
    if effect_id.is_null() {
        sdk.client.show(category, effect);
        return RZRESULT_SUCCESS;
    }
    let id = Guid {
        data1: 0x0C4A_0000 | (sdk.next_id >> 32) as u32,
        data2: 0x4F43,
        data3: 0x4852,
        data4: (sdk.next_id as u32 as u64).to_be_bytes(),
    };
    sdk.next_id += 1;
    sdk.effects.insert(id, (category, effect));
    unsafe { effect_id.write(id) };
    RZRESULT_SUCCESS
}

unsafe fn colors(p: *const c_void, offset: usize, n: usize) -> Vec<ColorRef> {
    std::slice::from_raw_parts(p.cast::<u8>().add(offset).cast::<ColorRef>(), n).to_vec()
}

unsafe fn word(p: *const c_void, offset: usize) -> u32 {
    p.cast::<u8>().add(offset).cast::<u32>().read_unaligned()
}

/// Breathing structs share the layout {Type, Color1, Color2} after `offset`.
/// `one_color_base` is the Type value meaning ONE_COLOR (if the device has it).
unsafe fn breathing(p: *const c_void, offset: usize, one_color_base: Option<u32>) -> SdkEffect {
    let kind = word(p, offset);
    let (c1, c2) = (word(p, offset + 4), word(p, offset + 8));
    let one = one_color_base == Some(kind);
    let two = kind == one_color_base.map_or(1, |b| b + 1);
    SdkEffect::Breathing { color1: c1, color2: (two && !one).then_some(c2), random: !one && !two }
}

/// Run an effect-creation call: validate state and parameters, then submit.
fn create(
    category: Category,
    param: *const c_void,
    effect_id: *mut Guid,
    parse: impl FnOnce(*const c_void) -> Option<SdkEffect>,
) -> RzResult {
    with_sdk(|sdk| match parse(param) {
        Some(effect) => submit(sdk, category, effect, effect_id),
        None if param.is_null() => RZRESULT_INVALID_PARAMETER,
        None => RZRESULT_NOT_SUPPORTED,
    })
}

#[no_mangle]
pub unsafe extern "C" fn CreateKeyboardEffect(effect: i32, param: *const c_void, effect_id: *mut Guid) -> RzResult {
    const N: usize = 6 * 22;
    create(Category::Keyboard, param, effect_id, |p| match (effect, p.is_null()) {
        (0, _) => Some(SdkEffect::None),
        (1, false) => Some(breathing(p, 0, None)),
        (2, false) => Some(SdkEffect::custom(Category::Keyboard, colors(p, 0, N))),
        // Reactive needs per-keypress input the service does not see; show
        // nothing rather than guessing.
        (3, _) => Some(SdkEffect::None),
        (4, false) => Some(SdkEffect::Static { color: word(p, 0) }),
        (5, _) => Some(SdkEffect::Spectrum),
        (6, false) => Some(SdkEffect::Wave { reverse: word(p, 0) == 2 }),
        (8, false) => Some(SdkEffect::custom_key(&colors(p, 0, N), &colors(p, N * 4, N))),
        (9, false) => {
            // CUSTOM2: Color[8][24] followed by Key[6][22]. The classic 6x22
            // grid sits in the top-left of the extended one.
            let wide = colors(p, 0, 8 * 24);
            let base: Vec<_> = (0..6).flat_map(|r| wide[r * 24..r * 24 + 22].to_vec()).collect();
            Some(SdkEffect::custom_key(&base, &colors(p, 8 * 24 * 4, N)))
        }
        _ => None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn CreateMouseEffect(effect: i32, param: *const c_void, effect_id: *mut Guid) -> RzResult {
    // Most mouse structs start with an RZLED LEDId (4 bytes). Per-LED
    // static/breathing requests are applied to the whole mouse.
    create(Category::Mouse, param, effect_id, |p| match (effect, p.is_null()) {
        (0, _) => Some(SdkEffect::None),
        (1, false) | (6, false) => Some(SdkEffect::Static { color: word(p, 4) }),
        (2, false) => Some(breathing(p, 4, Some(1))),
        (3, false) => Some(SdkEffect::mouse_v1(&colors(p, 0, 30))),
        (4, _) => Some(SdkEffect::None),
        (5, _) => Some(SdkEffect::Spectrum),
        (7, false) => Some(SdkEffect::Wave { reverse: word(p, 0) == 1 }),
        (8, false) => Some(SdkEffect::custom(Category::Mouse, colors(p, 0, 9 * 7))),
        _ => None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn CreateMousepadEffect(effect: i32, param: *const c_void, effect_id: *mut Guid) -> RzResult {
    create(Category::Mousepad, param, effect_id, |p| match (effect, p.is_null()) {
        (0, _) => Some(SdkEffect::None),
        (1, false) => Some(breathing(p, 0, None)),
        (2, false) => Some(SdkEffect::mousepad_v1(&colors(p, 0, 15))),
        (3, _) => Some(SdkEffect::Spectrum),
        (4, false) => Some(SdkEffect::Static { color: word(p, 0) }),
        (5, false) => Some(SdkEffect::Wave { reverse: word(p, 0) == 2 }),
        (6, false) => Some(SdkEffect::custom(Category::Mousepad, colors(p, 0, 20))),
        _ => None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn CreateHeadsetEffect(effect: i32, param: *const c_void, effect_id: *mut Guid) -> RzResult {
    create(Category::Headset, param, effect_id, |p| match (effect, p.is_null()) {
        (0, _) => Some(SdkEffect::None),
        (1, false) => Some(SdkEffect::Static { color: word(p, 0) }),
        (2, false) => Some(SdkEffect::Breathing { color1: word(p, 0), color2: None, random: false }),
        (3, _) => Some(SdkEffect::Spectrum),
        (4, false) => Some(SdkEffect::custom(Category::Headset, colors(p, 0, 5))),
        _ => None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn CreateKeypadEffect(effect: i32, param: *const c_void, effect_id: *mut Guid) -> RzResult {
    create(Category::Keypad, param, effect_id, |p| match (effect, p.is_null()) {
        (0, _) => Some(SdkEffect::None),
        (1, false) => Some(breathing(p, 0, None)),
        (2, false) => Some(SdkEffect::custom(Category::Keypad, colors(p, 0, 20))),
        (3, _) => Some(SdkEffect::None),
        (4, _) => Some(SdkEffect::Spectrum),
        (5, false) => Some(SdkEffect::Static { color: word(p, 0) }),
        (6, false) => Some(SdkEffect::Wave { reverse: word(p, 0) == 2 }),
        _ => None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn CreateChromaLinkEffect(effect: i32, param: *const c_void, effect_id: *mut Guid) -> RzResult {
    create(Category::ChromaLink, param, effect_id, |p| match (effect, p.is_null()) {
        (0, _) => Some(SdkEffect::None),
        (1, false) => Some(SdkEffect::custom(Category::ChromaLink, colors(p, 0, 5))),
        (2, false) => Some(SdkEffect::Static { color: word(p, 0) }),
        _ => None,
    })
}

/// Generic, device-addressed effects. These structs start with
/// `RZSIZE Size; DWORD Param;`: a pointer-sized field and a u32.
#[no_mangle]
pub unsafe extern "C" fn CreateEffect(device: Guid, effect: i32, param: *const c_void, effect_id: *mut Guid) -> RzResult {
    let Some(category) = guids::category(device.as_tuple()) else { return RZRESULT_NOT_FOUND };
    let body = std::mem::size_of::<usize>() + 4;
    create(category, param, effect_id, |p| match (effect, p.is_null()) {
        (0, _) => Some(SdkEffect::None),
        (1, _) => Some(SdkEffect::Wave { reverse: false }),
        (2, _) => Some(SdkEffect::Spectrum),
        (3, false) => Some(breathing(p, body, Some(1))),
        (4, false) | (6, false) => Some(SdkEffect::Static { color: word(p, body) }),
        (5, _) => Some(SdkEffect::None),
        (7, false) => {
            // Color[30][30]; take the top-left block matching the canvas.
            let (rows, cols) = category.dims();
            let all = colors(p, body, 30 * 30);
            let grid = (0..rows).flat_map(|r| all[r * 30..r * 30 + cols].to_vec()).collect();
            Some(SdkEffect::custom(category, grid))
        }
        _ => None,
    })
}

#[no_mangle]
pub extern "C" fn SetEffect(effect_id: Guid) -> RzResult {
    with_sdk(|sdk| match sdk.effects.get(&effect_id) {
        Some((category, effect)) => {
            sdk.client.show(*category, effect.clone());
            RZRESULT_SUCCESS
        }
        None => RZRESULT_NOT_FOUND,
    })
}

#[no_mangle]
pub extern "C" fn DeleteEffect(effect_id: Guid) -> RzResult {
    with_sdk(|sdk| match sdk.effects.remove(&effect_id) {
        Some(_) => RZRESULT_SUCCESS,
        None => RZRESULT_NOT_FOUND,
    })
}

#[no_mangle]
pub unsafe extern "C" fn QueryDevice(device: Guid, info: *mut DeviceInfo) -> RzResult {
    guard(|| {
        let Some(info) = info.as_mut() else { return RZRESULT_INVALID_PARAMETER };
        let Some(category) = guids::category(device.as_tuple()) else { return RZRESULT_NOT_FOUND };
        info.device_type = match category {
            Category::Keyboard => 1,
            Category::Mouse => 2,
            Category::Headset => 3,
            Category::Mousepad => 4,
            Category::Keypad => 5,
            Category::ChromaLink => 8,
        };
        let sdk = SDK.lock().unwrap_or_else(|e| e.into_inner());
        let available = sdk.as_ref().and_then(|s| s.client.categories()).unwrap_or_default();
        info.connected = available.contains(&category) as u32;
        RZRESULT_SUCCESS
    })
}

/// Razer posts WM_CHROMA_EVENT messages about access changes. OpenChroma
/// never revokes access, so there is nothing to send; accept the window so
/// callers that require success keep going.
#[no_mangle]
pub extern "C" fn RegisterEventNotification(_hwnd: *mut c_void) -> RzResult {
    RZRESULT_SUCCESS
}

#[no_mangle]
pub extern "C" fn UnregisterEventNotification() -> RzResult {
    RZRESULT_SUCCESS
}
