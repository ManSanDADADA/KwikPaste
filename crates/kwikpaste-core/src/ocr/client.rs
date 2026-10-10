//! OCR 使用通用扩展进程宿主，并为每张图片设置截止时间。
use super::protocol::{Request, Response};
#[cfg(target_os = "windows")]
pub(super) use crate::extensions::process::peak_private_usage;
pub(super) use crate::extensions::process::{kill, ChildHandle};
use std::{io, path::Path, time::Duration};

pub(super) struct Client {
    host: crate::extensions::process::ProcessHost<Request, Response>,
    pub child: ChildHandle,
}
impl Client {
    pub fn start(exe: &Path) -> io::Result<Self> {
        let host = crate::extensions::process::ProcessHost::start("ocr", exe)?;
        Ok(Self {
            child: host.child.clone(),
            host,
        })
    }
    pub fn request(&mut self, request: &Request) -> io::Result<Response> {
        self.host.request(request.clone(), Duration::from_secs(60))
    }
}
