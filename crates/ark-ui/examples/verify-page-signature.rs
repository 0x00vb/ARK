//! Verify a page manifest with the same Minisign mode as the native updater.

use std::path::PathBuf;

use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let public_key = args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: verify-page-signature PUBLIC_KEY MANIFEST SIGNATURE")
    })?;
    let manifest = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("manifest path is required"))?;
    let signature = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("signature path is required"))?;
    if args.next().is_some() {
        anyhow::bail!("too many arguments");
    }
    let decode = |value: String| -> anyhow::Result<String> {
        Ok(String::from_utf8(
            base64::engine::general_purpose::STANDARD.decode(value.trim())?,
        )?)
    };
    let public_key = PublicKey::decode(&decode(std::fs::read_to_string(public_key)?)?)?;
    let signature = Signature::decode(&decode(std::fs::read_to_string(signature)?)?)?;
    public_key.verify(&std::fs::read(manifest)?, &signature, true)?;
    println!("page manifest signature verified");
    Ok(())
}
