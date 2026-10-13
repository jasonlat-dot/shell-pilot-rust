use std::io;

/// DPAPI binds ciphertext to the current Windows account. Entropy binds it to a
/// connection ID and credential kind, preventing accidental record swapping.
pub(crate) fn protect(value: &str, context: &str) -> io::Result<Vec<u8>> {
    crypt(value.as_bytes(), context.as_bytes(), true)
}

pub(crate) fn unprotect(value: &[u8], context: &str) -> io::Result<String> {
    String::from_utf8(crypt(value, context.as_bytes(), false)?)
        .map_err(|_| io::Error::other("invalid credential encoding"))
}

#[cfg(windows)]
fn crypt(value: &[u8], entropy: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: value
            .len()
            .try_into()
            .map_err(|_| io::Error::other("credential too large"))?,
        pbData: value.as_ptr().cast_mut(),
    };
    let context = CRYPT_INTEGER_BLOB {
        cbData: entropy
            .len()
            .try_into()
            .map_err(|_| io::Error::other("context too large"))?,
        pbData: entropy.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: slices remain valid for the synchronous call. DPAPI owns output;
    // LocalFree releases it after copying. No UI or machine-wide protection.
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &context,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &context,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Ok(bytes)
    }
}

#[cfg(not(windows))]
fn crypt(_: &[u8], _: &[u8], _: bool) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "persistent credentials currently require Windows DPAPI",
    ))
}
