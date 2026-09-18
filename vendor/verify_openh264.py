"""Verify the experimental OpenH264 copy without building or fetching anything."""
import argparse
import difflib
import hashlib
import json
from pathlib import Path, PurePosixPath
import tarfile

PACKAGE = "openh264-sys2-0.9.6"
ARCHIVE_SHA256 = "fa9e072e9b270f3b291c80488dc160abc31ecc214ab3bfde937213cfd8c83b32"
ORIGINAL_BUILD_SHA256 = "cf01f861c8cb2ab1e4aae8d741a0aef2a43baa635deb1bbc81d90cf5fd278838"
PATCHED_BUILD_SHA256 = "88b25e67b70b21a056a56494d532326076c6de4e87e30f4163b953adbd307f7e"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, help="Optional cached original .crate archive")
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    root = here / PACKAGE
    provenance = json.loads((here / f"{PACKAGE}.provenance.json").read_text(encoding="utf-8"))
    require(provenance["archive_sha256"] == ARCHIVE_SHA256, "Archive identity changed")
    require(provenance["original_build_sha256"] == ORIGINAL_BUILD_SHA256, "Original build identity changed")
    require(provenance["patched_build_sha256"] == PATCHED_BUILD_SHA256, "Patched build identity changed")
    files = {p.relative_to(root).as_posix(): p.read_bytes() for p in root.rglob("*") if p.is_file()}
    expected = provenance["files"]
    require(len(files) == 343 and set(files) == set(expected), "Packaged file set changed")
    for name, data in files.items():
        wanted = PATCHED_BUILD_SHA256 if name == "build.rs" else expected[name]
        require(sha256(data) == wanted, f"Unexpected packaged bytes: {name}")

    original = files["build.rs"]
    for addition in (
        b"        Riscv64,\r\n",
        b'                "riscv64" => TargetArch::Riscv64,\r\n',
    ):
        require(original.count(addition) == 1, "Expected one exact added line")
        original = original.replace(addition, b"")
    require(sha256(original) == ORIGINAL_BUILD_SHA256, "Patch contains changes beyond the two additions")
    patch = "".join(difflib.unified_diff(
        original.decode("utf-8").splitlines(True),
        files["build.rs"].decode("utf-8").splitlines(True),
        fromfile="a/build.rs", tofile="b/build.rs",
    )).encode("utf-8")
    require((here / f"{PACKAGE}-riscv64.patch").read_bytes() == patch, "Recorded patch differs")

    if args.archive is not None:
        require(sha256(args.archive.read_bytes()) == ARCHIVE_SHA256, "Original archive checksum differs")
        archived = {}
        with tarfile.open(args.archive, "r:gz") as archive:
            for member in archive.getmembers():
                name = PurePosixPath(member.name)
                require(not name.is_absolute() and ".." not in name.parts and name.parts[0] == PACKAGE,
                        "Unexpected archive path")
                if member.isdir():
                    continue
                require(member.isfile(), "Unexpected nonregular archive entry")
                relative = str(PurePosixPath(*name.parts[1:]))
                require(relative not in archived, "Duplicate archive file")
                archived[relative] = sha256(archive.extractfile(member).read())
        require(archived == expected, "Provenance does not match original archive")
    print("Verified 343 packaged files; only the two experimental build.rs lines differ.")


if __name__ == "__main__":
    main()
