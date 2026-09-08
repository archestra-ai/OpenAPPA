//! Test-only kagent bundle for an actual image/registry acceptance run.
//!
//! The input descriptor supplies measured image digests and the real chart
//! digest. This helper never invents or verifies registry image evidence.
//! Unused Claude/binary descriptor entries may be placeholders; their artifacts
//! are not included. This fixture must not be published as an official generation.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use appa_package::generation::{ArtifactDigest, Generation, MAX_DESCRIPTOR_BYTES, Platform};
use appa_package::tree::{EntryKind, MAX_UNCOMPRESSED_BYTES, walk};
use appa_package::{PackageKind, PackageName};
use appa_runtime::installation::{Acquired, Selection};

const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: kagent_installation_fixture <descriptor-input.json> <config.toml> <runtime-chart.tgz> <output-bundle.tar.gz>".into());
    }
    let output = Path::new(&args[3]);
    let digest = build(Path::new(&args[0]), Path::new(&args[1]), Path::new(&args[2]), output)?;
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &serde_json::json!({"bundle": output, "sha256": digest.hex()}),
    )?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn build(
    descriptor: &Path,
    config: &Path,
    chart: &Path,
    output: &Path,
) -> Result<ArtifactDigest, Box<dyn std::error::Error>> {
    // Refuse an existing destination before doing any packaging work. The final
    // persist_noclobber also protects against a destination created meanwhile.
    match fs::symlink_metadata(output) {
        Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "fixture output already exists").into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let input = Generation::parse(&read_bounded(descriptor, MAX_DESCRIPTOR_BYTES as u64)?)?;
    let config = read_bounded(config, MAX_CONFIG_BYTES)?;
    let stage = tempfile::tempdir()?;
    let chart_snapshot = stage.path().join("runtime-chart.tgz");
    copy_bounded(chart, &chart_snapshot, MAX_ARTIFACT_BYTES)?;
    let chart_digest = digest(&chart_snapshot)?;
    let mut descriptor = serde_json::to_value(&input)?;
    if descriptor["runtime_chart"] != serde_json::to_value(&chart_digest)? {
        return Err("input runtime_chart digest does not match the supplied chart archive".into());
    }

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate belongs to the repository");
    let marketplace = stage.path().join("marketplace");
    snapshot_tree(&repository.join("marketplace"), &marketplace)?;
    let marketplace_archive = stage.path().join("marketplace.tar.gz");
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        File::create_new(&marketplace_archive)?,
        flate2::Compression::fast(),
    ));
    tar.append_dir_all(".", &marketplace)?;
    tar.into_inner()?.finish()?.sync_all()?;
    descriptor["catalog"] = serde_json::to_value(digest(&marketplace.join("marketplace.toml"))?)?;
    descriptor["marketplace"] = serde_json::to_value(digest(&marketplace_archive)?)?;
    let descriptor = serde_json::to_vec(&descriptor)?;
    let generation = Generation::parse(&descriptor)?;
    let version = generation
        .release()
        .strip_prefix('v')
        .expect("generation validates its release prefix")
        .to_owned();
    let platform = Platform::current().ok_or("unsupported fixture platform")?;
    let mut selection = Selection::empty(generation, platform);
    // Selecting kagent defaults to both runtimes through the real selection API.
    selection.select(PackageKind::Plugin, &PackageName::parse("kagent")?);

    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        temporary.as_file_mut(),
        flate2::Compression::fast(),
    ));
    append(&mut tar, "appa-generation.json", &descriptor)?;
    append(&mut tar, "selection.json", &serde_json::to_vec(&selection)?)?;
    append(&mut tar, "config.toml", &config)?;
    tar.append_dir_all("marketplace", &marketplace)?;
    tar.append_path_with_name(
        &marketplace_archive,
        format!("artifacts/appa-marketplace-{version}.tar.gz"),
    )?;
    tar.append_path_with_name(&chart_snapshot, format!("artifacts/appa-runtime-{version}.tgz"))?;
    tar.into_inner()?.finish()?.sync_all()?;
    let result = digest(temporary.path())?;
    // Exercise the actual offline importer before publishing. This checks the
    // expanded bundle/entry limits, selected package identities, configuration,
    // and required archives. Authored custom includes/files require a snapshot
    // and are intentionally refused here; use the installer export for those.
    drop(Acquired::import(temporary.path(), &result)?);
    temporary.persist_noclobber(output)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(result)
}

fn regular(path: &Path) -> io::Result<File> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture inputs must be regular files",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture input changed type",
        ));
    }
    Ok(file)
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    regular(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture input exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

fn copy_bounded(source: &Path, target: &Path, limit: u64) -> io::Result<u64> {
    let input = regular(source)?;
    let permissions = input.metadata()?.permissions();
    let mut output = File::create_new(target)?;
    let copied = io::copy(&mut input.take(limit + 1), &mut output)?;
    if copied > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture input exceeds its byte limit",
        ));
    }
    output.set_permissions(permissions)?;
    output.sync_all()?;
    Ok(copied)
}

fn snapshot_tree(source: &Path, target: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let entries = walk(source)?;
    fs::create_dir(target)?;
    let mut remaining = MAX_UNCOMPRESSED_BYTES;
    for entry in entries {
        let destination = target.join(entry.portable);
        match entry.kind {
            EntryKind::Directory => fs::create_dir_all(destination)?,
            EntryKind::File => {
                fs::create_dir_all(destination.parent().expect("snapshot entries have a parent"))?;
                remaining -= copy_bounded(&entry.absolute, &destination, remaining)?;
            }
        }
    }
    Ok(())
}

fn digest(path: &Path) -> io::Result<ArtifactDigest> {
    ArtifactDigest::of_reader(regular(path)?, MAX_ARTIFACT_BYTES)
}

fn append<W: Write>(tar: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_reads_are_bounded_and_output_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("existing");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(read_bounded(&path, 3).unwrap(), b"abc");
        assert!(read_bounded(&path, 2).is_err());
        assert!(build(&path, &path, &path, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        let output = root.path().join("absent");
        assert!(build(&path, &path, &path, &output).is_err());
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[test]
    fn inputs_refuse_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let link = root.path().join("link");
        fs::write(&target, b"abc").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_bounded(&link, 3).is_err());
    }
}
