//! Restarting a device's USB connection: what unplugging and replugging it
//! does, without touching the cable. The Razer ARGB controller can come up
//! from a power loss with its LEDs stuck on its own color while it accepts
//! and acknowledges every command, so nothing sent over USB clears it, but a
//! restart does.

use std::ffi::CStr;
use std::mem::size_of;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent, CM_Locate_DevNodeW, SetupDiCallClassInstaller,
    SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiGetDeviceInstallParamsW, SetupDiOpenDeviceInfoW,
    SetupDiSetClassInstallParamsW, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS, DICS_FLAG_CONFIGSPECIFIC, DICS_PROPCHANGE, DIF_PROPERTYCHANGE,
    DI_NEEDREBOOT, DI_NEEDRESTART, HDEVINFO, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SP_PROPCHANGE_PARAMS,
};
use windows_sys::Win32::Devices::Properties::DEVPKEY_Device_InstanceId;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

/// Restart the USB device behind the HID interface at `hid_path`, with all of
/// its interfaces. Takes a second or two and needs administrator rights,
/// which the service has. Close the device first: Windows won't stop a
/// device while it is open and would ask for a reboot instead.
pub fn restart(hid_path: &CStr) -> Result<(), String> {
    let mut node = locate(&interface_instance(hid_path)?)?;
    // HID collection -> USB interface ("...&MI_00") -> the USB device itself.
    loop {
        let id = String::from_utf16_lossy(&device_id(node)?).to_ascii_uppercase();
        if !(id.starts_with("HID\\") || id.contains("&MI_")) {
            break;
        }
        let mut parent = 0;
        cm(unsafe { CM_Get_Parent(&mut parent, node, 0) }, "find the parent device")?;
        node = parent;
    }
    property_change(&device_id(node)?)
}

fn cm(result: u32, what: &str) -> Result<(), String> {
    match result {
        CR_SUCCESS => Ok(()),
        e => Err(format!("could not {what} (CONFIGRET {e})")),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The device instance ("HID\VID_...\...") that owns an interface path.
fn interface_instance(path: &CStr) -> Result<Vec<u16>, String> {
    let path = wide(&path.to_string_lossy());
    let mut buf = [0u16; 512];
    let mut size = size_of::<[u16; 512]>() as u32;
    let mut kind = 0;
    cm(
        unsafe {
            CM_Get_Device_Interface_PropertyW(path.as_ptr(), &DEVPKEY_Device_InstanceId, &mut kind, buf.as_mut_ptr().cast(), &mut size, 0)
        },
        "look up the device",
    )?;
    Ok(buf.into_iter().take_while(|&c| c != 0).chain(Some(0)).collect())
}

fn locate(instance: &[u16]) -> Result<u32, String> {
    let mut node = 0;
    cm(unsafe { CM_Locate_DevNodeW(&mut node, instance.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) }, "find the device")?;
    Ok(node)
}

/// NUL-terminated instance id of a device node.
fn device_id(node: u32) -> Result<Vec<u16>, String> {
    let mut buf = [0u16; 512];
    cm(unsafe { CM_Get_Device_IDW(node, buf.as_mut_ptr(), buf.len() as u32 - 1, 0) }, "read the device id")?;
    Ok(buf.into_iter().take_while(|&c| c != 0).chain(Some(0)).collect())
}

struct InfoSet(HDEVINFO);

impl Drop for InfoSet {
    fn drop(&mut self) {
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// Stop and start a device, as `pnputil /restart-device` and `devcon
/// restart` do. Unlike disabling and enabling it, nothing is persisted, so
/// the device can't be left disabled.
fn property_change(instance: &[u16]) -> Result<(), String> {
    let os = |what: &str| format!("could not {what}: {}", std::io::Error::last_os_error());
    unsafe {
        let set = SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut());
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(os("list devices"));
        }
        let set = InfoSet(set);
        let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
        data.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        if SetupDiOpenDeviceInfoW(set.0, instance.as_ptr(), std::ptr::null_mut(), 0, &mut data) == 0 {
            return Err(os("open the device"));
        }
        let params = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: DICS_PROPCHANGE,
            Scope: DICS_FLAG_CONFIGSPECIFIC,
            HwProfile: 0,
        };
        if SetupDiSetClassInstallParamsW(set.0, &data, &params.ClassInstallHeader, size_of::<SP_PROPCHANGE_PARAMS>() as u32) == 0 {
            return Err(os("prepare the restart"));
        }
        if SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.0, &data) == 0 {
            return Err(os("restart the device"));
        }
        let mut install: SP_DEVINSTALL_PARAMS_W = std::mem::zeroed();
        install.cbSize = size_of::<SP_DEVINSTALL_PARAMS_W>() as u32;
        if SetupDiGetDeviceInstallParamsW(set.0, &data, &mut install) != 0 && install.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0 {
            return Err("Windows could not stop the device and will restart it on the next reboot".into());
        }
    }
    Ok(())
}
