//! HTTPS POST of a JSON body with Windows' own HTTP stack (system proxy and certificates), for the online
//! party room. The body of the answer is returned whatever the HTTP status, so the server's own error text
//! (a full room, a bad code) reaches the player.

#[cfg(windows)]
pub fn post_json(url: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Networking::WinHttp::*;
    use windows::core::{HSTRING, PCWSTR};
    let rest = url.strip_prefix("https://").ok_or("Only HTTPS is used")?;
    let (host, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_owned()), |(h, p)| (h, format!("/{p}")));
    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a handle returned by WinHttp*, closed once.
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }
    let failed = |what: &str| {
        format!(
            "Could not reach the server ({what}). Check your internet connection and the server address."
        )
    };
    // SAFETY: plain WinHTTP calls; every handle is closed by `Handle`, buffers outlive each call.
    unsafe {
        let agent = HSTRING::from(format!("LootLedger/{}", env!("CARGO_PKG_VERSION")));
        let session = Handle(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(failed("open"));
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let connection = Handle(WinHttpConnect(
            session.0,
            &HSTRING::from(host),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if connection.0.is_null() {
            return Err(failed("connect"));
        }
        let request = Handle(WinHttpOpenRequest(
            connection.0,
            &HSTRING::from("POST"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(failed("request"));
        }
        let headers: Vec<u16> = "Content-Type: application/json\r\n"
            .encode_utf16()
            .collect();
        WinHttpSendRequest(
            request.0,
            Some(&headers),
            Some(body.as_ptr() as *const core::ffi::c_void),
            body.len() as u32,
            body.len() as u32,
            0,
        )
        .map_err(|_| failed("send"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|_| failed("response"))?;
        let mut answer = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut _,
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|_| failed("read"))?;
            if read == 0 {
                break;
            }
            answer.extend_from_slice(&chunk[..read as usize]);
            if answer.len() > 4 * 1024 * 1024 {
                return Err("The server's answer was unexpectedly large.".into());
            }
        }
        Ok(answer)
    }
}

#[cfg(not(windows))]
pub fn post_json(_: &str, _: &[u8]) -> Result<Vec<u8>, String> {
    Err("The online room is only available on Windows".into())
}
