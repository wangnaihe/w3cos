//! Version gate between the system runtime and an AOT application plugin.
//!
//! The packed value is not a Rust compiler ABI. Apps and `libw3cos_runtime.so`
//! still have to be built by the same rustc. This integer is the product
//! contract: a host loads an app when the major matches and the host minor is
//! at least the minor the app was compiled against.

/// Major version. Breaking changes to the app entry symbols bump this.
pub const W3COS_RUNTIME_ABI_MAJOR: u32 = 1;
/// Minor version. Additive, backwards-compatible entry points bump this.
pub const W3COS_RUNTIME_ABI_MINOR: u32 = 0;

pub const fn pack_abi(major: u32, minor: u32) -> u32 {
    (major << 16) | (minor & 0xffff)
}

pub const W3COS_RUNTIME_ABI: u32 = pack_abi(W3COS_RUNTIME_ABI_MAJOR, W3COS_RUNTIME_ABI_MINOR);

pub const fn abi_major(packed: u32) -> u32 {
    packed >> 16
}

pub const fn abi_minor(packed: u32) -> u32 {
    packed & 0xffff
}

/// `host` is the runtime that is already loaded. `app` is the version the
/// plugin was compiled against.
pub const fn abi_compatible(host: u32, app: u32) -> bool {
    abi_major(host) == abi_major(app) && abi_minor(host) >= abi_minor(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_abi_is_one_point_zero() {
        assert_eq!(W3COS_RUNTIME_ABI, 0x0001_0000);
        assert_eq!(abi_major(W3COS_RUNTIME_ABI), 1);
        assert_eq!(abi_minor(W3COS_RUNTIME_ABI), 0);
    }

    #[test]
    fn host_accepts_same_or_older_minor_and_rejects_other_majors() {
        let host = pack_abi(1, 2);
        assert!(abi_compatible(host, pack_abi(1, 2)));
        assert!(abi_compatible(host, pack_abi(1, 0)));
        assert!(abi_compatible(host, pack_abi(1, 1)));
        assert!(!abi_compatible(host, pack_abi(1, 3)));
        assert!(!abi_compatible(host, pack_abi(2, 0)));
        assert!(!abi_compatible(pack_abi(1, 0), pack_abi(2, 0)));
    }
}
