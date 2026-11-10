//! DNS leak protection on Windows via the Windows Filtering Platform.
//!
//! Windows sends DNS queries to every adapter's DNS servers in parallel
//! ("smart multi-homed name resolution"). A DNS server inside the LAN is
//! reachable on-link, outside the tunnel, so queries leak to the ISP.
//! We add, in our own sublayer:
//!   * permit  port 53 for Mosaic itself and its engines (weight 15)
//!   * permit  port 53 out of the TUN interface               (weight 14)
//!   * block   port 53 everywhere else                         (weight 10)
//! for both IPv4 and IPv6. The WFP session is *dynamic*: Windows removes all
//! these filters as soon as the handle is closed — including when Mosaic
//! crashes — so the machine can never be left without DNS.
//!
//! Struct layouts follow fwpmtypes.h (checked against wireguard-windows).

use std::ffi::c_void;
use std::path::PathBuf;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    Guid { d1, d2, d3, d4 }
}

const LAYER_ALE_AUTH_CONNECT_V4: Guid = guid(0xc38d57d1, 0x05a7, 0x4c33, [0x90, 0x4f, 0x7f, 0xbc, 0xee, 0xe6, 0x0e, 0x82]);
const LAYER_ALE_AUTH_CONNECT_V6: Guid = guid(0x4a72393b, 0x319f, 0x44bc, [0x84, 0xc3, 0xba, 0x54, 0xdc, 0xb3, 0xb6, 0xb4]);
const CONDITION_IP_REMOTE_PORT: Guid = guid(0xc35a604d, 0xd22b, 0x4e1a, [0x91, 0xb4, 0x68, 0xf6, 0x74, 0xee, 0x67, 0x4b]);
const CONDITION_ALE_APP_ID: Guid = guid(0xd78e1e87, 0x8644, 0x4ea5, [0x94, 0x37, 0xd8, 0x09, 0xec, 0xef, 0xc9, 0x71]);
const CONDITION_IP_REMOTE_ADDRESS: Guid = guid(0xb235ae9a, 0x1d64, 0x49b8, [0xa4, 0x4c, 0x5f, 0xf3, 0xd9, 0x09, 0x50, 0x45]);
const CONDITION_FLAGS: Guid = guid(0x632ce23b, 0x5167, 0x435c, [0x86, 0xd7, 0xe9, 0x03, 0x68, 0x4a, 0xa8, 0x0c]);
const CONDITION_IP_LOCAL_INTERFACE: Guid = guid(0x4cd62a49, 0x59c3, 0x4969, [0xb7, 0xf3, 0xbd, 0xa5, 0xd3, 0x28, 0x90, 0xa4]);

const FWP_UINT8: u32 = 1;
const FWP_UINT16: u32 = 2;
const FWP_UINT32: u32 = 3;
const FWP_UINT64: u32 = 4;
const FWP_BYTE_BLOB_TYPE: u32 = 12;
const FWP_MATCH_EQUAL: u32 = 0;
const FWP_ACTION_BLOCK: u32 = 0x1001;
const FWP_ACTION_PERMIT: u32 = 0x1002;
const FWPM_SESSION_FLAG_DYNAMIC: u32 = 1;
const FWP_V4_ADDR_MASK: u32 = 0x100;
const FWP_V6_ADDR_MASK: u32 = 0x101;
const FWP_MATCH_FLAGS_ALL_SET: u32 = 6;
const FWP_CONDITION_FLAG_IS_LOOPBACK: u32 = 1;
const FWPM_SUBLAYER_FLAG_PERSISTENT: u32 = 1;
const FWPM_FILTER_FLAG_PERSISTENT: u32 = 1;
const RPC_C_AUTHN_DEFAULT: u32 = 0xFFFF_FFFF;

#[repr(C)]
struct DisplayData {
    name: *const u16,
    description: *const u16,
}

#[repr(C)]
struct ByteBlob {
    size: u32,
    data: *mut u8,
}

/// FWP_VALUE0 / FWP_CONDITION_VALUE0: a type tag and an 8-byte union.
#[repr(C)]
struct Value {
    ty: u32,
    v: u64,
}

#[repr(C)]
struct Session0 {
    key: Guid,
    display: DisplayData,
    flags: u32,
    txn_wait_ms: u32,
    process_id: u32,
    sid: *mut c_void,
    username: *mut u16,
    kernel_mode: i32,
}

#[repr(C)]
struct Sublayer0 {
    key: Guid,
    display: DisplayData,
    flags: u32,
    provider_key: *mut Guid,
    provider_data: ByteBlob,
    weight: u16,
}

#[repr(C)]
struct Condition0 {
    field: Guid,
    match_type: u32,
    value: Value,
}

#[repr(C)]
struct Action0 {
    ty: u32,
    filter_type: Guid,
}

#[repr(C)]
union FilterCtx {
    raw: u64,
    key: Guid,
}

#[repr(C)]
struct Filter0 {
    key: Guid,
    display: DisplayData,
    flags: u32,
    provider_key: *mut Guid,
    provider_data: ByteBlob,
    layer: Guid,
    sublayer: Guid,
    weight: Value,
    num_conditions: u32,
    conditions: *const Condition0,
    action: Action0,
    ctx: FilterCtx,
    reserved: *mut Guid,
    filter_id: u64,
    effective_weight: Value,
}

#[link(name = "fwpuclnt")]
extern "system" {
    fn FwpmEngineOpen0(server: *const u16, authn: u32, auth_identity: *const c_void, session: *const Session0, handle: *mut isize) -> u32;
    fn FwpmEngineClose0(handle: isize) -> u32;
    fn FwpmTransactionBegin0(handle: isize, flags: u32) -> u32;
    fn FwpmTransactionCommit0(handle: isize) -> u32;
    fn FwpmTransactionAbort0(handle: isize) -> u32;
    fn FwpmSubLayerAdd0(handle: isize, sublayer: *const Sublayer0, sd: *const c_void) -> u32;
    fn FwpmFilterAdd0(handle: isize, filter: *const Filter0, sd: *const c_void, id: *mut u64) -> u32;
    fn FwpmGetAppIdFromFileName0(file: *const u16, app_id: *mut *mut ByteBlob) -> u32;
    fn FwpmFreeMemory0(p: *mut *mut c_void);
    fn FwpmFilterDeleteByKey0(handle: isize, key: *const Guid) -> u32;
    fn FwpmSubLayerDeleteByKey0(handle: isize, key: *const Guid) -> u32;
}

#[repr(C)]
struct V4AddrMask {
    addr: u32,
    mask: u32,
}

#[repr(C)]
struct V6AddrMask {
    addr: [u8; 16],
    prefix: u8,
}

#[link(name = "iphlpapi")]
extern "system" {
    fn ConvertInterfaceIndexToLuid(index: u32, luid: *mut u64) -> u32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn check(rc: u32, what: &str) -> Result<(), String> {
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("WFP {what} failed: 0x{rc:08x}"))
    }
}

/// Active DNS leak protection; dropping it removes every filter.
pub struct DnsGuard {
    handle: isize,
}

impl Drop for DnsGuard {
    fn drop(&mut self) {
        unsafe {
            FwpmEngineClose0(self.handle);
        }
    }
}

struct AppId(*mut ByteBlob);

impl Drop for AppId {
    fn drop(&mut self) {
        unsafe {
            let mut p = self.0 as *mut c_void;
            FwpmFreeMemory0(&mut p);
        }
    }
}

impl DnsGuard {
    /// `tun_ifindex`: interface index of the TUN adapter. `apps`: executables
    /// allowed to send DNS anywhere (Mosaic and its engines).
    pub fn install(tun_ifindex: Option<u32>, apps: &[PathBuf]) -> Result<DnsGuard, String> {
        let mut luid: u64 = 0;
        if let Some(idx) = tun_ifindex {
            if unsafe { ConvertInterfaceIndexToLuid(idx, &mut luid) } != 0 {
                luid = 0;
            }
        }

        let name = wide("Mosaic DNS leak protection");
        let session = Session0 {
            key: Guid::default(),
            display: DisplayData { name: name.as_ptr(), description: name.as_ptr() },
            flags: FWPM_SESSION_FLAG_DYNAMIC,
            txn_wait_ms: 0,
            process_id: 0,
            sid: std::ptr::null_mut(),
            username: std::ptr::null_mut(),
            kernel_mode: 0,
        };
        let mut handle: isize = 0;
        check(unsafe { FwpmEngineOpen0(std::ptr::null(), RPC_C_AUTHN_DEFAULT, std::ptr::null(), &session, &mut handle) }, "open")?;
        let guard = DnsGuard { handle };

        // Unique sublayer key per run (the dynamic session owns it).
        let seed = crate::util::now_ms() ^ ((std::process::id() as u64) << 32);
        let sublayer_key = guid(0x6d6f7361, 0x6963, 0x0d45, seed.to_le_bytes());

        check(unsafe { FwpmTransactionBegin0(handle, 0) }, "transaction")?;
        let res = guard.add_all(sublayer_key, luid, apps, &name);
        match res {
            Ok(()) => {
                check(unsafe { FwpmTransactionCommit0(handle) }, "commit")?;
                Ok(guard)
            }
            Err(e) => {
                unsafe {
                    FwpmTransactionAbort0(handle);
                }
                Err(e)
            }
        }
    }

    fn add_all(&self, sublayer_key: Guid, luid: u64, apps: &[PathBuf], name: &[u16]) -> Result<(), String> {
        let sublayer = Sublayer0 {
            key: sublayer_key,
            display: DisplayData { name: name.as_ptr(), description: name.as_ptr() },
            flags: 0,
            provider_key: std::ptr::null_mut(),
            provider_data: ByteBlob { size: 0, data: std::ptr::null_mut() },
            weight: 0xFFFF,
        };
        check(unsafe { FwpmSubLayerAdd0(self.handle, &sublayer, std::ptr::null()) }, "sublayer")?;

        // Keep app ids alive until the filters are added.
        let mut app_ids = Vec::new();
        for app in apps {
            let path = wide(&app.to_string_lossy());
            let mut blob: *mut ByteBlob = std::ptr::null_mut();
            if unsafe { FwpmGetAppIdFromFileName0(path.as_ptr(), &mut blob) } == 0 && !blob.is_null() {
                app_ids.push(AppId(blob));
            }
        }

        let port53 = Condition0 { field: CONDITION_IP_REMOTE_PORT, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT16, v: 53 } };
        let luid_ptr: *const u64 = &luid;
        for layer in [LAYER_ALE_AUTH_CONNECT_V4, LAYER_ALE_AUTH_CONNECT_V6] {
            for app in &app_ids {
                let conds = [
                    Condition0 { field: CONDITION_IP_REMOTE_PORT, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT16, v: 53 } },
                    Condition0 { field: CONDITION_ALE_APP_ID, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_BYTE_BLOB_TYPE, v: app.0 as u64 } },
                ];
                self.add_filter(layer, sublayer_key, 15, &conds, FWP_ACTION_PERMIT, name)?;
            }
            if luid != 0 {
                let tun = [
                    Condition0 { field: CONDITION_IP_REMOTE_PORT, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT16, v: 53 } },
                    Condition0 { field: CONDITION_IP_LOCAL_INTERFACE, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT64, v: luid_ptr as u64 } },
                ];
                self.add_filter(layer, sublayer_key, 14, &tun, FWP_ACTION_PERMIT, name)?;
            }
            // DNS to the TUN's DNS address (172.19.0.2) is always allowed: it can only go into
            // the tunnel, and it does not depend on finding the right adapter.
            if layer == LAYER_ALE_AUTH_CONNECT_V4 {
                let peer = [
                    Condition0 { field: CONDITION_IP_REMOTE_PORT, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT16, v: 53 } },
                    Condition0 {
                        field: CONDITION_IP_REMOTE_ADDRESS,
                        match_type: FWP_MATCH_EQUAL,
                        value: Value { ty: FWP_UINT32, v: u32::from(std::net::Ipv4Addr::new(172, 19, 0, 2)) as u64 },
                    },
                ];
                self.add_filter(layer, sublayer_key, 14, &peer, FWP_ACTION_PERMIT, name)?;
            }
            self.add_filter(layer, sublayer_key, 10, std::slice::from_ref(&port53), FWP_ACTION_BLOCK, name)?;
        }
        drop(app_ids);
        Ok(())
    }

    fn add_filter(&self, layer: Guid, sublayer: Guid, weight: u8, conds: &[Condition0], action: u32, name: &[u16]) -> Result<(), String> {
        let f = Filter0 {
            key: Guid::default(),
            display: DisplayData { name: name.as_ptr(), description: name.as_ptr() },
            flags: 0,
            provider_key: std::ptr::null_mut(),
            provider_data: ByteBlob { size: 0, data: std::ptr::null_mut() },
            layer,
            sublayer,
            weight: Value { ty: FWP_UINT8, v: weight as u64 },
            num_conditions: conds.len() as u32,
            conditions: conds.as_ptr(),
            action: Action0 { ty: action, filter_type: Guid::default() },
            ctx: FilterCtx { raw: 0 },
            reserved: std::ptr::null_mut(),
            filter_id: 0,
            effective_weight: Value { ty: 0, v: 0 },
        };
        let mut id = 0u64;
        check(unsafe { FwpmFilterAdd0(self.handle, &f, std::ptr::null(), &mut id) }, "filter")
    }
}

// ---------------------------------------------------------------- kill switch
//
// Unlike DNS protection these filters are *persistent*: they outlive Mosaic. If Mosaic
// crashes or the tunnel cannot come back, nothing but Mosaic itself, its engines, the
// loopback, the local network and DHCP can reach the network. They are removed when the
// user disconnects, quits, turns the kill switch off or presses "Unblock".
//
// Fixed keys (sublayer + numbered filters) let a later run find and remove them.

const KS_SUBLAYER: Guid = guid(0x6d6f7361, 0x6b73, 0x0001, *b"killswch");
const KS_MAX_FILTERS: u16 = 96;

fn ks_key(i: u16) -> Guid {
    guid(0x6d6f7361, 0x6b73, 0x0100 + i, *b"killswch")
}

struct Engine(isize);

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            FwpmEngineClose0(self.0);
        }
    }
}

fn open_persistent() -> Result<Engine, String> {
    let name = wide("Mosaic kill switch");
    let session = Session0 {
        key: Guid::default(),
        display: DisplayData { name: name.as_ptr(), description: name.as_ptr() },
        flags: 0,
        txn_wait_ms: 0,
        process_id: 0,
        sid: std::ptr::null_mut(),
        username: std::ptr::null_mut(),
        kernel_mode: 0,
    };
    let mut handle: isize = 0;
    check(unsafe { FwpmEngineOpen0(std::ptr::null(), RPC_C_AUTHN_DEFAULT, std::ptr::null(), &session, &mut handle) }, "open")?;
    Ok(Engine(handle))
}

fn ks_delete_all(h: isize) {
    for i in 0..KS_MAX_FILTERS {
        let k = ks_key(i);
        unsafe {
            FwpmFilterDeleteByKey0(h, &k);
        }
    }
    unsafe {
        FwpmSubLayerDeleteByKey0(h, &KS_SUBLAYER);
    }
}

/// Remove the kill switch filters (no error if there are none).
pub fn killswitch_remove() -> Result<(), String> {
    let e = open_persistent()?;
    check(unsafe { FwpmTransactionBegin0(e.0, 0) }, "transaction")?;
    ks_delete_all(e.0);
    check(unsafe { FwpmTransactionCommit0(e.0) }, "commit")
}

/// Install (or refresh) the kill switch. `tun_ifindex`: the TUN adapter — traffic into it is
/// allowed. `apps`: Mosaic and its engines, allowed to reach the network directly.
pub fn killswitch_install(tun_ifindex: Option<u32>, apps: &[PathBuf]) -> Result<(), String> {
    let mut luid: u64 = 0;
    if let Some(idx) = tun_ifindex {
        if unsafe { ConvertInterfaceIndexToLuid(idx, &mut luid) } != 0 {
            luid = 0;
        }
    }
    let e = open_persistent()?;
    check(unsafe { FwpmTransactionBegin0(e.0, 0) }, "transaction")?;
    let res = (|| -> Result<(), String> {
        ks_delete_all(e.0);
        let name = wide("Mosaic kill switch");
        let sublayer = Sublayer0 {
            key: KS_SUBLAYER,
            display: DisplayData { name: name.as_ptr(), description: name.as_ptr() },
            flags: FWPM_SUBLAYER_FLAG_PERSISTENT,
            provider_key: std::ptr::null_mut(),
            provider_data: ByteBlob { size: 0, data: std::ptr::null_mut() },
            weight: 0xFFFE,
        };
        check(unsafe { FwpmSubLayerAdd0(e.0, &sublayer, std::ptr::null()) }, "sublayer")?;
        let mut app_ids = Vec::new();
        for app in apps {
            let path = wide(&app.to_string_lossy());
            let mut blob: *mut ByteBlob = std::ptr::null_mut();
            if unsafe { FwpmGetAppIdFromFileName0(path.as_ptr(), &mut blob) } == 0 && !blob.is_null() {
                app_ids.push(AppId(blob));
            }
        }
        let mut n: u16 = 0;
        let mut add = |layer: Guid, weight: u8, conds: &[Condition0], action: u32| -> Result<(), String> {
            if n >= KS_MAX_FILTERS {
                return Err("too many kill switch filters".into());
            }
            let f = Filter0 {
                key: ks_key(n),
                display: DisplayData { name: name.as_ptr(), description: name.as_ptr() },
                flags: FWPM_FILTER_FLAG_PERSISTENT,
                provider_key: std::ptr::null_mut(),
                provider_data: ByteBlob { size: 0, data: std::ptr::null_mut() },
                layer,
                sublayer: KS_SUBLAYER,
                weight: Value { ty: FWP_UINT8, v: weight as u64 },
                num_conditions: conds.len() as u32,
                conditions: if conds.is_empty() { std::ptr::null() } else { conds.as_ptr() },
                action: Action0 { ty: action, filter_type: Guid::default() },
                ctx: FilterCtx { raw: 0 },
                reserved: std::ptr::null_mut(),
                filter_id: 0,
                effective_weight: Value { ty: 0, v: 0 },
            };
            n += 1;
            let mut id = 0u64;
            check(unsafe { FwpmFilterAdd0(e.0, &f, std::ptr::null(), &mut id) }, "filter")
        };
        let luid_ptr: *const u64 = &luid;
        let lan4 = [(10u32 << 24, 8u32), (0xAC10_0000, 12), (0xC0A8_0000, 16), (0xA9FE_0000, 16), (0xE000_0000, 4), (0xFFFF_FFFF, 32)]
            .map(|(addr, bits)| V4AddrMask { addr, mask: if bits == 0 { 0 } else { u32::MAX << (32 - bits) } });
        let lan6 = [([0xfe, 0x80], 10u8), ([0xfc, 0x00], 7), ([0xff, 0x00], 8)].map(|(p, prefix)| {
            let mut addr = [0u8; 16];
            addr[..2].copy_from_slice(&p);
            V6AddrMask { addr, prefix }
        });
        for (layer, v6) in [(LAYER_ALE_AUTH_CONNECT_V4, false), (LAYER_ALE_AUTH_CONNECT_V6, true)] {
            for app in &app_ids {
                let c = [Condition0 { field: CONDITION_ALE_APP_ID, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_BYTE_BLOB_TYPE, v: app.0 as u64 } }];
                add(layer, 15, &c, FWP_ACTION_PERMIT)?;
            }
            if luid != 0 {
                let c = [Condition0 { field: CONDITION_IP_LOCAL_INTERFACE, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT64, v: luid_ptr as u64 } }];
                add(layer, 14, &c, FWP_ACTION_PERMIT)?;
            }
            let lo = [Condition0 { field: CONDITION_FLAGS, match_type: FWP_MATCH_FLAGS_ALL_SET, value: Value { ty: FWP_UINT32, v: FWP_CONDITION_FLAG_IS_LOOPBACK as u64 } }];
            add(layer, 13, &lo, FWP_ACTION_PERMIT)?;
            // DNS to the local network would leak names: blocked even though the LAN is allowed.
            let dns = [Condition0 { field: CONDITION_IP_REMOTE_PORT, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT16, v: 53 } }];
            add(layer, 12, &dns, FWP_ACTION_BLOCK)?;
            let dhcp = [Condition0 { field: CONDITION_IP_REMOTE_PORT, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_UINT16, v: if v6 { 547 } else { 67 } } }];
            add(layer, 11, &dhcp, FWP_ACTION_PERMIT)?;
            if v6 {
                for m in &lan6 {
                    let c = [Condition0 { field: CONDITION_IP_REMOTE_ADDRESS, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_V6_ADDR_MASK, v: m as *const V6AddrMask as u64 } }];
                    add(layer, 10, &c, FWP_ACTION_PERMIT)?;
                }
            } else {
                for m in &lan4 {
                    let c = [Condition0 { field: CONDITION_IP_REMOTE_ADDRESS, match_type: FWP_MATCH_EQUAL, value: Value { ty: FWP_V4_ADDR_MASK, v: m as *const V4AddrMask as u64 } }];
                    add(layer, 10, &c, FWP_ACTION_PERMIT)?;
                }
            }
            add(layer, 0, &[], FWP_ACTION_BLOCK)?;
        }
        drop(app_ids);
        Ok(())
    })();
    match res {
        Ok(()) => check(unsafe { FwpmTransactionCommit0(e.0) }, "commit"),
        Err(err) => {
            unsafe {
                FwpmTransactionAbort0(e.0);
            }
            Err(err)
        }
    }
}

/// Interface index of a network adapter by its alias.
pub fn interface_index(alias: &str) -> Result<u32, String> {
    use std::os::windows::process::CommandExt;
    let script = format!("(Get-NetAdapter -Name '{alias}' -ErrorAction Stop).ifIndex");
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| format!("adapter '{alias}' not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_windows_headers() {
        // x64 sizes from fwpmtypes.h / fwptypes.h
        assert_eq!(std::mem::size_of::<Guid>(), 16);
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(std::mem::size_of::<Condition0>(), 40);
        assert_eq!(std::mem::size_of::<Action0>(), 20);
        assert_eq!(std::mem::size_of::<Filter0>(), 200);
        assert_eq!(std::mem::size_of::<Sublayer0>(), 72);
        assert_eq!(std::mem::size_of::<Session0>(), 72);
    }
}
