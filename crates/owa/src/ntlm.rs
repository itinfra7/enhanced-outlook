//! NTLM 토큰 생성. Windows SSPI(브라우저·curl과 같은 OS 구현)에 명시적 자격 증명과
//! 채널 바인딩(EPA)을 넘긴다. 이 PC는 도메인에 가입되지 않아 Kerberos 대신 NTLM을 쓴다.
#![allow(unsafe_code)]

use crate::creds::Credentials;
use crate::error::{Error, Result};

/// `도메인\사용자`면 둘로 나누고, 그 밖(`user@domain` UPN 등)은 도메인 없이 그대로 쓴다.
fn split_user(username: &str) -> (&str, &str) {
    match username.split_once('\\') {
        Some((domain, user)) => (user, domain),
        None => (username, ""),
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    use windows_sys::Win32::Foundation::{
        SEC_E_OK, SEC_I_COMPLETE_AND_CONTINUE, SEC_I_COMPLETE_NEEDED, SEC_I_CONTINUE_NEEDED,
    };
    use windows_sys::Win32::Security::Authentication::Identity::{
        AcquireCredentialsHandleW, DeleteSecurityContext, FreeCredentialsHandle,
        ISC_REQ_CONNECTION, InitializeSecurityContextW, SECBUFFER_CHANNEL_BINDINGS,
        SECBUFFER_TOKEN, SECBUFFER_VERSION, SECPKG_CRED_OUTBOUND, SECURITY_NATIVE_DREP, SecBuffer,
        SecBufferDesc,
    };
    use windows_sys::Win32::Security::Credentials::SecHandle;
    use windows_sys::Win32::System::Rpc::{
        SEC_WINNT_AUTH_IDENTITY_UNICODE, SEC_WINNT_AUTH_IDENTITY_W,
    };
    use zeroize::Zeroizing;

    use super::{Credentials, Error, Result, split_user};

    const MAX_TOKEN: usize = 16 * 1024;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn len_u32<T>(v: &[T]) -> u32 {
        u32::try_from(v.len()).unwrap_or(u32::MAX)
    }

    /// NTLM 인증 한 번(연결 하나)의 SSPI 상태.
    pub(crate) struct NtlmContext {
        cred: SecHandle,
        ctx: Option<SecHandle>,
        target: Vec<u16>,
        bindings: Option<Vec<u8>>,
    }

    impl NtlmContext {
        pub(crate) fn new(creds: &Credentials, spn: &str, bindings: Option<&[u8]>) -> Result<Self> {
            let (user, domain) = split_user(&creds.username);
            let mut user: Vec<u16> = user.encode_utf16().collect();
            let mut domain: Vec<u16> = domain.encode_utf16().collect();
            let mut password: Zeroizing<Vec<u16>> =
                Zeroizing::new(creds.password.expose().encode_utf16().collect());
            let identity = SEC_WINNT_AUTH_IDENTITY_W {
                User: user.as_mut_ptr(),
                UserLength: len_u32(&user),
                Domain: if domain.is_empty() {
                    ptr::null_mut()
                } else {
                    domain.as_mut_ptr()
                },
                DomainLength: len_u32(&domain),
                Password: password.as_mut_ptr(),
                PasswordLength: len_u32(&password),
                Flags: SEC_WINNT_AUTH_IDENTITY_UNICODE,
            };
            let package = wide("NTLM");
            let mut cred = SecHandle {
                dwLower: 0,
                dwUpper: 0,
            };
            let mut expiry = 0i64;
            // SAFETY: 모든 포인터는 이 호출 동안 살아 있는 지역 버퍼를 가리킨다.
            // SSPI는 자격 증명을 핸들 안으로 복사하므로 호출 뒤 버퍼를 지워도 된다.
            let status = unsafe {
                AcquireCredentialsHandleW(
                    ptr::null(),
                    package.as_ptr(),
                    SECPKG_CRED_OUTBOUND,
                    ptr::null(),
                    (&raw const identity).cast::<c_void>(),
                    None,
                    ptr::null(),
                    &mut cred,
                    &mut expiry,
                )
            };
            if status != SEC_E_OK {
                return Err(Error::Auth(format!(
                    "AcquireCredentialsHandle 실패 (0x{status:08X})"
                )));
            }
            Ok(Self {
                cred,
                ctx: None,
                target: wide(spn),
                bindings: bindings.map(<[u8]>::to_vec),
            })
        }

        /// 첫 호출(`input` 없음)은 NEGOTIATE, 서버 CHALLENGE를 넣은 두 번째 호출은 AUTHENTICATE 메시지.
        pub(crate) fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>> {
            let mut out = vec![0u8; MAX_TOKEN];
            let mut out_buf = SecBuffer {
                cbBuffer: len_u32(&out),
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: out.as_mut_ptr().cast(),
            };
            let mut out_desc = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: 1,
                pBuffers: &raw mut out_buf,
            };
            let mut token = input.map(<[u8]>::to_vec);
            let mut in_bufs: Vec<SecBuffer> = Vec::with_capacity(2);
            if let Some(t) = token.as_mut() {
                in_bufs.push(SecBuffer {
                    cbBuffer: len_u32(t),
                    BufferType: SECBUFFER_TOKEN,
                    pvBuffer: t.as_mut_ptr().cast(),
                });
            }
            if let Some(b) = self.bindings.as_mut() {
                in_bufs.push(SecBuffer {
                    cbBuffer: len_u32(b),
                    BufferType: SECBUFFER_CHANNEL_BINDINGS,
                    pvBuffer: b.as_mut_ptr().cast(),
                });
            }
            let in_desc = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: len_u32(&in_bufs),
                pBuffers: in_bufs.as_mut_ptr(),
            };
            let in_ptr: *const SecBufferDesc = if in_bufs.is_empty() {
                ptr::null()
            } else {
                &raw const in_desc
            };
            let old = self.ctx;
            let old_ptr: *const SecHandle = match &old {
                Some(h) => h,
                None => ptr::null(),
            };
            let mut new_ctx = old.unwrap_or(SecHandle {
                dwLower: 0,
                dwUpper: 0,
            });
            let mut attrs = 0u32;
            let mut expiry = 0i64;
            // SAFETY: 입력·출력 버퍼는 이 함수의 지역 값이고 호출이 끝날 때까지 살아 있다.
            let status = unsafe {
                InitializeSecurityContextW(
                    &self.cred,
                    old_ptr,
                    self.target.as_ptr(),
                    ISC_REQ_CONNECTION,
                    0,
                    SECURITY_NATIVE_DREP,
                    in_ptr,
                    0,
                    &mut new_ctx,
                    &raw mut out_desc,
                    &mut attrs,
                    &mut expiry,
                )
            };
            match status {
                SEC_E_OK
                | SEC_I_CONTINUE_NEEDED
                | SEC_I_COMPLETE_NEEDED
                | SEC_I_COMPLETE_AND_CONTINUE => self.ctx = Some(new_ctx),
                _ => {
                    return Err(Error::Auth(format!(
                        "InitializeSecurityContext 실패 (0x{status:08X})"
                    )));
                }
            }
            out.truncate(out_buf.cbBuffer as usize);
            Ok(out)
        }
    }

    impl Drop for NtlmContext {
        fn drop(&mut self) {
            // SAFETY: 이 구조체가 얻은 핸들을 한 번만 해제한다.
            unsafe {
                if let Some(ctx) = self.ctx.take() {
                    DeleteSecurityContext(&ctx);
                }
                FreeCredentialsHandle(&self.cred);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Credentials, Error, Result};

    pub(crate) struct NtlmContext;

    impl NtlmContext {
        pub(crate) fn new(_: &Credentials, _: &str, _: Option<&[u8]>) -> Result<Self> {
            Err(Error::Auth("NTLM 인증은 Windows에서만 지원합니다".into()))
        }

        pub(crate) fn step(&mut self, _: Option<&[u8]>) -> Result<Vec<u8>> {
            Err(Error::Auth("NTLM 인증은 Windows에서만 지원합니다".into()))
        }
    }
}

pub(crate) use imp::NtlmContext;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_domain_user_but_keeps_upn() {
        assert_eq!(split_user(r"CORP\kim"), ("kim", "CORP"));
        assert_eq!(split_user("kim@example.com"), ("kim@example.com", ""));
    }
}
