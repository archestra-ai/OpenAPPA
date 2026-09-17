//! Release tooling. Run only after all binaries, archives and images exist.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::Path;

use appa_package::generation::{ArtifactDigest, Generation, Image, ImageDigests, Platform, REPOSITORY};

fn digest(path: &Path) -> Result<ArtifactDigest, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !file.metadata().map_err(|error| error.to_string())?.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    ArtifactDigest::of_reader(file, 512 * 1024 * 1024).map_err(|error| format!("{}: {error}", path.display()))
}

fn render(dist: &Path, marketplace: &Path, images: &Path, commit: &str, release: &str) -> Result<String, String> {
    let version = release.strip_prefix('v').ok_or("release must start with v")?;
    // Validate the commit/tag before deriving any paths from them.
    appa_package::generation::Commit::parse(commit).map_err(|error| error.to_string())?;
    if version.is_empty()
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
    {
        return Err("invalid release tag".into());
    }
    let mut binaries = BTreeMap::new();
    for platform in Platform::ALL {
        binaries.insert(platform, digest(&dist.join(platform.archive()))?);
    }
    let mut image_digests = BTreeMap::new();
    for image in Image::ALL {
        let name = serde_json::to_value(image).map_err(|error| error.to_string())?;
        let name = name.as_str().expect("Image serializes as a string");
        let path = images.join(format!("{name}.json"));
        let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        if bytes.len() > 64 * 1024 {
            return Err(format!("{} is too large", path.display()));
        }
        let descriptor: ImageDigests = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        image_digests.insert(image, descriptor);
    }
    let catalog = marketplace.join("marketplace.toml");
    let listed = appa_package::Marketplace::read(&catalog).map_err(|error| error.to_string())?;
    let mut packages = Vec::new();
    for entry in listed.packages {
        let path = marketplace.join(entry.path.as_path());
        let package = appa_package::validate_package(&path).map_err(|error| error.to_string())?;
        if package.name != entry.name
            || appa_package::TreeDigest::of_tree(&path).map_err(|error| error.to_string())? != entry.digest
        {
            return Err(format!("{} does not match its catalog entry", path.display()));
        }
        let kind = match package.role {
            appa_package::Role::Plugin(_) => appa_package::PackageKind::Plugin,
            appa_package::Role::Battery(_) => appa_package::PackageKind::Battery,
        };
        if kind != entry.kind {
            return Err(format!("{} has the wrong package kind", path.display()));
        }
        packages.push(package);
    }
    appa_package::check_ownership(&packages).map_err(|error| error.to_string())?;
    let document = serde_json::json!({
        "schema": 1, "repository": REPOSITORY, "commit": commit, "release": release,
        "protocol": appa_package::PROTOCOL, "catalog": digest(&catalog)?,
        "marketplace": digest(&dist.join(format!("appa-marketplace-{version}.tar.gz")))?,
        "batteries": digest(&dist.join(format!("appa-batteries-{version}.tar.gz")))?,
        "runtime_chart": digest(&dist.join(format!("appa-runtime-{version}.tgz")))?,
        "binaries": binaries, "images": image_digests,
    });
    let bytes = serde_json::to_vec(&document).map_err(|error| error.to_string())?;
    let generation = Generation::parse(&bytes).map_err(|error| error.to_string())?;
    serde_json::to_string_pretty(&generation).map_err(|error| error.to_string())
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        eprintln!("usage: generation <dist> <marketplace> <image-descriptors> <commit> <release-tag>");
        std::process::exit(2);
    }
    match render(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        &args[3],
        &args[4],
    ) {
        Ok(document) => println!("{document}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_built_from_artifacts_and_refuses_a_missing_artifact() {
        let root = tempfile::tempdir().unwrap();
        let dist = root.path().join("dist");
        let marketplace = root.path().join("marketplace");
        let images = root.path().join("images");
        for directory in [&dist, &marketplace, &images] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(marketplace.join("marketplace.toml"), "schema = 1\nname = 'appa'\n").unwrap();
        for platform in Platform::ALL {
            fs::write(dist.join(platform.archive()), b"runtime").unwrap();
        }
        for artifact in [
            "appa-marketplace-1.0.0.tar.gz",
            "appa-batteries-1.0.0.tar.gz",
            "appa-runtime-1.0.0.tgz",
        ] {
            fs::write(dist.join(artifact), b"archive").unwrap();
        }
        let image = serde_json::json!({"digest": ArtifactDigest::of_bytes(b"index"),
            "platforms": {"linux/amd64": ArtifactDigest::of_bytes(b"image")}});
        for name in ["runtime", "python", "go"] {
            fs::write(images.join(format!("{name}.json")), serde_json::to_vec(&image).unwrap()).unwrap();
        }
        let document = render(&dist, &marketplace, &images, &"a".repeat(40), "v1.0.0").unwrap();
        let generation = Generation::parse(document.as_bytes()).unwrap();
        assert_eq!(
            generation.published().unwrap().binaries()[&Platform::MacArm64],
            ArtifactDigest::of_bytes(b"runtime")
        );
        fs::remove_file(dist.join(Platform::MacArm64.archive())).unwrap();
        assert!(render(&dist, &marketplace, &images, &"a".repeat(40), "v1.0.0").is_err());
    }
}
