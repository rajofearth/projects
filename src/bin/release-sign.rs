//! Signs release exes for the updater.
//!
//! release-sign keygen                      prints a new key pair
//! release-sign sign <exe> <version>        prints the .sig file; the key comes from RELEASE_SIGNING_KEY
//! release-sign verify <exe> <sig>          checks a .sig against the exe and the key built into the app
//!
//! The format is the one the app checks, from `src/update/verify.rs`.

#[path = "../update/verify.rs"]
#[allow(dead_code)]
mod verify;

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn sha256_file(path: &str) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("can't read {path}: {e}"))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("can't read {path}: {e}"))?;
    Ok(verify::to_hex(&h.finalize()))
}

fn signing_key() -> Result<SigningKey, String> {
    let hex = std::env::var("RELEASE_SIGNING_KEY").map_err(|_| "RELEASE_SIGNING_KEY isn't set".to_string())?;
    let bytes = verify::from_hex(hex.trim()).filter(|b| b.len() == 32).ok_or("RELEASE_SIGNING_KEY isn't 32 bytes of hex")?;
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&seed))
}

fn run(args: &[String]) -> Result<(), String> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen"] => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| format!("no random source: {e}"))?;
            let key = SigningKey::from_bytes(&seed);
            println!("secret {}", verify::to_hex(&seed));
            let public: Vec<String> = key.verifying_key().to_bytes().iter().map(|b| format!("0x{b:02x}")).collect();
            println!("public [{}]", public.join(", "));
            Ok(())
        }
        ["sign", exe, version] => {
            let key = signing_key()?;
            if key.verifying_key().to_bytes() != verify::PUBLIC_KEY {
                return Err("RELEASE_SIGNING_KEY doesn't match the public key in src/update/verify.rs".into());
            }
            let sha256 = sha256_file(exe)?;
            let signature = key.sign(verify::message(version, &sha256).as_bytes()).to_bytes();
            print!("{}", verify::SigFile { version: version.to_string(), sha256, signature }.render());
            Ok(())
        }
        ["verify", exe, sig] => {
            let text = std::fs::read_to_string(sig).map_err(|e| format!("can't read {sig}: {e}"))?;
            let file = verify::SigFile::parse(&text)?;
            if sha256_file(exe)? != file.sha256 {
                return Err(format!("{exe} doesn't match the checksum in {sig}"));
            }
            file.verify(&verify::PUBLIC_KEY)?;
            println!("{exe} {}: signature good", file.version);
            Ok(())
        }
        _ => Err("usage: release-sign keygen | sign <exe> <version> | verify <exe> <sig>".into()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("release-sign: {e}");
        std::process::exit(1);
    }
}
