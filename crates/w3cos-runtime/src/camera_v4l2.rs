//! Linux Video4Linux2 camera adapter for `navigator.mediaDevices`.
//!
//! Enumerates `/dev/video*` capture nodes with `VIDIOC_QUERYCAP` and grabs
//! frames with memory-mapped streaming I/O (`VIDIOC_REQBUFS` / `QBUF` /
//! `DQBUF`), negotiating YUYV and falling back to MJPEG. Only the kernel
//! ioctl ABI is used, so no system library is linked.

use std::ffi::CString;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{CameraError, CameraFrame, CameraInfo};

const V4L2_CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
const V4L2_CAP_STREAMING: u32 = 0x0400_0000;
const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_MEMORY_MMAP: u32 = 1;
const V4L2_FIELD_ANY: u32 = 0;

pub(crate) const PIX_FMT_YUYV: u32 = fourcc(b"YUYV");
pub(crate) const PIX_FMT_MJPEG: u32 = fourcc(b"MJPG");

const fn fourcc(code: &[u8; 4]) -> u32 {
    (code[0] as u32) | ((code[1] as u32) << 8) | ((code[2] as u32) << 16) | ((code[3] as u32) << 24)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
union FormatUnion {
    pix: PixFormat,
    raw: [u8; 200],
    // `struct v4l2_window` contains pointers, which sets the union alignment.
    _align: [*mut libc::c_void; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Format {
    type_: u32,
    fmt: FormatUnion,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RequestBuffers {
    count: u32,
    type_: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Timecode {
    type_: u32,
    flags: u32,
    frames: u8,
    seconds: u8,
    minutes: u8,
    hours: u8,
    userbits: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
union BufferMemory {
    offset: u32,
    userptr: libc::c_ulong,
    planes: *mut libc::c_void,
    fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: libc::timeval,
    timecode: Timecode,
    sequence: u32,
    memory: u32,
    m: BufferMemory,
    length: u32,
    reserved2: u32,
    request_fd: i32,
}

// _IOC encoding shared by x86, ARM and RISC-V Linux.
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;

const fn ioc(direction: u64, number: u64, size: usize) -> u64 {
    (direction << 30) | ((size as u64) << 16) | ((b'V' as u64) << 8) | number
}

const VIDIOC_QUERYCAP: u64 = ioc(IOC_READ, 0, size_of::<Capability>());
const VIDIOC_S_FMT: u64 = ioc(IOC_READ | IOC_WRITE, 5, size_of::<Format>());
const VIDIOC_REQBUFS: u64 = ioc(IOC_READ | IOC_WRITE, 8, size_of::<RequestBuffers>());
const VIDIOC_QUERYBUF: u64 = ioc(IOC_READ | IOC_WRITE, 9, size_of::<Buffer>());
const VIDIOC_QBUF: u64 = ioc(IOC_READ | IOC_WRITE, 15, size_of::<Buffer>());
const VIDIOC_DQBUF: u64 = ioc(IOC_READ | IOC_WRITE, 17, size_of::<Buffer>());
const VIDIOC_STREAMON: u64 = ioc(IOC_WRITE, 18, size_of::<libc::c_int>());
const VIDIOC_STREAMOFF: u64 = ioc(IOC_WRITE, 19, size_of::<libc::c_int>());

struct Fd(libc::c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: the descriptor was opened by `open_device` and is owned here.
        unsafe {
            libc::close(self.0);
        }
    }
}

fn os_error(what: &str, path: &Path) -> CameraError {
    let error = std::io::Error::last_os_error();
    let name = match error.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => "NotAllowedError",
        Some(libc::ENOENT) | Some(libc::ENODEV) | Some(libc::ENXIO) => "NotFoundError",
        _ => "NotReadableError",
    };
    CameraError {
        name,
        message: format!("{what} {}: {error}", path.display()),
    }
}

fn open_device(path: &Path) -> Result<Fd, CameraError> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| CameraError {
        name: "NotFoundError",
        message: format!("invalid camera path {}", path.display()),
    })?;
    // SAFETY: `c_path` is a valid NUL-terminated string.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(os_error("cannot open camera", path));
    }
    Ok(Fd(fd))
}

/// # Safety
/// `arg` must point to the structure the request number encodes.
unsafe fn ioctl<T>(fd: &Fd, request: u64, arg: *mut T) -> libc::c_int {
    loop {
        // SAFETY: forwarded from the caller.
        let result = unsafe { libc::ioctl(fd.0, request as _, arg) };
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return result;
    }
}

fn c_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn query(fd: &Fd) -> Option<Capability> {
    // SAFETY: all-zero is a valid `v4l2_capability`.
    let mut capability: Capability = unsafe { std::mem::zeroed() };
    // SAFETY: VIDIOC_QUERYCAP takes a `v4l2_capability`.
    (unsafe { ioctl(fd, VIDIOC_QUERYCAP, &mut capability) } == 0).then_some(capability)
}

fn effective_caps(capability: &Capability) -> u32 {
    if capability.capabilities & V4L2_CAP_DEVICE_CAPS != 0 {
        capability.device_caps
    } else {
        capability.capabilities
    }
}

/// Lists streaming video-capture nodes under `dev_root` (normally `/dev`).
pub fn list(dev_root: &Path) -> Vec<CameraInfo> {
    let Ok(entries) = std::fs::read_dir(dev_root) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("video"))
                .is_some_and(|number| {
                    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
                })
        })
        .collect();
    paths.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name[5..].parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    let mut cameras = Vec::new();
    for path in paths {
        let Ok(fd) = open_device(&path) else {
            continue;
        };
        let Some(capability) = query(&fd) else {
            continue;
        };
        let caps = effective_caps(&capability);
        if caps & V4L2_CAP_VIDEO_CAPTURE == 0 || caps & V4L2_CAP_STREAMING == 0 {
            continue;
        }
        let card = c_text(&capability.card);
        let bus = c_text(&capability.bus_info);
        cameras.push(CameraInfo {
            label: if card.is_empty() {
                path.display().to_string()
            } else {
                card.clone()
            },
            device_key: format!("v4l2:{bus}:{card}:{}", path.display()),
            group_key: format!("v4l2:{bus}"),
            path,
        });
    }
    cameras
}

struct Mapping {
    pointer: *mut libc::c_void,
    length: usize,
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping was created by `mmap` with this length.
        unsafe {
            libc::munmap(self.pointer, self.length);
        }
    }
}

fn buffer(index: u32) -> Buffer {
    // SAFETY: all-zero is a valid `v4l2_buffer`.
    let mut buffer: Buffer = unsafe { std::mem::zeroed() };
    buffer.index = index;
    buffer.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    buffer.memory = V4L2_MEMORY_MMAP;
    buffer
}

/// Captures one frame from `path`, preferring `width`×`height`.
pub fn capture(path: &Path, width: u32, height: u32) -> Result<CameraFrame, CameraError> {
    let fd = open_device(path)?;
    let mut negotiated = None;
    for pixel_format in [PIX_FMT_YUYV, PIX_FMT_MJPEG] {
        // SAFETY: all-zero is a valid `v4l2_format`.
        let mut format: Format = unsafe { std::mem::zeroed() };
        format.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        format.fmt.pix = PixFormat {
            width,
            height,
            pixelformat: pixel_format,
            field: V4L2_FIELD_ANY,
            // SAFETY: all-zero is valid for the remaining plain integers.
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: VIDIOC_S_FMT takes a `v4l2_format`.
        if unsafe { ioctl(&fd, VIDIOC_S_FMT, &mut format) } == 0 {
            // SAFETY: the driver filled the `pix` member for capture types.
            let pix = unsafe { format.fmt.pix };
            if pix.pixelformat == PIX_FMT_YUYV || pix.pixelformat == PIX_FMT_MJPEG {
                negotiated = Some(pix);
                break;
            }
        }
    }
    let Some(pix) = negotiated else {
        return Err(CameraError {
            name: "NotReadableError",
            message: format!("{} offers neither YUYV nor MJPEG capture", path.display()),
        });
    };

    let mut request = RequestBuffers {
        count: 2,
        type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
        memory: V4L2_MEMORY_MMAP,
        capabilities: 0,
        flags: 0,
        reserved: [0; 3],
    };
    // SAFETY: VIDIOC_REQBUFS takes a `v4l2_requestbuffers`.
    if unsafe { ioctl(&fd, VIDIOC_REQBUFS, &mut request) } != 0 || request.count == 0 {
        return Err(os_error("cannot allocate capture buffers on", path));
    }
    let mut mappings = Vec::new();
    for index in 0..request.count {
        let mut info = buffer(index);
        // SAFETY: VIDIOC_QUERYBUF takes a `v4l2_buffer`.
        if unsafe { ioctl(&fd, VIDIOC_QUERYBUF, &mut info) } != 0 {
            return Err(os_error("cannot query capture buffer on", path));
        }
        // SAFETY: the driver reported an mmap offset/length for this buffer.
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                info.length as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.0,
                info.m.offset as libc::off_t,
            )
        };
        if pointer == libc::MAP_FAILED {
            return Err(os_error("cannot map capture buffer on", path));
        }
        mappings.push(Mapping {
            pointer,
            length: info.length as usize,
        });
        // SAFETY: VIDIOC_QBUF takes a `v4l2_buffer`.
        if unsafe { ioctl(&fd, VIDIOC_QBUF, &mut info) } != 0 {
            return Err(os_error("cannot queue capture buffer on", path));
        }
    }
    let mut kind = V4L2_BUF_TYPE_VIDEO_CAPTURE as libc::c_int;
    // SAFETY: VIDIOC_STREAMON takes an `int` buffer type.
    if unsafe { ioctl(&fd, VIDIOC_STREAMON, &mut kind) } != 0 {
        return Err(os_error("cannot start streaming on", path));
    }
    let result = read_frame(&fd, &mappings, &pix, path);
    // SAFETY: VIDIOC_STREAMOFF takes an `int` buffer type.
    unsafe {
        ioctl(&fd, VIDIOC_STREAMOFF, &mut kind);
    }
    drop(mappings);
    result
}

/// Frames to skip while auto-exposure settles.
const WARMUP_FRAMES: u32 = 2;

fn read_frame(
    fd: &Fd,
    mappings: &[Mapping],
    pix: &PixFormat,
    path: &Path,
) -> Result<CameraFrame, CameraError> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = 0;
    let mut last: Option<Vec<u8>> = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut poll = libc::pollfd {
            fd: fd.0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().min(1000) as i32) };
        if ready <= 0 {
            continue;
        }
        let mut info = buffer(0);
        // SAFETY: VIDIOC_DQBUF takes a `v4l2_buffer`.
        if unsafe { ioctl(fd, VIDIOC_DQBUF, &mut info) } != 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) {
                continue;
            }
            return Err(os_error("cannot dequeue a frame from", path));
        }
        let Some(mapping) = mappings.get(info.index as usize) else {
            return Err(CameraError {
                name: "NotReadableError",
                message: "driver returned an unknown buffer index".into(),
            });
        };
        let used = (info.bytesused as usize).min(mapping.length);
        // SAFETY: the mapping is `length` bytes and `used` is clamped to it.
        let bytes = unsafe { std::slice::from_raw_parts(mapping.pointer as *const u8, used) };
        last = Some(bytes.to_vec());
        seen += 1;
        // SAFETY: requeue the buffer we just dequeued.
        unsafe {
            ioctl(fd, VIDIOC_QBUF, &mut info);
        }
        if seen > WARMUP_FRAMES {
            break;
        }
    }
    let Some(bytes) = last else {
        return Err(CameraError {
            name: "NotReadableError",
            message: format!("{} produced no frame within 3 s", path.display()),
        });
    };
    encode_frame(
        &bytes,
        pix.pixelformat,
        pix.width,
        pix.height,
        pix.bytesperline,
    )
}

/// Converts a captured buffer to PNG.
pub(crate) fn encode_frame(
    bytes: &[u8],
    pixel_format: u32,
    width: u32,
    height: u32,
    stride: u32,
) -> Result<CameraFrame, CameraError> {
    let unreadable = |message: String| CameraError {
        name: "NotReadableError",
        message,
    };
    let rgb = match pixel_format {
        PIX_FMT_YUYV => {
            let rgb = yuyv_to_rgb(bytes, width, height, stride)
                .ok_or_else(|| unreadable("short YUYV frame".into()))?;
            image::RgbImage::from_raw(width, height, rgb)
                .ok_or_else(|| unreadable("bad YUYV frame size".into()))?
        }
        PIX_FMT_MJPEG => image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
            .map_err(|error| unreadable(format!("cannot decode MJPEG frame: {error}")))?
            .to_rgb8(),
        _ => return Err(unreadable("unsupported pixel format".into())),
    };
    let (width, height) = rgb.dimensions();
    let mut png = Vec::new();
    rgb.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| unreadable(format!("cannot encode frame: {error}")))?;
    Ok(CameraFrame { width, height, png })
}

/// BT.601 limited-range YUYV (YUY2) to packed RGB.
pub(crate) fn yuyv_to_rgb(bytes: &[u8], width: u32, height: u32, stride: u32) -> Option<Vec<u8>> {
    let width = width as usize;
    let height = height as usize;
    let stride = (stride as usize).max(width * 2);
    if width % 2 != 0 || bytes.len() < stride * (height.saturating_sub(1)) + width * 2 {
        return None;
    }
    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        let line = &bytes[row * stride..row * stride + width * 2];
        for pair in line.chunks_exact(4) {
            let (u, v) = (pair[1] as f32 - 128.0, pair[3] as f32 - 128.0);
            for y in [pair[0], pair[2]] {
                let c = (y as f32 - 16.0) * 1.164;
                rgb.push((c + 1.596 * v).round().clamp(0.0, 255.0) as u8);
                rgb.push((c - 0.392 * u - 0.813 * v).round().clamp(0.0, 255.0) as u8);
                rgb.push((c + 2.017 * u).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    Some(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_kernel_abi() {
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<RequestBuffers>(), 20);
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_REQBUFS, 0xC014_5608);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<Format>(), 208);
            assert_eq!(size_of::<Buffer>(), 88);
            assert_eq!(VIDIOC_S_FMT, 0xC0D0_5605);
            assert_eq!(VIDIOC_QUERYBUF, 0xC058_5609);
            assert_eq!(VIDIOC_QBUF, 0xC058_560F);
            assert_eq!(VIDIOC_DQBUF, 0xC058_5611);
        }
        assert_eq!(PIX_FMT_YUYV, 0x5659_5559);
        assert_eq!(PIX_FMT_MJPEG, 0x4750_4A4D);
    }

    #[test]
    fn yuyv_converts_with_stride_and_encodes_png() {
        // 2x2: white/black on row 0, neutral grey and red-ish on row 1, 2 bytes padding.
        let frame = [
            235, 128, 16, 128, 0, 0, // row 0 (Y0 U Y1 V) + padding
            126, 128, 126, 128, 0, 0, // row 1 mid grey
        ];
        let rgb = yuyv_to_rgb(&frame, 2, 2, 6).unwrap();
        assert_eq!(&rgb[0..3], &[255, 255, 255]);
        assert_eq!(&rgb[3..6], &[0, 0, 0]);
        assert!(rgb[6..].iter().all(|channel| (127..=129).contains(channel)));
        assert!(yuyv_to_rgb(&frame[..8], 2, 2, 6).is_none());
        let encoded = encode_frame(&frame, PIX_FMT_YUYV, 2, 2, 6).unwrap();
        assert_eq!((encoded.width, encoded.height), (2, 2));
        assert_eq!(&encoded.png[1..4], b"PNG");
    }

    #[test]
    fn enumeration_skips_non_device_nodes_and_missing_roots() {
        let root = std::env::temp_dir().join(format!("w3cos-v4l2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("video0"), b"not a device").unwrap();
        std::fs::write(root.join("videox"), b"").unwrap();
        assert!(list(&root).is_empty());
        assert!(list(&root.join("missing")).is_empty());
        let error = capture(&root.join("video9"), 640, 480).err().unwrap();
        assert_eq!(error.name, "NotFoundError");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Opt-in: `W3COS_CAMERA_TESTS=1` grabs a frame from the first camera.
    #[test]
    fn first_camera_captures_a_frame_when_enabled() {
        if std::env::var_os("W3COS_CAMERA_TESTS").is_none() {
            return;
        }
        let cameras = list(Path::new("/dev"));
        let camera = cameras
            .first()
            .expect("W3COS_CAMERA_TESTS set but no camera");
        let frame = capture(&camera.path, 640, 480).expect("capture");
        assert!(frame.width > 0 && frame.height > 0 && !frame.png.is_empty());
    }
}
