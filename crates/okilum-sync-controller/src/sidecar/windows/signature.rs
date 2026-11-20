//! Production Authenticode trust plus the release-configured publisher and
//! issuer chain. The same open file excludes replacement through trust and signer reads.
use super::super::supervisor::ipc::windows_discovery::ImagePolicy;
use anyhow::{ensure, Context, Result};
use std::{
    fs::OpenOptions,
    mem::size_of,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, HWND},
        Security::{
            Cryptography::{
                CertNameToStrW, CertStrToNameW, CERT_NAME_STR_REVERSE_FLAG, CERT_STRING_TYPE,
                CERT_X500_NAME_STR, CRYPT_INTEGER_BLOB, X509_ASN_ENCODING,
            },
            WinTrust::*,
        },
        Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ},
    },
};

/// Names are held as canonical X.500 text decoded by CryptoAPI, so a policy
/// matches the certificate regardless of the ASN.1 string type (UTF8String,
/// PrintableString, BMPString) the CA chose for each attribute value.
#[derive(Clone)]
pub struct AuthenticodePolicy {
    subject: String,
    issuers: Vec<String>,
}
const NAME_TEXT: CERT_STRING_TYPE =
    CERT_STRING_TYPE(CERT_X500_NAME_STR.0 | CERT_NAME_STR_REVERSE_FLAG);
fn name_blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}
fn parse_name(name: &str) -> Result<String> {
    canonical_name(&encode_name(name, NAME_TEXT)?)
}
fn encode_name(name: &str, flags: CERT_STRING_TYPE) -> Result<Vec<u8>> {
    ensure!(
        !name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control),
        "invalid Authenticode distinguished name"
    );
    let text: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let mut size = 0;
    unsafe {
        CertStrToNameW(
            X509_ASN_ENCODING,
            PCWSTR(text.as_ptr()),
            flags,
            None,
            None,
            &mut size,
            None,
        )?;
    }
    ensure!(
        (1..=16384).contains(&size),
        "invalid Authenticode name encoding size"
    );
    let mut bytes = vec![0; size as usize];
    unsafe {
        CertStrToNameW(
            X509_ASN_ENCODING,
            PCWSTR(text.as_ptr()),
            flags,
            None,
            Some(bytes.as_mut_ptr()),
            &mut size,
            None,
        )?;
    }
    bytes.truncate(size as usize);
    Ok(bytes)
}
/// Decodes an encoded X.509 name to its canonical X.500 text; the decoded
/// value text does not depend on the attribute's ASN.1 string type.
fn canonical_name(encoded: &[u8]) -> Result<String> {
    let blob = name_blob(encoded);
    let size = unsafe { CertNameToStrW(X509_ASN_ENCODING, &blob, NAME_TEXT, None) };
    ensure!(
        (2..=16384).contains(&size),
        "invalid Authenticode name text size"
    );
    let mut wide = vec![0; size as usize];
    ensure!(
        unsafe { CertNameToStrW(X509_ASN_ENCODING, &blob, NAME_TEXT, Some(&mut wide)) } == size,
        "Authenticode name text changed"
    );
    Ok(String::from_utf16(&wide[..wide.len() - 1])?)
}
impl AuthenticodePolicy {
    /// RFC2253 publisher and complete issuer-chain names from release config.
    /// Annual leaf-certificate renewal preserves this policy.
    pub fn new(subject: &str, issuers_json: &str) -> Result<Self> {
        let issuers: Vec<String> = serde_json::from_str(issuers_json)?;
        ensure!(
            (1..=16).contains(&issuers.len()),
            "Authenticode policy needs its complete issuer chain"
        );
        Ok(Self {
            subject: parse_name(subject)?,
            issuers: issuers
                .iter()
                .map(|name| parse_name(name))
                .collect::<Result<_>>()?,
        })
    }
    /// The leaf must be the configured publisher and the configured issuers an
    /// ordered prefix of the rest: Windows may extend a trusted chain past the
    /// configured root, for example through a cross-signed root certificate.
    fn matches_chain(&self, actual: &[String]) -> bool {
        actual.len() > self.issuers.len()
            && actual[0] == self.subject
            && actual[1..]
                .iter()
                .zip(&self.issuers)
                .all(|(actual, expected)| actual == expected)
    }
    pub fn from_build() -> Result<Self> {
        Self::new(
            option_env!("OKILUM_WINDOWS_SIGN_SUBJECT").context("this build has no Authenticode publisher policy; install the signed beta build before connecting Sync")?,
            option_env!("OKILUM_WINDOWS_SIGN_ISSUER_CHAIN").context("this build has no Authenticode issuer-chain policy; install the signed beta build before connecting Sync")?,
        )
    }
}
impl ImagePolicy for AuthenticodePolicy {
    fn verify_image(&self, image: &Path) -> Result<()> {
        let actual = verified_names(image)?
            .iter()
            .map(|name| canonical_name(name))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            self.matches_chain(&actual),
            "Authenticode publisher or issuer chain differs from this build's configured policy"
        );
        Ok(())
    }
}
struct TrustState(WINTRUST_DATA);
impl Drop for TrustState {
    fn drop(&mut self) {
        self.0.dwStateAction = WTD_STATEACTION_CLOSE;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        unsafe {
            WinVerifyTrust(
                HWND::default(),
                &mut action,
                (&mut self.0 as *mut WINTRUST_DATA).cast(),
            );
        }
    }
}
fn verified_names(path: &Path) -> Result<Vec<Vec<u8>>> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "signed image must be a regular file"
    );
    okilum_write_authority::filesystem_identity::from_file(&file)?;
    let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path.as_ptr()),
        hFile: HANDLE(file.as_raw_handle()),
        ..Default::default()
    };
    let mut trust = TrustState(WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        // Trust follows the native timestamp/chain policy, without weakening
        // expired signatures to a bare matching certificate or a same-user peer.
        dwProvFlags: WTD_DISABLE_MD2_MD4,
        ..Default::default()
    });
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut trust.0 as *mut WINTRUST_DATA).cast(),
        )
    };
    ensure!(
        status == 0,
        "Authenticode trust verification failed: 0x{:08x}",
        status as u32
    );
    unsafe {
        let provider = WTHelperProvDataFromStateData(trust.0.hWVTStateData);
        ensure!(
            !provider.is_null(),
            "verified signer provider is unavailable"
        );
        let signer = WTHelperGetProvSignerFromChain(provider, 0, false, 0);
        ensure!(
            !signer.is_null()
                && (*signer).dwError == 0
                && (1..=64).contains(&(*signer).csCertChain)
                && !(*signer).pasCertChain.is_null(),
            "verified signer chain is unavailable"
        );
        let chain =
            std::slice::from_raw_parts((*signer).pasCertChain, (*signer).csCertChain as usize);
        chain
            .iter()
            .map(|entry| {
                ensure!(
                    !entry.pCert.is_null() && !(*entry.pCert).pCertInfo.is_null(),
                    "verified signer certificate is unavailable"
                );
                let subject = (*(*entry.pCert).pCertInfo).Subject;
                ensure!(
                    (1..=16384).contains(&subject.cbData) && !subject.pbData.is_null(),
                    "verified signer name is unavailable"
                );
                Ok(std::slice::from_raw_parts(subject.pbData, subject.cbData as usize).to_vec())
            })
            .collect()
    }
}

#[cfg(test)]
pub(super) fn fixture_policy(path: &Path) -> Result<AuthenticodePolicy> {
    let names = verified_names(path)?
        .iter()
        .map(|name| canonical_name(name))
        .collect::<Result<Vec<_>>>()?;
    ensure!(names.len() > 1, "signed fixture has no issuer chain");
    Ok(AuthenticodePolicy {
        subject: names[0].clone(),
        issuers: names[1..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::Cryptography::{
        CertCompareCertificateName, CERT_NAME_STR_FORCE_UTF8_DIR_STR_FLAG,
    };
    const PUBLISHER: &str = "CN=Open Source Developer Oleg Kossoy,O=Open Source Developer,L=Netanya,ST=Central District,C=IL";
    const ISSUERS: [&str; 2] = ["CN=Certum Code Signing 2021 CA,OU=Certum Certification Authority,O=Asseco Data Systems S.A.,C=PL", "CN=Certum Trusted Network CA 2,OU=Certum Certification Authority,O=Unizeto Technologies S.A.,C=PL"];

    fn release_policy() -> Result<AuthenticodePolicy> {
        AuthenticodePolicy::new(PUBLISHER, &serde_json::to_string(&ISSUERS)?)
    }

    /// #3149: the real Certum leaf encodes CN, O, L and ST as UTF8String,
    /// while the policy text encodes them as PrintableString by default.
    #[test]
    fn windows_authenticode_policy_matches_utf8_encoded_certificate_names() -> Result<()> {
        let printable = encode_name(PUBLISHER, NAME_TEXT)?;
        let utf8 = encode_name(
            PUBLISHER,
            CERT_STRING_TYPE(NAME_TEXT.0 | CERT_NAME_STR_FORCE_UTF8_DIR_STR_FLAG),
        )?;
        // Control: the encodings differ and a binary name compare rejects them.
        ensure!(printable != utf8, "UTF8String encoding was not produced");
        assert!(!unsafe {
            CertCompareCertificateName(X509_ASN_ENCODING, &name_blob(&utf8), &name_blob(&printable))
                .as_bool()
        });
        let policy = release_policy()?;
        let mut chain = vec![canonical_name(&utf8)?];
        for issuer in ISSUERS {
            chain.push(canonical_name(&encode_name(
                issuer,
                CERT_STRING_TYPE(NAME_TEXT.0 | CERT_NAME_STR_FORCE_UTF8_DIR_STR_FLAG),
            )?)?);
        }
        assert!(policy.matches_chain(&chain));
        chain[0] = canonical_name(&encode_name(
            "CN=Open Source Developer Other,O=Open Source Developer,L=Netanya,ST=Central District,C=IL",
            CERT_STRING_TYPE(NAME_TEXT.0 | CERT_NAME_STR_FORCE_UTF8_DIR_STR_FLAG),
        )?)?;
        assert!(!policy.matches_chain(&chain));
        Ok(())
    }

    #[test]
    fn windows_authenticode_policy_issuers_are_an_ordered_chain_prefix() -> Result<()> {
        let policy = release_policy()?;
        let leaf = parse_name(PUBLISHER)?;
        let issuing = parse_name(ISSUERS[0])?;
        let root = parse_name(ISSUERS[1])?;
        let cross = parse_name("CN=Certum Trusted Network CA,OU=Certum Certification Authority,O=Unizeto Technologies S.A.,C=PL")?;
        assert!(policy.matches_chain(&[leaf.clone(), issuing.clone(), root.clone()]));
        // Windows may extend the chain through the cross-signed root.
        assert!(policy.matches_chain(&[
            leaf.clone(),
            issuing.clone(),
            root.clone(),
            cross.clone()
        ]));
        assert!(!policy.matches_chain(&[leaf.clone(), issuing.clone()]));
        assert!(!policy.matches_chain(&[leaf.clone(), root.clone(), issuing.clone()]));
        assert!(!policy.matches_chain(&[
            leaf.clone(),
            issuing.clone(),
            cross.clone(),
            root.clone()
        ]));
        assert!(!policy.matches_chain(&[cross, issuing, root]));
        assert!(!policy.matches_chain(&[leaf]));
        Ok(())
    }
    #[test]
    fn windows_authenticode_policy_requires_trust_exact_signer_and_unchanged_bytes() -> Result<()> {
        use sha2::{Digest, Sha256};
        let signed = std::path::PathBuf::from(
            std::env::var_os("OKILUM_SYNC_WINDOWS_CLIENT")
                .context("pinned embedded-signature fixture missing")?,
        );
        ensure!(
            format!("{:x}", Sha256::digest(std::fs::read(&signed)?))
                == super::super::payload::UPSTREAM_HASH,
            "signed positive fixture differs from the pinned upstream executable"
        );
        let names = verified_names(&signed)?
            .iter()
            .map(|name| canonical_name(name))
            .collect::<Result<Vec<_>>>()?;
        let policy = AuthenticodePolicy::new(&names[0], &serde_json::to_string(&names[1..])?)?;
        policy.verify_image(&signed)?;
        assert!(AuthenticodePolicy::new(
            "CN=Other publisher",
            &serde_json::to_string(&names[1..])?
        )?
        .verify_image(&signed)
        .is_err());
        assert!(policy.verify_image(&std::env::current_exe()?).is_err());
        assert!(AuthenticodePolicy::new("CN=Publisher", "[]").is_err());
        assert!(AuthenticodePolicy::new(&names[0], "[\"CN=Other issuer\"]")?
            .verify_image(&signed)
            .is_err());
        let temporary = tempfile::tempdir()?;
        let copy = temporary.path().join("signed-copy.exe");
        let mut bytes = std::fs::read(&signed)?;
        std::fs::write(&copy, &bytes)?;
        policy.verify_image(&copy)?;
        ensure!(bytes.len() > 8192, "signed positive fixture is too short");
        bytes[4096] ^= 1;
        std::fs::write(&copy, &bytes)?;
        assert!(policy.verify_image(&copy).is_err());
        Ok(())
    }
}
