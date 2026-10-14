//! File-API integration tests (M17): the §2.6/§2.7 attack surface.
//!
//! These run against the real host filesystem (temp directories);
//! no Docker daemon is needed — the runtime-side surface lives in
//! the workload integration's docker tests.

use std::fs;
use std::path::{Path, PathBuf};

use zim_engine_files::{
    archive, extract_with_limits, ArchiveFormat, ExtractLimits, FileError, Filesystem,
};

fn tmp_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("zim-files-tests-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create temp root");
    root
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let target = root.join(rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(target, content).expect("write file");
}

// ── §2.6: traversal and symlinks ────────────────────────────────

#[test]
fn traversal_requests_are_rejected_for_every_operation() {
    let root = tmp_root();
    let fs_api = Filesystem::new(&root);
    for bad in ["../steal.txt", "worlds/../../steal.txt", "/etc/passwd"] {
        assert!(fs_api.read_file(bad).is_err(), "{bad:?} read");
        assert!(fs_api.write_file(bad, b"x").is_err(), "{bad:?} write");
        assert!(fs_api.delete(bad).is_err(), "{bad:?} delete");
        assert!(fs_api.list_dir(bad).is_err(), "{bad:?} list");
        assert!(fs_api.mkdir(bad).is_err(), "{bad:?} mkdir");
    }
}

#[cfg(unix)]
#[test]
fn an_escaping_symlink_cannot_be_read_through_the_api() {
    let root = tmp_root();
    let outside = tmp_root();
    write(&outside, "secret.txt", b"outside-secret");
    // The symlink lives inside the root but points outside it.
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("leak.txt")).expect("symlink");
    let fs_api = Filesystem::new(&root);
    let err = fs_api.read_file("leak.txt").unwrap_err();
    assert!(
        matches!(err, FileError::EscapesRoot(_)),
        "escaping symlink must be rejected, got {err:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_in_root_symlink_is_followed() {
    let root = tmp_root();
    write(&root, "real/notes.txt", b"notes");
    std::os::unix::fs::symlink("real", root.join("link")).expect("symlink");
    let fs_api = Filesystem::new(&root);
    assert_eq!(fs_api.read_file("link/notes.txt").unwrap(), b"notes");
}

#[cfg(unix)]
#[test]
fn a_symlink_directory_ancestor_escaping_the_root_is_rejected() {
    let root = tmp_root();
    let outside = tmp_root();
    write(&outside, "loot/gold.txt", b"gold");
    fs::create_dir_all(root.join("worlds")).expect("mkdir");
    std::os::unix::fs::symlink(outside.join("loot"), root.join("worlds/loot")).expect("symlink");
    let fs_api = Filesystem::new(&root);
    let err = fs_api.read_file("worlds/loot/gold.txt").unwrap_err();
    assert!(
        matches!(err, FileError::EscapesRoot(_)),
        "escaping symlink ancestor must be rejected, got {err:?}"
    );
}

// ── Archive round-trips ─────────────────────────────────────────

#[test]
fn tar_gz_archive_and_extract_round_trip() {
    let root = tmp_root();
    write(&root, "server.properties", b"server-port=25565\n");
    write(&root, "eula.txt", b"eula=true\n");
    write(&root, "worlds/world/level.dat", b"leveldata");

    let written = archive(
        &root,
        "backup.tar.gz",
        &["server.properties", "eula.txt", "worlds"],
        ArchiveFormat::TarGz,
    )
    .expect("archive");
    assert!(written > 0);

    let stats = extract_to(&root, "backup.tar.gz", "restored");
    assert_eq!(stats.entries, 5); // 2 nested dir entries + level.dat + 2 files
    let fs_api = Filesystem::new(&root);
    assert_eq!(
        fs_api.read_file("restored/server.properties").unwrap(),
        b"server-port=25565\n"
    );
    assert_eq!(
        fs_api.read_file("restored/worlds/world/level.dat").unwrap(),
        b"leveldata"
    );
}

#[test]
fn plain_tar_and_zip_round_trip() {
    let root = tmp_root();
    write(&root, "a.txt", b"aaa");
    write(&root, "dir/b.txt", b"bbb");

    archive(&root, "copy.tar", &["a.txt", "dir"], ArchiveFormat::Tar).expect("tar");
    let stats = extract_to(&root, "copy.tar", "out-tar");
    assert_eq!(stats.entries, 3);
    assert_eq!(
        Filesystem::new(&root)
            .read_file("out-tar/dir/b.txt")
            .unwrap(),
        b"bbb"
    );

    archive(&root, "copy.zip", &["a.txt", "dir"], ArchiveFormat::Zip).expect("zip");
    let stats = extract_to(&root, "copy.zip", "out-zip");
    assert_eq!(stats.entries, 3);
    assert_eq!(
        Filesystem::new(&root).read_file("out-zip/a.txt").unwrap(),
        b"aaa"
    );
}

#[test]
fn archives_are_deterministic_for_identical_trees() {
    let root = tmp_root();
    write(&root, "a.txt", b"same bytes");
    let first = archive(&root, "one.tar", &["a.txt"], ArchiveFormat::Tar).unwrap();
    let second = archive(&root, "two.tar", &["a.txt"], ArchiveFormat::Tar).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        fs::read(root.join("one.tar")).unwrap(),
        fs::read(root.join("two.tar")).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn the_archiver_follows_in_root_symlinks_but_not_escapes() {
    let root = tmp_root();
    write(&root, "real/x.txt", b"x");
    std::os::unix::fs::symlink("real", root.join("linked")).expect("symlink");
    archive(&root, "snap.tar", &["linked"], ArchiveFormat::Tar).expect("archive");
    extract_to(&root, "snap.tar", "restored");
    assert_eq!(
        Filesystem::new(&root)
            .read_file("restored/linked/x.txt")
            .unwrap(),
        b"x"
    );

    let outside = tmp_root();
    write(&outside, "y.txt", b"y");
    std::os::unix::fs::symlink(outside.join("y.txt"), root.join("escape.txt")).expect("symlink");
    let err = archive(&root, "bad.tar", &["escape.txt"], ArchiveFormat::Tar).unwrap_err();
    assert!(matches!(err, FileError::EscapesRoot(_)), "got {err:?}");
}

// ── §2.7: extraction attacks ────────────────────────────────────

/// One hand-built tar entry: full control over the raw header (the
/// tar *builder* refuses `..` names, absolute names, and link
/// types — exactly the hostile shapes the §2.7 tests need, so the
/// fixtures are assembled at the byte level).
struct RawTarEntry {
    /// The entry name, verbatim into the header's name field.
    name: &'static str,
    /// Regular-file data (`None` for a directory); symlink targets
    /// ride `link`.
    data: Option<&'static [u8]>,
    /// The typeflag: `b'0'` regular, `b'5'` directory, `b'2'` symlink.
    typeflag: u8,
    /// The symlink target (linkname field).
    link: Option<&'static str>,
}

/// Hand-build a tar archive from raw entries (ustar headers, GNU
/// checksum convention — the format every tar reader accepts).
fn build_raw_tar(root: &Path, dest: &str, entries: &[RawTarEntry]) {
    let mut out: Vec<u8> = Vec::new();
    for entry in entries {
        let mut header = [0u8; 512];
        // Name (100 bytes, NUL-padded).
        header[..entry.name.len()].copy_from_slice(entry.name.as_bytes());
        // Mode, uid, gid: 0644 / 0 / 0 in the octal fields.
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        // Size (octal, 11 digits + NUL).
        let size = entry.data.map(<[u8]>::len).unwrap_or(0);
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        // Mtime: 0.
        header[136..148].copy_from_slice(b"00000000000\0");
        // Checksum: computed below (write spaces first).
        header[148..156].copy_from_slice(b"        ");
        // Typeflag.
        header[156] = entry.typeflag;
        // Linkname (symlink target).
        if let Some(link) = entry.link {
            header[157..157 + link.len()].copy_from_slice(link.as_bytes());
        }
        // Magic + version (ustar).
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        // Checksum: the unsigned sum of all 512 header bytes with
        // the checksum field read as spaces.
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&header);
        if let Some(data) = entry.data {
            out.extend_from_slice(data);
            let pad = (512 - (data.len() % 512)) % 512;
            out.extend(std::iter::repeat_n(0u8, pad));
        }
    }
    // Two zero blocks terminate the archive.
    out.extend([0u8; 1024]);
    fs::write(root.join(dest), out).expect("write raw tar");
}

#[test]
fn tar_slip_entries_are_rejected_not_extracted() {
    let root = tmp_root();
    build_raw_tar(
        &root,
        "evil.tar",
        &[
            RawTarEntry {
                name: "../escaped.txt",
                data: Some(b"stolen"),
                typeflag: b'0',
                link: None,
            },
            RawTarEntry {
                name: "ok.txt",
                data: Some(b"fine"),
                typeflag: b'0',
                link: None,
            },
        ],
    );
    let err = extract_to_err(&root, "evil.tar", "out");
    assert!(matches!(err, FileError::Traversal(_)), "got {err:?}");
    assert!(!root.parent().unwrap().join("escaped.txt").exists());
    // Nothing partial was extracted either: the rejection happened
    // on entry 1, before `ok.txt` was written.
    assert!(!root.join("out").join("ok.txt").exists());
}

#[test]
fn absolute_and_backslash_entry_names_are_rejected() {
    let root = tmp_root();
    build_raw_tar(
        &root,
        "abs.tar",
        &[RawTarEntry {
            name: "/abs.txt",
            data: Some(b"x"),
            typeflag: b'0',
            link: None,
        }],
    );
    assert!(matches!(
        extract_to_err(&root, "abs.tar", "out"),
        FileError::Absolute(_)
    ));
    build_raw_tar(
        &root,
        "win.tar",
        &[RawTarEntry {
            name: "a\\b.txt",
            data: Some(b"x"),
            typeflag: b'0',
            link: None,
        }],
    );
    assert!(matches!(
        extract_to_err(&root, "win.tar", "out"),
        FileError::Traversal(_)
    ));
}

#[test]
fn symlink_entries_are_rejected() {
    let root = tmp_root();
    // A tar entry whose type is a symlink with a far-away target.
    build_raw_tar(
        &root,
        "link.tar",
        &[RawTarEntry {
            name: "evil-link",
            data: None,
            typeflag: b'2',
            link: Some("/etc/passwd"),
        }],
    );
    let err = extract_to_err(&root, "link.tar", "out");
    assert!(matches!(err, FileError::LinkEntry(_)), "got {err:?}");
}

#[test]
fn overlapping_entries_are_rejected() {
    let root = tmp_root();
    build_raw_tar(
        &root,
        "dup.tar",
        &[
            RawTarEntry {
                name: "twice.txt",
                data: Some(b"one"),
                typeflag: b'0',
                link: None,
            },
            RawTarEntry {
                name: "twice.txt",
                data: Some(b"two"),
                typeflag: b'0',
                link: None,
            },
        ],
    );
    let err = extract_to_err(&root, "dup.tar", "out");
    assert!(matches!(err, FileError::Overlap(_)), "got {err:?}");
}

#[test]
fn the_uncompressed_size_cap_fires_before_the_copy() {
    let root = tmp_root();
    // One 100-byte entry against a 50-byte cap.
    build_raw_tar(
        &root,
        "bomb.tar",
        &[RawTarEntry {
            name: "big.bin",
            data: Some(&[0u8; 100]),
            typeflag: b'0',
            link: None,
        }],
    );
    let err = extract_with_limits(
        &root,
        "bomb.tar",
        "out",
        &ExtractLimits {
            max_uncompressed_bytes: 50,
            max_entries: 100_000,
            max_nesting_depth: 3,
        },
    )
    .unwrap_err();
    assert!(matches!(err, FileError::Limit(_)), "got {err:?}");
}

#[test]
fn the_entry_count_cap_fires() {
    let root = tmp_root();
    build_raw_tar(
        &root,
        "many.tar",
        &[
            RawTarEntry {
                name: "a.txt",
                data: Some(b"a"),
                typeflag: b'0',
                link: None,
            },
            RawTarEntry {
                name: "b.txt",
                data: Some(b"b"),
                typeflag: b'0',
                link: None,
            },
            RawTarEntry {
                name: "c.txt",
                data: Some(b"c"),
                typeflag: b'0',
                link: None,
            },
        ],
    );
    let err = extract_with_limits(
        &root,
        "many.tar",
        "out",
        &ExtractLimits {
            max_uncompressed_bytes: 5 * 1024 * 1024 * 1024,
            max_entries: 2,
            max_nesting_depth: 3,
        },
    )
    .unwrap_err();
    assert!(matches!(err, FileError::Limit(_)), "got {err:?}");
}

#[test]
fn nested_archives_recurse_and_stop_at_the_depth_cap() {
    let root = tmp_root();
    // inner.tar (plain tar with one file) — built in a subdir so
    // the outer archive collects it.
    let staging = root.join("stage");
    fs::create_dir_all(&staging).expect("mkdir");
    {
        let file = fs::File::create(staging.join("inner.tar")).expect("create inner");
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, "deep.txt", &b"xyz"[..])
            .expect("append");
        builder.finish().expect("finish");
    }
    write(&root, "plain.txt", b"top");
    archive(
        &root,
        "outer.tar",
        &["stage/inner.tar", "plain.txt"],
        ArchiveFormat::Tar,
    )
    .expect("outer");

    // Depth cap 2: outer (1) → inner (2) is allowed.
    let stats = extract_with_limits(
        &root,
        "outer.tar",
        "out",
        &ExtractLimits {
            max_uncompressed_bytes: 5 * 1024 * 1024 * 1024,
            max_entries: 100_000,
            max_nesting_depth: 2,
        },
    )
    .expect("extract with recursion");
    assert_eq!(stats.nested_archives, 1);
    assert_eq!(
        fs::read(root.join("out").join("stage").join("deep.txt")).unwrap(),
        b"xyz"
    );

    // Depth cap 1: the inner archive is stored as a plain file,
    // not recursed into.
    let stats = extract_with_limits(
        &root,
        "outer.tar",
        "out2",
        &ExtractLimits {
            max_uncompressed_bytes: 5 * 1024 * 1024 * 1024,
            max_entries: 100_000,
            max_nesting_depth: 1,
        },
    )
    .expect("extract without recursion");
    assert_eq!(stats.nested_archives, 0);
    assert!(root.join("out2").join("stage").join("inner.tar").is_file());
}

#[test]
fn unknown_archive_extensions_are_rejected() {
    let root = tmp_root();
    write(&root, "mystery.rar", b"not a real archive");
    let err = extract_to_err(&root, "mystery.rar", "out");
    assert!(
        matches!(err, FileError::UnsupportedFormat(_)),
        "got {err:?}"
    );
}

#[test]
fn a_zip_with_a_zip_slip_name_is_rejected() {
    let root = tmp_root();
    let file = fs::File::create(root.join("evil.zip")).expect("create zip");
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("../escaped.txt", options).expect("start");
    std::io::Write::write_all(&mut zip, b"stolen").expect("write");
    zip.finish().expect("finish");
    let err = extract_to_err(&root, "evil.zip", "out");
    assert!(matches!(err, FileError::Traversal(_)), "got {err:?}");
    assert!(!root.parent().unwrap().join("escaped.txt").exists());
}

#[test]
fn an_empty_allow_list_is_a_not_found() {
    let root = tmp_root();
    let err = archive(&root, "empty.tar", &["missing-dir"], ArchiveFormat::Tar).unwrap_err();
    assert!(err.is_not_found(), "got {err:?}");
}

// ── helpers ─────────────────────────────────────────────────────

fn extract_to(root: &Path, archive_path: &str, dest: &str) -> zim_engine_files::ExtractStats {
    zim_engine_files::extract(root, archive_path, dest).expect("extract")
}

fn extract_to_err(root: &Path, archive_path: &str, dest: &str) -> FileError {
    zim_engine_files::extract(root, archive_path, dest).expect_err("extraction must fail")
}
