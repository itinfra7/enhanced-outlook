//! NTLM 채널 바인딩(확장 보호, EPA). RFC 5929 `tls-server-end-point`: 서버 인증서 DER의 해시.
//! 해시 알고리즘은 인증서 서명 알고리즘을 따르고, MD5·SHA-1이면 SHA-256을 쓴다.

use sha2::{Digest, Sha256, Sha384, Sha512};

const SHA384_WITH_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C];
const SHA512_WITH_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D];
const ECDSA_WITH_SHA384: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03];
const ECDSA_WITH_SHA512: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x04];

/// DER TLV 하나: (태그, 내용, 나머지).
fn read_tlv(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *d.first()?;
    let first = *d.get(1)?;
    let mut i = 2;
    let len = if first & 0x80 == 0 {
        usize::from(first)
    } else {
        let n = usize::from(first & 0x7F);
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for _ in 0..n {
            len = (len << 8) | usize::from(*d.get(i)?);
            i += 1;
        }
        len
    };
    let content = d.get(i..i.checked_add(len)?)?;
    Some((tag, content, &d[i + len..]))
}

/// `Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }`의 서명 알고리즘 OID.
fn signature_oid(cert: &[u8]) -> Option<&[u8]> {
    let (0x30, cert, _) = read_tlv(cert)? else {
        return None;
    };
    let (_, _tbs, rest) = read_tlv(cert)?;
    let (0x30, alg, _) = read_tlv(rest)? else {
        return None;
    };
    let (0x06, oid, _) = read_tlv(alg)? else {
        return None;
    };
    Some(oid)
}

pub(crate) fn server_end_point_hash(cert: &[u8]) -> Vec<u8> {
    match signature_oid(cert) {
        Some(SHA384_WITH_RSA | ECDSA_WITH_SHA384) => Sha384::digest(cert).to_vec(),
        Some(SHA512_WITH_RSA | ECDSA_WITH_SHA512) => Sha512::digest(cert).to_vec(),
        _ => Sha256::digest(cert).to_vec(),
    }
}

/// SSPI `SEC_CHANNEL_BINDINGS`(32바이트 머리, 응용 데이터 위치 32) + `tls-server-end-point:` + 해시.
pub(crate) fn channel_bindings(cert: &[u8]) -> Vec<u8> {
    let mut app = b"tls-server-end-point:".to_vec();
    app.extend(server_end_point_hash(cert));
    let mut blob = vec![0u8; 32];
    blob[24..28].copy_from_slice(&u32::try_from(app.len()).unwrap_or(0).to_le_bytes());
    blob[28..32].copy_from_slice(&32u32.to_le_bytes());
    blob.extend(app);
    blob
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 최소 인증서 모양: SEQUENCE { SEQUENCE{INTEGER 0}, SEQUENCE{OID, NULL}, BIT STRING }.
    fn fake_cert(oid: &[u8]) -> Vec<u8> {
        let mut alg = vec![0x06, oid.len() as u8];
        alg.extend(oid);
        alg.extend([0x05, 0x00]);
        let mut body = vec![0x30, 0x03, 0x02, 0x01, 0x00, 0x30, alg.len() as u8];
        body.extend(alg);
        body.extend([0x03, 0x01, 0x00]);
        let mut cert = vec![0x30, body.len() as u8];
        cert.extend(body);
        cert
    }

    #[test]
    fn hash_follows_signature_algorithm() {
        let sha256_rsa = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];
        assert_eq!(server_end_point_hash(&fake_cert(&sha256_rsa)).len(), 32);
        assert_eq!(server_end_point_hash(&fake_cert(SHA384_WITH_RSA)).len(), 48);
        assert_eq!(
            server_end_point_hash(&fake_cert(ECDSA_WITH_SHA512)).len(),
            64
        );
        // SHA-1 서명 → SHA-256
        let sha1_rsa = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x05];
        assert_eq!(server_end_point_hash(&fake_cert(&sha1_rsa)).len(), 32);
    }

    #[test]
    fn long_form_length_is_parsed() {
        let content = vec![0u8; 300];
        let mut der = vec![0x04, 0x82, 0x01, 0x2C];
        der.extend(&content);
        let (tag, c, rest) = read_tlv(&der).unwrap();
        assert_eq!((tag, c.len(), rest.len()), (0x04, 300, 0));
    }

    #[test]
    fn bindings_layout() {
        let cert = fake_cert(SHA384_WITH_RSA);
        let b = channel_bindings(&cert);
        let app_len = u32::from_le_bytes(b[24..28].try_into().unwrap()) as usize;
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 32);
        assert_eq!(b.len(), 32 + app_len);
        assert!(b[32..].starts_with(b"tls-server-end-point:"));
        assert_eq!(app_len, "tls-server-end-point:".len() + 48);
        assert!(b[..24].iter().all(|&x| x == 0));
    }
}
