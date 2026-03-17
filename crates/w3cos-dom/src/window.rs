/// W3C Window API — global scope for the application.
pub struct Window {
    pub inner_width: f32,
    pub inner_height: f32,
    pub device_pixel_ratio: f32,
    animation_frame_callbacks: Vec<Box<dyn FnOnce(f64)>>,
}

impl Window {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            inner_width: width,
            inner_height: height,
            device_pixel_ratio: 1.0,
            animation_frame_callbacks: Vec::new(),
        }
    }

    pub fn request_animation_frame(&mut self, callback: Box<dyn FnOnce(f64)>) -> u32 {
        self.animation_frame_callbacks.push(callback);
        self.animation_frame_callbacks.len() as u32
    }

    pub fn flush_animation_frames(&mut self, timestamp: f64) {
        let callbacks = std::mem::take(&mut self.animation_frame_callbacks);
        for cb in callbacks {
            cb(timestamp);
        }
    }

    pub fn resize(&mut self, width: f32, height: f32) {
        self.inner_width = width;
        self.inner_height = height;
    }
}

impl Default for Window {
    fn default() -> Self {
        Self::new(960.0, 640.0)
    }
}
