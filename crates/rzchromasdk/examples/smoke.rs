//! Load an RzChromaSDK DLL the way games do (LoadLibrary + GetProcAddress)
//! and drive it briefly. Build for i686 to check the 32-bit ABI.
//!
//! usage: smoke <path-to-dll>

use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct Guid(u32, u16, u16, [u8; 8]);

#[repr(C)]
#[derive(Default)]
struct DeviceInfo {
    device_type: i32,
    connected: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

type Init = extern "C" fn() -> i32;
type Create = extern "C" fn(i32, *const c_void, *mut Guid) -> i32;
type ById = extern "C" fn(Guid) -> i32;
type Query = extern "C" fn(Guid, *mut DeviceInfo) -> i32;

fn main() {
    let path = std::env::args().nth(1).expect("usage: smoke <dll>");
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let module = LoadLibraryW(wide.as_ptr());
        assert!(!module.is_null(), "LoadLibrary failed");
        let f = |name: &str| {
            let c = format!("{name}\0");
            let p = GetProcAddress(module, c.as_ptr());
            assert!(!p.is_null(), "missing export {name}");
            p
        };
        let init: Init = std::mem::transmute(f("Init"));
        let uninit: Init = std::mem::transmute(f("UnInit"));
        let keyboard: Create = std::mem::transmute(f("CreateKeyboardEffect"));
        let set: ById = std::mem::transmute(f("SetEffect"));
        let delete: ById = std::mem::transmute(f("DeleteEffect"));
        let query: Query = std::mem::transmute(f("QueryDevice"));

        println!("pointer size {} bytes", std::mem::size_of::<usize>());
        println!("Init -> {}", init());
        let blackwidow = Guid(0x2ea1bb63, 0xca28, 0x428d, [0x9f, 0x06, 0x19, 0x6b, 0x88, 0x33, 0x0b, 0xbb]);
        let mut info = DeviceInfo::default();
        println!("QueryDevice(keyboard) -> {} type={} connected={}", query(blackwidow, &mut info), info.device_type, info.connected);
        let color: u32 = 0x00FFFF00; // cyan as COLORREF
        let mut id = Guid::default();
        println!("CreateKeyboardEffect(static, stored) -> {} id={:x?}", keyboard(4, &color as *const u32 as *const c_void, &mut id), id);
        println!("SetEffect(id) -> {}", set(id));
        std::thread::sleep(std::time::Duration::from_millis(800));
        println!("DeleteEffect(id) -> {}", delete(id));
        println!("SetEffect(deleted) -> {} (expect 1168)", set(id));
        println!("UnInit -> {}", uninit());
    }
}
