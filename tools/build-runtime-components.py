"""Build Windows x64 on-demand component assets and their release catalog."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
from urllib.parse import urlsplit
from urllib.request import urlopen
import zipfile


ROOT = Path(__file__).resolve().parent.parent
TARGET = "x86_64-pc-windows-gnu"
PYTHON = "3.13.12"
PYTHON_SHA256 = "76f238f606250c87c6beac75dccd35ee99070a13490555936abb6cb64ecce3d0"
RUST = "1.93.0"
RUST_DIST = "https://static.rust-lang.org/dist/2026-01-22"
RUST_SHA256 = {
    "cargo": "035f31e1204dce65652cf35bc054f0bcade06ea0c1a40aff9b4a9c76453be863",
    "rustc": "139f1f1d904198d329344726e8adb16193e18512236f5417036cc4a82dd665f4",
    "rust-std": "a07c6ab596fad15ca7acd63ee7f2a5fea93fd421179252067e309c2aa0b2021b",
    "rust-mingw": "1aa52dfc1299896b3da4cec3882b18ffd4ab4f5d9c7516fcfd17e670d4d2b5e6",
}


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def download(cache, url, expected):
    path = cache / url.rsplit("/", 1)[1]
    if path.is_file() and sha256(path) == expected:
        return path
    print(f"Fetching {url}", flush=True)
    with tempfile.NamedTemporaryFile(dir=cache, delete=False) as stream:
        partial = Path(stream.name)
        try:
            with urlopen(url, timeout=90) as response:
                shutil.copyfileobj(response, stream)
        except Exception:
            stream.close()
            partial.unlink(missing_ok=True)
            raise
    try:
        if sha256(partial) != expected:
            raise ValueError(f"Upstream SHA-256 mismatch for {url}")
        partial.replace(path)
    finally:
        partial.unlink(missing_ok=True)
    return path


def stage_python(cache, stage):
    filename = f"python-{PYTHON}-embed-amd64.zip"
    package = download(cache, f"https://www.python.org/ftp/python/{PYTHON}/{filename}", PYTHON_SHA256)
    primary = stage / "primary-runtime"
    interpreter = primary / "dependencies/python"
    interpreter.mkdir(parents=True)
    with zipfile.ZipFile(package) as archive:
        archive.extractall(interpreter)
    (interpreter / "Lib/site-packages").mkdir(parents=True)
    pth = next(interpreter.glob("python*._pth"))
    pth.write_text(pth.read_text().rstrip() + "\nLib/site-packages\n", encoding="utf-8", newline="\n")
    (primary / "runtime.json").write_text(
        json.dumps({
            "desktopVersion": "dshr",
            "platform": "win32",
            "arch": "x64",
            "python": PYTHON,
            "pythonPackages": {},
        }) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def stage_creator(cache, stage, work, cargo):
    toolchain = stage / "toolchain"
    for name, checksum in RUST_SHA256.items():
        prefix = f"{name}-{RUST}-{TARGET}"
        archive = download(cache, f"{RUST_DIST}/{prefix}.tar.xz", checksum)
        unpack = work / name
        with tarfile.open(archive) as bundle:
            bundle.extractall(unpack, filter="data")
        root = unpack / prefix
        for component in (root / "components").read_text().splitlines():
            shutil.copytree(
                root / component, toolchain, dirs_exist_ok=True,
                ignore=shutil.ignore_patterns("manifest.in"),
            )
        for licence in root.glob("LICENSE*"):
            target = stage / "licenses" / name / licence.name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(licence, target)
        shutil.rmtree(unpack)
    sdk = work / "plugin-sdk"
    shutil.copytree(ROOT / "crates/plugin-sdk", sdk)
    with (sdk / "Cargo.toml").open("a", encoding="utf-8") as manifest:
        manifest.write("\n[workspace]\n")
    shutil.copy2(ROOT / "Cargo.lock", sdk / "Cargo.lock")
    with (stage / "cargo-vendor.toml").open("w", encoding="utf-8") as config:
        subprocess.run(
            [cargo, "vendor", "--versioned-dirs", str(stage / "vendor")],
            cwd=sdk, check=True, stdout=config,
        )
    (stage / "cargo-vendor.toml").unlink()
    shutil.copy2(sdk / "Cargo.lock", stage / "sdk-dependencies.lock")


def create_asset(stage, out, component, version, base_url):
    filename = f"{component}-{version}-windows-x64.zip"
    path = out / "runtime-packages" / filename
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        partial = Path(stream.name)
    try:
        with zipfile.ZipFile(partial, "w") as archive:
            for item in sorted(stage.rglob("*")):
                if item.is_symlink():
                    raise ValueError(f"Unexpected component symlink: {item}")
                relative = item.relative_to(stage).as_posix()
                if item.is_dir():
                    if not any(item.iterdir()):
                        info = zipfile.ZipInfo(relative + "/")
                        info.create_system = 3
                        info.external_attr = (0o40755 << 16) | 0x10
                        archive.writestr(info, b"")
                    continue
                info = zipfile.ZipInfo(relative)
                info.create_system = 3
                info.compress_type = zipfile.ZIP_DEFLATED
                with item.open("rb") as source, archive.open(info, "w") as target:
                    shutil.copyfileobj(source, target)
        result = {"version": version, "archive": filename, "bytes": partial.stat().st_size,
                  "sha256": sha256(partial)}
        if base_url:
            result["url"] = f"{base_url.rstrip('/')}/{filename}"
        if path.exists():
            if sha256(path) != result["sha256"]:
                raise ValueError(f"Cannot replace immutable component {filename}")
        else:
            partial.rename(path)
        return result
    finally:
        partial.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--cache", type=Path, default=ROOT / ".tmp/runtime-component-sources")
    parser.add_argument("--base-url", help="HTTPS release directory for the generated ZIPs")
    parser.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    args = parser.parse_args()
    if args.base_url:
        url = urlsplit(args.base_url)
        if url.scheme != "https" or not url.hostname or url.username or url.password or url.query or url.fragment:
            parser.error("--base-url must be an HTTPS directory without credentials, query or fragment")
    out = args.out.resolve()
    cache = args.cache.resolve()
    cache.mkdir(parents=True, exist_ok=True)
    (out / "runtime-packages").mkdir(parents=True, exist_ok=True)
    assets = {}
    with tempfile.TemporaryDirectory(dir=cache) as temp:
        work = Path(temp)
        python = work / "python"
        stage_python(cache, python)
        assets["python"] = create_asset(python, out, "python", PYTHON, args.base_url)
        creator = work / "creator-build"
        creator.mkdir()
        stage_creator(cache, creator, work, args.cargo)
        assets["creator-build"] = create_asset(creator, out, "creator-build", RUST + "-sdk.1", args.base_url)
    metadata = {"schema_version": 1, "platform": "windows-x64", "components": assets}
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", newline="\n", dir=out, delete=False
    ) as catalog:
        catalog.write(json.dumps(metadata, indent=2) + "\n")
    Path(catalog.name).replace(out / "components.json")


if __name__ == "__main__":
    main()
