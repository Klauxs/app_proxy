use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    // Prevent Windows installer-name heuristics from elevating the entire setup.
    // Only the narrowly scoped listener host requests UAC.
    println!("cargo:rustc-link-arg-bin=AppProxy-Setup=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=AppProxy-Setup=/MANIFESTUAC:level='asInvoker' uiAccess='false'"
    );
    println!("cargo:rerun-if-env-changed=APP_PROXY_PAYLOAD_DIR");
    println!("cargo:rerun-if-env-changed=APP_PROXY_BUILD_ID");
    let destination = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("payload.rs");
    if env::var_os("CARGO_FEATURE_BUNDLE").is_none() {
        fs::write(
            destination,
            "pub const PAYLOAD: Option<crate::Payload<'static>> = None;",
        )
        .unwrap();
        return;
    }
    let directory = PathBuf::from(
        env::var_os("APP_PROXY_PAYLOAD_DIR")
            .expect("bundle requires APP_PROXY_PAYLOAD_DIR from scripts/package.ps1"),
    );
    let build = env::var("APP_PROXY_BUILD_ID").expect("bundle requires APP_PROXY_BUILD_ID");
    let mut values = Vec::new();
    for file in ["app-proxy.exe", "app-proxy-host.exe"] {
        let path = directory
            .join(file)
            .canonicalize()
            .expect("build both release executables first");
        let bytes = fs::read(&path).unwrap();
        assert!(
            bytes.len() > 4096 && bytes.starts_with(b"MZ"),
            "invalid Windows executable"
        );
        let pe = u32::from_le_bytes(bytes[60..64].try_into().unwrap()) as usize;
        assert!(
            bytes.get(pe..pe + 4) == Some(b"PE\0\0")
                && bytes.get(pe + 4..pe + 6) == Some(&[0x64, 0x86]),
            "only Windows x64 payloads are supported"
        );
        println!("cargo:rerun-if-changed={}", path.display());
        values.push(format!(
            "crate::FilePayload {{ bytes: include_bytes!({:?}), sha256: {:?} }}",
            path.to_str().unwrap(),
            format!("{:x}", Sha256::digest(&bytes))
        ));
    }
    fs::write(destination, format!("pub const PAYLOAD: Option<crate::Payload<'static>> = Some(crate::Payload {{ build: {build:?}, files: [{}, {}] }});", values[0], values[1])).unwrap();
}
