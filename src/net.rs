//! HTTPS GET through WinHTTP: Windows' own TLS stack and certificate store, the system proxy,
//! and redirects that may not drop from HTTPS to HTTP. No TLS library ships in the exe, and
//! certificate revocation and root updates come with Windows Update.

/// What a WinINet error means in words. Windows keeps no text for these codes in its message
/// table (`FormatMessageW` answers 317), so an `io::Error`'s Display prints the number twice.
#[cfg(windows)]
fn reason(code: i32) -> Option<&'static str> {
    Some(match code {
        12002 => "GitHub didn't answer in time; check the connection and try again",
        12007 => "GitHub's address couldn't be found; check the internet connection",
        12029 => "couldn't connect to GitHub; a firewall, VPN or proxy may be blocking it",
        12030 | 12031 => "the connection to GitHub dropped; try again",
        12037 => "Windows thinks GitHub's certificate has expired; check the date and time",
        12038 => "GitHub's certificate is for a different name; something may be intercepting the connection",
        12057 => "Windows couldn't check GitHub's certificate; check the date and time",
        12157 | 12175 => "the encrypted connection to GitHub failed; a proxy or antivirus may be intercepting it",
        12169 => "Windows rejected GitHub's certificate; something may be intercepting the connection",
        _ => return None,
    })
}

/// Streams the body of `url` to `on_chunk`, with the Content-Length when the server sent one.
/// Fails on anything but a 200, and once more than `max_bytes` arrive.
#[cfg(windows)]
pub fn get(
    url: &str,
    accept: &str,
    max_bytes: u64,
    mut on_chunk: impl FnMut(&[u8], Option<u64>) -> Result<(), String>,
) -> Result<(), String> {
    use std::ffi::c_void;
    use std::ptr::null;
    use windows_sys::Win32::Networking::WinHttp::*;

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn fail(what: &str) -> String {
        let e = std::io::Error::last_os_error();
        match e.raw_os_error().and_then(reason) {
            Some(why) => why.to_string(),
            // Keep the raw error for the codes not worth a sentence, so bug reports stay useful.
            None => format!("{what} failed ({e})"),
        }
    }
    fn open(h: *mut c_void, what: &str) -> Result<Handle, String> {
        if h.is_null() { Err(fail(what)) } else { Ok(Handle(h)) }
    }

    let rest = url.strip_prefix("https://").ok_or("only HTTPS addresses are fetched")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty() || host.contains(['@', ':']) {
        return Err(format!("refusing the address {url}"));
    }
    let agent = wide(&format!("Projects/{} (updater)", env!("CARGO_PKG_VERSION")));
    let headers = wide(&format!("Accept: {accept}\r\n"));
    let (host_w, path_w, verb) = (wide(host), wide(path), wide("GET"));

    unsafe {
        let session = open(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0), "WinHttpOpen")?;
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000);
        // TLS 1.2 and 1.3 only. Windows builds without 1.3 refuse the flag; the default then
        // still excludes SSL 3 and TLS 1.0 on current Windows 10 and 11.
        let protocols: u32 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
        if WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, &protocols as *const u32 as *const c_void, 4) == 0 {
            let tls12 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2;
            WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, &tls12 as *const u32 as *const c_void, 4);
        }
        let policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
        WinHttpSetOption(session.0, WINHTTP_OPTION_REDIRECT_POLICY, &policy as *const u32 as *const c_void, 4);

        let connect = open(WinHttpConnect(session.0, host_w.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0), "WinHttpConnect")?;
        let request = open(
            WinHttpOpenRequest(connect.0, verb.as_ptr(), path_w.as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE),
            "WinHttpOpenRequest",
        )?;
        if WinHttpSendRequest(request.0, headers.as_ptr(), u32::MAX, null(), 0, 0, 0) == 0 {
            return Err(fail("Connecting to GitHub"));
        }
        if WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
            return Err(fail("Reading GitHub's reply"));
        }
        let number = |level: u32| -> Option<u32> {
            let mut v: u32 = 0;
            let mut len: u32 = 4;
            let ok = WinHttpQueryHeaders(
                request.0,
                level | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                &mut v as *mut u32 as *mut c_void,
                &mut len,
                std::ptr::null_mut(),
            );
            (ok != 0).then_some(v)
        };
        let status = number(WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
        if status != 200 {
            return Err(match status {
                403 | 429 => "GitHub is rate-limiting update checks; trying again later".to_string(),
                404 => "no release found".to_string(),
                s => format!("GitHub answered HTTP {s}"),
            });
        }
        let length = number(WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
        if length.is_some_and(|l| l > max_bytes) {
            return Err("the download is larger than expected".into());
        }
        let mut got: u64 = 0;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(request.0, &mut avail) == 0 {
                return Err(fail("Downloading"));
            }
            if avail == 0 {
                break;
            }
            let want = (avail as usize).min(buf.len());
            let mut read: u32 = 0;
            if WinHttpReadData(request.0, buf.as_mut_ptr() as *mut c_void, want as u32, &mut read) == 0 {
                return Err(fail("Downloading"));
            }
            if read == 0 {
                break;
            }
            got += read as u64;
            if got > max_bytes {
                return Err("the download is larger than expected".into());
            }
            on_chunk(&buf[..read as usize], length)?;
        }
        if length.is_some_and(|l| l != got) {
            return Err("the download ended early".into());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn get(
    _url: &str,
    _accept: &str,
    _max_bytes: u64,
    _on_chunk: impl FnMut(&[u8], Option<u64>) -> Result<(), String>,
) -> Result<(), String> {
    Err("updates are only built for Windows".into())
}

/// A small body as text.
pub fn get_text(url: &str, accept: &str, max_bytes: u64) -> Result<String, String> {
    let mut body = Vec::new();
    get(url, accept, max_bytes, |chunk, _| {
        body.extend_from_slice(chunk);
        Ok(())
    })?;
    String::from_utf8(body).map_err(|_| "the reply isn't text".to_string())
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn wininet_codes_read_as_sentences() {
        assert!(super::reason(12002).unwrap().contains("didn't answer in time"));
        assert!(super::reason(12029).unwrap().contains("firewall"));
        assert!(super::reason(12175).unwrap().contains("encrypted connection"));
        // Codes with real system text keep their raw error instead.
        assert_eq!(super::reason(5), None);
    }

    /// Reaches GitHub over the network; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_the_repository_over_https() {
        let body = super::get_text("https://api.github.com/repos/rust-lang/rust", "application/vnd.github+json", 1 << 20).unwrap();
        assert!(body.contains("\"full_name\""));
        let missing = super::get_text("https://api.github.com/repos/rust-lang/rust/releases/tags/v0.0.0-none", "application/json", 1 << 20);
        assert_eq!(missing.unwrap_err(), "no release found");
    }
}
