//! Version gate between the system runtime and an AOT application plugin.
//!
//! The packed value is the product contract, not a Rust compiler ABI. A host
//! loads a plugin when the major matches and the host minor is at least the
//! minor the plugin was compiled against. Behavioral plugins cross this
//! boundary with the C calls in `w3cos-runtime::plugin_abi`.

/// Major version. Breaking changes to the app entry symbols bump this.
pub const W3COS_RUNTIME_ABI_MAJOR: u32 = 1;
/// Minor version. Additive, backwards-compatible entry points bump this.
/// 1 adds the behavioral plugin calls (`w3cos_plugin_*`, `w3cos_runtime_run_plugin`).
pub const W3COS_RUNTIME_ABI_MINOR: u32 = 1;

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

/// Returned by node constructors when the call is rejected.
pub const W3COS_PLUGIN_NODE_NONE: u32 = u32::MAX;

/// Packed RGBA, `0xRRGGBBAA`. Alpha 0 leaves the runtime default for that color.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W3cosPluginStyle {
    pub background_rgba: u32,
    pub color_rgba: u32,
    pub font_size: f32,
    pub gap: f32,
    pub padding: f32,
    pub radius: f32,
    pub width: f32,
    pub height: f32,
    pub grow: f32,
    pub align_center: u8,
    pub justify_center: u8,
}

impl Default for W3cosPluginStyle {
    fn default() -> Self {
        Self {
            background_rgba: 0,
            color_rgba: 0,
            font_size: 0.0,
            gap: 0.0,
            padding: 0.0,
            radius: 0.0,
            width: 0.0,
            height: 0.0,
            grow: 0.0,
            align_center: 0,
            justify_center: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_abi_is_one_point_one() {
        assert_eq!(W3COS_RUNTIME_ABI, 0x0001_0001);
        assert_eq!(abi_major(W3COS_RUNTIME_ABI), 1);
        assert_eq!(abi_minor(W3COS_RUNTIME_ABI), 1);
    }

    #[test]
    fn plugin_style_pod_layout_stays_40_bytes() {
        assert_eq!(std::mem::size_of::<W3cosPluginStyle>(), 40);
        assert_eq!(std::mem::align_of::<W3cosPluginStyle>(), 4);
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
