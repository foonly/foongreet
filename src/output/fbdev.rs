//! Legacy /dev/fb0 output. Only works while no DRM master is active.

use super::Output;
use std::{fs, fs::OpenOptions, io, os::fd::AsRawFd};

pub struct Fbdev {
    ptr: *mut u32,
    len: usize,
    width: usize,
    height: usize,
    stride: usize,
    _file: fs::File,
}

fn sysfs(name: &str) -> io::Result<String> {
    Ok(fs::read_to_string(format!("/sys/class/graphics/fb0/{name}"))?.trim().to_string())
}

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl Fbdev {
    pub fn open() -> io::Result<Fbdev> {
        let bpp: usize = sysfs("bits_per_pixel")?.parse().map_err(|_| bad("bad bits_per_pixel".into()))?;
        if bpp != 32 {
            return Err(bad(format!("fb0 is {bpp} bpp, only 32 is supported")));
        }
        let size = sysfs("virtual_size")?;
        let (w, h) = size.split_once(',').ok_or_else(|| bad(format!("bad virtual_size {size}")))?;
        let width: usize = w.parse().map_err(|_| bad(size.clone()))?;
        let height: usize = h.parse().map_err(|_| bad(size.clone()))?;
        let stride_bytes: usize = sysfs("stride")?.parse().map_err(|_| bad("bad stride".into()))?;

        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        let len = stride_bytes * height;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Fbdev { ptr: ptr.cast(), len, width, height, stride: stride_bytes / 4, _file: file })
    }
}

impl Output for Fbdev {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn present(&mut self, draw: &mut dyn FnMut(&mut [u32], usize)) -> io::Result<()> {
        let buf = unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len / 4) };
        draw(buf, self.stride);
        Ok(())
    }
}

impl Drop for Fbdev {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}
