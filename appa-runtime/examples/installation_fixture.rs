//! Test-only offline fixture for the current platform. This is not the release
//! publisher: unused platform/image entries are placeholders and it must never
//! be distributed as an official generation.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use appa_package::generation::{ArtifactDigest, Generation, Image, Platform, REPOSITORY};
use appa_package::{PackageKind, PackageName};
use appa_runtime::installation::Selection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "usage: installation_fixture <release-identity-binary> <native-plugin-archive> <new-bundle-path>".into(),
        );
    }
    let binary = Path::new(&args[0]);
    let plugin = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    let identity = std::process::Command::new(binary).arg("build-info").output()?;
    if !identity.status.success() {
        return Err("fixture binary did not report build identity".into());
    }
    let identity: serde_json::Value = serde_json::from_slice(&identity.stdout)?;
    let release = identity["release"]
        .as_str()
        .ok_or("fixture binary has no release identity")?;
    let plugin_digest = digest(plugin)?;
    if identity["plugin_sha256"].as_str() != Some(plugin_digest.hex()) {
        return Err("fixture native plugin is not this binary's compiled twin".into());
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let marketplace = repository.join("marketplace");
    let binary_archive = tempfile::NamedTempFile::new()?;
    let platform = Platform::current().ok_or("unsupported fixture platform")?;
    if matches!(platform, Platform::WindowsAmd64 | Platform::WindowsArm64) {
        let mut zip = zip::ZipWriter::new(File::create(binary_archive.path())?);
        zip.start_file("appa.exe", zip::write::SimpleFileOptions::default())?;
        std::io::copy(&mut File::open(binary)?, &mut zip)?;
        zip.finish()?;
    } else {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            File::create(binary_archive.path())?,
            flate2::Compression::fast(),
        ));
        tar.append_path_with_name(binary, "appa")?;
        tar.into_inner()?.finish()?;
    }
    let marketplace_archive = tempfile::NamedTempFile::new()?;
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        File::create(marketplace_archive.path())?,
        flate2::Compression::fast(),
    ));
    tar.append_dir_all(".", &marketplace)?;
    tar.into_inner()?.finish()?;
    let binary_digest = digest(binary_archive.path())?;
    let marketplace_digest = digest(marketplace_archive.path())?;
    let placeholder = ArtifactDigest::of_bytes(b"unused fixture artifact");
    let descriptor = serde_json::json!({"schema":1,"repository":REPOSITORY,"commit":identity["commit"],"release":release,"protocol":appa_package::PROTOCOL,
        "catalog":digest(&marketplace.join("marketplace.toml"))?,"marketplace":marketplace_digest,"claude_plugin":plugin_digest,"runtime_chart":placeholder,
        "binaries":Platform::ALL.into_iter().map(|p|(p,if p == platform { binary_digest.clone() } else { placeholder.clone() })).collect::<BTreeMap<_,_>>(),
        "images":Image::ALL.into_iter().map(|image|(image,serde_json::json!({"digest":placeholder,"platforms":{"linux/amd64":placeholder}}))).collect::<BTreeMap<_,_>>()});
    let generation = Generation::parse(&serde_json::to_vec(&descriptor)?)?;
    let mut selection = Selection::empty(generation, platform);
    selection.select(PackageKind::Plugin, &PackageName::parse("claude-code")?);
    let output_file = File::create_new(output)?;
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(output_file, flate2::Compression::fast()));
    append(&mut tar, "appa-generation.json", &serde_json::to_vec(&descriptor)?)?;
    append(&mut tar, "selection.json", &serde_json::to_vec(&selection)?)?;
    append(
        &mut tar,
        "config.toml",
        &std::fs::read(marketplace.join("plugins/claude-code/default.appa.toml"))?,
    )?;
    tar.append_dir_all("marketplace", &marketplace)?;
    tar.append_path_with_name(binary_archive.path(), format!("artifacts/{}", platform.archive()))?;
    tar.append_path_with_name(plugin, format!("artifacts/appa-plugin-{}.tar.gz", &release[1..]))?;
    tar.append_path_with_name(
        marketplace_archive.path(),
        format!("artifacts/appa-marketplace-{}.tar.gz", &release[1..]),
    )?;
    tar.into_inner()?.finish()?.sync_all()?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &serde_json::json!({"bundle":output,"sha256":digest(output)?.hex(),"commit":identity["commit"]}),
    )?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn digest(path: &Path) -> std::io::Result<ArtifactDigest> {
    ArtifactDigest::of_reader(File::open(path)?, 512 * 1024 * 1024)
}

fn append(tar: &mut tar::Builder<flate2::write::GzEncoder<File>>, path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, path, bytes)
}
